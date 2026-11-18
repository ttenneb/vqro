#!/usr/bin/env python3
"""Runtime-neutral agent panels and native capability-driven controls. No launcher."""
import json
from pathlib import Path
import sys
import threading

sys.path.insert(0, str(Path(__file__).resolve().parent))
from prim_ext import Extension, PrimError
from prim_display import label
from prim_views import ended

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


def panel_key(key):
    return key.startswith("view/agents-r") and key[len("view/agents-r"):].isdigit()


class Agents:
    def __init__(self, ext):
        self.ext, self.lock = ext, threading.RLock()
        self.open_requests = set()
        self.published = {}  # Only current agents; suppress unchanged derived panel RPCs.
        self.list_open = False
        for name, fn, title in [("list", self.list_agents, "Show agents"), ("show", self.show, "Show an agent in a tab")]:
            ext.command("agents."+name, title)(self.guarded(fn))
        for op, title in EFFECTS.items():
            fn = self.answer if op == "answer" else lambda args, op=op: self.effect(op, args)
            ext.command("agents."+op, title)(self.guarded(fn))
        self.signature = self.current_signature()
        declarations = {"menu/agents.list": {"where": "tabbar", "label": "Agents", "command": "agents.list", "order": 6},
                        "view/agents-list": {"kind": "panel", "title": "Agents", "items": []}}
        declarations.update(self.declarations())
        # Provider inventory may not be restored yet. Referenced panels must
        # remain valid across extension disable/re-enable and core restart.
        for key, entry in ext.entries.items():
            if panel_key(key) and entry["value"].get("owner") == "agents":
                declarations.setdefault(key, entry["value"])
        ext.contribute(declarations)  # reconciles removed/moved launcher entries
        ext.on_event(self.event)
        self.publish_panels()

    def supported(self, resource, op):
        provider = self.ext.providers.get(resource.get("provider"), {})
        return isinstance(provider.get("capabilities", {}).get("agent."+op), dict)

    def current_signature(self):
        return tuple(sorted((r["id"], tuple(op for op in EFFECTS if self.supported(r, op)))
                            for r in self.ext.resources.values() if r.get("kind") == "agent"))

    def guarded(self, fn):
        def command(args):
            try:
                return fn(args)
            except PrimError as error:
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
            raise PrimError("choose an agent pane or an agent from Agents", "invalid")
        if not inspect and (not resource.get("available", False) or resource.get("state") != "live"):
            raise PrimError("agent is unavailable; nothing sent", "unavailable")
        if op and not self.supported(resource, op):
            raise PrimError("agent provider does not declare agent."+op+"; nothing sent", "unsupported")
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
                raise PrimError("message must not be empty", "invalid")
            native["message"] = text
        result = self.ext.prim.call("resource.invoke", {"resource": r["id"], "op": "agent."+op, "args": native})
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
            raise PrimError("that request is no longer pending; nothing sent", "not_found")
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
            raise PrimError("choose a valid answer for this request", "invalid")
        result = self.ext.prim.call("resource.invoke", {"resource": rid, "op": "agent.answer", "args": native})
        self.outcome("answer", result)
        return {**result, "client": {"close": True}}

    def panel(self, r):
        title = label(r.get("name") or r.get("title") or "Agent")+" "+r["id"]
        native = self.ext.live.get("agent-"+r["id"])
        if isinstance(native, dict) and isinstance(native.get("items"), list):
            # Provider-native output is optional, read-only data. Controls stay
            # here and are offered only from declared native capabilities.
            def readonly(row):
                if not isinstance(row, dict):
                    return {"type": "text", "text": label(row)}
                kept = {k: v for k, v in row.items() if k in ("type", "text", "style", "description", "detail", "detail_style", "badges", "id", "expanded")}
                for key in ("text", "description", "detail"):
                    if key in kept:
                        kept[key] = label(kept[key], 8192)
                if isinstance(kept.get("badges"), list):
                    kept["badges"] = [{"text": label(b.get("text", ""), 200), "style": b.get("style", "plain")}
                                      for b in kept["badges"] if isinstance(b, dict)]
                if isinstance(row.get("children"), list):
                    kept["children"] = [readonly(child) for child in row["children"]]
                return kept
            content = {"title": title, "requires": native.get("requires", []), "items": [readonly(row) for row in native["items"]]}
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
                 "command": "agents.show", "args": {"resource": r["id"]}, "badges": [{"text": "ended", "style": "dim"}] if ended(r) else []}
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
            entries["view/"+view(rid)] = {"kind": "panel", "title": "Agent "+rid, "items": [{"type": "text", "text": "Agents extension is not running."}]}
            for i, (op, title) in enumerate(EFFECTS.items()):
                if not self.supported(r, op):
                    continue
                menu = {"where": "pane", "label": title, "command": "agents."+op, "when": {"view": view(rid)}, "order": 100+i}
                if op in ("prompt", "steer", "follow_up"):
                    menu["input"] = title
                entries["menu/agents."+rid+"."+op] = menu
        return entries

    def refresh(self):
        with self.lock:
            signature = self.current_signature()
            if signature == self.signature:
                return
            entries = self.declarations()
            def prune():
                stale = [key for key, entry in self.ext.entries.items() if key.startswith("menu/agents.r")
                         and entry["value"].get("owner") == "agents" and key not in entries]
                if stale:
                    self.ext.apply([{"op": "delete", "key": key, "expect": self.ext.version(key)} for key in stale], "agents: stale actions/panels")
            self.ext.retrying(prune)
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
        if kind in ("resource", "resource.removed", "provider", "missed"):
            self.refresh()
            changed = event.get("id") if kind in ("resource", "resource.removed") else None
            selected = [r for r in self.ext.resources.values() if r.get("kind") == "agent" and (r["id"] == changed or r.get("host") == changed)] if changed else None
            self.publish_panels(selected)
            if kind == "resource.removed" and changed:
                self.publish_gone(changed)
            if self.list_open and (not changed or self.ext.resources.get(changed, {}).get("kind") == "agent" or kind == "resource.removed"):
                self.publish_list()
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
