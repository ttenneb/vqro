#!/usr/bin/env python3
"""Runtime-neutral agent panels and native capability-driven controls. No launcher."""
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
# (64-char id, 64 printable 4-byte characters of app twice, maximum seq) encodes to 700 B, and 1024 of them fit
# TIER1_BYTES with room to spare. The byte check at admission is the backstop.
TIER1_MAX = 1024
TIER1_BYTES = 1024 * 1024 - 64 * 1024
TIER1_SEQ_MAX = 2 ** 53 - 1  # exact in every JSON reader
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
OWNED_PREFIXES = ("view/agents-requests-", "view/agents-settings-", "view/agents-stop-", "view/agents-send-", "view/agents-")


def host_of(resources, r):
    """The agent's bridge terminal resource, or None when it has none vqro knows."""
    host = resources.get(r.get("host")) if r.get("host") else None
    return host if host and host.get("kind") == "terminal" else None


def unknown_badge(ext, r):
    """Badge for an unavailable agent: "unknown since HH:MM" when the core says when it lost it."""
    since = unknown_since(ext, r)
    return {"text": "unknown since "+since if since else "unknown", "style": "warn"}


class Conversation:
    """One agent's item stream (agent-v0 section 4) and its reconstruction. The SDK calls the sink on its reader
    thread: it only feeds bytes and wakes the worker, never calls the core."""

    def __init__(self, rid, wake, bounds):
        self.rid, self.wake = rid, wake
        self.view = ItemView(**bounds)
        self.lock = threading.Lock()
        self.stream = None
        self.generation = 0  # bumped when a stream ends, so a late attach reply can't revive it

    def data(self, data):
        with self.lock:
            self.view.feed(data)
            broken = self.view.broken
        self.wake(self.rid, reattach=broken)

    def error(self, error):
        pass

    def ended(self, kind, reason):
        with self.lock:
            self.view.stream_ended()
            self.stream = None
            self.generation += 1
        self.wake(self.rid, reattach=True)


