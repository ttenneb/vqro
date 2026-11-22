#!/usr/bin/env python3
"""Runtime-neutral agent panels and native capability-driven controls. No launcher."""
import collections
import json
from pathlib import Path
import sys
import threading
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
from vqro_ext import Extension, VqroError, views_in
from vqro_display import label, plain
from vqro_views import ended, unknown_since
from vqro_items import ItemView
from agent_request_routes import REQUEST_ROUTES, request_key
import agent_transcript

EFFECTS = {"prompt": "Prompt", "steer": "Steer", "follow_up": "Follow-up", "abort": "Abort",
           "clear_queue": "Clear queue", "answer": "Answer pending request"}

# Agent standard v0 (docs/spec/agent-v0.md): legacy pane actions map onto declared delivery modes and ops.
# A provider that declares the v0 op is driven with it; otherwise the legacy op (provider-v0 section 6) is used.
V0_MODE = {"prompt": "start", "steer": "boundary", "follow_up": "settle"}
V0_OP = {"prompt": "send", "steer": "send", "follow_up": "send", "abort": "interrupt", "clear_queue": "queue",
         "answer": "respond"}
# Tier-1 status (agents.status): a plugin without a tty (Claude Code hooks) reports its own terminal's state.
# Lossy like OSC 7501: kept in memory, last writer by seq, gone with the terminal or an extension restart.
TIER1_STATES = {"idle": "idle", "running": "working", "requires_action": "blocked", "unknown": "unknown"}
# Bounds keep the whole published table under the core's 1 MiB live-value cap (src/limits.rs): a worst-case entry
# without facts (64-char id, 64 printable 4-byte characters of app twice, maximum seq) encodes to 700 B, and 1024 of
# them fit TIER1_BYTES with room to spare. With up to TIER1_FACTS_BYTES of facts an entry is about 1.8 KB, so about
# 520 such entries fit; the byte check at admission refuses the rest (`overloaded`).
TIER1_MAX = 1024
TIER1_BYTES = 1024 * 1024 - 64 * 1024
TIER1_SEQ_MAX = 2 ** 53 - 1  # exact in every JSON reader
# Optional view facts a tier-1 plugin reads from its runtime (agent-v0 section 2.3 shapes): at most this many encoded
# bytes per terminal, counted in TIER1_BYTES; with facts, fewer terminals fit the table before `overloaded`.
TIER1_FACTS_BYTES = 1024


def tier1_facts(value):
    """Validate agents.status `facts` {session?: {cwd?, model?}, usage?}: the agent-v0 view-fact shapes. Unknown keys
    are dropped (forward compatible); a known key with the wrong shape is refused, never coerced."""
    def text(v, most, name):
        if not isinstance(v, str) or not v or len(v) > most or not v.isprintable():
            raise VqroError(f"{name} must be 1 to {most} printable characters", "invalid")
        return v

    def count(v, name):
        if type(v) is not int or not 0 <= v <= TIER1_SEQ_MAX:
            raise VqroError(f"{name} must be a non-negative integer", "invalid")
        return v

    def section(v, name, fields, required=()):
        if not isinstance(v, dict):
            raise VqroError(f"{name} must be an object", "invalid")
        out = {key: check(v[key], f"{name}.{key}") for key, check in fields.items() if key in v}
        missing = [key for key in required if key not in out]
        if missing:
            raise VqroError(f"{name} needs {', '.join(missing)}", "invalid")
        return out

    def amount(v, name):
        if type(v) not in (int, float) or v != v or not 0 <= v < 1e15:
            raise VqroError(f"{name} must be a non-negative number", "invalid")
        return v

    def basis(v, name):
        if v not in ("reported", "estimated"):
            raise VqroError(f"{name} must be reported or estimated", "invalid")
        return v
    model = lambda v, name: section(v, name, {"id": lambda x, n: text(x, 256, n), "label": lambda x, n: text(x, 256, n),
                                               "effort": lambda x, n: text(x, 64, n)}, ("id",))
    session = lambda v, name: section(v, name, {"cwd": lambda x, n: text(x, 512, n), "model": model})
    usage = lambda v, name: section(v, name, {
        "context": lambda x, n: section(x, n, {"used": count, "window": count, "limit": count}, ("window",)),
        "tokens": lambda x, n: section(x, n, {"input": count, "output": count, "cache_read": count, "cache_write": count},
                                       ("input", "output")),
        "cost": lambda x, n: section(x, n, {"amount": amount, "currency": lambda y, m: text(y, 16, m), "basis": basis,
                                            "plan": lambda y, m: text(y, 64, m)}, ("amount", "currency", "basis")),
        "at_ms": count})
    facts = section(value, "facts", {"session": session, "usage": usage})
    if len(json.dumps(facts, ensure_ascii=False, separators=(",", ":")).encode()) > TIER1_FACTS_BYTES:
        raise VqroError(f"facts exceed {TIER1_FACTS_BYTES} encoded bytes; nothing recorded", "invalid")
    return facts
MODE_ORDER = ("start", "boundary", "settle", "admit", "preempt")
MODE_SHORT = {"start": "Prompt", "boundary": "Steer", "settle": "Queue", "admit": "Admit", "preempt": "Interrupt and send"}
RECEIPT_TEXT = {"admitted": "admitted; where it landed is unknown", "queued": "held by the runtime",
                "inserted": "in the conversation", "handled": "handled without a model turn"}


def data(resource):
    value = resource.get("data")
    return value if isinstance(value, dict) else {}


def status(resource):
    lifecycle = agent_facts(resource).get("lifecycle")
    if isinstance(lifecycle, str) and lifecycle != "live":
        return label(lifecycle)  # agent v0 section 7: stopping, disconnected, unknown, ended
    state = agent_facts(resource).get("state")
    if isinstance(state, str):
        return label(state.replace("_", " "))
    value = resource.get("status")
    return label(value.get("state", "unknown")) if isinstance(value, dict) else "unknown"


def availability_label(resource):
    """Only a declared block changes the frame. Retry delay is a reported snapshot, not a ticking clock or clear."""
    availability = agent_facts(resource).get("availability")
    if not isinstance(availability, dict) or availability.get("state") != "blocked":
        return None
    reason = availability.get("reason")
    labels = {"rate_limited": "rate limited", "quota": "quota", "auth": "login", "_retrying": "retrying"}
    reason = labels.get(reason) if isinstance(reason, str) else None
    text = "blocked" + (" · " + reason if reason else "")
    delay = availability.get("retry_after_ms")
    if type(delay) is int and delay >= 0:
        text += f" · retry in {(delay + 999) // 1000}s"
    return text


def view(rid):
    return "agents-"+rid


def agent_facts(resource):
    """The resource's `data.agent` object (agent v0 section 2.3), or {}."""
    value = data(resource).get("agent")
    return value if isinstance(value, dict) else {}


def caps(resource):
    """This resource's own capabilities (`data.agent.caps`, agent v0 section 2.4), or {}."""
    value = agent_facts(resource).get("caps")
    return value if isinstance(value, dict) else {}


def mode_label(mode, descriptor):
    """What a delivery mode does, in the words of its declaration (no runtime names)."""
    point = descriptor.get("point")
    where = " at "+label(str(point), 40) if isinstance(point, str) else ""
    return {"start": "Prompt (start work)",
            "boundary": "Steer: join current work"+where,
            "settle": "Queue: when current work would stop" if point == "would_stop" else "Queue: after current work",
            "admit": "Admit (runtime order; when it is read is not guaranteed)",
            "preempt": "Interrupt and send (native)"}.get(mode, label(mode, 40))


def busy(resource):
    """Only explicit, current idle evidence is idle (agent v0 sections 2.3, 7). A missing, cut or unknown state, or
    a missing, cut or non-live lifecycle, counts as busy; the coarse OSC-compatible status is never used for a v0
    agent."""
    facts = agent_facts(resource)
    incomplete = facts.get("incomplete")
    if isinstance(incomplete, list) and ("state" in incomplete or "lifecycle" in incomplete):
        return True
    return facts.get("lifecycle") != "live" or facts.get("state") != "idle"


DISCLOSURE_BYTES = 16 * 1024  # a whole request as shown; legal summaries (<= 2 KiB, escaped <= 4x) always fit
DISCLOSURE_ROW_BYTES = 512


def head(text, size=200):
    """At most `size` bytes of `text` from its start, marked with … when cut (short labels only, never the question)."""
    wire = text.encode()
    return text if len(wire) <= size else wire[:size - 3].decode("utf-8", errors="ignore") + "…"


def disclosure(parts):
    """A request's question shown whole: `parts` are (prefix, value) pairs, rendered control-safe (plain) and split
    across rows at line breaks and every DISCLOSURE_ROW_BYTES, never clipped. Returns (rows, complete); when the
    whole presentation would exceed DISCLOSURE_BYTES, complete is False and the request must not be approvable."""
    rows, total = [], 0
    for prefix, value in parts:  # an explicit null is shown ("null"); absent fields are simply not in `parts`
        text = value if isinstance(value, str) else json.dumps(value, ensure_ascii=False, separators=(",", ":"))
        for line in (prefix + text).split("\n"):
            line = plain(line)
            total += len(line.encode()) + 1
            if total > DISCLOSURE_BYTES:
                return rows, False
            wire = line.encode()
            while True:
                cut = len(wire) if len(wire) <= DISCLOSURE_ROW_BYTES else DISCLOSURE_ROW_BYTES
                while cut < len(wire) and (wire[cut] & 0xC0) == 0x80:  # never split a character
                    cut -= 1
                rows.append({"type": "text", "wrap": True, "style": "muted", "text": wire[:cut].decode()})
                wire = wire[cut:]
                if not wire:
                    break
    return rows, True


def effect_words(choice):
    effect = choice.get("effect")
    return str(effect).replace("_", " ") if isinstance(effect, str) else None


def choice_label(n, choice):
    """A menu label that keeps what tells choices apart: its number and effect first, then the label (head-cut)."""
    effect = effect_words(choice)
    return head(f"{n}. " + (effect.capitalize() + ": " if effect else "") + plain(str(choice.get("label") or choice.get("id"))))


def choices_of(q):
    choices = q.get("choices")
    return [c for c in choices if isinstance(c, dict) and isinstance(c.get("id"), str)] if isinstance(choices, list) else []


def request_parts(q):
    """What a v0 request asks (agent v0 section 5): its title, every native param except presentation hints (an
    explicit null included), and every choice whole with its id and effect; all in one completeness budget."""
    params = q.get("params") if isinstance(q.get("params"), dict) else {}
    return [("", q.get("title") or q["request"])] + [
        (f"Choice {n} ({', '.join(x for x in (effect_words(c), 'id ' + c['id']) if x)}): ", c.get("label") or c["id"])
        for n, c in enumerate(choices_of(q), 1)] + [
        ("" if key == "message" else key.replace("_", " ")+": ", value)
        for key, value in params.items() if key != "multiline"]


def legacy_parts(request):
    """What a provider-v0 request asks: its prompt, message, prefill and options (shown whole)."""
    detail = request.get("data") if isinstance(request.get("data"), dict) else {}
    return [("", request.get("prompt") or request["id"])] + [
        (prefix, detail[key]) for key, prefix in (("message", ""), ("prefill", "Prefilled: "), ("options", "Options: "))
        if key in detail]  # absent optional fields are skipped; an explicit null is shown


REQUESTS_BYTES = 256 * 1024  # one requests popup, well under the core's 1 MiB live-value cap
REQUESTS_CANCEL_MAX = 256


def bounded_requests(rid, prefix, groups):
    """The requests popup, bounded for every runtime (v0 and provider-v0): requests are shown whole, in order, while
    they fit REQUESTS_BYTES; the rest are listed by Cancel only (bounded), never dropped silently. `groups` are
    (rows, cancel_row) per request."""
    items, size, rest = list(prefix), len(json.dumps(prefix).encode()), []
    for rows, cancel in groups:
        cost = len(json.dumps(rows + [cancel]).encode())  # ASCII-escaped: an over-count, never an under-count
        if not rest and size + cost <= REQUESTS_BYTES:
            items += rows + [cancel]
            size += cost
        else:
            rest.append(cancel)
    if rest:
        items.append({"type": "text", "style": "warn", "text": f"{len(rest)} more requests don't fit here. Answer the ones "
                      "above first (they then make room), or cancel these; nothing is answered for you:"})
        items += rest[:REQUESTS_CANCEL_MAX]
        if len(rest) > REQUESTS_CANCEL_MAX:
            items.append({"type": "text", "style": "warn", "text": f"… and {len(rest) - REQUESTS_CANCEL_MAX} more."})
    if not items:
        items = [{"type": "text", "text": "No pending requests."}]
    # `wrap` is required: a client that can't wrap would cut the question at the pane edge.
    return {"title": "Requests: "+rid, "requires": ["menu", "input", "wrap"], "items": items}


def owns_lifecycle(resource):
    """False only when the agent's facts say its host does not own the runtime's lifecycle (agent v0 section 1)."""
    host = agent_facts(resource).get("host")
    return not (isinstance(host, dict) and host.get("owns_lifecycle") is False)


def advertised(resource, field):
    """Agent standard v0 section 6.3: the resource's `data.agent.commands` / `data.agent.config` (or [])."""
    agent = data(resource).get("agent")
    rows = agent.get(field) if isinstance(agent, dict) else None
    key = "name" if field == "commands" else "id"
    return [row for row in rows if isinstance(row, dict) and isinstance(row.get(key), str)] if isinstance(rows, list) else []


def panel_key(key):
    return key.startswith("view/agents-r") and key[len("view/agents-r"):].isdigit()


