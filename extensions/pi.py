#!/usr/bin/env python3
"""Pi launch/runtime settings policy. Execution stays in the backend-owned bridge."""
import json
from pathlib import Path
import sys
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
from vqro_ext import Extension, VqroError
from pi_testing import testing_notice
from vqro_display import label
from account_profiles import choose as choose_account, environment as account_environment, provider_argv, runtime_environment, require_fresh
from agent_launch import initialize_request_route, launch, launch_cwd, runtimes as validate_runtimes, valid_argv

SETTINGS = "doc/settings/pi"
LEGACY = "doc/settings/agents"
DEFAULTS = {"runtimes": [{"name": "pi", "argv": ["pi1", "--mode", "rpc"]}]}
EXTENSIONS = str(Path(__file__).resolve().parent)


def default_bridge():
    # Resolved by the terminal host, never selected from the core host's OS.
    # Windows/other hosts configure their launcher explicitly (e.g. py -3).
    return ["python3", "-I", str(Path(EXTENSIONS) / "pi_bridge" / "launch.py")]


def runtimes(value):
    return validate_runtimes(value, brand="Pi")


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
            "view/accounts-choice-pi-new": {"kind":"panel","title":"Pi account","items":[]},
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
        cwd = launch_cwd(self.ext, args)
        if selected is None and len(options) > 1:
            self.ext.publish("pi-runtimes", {"title": "New Pi agent: runtime", "requires": ["menu"], "items": [
                {"text": label(r["name"]), "description": label(" ".join(r["argv"])), "command": "pi.new",
                 "args": {"runtime": r["name"], **({"cwd": cwd} if cwd else {}),
                          **{k:args[k] for k in ('account','_account_revision','fresh') if k in args}}} for r in options]})
            return {"client": {"popup": {"view": "pi-runtimes", "width": 72}}}
        runtime = next((r for r in options if r["name"] == selected), None) if selected is not None else options[0]
        if runtime is None:
            raise VqroError("configured runtime not found", "invalid")
        if args.get('fresh') is True: require_fresh('pi', runtime['argv'])
        account, popup = choose_account(self.ext, dict(args, runtime=runtime['name'], **({'cwd':cwd} if cwd else {})), 'pi', 'pi.new')
        if popup: return popup
        # RPC bridges intentionally reject extension injection and filter runtime env.
        # Do not weaken those guards to claim setup-testing coverage.
        testing_notice(self.ext, "testing plugin unsupported for RPC Pi; launching without testing hooks (NOT VERIFIED)")
        native_argv = provider_argv(runtime['argv'], account)
        launcher = runtime.get("bridge_argv", default_bridge())
        # The local isolated launcher locates its installed package itself. Neither it nor
        # a configured host launcher injects a module path into the agent environment.
        env = account_environment(account, runtime_environment('pi', runtime))
        # A new agent owns its own logical work, not its launching TUI's undo scope.
        env.setdefault("VQRO_PRINCIPAL", "p:"+uuid.uuid4().hex)
        params = {"bridge": True, "name": "pi-bridge-"+uuid.uuid4().hex[:12], "argv": [*launcher, "--", *native_argv], "env": env}
        if cwd:
            params["cwd"] = cwd
        return launch(self.ext, params, brand="Pi", initialize_route=self.initialize_request_route,
                      show_terminal=self.terminal)

    def initialize_request_route(self, agent):
        # Preserve Pi's return shape while sharing the actual-caller default mechanism.
        initialize_request_route(self.ext, agent, brand="Pi")

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
