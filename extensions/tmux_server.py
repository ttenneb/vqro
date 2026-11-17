#!/usr/bin/env python3
"""Optional tmux server controls, independent of the tmux provider extension.

Only the direct helper knows tmux. Commands remain usable when the provider is off;
menu presence changes only with server existence, counts use live view content.
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from prim_ext import Extension, PrimError
from prim_display import label


class TmuxServer:
    def __init__(self, ext, helper=None):
        self.ext = ext
        self.helper = helper or os.environ.get("PRIM_TMUX_HELPER", "prim-tmux")
        self.last = None
        self.menu = None
        ext.command("tmux.server.status", "Prim-owned tmux server status")(self.status)
        ext.command("tmux.server.stop", "Stop prim-owned tmux server (kills its terminals)")(self.stop)

    def call(self, action, *args):
        state = self.ext.info.get("state_dir")
        if not state:
            raise PrimError("tmux needs a persistent prim state directory", "unavailable")
        argv = [self.helper, "server", action, "--state-dir", state, *args]
        child = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 start_new_session=True)
        try:
            out, err = child.communicate(timeout=4)
        except subprocess.TimeoutExpired:
            # Only our helper group, not the independently running tmux daemon.
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            try:
                child.communicate(timeout=1)
            except subprocess.TimeoutExpired:
                child.stdout.close()
                child.stderr.close()
            raise PrimError("tmux helper timed out; never automatically retry a stop",
                            "indeterminate" if action == "stop" else "timeout")
        if child.returncode:
            message = label(err.decode(errors="replace")[:2000])
            raise PrimError(message, "indeterminate" if "indeterminate stop:" in message else "backend")
        try:
            return json.loads(out)
        except (ValueError, UnicodeError):
            raise PrimError("invalid helper reply; stop outcome may be indeterminate, do not retry",
                            "indeterminate" if action == "stop" else "backend")

    def refresh(self, observed=None):
        observed = self.call("status") if observed is None else observed
        exists = observed.get("running") is True
        if exists != self.menu:
            entries = {"view/tmux-server": {"kind": "panel", "title": "Prim-owned tmux server", "items": []}}
            if exists:
                entries["menu/tmux-server"] = {"where": "status", "label": "tmux server",
                                               "command": "tmux.server.status", "order": 40}
            self.ext.contribute(entries)
            self.menu = exists
        if observed != self.last:
            self.last = dict(observed)
            self.ext.publish("tmux-server", self.panel(observed))
        return observed

    @staticmethod
    def panel(observed):
        if not observed.get("running"):
            return {"title": "Prim-owned tmux server", "requires": ["menu", "input"], "items": [
                {"type": "text", "text": "No prim-owned tmux server running"}]}
        n = observed["terminals"]
        return {"title": "Prim-owned tmux server", "requires": ["menu", "input"], "items": [
            {"type": "text", "text": f"Running · {n} terminals"},
            {"type": "text", "text": "Survives provider/core restarts, safe mode and extension disable."},
            {"text": "Stop server…", "description": f"Disable its provider and kill all {n} current terminals",
             "style": "error", "command": "tmux.server.stop", "input": f"Kill all {n} terminals? Type STOP {n}",
             "args": {"instance": observed["instance"], "terminals": n}}]}

    def absent(self):
        self.ext.notify("No prim-owned tmux server running")
        return {"running": False, "outcome": "not_submitted"}

    def status(self, args):
        observed = self.refresh()
        if not observed.get("running"):
            return self.absent()
        return {**observed, "client": {"popup": {"view": "tmux-server", "width": 74}}}

    def stop(self, args):
        observed = self.refresh()
        if not observed.get("running"):
            return self.absent()
        if "text" not in args:
            # Palette invocation first opens the normal panel's explicit input action.
            return {"client": {"popup": {"view": "tmux-server", "width": 74}}}
        n = observed["terminals"]
        if args.get("instance") != observed["instance"] or args.get("terminals") != n:
            raise PrimError("Server instance or terminal count changed; confirm again. No stop submitted.", "conflict")
        if args["text"].strip() != f"STOP {n}":
            raise PrimError("Confirmation did not match; no stop submitted", "invalid")
        # Persist off before the destructive native action: future core installs/restarts must not
        # recreate the server. Configured servers (--tmux-socket) are never changed or stopped here.
        self.ext.fresh()
        ops = []
        for key, entry in self.ext.entries.items():
            if not key.startswith("extension/"):
                continue
            value = entry.get("value") or {}
            argv = value.get("argv") or []
            if argv and Path(argv[0]).name == "prim-tmux" \
                    and "--tmux-socket" not in argv and value.get("enabled", True):
                ops.append({"op": "set", "key": key, "expect": entry["version"], "value": dict(value, enabled=False)})
        if ops:
            self.ext.apply(ops, "tmux-server: disable owned provider before explicit stop")
        result = self.call("stop", "--expect-instance", observed["instance"], "--confirm-terminals", str(n))
        self.ext.notify(f"tmux accepted server stop ({n} terminals)", "ok")
        return {**result, "client": {"close": True}}


def main():
    ext = Extension("tmux-server", event_types=["entries"])
    server = TmuxServer(ext)
    # Keep the SDK's command/event dispatcher; poll only the native server, never a provider RPC.
    import threading
    threading.Thread(target=ext.run, daemon=True).start()
    error = None
    while True:
        try:
            server.refresh()
            error = None
        except Exception as exc:
            message = str(exc)
            if message != error:
                print("tmux-server: " + label(message), file=sys.stderr)
                error = message
        time.sleep(1)


if __name__ == "__main__":
    main()
