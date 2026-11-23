#!/usr/bin/env python3
"""Optional tmux server controls, independent of the tmux provider extension.

Only the direct helper executes tmux. Idle polling stats its owned endpoint; commands
remain usable when the provider is off. Counts use live view content.
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from vqro_ext import Extension, VqroError
from vqro_display import label
from vqro_process import background_kwargs

POLL_INTERVAL = 5
UNAVAILABLE = "tmux is not available on native Windows; run vqro inside WSL"


def require_supported():
    if sys.platform == "win32":
        raise VqroError(UNAVAILABLE, "unavailable")


class TmuxServer:
    def __init__(self, ext, helper=None):
        self.ext = ext
        self.helper = helper or os.environ.get("VQRO_TMUX_HELPER", "vqro-tmux")
        self.state_dir = ext.info.get("state_dir")
        # Same local owned-server rule as vqro-tmux; no helper process needed to locate it.
        self.socket = Path(self.state_dir) / "tmux.sock" if self.state_dir else None
        self._negative_stamp = None
        self._absent_stamp = None
        self.last = None
        self.menu = None
        ext.command("tmux.server.status", "vqro-owned tmux server status")(self.status)
        ext.command("tmux.server.stop", "Stop vqro-owned tmux server (kills its terminals)")(self.stop)

    def call(self, action, *args):
        require_supported()
        state = self.state_dir
        if not state:
            raise VqroError("tmux needs a persistent vqro state directory", "unavailable")
        argv = [self.helper, "server", action, "--state-dir", state, *args]
        use_groups = os.name == "posix" and callable(getattr(os, "killpg", None)) and hasattr(signal, "SIGKILL")
        group_options = {"start_new_session": True} if use_groups else {}
        child = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 **group_options, **background_kwargs())
        try:
            out, err = child.communicate(timeout=4)
        except subprocess.TimeoutExpired:
            # Only our helper/group, never the independently running tmux daemon. Popen.kill
            # is portable; POSIX may additionally clean up the helper's own CLI descendants.
            try:
                if use_groups:
                    os.killpg(child.pid, signal.SIGKILL)
                else:
                    child.kill()
            except ProcessLookupError:
                pass
            try:
                child.communicate(timeout=1)
            except subprocess.TimeoutExpired:
                child.stdout.close()
                child.stderr.close()
            raise VqroError("tmux helper timed out; never automatically retry a stop",
                            "indeterminate" if action == "stop" else "timeout")
        if child.returncode:
            message = label(err.decode(errors="replace")[:2000])
            raise VqroError(message, "indeterminate" if "indeterminate stop:" in message else "backend")
        try:
            return json.loads(out)
        except (ValueError, UnicodeError):
            raise VqroError("invalid helper reply; stop outcome may be indeterminate, do not retry",
                            "indeterminate" if action == "stop" else "backend")

    def poll(self):
        require_supported()
        absent = {"running": False, "terminals": 0, "socket": str(self.socket) if self.socket else None}
        try:
            metadata = self.socket.stat() if self.socket else None
        except FileNotFoundError:
            metadata = None
        if metadata is None:
            self._negative_stamp = self._absent_stamp = None
            return self.refresh(absent)
        stamp = (metadata.st_dev, metadata.st_ino, metadata.st_mtime_ns, metadata.st_ctime_ns)
        if stamp == self._absent_stamp:
            return self.refresh(absent)
        observed = self.refresh()
        if not observed.get("running"):
            # tmux can leave a stale socket. Confirm the same unchanged endpoint twice before
            # caching absence, rather than cache an initial startup observation immediately.
            self._absent_stamp = stamp if stamp == self._negative_stamp else None
            self._negative_stamp = stamp
        return observed

    VIEW = {"kind": "panel", "title": "vqro-owned tmux server", "items": []}

    def declare(self):
        """Declare the view before asking anything: it never waits on the helper (a slow or failing status call
        under load used to leave the view undeclared for poll after poll). Its content says it is checking."""
        self.ext.contribute({"view/tmux-server": self.VIEW})
        self.menu = False
        self.ext.publish("tmux-server", {"title": "vqro-owned tmux server", "items": [
            {"type": "text", "text": "Checking the vqro-owned tmux server…"}]})

    def unknown(self, message):
        """A status call that failed or timed out: the panel says so (nothing is hidden or retried at once)."""
        observed = {"unknown": message}
        if observed != self.last:
            self.last = observed
            self.ext.publish("tmux-server", {"title": "vqro-owned tmux server", "items": [
                {"type": "text", "text": f"Status unknown: {label(message)}", "style": "warn"}]})

    def refresh(self, observed=None):
        observed = self.call("status") if observed is None else observed
        exists = observed.get("running") is True
        if exists:
            # Explicit commands always query; a positive observation invalidates cached absence.
            self._negative_stamp = self._absent_stamp = None
        if exists != self.menu:
            entries = {"view/tmux-server": self.VIEW}
            if exists:
                entries["menu/tmux-server"] = {"where": "status", "label": "tmux server",
                                               "command": "tmux.server.status", "order": 40}
            # The whole set each time (exclusive): when the server goes away its menu entry is deleted, which a
            # later non-exclusive call (the SDK's default after the first) would leave behind.
            self.ext.contribute(entries, exclusive=True)
            self.menu = exists
        if observed != self.last:
            self.last = dict(observed)
            self.ext.publish("tmux-server", self.panel(observed))
        return observed

    @staticmethod
    def panel(observed):
        if not observed.get("running"):
            text = "No vqro-owned tmux server running"
            if sys.platform == "win32":
                text += " (tmux is not available on native Windows; run vqro inside WSL)"
            return {"title": "vqro-owned tmux server", "requires": ["menu", "input"], "items": [
                {"type": "text", "text": text}]}
        n = observed["terminals"]
        return {"title": "vqro-owned tmux server", "requires": ["menu", "input"], "items": [
            {"type": "text", "text": f"Running · {n} terminals"},
            {"type": "text", "text": "Survives provider/core restarts, safe mode and extension disable."},
            {"text": "Stop server…", "description": f"Disable its provider and kill all {n} current terminals",
             "style": "error", "command": "tmux.server.stop", "input": f"Kill all {n} terminals? Type STOP {n}",
             "args": {"instance": observed["instance"], "terminals": n}}]}

    def absent(self):
        self.ext.notify("No vqro-owned tmux server running")
        return {"running": False, "outcome": "not_submitted"}

    def status(self, args):
        require_supported()
        observed = self.refresh()
        if not observed.get("running"):
            return self.absent()
        return {**observed, "client": {"popup": {"view": "tmux-server", "width": 74}}}

    def stop(self, args):
        require_supported()
        observed = self.refresh()
        if not observed.get("running"):
            return self.absent()
        if "text" not in args:
            # Palette invocation first opens the normal panel's explicit input action.
            return {"client": {"popup": {"view": "tmux-server", "width": 74}}}
        n = observed["terminals"]
        if args.get("instance") != observed["instance"] or args.get("terminals") != n:
            raise VqroError("Server instance or terminal count changed; confirm again. No stop submitted.", "conflict")
        if args["text"].strip() != f"STOP {n}":
            raise VqroError("Confirmation did not match; no stop submitted", "invalid")
        # Persist off before the destructive native action: future core installs/restarts must not
        # recreate the server. Configured servers (--tmux-socket) are never changed or stopped here.
        self.ext.fresh()
        ops = []
        for key, entry in self.ext.entries.items():
            if not key.startswith("extension/"):
                continue
            value = entry.get("value") or {}
            argv = value.get("argv") or []
            if argv and Path(argv[0]).name == "vqro-tmux" \
                    and "--tmux-socket" not in argv and value.get("enabled", True):
                ops.append({"op": "set", "key": key, "expect": entry["version"], "value": dict(value, enabled=False)})
        if ops:
            self.ext.apply(ops, "tmux-server: disable owned provider before explicit stop")
        result = self.call("stop", "--expect-instance", observed["instance"], "--confirm-terminals", str(n))
        self.ext.notify(f"tmux accepted server stop ({n} terminals)", "ok")
        return {**result, "client": {"close": True}}


def main():
    # Stay available for diagnostics on Windows, but commands/poll explicitly refuse before filesystem or helper
    # access. The normal poll error path publishes the unavailable reason; no endpoint absence is assumed.
    ext = Extension("tmux-server", event_types=["entries"])
    server = TmuxServer(ext)
    # Keep the SDK's command/event dispatcher; poll only the native server, never a provider RPC.
    import threading
    threading.Thread(target=ext.run, daemon=True).start()
    server.declare()
    error = None
    while True:
        try:
            server.poll()
            error = None
        except Exception as exc:
            message = str(exc)
            if message != error:
                print("tmux-server: " + label(message), file=sys.stderr)
                error = message
            server.unknown(message)
        time.sleep(POLL_INTERVAL)


if __name__ == "__main__":
    main()
