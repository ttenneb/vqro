#!/usr/bin/env python3
"""Backend-owned Pi agents: launch, tabs, native effects and pending request UI.

All execution is resource.spawn/invoke; this extension never starts Pi itself.
"""
import json
import os
from pathlib import Path
import sys
import threading
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from prim_ext import Extension, PrimError
from pi_bridge.display import plain, tail

SETTINGS = "doc/settings/agents"
DEFAULTS = {"runtimes": [{"name": "pi", "argv": ["pi1", "--mode", "rpc"]}]}
EXTENSIONS = str(Path(__file__).resolve().parent)
EFFECTS = {"prompt": "Prompt", "steer": "Steer", "follow_up": "Follow-up",
           "abort": "Abort", "clear_queue": "Clear queue", "answer": "Answer pending request"}


def label(value, size=512):
    return tail(plain(str(value)), size)


def runtimes(value):
    rows = value.get("runtimes") if isinstance(value, dict) else None
    if not isinstance(rows, list) or not 1 <= len(rows) <= 32:
        raise PrimError("agents runtimes must be a nonempty list (at most 32)", "invalid")
    seen = set()
    for row in rows:
        if not isinstance(row, dict) or not isinstance(row.get("name"), str) or not row["name"].strip() \
                or len(row["name"]) > 64 or row["name"] in seen:
            raise PrimError("runtime names must be unique, nonempty strings (at most 64 characters)", "invalid")
        argv = row.get("argv")
        if not isinstance(argv, list) or not 1 <= len(argv) <= 64 \
                or any(not isinstance(a, str) or not a or "\0" in a or len(a.encode()) > 32768 for a in argv):
            raise PrimError("runtime argv must be a nonempty list of literal argument strings", "invalid")
        seen.add(row["name"])
    return rows


