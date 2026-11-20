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
from vqro_display import label
from vqro_views import ended, unknown_since

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
TIER1_MAX = 4096
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
    """Treat anything but evidenced idle as busy (agent v0 section 2.3)."""
    state = agent_facts(resource).get("state")
    if isinstance(state, str):
        return state != "idle"
    value = resource.get("status")
    return not (isinstance(value, dict) and value.get("state") in ("idle", "done"))


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


class Agents:
    def __init__(self, ext):
        self.ext, self.lock = ext, threading.RLock()
        self.open_requests = set()
        self.open_settings = set()
        self.published = {}  # Only current agents; suppress unchanged derived panel RPCs.
        self.list_open = False
        self.tier1 = {}  # terminal id -> {app, state, seq, at_ms, status}
        self.tier1_lock = threading.Lock()
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
        declarations = {"menu/agents.list": {"where": "tabbar", "label": "Agents", "command": "agents.list", "order": 6},
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

    def provider_caps(self, resource):
        caps = self.ext.providers.get(resource.get("provider"), {}).get("capabilities", {})
        return caps if isinstance(caps, dict) else {}

    def v0(self, resource, op):
        """The provider declares the agent v0 op (`agent.send`, `agent.interrupt`, ...)."""
        return isinstance(self.provider_caps(resource).get("agent."+op), dict)

    def modes(self, resource):
        """Delivery modes this agent declares in its own caps, in a fixed order; {} for legacy agents."""
        if not self.v0(resource, "send"):
            return {}
        declared = (caps(resource).get("agent.send") or {}).get("modes")
        declared = declared if isinstance(declared, dict) else {}
        return {m: declared[m] for m in MODE_ORDER if isinstance(declared.get(m), dict)}

    def supported(self, resource, op):
        if op in V0_OP and self.v0(resource, V0_OP[op]):
            if op in V0_MODE:
                return V0_MODE[op] in self.modes(resource) or (op == "prompt" and bool(self.modes(resource)))
            if op == "clear_queue":
                actions = (caps(resource).get("agent.queue") or {}).get("actions")
                return isinstance(actions, list) and "clear" in actions
            return True
        return isinstance(self.provider_caps(resource).get("agent."+op), dict)

    def current_signature(self):
        return tuple(sorted((r["id"], tuple(op for op in EFFECTS if self.supported(r, op)), tuple(self.modes(r)),
                             self.has_settings(r), self.v0(r, "stop"))
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
        """End an agent, after confirmation: gracefully when its runtime declares agent.stop (agent v0 section 6.7),
        or by killing its host (bridge) terminal, whose end takes the runtime with it.

        The confirmation names everything the host terminal serves. Nothing escalates silently: a graceful stop
        that the runtime refuses leaves the agent running, and the host kill is a separate, explicit choice.
        """
        self.ext.fresh()
        r = self.agent(args, inspect=True)
        rid, name = r["id"], label(r.get("name") or "Agent")
        host = host_of(self.ext.resources, r)
        graceful = self.v0(r, "stop") and r.get("available") and not ended(r)
        if args.get("how") == "graceful" and args.get("confirm") is True:
            if not graceful:
                raise VqroError(f"{name} {rid} declares no graceful stop or is unavailable; nothing sent", "unsupported")
            covers = (caps(r).get("agent.stop") or {}).get("covers")
            native = {**self.v0_args(r), "covers": covers if isinstance(covers, list) else []}
            result = self.ext.vqro.call("resource.invoke", {"resource": rid, "op": "agent.stop", "args": native})
            outcome = result.get("outcome", "unknown")
            what = {"accepted": "stop accepted; it is ended only when the runtime reports its end",
                    "observed": "the runtime reports it ended"}.get(outcome, outcome+"; watch the agent's status")
            self.ext.notify(label(f"Stop {name} {rid}: {what}"), "ok" if outcome in ("accepted", "observed") else "warn")
            return {**result, "client": {"close": True}}
        if host is None and not graceful:
            why = ("its provider ("+label(str(r.get("provider")))+") runs it outside any vqro terminal" if not r.get("host")
                   else "its bridge terminal "+label(str(r["host"]))+" is not known to vqro right now")
            raise VqroError(f"{name} {rid} can't be stopped from vqro: {why}. Stop it where it runs; nothing sent.", "unsupported")
        hid = host["id"] if host else None
        if host is not None and ended(host) and not graceful:
            raise VqroError(f"{name} {rid}: its bridge terminal {hid} has already ended; nothing to stop", "invalid")
        if args.get("confirm") is not True:
            items = []
            if graceful:
                covers = (caps(r).get("agent.stop") or {}).get("covers")
                what = ", ".join(str(c) for c in covers) if isinstance(covers, list) and covers else "what the runtime decides"
                items += [{"type": "text", "text": label(f"Graceful stop asks the runtime to end; it covers: {what}.", 300)},
                          {"text": "Stop "+rid+" gracefully", "style": "warn", "command": "agents.stop",
                           "args": {"resource": rid, "how": "graceful", "confirm": True}}]
            if host is not None and not ended(host):
                program = label(" ".join(str(a) for a in (host.get("argv") or [])[:3]) or host.get("name") or hid, 200)
                served = [a["id"] for a in self.ext.resources.values() if a.get("kind") == "agent" and a.get("host") == hid]
                items += [{"type": "text", "text": f"Killing terminal {hid} ({program}), the agent's bridge, ends the agent's runtime process inside it"
                           + (" and every agent it serves: "+", ".join(served) if len(served) > 1 else "")
                           + "; work in progress is interrupted."},
                          {"text": "Stop agent "+rid+" (kill terminal "+hid+")", "style": "error", "command": "agents.stop",
                           "args": {"resource": rid, "host": hid, "confirm": True}}]
            items.append({"type": "text", "style": "warn", "text": "Undo can't bring it back, and nothing is resent. Esc cancels."})
            self.ext.publish("agents-stop-"+rid, {"title": "Stop "+name+" "+rid+"?", "items": items})
            return {"client": {"popup": {"view": "agents-stop-"+rid, "width": 76}}}
        if host is None or args.get("host") != hid:
            raise VqroError(f"{name} {rid}: its bridge terminal changed since you confirmed; confirm again. Nothing sent.", "conflict")
        result = self.ext.vqro.call("resource.kill", {"resource": hid})
        outcome = result.get("outcome", "unknown")
        what = {"accepted": "kill accepted; the agent is removed once its terminal ends",
                "observed": "terminal ended"}.get(outcome, outcome+"; watch the agent's status")
        self.ext.notify(label(f"Stop {name} {rid}: terminal {hid} {what}"), "ok" if outcome in ("accepted", "observed") else "warn")
        return {**result, "client": {"close": True}}

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
        """One agent.send in exactly the chosen, declared mode (agent v0 section 3.2)."""
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
            row = {"text": mode_label(mode, descriptor), "command": "agents.send", "args": {"resource": rid, "mode": mode}}
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
        if not isinstance(terminal, str) or not terminal:
            raise VqroError("terminal must be a resource id", "invalid")
        if not isinstance(app, str) or not app or len(app) > 64:
            raise VqroError("app must be a non-empty string of at most 64 characters", "invalid")
        if type(seq) is not int or seq < 0:
            raise VqroError("seq must be a non-negative integer (Unix epoch ms recommended)", "invalid")
        r = self.ext.resources.get(terminal)
        if r is None:
            self.ext.fresh()
            r = self.ext.resources.get(terminal)
        if r is None or r.get("kind", "terminal") != "terminal" or ended(r):
            raise VqroError(f"{label(terminal, 64)} is not a live terminal; nothing recorded", "not_found")
        state = args.get("state") if args.get("state") in TIER1_STATES else "unknown"
        with self.tier1_lock:
            previous = self.tier1.get(terminal)
            if previous is not None and seq <= previous["seq"]:
                return {"ok": True, "applied": False, "reason": "stale", "seq": previous["seq"]}
            if previous is None and len(self.tier1) >= TIER1_MAX:
                raise VqroError("too many reporting terminals; nothing recorded", "overloaded")
            entry = {"app": app, "state": state, "seq": seq, "at_ms": int(time.time() * 1000),
                     "status": {"state": TIER1_STATES[state], "app": app}}
            self.tier1[terminal] = entry
            changed = previous is None or (previous["state"], previous["app"]) != (state, app)
        if changed:
            self.publish_tier1()
        return {"ok": True, "applied": True}

    def publish_tier1(self):
        with self.tier1_lock:
            data = {rid: dict(entry) for rid, entry in self.tier1.items()}
        self.ext.publish("agents.terminal_status", {"data": data})

    def prune_tier1(self):
        with self.tier1_lock:
            gone = [rid for rid in self.tier1 if rid not in self.ext.resources or ended(self.ext.resources[rid])]
            for rid in gone:
                del self.tier1[rid]
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
        rid, items = r["id"], []
        requests = self.requests_v0(r)
        if requests is None:
            requests = []
            items.append({"type": "text", "style": "warn", "text": "Request facts incomplete/unavailable; nothing assumed answered."})
        for q in requests:
            args = {"resource": rid, "request": q["request"]}
            method, choices = q.get("method"), q.get("choices")
            row = {"text": label(q.get("title") or q["request"]), "description": label(str(method)), "command": "agents.answer",
                   "args": args}
            if q.get("incomplete"):  # true or a non-empty list of cut fields
                row = {"type": "text", "style": "warn", "text": label((q.get("title") or q["request"])
                       + ": details incomplete, so it can't be approved here; Cancel stays available.")}
            elif method == "confirm":
                row["options"] = [{"label": "Yes", "value": True}, {"label": "No", "value": False}]
            elif method in ("choice", "permission") and isinstance(choices, list) and choices:
                row["options"] = [{"label": label(str(c.get("label") or c.get("id")), 200), "value": c.get("id")}
                                  for c in choices if isinstance(c, dict) and isinstance(c.get("id"), str)]
            elif method == "text":
                row["input"] = label(q.get("title") or "Answer")
            else:
                row = {"type": "text", "style": "warn", "text": label((q.get("title") or q["request"])+": "+str(method)
                       + " can't be answered from this panel; Cancel stays available.")}
            items.extend([row, {"text": "Cancel "+label(q["request"]), "command": "agents.answer", "args": {**args, "cancelled": True}}])
        if not items:
            items = [{"type": "text", "text": "No pending requests."}]
        return {"title": "Requests: "+rid, "requires": ["menu", "input"], "items": items}

    def answer_v0(self, r, args):
        """agent.respond with the request's epoch and revision: a stale answer is refused by the adapter."""
        request = next((q for q in self.requests_v0(r) or [] if q["request"] == args["request"]), None)
        if request is None:
            raise VqroError("that request is no longer pending; nothing sent", "not_found")
        method, choices = request.get("method"), request.get("choices")
        ids = [c.get("id") for c in choices if isinstance(c, dict)] if isinstance(choices, list) else []
        if args.get("cancelled") is True:
            response = {"action": "cancel"}
        elif request.get("incomplete"):
            raise VqroError("request details are incomplete; only Cancel is possible", "invalid")
        elif method == "confirm" and type(args.get("value")) is bool:
            response = {"value": args["value"]}
        elif method in ("choice", "permission") and args.get("value") in ids:
            response = {"choice": args["value"]}
        elif method == "text" and isinstance(args.get("text"), str):
            response = {"value": args["text"]}
        else:
            raise VqroError("choose a valid answer for this request", "invalid")
        native = {"v": 0, "request": request["request"], "response": response,
                  **({"revision": request["revision"]} if type(request.get("revision")) is int else {}),
                  **({"epoch": agent_facts(r)["epoch"]} if isinstance(agent_facts(r).get("epoch"), str) else {})}
        result = self.ext.vqro.call("resource.invoke", {"resource": r["id"], "op": "agent.respond", "args": native})
        self.outcome("answer", result)
        return {**result, "client": {"close": True}}

    def requests_panel(self, r):
        if self.v0(r, "respond"):
            return self.requests_panel_v0(r)
        items = []
        requests = data(r).get("requests")
        if not isinstance(requests, list):
            requests = []
            items.append({"type": "text", "style": "warn", "text": "Request facts incomplete/unavailable; nothing assumed answered."})
        requests = [q for q in requests if isinstance(q, dict) and isinstance(q.get("id"), str)]
        for request in requests:
            detail = request.get("data") if isinstance(request.get("data"), dict) else {}
            kind = request.get("kind")
            args = {"resource": r["id"], "request": request["id"]}
            row = {"text": label(request.get("prompt") or request["id"]), "description": label(detail.get("message") or detail.get("prefill") or detail.get("placeholder") or kind),
                   "command": "agents.answer", "args": args}
            if kind == "confirm":
                row["options"] = [{"label": "Yes", "value": True}, {"label": "No", "value": False}]
            elif kind == "select" and isinstance(detail.get("options"), list):
                row["options"] = [{"label": label(v, 200), "value": v} for v in detail["options"]]
            elif kind in ("input", "editor"):
                row["input"] = label(request.get("prompt") or "Answer")
            else:
                row = {"type": "text", "text": label("Unsupported request kind: "+str(kind)), "style": "warn"}
            items.extend([row, {"text": "Cancel "+label(request["id"]), "command": "agents.answer", "args": {**args, "cancelled": True}}])
        if not items:
            items = [{"type": "text", "text": "No pending requests."}]
        content = {"title": "Requests: "+r["id"], "requires": ["menu", "input"], "items": items}
        if len(json.dumps(content).encode()) > 262144:
            content["items"] = [{"type": "text", "style": "warn", "text": "Request details exceed the UI budget. Nothing answered; cancellation remains available."}]
            content["items"] += [{"text": "Cancel "+label(q["id"]), "command": "agents.answer",
                                  "args": {"resource": r["id"], "request": q["id"], "cancelled": True}} for q in requests]
        return content

    def answer(self, args):
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
        if sources:
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
        if len(json.dumps(content, ensure_ascii=False).encode()) > 32768:
            content["items"].insert(0, {"type": "text", "style": "warn", "text": "Bounded agent panel: some provider output omitted."})
            while len(json.dumps(content, ensure_ascii=False).encode()) > 32768 and len(content["items"]) > 1:
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
                            if (host_of(self.ext.resources, r) is not None or self.v0(r, "stop")) and not ended(r) else []),
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