# Owned per-agent derived views, longest prefix first ("agents-" would also match the others).
# Keys in an agent pane without the prefix (AS10b/AS10 keyboard flow; D29 pane-scoped bindings). All are ordinary
# bindings, so Settings → Keys and keymap profiles can change them; the prefix key always comes first.
AGENT_PANE = {"resource_kind": "agent"}
PANE_KEYS = {
    "binding/agents.copy": {"key": "y", "scope": "pane", "when": AGENT_PANE, "command": "client.copy",
                            "label": "copy the selected message or tool output"},
    "binding/agents.compose": {"key": "i", "scope": "pane", "when": AGENT_PANE, "command": "client.composer",
                               "args": {"action": "focus"}, "label": "write a message"},
    **{f"binding/agents.toggle.{kind}": {"key": key, "scope": "pane", "when": AGENT_PANE, "command": "agents.view",
                                         "args": {"set": kind, "value": "cycle"}, "label": f"{kind}: show / summary / hide"}
       for kind, key in (("thinking", "t"), ("tools", "T"), ("system", "s"))},
    "binding/agents.send.steer": {"key": "M-s", "scope": "composer", "when": AGENT_PANE, "command": "client.composer",
                                  "args": {"action": "send", "set": {"mode": "boundary"}}, "label": "send as steer"},
    "binding/agents.send.follow_up": {"key": "M-f", "scope": "composer", "when": AGENT_PANE, "command": "client.composer",
                                      "args": {"action": "send", "set": {"mode": "settle"}}, "label": "send as follow-up"},
}
# The message box starts at one line and grows with the draft up to this many lines (mockup: about a third of the pane),
# then scrolls; clients that can't grow draw it at this height.
COMPOSER_ROWS = 8
VIEW_DEFAULTS = "doc/settings/agents"  # {"view": {...}}: defaults for new agent panes (agent_transcript.view_settings)
VIEW_PANE = "doc/agents/view/"  # + rid: this agent pane's own view settings (AS10d: per pane, workspace defaults)
OWNED_PREFIXES = ("view/agents-requests-", "view/agents-settings-", "view/agents-stop-", "view/agents-send-", "view/agents-pane-",
                  "view/agents-tools-", "view/agents-")


def host_of(resources, r):
    """The agent's bridge terminal resource, or None when it has none vqro knows."""
    host = resources.get(r.get("host")) if r.get("host") else None
    return host if host and host.get("kind") == "terminal" else None


def unknown_badge(ext, r):
    """Badge for an unavailable agent: "unknown since HH:MM" when the core says when it lost it."""
    since = unknown_since(ext, r)
    return {"text": "unknown since "+since if since else "unknown", "style": "warn"}


# A viewer's own ceiling: provider declarations are taken only up to these (review P2-2), so one agent can't make
# this extension hold unbounded state.
VIEW_MAX_ENTITIES = 4096
VIEW_MAX_ENTITY_BYTES = 4 * 1024 * 1024
LOGGED_MAX = 256  # agents' last logged failures kept for de-duplication (Agents.log_failure)
LIVE_NOTICES = 3  # the newest live notice records (agent-v0 section 4) a pane shows
# agent-v0 section 4 "Incompatible stream": a rejection that repeats on reattach backs off, then stops and is reported.
INCOMPATIBLE_BACKOFF_S = (2.0, 10.0)  # waits after the 1st and 2nd identical rejection
INCOMPATIBLE_STOP = len(INCOMPATIBLE_BACKOFF_S) + 1  # the 3rd identical rejection stops automatic reattaching


class Conversation:
    """One agent's item stream (agent-v0 section 4) and its reconstruction. The SDK calls the sink on its reader
    thread: it only feeds bytes and wakes the worker, never calls the core."""

    def __init__(self, rid, wake, bounds):
        self.rid, self.wake = rid, wake
        self.view = ItemView(**bounds)
        self.lock = threading.Lock()
        self.stream = None
        self.generation = 0  # bumped when a stream ends, so a late attach reply can't revive it
        self.clamped = False  # the provider declared more than this viewer keeps
        self.notices = collections.deque(maxlen=LIVE_NOTICES)  # (at_ms, value): live notice records, newest last
        # Incompatible stream (agent-v0 section 4): consecutive identical rejections, the wait before the next attach,
        # and, once stopped, the rejection reported (no automatic reattach until shown again or bounds change).
        self.rejections, self.rejected_at, self.retry_at, self.incompatible = 0, None, 0.0, None
        self.compat = None  # the provider's declared items bounds when it stopped
        self.retry_scheduled = False  # one pending backoff timer at a time
        self.counted_generation = None  # the stream whose rejection was counted: one attempt per stream, however chunked
        self.declared = {}  # the provider's declared bounds this view was built from (shown when clamped)

    @property
    def too_big(self):
        return bool(self.incompatible) and self.incompatible.get("kind") == "over_bound"

    def data(self, data, generation=None):
        with self.lock:
            if generation is not None and generation != self.generation:
                return  # late bytes of a stream already closed: never applied or counted against the current one
            now, epoch = int(time.time() * 1000), self.view.epoch
            rejected_before = self.view.rejection
            received = self.view.feed(data, epochs=True)
            if self.view.epoch != epoch:  # a new epoch or session: earlier live notices are about something else
                self.notices.clear()
            # Only notices of the epoch now installed, judged per record, so chunk boundaries never matter (#97).
            self.notices.extend((now, n) for at, n in received if at == self.view.epoch)
            broken = self.view.broken
            rejection = self.view.rejection if broken and self.view.rejection is not rejected_before else None
            if rejection is not None and self.counted_generation != self.generation:
                self.counted_generation = self.generation  # this stream's attempt; more bytes of it add nothing
                self.rejected(rejection)
            elif not broken and self.rejections and not self.incompatible and self.passed_rejection():
                self.rejections, self.rejected_at = 0, None  # real progress: the warning no longer applies
        self.wake(self.rid, reattach=broken)

    def passed_rejection(self):
        """The view moved past the rejected position: a new epoch, or a record applied beyond it. A reset snapshot back
        at the same cut is not progress (the same rejection then still counts)."""
        _, _, epoch, seq = self.rejected_at or (None, None, None, None)
        return self.view.epoch is not None and (self.view.epoch != epoch or self.view.seq > seq)

    def rejected(self, rejection):
        """Count an incompatible rejection (under the lock). The same cause at the same position again is not
        progress; anything else starts a new count. Never skips, relaxes bounds or moves the cursor."""
        at = (rejection.get("kind"), rejection.get("op"), self.view.epoch, self.view.seq)
        self.rejections = self.rejections + 1 if at == self.rejected_at else 1
        self.rejected_at = at
        if self.rejections >= INCOMPATIBLE_STOP:
            self.incompatible = dict(rejection, attempts=self.rejections)
        else:
            self.retry_at = time.monotonic() + INCOMPATIBLE_BACKOFF_S[self.rejections - 1]

    def compatible_again(self):
        """Shown again, or the producer's bounds changed: allow attaching (the rejection count starts over)."""
        self.rejections, self.rejected_at, self.retry_at, self.incompatible, self.compat = 0, None, 0.0, None, None

    def error(self, error):
        pass

    def ended(self, kind, reason, generation=None):
        with self.lock:
            if generation is not None and generation != self.generation:
                return  # an old stream's end never ends the current one
            self.view.stream_ended()
            self.stream = None
            self.generation += 1
        self.wake(self.rid, reattach=True)


class StreamSink:
    """The sink of ONE attached stream: it forwards to its conversation only while that stream is current (the
    generation it was attached in), so bytes or an end the SDK still delivers after a close are dropped."""

    def __init__(self, conversation, generation):
        self.conversation, self.generation = conversation, generation

    def data(self, data):
        self.conversation.data(data, self.generation)

    def error(self, error):
        self.conversation.error(error)

    def ended(self, kind, reason):
        self.conversation.ended(kind, reason, self.generation)


def incompatible_note(c):
    """The visible incompatible-stream report (agent-v0 section 4): the cause and the limit it ran into, and whether
    the view is backing off or has stopped. Called under the conversation lock."""
    r = c.incompatible or (c.view.rejection or {})
    limits, kind = r.get("limits") or {}, r.get("kind")
    at = f" at seq {r['seq']}" if type(r.get("seq")) is int else ""
    if kind == "over_bound":
        declared = c.declared or {}
        cause = (f"its history is larger than this view's limit ({limits.get('entities')} items, "
                 f"{limits.get('entity_bytes')} bytes)" + (
                     f"; the agent declares {declared.get('max_entities')} items, {declared.get('max_entity_bytes')} bytes"
                     if c.clamped else ""))
    elif kind == "oversize":
        cause = f"it sent a record larger than this view's limit of {limits.get('record_bytes')} bytes"
    elif r.get("unknown_op"):
        cause = f"it sent an unknown operation \"{label(str(r.get('op')), 64)}\"{at}, perhaps from a newer agent"
    else:
        cause = f"it sent a record this view can't read (bad_record{', op ' + label(str(r['op']), 64) if r.get('op') else ''}){at}"
    if c.incompatible:
        tail = (f" The conversation stopped updating after {c.incompatible.get('attempts')} identical attempts. "
                "Show the agent again from the Agents list to retry.")
    else:
        tail = f" Retrying after a pause (attempt {c.rejections} of {INCOMPATIBLE_STOP})."
    return "Incompatible stream: " + cause + "." + tail


def broken_panel(r, error):
    """An agent pane whose drawing failed: says so, bounded, in that pane only (the other agents' panes keep updating).
    Nothing is sent to the agent; showing it again or the next update redraws it."""
    return {"title": label(r.get("name") or r.get("title") or "Agent") + " " + str(r.get("id")), "items": [
        {"type": "text", "style": "error", "text": "This agent's pane couldn't be drawn ("
         + label(f"{type(error).__name__}: {error}", 300) + "). Other agents are unaffected; it redraws on the next update."}]}


