#!/usr/bin/env python3
"""Pi launch/runtime settings policy. Execution stays in the backend-owned bridge."""
import json
from pathlib import Path
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from prim_ext import Extension, PrimError
from prim_display import label

SETTINGS = "doc/settings/pi"
LEGACY = "doc/settings/agents"
DEFAULTS = {"runtimes": [{"name": "pi", "argv": ["pi1", "--mode", "rpc"]}]}
EXTENSIONS = str(Path(__file__).resolve().parent)


def runtimes(value):
    rows = value.get("runtimes") if isinstance(value, dict) else None
    if not isinstance(rows, list) or not 1 <= len(rows) <= 32:
        raise PrimError("Pi runtimes must be a nonempty list (at most 32)", "invalid")
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


class Pi:
    def __init__(self, ext):
        self.ext = ext
        for name, fn, title in [("new", self.new, "New Pi agent"), ("settings", self.settings, "Pi runtime settings"),
                                ("configure", self.configure, "Configure Pi runtime argv")]:
            ext.command("pi."+name, title)(self.guarded(fn))
        def initialize():
            if not ext.version(SETTINGS):
                old = ext.value(LEGACY)
                value = {"runtimes": old["runtimes"]} if isinstance(old, dict) and "runtimes" in old else DEFAULTS
                ext.apply([{"op": "set", "key": SETTINGS, "expect": 0, "value": value}], "pi: migrate/default runtimes")
        ext.retrying(initialize)
        ext.contribute({
            "menu/pi.new": {"where": "tabbar", "label": "New Pi agent", "command": "pi.new", "order": 5},
            "binding/pi.new": {"key": "a", "command": "pi.new", "label": "new Pi agent"},
            "view/pi-runtimes": {"kind": "panel", "title": "Pi runtimes", "items": []},
            "view/pi-settings": {"kind": "panel", "title": "Pi runtime settings", "items": []},
        })

    def guarded(self, fn):
        def command(args):
            try:
                return fn(args)
            except PrimError as error:
                self.ext.notify(label(f"Pi: {error} ({error.kind}); no automatic retry."), "warn" if error.kind in ("timeout", "indeterminate") else "error")
                raise
        return command

    def new(self, args):
        self.ext.fresh()
        options = runtimes(self.ext.value(SETTINGS, DEFAULTS))
        selected = args.get("runtime") or args.get("value")
        context = args.get("context") or {}
        source_id = context.get("resource")
        for prefix in ("agents-", "agent-"):
            if not source_id and str(context.get("view", "")).startswith(prefix):
                source_id = context["view"][len(prefix):]
        cwd = args.get("cwd") or self.ext.resources.get(source_id, {}).get("cwd")
        if selected is None and len(options) > 1:
            self.ext.publish("pi-runtimes", {"title": "New Pi agent: runtime", "requires": ["menu"], "items": [
                {"text": label(r["name"]), "description": label(" ".join(r["argv"])), "command": "pi.new",
                 "args": {"runtime": r["name"], **({"cwd": cwd} if cwd else {})}} for r in options]})
            return {"client": {"popup": {"view": "pi-runtimes", "width": 72}}}
        runtime = next((r for r in options if r["name"] == selected), None) if selected is not None else options[0]
        if runtime is None:
            raise PrimError("configured runtime not found", "invalid")
        params = {"bridge": True, "argv": [sys.executable, "-m", "pi_bridge", "--", *runtime["argv"]],
                  "env": {"PYTHONPATH": EXTENSIONS}}
        if cwd:
            params["cwd"] = cwd
        result = self.ext.prim.call("resource.spawn", params)  # exactly once, never retried
        host = result["resource"]["id"]
        end = time.monotonic() + 12
        while True:
            self.ext.fresh()
            found = [r for r in self.ext.resources.values() if r.get("kind") == "agent" and r.get("host") == host and r.get("available")]
            if found:
                resource = found[0]
                # Agents is optional: its disable must never kill or hide the
                # backend terminal. Fall back to that public terminal view.
                try:
                    self.ext.invoke("agents.show", {"resource": resource["id"]})
                except PrimError:
                    self.terminal(host)
                self.ext.notify("Pi agent "+resource["id"]+" created (observed); no prompt sent.", "ok")
                return {"outcome": result["outcome"], "host": host, "agent": resource["id"], "client": {"close": True}}
            if time.monotonic() >= end:
                self.ext.notify("Terminal "+host+" spawned; Pi agent not ready. Do not blindly retry.", "warn")
                self.terminal(host)
                return {"outcome": result["outcome"], "host": host, "ready": False}
            time.sleep(.1)

    def terminal(self, host):
        try:
            return self.ext.invoke("tabs.show", {"resource": host})
        except PrimError:
            self.ext.notify("Terminal "+host+" still exists; tabs unavailable. Use the resource picker.", "warn")

    def settings(self, args):
        self.ext.fresh()
        value = self.ext.value(SETTINGS, DEFAULTS)
        self.ext.publish("pi-settings", {"title": "Pi runtime argv", "requires": ["input"], "items": [
            {"type": "text", "text": "Explicit Pi RPC commands. No shell interpolation."},
            {"type": "text", "text": label(json.dumps(value.get("runtimes", value) if isinstance(value, dict) else value, ensure_ascii=False), 8192)},
            {"text": "Edit Pi runtimes JSON list", "input": "JSON list of {name, argv}", "command": "pi.configure"}]})
        return {"client": {"popup": {"view": "pi-settings", "width": 90}}}

    def configure(self, args):
        try:
            value = self.ext.value(SETTINGS, {})
            value = dict(value) if isinstance(value, dict) else {}
            value["runtimes"] = json.loads(args.get("text", ""))
        except (ValueError, TypeError):
            raise PrimError("enter a JSON list of {name, argv}", "invalid") from None
        runtimes(value)
        def save():
            self.ext.apply([{"op": "set", "key": SETTINGS, "value": value, "expect": self.ext.version(SETTINGS)}], "pi: runtimes")
        self.ext.retrying(save)
        self.ext.notify("Pi runtimes saved; existing agents unchanged.", "ok")
        return {"client": {"close": True}}


def main():
    ext = Extension("pi", event_types=["entries", "resource"])
    Pi(ext)
    ext.run()


if __name__ == "__main__":
    main()
