#!/usr/bin/env python3
"""vqro extension: tier1. New Claude Code and Pi agents in a plain terminal, each with vqro's pull plugin wired for
that launch only (coordination-v0 section 7, tier 1). The human's own ~/.claude and ~/.pi are never read or written.

- Claude Code: `claude --settings <file>`; the file holds only vqro's hooks and lives in this extension's state.
- Pi: `pi -e <extensions dir>/tier1/vqro-pull.ts`.

A launch spawns `tier1_run.py` in a new backend terminal named `<runtime>-<nonce>`, with a fresh principal
(`VQRO_PRINCIPAL=p:<uuid>`) and the launch nonce. Then, in the owner's command, it records the endpoint's pull declaration (the plugin's own) with that
nonce in `doc/agents/terminals/<rN>`, grants the principal `receive` on `agent:<rN>`, and resumes its inbox gate (the
owner's click is the intent; a refusal leaves it paused and says why). The wrapper finds its own terminal by that name, waits
(bounded) for the declaration carrying its nonce and the grant, and execs the runtime with `VQRO_AGENT=rN`. Request routing is not recorded (tier-1 plugins forward no native requests;
the human answers them in the pane): `human`, in effect.

Nothing else depends on this extension; with it off, launched terminals keep running as plain terminals.
"""
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
from vqro_ext import Extension, VqroError
from vqro_display import label

HERE = Path(__file__).resolve().parent
RUN = HERE / "tier1_run.py"
# Installed: <extensions dir>/tier1/vqro-pull.ts (scripts/dev-install.sh). From a checkout: integrations/pi/.
PI_PLUGIN = next((path for path in (HERE / "tier1" / "vqro-pull.ts", HERE.parent / "integrations" / "pi" / "vqro-pull.ts")
                  if path.is_file()), HERE / "tier1" / "vqro-pull.ts")
SETTINGS = "doc/settings/tier1"
DEFAULTS = {"claude": ["claude"], "pi": ["pi"]}
TITLES = {"claude": "Claude", "pi": "Pi"}


def state_dir():
    """Private per-launch files beside the core's state (`VQRO_STATE_DIR`, else XDG/`~/.local/state/vqro`)."""
    base = os.environ.get("VQRO_STATE_DIR") or os.path.join(
        os.environ.get("XDG_STATE_HOME") or os.path.join(os.path.expanduser("~"), ".local", "state"), "vqro")
    path = Path(base) / "tier1"
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(path, 0o700)
    return path


def valid_argv(argv):
    return isinstance(argv, list) and 1 <= len(argv) <= 64 and all(
        isinstance(arg, str) and arg and "\0" not in arg and len(arg.encode()) <= 32768 for arg in argv)


def find_hook(configured=None):
    """vqro-claude-hook: as configured, else beside the running core (its own binary), else on PATH."""
    if configured:
        return configured
    try:
        core = Path(os.readlink(f"/proc/{os.getppid()}/exe")).parent / "vqro-claude-hook"
        if core.is_file() and os.access(core, os.X_OK):
            return str(core)
    except OSError:
        pass
    return shutil.which("vqro-claude-hook")


def pi_declaration():
    """The Pi plugin's own declaration (`PULL_DECLARATION`, one JSON line in the plugin source)."""
    match = re.search(r"^export const PULL_DECLARATION = (.*);$", PI_PLUGIN.read_text(), re.M)
    if not match:
        raise VqroError("the installed Pi plugin declares no PULL_DECLARATION", "unavailable")
    return json.loads(match.group(1))["pull"]