class Agents:
    def __init__(self, ext):
        self.ext, self.lock = ext, threading.RLock()
        # strict: a render failure raises instead of becoming that pane's error row (tests set it, so a masked render
        # bug still fails them); logged: the last failure logged per (agent, step), to log each distinct one once.
        self.strict, self.logged, self.log_lock = bool(getattr(ext, "strict_render", False)), {}, threading.Lock()
        self.open_requests = set()
        self.open_settings = set()
        self.open_pane, self.published_pane = set(), {}
        self.open_tools, self.published_tools = {}, {}  # rid -> the run its open tools table shows; last published  # ⚙ popups shown (republished as the view or agent changes)
        self.published = {}  # Only current agents; suppress unchanged derived panel RPCs.
        # Conversations from item streams; a worker attaches, reattaches and republishes (coalesced).
        self.conversations, self.conv_lock, self.conv_event = {}, threading.Lock(), threading.Event()
        self.conv_dirty, self.conv_attach = set(), set()
        threading.Thread(target=self.conversation_worker, daemon=True, name="agents-conversations").start()
        self.list_open = False
        self.tier1 = {}  # terminal id -> {app, state, seq, at_ms, status}
        self.tier1_size = {}  # terminal id -> encoded bytes of its published entry
        self.tier1_lock = threading.Lock()
        self.tier1_dirty = self.tier1_publishing = False
        ext.command("agents.status", "Report a tier-1 agent's state for its terminal (args: terminal, app, state, seq, facts?)")(self.report_status)
        for name, fn, title in [("list", self.list_agents, "Show agents"), ("show", self.show, "Show an agent in a tab"),
                                ("config", self.settings, "Agent settings and commands (popup)"),
                                ("configure", self.configure, "Change an agent setting (args: id, value)"),
                                ("command", self.command, "Run an agent command (args: name, text?)"),
                                ("send", self.send, "Send a message in a declared delivery mode (args: mode?, text?)"),
                                ("view", self.set_view, "Show, summarise or hide parts of an agent pane (args: set, value)"),
                                ("stop", self.stop, "Stop an agent: kill its bridge terminal and the runtime in it (asks first)"),
                                ("pane", self.pane_menu, "The agent pane's ⚙ popup: view filters, settings, bridge terminal, stop"),
                                ("tools", self.tools_table, "One turn's tools as a table (args: run = the turn's first tool id)"),
                                ("forget", self.forget, "Forget an unavailable agent's cached facts (its process is not touched)")]:
            ext.command("agents."+name, title)(self.guarded(fn))
        for op, title in EFFECTS.items():
            fn = self.answer if op == "answer" else lambda args, op=op: self.effect(op, args)
            ext.command("agents."+op, title)(self.guarded(fn))
        self.signature = self.current_signature()
        declarations = {"binding/agents.list": {"key": "A", "command": "agents.list", "label": "agents"},  # off the tab bar
                        "view/agents-list": {"kind": "panel", "title": "Agents", "items": []},
                        # Prefix p: prompt the agent in the focused pane. `when` keeps it off other
                        # panes (the key falls through there), so no text is typed only to fail.
                        "binding/agents.prompt": {"key": "p", "command": "agents.prompt", "input": "Prompt agent",
                                                  "label": "prompt agent", "when": {"resource_kind": "agent"}},
                        # Prefix P (Shift-p): Agent settings for the agent in the focused pane.
                        "binding/agents.config": {"key": "P", "command": "agents.config", "label": "agent settings",
                                                  "when": {"resource_kind": "agent"}},
                        **PANE_KEYS}
        declarations.update(self.declarations())
        # Provider inventory may not be restored yet. Referenced panels must
        # remain valid across extension disable/re-enable and core restart.
        for key, entry in ext.entries.items():
            if panel_key(key) and entry["value"].get("owner") == "agents":
                declarations.setdefault(key, {k: v for k, v in entry["value"].items() if k != "owner"} | {"resource": key[len("view/agents-"):]})
        ext.contribute(declarations)  # reconciles removed/moved launcher entries
        ext.on_event(self.event)
        self.publish_panels()
        self.refresh(prune_views=True)
        self.sync_conversations()

    def streams_items(self, r):
        """The agent's runtime publishes an item stream and the agent is reachable now."""
        return (r.get("kind") == "agent" and r.get("available") is not False and not ended(r)
                and isinstance(self.provider_caps(r).get("items.stream"), dict))

    def later(self, delay, rid):
        """One delayed reattach of `rid` (a daemon timer; tests replace this)."""
        timer = threading.Timer(delay, self.retry_due, (rid,))
        timer.daemon = True
        timer.start()

    def fresh_view(self, rid):
        """An ItemView built from the provider's CURRENT items declaration, clamped to this viewer's own ceiling:
        (view, clamped, declared). Used when a conversation starts, is shown again, or its declaration changes."""
        r = self.ext.resources.get(rid)
        bounds = (self.provider_caps(r).get("items.stream") if r is not None else None) or {}
        declared = {k: bounds[f] for k, f in (("max_entities", "entities"), ("max_entity_bytes", "entity_bytes"))
                    if type(bounds.get(f)) is int and bounds[f] > 0}
        kept = {k: min(v, VIEW_MAX_ENTITIES if k == "max_entities" else VIEW_MAX_ENTITY_BYTES) for k, v in declared.items()}
        return ItemView(**kept), kept != declared, declared

    def renew(self, c, rid):
        """Allow attaching again with a view built from the current declaration (under c.lock)."""
        c.compatible_again()
        c.view, c.clamped, c.declared = self.fresh_view(rid)

    def retry_due(self, rid):
        """A backoff timer fired: it is no longer pending (even if it fired a hair early, so the next attach can
        schedule another instead of leaving the pane with neither a stream nor a timer)."""
        c = self.conversations.get(rid)
        if c is not None:
            with c.lock:
                c.retry_scheduled = False
        self.wake(rid, reattach=True)

    def items_bounds(self, rid):
        r = self.ext.resources.get(rid)
        caps = self.provider_caps(r).get("items.stream") if r is not None else None
        return json.dumps(caps, sort_keys=True) if isinstance(caps, dict) else None

    def wake(self, rid, reattach=False):
        with self.conv_lock:
            self.conv_dirty.add(rid)
            if reattach:
                self.conv_attach.add(rid)
        self.conv_event.set()

    def sync_conversations(self):
        """Follow the resources: a conversation for every agent with an item stream; a gone agent's is dropped, an
        unavailable one's stream is closed but its last state is kept (shown as stale)."""
        for r in list(self.ext.resources.values()):
            if self.streams_items(r):
                bounds = self.provider_caps(r)["items.stream"]
                if r["id"] not in self.conversations:
                    c = self.conversations[r["id"]] = Conversation(r["id"], self.wake, {})
                    c.view, c.clamped, c.declared = self.fresh_view(r["id"])
                    self.wake(r["id"], reattach=True)
                elif self.conversations[r["id"]].stream is None:
                    c = self.conversations[r["id"]]
                    if c.incompatible and c.compat is not None and c.compat != self.items_bounds(r["id"]):
                        with c.lock:  # a compatibility change (the producer's declared bounds) retries, with them
                            self.renew(c, r["id"])
                    self.wake(r["id"], reattach=True)
        for rid, c in list(self.conversations.items()):
            r = self.ext.resources.get(rid)
            if r is None or not self.streams_items(r):
                if c.stream is not None:
                    self.ext.close_stream(c.stream)
                    with c.lock:
                        c.view.stream_ended()
                        c.stream = None
                        c.generation += 1
                if r is None:
                    del self.conversations[rid]

    def attach(self, rid):
        c, r = self.conversations.get(rid), self.ext.resources.get(rid)
        if c is None or r is None or not self.streams_items(r):
            return
        # Lock order: never call the SDK (close_stream and attach_items are synchronous RPCs whose replies the reader
        # thread delivers) while holding c.lock. That reader may be blocked on c.lock delivering an old stream's
        # callback, and could then never read the reply. Decide and detach under the lock; call the SDK after.
        with c.lock:
            waiting = c.retry_at - time.monotonic()
            stop = bool(c.incompatible or waiting > 0 or c.stream is not None and not c.view.broken)
            if stop:
                old = None
                if (c.incompatible or waiting > 0) and c.stream is not None and c.view.broken:
                    old, c.stream = c.stream, None  # stop the rejected stream now; never read past the rejection
                    c.view.stream_ended()
                    c.generation += 1  # its late callbacks are now non-current: its StreamSink drops them
                if c.incompatible:
                    c.compat = c.compat or self.items_bounds(rid)
                elif waiting > 0 and not c.retry_scheduled:  # back off: one delayed reattach, never a tight loop
                    c.retry_scheduled = True
                    self.later(waiting, rid)
            else:
                c.retry_scheduled = False
                old, c.stream = c.stream, None
                # After lost records, start over from a fresh snapshot: simple, bounded, and right whatever the loss
                # was (vqro_items keeps after() at the last record applied, but a snapshot also repairs an over-bound
                # view).
                lost = c.view.broken
                if old is not None:
                    c.view.stream_ended()
                    c.generation += 1
                after, generation = None if lost else c.view.after(), c.generation
        if old is not None:
            self.ext.close_stream(old)  # outside the lock (see above)
        if stop:
            return
        try:
            reply = self.ext.attach_items(rid, StreamSink(c, generation), after)
        except VqroError:
            self.later(1.0, rid)
            return
        late = None
        with c.lock:
            if c.generation == generation and isinstance(reply.get("stream"), int):
                c.stream = reply["stream"]
            elif isinstance(reply.get("stream"), int):
                late = reply["stream"]  # the conversation moved on while this attach was in flight: nobody reads it
        if late is not None:
            self.ext.close_stream(late)  # outside the lock (see above); otherwise the core keeps it open, using credit

    def flush_conversations(self):
        """One worker pass: (re)attach what needs it, then republish changed panes."""
        with self.conv_lock:
            attach, dirty = self.conv_attach, self.conv_dirty
            self.conv_attach, self.conv_dirty = set(), set()
        for rid in attach:
            self.isolated(rid, "attach", self.attach, rid)
        for rid in dirty:
            r = self.ext.resources.get(rid)
            if r is not None:
                self.isolated(rid, "publish", self.publish_panel, r)

    def isolated(self, rid, what, fn, *args):
        """Run one agent's step so that its failure never stops the others' (review: one agent's render bug stalled
        every pane). The failure is logged; publish_panel already shows it in that agent's own pane."""
        try:
            fn(*args)
        except Exception as error:  # noqa: BLE001 - any bug in one agent's step
            if self.strict:
                raise
            self.log_failure(rid, what, error)

    def log_failure(self, rid, what, error):
        """Log an agent's failure once per distinct error: `logged` keeps the LAST line logged per (agent, step), so a
        failing pane that redraws several times a second logs once, a different error logs again, and the same error
        after a good draw (which forgets it) logs again. Entries go when the agent goes; at most LOGGED_MAX are kept
        (oldest dropped)."""
        line = f"agents: {what} failed for {rid}: {type(error).__name__}: {str(error)[:300]}"
        with self.log_lock:  # the worker logs while the event thread prunes (publish_gone)
            if self.logged.get((rid, what)) == line:
                return
            self.logged.pop((rid, what), None)
            self.logged[(rid, what)] = line
            while len(self.logged) > LOGGED_MAX:
                self.logged.pop(next(iter(self.logged)))
        print(line, file=sys.stderr, flush=True)

    def conversation_worker(self):
        while True:
            self.conv_event.wait()
            time.sleep(0.15)  # coalesce a burst of streamed text into one publication
            self.conv_event.clear()
            try:
                self.flush_conversations()
            except Exception as error:  # report, keep the worker alive
                print(f"agents: conversation update failed: {error}", file=sys.stderr, flush=True)

    def view_of(self, rid):
        defaults = self.ext.value(VIEW_DEFAULTS)
        return agent_transcript.view_settings(defaults.get("view") if isinstance(defaults, dict) else None,
                                              self.ext.value(VIEW_PANE + rid))

    def set_view(self, args):
        """agents.view {resource, set, value}: one view setting of this agent pane (a workspace setting, so it is
        undoable). `set` is thinking|tools|system (show|summary|hide), density, sender (toggles that sender),
        summary (toggles a summary field), or defaults (this pane's settings become the default for new panes)."""
        r = self.agent(args, inspect=True)
        rid, what, value = r["id"], args.get("set"), args.get("value")
        current = self.view_of(rid)
        if what in agent_transcript.KINDS and rid not in self.conversations:
            raise VqroError(f"{label(r.get('name') or rid)} doesn't stream its conversation, so there is nothing to show or "
                            "hide here", "unsupported")
        if what in agent_transcript.KINDS and value == "cycle":  # the pane keys: show -> summary -> hide -> show
            modes = agent_transcript.MODES
            change = {what: modes[(modes.index(current[what]) + 1) % len(modes)]}
        elif what in agent_transcript.KINDS and value in agent_transcript.MODES:
            change = {what: value}
        elif what == "density" and value in ("detailed", "compact"):
            change = {"density": value}
        elif what == "sender" and isinstance(value, str):
            hidden = [k for k in current["hidden_senders"] if k != value] + ([] if value in current["hidden_senders"] else [value])
            change = {"hidden_senders": hidden}
        elif what == "summary" and value in agent_transcript.SUMMARY_FIELDS:
            fields = current["summary"]
            change = {"summary": [f for f in fields if f != value] + ([] if value in fields else [value])}
        elif what == "defaults":
            key, entry = VIEW_DEFAULTS, {"view": {k: v for k, v in current.items() if k != "hidden_senders"}}
            self.ext.retrying(lambda: self.ext.apply([{"op": "set", "key": key, "value": entry, "expect": self.ext.version(key)}],
                                                     "agents: default view for agent panes"))
            return {"ok": True}
        else:
            raise VqroError("unknown view setting; nothing changed", "invalid")
        key = VIEW_PANE + rid

        def write():
            own = self.ext.value(key) if isinstance(self.ext.value(key), dict) else {}
            return self.ext.apply([{"op": "set", "key": key, "value": {**own, **change}, "expect": self.ext.version(key)}],
                                  "agents: agent pane view")
        self.ext.retrying(write)
        return {"ok": True}

    def view_rows(self, rid, view, seen):
        """The pane's view filters, one row each (pick show/summary/hide, senders, density, summary fields)."""
        def row(text, what, options):
            return {"text": text, "command": "agents.view", "args": {"resource": rid, "set": what}, "options": options}
        modes = [{"label": m, "value": m} for m in agent_transcript.MODES]
        hidden = view["hidden_senders"]
        return [
            *[row(f"{kind.capitalize()}: {view[kind]}", kind, modes) for kind in agent_transcript.KINDS],
            row("Senders: all" if not set(hidden) & set(seen) else f"Senders: {len(set(hidden) & set(seen))} hidden", "sender",
                [{"label": ("hidden: " if key in hidden else "shown: ") + name, "value": key} for key, name in seen.items()]
                or [{"label": "no senders yet", "value": ""}]),
            row("Layout: " + view["density"], "density", [{"label": d, "value": d} for d in ("detailed", "compact")]),
            row("Summary: " + (", ".join(view["summary"]) or "title only"), "summary",
                [{"label": ("✓ " if f in view["summary"] else "  ") + f, "value": f} for f in agent_transcript.SUMMARY_FIELDS]),
            {"text": "Make these the default for agent panes", "command": "agents.view", "args": {"resource": rid, "set": "defaults"}}]

    def gear_panel(self, r):
        """The ⚙ popup (the human, relay-11: everything but the conversation and the message box lives here): the view
        filters, then this agent's settings, its bridge terminal and Stop (also in the pane menu, D15)."""
        rid = r["id"]
        items = [{"type": "header", "text": "View", "style": "heading"}]
        c = self.conversations.get(rid)
        if c is not None:
            with c.lock:
                seen = agent_transcript.senders(c.view.entities, agent_transcript.Names(self.ext.resources, rid, self.ext.owner()))
            items += self.view_rows(rid, self.view_of(rid), seen)
        else:
            items.append({"type": "text", "style": "dim", "text": "This agent doesn't stream its conversation."})
        items.append({"type": "header", "text": "Agent", "style": "heading"})
        if self.has_settings(r) and r.get("available"):
            items.append({"text": "Agent settings…", "command": "agents.config", "args": {"resource": rid}})
        host = self.ext.resources.get(r.get("host"))
        if host and host.get("kind") == "terminal":
            items.append({"text": "Bridge terminal (" + label(host.get("name") or host["id"], 60) + ")", "command": "agents.pane",
                          "args": {"resource": rid, "value": "bridge"}})
        if self.can_stop(r):
            items.append({"text": "Stop agent…", "style": "warn", "command": "agents.stop", "args": {"resource": rid}})
        return {"title": label(r.get("name") or "Agent") + " " + rid, "requires": ["menu"], "items": items}

    def pane_menu(self, args):
        """agents.pane {resource}: open the ⚙ popup; {value: bridge}: show the agent's bridge terminal (from it)."""
        # This is derived UI: the event-fed caches already supply its facts. A full workspace
        # snapshot also copies every live transcript, making a small popup scale with all agents.
        # Actions selected here validate their target again in their own command handlers.
        if args.get("value") == "bridge":
            self.ext.fresh()
        try:
            r = self.agent(args, inspect=True)
        except VqroError:
            if args.get("value") == "bridge":
                raise
            # A just-created resource may not have reached the event cache yet.
            self.ext.fresh()
            r = self.agent(args, inspect=True)
        if args.get("value") == "bridge":
            host = self.ext.resources.get(r.get("host"))
            if not host or host.get("kind") != "terminal":
                raise VqroError("this agent has no bridge terminal", "not_found")
            result = self.ext.invoke("tabs.show", {"resource": host["id"]})
            return {**(result if isinstance(result, dict) else {}), "client": {"close": True}}
        target = "agents-pane-" + r["id"]
        with self.lock:
            self.open_pane.add(r["id"])
            content = self.gear_panel(r)
            self.ext.publish(target, content)
            self.published_pane[r["id"]] = content
        return {"client": {"popup": {"view": target, "width": 60}}}

    def tools_panel(self, r, run_id):
        """The ⊞ Table popup of one turn's tools (mockup Screen C), from the conversation as it is now."""
        c = self.conversations.get(r["id"])
        session = agent_facts(r).get("session") if isinstance(agent_facts(r).get("session"), dict) else {}
        title = "Tools · " + label(r.get("name") or "Agent") + " " + r["id"] + " · this turn"
        if c is None:
            return {"title": title, "items": [{"type": "text", "style": "dim", "text": "This agent doesn't stream its conversation."}]}
        with c.lock:
            run = agent_transcript.tool_runs(c.view.entities).get(run_id, [])
        return agent_transcript.tools_table(run, title, session.get("cwd") if isinstance(session.get("cwd"), str) else None)

    def tools_table(self, args):
        """agents.tools {resource, run}: open the turn's tools table (republished while open, like the ⚙ popup)."""
        self.ext.fresh()
        r = self.agent(args, inspect=True)
        run = args.get("run")
        if not isinstance(run, str) or not run or len(run) > 256:
            raise VqroError("which turn? (run: its first tool id)", "invalid")
        target = "agents-tools-" + r["id"]
        with self.lock:
            self.open_tools[r["id"]] = run
            content = self.tools_panel(r, run)
            self.ext.publish(target, content)
            self.published_tools[r["id"]] = content
        return {"client": {"popup": {"view": target, "width": 120}}}

    def frame(self, r):
        """The pane's frame: name · folder as the title, the live state at its right end (feature-free fallback: the
        plain title). Only what the agent reports; nothing guessed."""
        name = label(r.get("name") or r.get("title") or "Agent") + " " + r["id"]
        session = agent_facts(r).get("session") if isinstance(agent_facts(r).get("session"), dict) else {}
        folder = session.get("cwd") if isinstance(session.get("cwd"), str) and session["cwd"] else None
        folder = agent_transcript.home_short(folder) if folder else None
        blocked = availability_label(r)
        state = blocked or status(r)
        waiting = bool(blocked) or state == "blocked" or bool(self.requests_v0(r) if self.v0(r, "respond") else None)
        style = "warn" if waiting or ended(r) or agent_facts(r).get("lifecycle") not in (None, "live") else "ok" if state == "idle" else "info"
        transform = agent_facts(r).get("input_transform")
        note = []
        if isinstance(transform, dict) and transform.get("state") == "may":
            names = agent_transcript.transform_names(transform.get("names"))
            if names == "extensions (unknown which)":
                names = "extensions (not named)"
            elif transform.get("names_complete") is not True:
                names += " and others"
            note = ["input may be changed by: " + names]
        return {"title": " · ".join([name] + ([head(plain(folder), 80)] if folder else []) + note),
                "title_right": "● " + state, "title_right_style": style}

    def request_presentation(self, r, waiting):
        configured = self.ext.value(REQUEST_ROUTES + r['id'])
        if configured is None or configured == {'route': 'human'}:
            return True, None
        if not isinstance(configured, dict) or configured.get('route') not in ('human', 'coordinator', 'both'):
            return True, 'Invalid request route; human fallback'
        if configured['route'] == 'human':
            return True, None
        route = self.ext.live.get('coord-requests-' + r['id'])
        keys = {request_key(r, question) for question in waiting}
        projected = route.get('request_keys') if isinstance(route, dict) else None
        if (not isinstance(projected, list) or any(not isinstance(key, str) for key in projected)
                or route.get('version') != self.ext.version(REQUEST_ROUTES + r['id'])
                or None in keys or keys != set(projected)):
            return True, 'Request routing unavailable or catching up; human fallback'
        return route.get('human') is not False, route.get('fallback_reason')

    def bottom_rows(self, r):
        """Pinned under the conversation (feature `pin`; otherwise simply last): what the runtime holds, what waits
        for you, the composer. The facts line follows."""
        rid, name = r["id"], plain(str(r.get("name") or r["id"]))
        rows = []
        queue = agent_facts(r).get("queue")
        if isinstance(queue, list) and queue:  # null: the runtime doesn't show its queue; nothing claimed
            held = [q for q in queue[:64] if isinstance(q, dict)]
            rows.append({"id": "queue", "pin": "bottom", "text": f"Messages · {len(queue)} held by {name}", "style": "dim",
                         "children": [{"type": "text", "style": "muted", "text": head(plain(f"{q.get('mode') or 'queued'}: {q.get('preview') or ''}"), 300)}
                                      for q in held], "expanded": False,
                         "menu": [{"label": "Clear queue", "command": "agents.clear_queue", "args": {"resource": rid}}]
                                 if self.supported(r, "clear_queue") else []})
        waiting = self.requests_v0(r) if self.v0(r, "respond") else None
        human, fallback = self.request_presentation(r, waiting) if waiting else (False, None)
        # Exactly one waiting row; non-transcript panels also select it by id.
        if waiting and not human:
            route = self.ext.live.get('coord-requests-' + rid) or {}
            coordinator = label(plain(str(route.get('coordinator') or 'the recorded coordinator')), 120)
            rows.append({'id': 'waiting', 'pin': 'bottom', 'style': 'warn',
                         'text': f'⚠ {name} is waiting for {coordinator}',
                         'description': 'Request routed to the coordinator; manual Control remains available'})
        if waiting and human:
            more = f" (and {len(waiting) - 1} more)" if len(waiting) > 1 else ""
            rows.append({"id": "waiting", "pin": "bottom", "style": "warn", "command": "agents.answer", "args": {"resource": rid},
                         "text": f"⚠ {name} is waiting for your answer: " + head(plain(str(waiting[0].get("title") or waiting[0]["request"])), 160) + more,
                         "description": (str(fallback) + ' · ' if fallback else '') + "Enter: answer…",
                         # Answer stays in the pane (button/Enter), never in a right-click menu.
                         "actions": [{"text": "Answer…", "command": "agents.answer", "args": {"resource": rid}}]})
        # The pane's one settings button, ⚙ (agents.pane: view filters, agent settings, bridge terminal, stop). The
        # user's icon set draws it (a bare "⚙" is ambiguous width: its highlight came out uneven).
        gear = {"text": "", "icon": {"name": "settings", "text": "⚙"}, "style": "control", "icon_style": "control", "command": "agents.pane", "args": {"resource": rid}}
        modes = self.modes(r)
        composing = bool(modes and r.get("available") and not ended(r))
        if composing:
            names = {"start": "start", "boundary": "steer", "settle": "follow-up", "admit": "admit (weaker)",
                     "preempt": "interrupt and send"}
            # admit stays available to agents and the API (agents.send mode admit), not as a chip for the human (mockup).
            options = [{"label": "auto", "value": "auto"}] + [
                {"label": names.get(m, m), "value": m} for m in modes if m not in ("start", "admit")]
            rows.append({"type": "composer", "id": "compose", "pin": "bottom", "text": "Message " + name,
                         "command": "agents.send", "args": {"resource": rid, "caps_rev": caps(r).get("rev")},
                         "placeholder": f"Write a message to {name}…", "rows": COMPOSER_ROWS, "max_bytes": 65536, "submit": "Enter",
                         "autofocus": True, "requires": ["composer"], "send_button": False,  # Enter sends (the human, relay-11)
                         # One input-settings button (the human, relay-11): the delivery mode now, more input options later.
                         # Then the pane's ⚙ (the human, 17:00: no top bar; the settings button at the bottom).
                         "controls": [{"type": "select", "arg": "mode", "text": "Input", "value": "auto", "options": options},
                                      {"type": "button", **gear}],
                         "fallback": {"text": f"Message {name}…", "command": "agents.send",
                                      "args": {"resource": rid, "mode": "auto"}, "input": "Message", "multiline": True}})
        # Clients without composer buttons (or a pane with no message box) get the ⚙ as a one-chip row at the bottom.
        rows.append({"type": "chips", "id": "gear", "pin": "bottom", "text": "Pane options", "chips": [gear],
                     **({"requires_absent": ["composer_buttons"]} if composing else {})})
        return rows

    def conversation_rows(self, r):
        """The agent's conversation as rows, or None when it has no item stream (today's panel stays)."""
        c = self.conversations.get(r["id"])
        if c is None:
            return None
        names = agent_transcript.Names(self.ext.resources, r["id"], self.ext.owner())
        with c.lock:
            notes = []
            if c.incompatible or c.rejections:
                notes.append({"type": "text", "style": "warn", "text": incompatible_note(c)})
            elif c.clamped:
                notes.append({"type": "text", "style": "dim", "text": "This agent declares more history than this pane keeps; "
                              "if it sends that much, the conversation stops updating and says so."})
            if c.view.epoch is None:
                return notes + [{"type": "text", "style": "dim", "text": "Loading the conversation…"}]
            session = agent_facts(r).get("session") if isinstance(agent_facts(r).get("session"), dict) else {}
            live = agent_transcript.notice_rows(c.notices, names.agent_name(r["id"]))
            out = notes + agent_transcript.rows(c.view.entities, names, c.view.coverage, self.view_of(r["id"]),
                                                session.get("cwd") if isinstance(session.get("cwd"), str) else None) + live
        if c.stream is None and r.get("available") is not False and not c.incompatible:  # stopped: the note says so
            out.append({"type": "text", "style": "dim", "text": "Reconnecting to the conversation…"})
        return out or [{"type": "text", "style": "dim", "text": "No messages yet."}]  # complete coverage only: rows() discloses others

    def provider_caps(self, resource):
        caps = self.ext.providers.get(resource.get("provider"), {}).get("capabilities", {})
        return caps if isinstance(caps, dict) else {}

    def v0(self, resource, op):
        """The provider declares the agent v0 op (`agent.send`, `agent.interrupt`, ...)."""
        return isinstance(self.provider_caps(resource).get("agent."+op), dict)

    def endpoint(self, resource, op):
        """This agent's own declaration of a v0 op (`data.agent.caps`), or None. The provider's declaration is only
        the upper bound (agent v0 section 2.4): an op absent here is unsupported on this agent, never sent as legacy."""
        value = caps(resource).get("agent."+op)
        return value if self.v0(resource, op) and isinstance(value, dict) else None

    def host_kill(self, r):
        """The agent's live host terminal, when its provider declares terminal.kill; else None."""
        host = host_of(self.ext.resources, r)
        if host is None or ended(host):
            return None
        declared = self.ext.providers.get(host.get("provider"), {}).get("capabilities", {})
        return host if isinstance(declared, dict) and isinstance(declared.get("terminal.kill"), dict) else None

    def can_stop(self, r):
        return not ended(r) and (self.host_kill(r) is not None or self.endpoint(r, "stop") is not None)

    def served(self, hid):
        """Every known agent the host terminal serves, as shown in the stop confirmation."""
        return sorted(a["id"] for a in self.ext.resources.values() if a.get("kind") == "agent" and a.get("host") == hid
                      and not ended(a))

    def modes(self, resource):
        """Delivery modes this agent declares in its own caps, in a fixed order; {} for legacy agents."""
        declared = (self.endpoint(resource, "send") or {}).get("modes")
        declared = declared if isinstance(declared, dict) else {}
        return {m: declared[m] for m in MODE_ORDER if isinstance(declared.get(m), dict)}

    def supported(self, resource, op):
        if op in V0_OP and self.v0(resource, V0_OP[op]):
            declared = self.endpoint(resource, V0_OP[op])
            if declared is None:
                return False
            if op in V0_MODE:
                return V0_MODE[op] in self.modes(resource) or (op == "prompt" and bool(self.modes(resource)))
            if op == "clear_queue":
                actions = declared.get("actions")
                return isinstance(actions, list) and "clear" in actions
            return True
        return isinstance(self.provider_caps(resource).get("agent."+op), dict)

    def pane_menu_flags(self, r):
        """Native applicability travels with the panel, never through per-turn workspace writes."""
        facts = agent_facts(r)
        available = r.get("available") is True and not ended(r) and facts.get("lifecycle") != "ended"
        # Legacy runtimes also publish data.agent config/commands; choose facts by the declared op,
        # not by that object's presence (the same dispatch rule as the action handler).
        if self.v0(r, "interrupt"):
            incomplete = facts.get("incomplete") if isinstance(facts.get("incomplete"), list) else []
            working = (facts.get("lifecycle") == "live" and facts.get("state") == "running"
                       and not any(field in incomplete for field in ("state", "lifecycle")))
        else:
            legacy = r.get("status") if isinstance(r.get("status"), dict) else {}
            working = legacy.get("state") in ("working", "running")
        if self.v0(r, "queue"):
            queue = facts.get("queue")
            queued = isinstance(queue, list) and any(isinstance(item, dict) and isinstance(item.get("mode"), str)
                                                    and bool(item["mode"]) for item in queue)
        else:
            queues = data(r).get("queues")
            queued = isinstance(queues, dict) and any(isinstance(queues.get(key), list)
                and any(isinstance(item, str) for item in queues[key]) for key in ("steering", "follow_up"))
        return {"agent_working": available and working and self.supported(r, "abort"),
                "agent_queued": available and queued and self.supported(r, "clear_queue"),
                # Core resources declare availability as a boolean, exactly the Forget handler's guard.
                "agent_forgettable": r.get("available") is False}

    def current_signature(self):
        return tuple(sorted((r["id"], tuple(op for op in EFFECTS if self.supported(r, op)), tuple(self.modes(r)),
                             self.has_settings(r), self.can_stop(r))
                            for r in self.ext.resources.values() if r.get("kind") == "agent"))

    def customization_rows(self, r):
        conversation = self.conversations.get(r["id"])
        if conversation is None:
            return []
        with conversation.lock:
            startup = conversation.view.entities.get(("startup", "current"), (None, None))[1]
            if not isinstance(startup, dict):
                return []
            inventory = {k: v for k, v in startup.items() if k in (
                "skills", "skills_scope", "prompts", "commands", "plugins", "mcp", "extensions", "sources", "incomplete")}
            row = agent_transcript.startup_row(inventory, hint=False)
        return row.get("children", []) if row else []

    def has_settings(self, r):
        return bool(self.customization_rows(r) or (self.supported(r, "configure") and advertised(r, "config")) or (self.supported(r, "command") and advertised(r, "commands")))

    def guarded(self, fn):
        def command(args):
            try:
                return fn(args)
            except VqroError as error:
                self.ext.notify(label(f"Agents: {error} ({error.kind}); no automatic retry."), "warn" if error.kind in ("timeout", "indeterminate") else "error")
                raise
        return command

    def agent(self, args, op=None, inspect=False):
        context = args.get("context") or {}
        rid = args.get("resource") or context.get("resource")
        for prefix in ("agents-", "agent-"):
            if not rid and str(context.get("view", "")).startswith(prefix):
                rid = context["view"][len(prefix):]
        resource = self.ext.resources.get(rid)
        if not resource or resource.get("kind") != "agent":
            raise VqroError("choose an agent pane or an agent from Agents", "invalid")
        if not inspect and (not resource.get("available", False) or resource.get("state") != "live"):
            raise VqroError("agent is unavailable; nothing sent", "unavailable")
        if op and not self.supported(resource, op):
            raise VqroError("agent provider does not declare agent."+op+"; nothing sent", "unsupported")
        return resource

    def show(self, args):
        self.ext.fresh()
        r = self.agent(args, inspect=True)
        c = self.conversations.get(r["id"])
        if c is not None and c.incompatible:  # showing the agent again retries an incompatible conversation
            with c.lock:
                self.renew(c, r["id"])
            self.wake(r["id"], reattach=True)
        self.ext.contribute(self.declarations([r]))
        self.publish_panel(r)
        result = self.ext.invoke("tabs.show", {"view": view(r["id"]), "title": label(r.get("name") or "Agent")+" "+r["id"]})
        return {"resource": r["id"], "view": view(r["id"]), "tab": result, "client": {"close": True, "focus": {"view": view(r["id"])}}}

    def stop(self, args):
        # Close owns consent for auto; unlike the standalone Stop command it must never open a popup.
        if args.get("how") != "auto":
            return self.stop_confirmed(args)
        try:
            if args.get("confirm") is not True:
                raise VqroError("close must confirm stopping the agent", "invalid")
            result = self.stop_confirmed(args)
            if result.get("outcome") not in ("accepted", "observed"):
                raise VqroError("stop outcome " + str(result.get("outcome", "unknown")) +
                                "; check the agent before trying again", "unknown")
            return result
        except VqroError as error:
            uncertain = error.kind in ("unknown", "timeout") or (error.data or {}).get("outcome") == "indeterminate"
            prefix = "stop not confirmed: " if uncertain else "not stopped: "
            raise VqroError(prefix + str(error), error.kind, data=error.data, wire_error=error.wire_error) from error

    def stop_effect(self, method, params, auto):
        try:
            result = self.ext.vqro.call(method, params)
        except VqroError as error:
            if not auto:
                raise
            # Kind alone is not evidence: unavailable can mean either side of dispatch.
            data = error.data if isinstance(error.data, dict) else {}
            outcome = "rejected" if data.get("outcome") == "rejected" else "indeterminate"
            evidence = {**data, "outcome": outcome}
            if error.wire_error is not None:
                evidence["wire_error"] = error.wire_error
            raise VqroError(str(error), error.kind, data=evidence, wire_error=error.wire_error) from error
        except (OSError, EOFError) as error:
            if not auto:
                raise
            raise VqroError(str(error), "unavailable", data={"outcome": "indeterminate"}) from error
        if auto and result.get("outcome") not in ("accepted", "observed"):
            outcome = "rejected" if result.get("outcome") == "rejected" else "indeterminate"
            raise VqroError("stop outcome " + str(result.get("outcome", "unknown")), "effect", data={**result, "outcome": outcome})
        return result

    def stop_notice(self, message, style):
        # The effect has already decided the outcome; UI delivery cannot undo it or invite replay.
        try:
            self.ext.notify(message, style)
        except Exception as error:
            try:
                self.ext.log("stop notification failed:", error)
            except Exception:
                pass

    def stop_confirmed(self, args):
        """End an agent, after confirmation: gracefully when it declares agent.stop (agent v0 section 6.7), or by
        killing its host terminal when that terminal's provider declares terminal.kill.

        Each choice is confirmed on what was shown: graceful stop on the caps revision and coverage, host kill on the
        host and every agent it serves. A change re-shows the choice. Only `how: "graceful"` or `"host_kill"` act;
        anything else is refused, and a graceful stop is never escalated to a kill.
        """
        self.ext.fresh()
        r = self.agent(args, inspect=True)
        rid, name = r["id"], label(r.get("name") or "Agent")
        how = args.get("how")
        auto = how == "auto"
        if args.get("confirm") is True and how not in ("auto", "graceful", "host_kill"):
            raise VqroError(f"unknown stop action {label(str(how), 40)}; nothing sent", "invalid")
        declared = self.endpoint(r, "stop") if r.get("available") and not ended(r) else None
        host = self.host_kill(r)
        if auto:
            # Select once from fresh facts; a graceful refusal never falls back to killing the host.
            how = "graceful" if declared is not None else "host_kill"
            if host and how == "host_kill":
                provider = self.ext.providers.get(r.get("provider")) or {}
                host_caps = caps(r).get("host")
                if (not isinstance(host_caps, dict) or host_caps.get("endpoints") != "one"
                        or provider.get("role") != "runtime" or provider.get("host") != host["id"]
                        or r.get("host") != host["id"]):
                    raise VqroError("host may serve other agents or lacks a launcher-owned binding; use Stop agent…", "unsupported")
            others = [a for a in self.served(host["id"]) if a != rid] if host and how == "host_kill" else []
            if others:
                raise VqroError("its terminal also runs " + ", ".join(
                    label(self.ext.resources[a].get("name") or a) for a in others), "conflict")
            args = dict(args, caps_rev=caps(r).get("rev"),
                        covers=(declared or {}).get("covers") if isinstance((declared or {}).get("covers"), list) else [],
                        host=host["id"] if host else None, serves=self.served(host["id"]) if host else [])
        if args.get("confirm") is not True:
            return self.stop_popup(r, declared, host)
        if how == "graceful":
            if declared is None:
                raise VqroError(f"{name} {rid} declares no graceful stop or is unavailable; nothing sent", "unsupported")
            covers = declared.get("covers") if isinstance(declared.get("covers"), list) else []
            if (args.get("caps_rev"), args.get("covers")) != (caps(r).get("rev"), covers):
                if not auto:
                    self.stop_popup(r, declared, host)
                raise VqroError(f"{name} {rid}: what a graceful stop covers changed since you confirmed; confirm again. "
                                "Nothing sent.", "stale")
            native = {"v": 0, "covers": covers, **({"caps_rev": args["caps_rev"]} if type(args["caps_rev"]) is int else {})}
            result = self.stop_effect("resource.invoke", {"resource": rid, "op": "agent.stop", "args": native}, auto)
            outcome = result.get("outcome", "unknown")
            what = {"accepted": "stop accepted; it is ended only when the runtime reports its end",
                    "observed": "the runtime reports it ended"}.get(outcome, outcome+"; watch the agent's status")
            self.stop_notice(label(f"Stop {name} {rid}: {what}"), "ok" if outcome in ("accepted", "observed") else "warn")
            return {**result, "client": {"close": True}}
        if host is None:
            gone = host_of(self.ext.resources, r)
            if gone is not None and ended(gone):
                raise VqroError(f"{name} {rid}: its bridge terminal {gone['id']} has already ended; nothing to stop", "invalid")
            raise VqroError(f"{name} {rid}: its host terminal can't be killed from vqro now; nothing sent.", "unsupported")
        hid = host["id"]
        if args.get("host") != hid or args.get("serves") != self.served(hid):
            if not auto:
                self.stop_popup(r, declared, host)
            raise VqroError(f"{name} {rid}: its host terminal or the agents it serves changed since you confirmed; "
                            "confirm again. Nothing sent.", "conflict")
        result = self.stop_effect("resource.kill", {"resource": hid}, auto)
        outcome = result.get("outcome", "unknown")
        if owns_lifecycle(r):
            what = {"accepted": "kill accepted; the agent is removed once its terminal ends",
                    "observed": "terminal ended"}.get(outcome, outcome+"; watch the agent's status")
        else:
            what = outcome+"; the agent's own end is unknown until its runtime reports it"
        self.stop_notice(label(f"Stop {name} {rid}: terminal {hid} {what}"), "ok" if outcome in ("accepted", "observed") else "warn")
        return {**result, "client": {"close": True}}

    def stop_popup(self, r, declared, host):
        rid, name = r["id"], label(r.get("name") or "Agent")
        if declared is None and host is None:
            hid = r.get("host")
            why = ("its provider ("+label(str(r.get("provider")))+") runs it outside any vqro terminal" if not hid
                   else "its bridge terminal "+label(str(hid))+" is not known to vqro, has ended or can't be killed")
            raise VqroError(f"{name} {rid} can't be stopped from vqro: {why}. Stop it where it runs; nothing sent.", "unsupported")
        items = []
        if declared is not None:
            covers = declared.get("covers") if isinstance(declared.get("covers"), list) else []
            what = ", ".join(str(c) for c in covers) if covers else "nothing declared"
            items += [{"type": "text", "text": label(f"Graceful stop asks the runtime to end; it covers: {what}.", 300)},
                      {"text": "Stop "+rid+" gracefully", "style": "warn", "command": "agents.stop",
                       "args": {"resource": rid, "how": "graceful", "confirm": True, "caps_rev": caps(r).get("rev"),
                                "covers": covers}}]
        if host is not None:
            hid = host["id"]
            program = label(" ".join(str(a) for a in (host.get("argv") or [])[:3]) or host.get("name") or hid, 200)
            served = self.served(hid)
            others = [a for a in served if a != rid]
            if owns_lifecycle(r):
                effect = (f"Killing terminal {hid} ({program}), the agent's bridge, ends the agent's runtime process inside it"
                          + (" and every agent it serves: "+", ".join(served) if others else "")
                          + "; work in progress is interrupted.")
            else:
                effect = (f"Killing terminal {hid} ({program}) ends only this agent's bridge"
                          + (" for "+", ".join(served) if others else "")
                          + ": its runtime runs elsewhere and may continue; its end stays unknown until it reports one.")
            items += [{"type": "text", "text": effect},
                      {"text": "Stop agent "+rid+" (kill terminal "+hid+")", "style": "error", "command": "agents.stop",
                       "args": {"resource": rid, "how": "host_kill", "host": hid, "serves": served, "confirm": True}}]
        items.append({"type": "text", "style": "warn", "text": "Undo can't bring it back, and nothing is resent. Esc cancels."})
        self.ext.publish("agents-stop-"+rid, {"title": "Stop "+name+" "+rid+"?", "items": items})
        return {"client": {"popup": {"view": "agents-stop-"+rid, "width": 76}}}

    def forget(self, args):
        """Explicit request only: drop an unavailable agent's cached facts (core resource.forget_cached).

        Nothing is sent to the runtime. If its bridge is alive and reconnects later, the agent shows up
        again (possibly under a new id). The core rejects the call if the agent reconnected meanwhile.
        """
        self.ext.fresh()
        r = self.agent(args, inspect=True)
        rid, name = r["id"], label(r.get("name") or "Agent")
        if r.get("available"):
            raise VqroError(f"{name} {rid} is connected; Forget is only for unavailable agents (Stop ends a running one). Nothing changed.", "invalid")
        result = self.ext.vqro.call("resource.forget_cached", {"resource": rid})
        self.ext.notify(label(f"Forgot {name} {rid}: cached facts removed; nothing was sent to its runtime"), "ok")
        return {**result, "client": {"close": True}}

    def effect(self, op, args):
        self.ext.fresh()
        r = self.agent(args, op)
        if op in V0_OP and self.v0(r, V0_OP[op]):
            return self.effect_v0(op, r, args)
        native = {}
        if op in ("prompt", "steer", "follow_up"):
            text = args.get("text")
            if not isinstance(text, str) or not text.strip():
                raise VqroError("message must not be empty", "invalid")
            native["message"] = text
        result = self.ext.vqro.call("resource.invoke", {"resource": r["id"], "op": "agent."+op, "args": native})
        self.outcome(op, result)
        return result

    def effect_v0(self, op, r, args):
        """Legacy pane actions on an agent v0 runtime: modes for messages, interrupt, queue clear."""
        if op in V0_MODE:
            text = args.get("text")
            if not isinstance(text, str) or not text.strip():
                raise VqroError("message must not be empty", "invalid")
            mode = V0_MODE[op]
            if op == "prompt" and (busy(r) or mode not in self.modes(r)):
                # Agent v0 section 3.1: no mode falls back to another. A busy agent gets an explicit choice.
                return self.choose_mode(r, text)
            return self.send_mode(r, mode, text)
        if op == "abort":
            result = self.ext.vqro.call("resource.invoke", {"resource": r["id"], "op": "agent.interrupt", "args": self.v0_args(r)})
        else:  # clear_queue
            native = {**self.v0_args(r), "action": "clear"}
            if isinstance(agent_facts(r).get("epoch"), str):
                native["epoch"] = agent_facts(r)["epoch"]
            result = self.ext.vqro.call("resource.invoke", {"resource": r["id"], "op": "agent.queue", "args": native})
        self.outcome(op, result)
        return result

    @staticmethod
    def v0_args(r):
        """Common agent v0 args: version and, when known, the caps revision the choice was made on."""
        rev = caps(r).get("rev")
        return {"v": 0, **({"caps_rev": rev} if type(rev) is int else {})}

    def send_mode(self, r, mode, text):
        """One agent.send in exactly the chosen, declared mode (agent v0 section 3.2), fenced by the caps revision
        it was chosen on."""
        if mode not in self.modes(r):
            raise VqroError("this agent does not declare that delivery mode; nothing sent", "unsupported")
        native = {**self.v0_args(r), "mode": mode, "content": [{"type": "text", "text": text}],
                  "delivery": "d:"+uuid.uuid4().hex,
                  # Identity (principal, actor, initiator) is the core's record of this call (C2); the adapter binds it
                  # and refuses any supplied value that differs (agent-v0 3.2). Only the classification is ours: a
                  # claim that a person typed this, rendered as claimed.
                  "origin": {"kind": "human"},
                  # The human's own words as the user (plain), when the core proves the sender is the owner; otherwise the
                  # adapter frames them with their attribution. Without this, adapters default to a framed rendering.
                  "render": ["user", "attributed_user"]}
        result = self.ext.vqro.call("resource.invoke", {"resource": r["id"], "op": "agent.send", "args": native})
        outcome = result.get("outcome", "unknown")
        receipt = (result.get("data") or {}).get("receipt") if isinstance(result.get("data"), dict) else None
        stage = receipt.get("stage") if isinstance(receipt, dict) else None
        detail = RECEIPT_TEXT.get(stage, "acceptance is not settlement: watch agent status" if outcome == "accepted" else
                                  "no runtime acknowledgement" if outcome == "dispatched" else "")
        self.ext.notify(label(MODE_SHORT.get(mode, mode)+": "+outcome+("; "+detail if detail else "")),
                        "ok" if outcome in ("accepted", "observed") else "warn")
        return {**result, "client": {"close": True}}

    def choose_mode(self, r, text=None):
        """Popup of the modes this agent declares; the human picks one. Nothing is sent until then."""
        rid, modes = r["id"], self.modes(r)
        if not modes:
            raise VqroError("this agent declares no delivery modes; nothing sent", "unsupported")
        target = "agents-send-"+rid
        items = [{"type": "text", "text": "The agent is busy. Choose how this message joins its work:" if busy(r)
                  else "Choose how to deliver this message:"}]
        for mode, descriptor in modes.items():
            row = {"text": mode_label(mode, descriptor), "command": "agents.send",
                   "args": {"resource": rid, "mode": mode, "caps_rev": caps(r).get("rev")}}
            if mode == "admit":
                row["description"] = "weaker: the runtime decides when it reads it"
            if text is None:
                row["input"] = "Message"
            else:
                row["args"]["text"] = text
            if mode == "start" and busy(r) and descriptor.get("busy") == "reject":
                row["description"] = "the agent is busy: the runtime will refuse this"
            items.append(row)
        self.ext.publish(target, {"title": "Send to "+label(r.get("name") or "Agent")+" "+rid, "requires": ["menu", "input"],
                                  "items": items})
        return {"client": {"popup": {"view": target, "width": 76}}}

    def send(self, args):
        """agents.send {resource, mode?, text?}: a declared mode sends at once; no mode opens the choice."""
        self.ext.fresh()
        r = self.agent(args, "send")
        text, mode = args.get("text"), args.get("mode")
        if isinstance(text, str) and len(text) > 1 and text.startswith("/") and not text[1].isspace():
            parts = text[1:].split(maxsplit=1)
            name = parts[0]
            entries = advertised(r, "commands")
            skill_aliases = {}
            conversation = self.conversations.get(r["id"])
            if conversation is not None:
                with conversation.lock:
                    startup = conversation.view.entities.get(("startup", "current"), (None, {}))[1]
                    if isinstance(startup, dict):
                        for field in ("commands", "prompts", "skills"):
                            inventory = startup.get(field)
                            if isinstance(inventory, list):
                                for entry in inventory:
                                    if not isinstance(entry, dict) or not isinstance(entry.get("name"), str):
                                        continue
                                    native_name = entry["name"]
                                    if field == "skills" and any(e.get("name") == "skill:" + native_name for e in entries):
                                        native_name = "skill:" + native_name
                                    if field == "skills" and native_name != entry["name"]:
                                        skill_aliases[entry["name"]] = native_name
                                    entries = entries + [{"name": native_name}]
            # Exact native commands/templates always win over optional short skill aliases.
            if not any(entry.get("name") == name for entry in entries):
                name = skill_aliases.get(name, name)
            if name and any(entry.get("name") == name for entry in entries):
                if not self.supported(r, "command"):
                    raise VqroError("explicit invocation requires agent.command; nothing sent", "unsupported")
                native = {"v": 0, "name": name}
                if len(parts) > 1:
                    native["args"] = parts[1]
                result = self.ext.vqro.call("resource.invoke", {"resource": r["id"], "op": "agent.command", "args": native})
                self.ext.notify(label("/" + name + ": " + str(result.get("outcome", "unknown"))), "dim")
                return {**result, "client": {"close": True}}
        if mode is None:
            return self.choose_mode(r, text if isinstance(text, str) and text.strip() else None)
        if not isinstance(text, str) or not text.strip():
            raise VqroError("message must not be empty", "invalid")
        if mode == "auto":  # the composer's default: start when evidenced idle, else ask (the agreed prefix-p rule)
            if busy(r) or "start" not in self.modes(r):
                return self.choose_mode(r, text)
            mode = "start"
        if "caps_rev" in args and args["caps_rev"] != caps(r).get("rev"):
            # Chosen from a popup whose declaration has changed: show the current choice, never rebind consent.
            self.choose_mode(r, text)
            raise VqroError("this agent's delivery modes changed since they were shown; choose again. Nothing sent.", "stale")
        return self.send_mode(r, mode, text)

    def report_status(self, args):
        """agents.status {terminal, app, state, seq, facts?}: a tier-1 plugin's report for its own terminal.

        Published as live data `agents.terminal_status` ({data: {terminal: {app, state, seq, at_ms, status}}}), where
        `status` is OSC 7501-shaped for readers of terminal status. Lossy and advisory: an unknown state value
        becomes `unknown`, a report with seq not above the last one is ignored, and nothing is kept across a
        restart of this extension. Only the terminal's launcher-tagged principal (resource.principal) or the
        workspace owner may report (C2); others get not_authorized. Errors go to the caller only, never to a notice.
        Optional `facts` are view facts the plugin read from its runtime (`tier1_facts`); a report without `facts`
        keeps the last ones while the app is unchanged (a different app starts with none), and a report with `facts`
        replaces them. They are published as the entry's `facts`, never inferred or merged.
        """
        terminal, app, seq = args.get("terminal"), args.get("app"), args.get("seq")
        if not isinstance(terminal, str) or not terminal or len(terminal) > 64:
            raise VqroError("terminal must be a resource id", "invalid")
        if not isinstance(app, str) or not app or len(app) > 64 or not app.isprintable():
            raise VqroError("app must be 1 to 64 printable characters", "invalid")
        if type(seq) is not int or not 0 <= seq <= TIER1_SEQ_MAX:
            raise VqroError("seq must be an integer from 0 to 2^53-1 (Unix epoch ms recommended)", "invalid")
        r = self.ext.resources.get(terminal)
        if r is None:
            self.ext.fresh()
            r = self.ext.resources.get(terminal)
        if r is None or r.get("kind", "terminal") != "terminal" or ended(r):
            raise VqroError(f"{label(terminal, 64)} is not a live terminal; nothing recorded", "not_found")
        # Only the terminal's own launcher-tagged principal (resource.principal, recorded by the core at spawn) or the
        # workspace owner may report its status. Unknown (null) or undeclared tags authorise nobody but the owner.
        caller = (self.ext.caller() or {}).get("principal")
        tag, owner = r.get("principal"), self.ext.owner()
        known = isinstance(tag, str) and tag != "undeclared"
        if not (owner is not None and caller == owner) and not (known and caller == tag):
            raise VqroError(f"only {label(terminal, 64)}'s launcher-tagged principal or the workspace owner may report its "
                            "status; nothing recorded", "not_authorized")
        state = args.get("state") if args.get("state") in TIER1_STATES else "unknown"
        facts = tier1_facts(args["facts"]) if args.get("facts") is not None else None
        with self.tier1_lock:
            previous = self.tier1.get(terminal)
            stale = previous is not None and seq <= previous["seq"]
            if not stale:
                if previous is None and len(self.tier1) >= TIER1_MAX:
                    raise VqroError("too many reporting terminals; nothing recorded", "overloaded")
                entry = {"app": app, "state": state, "seq": seq, "at_ms": int(time.time() * 1000),
                         "status": {"state": TIER1_STATES[state], "app": app}}
                # Kept facts belong to the app that reported them: a different app starts with none.
                kept = facts if facts is not None else (previous or {}).get("facts") if (previous or {}).get("app") == app else None
                if kept:
                    entry["facts"] = kept
                size = len(json.dumps({terminal: entry}, ensure_ascii=False, separators=(",", ":")).encode())
                if sum(self.tier1_size.values()) - self.tier1_size.get(terminal, 0) + size > TIER1_BYTES:
                    raise VqroError("the published status table would exceed its size bound; nothing recorded", "overloaded")
                self.tier1[terminal], self.tier1_size[terminal] = entry, size
            # Publish a change; and a table left unpublished by a failed publication goes out with any report,
            # changed, unchanged or stale.
            publish = self.tier1_dirty or not stale and (previous is None or (previous["state"], previous["app"], previous.get("facts"))
                                                          != (state, app, self.tier1[terminal].get("facts")))
            current = self.tier1[terminal]["seq"]
        if publish:
            self.publish_tier1()
        return {"ok": True, "applied": False, "reason": "stale", "seq": current} if stale else {"ok": True, "applied": True}

    def publish_tier1(self):
        """Publish the current table. One publisher at a time, always from a fresh snapshot: a report arriving
        while another publishes marks the table dirty and that publisher sends it again, so an older snapshot
        never lands after a newer one and no change is skipped."""
        with self.tier1_lock:
            self.tier1_dirty = True
            if self.tier1_publishing:
                return
            self.tier1_publishing = True
        try:
            while True:
                with self.tier1_lock:
                    if not self.tier1_dirty:
                        self.tier1_publishing = False
                        return
                    self.tier1_dirty = False
                    data = {rid: dict(entry) for rid, entry in self.tier1.items()}
                self.ext.publish("agents.terminal_status", {"data": data})
        except BaseException:
            with self.tier1_lock:
                self.tier1_publishing, self.tier1_dirty = False, True  # the next report publishes again
            raise

    def prune_tier1(self):
        with self.tier1_lock:
            gone = [rid for rid in self.tier1 if rid not in self.ext.resources or ended(self.ext.resources[rid])]
            for rid in gone:
                del self.tier1[rid]
                self.tier1_size.pop(rid, None)
        if gone:
            self.publish_tier1()

    def outcome(self, op, result):
        outcome = result.get("outcome", "unknown")
        extra = "; acceptance is not settlement: watch agent status" if outcome == "accepted" else "; no runtime acknowledgement" if outcome == "dispatched" else ""
        self.ext.notify(label(EFFECTS[op]+": "+outcome+extra), "ok" if outcome in ("accepted", "observed") else "warn")

    def requests_v0(self, r):
        """Pending requests from agent v0 facts (section 5): summaries with request id, revision and method."""
        rows = agent_facts(r).get("requests")
        return [q for q in rows if isinstance(q, dict) and isinstance(q.get("request"), str)
                and q.get("status", "pending") == "pending"] if isinstance(rows, list) else None

    def requests_panel_v0(self, r):
        rid, items, groups = r["id"], [], []
        requests = self.requests_v0(r)
        if requests is None:
            requests = []
            items.append({"type": "text", "style": "warn", "text": "Request facts incomplete/unavailable; nothing assumed answered."})
        epoch = agent_facts(r).get("epoch")
        for q in requests:
            args = {"resource": rid, "request": q["request"], "epoch": epoch, "revision": q.get("revision")}
            method, choices = q.get("method"), q.get("choices")
            if not isinstance(epoch, str) or type(q.get("revision")) is not int:
                items.append({"type": "text", "style": "warn", "text": label((q.get("title") or q["request"])
                              + ": no epoch or revision to answer against; answer it in the agent's own interface.")})
                continue
            row = {"text": "Answer: "+head(plain(q.get("title") or q["request"])), "description": label(str(method)),
                   "command": "agents.answer", "args": args}
            params = q.get("params")
            shown, complete = disclosure(request_parts(q))
            if not complete:
                row = {"type": "text", "style": "warn", "text": "The request is too long to show here whole, so it can't be "
                       "approved; answer it in the agent's own screen. Cancel stays available."}
            elif q.get("incomplete"):  # true or a non-empty list of cut fields
                row = {"type": "text", "style": "warn", "text": label((q.get("title") or q["request"])
                       + ": details incomplete, so it can't be approved here; Cancel stays available.")}
            elif not isinstance(params, dict):
                # The native details (a confirmation's message, say) are the question: no approval without them.
                row = {"type": "text", "style": "warn", "text": label((q.get("title") or q["request"])
                       + ": details not available here, so it can't be approved; Cancel stays available.")}
            elif method == "confirm":
                row["options"] = [{"label": "Yes", "value": True}, {"label": "No", "value": False}]
            elif method in ("choice", "permission") and isinstance(choices, list) and choices:
                # Each choice is disclosed whole above (request_parts); menu labels lead with number and effect.
                row["options"] = [{"label": choice_label(n, c), "value": c["id"]} for n, c in enumerate(choices_of(q), 1)]
            elif method == "text":
                row["input"] = label(params.get("placeholder") if isinstance(params.get("placeholder"), str)
                                     else q.get("title") or "Answer")
                if isinstance(params.get("prefill"), str):
                    row["prefill"] = params["prefill"]
                if params.get("multiline") is True:
                    row["multiline"] = True  # clients without multi-line prompts take one line
            else:
                row = {"type": "text", "style": "warn", "text": label((q.get("title") or q["request"])+": "+str(method)
                       + " can't be answered from this panel; Cancel stays available.")}
            groups.append((shown + [row], {"text": "Cancel "+label(q["request"]), "command": "agents.answer",
                                           "args": {**args, "cancelled": True}}))
        return bounded_requests(rid, items, groups)

    def answer_v0(self, r, args):
        """agent.respond with the request's epoch and revision: a stale answer is refused by the adapter."""
        request = next((q for q in self.requests_v0(r) or [] if q["request"] == args["request"]), None)
        if request is None:
            raise VqroError("that request is no longer pending; nothing sent", "not_found")
        epoch = agent_facts(r).get("epoch")
        if not isinstance(epoch, str) or type(request.get("revision")) is not int:
            raise VqroError("this request has no epoch or revision to answer against; nothing sent", "invalid")
        if (args.get("epoch"), args.get("revision")) != (epoch, request["revision"]):
            # An answer is consent to the question shown: a revised or re-issued request is shown again, never
            # answered with the old choice (agent v0 section 5).
            if r["id"] in self.open_requests:
                self.ext.publish("agents-requests-"+r["id"], self.requests_panel(r))
            raise VqroError("the request changed since it was shown; check it again. Nothing sent.", "stale")
        method, choices = request.get("method"), request.get("choices")
        ids = [c.get("id") for c in choices if isinstance(c, dict)] if isinstance(choices, list) else []
        if args.get("cancelled") is True:
            response = {"action": "cancel"}
        elif request.get("incomplete") or not isinstance(request.get("params"), dict) or not disclosure(request_parts(request))[1]:
            raise VqroError("request details are incomplete, missing or too long to show; only Cancel is possible", "invalid")
        elif method == "confirm" and type(args.get("value")) is bool:
            response = {"value": args["value"]}
        elif method in ("choice", "permission") and args.get("value") in ids:
            response = {"choice": args["value"]}
        elif method == "text" and isinstance(args.get("text"), str):
            response = {"value": args["text"]}
        else:
            raise VqroError("choose a valid answer for this request", "invalid")
        native = {"v": 0, "request": request["request"], "response": response, "revision": args["revision"],
                  "epoch": args["epoch"]}
        registered = self.ext.vqro.call('command.list', {}).get('commands')
        if not isinstance(registered, list):
            raise VqroError('command registration could not be inspected; nothing sent', 'unavailable')
        names = {row['name'] for row in registered if isinstance(row, dict) and isinstance(row.get('name'), str)}
        if not {'coord.inbox.window', 'coord.control'} <= names:
            # D15: positively absent commands, before attempting a coord effect.
            # Never fall back after ANY registered command error/timeout.
            result = self.ext.vqro.call('resource.invoke', {'resource': r['id'], 'op': 'agent.respond', 'args': native})
            observed = result
        else:
            window = self.ext.invoke('coord.inbox.window', {})
            if not isinstance(window, dict) or not isinstance(window.get('epoch'), str):
                raise VqroError('coordination response path unavailable; nothing sent', 'unavailable')
            # A deliberate retry is a new op; the adapter's epoch/revision fence
            # (not a replay) preserves first-valid-answer-wins.
            result = self.ext.invoke('coord.control', {'op': window['epoch'] + ':agents-answer:' + uuid.uuid4().hex,
                                     'target': r['id'], 'action': 'respond', 'args': native})
            observed = result.get('native') or {'outcome': result.get('state', 'unknown')}
        self.outcome('answer', observed)
        return {**result, 'outcome': observed.get('outcome', 'unknown'), 'client': {'close': True}}

    def requests_panel(self, r):
        if self.v0(r, "respond"):
            return self.requests_panel_v0(r)
        items, groups = [], []
        requests = data(r).get("requests")
        if not isinstance(requests, list):
            requests = []
            items.append({"type": "text", "style": "warn", "text": "Request facts incomplete/unavailable; nothing assumed answered."})
        requests = [q for q in requests if isinstance(q, dict) and isinstance(q.get("id"), str)]
        for request in requests:
            detail = request.get("data") if isinstance(request.get("data"), dict) else {}
            kind = request.get("kind")
            args = {"resource": r["id"], "request": request["id"]}
            row = {"text": "Answer: "+head(plain(request.get("prompt") or request["id"])), "description": label(str(kind)),
                   "command": "agents.answer", "args": args}
            shown, complete = disclosure(legacy_parts(request))
            if not complete:
                row = {"type": "text", "style": "warn", "text": "The request is too long to show here whole, so it can't be "
                       "approved; answer it in the agent's own screen. Cancel stays available."}
            elif kind == "confirm":
                row["options"] = [{"label": "Yes", "value": True}, {"label": "No", "value": False}]
            elif kind == "select" and isinstance(detail.get("options"), list):
                row["options"] = [{"label": head(f"{n}. " + plain(str(v))), "value": v} for n, v in enumerate(detail["options"], 1)]
            elif kind in ("input", "editor"):
                row["input"] = head(plain(request.get("prompt") or "Answer"))
            else:
                row = {"type": "text", "text": label("Unsupported request kind: "+str(kind)), "style": "warn"}
            groups.append((shown + [row], {"text": "Cancel "+label(request["id"]), "command": "agents.answer",
                                           "args": {**args, "cancelled": True}}))
        return bounded_requests(r["id"], items, groups)

    def answer(self, args):
        """Answer a pending request. Approval needs the whole question to have been showable (disclosure)."""
        self.ext.fresh()
        r = self.agent(args, "answer")
        rid = r["id"]
        if "request" not in args:
            target = "agents-requests-"+rid
            self.open_requests.add(rid)
            self.ext.publish(target, self.requests_panel(r))
            return {"client": {"popup": {"view": target, "width": 80}}}
        if self.v0(r, "respond"):
            return self.answer_v0(r, args)
        requests = data(r).get("requests")
        request = next((q for q in requests if isinstance(q, dict) and q.get("id") == args["request"]), None) if isinstance(requests, list) else None
        if request is None:
            raise VqroError("that request is no longer pending; nothing sent", "not_found")
        detail = request.get("data") if isinstance(request.get("data"), dict) else {}
        native = {"id": request["id"]}
        if args.get("cancelled") is True:
            native["cancelled"] = True
        elif not disclosure(legacy_parts(request))[1]:
            raise VqroError("this request is too long to show whole; only Cancel is possible", "invalid")
        elif request.get("kind") == "confirm" and type(args.get("value")) is bool:
            native["confirmed"] = args["value"]
        elif request.get("kind") == "select" and isinstance(detail.get("options"), list) and args.get("value") in detail["options"]:
            native["value"] = args["value"]
        elif request.get("kind") in ("input", "editor") and isinstance(args.get("text"), str):
            native["value"] = args["text"]
        else:
            raise VqroError("choose a valid answer for this request", "invalid")
        result = self.ext.vqro.call("resource.invoke", {"resource": rid, "op": "agent.answer", "args": native})
        self.outcome("answer", result)
        return {**result, "client": {"close": True}}

    @staticmethod
    def shown_value(entry):
        value = entry.get("value")
        if entry.get("type") == "boolean":
            return "on" if value is True else "off" if value is False else "unknown"
        choice = next((c for c in entry.get("choices") or [] if isinstance(c, dict) and c.get("value") == value), None)
        return label(str((choice or {}).get("label") or value if value is not None else "unknown"), 120)

    def settings_panel(self, r):
        """Generic rows from the agent's own declarations; nothing is assumed about a runtime."""
        rid, items = r["id"], []
        config = advertised(r, "config") if self.supported(r, "configure") else []
        commands = advertised(r, "commands") if self.supported(r, "command") else []
        if config:
            items.append({"type": "header", "text": "Settings", "style": "heading"})
        for entry in config:
            if entry.get("type") == "boolean":
                options = [{"label": "On", "value": True}, {"label": "Off", "value": False}]
            else:
                options = [{"label": label(str(c.get("label") or c.get("value")), 120), "value": c.get("value")}
                           for c in entry.get("choices") or [] if isinstance(c, dict)]
            # `rev` makes a choice made on an outdated popup fail as stale instead of overriding a newer value.
            items.append({"type": "select", "text": label(entry.get("label") or entry["id"], 80), "value": entry.get("value"),
                          "options": options, "command": "agents.configure",
                          "args": {"resource": rid, "id": entry["id"], **({"rev": entry["rev"]} if "rev" in entry else {})},
                          "description": label(entry.get("description") or self.shown_value(entry), 300)})
        if commands:
            items.append({"type": "header", "text": "Commands", "style": "heading"})
        for entry in commands:
            row = {"text": label("/"+entry["name"] if not entry.get("label") else entry["label"], 80), "command": "agents.command",
                   "args": {"resource": rid, "name": entry["name"]}}
            if entry.get("description"):
                row["description"] = label(entry["description"], 300)
            if isinstance(entry.get("input"), dict):
                row["input"] = label(entry["input"].get("hint") or entry["name"], 120)
            items.append(row)
        customizations = self.customization_rows(r)
        if customizations:
            items.append({"type": "header", "text": "Customizations", "style": "heading"})
            items.append({"type": "text", "text": "Read-only native inventory; absent details are not reported.", "style": "dim"})
            items.extend(customizations)
        if not items:
            items = [{"type": "text", "text": "This agent declares no settings or commands."}]
        if not r.get("available") or ended(r):
            items.insert(0, {"type": "text", "style": "warn", "text": "Agent unavailable; nothing will be sent."})
        return {"title": "Agent settings: "+label(r.get("name") or "Agent")+" "+rid, "requires": ["menu", "input"], "items": items}

    def settings(self, args):
        self.ext.fresh()
        r = self.agent(args, inspect=True)
        if not self.has_settings(r):
            raise VqroError("this agent declares no settings or commands", "unsupported")
        target = "agents-settings-"+r["id"]
        self.open_settings.add(r["id"])
        self.ext.publish(target, self.settings_panel(r))
        return {"client": {"popup": {"view": target, "width": 76}}}

    def configure(self, args):
        self.ext.fresh()
        r = self.agent(args, "configure")
        entry = next((c for c in advertised(r, "config") if c["id"] == args.get("id")), None)
        value = args.get("value")
        if entry is None:
            raise VqroError("this agent does not declare that setting; nothing sent", "unsupported")
        valid = isinstance(value, bool) if entry.get("type") == "boolean" else any(
            isinstance(c, dict) and c.get("value") == value for c in entry.get("choices") or [])
        if not valid:
            raise VqroError("choose one of the declared values; nothing sent", "invalid")
        native = {"v": 0, "id": entry["id"], "value": value, **({"rev": args["rev"]} if isinstance(args.get("rev"), int) else {})}
        result = self.ext.vqro.call("resource.invoke", {"resource": r["id"], "op": "agent.configure", "args": native})
        outcome = result.get("outcome", "unknown")
        shown = self.shown_value(dict(entry, value=value))
        self.ext.notify(label(f"{entry.get('label') or entry['id']}: {shown} ({outcome}"
                              + ("; the agent has not confirmed it yet" if outcome != "observed" else "") + ")"),
                        "ok" if outcome == "observed" else "warn")
        return result

    def command(self, args):
        self.ext.fresh()
        r = self.agent(args, "command")
        entry = next((c for c in advertised(r, "commands") if c["name"] == args.get("name")), None)
        if entry is None:
            raise VqroError("this agent does not declare that command; nothing sent", "unsupported")
        native = {"v": 0, "name": entry["name"]}
        if isinstance(entry.get("input"), dict) and isinstance(args.get("text"), str) and args["text"].strip():
            native["args"] = args["text"]  # a string (agent v0 6.3)
        result = self.ext.vqro.call("resource.invoke", {"resource": r["id"], "op": "agent.command", "args": native})
        outcome = result.get("outcome", "unknown")
        self.ext.notify(label(f"/{entry['name']}: {outcome}"
                              + ("; acceptance is not completion: watch agent status" if outcome == "accepted" else "")),
                        "ok" if outcome in ("accepted", "observed") else "warn")
        return {**result, "client": {"close": True}}

    def panel(self, r):
        title = label(r.get("name") or r.get("title") or "Agent")+" "+r["id"]
        declared = data(r).get("views")
        if isinstance(declared, list):
            sources = list(dict.fromkeys(v for v in declared[:8] if isinstance(v, str) and v))
        else:
            canonical = "agent-"+r["id"]
            sources = [canonical] if canonical in self.ext.live or "view/"+canonical in self.ext.entries else []
        conversation = self.conversation_rows(r)
        if conversation is not None:
            # The agent's own item stream: the conversation is drawn here (agent pane, K3 phase 2).
            content = {**self.frame(r), "follow": True, "search": True,
                       "items": [*conversation, *self.bottom_rows(r),
                agent_transcript.facts_footer(agent_facts(r), plain(str(r.get("name") or r["id"])))]}
        elif sources:
            # Keep controls/warnings here, but never copy a provider's transcript into another
            # publication. Generic client composition enforces read-only source rows.
            content = {"title": title, "requires": ["embedded_views"], "items": [
                {"text": title, "badges": [{"text": status(r), "style": "dim"}]},
                *[{"type": "view", "view": source, "readonly": True,
                   "text": "Native output unavailable or unsupported: "+label(source)} for source in sources]]}
        else:
            items = [{"text": title, "badges": [{"text": status(r), "style": "dim"}],
                      "description": label("provider: "+str(r.get("provider"))+" | available="+str(r.get("available")))}]
            details = data(r)
            if isinstance(details.get("run"), dict):
                items.append({"type": "text", "text": label("Run: "+json.dumps(details["run"]), 2048)})
            if isinstance(details.get("queues"), dict):
                items.append({"type": "text", "text": label("Queues: "+json.dumps(details["queues"]), 4096)})
            requests = details.get("requests")
            items.append({"type": "text", "text": "Pending requests: "+(str(len(requests)) if isinstance(requests, list) else "unknown")})
            items.append({"type": "text", "text": label(json.dumps(details, ensure_ascii=False), 8192)})
            content = {"title": title, "requires": ["badges", "description"], "items": items}
        if conversation is None:
            # Waiting is agent state, not a transcript feature; keep it visible
            # for runtimes without an item stream or with an embedded view too.
            content['items'].extend(row for row in self.bottom_rows(r) if row.get('id') == 'waiting')
        details = data(r)
        failure = details.get("failure")
        if isinstance(failure, dict) and isinstance(failure.get("message"), str) and failure["message"]:
            content["items"].insert(0, {"type": "text", "style": "error", "text": "Agent error: "+label(failure["message"], 4096)})
        retry = details.get("retry")
        if isinstance(retry, dict):
            active, success = retry.get("active") is True, retry.get("success")
            text = "Retrying" if active else "Retry succeeded" if success is True else "Retry failed" if success is False else "Retry"
            attempt, maximum = retry.get("attempt"), retry.get("max_attempts")
            if type(attempt) is int and attempt > 0:
                text += f" (attempt {attempt}"
                text += f"/{maximum}" if type(maximum) is int and maximum > 0 else ""
                text += ")"
            if active and type(retry.get("delay_ms")) is int and retry["delay_ms"] >= 0:
                text += f"; reported delay {retry['delay_ms']} ms"
            content["items"].insert(0, {"type": "text", "style": "warn" if active else "error" if success is False else "dim", "text": label(text)})
        if self.has_settings(r) and r.get("available") and conversation is None:
            content["items"].insert(0, {"text": "Agent settings…", "description": label(", ".join(
                f"{c.get('label') or c['id']}: {self.shown_value(c)}" for c in advertised(r, "config")[:4]), 300),
                "command": "agents.config", "args": {"resource": r["id"]}})
        host = self.ext.resources.get(r.get("host"))
        if host and host.get("kind") == "terminal" and conversation is None:
            content["items"].insert(0, {"text": "Bridge terminal", "description": label(host.get("name") or host["id"]),
                                         "command": "tabs.show", "args": {"resource": host["id"]}})
        if ended(r):
            content["items"].insert(0, {"type": "text", "style": "warn", "text": "Agent ended; no operation will be sent. Close this tab or use Close ended tabs."})
        elif not r.get("available"):
            content["items"].insert(0, {"type": "text", "style": "warn", "text": self.disconnected(r)})
        content["menu_context"] = self.pane_menu_flags(r)
        bound = 32768 if conversation is None else agent_transcript.BUDGET + 32768  # the conversation bounds itself
        size = len(json.dumps(content, ensure_ascii=False).encode())
        if size > bound:  # drop the oldest rows after the first; each row's size is measured once
            first, rest = content["items"][0], content["items"][1:]
            sizes = [len(json.dumps(row, ensure_ascii=False).encode()) + 1 for row in rest]
            cut = 0
            while size > bound and cut < len(rest):
                size -= sizes[cut]
                cut += 1
            content["items"] = [{"type": "text", "style": "warn", "text": "Bounded agent panel: some provider output omitted."},
                                first, *rest[cut:]]
        return content

    def disconnected(self, r):
        """Honest wording for an unavailable agent: dead bridge, or waiting for a reconnect."""
        host = host_of(self.ext.resources, r)
        if host is not None and ended(host):
            return f"Agent ended: its bridge terminal {host['id']} has finished, so it can't reconnect. Removing it."
        since = unknown_since(self.ext, r)
        source = f"its bridge (terminal {host['id']}, still running)" if host is not None else "its provider"
        when = f" since {since}" if since else ""
        stop = f"Stop ends terminal {host['id']}; " if host is not None else ""
        return (f"Agent unavailable: state unknown{when}; {source} disconnected and displayed facts may be stale. "
                f"vqro keeps it until it reconnects. {stop}Forget removes only these facts (pane menu).")

    def list_agents(self, args):
        self.ext.fresh()
        self.list_open = True
        self.publish_list()
        return {"client": {"popup": {"view": "agents-list", "width": 80}}}

    def publish_list(self):
        rows = [{"text": label(r.get("name") or "Agent")+" "+r["id"], "description": status(r),
                 "command": "agents.show", "args": {"resource": r["id"]},
                 "badges": [{"text": "ended", "style": "dim"}] if ended(r) else
                           [] if r.get("available") else [unknown_badge(self.ext, r)],
                 "menu": [{"label": "Show", "command": "agents.show", "args": {"resource": r["id"]}},
                          *([{"label": "Agent settings…", "command": "agents.config", "args": {"resource": r["id"]}}]
                            if self.has_settings(r) and r.get("available") else []),
                          *([{"label": "Stop agent…", "command": "agents.stop", "args": {"resource": r["id"]}}]
                            if self.can_stop(r) else []),
                          *([{"label": "Forget agent", "command": "agents.forget", "args": {"resource": r["id"]}}]
                            if not r.get("available") and not ended(r) else [])]}
                for r in self.ext.resources.values() if r.get("kind") == "agent"]
        count = len(rows)
        rows = rows[:128]
        if count > 128:
            rows.append({"type": "text", "style": "warn", "text": f"Showing 128 of {count} agents; existing agent panes remain usable."})
        self.ext.publish("agents-list", {"title": "Agents", "requires": ["menu"],
                                         "items": rows or [{"type": "text", "text": "No agents observed."}]})

    def declarations(self, resources=None):
        entries = {}
        for r in list(self.ext.resources.values()) if resources is None else resources:
            if r.get("kind") != "agent":
                continue
            rid = r["id"]
            # `resource` names the agent this pane shows: context.resource / resource_kind for keys and menus.
            entries["view/"+view(rid)] = {"kind": "panel", "title": "Agent "+rid, "resource": rid, "items": [{"type": "text", "text": "Agents extension is not running."}]}
            # Every popup this agent can open is declared with the agent, not on click. A popup reply names a view the
            # client must already know: a view written by the same click can reach a client after the reply, and the
            # popup is then dropped ("popup: no view"). Opening a popup writes no workspace state.
            for popup, title in (("stop", "Stop agent"), ("requests", "Pending requests"), ("settings", "Agent settings"),
                                 ("send", "Send to agent"), ("pane", "Agent pane"), ("tools", "Tools")):
                entries[f"view/agents-{popup}-{rid}"] = {"kind": "panel", "title": f"{title} {rid}", "items": [
                    {"type": "text", "text": "Loading… (stays if the agents extension is not running)"}]}
            # Static mouse-only entries; clients join the panel's live scalars as menu.<key>.
            actions = [("abort", "Abort", "agents.abort", {"menu.agent_working": True}, self.supported(r, "abort")),
                       ("clear_queue", "Clear queue", "agents.clear_queue", {"menu.agent_queued": True}, self.supported(r, "clear_queue")),
                       ("stop", "Stop agent…", "agents.stop", {"live": True}, self.can_stop(r)),
                       ("settings", "Agent settings…", "agents.config", {"live": True}, self.has_settings(r)),
                       ("pane", "Pane options…", "agents.pane", {"live": True}, True),
                       ("forget", "Forget agent", "agents.forget", {"menu.agent_forgettable": True}, True)]
            for i, (key, title, command, condition, supported) in enumerate(actions):
                if supported:
                    entries[f"menu/agents.{rid}.{key}"] = {"where": "pane", "label": title, "command": command,
                        "args": {"resource": rid}, "when": {"view": view(rid), **condition},
                        "group": "agent", "order": 100 + i}
        return entries

    def refresh(self, prune_views=False):
        with self.lock:
            signature = self.current_signature()
            changed = signature != self.signature
            if not changed and not prune_views:
                return
            entries = self.declarations()
            def prune():
                with self.ext.cache_lock:
                    shown = {view for key, entry in self.ext.entries.items() if key.startswith("layout/")
                             for view in views_in(entry.get("value", {}))}
                    stale = []
                    for key, entry in self.ext.entries.items():
                        if not isinstance(entry.get("value"), dict) or entry["value"].get("owner") != "agents":
                            continue
                        rid = next((key[len(prefix):] for prefix in OWNED_PREFIXES if key.startswith(prefix)), "")
                        orphan = rid.startswith("r") and rid[1:].isascii() and rid[1:].isdigit() and rid not in self.ext.resources
                        if ((key.startswith("menu/agents.r") and key not in entries)
                                or (orphan and key[5:] not in shown)):
                            stale.append(key)
                    ops = [{"op": "delete", "key": key, "expect": self.ext.version(key)} for key in stale]
                if stale:
                    self.ext.apply(ops, "agents: stale actions/panels")
                    for key in stale:
                        if key.startswith("view/"):
                            self.ext.publish(key[5:], None)  # release this publisher's derived cache too
            try:
                self.ext.retrying(prune)
            except VqroError as error:
                if error.kind != "invalid":
                    raise
                # A layout can reference a view after our scan; atomic validation refused the
                # delete. Leave it intact and let the subsequent entries event re-evaluate.
            if changed:
                self.ext.contribute(entries)
            self.signature = signature

    def publish_panel(self, resource):
        rid = resource["id"]
        # Commands (agents.show after its snapshot) and events both publish this panel. Read the
        # resource from the cache inside the lock, so the last publication always uses the newest
        # facts: an older copy can never overwrite the embedded native view with the fallback.
        with self.lock:
            resource = self.ext.resources.get(rid) or resource
            try:
                content = self.panel(resource)
                with self.log_lock:
                    self.logged.pop((rid, "pane"), None)  # a good draw: the next failure is news again
            except Exception as error:  # noqa: BLE001 - this agent's pane says so; the others keep updating
                if self.strict:
                    raise
                self.log_failure(rid, "pane", error)
                content = broken_panel(resource, error)
            if self.published.get(rid) != content:
                self.ext.publish(view(rid), content)
                self.published[rid] = content
            if rid in self.open_tools:
                try:
                    table = self.tools_panel(resource, self.open_tools[rid])
                except Exception as error:  # noqa: BLE001
                    if self.strict:
                        raise
                    self.log_failure(rid, "tools table", error)
                    table = broken_panel(resource, error)
                if self.published_tools.get(rid) != table:
                    self.ext.publish("agents-tools-" + rid, table)
                    self.published_tools[rid] = table
            if rid in self.open_pane:
                try:
                    gear = self.gear_panel(resource)
                except Exception as error:  # noqa: BLE001
                    if self.strict:
                        raise
                    self.log_failure(rid, "pane options", error)
                    gear = broken_panel(resource, error)
                if self.published_pane.get(rid) != gear:
                    self.ext.publish("agents-pane-" + rid, gear)
                    self.published_pane[rid] = gear

    def publish_gone(self, rid):
        self.published.pop(rid, None)
        with self.log_lock:  # the agent is gone: so are its logged errors
            for key in [k for k in self.logged if k[0] == rid]:
                del self.logged[key]
        self.open_pane.discard(rid)
        self.published_pane.pop(rid, None)
        self.open_tools.pop(rid, None)
        self.published_tools.pop(rid, None)
        entry = self.ext.entries.get("view/"+view(rid), {})
        if entry.get("value", {}).get("owner") == "agents":
            self.ext.publish(view(rid), {"items": [{"type": "text", "style": "warn", "text": "Agent gone (facts not currently available); no operation sent. Close this tab or use Close ended tabs."}]})

    def publish_panels(self, resources=None):
        if resources is None:
            self.published = {rid: content for rid, content in self.published.items() if rid in self.ext.resources}
        for r in list(self.ext.resources.values()) if resources is None else resources:
            if r.get("kind") == "agent":
                self.isolated(r.get("id"), "publish", self.publish_panel, r)
        if resources is None:
            for key, entry in list(self.ext.entries.items()):
                if panel_key(key) and entry["value"].get("owner") == "agents" and key[12:] not in self.ext.resources:
                    self.publish_gone(key[12:])

    def event(self, kind, event):
        if kind in ("entries", "resource", "resource.removed", "provider", "missed"):
            self.refresh(prune_views=kind in ("entries", "resource.removed", "missed"))
            if kind != "entries":
                self.sync_conversations()
            else:  # a pane's view settings or the defaults changed: redraw those panes
                keys = [c.get("key", "") for c in event.get("changes") or [] if isinstance(c, dict)]
                if VIEW_DEFAULTS in keys:
                    self.publish_panels([self.ext.resources[rid] for rid in self.conversations if rid in self.ext.resources])
                else:
                    for key in keys:
                        r = self.ext.resources.get(key[len(VIEW_PANE):]) if key.startswith(VIEW_PANE) else None
                        if r is not None:
                            self.publish_panel(r)
            if kind in ("resource", "resource.removed", "missed") and self.tier1:
                self.prune_tier1()
            if kind == "entries":
                return
            changed = event.get("id") if kind in ("resource", "resource.removed") else None
            selected = [r for r in self.ext.resources.values() if r.get("kind") == "agent" and (r["id"] == changed or r.get("host") == changed)] if changed else None
            self.publish_panels(selected)
            if kind == "resource.removed" and changed:
                self.publish_gone(changed)
            if self.list_open and (not changed or self.ext.resources.get(changed, {}).get("kind") == "agent" or kind == "resource.removed"):
                self.publish_list()
            for rid in tuple(self.open_settings):
                if changed and rid != changed:
                    continue
                r = self.ext.resources.get(rid)
                self.ext.publish("agents-settings-"+rid, self.settings_panel(r) if r else {"items": [{"type": "text", "text": "Agent is gone; nothing sent."}]})
                if not r:
                    self.open_settings.discard(rid)
            for rid in tuple(self.open_requests):
                if changed and rid != changed:
                    continue
                r = self.ext.resources.get(rid)
                if r:
                    self.ext.publish("agents-requests-"+rid, self.requests_panel(r))
                else:
                    self.ext.publish("agents-requests-"+rid, {"items": [{"type": "text", "text": "Agent is gone; nothing sent."}]})
                    self.open_requests.discard(rid)
        elif kind == 'view.content' and str(event.get('view', '')).startswith('coord-requests-'):
            r = self.ext.resources.get(event['view'][len('coord-requests-'):])
            if r and r.get('kind') == 'agent':
                self.publish_panel(r)
        elif kind == "view.content" and str(event.get("view", "")).startswith("agent-"):
            r = self.ext.resources.get(event["view"][6:])
            if r and r.get("kind") == "agent":
                self.publish_panel(r)


def main():
    ext = Extension("agents", event_types=["entries", "resource", "resource.removed", "provider", "view.content"],
                    view_content={"prefixes": ["agent-", "coord-requests-"]})
    Agents(ext)
    ext.run()


if __name__ == "__main__":
    main()