class Agents:
    def __init__(self, ext):
        self.ext, self.lock = ext, threading.RLock()
        self.ids = None
        self.open_requests = set()
        self.list_open = False
        for name, fn, title in [
            ("new", self.new, "New Pi agent"), ("list", self.list_agents, "Show agents"),
            ("show", self.show, "Show an agent in a tab"), ("settings", self.settings, "Agent runtime settings"),
            ("configure", self.configure, "Configure Pi runtime argv")]:
            ext.command("agents."+name, title)(self.guarded(fn))
        for op, title in EFFECTS.items():
            fn = self.answer if op == "answer" else lambda args, op=op: self.effect(op, args)
            ext.command("agents."+op, title)(self.guarded(fn))
        if not ext.version(SETTINGS):
            def initialize():
                if not ext.version(SETTINGS):
                    ext.apply([{"op": "set", "key": SETTINGS, "expect": 0, "value": DEFAULTS}], "agents: default runtimes")
            ext.retrying(initialize)
        ext.contribute({
            "menu/agents.new": {"where": "tabbar", "label": "New Pi agent", "command": "agents.new", "order": 5},
            "menu/agents.list": {"where": "tabbar", "label": "Agents", "command": "agents.list", "order": 6},
            "binding/agents.new": {"key": "a", "command": "agents.new", "label": "new Pi agent"},
            "view/agents-list": {"kind": "panel", "title": "Agents", "items": []},
            "view/agents-runtimes": {"kind": "panel", "title": "Pi runtimes", "items": []},
            "view/agents-settings": {"kind": "panel", "title": "Agent runtimes", "items": []},
        })
        ext.on_event(self.event)
        self.refresh_menus()

    def guarded(self, fn):
        def command(args):
            try:
                return fn(args)
            except PrimError as error:
                self.ext.notify(label(f"Agents: {error} ({error.kind}); no automatic retry."), "warn" if error.kind in ("timeout", "indeterminate") else "error")
                raise
        return command

    def config(self):
        return runtimes(self.ext.value(SETTINGS, DEFAULTS))

    def agent(self, args):
        context = args.get("context") or {}
        rid = args.get("resource") or context.get("resource")
        if not rid and str(context.get("view", "")).startswith("agent-"):
            rid = context["view"][6:]
        resource = self.ext.resources.get(rid)
        if not resource or resource.get("kind") != "agent":
            raise PrimError("choose an agent pane or an agent from Agents", "invalid")
        if not resource.get("available", False) or resource.get("state") != "live":
            raise PrimError("agent is unavailable; nothing sent", "unavailable")
        return resource

    def show(self, args):
        self.ext.fresh()
        r = self.agent(args)
        return self.ext.invoke("tabs.show", {"view": "agent-"+r["id"], "title": "Pi "+r["id"]})

    def new(self, args):
        self.ext.fresh()
        options = self.config()
        selected = args.get("runtime") or args.get("value")
        if selected is None and len(options) > 1:
            self.ext.publish("agents-runtimes", {"title": "New Pi agent: runtime", "requires": ["menu"], "items": [
                {"text": label(r["name"]), "description": label(" ".join(r["argv"])),
                 "command": "agents.new", "args": {"runtime": r["name"], **({"cwd": args["cwd"]} if "cwd" in args else {})}}
                for r in options]})
            return {"client": {"popup": {"view": "agents-runtimes", "width": 72}}}
        runtime = next((r for r in options if r["name"] == selected), None) if selected is not None else options[0]
        if runtime is None:
            raise PrimError("configured runtime not found", "invalid")
        context = args.get("context") or {}
        source = self.ext.resources.get(context.get("resource"), {})
        cwd = args.get("cwd") or source.get("cwd")
        params = {"bridge": True, "name": runtime["name"],
                  "argv": [sys.executable, "-m", "pi_bridge", "--", *runtime["argv"]],
                  "env": {"PYTHONPATH": EXTENSIONS}}
        if cwd:
            params["cwd"] = cwd
        # One native effect. Only layout transactions may be retried afterwards.
        result = self.ext.prim.call("resource.spawn", params)
        host = result["resource"]["id"]
        end = time.monotonic() + 12
        while True:
            self.ext.fresh()
            found = [r for r in self.ext.resources.values() if r.get("kind") == "agent" and r.get("host") == host and r.get("available")]
            if found and self.ext.value("view/agent-"+found[0]["id"]):
                r = found[0]
                try:
                    self.ext.invoke("tabs.show", {"view": "agent-"+r["id"], "title": label(runtime["name"])+" "+r["id"]})
                except PrimError:
                    self.ext.notify("Agent "+r["id"]+" created; tabs unavailable. Open it from Agents.", "warn")
                    return {"outcome": result["outcome"], "host": host, "agent": r["id"],
                            "client": {"popup": {"view": "agent-"+r["id"], "width": 80}}}
                self.ext.notify("Agent "+r["id"]+" created (observed); no prompt sent.", "ok")
                return {"outcome": result["outcome"], "host": host, "agent": r["id"], "client": {"close": True}}
            if time.monotonic() >= end:
                self.ext.notify("Terminal "+host+" spawned; agent not ready. Check Agents later; do not blindly retry.", "warn")
                return {"outcome": result["outcome"], "host": host, "ready": False}
            time.sleep(.1)

    def effect(self, op, args):
        self.ext.fresh()
        r = self.agent(args)
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
        extra = "; not settled: watch agent status" if outcome == "accepted" else "; no runtime acknowledgement" if outcome == "dispatched" else ""
        self.ext.notify(label(EFFECTS[op]+": "+outcome+extra), "ok" if outcome in ("accepted", "observed") else "warn")

    def requests_panel(self, r):
        items = []
        requests = r.get("data", {}).get("requests")
        if not isinstance(requests, list):
            items.append({"type": "text", "style": "warn", "text": "Request facts incomplete/unavailable; nothing assumed answered."})
        for request in requests or []:
            args = {"resource": r["id"], "request": request["id"]}
            row = {"text": label(request.get("prompt") or request["id"]), "description": label(request.get("data", {}).get("message", request["kind"])),
                   "command": "agents.answer", "args": args}
            if request["kind"] == "confirm":
                row["options"] = [{"label": "Yes", "value": True}, {"label": "No", "value": False}]
            elif request["kind"] == "select":
                row["options"] = [{"label": label(v, 200), "value": v} for v in request.get("data", {}).get("options", [])]
            elif request["kind"] in ("input", "editor"):
                row["input"] = label(request.get("prompt") or "Answer")
            else:
                row = {"type": "text", "text": label("Unsupported request kind: "+str(request["kind"])), "style": "warn"}
            items.extend([row, {"text": "Cancel "+label(request["id"]), "command": "agents.answer", "args": {**args, "cancelled": True}}])
        if not items:
            items = [{"type": "text", "text": "No pending requests."}]
        content = {"title": "Requests: "+r["id"], "requires": ["menu", "input"], "items": items}
        if len(json.dumps(content).encode()) > 262144:
            content["items"] = [{"type": "text", "style": "warn", "text": "Requests exceed UI display budget. Nothing answered; inspect the agent facts."}]
        return content

    def answer(self, args):
        self.ext.fresh()
        r = self.agent(args)
        rid = r["id"]
        if "request" not in args:
            view = "agents-requests-"+rid
            self.ext.contribute({"view/"+view: {"kind": "panel", "title": "Pending requests", "items": []}})
            self.open_requests.add(rid)
            self.ext.publish(view, self.requests_panel(r))
            return {"client": {"popup": {"view": view, "width": 80}}}
        request = next((q for q in r.get("data", {}).get("requests") or [] if q["id"] == args["request"]), None)
        if request is None:
            raise PrimError("that request is no longer pending; nothing sent", "not_found")
        native = {"id": request["id"]}
        if args.get("cancelled") is True:
            native["cancelled"] = True
        elif request["kind"] == "confirm" and type(args.get("value")) is bool:
            native["confirmed"] = args["value"]
        elif request["kind"] == "select" and args.get("value") in request.get("data", {}).get("options", []):
            native["value"] = args["value"]
        elif request["kind"] in ("input", "editor") and isinstance(args.get("text"), str):
            native["value"] = args["text"]
        else:
            raise PrimError("choose a valid answer for this request", "invalid")
        result = self.ext.prim.call("resource.invoke", {"resource": rid, "op": "agent.answer", "args": native})
        self.outcome("answer", result)
        return {**result, "client": {"close": True}}

    def list_agents(self, args):
        self.ext.fresh()
        self.list_open = True
        self.publish_list()
        return {"client": {"popup": {"view": "agents-list", "width": 80}}}

    def publish_list(self):
        rows = [{"text": label(r.get("name") or r["id"])+" "+r["id"], "description": label((r.get("status") or {}).get("state", "unknown")),
                 "command": "agents.show", "args": {"resource": r["id"]}, "disabled": not r.get("available")}
                for r in self.ext.resources.values() if r.get("kind") == "agent"]
        rows = rows[:128]
        rows.append({"text": "Runtime settings", "command": "agents.settings"})
        self.ext.publish("agents-list", {"title": "Agents", "items": rows})

    def settings(self, args):
        value = self.ext.value(SETTINGS, DEFAULTS)
        self.ext.publish("agents-settings", {"title": "Pi runtime argv", "requires": ["input"], "items": [
            {"type": "text", "text": "Each runtime is an explicit Pi RPC command. No shell interpolation."},
            {"type": "text", "text": label(json.dumps(value, ensure_ascii=False), 8192)},
            {"text": "Edit runtimes JSON list", "input": "JSON list of {name, argv}", "command": "agents.configure"}]})
        return {"client": {"popup": {"view": "agents-settings", "width": 90}}}

    def configure(self, args):
        try:
            value = {"runtimes": json.loads(args.get("text", ""))}
        except (ValueError, TypeError):
            raise PrimError("enter a JSON list of {name, argv}", "invalid") from None
        runtimes(value)
        def save():
            self.ext.apply([{"op": "set", "key": SETTINGS, "value": value, "expect": self.ext.version(SETTINGS)}], "agents: runtimes")
        self.ext.retrying(save)
        self.ext.notify("Agent runtimes saved; existing agents unchanged.", "ok")
        return {"client": {"close": True}}

    def refresh_menus(self):
        with self.lock:
            ids = tuple(sorted(r["id"] for r in self.ext.resources.values() if r.get("kind") == "agent"))
            if ids == self.ids:
                return
            entries = {}
            for rid in ids:
                for i, (op, title) in enumerate(EFFECTS.items()):
                    menu = {"where": "pane", "label": title, "command": "agents."+op, "args": {"resource": rid},
                            "when": {"view": "agent-"+rid}, "order": 100+i}
                    if op in ("prompt", "steer", "follow_up"):
                        menu["input"] = title
                    entries["menu/agents."+rid+"."+op] = menu
            stale = [key for key, entry in self.ext.entries.items() if key.startswith("menu/agents.r")
                     and entry["value"].get("owner") == "agents" and key not in entries]
            if stale:
                self.ext.apply([{"op": "delete", "key": key} for key in stale], "agents: remove stale pane actions")
            self.ext.contribute(entries)
            self.ids = ids

    def event(self, kind, data):
        if kind in ("resource", "resource.removed", "missed"):
            self.refresh_menus()
            if self.list_open:
                self.publish_list()
            for rid in tuple(self.open_requests):
                r = self.ext.resources.get(rid)
                if r:
                    self.ext.publish("agents-requests-"+rid, self.requests_panel(r))
                else:
                    self.ext.publish("agents-requests-"+rid, {"items": [{"type": "text", "text": "Agent is gone; nothing sent."}]})
                    self.open_requests.discard(rid)


def main():
    ext = Extension("agents", event_types=["entries", "resource", "resource.removed"])
    Agents(ext)
    ext.run()


if __name__ == "__main__":
    main()
