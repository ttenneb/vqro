"""Bounded live transcript and best-effort fallback; never render raw terminal control bytes."""
from collections import deque
import json
import os
import queue
import threading


from prim_display import plain, tail


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

    def append(self, text):
        raw = plain(text)
        text = tail(raw, self.limit)
        self.clipped |= text != raw
        size = len(text.encode())
        self.lines.append(text)
        self.bytes += size
        while self.lines and (self.bytes > self.limit or len(self.lines) > self.rows):
            self.bytes -= len(self.lines.popleft().encode())
            self.clipped = True

    def gap(self):
        self.partial = ""
        self.lost = True
        self.append("[runtime event gap: partial transcript lost]")

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
                if text:
                    self.append(f"{role}> {text}")
        elif kind in ("tool_execution_start", "tool_execution_end"):
            name = tail(event.get("toolName", "tool"), 128)
            state = "started" if kind.endswith("start") else "failed" if event.get("isError") else "finished"
            self.append(f"tool {name}: {state}")

    def window(self):
        parts = list(self.lines)
        if self.partial:
            parts.append("assistant> " + self.partial)
        text = "\n".join(parts)
        bounded = tail(text, self.limit)
        rows = bounded.splitlines()[-self.rows:]
        return rows, self.clipped or bounded != text, self.lost


def agent_data(snapshot, budget=60000):
    """Keep native facts when they fit; mark omitted fields unknown, never empty."""
    data = {key: snapshot[key] for key in ("run", "queues", "requests")}
    data.update(runtime_available=snapshot.get("available"), stale=snapshot.get("stale"))
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
    """One-frame mailbox; a stalled terminal never blocks the RPC event loop.

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
        frame = tail(plain(text), 4096).encode() + b"\n"
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
        while not self.stopped.is_set() or not self.frames.empty():
            try:
                frame = self.frames.get(timeout=.05)
            except queue.Empty:
                continue
            try:
                while frame:
                    frame = frame[os.write(self.fd, frame):]
            except OSError:
                return

    def close(self):
        self.stopped.set()
        self.thread.join(.05)
