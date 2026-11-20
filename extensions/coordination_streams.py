"""One bounded receipt-only item viewer in the coordination extension.

Reads/reattaches only; never replays agent input. The core handles provider credit.
Network callbacks only enqueue bounded frames; the extension worker persists
positive projections before advancing its applied cursor. Other item bodies are
not coordination state and are discarded by ReceiptCollector.
"""
import base64
from collections import deque
import threading
import time

from coordination_receipts import ReceiptCollector, ReceiptError


class ItemReceiptSink:
    """SDK reader-thread sink: enqueue only, never call the core or apply evidence."""
    def __init__(self, viewer):
        self.viewer, self.stream = viewer, None

    def data(self, data):
        self.viewer.handle("stream.data", {"stream": self.stream}, decoded=data)

    def error(self, error):
        self.viewer.handle("stream.error", {"stream": self.stream, "error": error})

    def ended(self, kind, reason):
        self.viewer.handle("stream." + kind, {"stream": self.stream, "reason": reason})


class ItemReceiptStreams:
    def __init__(self, call, apply, *, wake=None, clock=None, limit=256):
        self.call, self.apply = call, apply
        self.wake = wake or (lambda: None)
        self.clock = clock or time.monotonic
        self.limit = limit
        self.lock = threading.RLock()
        self.queue = deque()
        self.queued_bytes = 0
        self.slots = {}  # exact agent -> collector/cursor; bounded by limit
        self.streams = {}  # owned stream id -> exact agent
        self.desired = {}
        self.issues = {}
        self.excess = 0

    def handle(self, method, params, *, decoded=None):
        if method not in ("stream.data", "stream.closed", "stream.gap", "stream.error"):
            return False
        stream = params.get("stream")
        if not (type(stream) is int and 0 <= stream < 2 ** 64 or isinstance(stream, str) and 0 < len(stream.encode()) <= 128):
            self.gap()
            return True
        data = None
        reason = method
        if method == "stream.data":
            try:
                data = base64.b64decode(params.get("data", ""), validate=True) if decoded is None else decoded
                if not isinstance(data, bytes) or len(data) > 1024 * 1024:
                    raise ValueError("frame exceeds viewer budget")
            except (ValueError, TypeError):
                reason = "invalid_base64_or_frame"
        with self.lock:
            charge = max(64, len(data or b""))
            if len(self.queue) >= 128 or self.queued_bytes + charge > 1024 * 1024:
                # This viewer gaps its own affected stream, never another viewer or
                # the runtime. Its next read uses a fresh fenced snapshot.
                retained = deque()
                for item in self.queue:
                    if item[0] == stream:
                        self.queued_bytes -= item[3]
                    else:
                        retained.append(item)
                self.queue = retained
                data, charge, reason = None, 64, "local_backpressure"
                if len(self.queue) >= 128 or self.queued_bytes + charge > 1024 * 1024:
                    # A global local queue gap is explicit; pending slots all reset.
                    self.queue.clear()
                    self.queued_bytes = 0
                    self.queue.append((None, None, "viewer_gap", 64))
                    self.queued_bytes = 64
            self.queue.append((stream, data, reason, charge))
            self.queued_bytes += charge
        self.wake()
        return True

    def sync(self, resources, providers):
        desired = {}
        for agent, resource in resources.items():
            caps = providers.get(resource.get("provider"), {}).get("capabilities", {})
            native = (resource.get("data") or {}).get("agent") or {}
            if (resource.get("kind") == "agent" and resource.get("available") is not False
                    and native.get("v") == 0 and isinstance(caps.get("items.stream"), dict)):
                desired[agent] = {"incarnation": resource.get("incarnation", ""),
                                  "signature": (resource.get("provider"), resource.get("instance"), resource.get("incarnation"),
                                                providers.get(resource.get("provider"), {}).get("generation"), (native.get("caps") or {}).get("rev"))}
        self.excess = max(0, len(desired) - self.limit)
        self.desired = {agent: desired[agent] for agent in sorted(desired)[:self.limit]}
        for agent in list(self.slots):
            if agent not in self.desired or self.slots[agent]["signature"] != self.desired[agent]["signature"]:
                self._close(agent, reset=True, reason="route_or_capability_changed", forget=True)
        for agent, target in self.desired.items():
            self.slots.setdefault(agent, dict(target, stream=None, collector=None, cursor=None,
                                              reset=True, retry_at=0.0, failures=0, suspended=False))

    def _close(self, agent, *, reset, reason, forget=False):
        slot = self.slots.get(agent)
        if slot is None:
            return
        stream = slot.get("stream")
        if stream is not None:
            self.streams.pop(stream, None)
            try:
                self.call("stream.close", {"stream": stream})
            except Exception:
                pass  # read handle cleanup is idempotent; never a native input
        slot["stream"] = None
        if slot.get("collector"):
            slot["collector"].discard_partial()
        if reset:
            slot.update(collector=None, cursor=None, reset=True)
        slot["failures"] += 1
        slot["retry_at"] = self.clock() + min(30.0, 0.5 * 2 ** min(slot["failures"], 6))
        self.issues[agent] = str(reason)[:128]
        if forget:
            self.slots.pop(agent, None)
            self.issues.pop(agent, None)

    def gap(self):
        """SDK notification loss is also loss of stream bytes, not just workspace events."""
        with self.lock:
            self.queue.clear()
            self.queued_bytes = 0
            self.queue.append((None, None, "viewer_gap", 64))
            self.queued_bytes = 64
        self.wake()

    def pump(self):
        # Drain before opening more snapshots. Four attaches at a time keep initial
        # catch-up from overflowing this connection's shared one-MiB stream budget.
        for _ in range(128):
            with self.lock:
                if not self.queue:
                    break
                stream, data, reason, charge = self.queue.popleft()
                self.queued_bytes -= charge
            if stream is None:
                for agent in list(self.slots):
                    self._close(agent, reset=True, reason=reason)
                continue
            agent = self.streams.get(stream)
            if agent is None:
                continue  # an old closed stream or a not-yet-installed attach
            slot = self.slots[agent]
            if data is None:
                self._close(agent, reset=reason != "stream.closed", reason=reason)
                continue
            try:
                events = slot["collector"].feed(data)
                if events:
                    self.apply(agent, slot["incarnation"], events)
                # A cursor is applied evidence, not just received bytes. On any
                # persistence failure reset; never resume after unpersisted events.
                cursor = slot["collector"].cursor
                if cursor is not None and cursor != slot["cursor"]:
                    slot["failures"] = 0
                    self.issues.pop(agent, None)
                slot["cursor"] = cursor
                slot["reset"] = False
            except Exception as error:
                if getattr(error, "kind", None) == "overflow":
                    slot["suspended"] = True  # explicit unsupported snapshot size, no endless reset loop
                self._close(agent, reset=True, reason=getattr(error, "kind", type(error).__name__))
        with self.lock:
            if self.queued_bytes > 256 * 1024:
                return True
        opened = 0
        for agent, slot in list(self.slots.items()):
            if opened >= 4:
                break
            if slot["stream"] is not None or slot["suspended"] or slot["retry_at"] > self.clock():
                continue
            params = {"resource": agent, "items": {}}
            after = slot["cursor"] if not slot["reset"] else None
            if after:
                params["items"]["after"] = after
            try:
                reply = self.call("resource.attach", params)
                stream = reply.get("stream")
                if type(stream) not in (int, str) or stream in self.streams:
                    raise ReceiptError("invalid", "item attach returned an invalid or reused stream")
                slot["stream"] = stream  # cleanup owns even a subsequently malformed reply
                if reply.get("initial") not in ("continue", "reset") or not isinstance(reply.get("epoch"), str):
                    raise ReceiptError("invalid", "item attach lacks epoch/initial fence")
                if reply["initial"] == "continue" and (after is None or after["epoch"] != reply["epoch"]):
                    raise ReceiptError("gap", "foreign or unrequested continuation")
                if reply["initial"] == "reset":
                    slot["collector"] = ReceiptCollector(reply["epoch"], entity_limit=1024)
                elif slot["collector"] is None:
                    slot["collector"] = ReceiptCollector(reply["epoch"], after=after, entity_limit=1024)
                stream = reply["stream"]
                slot["stream"] = stream
                self.streams[stream] = agent
                opened += 1
            except Exception as error:
                kind = getattr(error, "kind", "transport")
                if kind in ("unsupported", "invalid"):
                    slot["suspended"] = True  # a capability change may enable it later
                self._close(agent, reset=True, reason=kind)
        with self.lock:
            queued = bool(self.queue)
        due = any(slot["stream"] is None and not slot["suspended"] and slot["retry_at"] <= self.clock()
                  for slot in self.slots.values())
        return queued or due

    def status(self):
        issues = self.issues.copy()
        return {"attached": len(self.streams), "configured": len(self.slots), "unobserved_excess": self.excess,
                "queued_bytes": self.queued_bytes, "issues": dict(list(issues.items())[:64]),
                "incomplete": bool(self.issues) or bool(self.excess), "control_effects": False}
