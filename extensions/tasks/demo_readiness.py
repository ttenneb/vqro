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
import json
import shlex
import shutil
import socket
import tempfile
import time
from pathlib import Path

from demo import Demo

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
            + shlex.quote(str(self.launch_log)) + "\nexec " + shlex.quote(str(pi)) + ' "$@"\n')
        wrapper.chmod(0o755)
        (root / "bin" / "pi").unlink()
        (root / "bin" / "pi").symlink_to(wrapper)

    def launched_pi_args(self):
        records = [record for record in self.launch_log.read_text().split("-----\n") if record.strip()]
        return [[line for line in record.splitlines() if line] for record in records]

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
        assert linked[0]["plugin_root"] == str(bundled / "tasks"), linked[0]["plugin_root"]
        for name in ("vqro-extension.toml", "provider.py", "view.py", "document.py", "decision_queue.py", "messages.py", "graph.py"):
            assert (bundled / "tasks" / name).is_file(), name
        adapter = bundled / "pi" / "vqro-reply-routing.ts"
        skill = bundled / "pi" / "skills" / "vqro" / "SKILL.md"
        assert adapter.is_file() and skill.is_file(), (adapter, skill)

        # R2: two Pi panes are started by Vqro with the adapter and skill auto-loaded.
        human = self.cli("workspace", "create", "--cwd", str(self.root / "work"), "--no-focus")["result"]["root_pane"]["pane_id"]
        a = self.cli("pane", "split", "--pane", human, "--direction", "right", "--no-focus")["result"]["pane"]["pane_id"]
        b = self.cli("pane", "split", "--pane", human, "--direction", "down", "--no-focus")["result"]["pane"]["pane_id"]
        self.remember_pane(human)
        self.launch("task-a", a)
        self.launch("task-b", b)
        launches = self.launched_pi_args()
        assert len(launches) >= 2, launches
        for argv in launches:
            assert str(adapter) in argv, f"adapter not passed to Pi: {argv}"
            assert str(skill) in argv, f"skill not passed to Pi: {argv}"

        owner_a = self.agent("task-a", "a-identity", ["whoami"])["caller"]
        owner_b = self.agent("task-b", "b-identity", ["whoami"])["caller"]
        assert owner_a["class"] == owner_b["class"] == "agent" and owner_a != owner_b
        self.human(human, "human-identity", ["whoami"])

        # R3: import -> delegate. A human-approved plan assigns the two agents.
        plan = {"nodes": [
            {"key": "root", "title": "Readiness root", "owner": owner_a, "done_when": "All child outcomes are verified."},
            {"key": "import", "parent": "root", "title": "Import the plan", "owner": owner_a, "done_when": CRITERION},
            {"key": "delegate", "parent": "root", "title": "Delegate the work", "owner": owner_b, "done_when": CRITERION},
        ]}
        self.save("plan.json", plan)
        imported = self.human(human, "human-plan", ["plan", str(self.root / "plan.json"), "--op", "readiness-plan"])
        root = self.cli("task", "get", imported["tasks"]["root"])["task"]
        imported_task = self.cli("task", "get", imported["tasks"]["import"])["task"]
        delegated_task = self.cli("task", "get", imported["tasks"]["delegate"])["task"]
        self.change("task-a", "import-start", imported_task, "start")

        # R4: blocking a delegated task publishes a durable attention badge.
        self.change("task-b", "delegate-block", delegated_task, "block", "Waiting on the import receipt")
        badge, counts = self.wait_blocked_badge()
        self.save("badge.json", {"badge": badge, "counts": counts})

        # R5: a decision stays human-only and is answered by the human.
        options = json.dumps([{"key": "keep", "label": "Keep"}, {"key": "change", "label": "Change"}])
        decision = self.agent("task-a", "a-decision", ["decision", "Keep the bounded plan?", "--parent", root["id"],
                                                     "--done-when", CRITERION, "--options", options, "--default", "keep",
                                                     "--reversible", "true", "--waiting-cost", "Work waits."])["task"]
        answer_denied = self.agent("task-a", "answer-denied", ["answer", decision["id"], "keep", "--rev", str(decision["revision"])], expected=1)
        assert answer_denied["code"] == 1
        answered = self.human(human, "human-answer", ["answer", decision["id"], "keep", "--rev", str(decision["revision"])])["task"]
        assert answered["state"] in ("accepted", "done") or answered.get("owed_action") != "answer", answered["state"]

        # R6: a durable message crosses the two real Pi panes and is claimed by name.
        sent = self.agent("task-a", "msg-send", ["send", "task-b", "Readiness message from task-a", "--op", "msg-send"], tool="message")
        message_id = sent["message"]["id"]
        claimed = self.agent("task-b", "msg-claim", ["claim", "--op", "msg-claim"], tool="message")
        assert message_id in [item["id"] for item in claimed["claimed"]], claimed
        reply = self.agent("task-b", "msg-reply", ["send", "task-a", "ack from task-b", "--reply-to", message_id, "--op", "msg-reply"], tool="message")
        back = self.agent("task-a", "msg-back", ["claim", "--op", "msg-back"], tool="message")
        assert reply["message"]["id"] in [item["id"] for item in back["claimed"]], back

        self.change("task-b", "delegate-resume", self.cli("task", "get", delegated_task["id"])["task"], "start")

        result = {"result": "passed", "real_pi_agents": 2, "model_calls": 0,
                  "defaults_auto_linked": True, "pi_adapter_auto_loaded": True, "pi_skill_auto_loaded": True,
                  "imported_tasks": 3, "blocked_badge": counts, "decision_human_only": True,
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
