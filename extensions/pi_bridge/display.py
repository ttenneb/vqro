"""Bounded live transcript and best-effort fallback; never render raw terminal control bytes."""
from collections import deque
import json
import os
import queue
import threading


from vqro_display import label, plain, tail


def message_text(message):
    content = message.get("content", "")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(block.get("text", "") for block in content
                         if isinstance(block, dict) and block.get("type") == "text" and isinstance(block.get("text"), str))
    return ""


class Transcript:
    """Bounded tail, not a durable log. Final messages replace provisional text."""
    def __init__(self, byte_limit=16384, rows=128):
        if type(byte_limit) is not int or byte_limit < 256 or type(rows) is not int or rows < 1:
            raise ValueError("invalid transcript limits")
        self.limit, self.rows = byte_limit, rows
        self.lines = deque()
        self.bytes = 0
        self.partial = ""
        self.clipped = False
        self.lost = False

    def append(self, text, style=None):
        raw = plain(text)
        text = tail(raw, self.limit)
        self.clipped |= text != raw
        size = len(text.encode())
        self.lines.append((text, style))
        self.bytes += size
        while self.lines and (self.bytes > self.limit or len(self.lines) > self.rows):
            self.bytes -= len(self.lines.popleft()[0].encode())
            self.clipped = True

    def gap(self):
        self.partial = ""
        self.lost = True
        self.append("[runtime event gap: partial transcript lost]", "warn")

    def feed(self, event):
        kind = event.get("type")
        if kind == "message_start" and event.get("message", {}).get("role") == "assistant":
            self.partial = ""
        elif kind == "message_update":
            update = event.get("assistantMessageEvent", {})
            if update.get("type") == "text_delta" and isinstance(update.get("delta"), str):
                text = self.partial + plain(update["delta"])
                self.partial = tail(text, self.limit)
                self.clipped |= text != self.partial
        elif kind == "message_end":
            message = event.get("message", {})
            role = message.get("role")
            if role in ("user", "assistant"):
                if role == "assistant":
                    self.partial = ""
                text = message_text(message)
                failed = role == "assistant" and message.get("stopReason") == "error"
                if text:
                    self.append(f"{role}> {text}", "error" if failed else None)
                if failed:
                    self.append("assistant error> " + label(message.get("errorMessage") or "Runtime reported an assistant error (no details).", 4096), "error")
        elif kind in ("auto_retry_start", "auto_retry_end"):
            attempt, maximum = event.get("attempt"), event.get("maxAttempts")
            count = str(attempt) if type(attempt) is int and attempt > 0 else ""
            if count and type(maximum) is int and maximum > 0:
                count += "/"+str(maximum)
            text = "Retry" + (" "+count if count else "")
            if kind == "auto_retry_start":
                text += " scheduled"
                if type(event.get("delayMs")) is int and event["delayMs"] >= 0:
                    text += f" ({event['delayMs']} ms)"
                detail = event.get("errorMessage")
                style = "warn"
            else:
                text += " succeeded" if event.get("success") is True else " failed" if event.get("success") is False else " ended"
                detail = event.get("finalError")
                style = "ok" if event.get("success") is True else "error" if event.get("success") is False else "dim"
            self.append(text + (": "+label(detail, 4096) if detail else ""), style)
        elif kind in ("tool_execution_start", "tool_execution_end"):
            name = tail(event.get("toolName", "tool"), 128)
            state = "started" if kind.endswith("start") else "failed" if event.get("isError") else "finished"
            self.append(f"tool {name}: {state}")

    def window(self, styled=False):
        parts = list(self.lines)
        if self.partial:
            parts.append(("assistant> " + self.partial, None))
        lines = [(line, style) for text, style in parts for line in text.splitlines()]
        rows, remaining, clipped = [], self.limit, self.clipped
        for line, style in reversed(lines):
            separator = 1 if rows else 0
            if len(rows) >= self.rows or remaining <= separator:
                clipped = True
                break
            bounded = tail(line, remaining-separator)
            rows.append({"text": bounded, **({"style": style} if style else {})})
            remaining -= len(bounded.encode())+separator
            if bounded != line:
                clipped = True
                break
        rows.reverse()
        return rows if styled else [row["text"] for row in rows], clipped, self.lost


def agent_data(snapshot, budget=60000):
    """Keep native facts when they fit; mark omitted fields unknown, never empty."""
    data = {key: snapshot[key] for key in ("run", "queues", "requests")}
    data.update(runtime_available=snapshot.get("available"), stale=snapshot.get("stale"),
                start_pending=snapshot.get("start_pending", False),
                failure=snapshot.get("failure"), retry=snapshot.get("retry"))
    # Preserve IDs, titles, options and confirmation bodies when representable.
    # If a field cannot fit, null plus an explicit marker means unknown, not empty.
    incomplete = []
    for key in ("queues", "requests", "run"):
        if len(json.dumps(data, ensure_ascii=False).encode()) <= budget:
            break
        data[key] = None
        incomplete.append(key)
        data["incomplete"] = incomplete[:]
    return data


class Fallback:
    """Redrawn diagnostic screen; a stalled terminal never blocks the RPC event loop.

    The daemon uses os.write, not Python's buffered stdout lock. Shutdown does
    not wait for a terminal that stopped reading. It is not a second RPC pipe.
    """
    def __init__(self, fd=1):
        self.fd = fd
        self.frames = queue.Queue(maxsize=1)
        self.stopped = threading.Event()
        self.thread = threading.Thread(target=self._write, daemon=True, name="pi-fallback")
        self.thread.start()

    def show(self, text):
        if self.stopped.is_set():
            return
        # Frames replace the screen, never append a repeated transcript to scrollback.
        # Only our own cursor controls are emitted; runtime text is sanitized first.
        frame = b"\x1b[H\x1b[2J" + tail(plain(text), 4096).encode() + b"\n"
        try:
            self.frames.put_nowait(frame)
        except queue.Full:
            try:
                self.frames.get_nowait()
            except queue.Empty:
                pass
            try:
                self.frames.put_nowait(frame)
            except queue.Full:
                pass

    def _write(self):
        while True:
            frame = self.frames.get()
            if frame is None:
                return
            try:
                while frame:
                    frame = frame[os.write(self.fd, frame):]
            except OSError:
                return
            if self.stopped.is_set() and self.frames.empty():
                return

    def close(self):
        self.stopped.set()
        try:
            self.frames.put_nowait(None)  # wake an idle writer; a queued final frame also wakes it
        except queue.Full:
            pass
        self.thread.join(.05)