class Tier1:
    def __init__(self, ext):
        self.ext = ext
        for runtime, title in (("claude", "New Claude agent"), ("pi", "New Pi agent (tier 1)")):
            ext.command("tier1." + runtime, title + " (args: cwd)")(lambda args, runtime=runtime: self.launch(runtime, args))
        ext.on_event(self.changed)
        self.sweep()

    # -- launch -------------------------------------------------------------------------------------------------

    def launch(self, runtime, args):
        settings = self.ext.value(SETTINGS, DEFAULTS)
        argv = settings.get(runtime) if isinstance(settings, dict) else None
        argv = argv if valid_argv(argv) else DEFAULTS[runtime]
        nonce, principal = uuid.uuid4().hex, "p:" + uuid.uuid4().hex
        if runtime == "claude":
            hook = find_hook(settings.get("hook") if isinstance(settings, dict) else None)
            if not hook:
                raise VqroError("vqro-claude-hook not found beside vqro or on PATH; install it, or set "
                                + SETTINGS + ".hook", "unavailable")
            declaration = json.loads(self.run([hook, "declaration"]))["pull"]
            hooks = json.loads(self.run([hook, "settings", hook]))
            path = state_dir() / f"claude-{nonce}.json"
            fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, "w") as file:
                json.dump(hooks, file)
            argv = [*argv, "--settings", str(path)]
        else:
            if not PI_PLUGIN.is_file():
                raise VqroError("the Pi plugin is not installed at " + str(PI_PLUGIN), "unavailable")
            declaration = pi_declaration()
            argv = [*argv, "-e", str(PI_PLUGIN)]
        params = {"argv": ["python3", str(RUN), "--", *argv], "name": runtime + "-" + nonce,
                  "env": {"VQRO_PRINCIPAL": principal, "VQRO_TIER1_LAUNCH": nonce}}
        cwd = args.get("cwd") or self.ext.layout_folder(args)
        if cwd:
            params["cwd"] = cwd
        result = self.ext.vqro.call("resource.spawn", params)  # exactly once, never retried
        rid = result["resource"]["id"]
        problems = []
        # The endpoint's declaration (with the nonce the wrapper waits for), then its plugin's receive grant.
        try:
            self.ext.apply([{"op": "set", "key": "doc/agents/terminals/" + rid, "expect": 0,
                             "value": {"pull": declaration, "launch": nonce, "app": runtime}}], "tier1: " + runtime + " endpoint")
        except VqroError as error:
            problems.append(f"declaration not recorded ({error.kind}); the plugin stays inert")
        try:
            self.coord("grants", {"id": "tier1-" + rid, "expect": 0,
                                  "value": {"who": principal, "on": "agent:" + rid, "can": ["receive"]}})
            preview = self.ext.invoke("coord.scope", {"action": "inspect", "scope": "agent:" + rid})
            self.coord("scope", {"action": "resume", "scope": "agent:" + rid, "expect": preview["expect"]})
        except VqroError as error:
            problems.append(f"inbox left paused ({label(str(error), 160)}; {error.kind})")
        shown = None
        try:
            shown = self.ext.invoke("tabs.show", {"resource": rid})
        except VqroError:
            problems.append("tabs unavailable; open it from the sidebar")
        name = TITLES[runtime]
        if problems:
            self.ext.notify(f"{name} agent {rid} started; " + "; ".join(problems) + ".", "warn")
        else:
            self.ext.notify(f"{name} agent {rid} started with its inbox open; no prompt sent.", "ok")
        return {"outcome": result["outcome"], "resource": rid, "principal": principal,
                "client": (shown or {}).get("client", {})}

    def coord(self, command, args):
        epoch = self.ext.invoke("coord.inbox.window", {})["epoch"]
        return self.ext.invoke("coord." + command, {"op": f"{epoch}:tier1:{uuid.uuid4().hex}", **args})

    @staticmethod
    def run(argv):
        try:
            return subprocess.run(argv, capture_output=True, text=True, timeout=10, check=True).stdout
        except (OSError, subprocess.SubprocessError) as error:
            raise VqroError(f"{argv[0]} failed: {error}", "unavailable") from None

    # -- per-launch files ---------------------------------------------------------------------------------------

    def changed(self, kind, data):
        if kind in ("resource", "resource.removed", "missed"):
            self.sweep()

    def sweep(self):
        """Remove a Claude settings file once no live terminal's declaration carries its nonce."""
        try:
            files = list(state_dir().glob("claude-*.json"))
        except OSError:
            return
        if not files:
            return
        live = set()
        for key, entry in list(self.ext.entries.items()) if hasattr(self.ext, "entries") else []:
            if key.startswith("doc/agents/terminals/"):
                rid = key[len("doc/agents/terminals/"):]
                resource = self.ext.resources.get(rid)
                value = entry.get("value") if isinstance(entry, dict) else None
                if resource and resource.get("state") not in ("finished", "removed") and isinstance(value, dict):
                    live.add(value.get("launch"))
        for path in files:
            nonce = path.stem[len("claude-"):]
            # Files younger than a minute may belong to a launch whose declaration is not recorded yet.
            if nonce not in live and path.stat().st_mtime < time.time() - 60:
                path.unlink(missing_ok=True)


def main():
    ext = Extension("tier1", event_types=["entries", "resource", "resource.removed"])
    Tier1(ext)
    ext.run()


if __name__ == "__main__":
    main()
