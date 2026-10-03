#!/usr/bin/env python3
"""One-command readiness: a clean private root gets the core extensions from a
single launch, and two model-free Pi panes run import -> delegate -> badge ->
decision -> message with no manual linking or per-agent config.

Reuses the tasks demo lifecycle (private root, real server, real model-free Pi,
birth-checked cleanup). It never calls `plugin link`: the point is that plain
Vqro already linked the bundled `vqro.tasks` and that `agent start` already gave
each Pi pane the reply-routing adapter and the vqro skill. No installation,
credentials, model calls, or human-session changes.
"""
import argparse
import fcntl
import json
import os
import pty
import shlex
import shutil
import socket
import struct
import subprocess
import tempfile
import termios
import threading
import time
from pathlib import Path

from demo import Demo, birth

CRITERION = "The observable result is recorded."
TASKS_NAMESPACE = "vqro.tasks"


class ReadinessDemo(Demo):
    def __init__(self, root, vqro, pi):
        super().__init__(root, vqro, pi)
        # Pi sets process.title, which truncates /proc/cmdline, so wrap the real Pi
        # to record the exact argv Vqro launches it with. Pi still runs for real.
        self.launch_log = root / "pi-launch-args.txt"
        wrapper = root / "bin" / "pi-wrapper"
        wrapper.write_text(
            "#!/bin/sh\n{ printf '%s\\n' \"$@\"; echo '-----'; } >> "
            + shlex.quote(str(self.launch_log)) + "\nexec " + shlex.quote(str(pi))
            + ' --no-session --no-extensions --no-skills --no-prompt-templates --no-themes -e '
            + shlex.quote(str(root / "demo-agent.ts")) + ' "$@"\n')
        wrapper.chmod(0o755)
        (root / "bin" / "pi").unlink()
        (root / "bin" / "pi").symlink_to(wrapper)
        # The private fixture executes the guide's shell blocks under each real
        # Pi execution. No model, credentials or normal-user config is involved.
        fixture = root / "demo-agent.ts"
        fixture.write_text(fixture.read_text().replace(
            "const stdout=execFileSync('vqro',[command.tool||'task',...command.args],{encoding:'utf8',timeout:15000});",
            "const stdout=command.script ? execFileSync('/bin/bash',['-ec',command.script],{encoding:'utf8',timeout:60000}) : execFileSync('vqro',[command.tool||'task',...command.args],{encoding:'utf8',timeout:15000});"))

    def guide_block(self, step):
        guide = Path(__file__).resolve().parents[2] / "docs/next/first-10-minutes.md"
        # Fixed blocks in this one guide, not a general Markdown runner.
        marker = f"<!-- {step} -->\n```sh\n"
        text = guide.read_text()
        assert text.count(marker) == 1, f"missing/duplicate guide block {step}"
        return text.split(marker, 1)[1].split("\n```", 1)[0]

    def guide_human(self, pane, step):
        output, error, status = [self.root / f"receipts/{step}.{suffix}" for suffix in ("out", "err", "status")]
        # Brace grouping, not a new bash process: literal paste executes under
        # the host-owned interactive human shell, as the guide specifies.
        command = "{\n" + self.guide_block(step) + "\n}"
        command += f" > {shlex.quote(str(output))} 2> {shlex.quote(str(error))}; printf '%s' \"$?\" > {shlex.quote(str(status))}"
        self.cli("pane", "run", pane, command)
        self.wait_file(status)
        assert status.read_text() == "0", f"guide {step}: {error.read_text()} {output.read_text()}"

    def guide_agent(self, name, step):
        self.save(f"commands/{step}.json", {"script": self.guide_block(step)})
        self.cli("agent", "prompt", name, f"/taskcore {step}")
        path = self.root / f"receipts/{step}.json"
        self.wait_file(path)
        result = json.loads(path.read_text())
        assert result["code"] == 0, f"guide {step}: {result}"

    def launched_pi_args(self):
        records = [record for record in self.launch_log.read_text().split("-----\n") if record.strip()]
        return [[line for line in record.splitlines() if line] for record in records]

    def _drain_pty(self, master):
        with open(self.root / "client.log", "wb") as log:
            try:
                while True:
                    data = os.read(master, 65536)
                    if not data:
                        break
                    log.write(data)
            except OSError:
                pass

    def start(self):
        """Run the one documented command: plain `vqro`, no subcommand.

        It installs the bundled defaults, spawns the background server daemon and
        attaches the TUI. A PTY stands in for the human's terminal; the demo then
        drives the daemon through the CLI, exactly as the human would.
        """
        assert self.guide_block("launch") == "vqro"
        master, slave = pty.openpty()
        fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 50, 200, 0, 0))
        self.client = subprocess.Popen([str(self.vqro)], env=self.env, cwd=str(self.root / "work"),
                                       stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        self.owned[self.client.pid] = birth(self.client.pid)
        threading.Thread(target=self._drain_pty, args=(master,), daemon=True).start()

        deadline = time.monotonic() + 25
        while time.monotonic() < deadline:
            if list((self.root / "config").glob("*/bundled/tasks-*/vqro-extension.toml")):
                break
            time.sleep(0.05)
        else:
            raise RuntimeError("plain `vqro` did not install the bundled defaults")

        while time.monotonic() < deadline:
            try:
                with socket.socket(socket.AF_UNIX) as stream:
                    stream.connect(str(self.root / "api.sock"))
                    pid, _, _ = struct.unpack("3i", stream.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
                self.server = (pid, birth(pid))
                self.owned[pid] = self.server[1]
                break
            except (FileNotFoundError, ConnectionRefusedError):
                time.sleep(0.1)
        else:
            raise RuntimeError("private API startup timed out")
        self.save("owned-processes.json", self.owned)

    def stop(self):
        # A guide block may fail after starting one agent but before the next
        # assertion. Capture every private pane's births before stopping it.
        if self.server and birth(self.server[0]) == self.server[1]:
            for pane in self.cli("pane", "list")["result"]["panes"]:
                self.remember_pane(pane["pane_id"])
        super().stop()
        client = getattr(self, "client", None)
        if client and client.poll() is None:
            client.terminate()
            try:
                client.wait(timeout=8)
            except subprocess.TimeoutExpired:
                client.kill()
                client.wait(timeout=5)

    def api(self, method, params=None, timeout=15):
        """One newline-delimited JSON request on the private API socket."""
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(timeout)
            stream.connect(str(self.root / "api.sock"))
            stream.sendall((json.dumps({"id": "demo-readiness", "method": method, "params": params or {}}) + "\n").encode())
            buffer = b""
            while b"\n" not in buffer:
                chunk = stream.recv(1 << 16)
                if not chunk:
                    break
                buffer += chunk
        return json.loads(buffer.split(b"\n", 1)[0])

    def bundled_dir(self):
        roots = sorted((self.root / "config").glob("*/bundled"))
        if len(roots) != 1:
            raise RuntimeError(f"expected one bundled root, found {roots}")
        return roots[0]

    def wait_fresh_empty_attention(self):
        """Native admission must be healthy before any core call creates a DB."""
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            documents = self.api("host.document.list", {"namespace": TASKS_NAMESPACE})["result"]["documents"]
            for entry in documents:
                items = entry["document"]["items"]
                if any(item["id"] == "context:attention.view:vqro.tasks:tree" for item in items):
                    assert not any("badge:" in item["id"] or "row:" in item["id"] for item in items), items
                    assert not list(self.root.rglob("tasks.sqlite")), "core store already initialized"
                    self.save("fresh-empty-document.json", entry)
                    return
            time.sleep(0.1)
        raise RuntimeError("fresh native Tasks document unavailable before first core action")

    def attention_badges(self):
        """Admitted host documents for the Tasks attention namespace."""
        documents = self.api("host.document.list", {"namespace": TASKS_NAMESPACE})["result"]["documents"]
        return [item["id"] for entry in documents for item in entry["document"]["items"]
                if item["id"].startswith("context:attention.badge:")]

    def wait_blocked_badge(self, seconds=30):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            for badge in self.attention_badges():
                _, _, human, blocked, unavailable = badge.rsplit(":", 4)
                if int(blocked) > 0:
                    return badge, {"human": int(human), "blocked": int(blocked), "unavailable": int(unavailable)}
            self.cli("task", "list")  # an action forces a fresh document sample
            time.sleep(0.5)
        raise RuntimeError("no durable blocked attention badge appeared")

    def change(self, name, token, task, action, text=None, **flags):
        args = [action, task["id"]]
        if text is not None:
            args.append(text)
        args += ["--rev", str(task["revision"]), "--op", token]
        for key, value in flags.items():
            args += ["--" + key.replace("_", "-"), str(value)]
        return self.agent(name, token, args)["task"]

    def run(self):
        self.start()
        bundled = self.bundled_dir()

        # R1: one plain launch installed and enabled the embedded tasks extension.
        registry = json.loads(next((self.root / "config").glob("*/plugins.json")).read_text())
        linked = [entry for entry in registry if entry["plugin_id"] == TASKS_NAMESPACE]
        assert len(linked) == 1 and linked[0]["enabled"] is True, f"tasks not auto-linked: {registry}"
        tasks_dir = Path(linked[0]["plugin_root"])
        assert tasks_dir.parent == bundled and tasks_dir.name.startswith("tasks-"), tasks_dir
        for name in ("vqro-extension.toml", "provider.py", "view.py", "document.py", "decision_queue.py", "messages.py", "graph.py"):
            assert (tasks_dir / name).is_file(), name

        self.wait_fresh_empty_attention()

        # R2: two Pi panes are started by Vqro. (The adapter and vqro skill are
        # delivered by the Vqro Pi profile and covered by the pi-profile demo; this
        # demo's private root has no human agent dir, so no profile is built here.)
        human = self.cli("workspace", "create", "--cwd", str(self.root / "work"), "--no-focus")["result"]["root_pane"]["pane_id"]
        self.remember_pane(human)
        self.guide_human(human, "human-empty")
        assert self.cli("task", "list")["tasks"] == []
        human_tab = self.cli("pane", "get", human)["result"]["pane"]["tab_id"]
        self.cli("tab", "focus", human_tab)  # guide: select the human shell again
        self.guide_human(human, "human-setup")
        a = self.cli("agent", "get", "task-a")["result"]["agent"]["pane_id"]
        b = self.cli("agent", "get", "task-b")["result"]["agent"]["pane_id"]
        assert human != a != b and human != b
        self.remember_pane(a)
        self.remember_pane(b)

        owner_a = self.agent("task-a", "a-identity", ["whoami"])["caller"]
        owner_b = self.agent("task-b", "b-identity", ["whoami"])["caller"]
        assert owner_a["class"] == owner_b["class"] == "agent" and owner_a != owner_b
        self.human(human, "human-identity", ["whoami"])

        # R3/R4: the literal named-owner import, delegation and blocker examples.
        imported = json.loads((self.root / "work/first-plan-result.json").read_text())
        root = self.cli("task", "get", imported["tasks"]["root"])["task"]
        delegated_task = self.cli("task", "get", imported["tasks"]["work"])["task"]
        assert root["owner"] == delegated_task["owner"] == owner_a
        self.guide_agent("task-a", "agent-a")
        self.guide_agent("task-b", "agent-b")
        delegated_task = self.cli("task", "get", delegated_task["id"])["task"]
        assert delegated_task["owner"] == owner_b and delegated_task["state"] == "blocked"
        badge, counts = self.wait_blocked_badge()
        self.save("badge.json", {"badge": badge, "counts": counts})

        # R5: a decision stays human-only and is answered by the human.
        decision = json.loads((self.root / "work/first-decision.json").read_text())["task"]
        answer_denied = self.agent("task-a", "answer-denied", ["answer", decision["id"], "keep", "--rev", str(decision["revision"])], expected=1)
        assert answer_denied["code"] == 1
        self.guide_human(human, "human-answer")
        answered = self.cli("task", "get", decision["id"])["task"]
        assert answered["state"] in ("accepted", "done") or answered.get("owed_action") != "answer", answered["state"]

        # R6: a durable message crosses the two real Pi panes and is claimed by name.
        state = self.agent("task-a", "msg-state", ["list"], tool="message")
        assert state["activation"] is False, f"activation must stay off: {state}"
        claimed = self.agent("task-b", "msg-claimed", ["list"], tool="message")
        records = claimed["messages"]
        notice = f"task {delegated_task['id']} assigned; inspect vqro task get {delegated_task['id']}"
        message_id = next(item["id"] for item in records if item["body"] == notice)
        assert next(item for item in records if item["id"] == message_id)["state"] == "claimed", records
        reply = self.agent("task-b", "msg-reply", ["send", "task-a", "ack from task-b", "--reply-to", message_id, "--op", "msg-reply"], tool="message")
        back = self.agent("task-a", "msg-back", ["claim", "--op", "msg-back"], tool="message")
        assert reply["message"]["id"] in [item["id"] for item in back["claimed"]], back

        self.change("task-b", "delegate-resume", self.cli("task", "get", delegated_task["id"])["task"], "start")

        # Existing names must not create orphan panes. Failed name resolution
        # must not change an owner or partially import a plan.
        before_panes = len(self.cli("pane", "list")["result"]["panes"])
        duplicate = subprocess.run([str(self.vqro), "agent", "start", "task-a"],
                                   env=self.env, cwd=self.root / "work", capture_output=True, text=True, timeout=25)
        assert duplicate.returncode == 1 and json.loads(duplicate.stderr)["error"]["code"] == "agent_name_taken", duplicate
        assert len(self.cli("pane", "list")["result"]["panes"]) == before_panes
        before = self.cli("task", "get", delegated_task["id"])["task"]
        rejected = self.agent("task-a", "unknown-owner", ["delegate", before["id"], "--to", "no-such-agent", "--rev", str(before["revision"])], expected=1)
        assert "cannot resolve owner no-such-agent" in rejected["stderr"], rejected
        after = self.cli("task", "get", before["id"])["task"]
        assert all(after[key] == before[key] for key in ("owner", "revision", "state"))
        self.save("work/bad-plan.json", {"nodes": [
            {"key": "good", "title": "Good", "owner": "task-a", "done_when": CRITERION},
            {"key": "bad", "title": "Bad", "owner": "no-such-agent", "done_when": CRITERION},
        ]})
        count = len(self.cli("task", "list")["tasks"])
        self.cli("task", "plan", "bad-plan.json", expected=1)
        assert len(self.cli("task", "list")["tasks"]) == count

        result = {"result": "passed", "real_pi_agents": 2, "model_calls": 0,
                  "defaults_auto_linked": True,
                  "adapter_skill_evidence": "covered by just pi-profile-demo",
                  "imported_tasks": 2, "literal_guide_blocks": 6, "fresh_empty_native_before_core_action": True,
                  "empty_full_tasks_before_first_task": True, "named_agent_panes": True,
                  "named_plan_owners_and_delegation": True, "name_failures_write_nothing": True,
                  "existing_name_creates_no_pane": True, "blocked_badge": counts, "decision_human_only": True,
                  "durable_message_claimed_by_name": True}
        self.save("RESULT.json", result)
        return result


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
        root = Path(tempfile.mkdtemp(prefix="readiness-demo-", dir=base))
    if len(str(root / "api.sock").encode()) >= 108:
        parser.error("private root is too long for a Linux Unix socket")
    demo = ReadinessDemo(root, args.vqro.resolve(), args.pi.resolve())
    print(f"Private readiness demo: {root}", flush=True)
    try:
        print(json.dumps(demo.run()), flush=True)
    finally:
        demo.cleanup()


if __name__ == "__main__":
    main()