class Agents:
    def __init__(self, ext):
        self.ext, self.lock = ext, threading.RLock()
        self.open_requests = set()
        self.open_settings = set()
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
        ext.command("agents.status", "Report a tier-1 agent's state for its terminal (args: terminal, app, state, seq)")(self.report_status)
        for name, fn, title in [("list", self.list_agents, "Show agents"), ("show", self.show, "Show an agent in a tab"),
                                ("config", self.settings, "Agent settings and commands (popup)"),
                                ("configure", self.configure, "Change an agent setting (args: id, value)"),
                                ("command", self.command, "Run an agent command (args: name, text?)"),
                                ("send", self.send, "Send a message in a declared delivery mode (args: mode?, text?)"),
                                ("stop", self.stop, "Stop an agent: kill its bridge terminal and the runtime in it (asks first)"),
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
                                                  "when": {"resource_kind": "agent"}}}
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
                    self.conversations[r["id"]] = Conversation(r["id"], self.wake, {
                        k: bounds[f] for k, f in (("max_entities", "entities"), ("max_entity_bytes", "entity_bytes"))
                        if type(bounds.get(f)) is int and bounds[f] > 0})
                    self.wake(r["id"], reattach=True)
                elif self.conversations[r["id"]].stream is None:
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
        with c.lock:
            if c.stream is not None and not c.view.broken:
                return
            old, c.stream = c.stream, None
            # After lost records the cursor can't be trusted (a record past the loss may already be applied, as in
            # the Rust ItemView): start over from a fresh snapshot rather than continue from it.
            lost = c.view.broken
            if old is not None:
                c.view.stream_ended()
                c.generation += 1
            after, generation = None if lost else c.view.after(), c.generation
        if old is not None:
            self.ext.close_stream(old)  # data was lost: reattach from the last installed cursor
        try:
            reply = self.ext.attach_items(rid, c, after)
        except VqroError:
            timer = threading.Timer(1.0, self.wake, (rid,), {"reattach": True})
            timer.daemon = True
            timer.start()
            return
        with c.lock:
            if c.generation == generation and isinstance(reply.get("stream"), int):
                c.stream = reply["stream"]

    def flush_conversations(self):
        """One worker pass: (re)attach what needs it, then republish changed panes."""
        with self.conv_lock:
            attach, dirty = self.conv_attach, self.conv_dirty
            self.conv_attach, self.conv_dirty = set(), set()
        for rid in attach:
            self.attach(rid)
        for rid in dirty:
            r = self.ext.resources.get(rid)
            if r is not None:
                self.publish_panel(r)

    def conversation_worker(self):
        while True:
            self.conv_event.wait()
            time.sleep(0.15)  # coalesce a burst of streamed text into one publication
            self.conv_event.clear()
            try:
                self.flush_conversations()
            except Exception as error:  # report, keep the worker alive
                print(f"agents: conversation update failed: {error}", file=sys.stderr, flush=True)

    def conversation_rows(self, r):
        """The agent's conversation as rows, or None when it has no item stream (today's panel stays)."""
        c = self.conversations.get(r["id"])
        if c is None:
            return None
        # Until callers carry principals (C2), a human sender without a principal is the workspace's user ("You").
        names = agent_transcript.Names(self.ext.resources, r["id"])
        with c.lock:
            if c.view.epoch is None:
                return [{"type": "text", "style": "dim", "text": "Loading the conversation…"}]
            out = agent_transcript.rows(c.view.entities, names, c.view.coverage)
        if c.stream is None and r.get("available") is not False:
            out.append({"type": "text", "style": "dim", "text": "Reconnecting to the conversation…"})
        return out or [{"type": "text", "style": "dim", "text": "No messages yet."}]

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

    def current_signature(self):
        return tuple(sorted((r["id"], tuple(op for op in EFFECTS if self.supported(r, op)), tuple(self.modes(r)),
                             self.has_settings(r), self.can_stop(r))
                            for r in self.ext.resources.values() if r.get("kind") == "agent"))

    def has_settings(self, r):
        return bool((self.supported(r, "configure") and advertised(r, "config")) or (self.supported(r, "command") and advertised(r, "commands")))

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
        self.ext.contribute(self.declarations([r]))
        self.publish_panel(r)
        result = self.ext.invoke("tabs.show", {"view": view(r["id"]), "title": label(r.get("name") or "Agent")+" "+r["id"]})
        return {"resource": r["id"], "view": view(r["id"]), "tab": result, "client": {"close": True, "focus": {"view": view(r["id"])}}}

    def stop(self, args):
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
        if args.get("confirm") is True and how not in ("graceful", "host_kill"):
            raise VqroError(f"unknown stop action {label(str(how), 40)}; nothing sent", "invalid")
        declared = self.endpoint(r, "stop") if r.get("available") and not ended(r) else None
        host = self.host_kill(r)
        if args.get("confirm") is not True:
            return self.stop_popup(r, declared, host)
        if how == "graceful":
            if declared is None:
                raise VqroError(f"{name} {rid} declares no graceful stop or is unavailable; nothing sent", "unsupported")
            covers = declared.get("covers") if isinstance(declared.get("covers"), list) else []
            if (args.get("caps_rev"), args.get("covers")) != (caps(r).get("rev"), covers):
                self.stop_popup(r, declared, host)
                raise VqroError(f"{name} {rid}: what a graceful stop covers changed since you confirmed; confirm again. "
                                "Nothing sent.", "stale")
            native = {"v": 0, "covers": covers, **({"caps_rev": args["caps_rev"]} if type(args["caps_rev"]) is int else {})}
            result = self.ext.vqro.call("resource.invoke", {"resource": rid, "op": "agent.stop", "args": native})
            outcome = result.get("outcome", "unknown")
            what = {"accepted": "stop accepted; it is ended only when the runtime reports its end",
                    "observed": "the runtime reports it ended"}.get(outcome, outcome+"; watch the agent's status")
            self.ext.notify(label(f"Stop {name} {rid}: {what}"), "ok" if outcome in ("accepted", "observed") else "warn")
            return {**result, "client": {"close": True}}
        if host is None:
            gone = host_of(self.ext.resources, r)
            if gone is not None and ended(gone):
                raise VqroError(f"{name} {rid}: its bridge terminal {gone['id']} has already ended; nothing to stop", "invalid")
            raise VqroError(f"{name} {rid}: its host terminal can't be killed from vqro now; nothing sent.", "unsupported")
        hid = host["id"]
        if args.get("host") != hid or args.get("serves") != self.served(hid):
            self.stop_popup(r, declared, host)
            raise VqroError(f"{name} {rid}: its host terminal or the agents it serves changed since you confirmed; "
                            "confirm again. Nothing sent.", "conflict")
        result = self.ext.vqro.call("resource.kill", {"resource": hid})
        outcome = result.get("outcome", "unknown")
        if owns_lifecycle(r):
            what = {"accepted": "kill accepted; the agent is removed once its terminal ends",
                    "observed": "terminal ended"}.get(outcome, outcome+"; watch the agent's status")
        else:
            what = outcome+"; the agent's own end is unknown until its runtime reports it"
        self.ext.notify(label(f"Stop {name} {rid}: terminal {hid} {what}"), "ok" if outcome in ("accepted", "observed") else "warn")
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
                  # The effective principal comes from the core once principals by connection land (C2).
                  "origin": {"principal": "undeclared", "actor": "agents", "kind": "human"}}
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
        self.ext.contribute({"view/"+target: {"kind": "panel", "title": "Send to agent", "items": []}})
        self.ext.publish(target, {"title": "Send to "+label(r.get("name") or "Agent")+" "+rid, "requires": ["menu", "input"],
                                  "items": items})
        return {"client": {"popup": {"view": target, "width": 76}}}

    def send(self, args):
        """agents.send {resource, mode?, text?}: a declared mode sends at once; no mode opens the choice."""
        self.ext.fresh()
        r = self.agent(args, "send")
        text, mode = args.get("text"), args.get("mode")
        if mode is None:
            return self.choose_mode(r, text if isinstance(text, str) and text.strip() else None)
        if not isinstance(text, str) or not text.strip():
            raise VqroError("message must not be empty", "invalid")
        if "caps_rev" in args and args["caps_rev"] != caps(r).get("rev"):
            # Chosen from a popup whose declaration has changed: show the current choice, never rebind consent.
            self.choose_mode(r, text)
            raise VqroError("this agent's delivery modes changed since they were shown; choose again. Nothing sent.", "stale")
        return self.send_mode(r, mode, text)

    def report_status(self, args):
        """agents.status {terminal, app, state, seq}: a tier-1 plugin's report for its own terminal.

        Published as live data `agents.terminal_status` ({data: {terminal: {app, state, seq, at_ms, status}}}), where
        `status` is OSC 7501-shaped for readers of terminal status. Lossy and advisory: an unknown state value
        becomes `unknown`, a report with seq not above the last one is ignored, and nothing is kept across a
        restart of this extension. Not attributed: any caller can report for any terminal until callers carry
        a principal (C2). Errors go to the caller only, never to a notice.
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
        state = args.get("state") if args.get("state") in TIER1_STATES else "unknown"
        with self.tier1_lock:
            previous = self.tier1.get(terminal)
            stale = previous is not None and seq <= previous["seq"]
            if not stale:
                if previous is None and len(self.tier1) >= TIER1_MAX:
                    raise VqroError("too many reporting terminals; nothing recorded", "overloaded")
                entry = {"app": app, "state": state, "seq": seq, "at_ms": int(time.time() * 1000),
                         "status": {"state": TIER1_STATES[state], "app": app}}
                size = len(json.dumps({terminal: entry}, ensure_ascii=False, separators=(",", ":")).encode())
                if sum(self.tier1_size.values()) - self.tier1_size.get(terminal, 0) + size > TIER1_BYTES:
                    raise VqroError("the published status table would exceed its size bound; nothing recorded", "overloaded")
                self.tier1[terminal], self.tier1_size[terminal] = entry, size
            # Publish a change; and a table left unpublished by a failed publication goes out with any report,
            # changed, unchanged or stale.
            publish = self.tier1_dirty or not stale and (previous is None or (previous["state"], previous["app"]) != (state, app))
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
        result = self.ext.vqro.call("resource.invoke", {"resource": r["id"], "op": "agent.respond", "args": native})
        self.outcome("answer", result)
        return {**result, "client": {"close": True}}

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
            self.ext.contribute({"view/"+target: {"kind": "panel", "title": "Pending requests", "items": []}})
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
            items.append({"type": "header", "text": "Settings"})
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
            items.append({"type": "header", "text": "Commands"})
        for entry in commands:
            row = {"text": label("/"+entry["name"] if not entry.get("label") else entry["label"], 80), "command": "agents.command",
                   "args": {"resource": rid, "name": entry["name"]}}
            if entry.get("description"):
                row["description"] = label(entry["description"], 300)
            if isinstance(entry.get("input"), dict):
                row["input"] = label(entry["input"].get("hint") or entry["name"], 120)
            items.append(row)
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
        self.ext.contribute({"view/"+target: {"kind": "panel", "title": "Agent settings", "items": []}})
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
            content = {"title": title, "follow": True, "items": [
                {"text": title, "badges": [{"text": status(r), "style": "dim"}]}, *conversation]}
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
        if self.has_settings(r) and r.get("available"):
            content["items"].insert(0, {"text": "Agent settings…", "description": label(", ".join(
                f"{c.get('label') or c['id']}: {self.shown_value(c)}" for c in advertised(r, "config")[:4]), 300),
                "command": "agents.config", "args": {"resource": r["id"]}})
        host = self.ext.resources.get(r.get("host"))
        if host and host.get("kind") == "terminal":
            content["items"].insert(0, {"text": "Bridge terminal", "description": label(host.get("name") or host["id"]),
                                         "command": "tabs.show", "args": {"resource": host["id"]}})
        if ended(r):
            content["items"].insert(0, {"type": "text", "style": "warn", "text": "Agent ended; no operation will be sent. Close this tab or use Close ended tabs."})
        elif not r.get("available"):
            content["items"].insert(0, {"type": "text", "style": "warn", "text": self.disconnected(r)})
        bound = 32768 if conversation is None else agent_transcript.BUDGET + 32768  # the conversation bounds itself
        if len(json.dumps(content, ensure_ascii=False).encode()) > bound:
            content["items"].insert(0, {"type": "text", "style": "warn", "text": "Bounded agent panel: some provider output omitted."})
            while len(json.dumps(content, ensure_ascii=False).encode()) > bound and len(content["items"]) > 1:
                del content["items"][1]
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
            # Declared with the agent, not on click: confirming a stop writes no workspace state.
            entries["view/agents-stop-"+rid] = {"kind": "panel", "title": "Stop agent "+rid, "items": [{"type": "text", "text": "Agents extension is not running."}]}
            entries["menu/agents."+rid+".stop"] = {"where": "pane", "label": "Stop agent…", "command": "agents.stop",
                                                  "when": {"view": view(rid)}, "order": 190}
            # Static (menus are workspace entries; no churn per disconnect): the command refuses connected agents.
            entries["menu/agents."+rid+".forget"] = {"where": "pane", "label": "Forget agent (if unavailable)", "command": "agents.forget",
                                                    "when": {"view": view(rid)}, "order": 191}
            for i, (op, title) in enumerate(EFFECTS.items()):
                if not self.supported(r, op):
                    continue
                menu = {"where": "pane", "label": title, "command": "agents."+op, "when": {"view": view(rid)}, "order": 100+i}
                if op in ("prompt", "steer", "follow_up"):
                    menu["input"] = title
                entries["menu/agents."+rid+"."+op] = menu
            if self.modes(r):
                entries["menu/agents."+rid+".send"] = {"where": "pane", "label": "Send…", "command": "agents.send",
                                                       "when": {"view": view(rid)}, "order": 98}
            if self.has_settings(r):
                entries["menu/agents."+rid+".settings"] = {"where": "pane", "label": "Agent settings…", "command": "agents.config",
                                                           "when": {"view": view(rid)}, "order": 99}
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
            content = self.panel(resource)
            if self.published.get(rid) != content:
                self.ext.publish(view(rid), content)
                self.published[rid] = content

    def publish_gone(self, rid):
        self.published.pop(rid, None)
        entry = self.ext.entries.get("view/"+view(rid), {})
        if entry.get("value", {}).get("owner") == "agents":
            self.ext.publish(view(rid), {"items": [{"type": "text", "style": "warn", "text": "Agent gone (facts not currently available); no operation sent. Close this tab or use Close ended tabs."}]})

    def publish_panels(self, resources=None):
        if resources is None:
            self.published = {rid: content for rid, content in self.published.items() if rid in self.ext.resources}
        for r in list(self.ext.resources.values()) if resources is None else resources:
            if r.get("kind") == "agent":
                self.publish_panel(r)
        if resources is None:
            for key, entry in list(self.ext.entries.items()):
                if panel_key(key) and entry["value"].get("owner") == "agents" and key[12:] not in self.ext.resources:
                    self.publish_gone(key[12:])

    def event(self, kind, event):
        if kind in ("entries", "resource", "resource.removed", "provider", "missed"):
            self.refresh(prune_views=kind in ("entries", "resource.removed", "missed"))
            if kind != "entries":
                self.sync_conversations()
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
        elif kind == "view.content" and str(event.get("view", "")).startswith("agent-"):
            r = self.ext.resources.get(event["view"][6:])
            if r and r.get("kind") == "agent":
                self.publish_panel(r)


def main():
    ext = Extension("agents", event_types=["entries", "resource", "resource.removed", "provider", "view.content"],
                    view_content={"prefixes": ["agent-"]})
    Agents(ext)
    ext.run()


if __name__ == "__main__":
    main()
