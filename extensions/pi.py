#!/usr/bin/env python3
"""Pi launch/runtime settings policy. Execution stays in the backend-owned bridge."""
import json
from pathlib import Path
import sys
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
from vqro_ext import Extension, VqroError
from vqro_display import label

SETTINGS = "doc/settings/pi"
LEGACY = "doc/settings/agents"
DEFAULTS = {"runtimes": [{"name": "pi", "argv": ["pi1", "--mode", "rpc"]}]}
EXTENSIONS = str(Path(__file__).resolve().parent)


def default_bridge():
    # Resolved by the terminal host, never selected from the core host's OS.
    # Windows/other hosts configure their launcher explicitly (e.g. py -3).
    return ["python3", "-I", str(Path(EXTENSIONS) / "pi_bridge" / "launch.py")]


def valid_argv(argv):
    return isinstance(argv, list) and 1 <= len(argv) <= 64 and all(
        isinstance(arg, str) and arg and "\0" not in arg and len(arg.encode()) <= 32768 for arg in argv)


def runtimes(value):
    rows = value.get("runtimes") if isinstance(value, dict) else None
    if not isinstance(rows, list) or not 1 <= len(rows) <= 32:
        raise VqroError("Pi runtimes must be a nonempty list (at most 32)", "invalid")
    seen = set()
    for row in rows:
        if not isinstance(row, dict) or not isinstance(row.get("name"), str) or not row["name"].strip() \
                or len(row["name"]) > 64 or row["name"] in seen:
            raise VqroError("runtime names must be unique, nonempty strings (at most 64 characters)", "invalid")
        argv = row.get("argv")
        if not valid_argv(argv):
            raise VqroError("runtime argv must be a nonempty list of literal argument strings", "invalid")
        if "bridge_argv" in row and not valid_argv(row["bridge_argv"]):
            raise VqroError("bridge_argv must be literal argv for the terminal host", "invalid")
        env = row.get("bridge_env", {})
        if not isinstance(env, dict) or any(not isinstance(k, str) or not k or "=" in k or "\0" in k
                or not isinstance(v, str) or "\0" in v for k, v in env.items()):
            raise VqroError("bridge_env must map host environment names to strings", "invalid")
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
            # A compact "+<agent icon>" after the new-tab "+"; the name is in the command list (prefix a, :).
            "menu/pi.new": {"where": "tabbar", "label": "+", "icon_after": True,
                            "icon": {"name": "agent", "nerd": "\U000f06a9", "text": "◆"}, "command": "pi.new", "order": 5},
            "binding/pi.new": {"key": "a", "command": "pi.new", "label": "new Pi agent"},
            "view/pi-runtimes": {"kind": "panel", "title": "Pi runtimes", "items": []},
            "view/pi-settings": {"kind": "panel", "title": "Pi runtime settings", "items": []},
        })

    def guarded(self, fn):
        def command(args):
            try:
                return fn(args)
            except VqroError as error:
                self.ext.notify(label(f"Pi: {error} ({error.kind}); no automatic retry."), "warn" if error.kind in ("timeout", "indeterminate") else "error")
                raise
        return command

    def new(self, args):
        entry=self.ext.vqro.call("workspace.get",{"key":SETTINGS})
        options = runtimes(entry["value"] if entry else DEFAULTS)
        selected = args.get("runtime") or args.get("value")
        context = args.get("context") or {}
        source_id = context.get("resource")
        for prefix in ("agents-", "agent-"):
            if not source_id and str(context.get("view", "")).startswith(prefix):
                source_id = context["view"][len(prefix):]
        cwd = args.get("cwd")
        if not cwd and source_id:
            scoped=self.ext.vqro.call("resource.list",{"id":source_id})
            source=next((r for r in scoped.get("resources",[]) if r.get("id")==source_id),{})
            cwd=source.get("cwd")
        if not cwd:
            cwd = self.ext.layout_folder(args)  # the layout's starting folder, if it names one
        if selected is None and len(options) > 1:
            self.ext.publish("pi-runtimes", {"title": "New Pi agent: runtime", "requires": ["menu"], "items": [
                {"text": label(r["name"]), "description": label(" ".join(r["argv"])), "command": "pi.new",
                 "args": {"runtime": r["name"], **({"cwd": cwd} if cwd else {})}} for r in options]})
            return {"client": {"popup": {"view": "pi-runtimes", "width": 72}}}
        runtime = next((r for r in options if r["name"] == selected), None) if selected is not None else options[0]
        if runtime is None:
            raise VqroError("configured runtime not found", "invalid")
        launcher = runtime.get("bridge_argv", default_bridge())
        # The local isolated launcher locates its installed package itself. Neither it nor
        # a configured host launcher injects a module path into the agent environment.
        env = dict(runtime.get("bridge_env", {}))
        # A new agent owns its own logical work, not its launching TUI's undo scope.
        env.setdefault("VQRO_PRINCIPAL", "p:"+uuid.uuid4().hex)
        params = {"bridge": True, "name": "pi-bridge-"+uuid.uuid4().hex[:12], "argv": [*launcher, "--", *runtime["argv"]], "env": env}
        if cwd:
            params["cwd"] = cwd
        result = self.ext.vqro.call("resource.spawn", params)  # exactly once, never retried
        host = result["resource"]["id"]
        end = time.monotonic() + 12
        lookup_problem=None
        while True:
            try:
                scoped=self.ext.vqro.call("resource.list",{"host":host})
                rows=scoped.get("resources",[])
                lookup_problem=None
            except VqroError as error:
                lookup_problem=label(f"{error} ({error.kind})",256)
                rows=[]
                if error.kind not in ("timeout","overloaded","unavailable","disconnected"):
                    break
            found = [r for r in rows if r.get("kind") == "agent" and r.get("host") == host and r.get("available")]
            if found:
                resource = found[0]
                self.initialize_request_route(resource['id'])
                # Agents is optional: its disable must never kill or hide the
                # backend terminal. Fall back to that public terminal view.
                try:
                    shown = self.ext.invoke("agents.show", {"resource": resource["id"]})
                except VqroError as error:
                    # Showing a view is not part of the already-observed spawn. Never turn
                    # a UI/read failure into an apparent failed launch that invites a retry.
                    if error.kind not in ("unavailable", "not_found"):
                        self.ext.notify("Pi agent "+resource["id"]+" exists; view unavailable ("+error.kind+").", "warn")
                    shown = self.terminal(host)
                self.ext.notify("Pi agent "+resource["id"]+" created (observed); no prompt sent.", "ok")
                return {"outcome": result["outcome"], "host": host, "agent": resource["id"], "client": dict((shown or {}).get("client") or {}, close=True)}
            if time.monotonic() >= end:
                break
            time.sleep(.1)
        detail=" Status unavailable: "+lookup_problem+"." if lookup_problem else ""
        self.ext.notify("Terminal "+host+" spawned; Pi agent not ready."+detail+" Do not blindly retry.", "warn")
        shown = self.terminal(host)
        return {"outcome": result["outcome"], "host": host, "ready": False, "client": (shown or {}).get("client", {})}

    def initialize_request_route(self, agent):
        # The coord callback sees the original SDK caller/proof, never a launch
        # principal claimed in args. Configuration failure cannot replay spawn.
        try:
            window = self.ext.invoke('coord.inbox.window', {})
            if not isinstance(window, dict) or not isinstance(window.get('epoch'), str):
                raise VqroError('coordination window unavailable', 'unavailable')
            initialized = self.ext.invoke('coord.request_route', {'op': window['epoch'] + ':pi-route:' + uuid.uuid4().hex,
                            'action': 'initialize', 'agent': agent, 'expect': 0})
            if not isinstance(initialized, dict) or initialized.get('state') != 'accepted':
                raise VqroError('request route initialization not confirmed', 'unavailable')
            route = self.ext.invoke('coord.request_route', {'action': 'inspect', 'agent': agent})
            if not isinstance(route, dict):
                raise VqroError('request route inspection unavailable', 'unavailable')
            if route.get('fallback_reason'):
                self.ext.notify('Pi agent ' + agent + ': human request fallback (' + route['fallback_reason'] + ').', 'warn')
        except VqroError as error:
            self.ext.notify('Pi agent ' + agent + ' exists; request route initialization unconfirmed (' + error.kind
                            + '); use human Control and do not replay the launch.', 'warn')

    def terminal(self, host):
        try:
            return self.ext.invoke("tabs.show", {"resource": host})
        except VqroError:
            self.ext.notify("Terminal "+host+" still exists; tabs unavailable. Use the resource picker.", "warn")

    def settings(self, args):
        self.ext.fresh()
        value = self.ext.value(SETTINGS, DEFAULTS)
        self.ext.publish("pi-settings", {"title": "Pi runtime argv", "requires": ["input"], "items": [
            {"type": "text", "text": "Literal Pi argv; optional bridge_argv and bridge_env resolve on the terminal host."},
            {"type": "text", "text": label(json.dumps(value.get("runtimes", value) if isinstance(value, dict) else value, ensure_ascii=False), 8192)},
            {"text": "Edit Pi runtimes JSON list", "input": "JSON list of {name, argv}", "command": "pi.configure"}]})
        return {"client": {"popup": {"view": "pi-settings", "width": 90}}}

    def configure(self, args):
        try:
            value = self.ext.value(SETTINGS, {})
            value = dict(value) if isinstance(value, dict) else {}
            value["runtimes"] = json.loads(args.get("text", ""))
        except (ValueError, TypeError):
            raise VqroError("enter a JSON list of {name, argv}", "invalid") from None
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
