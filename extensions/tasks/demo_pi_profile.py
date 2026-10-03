#!/usr/bin/env python3
"""One-command check that a Pi Vqro starts gets the Vqro-only profile.

Reuses the readiness harness (private root, real server, plain `vqro`, PTY,
birth-checked cleanup). Model-free: it inspects the profile layout, Pi's own
package listing, the pane environment, and Pi's startup resource list (the
Skills / Extensions / Context header), and watches for any `herdr` process under
the Pi pane.

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
# Unambiguous excluded resource names; the bare `herdr` skill is covered by the
# profile-layout check and the sanitized header check below.
HERDR_SKILLS = (
    "agent-messaging",
    "herdr",
    "herdr-pi-test-root",
    "herdr-pi-upgrade-rollback",
    "launch-owner",
    "check-session-loaded",
    "scratch-cleanup",
)
EXCLUDED_EXTENSION = "herdr-agent-state.ts"
KEPT_EXTENSION = "keep-extension.ts"
KEPT_SKILL = "keep-skill"
WANTED = (
    "searxng-suite",
    "pi-tree-sitter",
    "avtc-pi-ask-user-question",
    "pi-system-prompt",
    "pi-budget",
    "pi-anthropic-auth",
)


def resource_name(entry):
    entry = entry.lstrip("!+-")
    parts = [part for part in entry.split("/") if part]
    return parts[-1] if parts else entry


def skill_name(entry):
    entry = entry.lstrip("!+-")
    parts = [part for part in entry.split("/") if part]
    if len(parts) >= 2 and parts[-1] == "SKILL.md":
        return parts[-2]
    return parts[-1] if parts else entry


def header_sections(text):
    """Map each `[Section]` header in Pi's startup header to its whitespace-stripped
    body, so assertions bind to the real section rather than any occurrence."""
    sections = {}
    current = None
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            current = stripped[1:-1].lower()
            sections[current] = []
        elif current is not None:
            sections[current].append(stripped)
    return {name: "".join(body) for name, body in sections.items()}


def package_name(source):
    source = source if isinstance(source, str) else source["source"]
    for prefix in ("npm:", "git:", "http:", "https:"):
        if source.startswith(prefix):
            source = source[len(prefix):]
            break
    parts = [part for part in source.split("/") if part]
    tail = parts[-1] if parts else source
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
    def __init__(self, root, vqro, pi, no_settings=False):
        super().__init__(root, vqro, pi)
        self.no_settings = no_settings
        # The readiness harness wraps `pi` to record argv; this demo needs the real
        # Pi so its own arguments and resource loading are exercised.
        link = root / "bin" / "pi"
        if link.is_symlink():
            link.unlink()
        link.symlink_to(pi)

    def seed_human_agent_dir(self):
        """A private stand-in for the human's ~/.pi/agent (read-only source).

        The settings come from the human's; the default provider and a dummy auth
        entry are only so `pi list` can resolve a model offline without real
        credentials. The extensions, skills and SYSTEM.md are stubs that make the
        excluded and kept sets visible.
        """
        agent = self.root / "home" / ".pi" / "agent"
        agent.mkdir(parents=True, exist_ok=True)
        extensions = agent / "extensions"
        extensions.mkdir(exist_ok=True)
        (extensions / EXCLUDED_EXTENSION).write_text("export default function () {}\n")
        (extensions / KEPT_EXTENSION).write_text("export default function () {}\n")
        skills = agent / "skills"
        skills.mkdir(exist_ok=True)
        for name in (*HERDR_SKILLS, KEPT_SKILL):
            directory = skills / name
            directory.mkdir(exist_ok=True)
            (directory / "SKILL.md").write_text("---\ndescription: stub\n---\nStub.\n")
        (agent / "SYSTEM.md").write_text("HUMAN GLOBAL SYSTEM PROMPT\n")
        if self.no_settings:
            # Resources and a SYSTEM.md but no settings.json: the profile must
            # still be built from empty settings.
            return
        human = Path.home() / ".pi" / "agent"
        settings = json.loads((human / "settings.json").read_text())
        settings["defaultProvider"] = "deepseek"
        settings["defaultModel"] = "deepseek-chat"
        # A trailing/duplicated-separator directory source must still be excluded.
        settings["packages"].append("/tmp//pi-input-gate//")
        # An explicit absolute extension path in settings must not bypass the
        # extension exclusion, nor an explicit absolute skill file the skill one.
        settings["extensions"] = [
            str(extensions / EXCLUDED_EXTENSION),
            str(extensions / KEPT_EXTENSION),
        ]
        settings["skills"] = [
            str(skills / "herdr" / "SKILL.md"),
            str(skills / KEPT_SKILL / "SKILL.md"),
        ]
        (agent / "settings.json").write_text(json.dumps(settings, indent=2))
        (agent / "auth.json").write_text('{"deepseek":{"type":"key","key":"demo"}}')
        for name in ("npm", "git"):
            source = human / name
            if source.exists() and not (agent / name).exists():
                (agent / name).symlink_to(source)
        # The human's relative local package sources resolve to ~/Projects from the
        # agent dir; mirror that base so they load in the private root.
        home_projects = self.root / "home" / "Projects"
        if not home_projects.exists():
            home_projects.symlink_to(Path.home() / "Projects")

    def profile_dir(self):
        return self.bundled_dir().parent / "pi-agent"

    def wait_file(self, path, seconds=20):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if path.exists():
                return
            time.sleep(0.05)
        raise RuntimeError(f"expected {path}")

    def wait_output(self, pane, regex, seconds=15):
        self.cli("pane", "wait-output", pane, "--regex", regex, "--timeout", str(int(seconds * 1000)))

    def run(self):
        self.seed_human_agent_dir()
        self.start()
        profile = self.profile_dir()
        if self.no_settings:
            return self.check_no_settings(profile)

        settings = json.loads((profile / "settings.json").read_text())
        names = [package_name(entry) for entry in settings["packages"]]
        assert not [name for name in names if name in EXCLUDED], names
        assert sorted(names) == sorted(WANTED), names
        explicit = [resource_name(entry) for entry in settings.get("extensions", [])]
        assert EXCLUDED_EXTENSION not in explicit, explicit
        explicit_skills = [skill_name(entry) for entry in settings.get("skills", [])]
        assert not [name for name in explicit_skills if name in HERDR_SKILLS], explicit_skills
        assert KEPT_SKILL in explicit_skills, explicit_skills

        # Profile layout: excluded resources gone, kept ones and the Vqro system
        # prompt present.
        profile_extensions = profile / "extensions"
        profile_skills = profile / "skills"
        assert not (profile_extensions / EXCLUDED_EXTENSION).exists()
        assert (profile_extensions / KEPT_EXTENSION).exists()
        for name in HERDR_SKILLS:
            assert not (profile_skills / name).exists(), name
        assert (profile_skills / KEPT_SKILL).exists()
        system = (profile / "SYSTEM.md").read_text()
        assert "Working with Vqro" in system, system[:80]
        assert "HUMAN GLOBAL SYSTEM PROMPT" not in system

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

        # A Pi started in a Vqro pane: its startup resource list shows none of the
        # excluded items, the vqro skill and reply-routing adapter, and the Vqro
        # SYSTEM.md as context; and no herdr process runs under the pane.
        agent = self.cli("pane", "split", "--pane", human, "--direction", "right", "--no-focus")["result"]["pane"]["pane_id"]
        self.wait_output(agent, r"\$", 5)
        self.cli("agent", "start", "pi-check", "--kind", "pi", "--pane", agent, "--timeout", "10000", "--", "--no-session")
        self.wait_output(agent, r"Skills", 20)
        header = self.cli("pane", "read", agent, "--source", "visible", "--format", "text", "--lines", "80")
        sanitized = header.replace(str(self.root), "")
        sections = header_sections(sanitized)
        context = sections.get("context", "")
        skills = sections.get("skills", "")
        extensions_section = sections.get("extensions", "")
        assert "/pi-agent/SYSTEM.md" in context, (context, sanitized)
        assert "vqro" in skills, (skills, sanitized)
        assert "vqro-reply-routing.ts" in extensions_section, (extensions_section, sanitized)
        for name in (*EXCLUDED, *HERDR_SKILLS, EXCLUDED_EXTENSION):
            assert name not in sanitized, (name, sanitized)

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
                        "excluded_extension_absent": EXCLUDED_EXTENSION,
                        "excluded_skills_absent": list(HERDR_SKILLS), "kept_resources_present": True,
                        "vqro_system_md": True, "pi_list_ok": True, "header_ok": True,
                        "hand_typed_pane_env": True, "herdr_processes": len(observed)}
        self.save("RESULT.json", result_value)
        (self.root / "header.txt").write_text(sanitized)
        return result_value

    def check_no_settings(self, profile):
        """A human agent dir without settings.json still gets a profile."""
        settings = json.loads((profile / "settings.json").read_text())
        assert settings.get("packages", []) == [], settings
        assert not (profile / "extensions" / EXCLUDED_EXTENSION).exists()
        assert (profile / "extensions" / KEPT_EXTENSION).exists()
        for name in HERDR_SKILLS:
            assert not (profile / "skills" / name).exists(), name
        assert (profile / "skills" / KEPT_SKILL).exists()
        system = (profile / "SYSTEM.md").read_text()
        assert "Working with Vqro" in system, system[:80]
        assert "HUMAN GLOBAL SYSTEM PROMPT" not in system
        human = self.cli("workspace", "create", "--cwd", str(self.root / "work"), "--no-focus")["result"]["root_pane"]["pane_id"]
        result = self.root / "pane-agent-dir"
        self.cli("pane", "run", human, f"printf '%s' \"$PI_CODING_AGENT_DIR\" > {result}")
        self.wait_file(result)
        assert result.read_text() == str(profile), result.read_text()
        value = {"result": "passed", "no_settings": True, "empty_settings": True,
                 "excluded_resources_absent": True, "vqro_system_md": True, "pane_env": True}
        self.save("RESULT-nosettings.json", value)
        return value


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
        if len(str(root / "api.sock").encode()) >= 108:
            parser.error("private root is too long for a Linux Unix socket")
        demo = PiProfileDemo(root, args.vqro.resolve(), args.pi.resolve())
        print(f"Private Pi profile demo: {root}", flush=True)
        try:
            print(json.dumps(demo.run()), flush=True)
        finally:
            demo.cleanup()
        return
    base = Path.home() / ".local/share/herdr/scratch"
    base.mkdir(parents=True, exist_ok=True)
    for no_settings in (False, True):
        root = Path(tempfile.mkdtemp(prefix="pi-profile-demo-", dir=base))
        if len(str(root / "api.sock").encode()) >= 108:
            parser.error("private root is too long for a Linux Unix socket")
        label = "no-settings" if no_settings else "default"
        demo = PiProfileDemo(root, args.vqro.resolve(), args.pi.resolve(), no_settings=no_settings)
        print(f"Private Pi profile demo ({label}): {root}", flush=True)
        try:
            print(json.dumps(demo.run()), flush=True)
        finally:
            demo.cleanup()


if __name__ == "__main__":
    main()
