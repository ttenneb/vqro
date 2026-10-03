#!/usr/bin/env python3
"""One-command check that a Pi Vqro starts gets the Vqro-only profile.

Reuses the readiness harness (private root, real server, plain `vqro`, PTY,
birth-checked cleanup). Model-free: it inspects Pi's own package listing and the
pane environment, and watches for any `herdr` process under the Pi pane.

No installation, credentials, model calls or human-session changes.
"""
import argparse
import json
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

from demo_readiness import ReadinessDemo

EXCLUDED = ("pi-input-gate", "pi-tasking-herdr-adapter", "rpiv-todo")
WANTED = (
    "searxng-suite",
    "pi-tree-sitter",
    "avtc-pi-ask-user-question",
    "pi-system-prompt",
    "pi-budget",
    "pi-anthropic-auth",
)


def package_name(source):
    source = source if isinstance(source, str) else source["source"]
    for prefix in ("npm:", "git:", "http:", "https:"):
        if source.startswith(prefix):
            source = source[len(prefix):]
            break
    tail = source.rstrip("/\\").rsplit("/", 1)[-1]
    return tail.rsplit("@", 1)[0] if "@" in tail else tail


def herdr_descendants(pane_pid):
    """Currently running `herdr*` processes under `pane_pid`."""
    procs = {}
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            stat = (entry / "stat").read_text()
            ppid = int(stat[stat.rfind(")") + 2:].split()[1])
            cmd = (entry / "cmdline").read_bytes().replace(b"\0", b" ").decode(errors="ignore").strip()
        except OSError:
            continue
        procs[int(entry.name)] = (ppid, cmd)
    found = []
    for pid, (_, cmd) in procs.items():
        if not cmd.split(" ")[0].endswith("herdr"):
            continue
        current = pid
        for _ in range(24):
            parent = procs.get(current, (0, ""))[0]
            if parent == pane_pid:
                found.append((pid, cmd))
                break
            if parent <= 1:
                break
            current = parent
    return found


class PiProfileDemo(ReadinessDemo):
    def __init__(self, root, vqro, pi):
        super().__init__(root, vqro, pi)
        # The readiness harness wraps `pi` to record argv; this demo needs the real
        # Pi so its own arguments and package loading are exercised.
        link = root / "bin" / "pi"
        if link.is_symlink():
            link.unlink()
        link.symlink_to(pi)

    def seed_human_agent_dir(self):
        """A private stand-in for the human's ~/.pi/agent (read-only source).

        The settings come from the human's; the default provider and a dummy auth
        entry are only so `pi list` can resolve a model offline without real
        credentials.
        """
        agent = self.root / "home" / ".pi" / "agent"
        agent.mkdir(parents=True, exist_ok=True)
        human = Path.home() / ".pi" / "agent"
        settings = json.loads((human / "settings.json").read_text())
        settings["defaultProvider"] = "deepseek"
        settings["defaultModel"] = "deepseek-chat"
        # A trailing-separator directory source must still be excluded by name.
        settings["packages"].append("/tmp/pi-input-gate/")
        (agent / "settings.json").write_text(json.dumps(settings, indent=2))
        (agent / "auth.json").write_text('{"deepseek":{"type":"key","key":"demo"}}')
        for name in ("npm", "git"):
            source = human / name
            if source.exists() and not (agent / name).exists():
                (agent / name).symlink_to(source)

    def profile_dir(self):
        return self.bundled_dir().parent / "pi-agent"

    def wait_file(self, path, seconds=20):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if path.exists():
                return
            time.sleep(0.05)
        raise RuntimeError(f"expected {path}")

    def run(self):
        self.seed_human_agent_dir()
        self.start()
        profile = self.profile_dir()

        settings = json.loads((profile / "settings.json").read_text())
        names = [package_name(entry) for entry in settings["packages"]]
        assert not [name for name in names if name in EXCLUDED], names
        assert sorted(names) == sorted(WANTED), names

        env = {**self.env, "PI_CODING_AGENT_DIR": str(profile), "PI_OFFLINE": "1",
               "PI_SKIP_VERSION_CHECK": "1", "PI_TELEMETRY": "0"}
        listing = subprocess.run([str(self.pi), "list"], env=env, capture_output=True, text=True, timeout=60)
        assert listing.returncode == 0, listing
        for excluded in EXCLUDED:
            assert excluded not in listing.stdout, (excluded, listing.stdout)
        for wanted in WANTED:
            assert wanted in listing.stdout, (wanted, listing.stdout)

        # A hand-typed `pi` in a Vqro pane sees the profile through the pane env.
        human = self.cli("workspace", "create", "--cwd", str(self.root / "work"), "--no-focus")["result"]["root_pane"]["pane_id"]
        result = self.root / "pane-agent-dir"
        self.cli("pane", "run", human, f"printf '%s' \"$PI_CODING_AGENT_DIR\" > {result}")
        self.wait_file(result)
        assert result.read_text() == str(profile), result.read_text()

        # A Pi started in a Vqro pane loads no herdr-coupled package, so no herdr
        # process runs under the pane while it starts.
        agent = self.cli("pane", "split", "--pane", human, "--direction", "right", "--no-focus")["result"]["pane"]["pane_id"]
        self.cli("pane", "wait-output", agent, "--regex", r"\$", "--timeout", "5000")
        self.cli("agent", "start", "pi-check", "--kind", "pi", "--pane", agent, "--timeout", "10000", "--", "--no-session")
        pane_pid = self.cli("pane", "process-info", "--pane", agent)["result"]["process_info"]["shell_pid"]
        observed = []
        deadline = time.monotonic() + 6
        while time.monotonic() < deadline:
            observed.extend(herdr_descendants(pane_pid))
            if observed:
                break
            time.sleep(0.1)
        assert not observed, observed
        self.remember_pane(agent)

        result_value = {"result": "passed", "excluded_absent": list(EXCLUDED), "wanted_present": list(WANTED),
                        "pi_list_ok": True, "hand_typed_pane_env": True, "herdr_processes": len(observed)}
        self.save("RESULT.json", result_value)
        return result_value


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vqro", required=True, type=Path)
    parser.add_argument("--pi", default=shutil.which("pi"), type=Path)
    parser.add_argument("--root", type=Path)
    args = parser.parse_args()
    if not args.pi:
        parser.error("an already-installed Pi executable is required")
    if args.root:
        args.root.mkdir(mode=0o700, parents=True, exist_ok=False)
        root = args.root.resolve()
    else:
        base = Path.home() / ".local/share/herdr/scratch"
        base.mkdir(parents=True, exist_ok=True)
        root = Path(tempfile.mkdtemp(prefix="pi-profile-demo-", dir=base))
    if len(str(root / "api.sock").encode()) >= 108:
        parser.error("private root is too long for a Linux Unix socket")
    demo = PiProfileDemo(root, args.vqro.resolve(), args.pi.resolve())
    print(f"Private Pi profile demo: {root}", flush=True)
    try:
        print(json.dumps(demo.run()), flush=True)
    finally:
        demo.cleanup()


if __name__ == "__main__":
    main()
