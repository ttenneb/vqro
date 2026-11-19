#!/usr/bin/env python3
"""Runtime-neutral agent panels and native capability-driven controls. No launcher."""
import json
from pathlib import Path
import sys
import threading

sys.path.insert(0, str(Path(__file__).resolve().parent))
from vqro_ext import Extension, VqroError, views_in
from vqro_display import label
from vqro_views import ended

EFFECTS = {"prompt": "Prompt", "steer": "Steer", "follow_up": "Follow-up", "abort": "Abort",
           "clear_queue": "Clear queue", "answer": "Answer pending request"}


def data(resource):
    value = resource.get("data")
    return value if isinstance(value, dict) else {}


def status(resource):
    value = resource.get("status")
    return label(value.get("state", "unknown")) if isinstance(value, dict) else "unknown"


def view(rid):
    return "agents-"+rid


def advertised(resource, field):
    """Agent standard v0 section 6.3: the resource's `data.agent.commands` / `data.agent.config` (or [])."""
    agent = data(resource).get("agent")
    rows = agent.get(field) if isinstance(agent, dict) else None
    key = "name" if field == "commands" else "id"
    return [row for row in rows if isinstance(row, dict) and isinstance(row.get(key), str)] if isinstance(rows, list) else []


def panel_key(key):
    return key.startswith("view/agents-r") and key[len("view/agents-r"):].isdigit()


class Agents:
    def __init__(self, ext):
        self.ext, self.lock = ext, threading.RLock()
        self.open_requests = set()
        self.open_settings = set()
        self.published = {}  # Only current agents; suppress unchanged derived panel RPCs.
        self.list_open = False
        for name, fn, title in [("list", self.list_agents, "Show agents"), ("show", self.show, "Show an agent in a tab"),
                                ("config", self.settings, "Agent settings and commands (popup)"),
                                ("configure", self.configure, "Change an agent setting (args: id, value)"),
                                ("command", self.command, "Run an agent command (args: name, text?)")]:
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
                                                  "label": "prompt agent", "when": {"resource_kind": "agent"}}}
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

    def supported(self, resource, op):
        provider = self.ext.providers.get(resource.get("provider"), {})
        return isinstance(provider.get("capabilities", {}).get("agent."+op), dict)

    def current_signature(self):
        return tuple(sorted((r["id"], tuple(op for op in EFFECTS if self.supported(r, op)), self.has_settings(r))
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

    def effect(self, op, args):
        self.ext.fresh()
        r = self.agent(args, op)
        native = {}
        if op in ("prompt", "steer", "follow_up"):
            text = args.get("text")
            if not isinstance(text, str) or not text.strip():
                raise VqroError("message must not be empty", "invalid")
            native["message"] = text
        result = self.ext.vqro.call("resource.invoke", {"resource": r["id"], "op": "agent."+op, "args": native})
        self.outcome(op, result)
        return result

    def outcome(self, op, result):
        outcome = result.get("outcome", "unknown")
        extra = "; acceptance is not settlement: watch agent status" if outcome == "accepted" else "; no runtime acknowledgement" if outcome == "dispatched" else ""
        self.ext.notify(label(EFFECTS[op]+": "+outcome+extra), "ok" if outcome in ("accepted", "observed") else "warn")

    def requests_panel(self, r):
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
            content["items"].insert(0, {"type": "text", "style": "warn", "text": "Agent unavailable; displayed facts may be stale."})
        if len(json.dumps(content, ensure_ascii=False).encode()) > 32768:
            content["items"].insert(0, {"type": "text", "style": "warn", "text": "Bounded agent panel: some provider output omitted."})
            while len(json.dumps(content, ensure_ascii=False).encode()) > 32768 and len(content["items"]) > 1:
                del content["items"][1]
        return content

    def list_agents(self, args):
        self.ext.fresh()
        self.list_open = True
        self.publish_list()
        return {"client": {"popup": {"view": "agents-list", "width": 80}}}

    def publish_list(self):
        rows = [{"text": label(r.get("name") or "Agent")+" "+r["id"], "description": status(r),
                 "command": "agents.show", "args": {"resource": r["id"]}, "badges": [{"text": "ended", "style": "dim"}] if ended(r) else [],
                 **({"menu": [{"label": "Show", "command": "agents.show", "args": {"resource": r["id"]}},
                              {"label": "Agent settings…", "command": "agents.config", "args": {"resource": r["id"]}}]}
                    if self.has_settings(r) and r.get("available") else {})}
                for r in self.ext.resources.values() if r.get("kind") == "agent"]
        count = len(rows)
        rows = rows[:128]
        if count > 128:
            rows.append({"type": "text", "style": "warn", "text": f"Showing 128 of {count} agents; existing agent panes remain usable."})
        self.ext.publish("agents-list", {"title": "Agents", "items": rows or [{"type": "text", "text": "No agents observed."}]})

    def declarations(self, resources=None):
        entries = {}
        for r in list(self.ext.resources.values()) if resources is None else resources:
            if r.get("kind") != "agent":
                continue
            rid = r["id"]
            # `resource` names the agent this pane shows: context.resource / resource_kind for keys and menus.
            entries["view/"+view(rid)] = {"kind": "panel", "title": "Agent "+rid, "resource": rid, "items": [{"type": "text", "text": "Agents extension is not running."}]}
            for i, (op, title) in enumerate(EFFECTS.items()):
                if not self.supported(r, op):
                    continue
                menu = {"where": "pane", "label": title, "command": "agents."+op, "when": {"view": view(rid)}, "order": 100+i}
                if op in ("prompt", "steer", "follow_up"):
                    menu["input"] = title
                entries["menu/agents."+rid+"."+op] = menu
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
                        rid = next((key[len(prefix):] for prefix in ("view/agents-requests-", "view/agents-settings-", "view/agents-")
                                    if key.startswith(prefix)), "")
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
