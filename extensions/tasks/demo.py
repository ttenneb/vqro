#!/usr/bin/env python3
"""Real Vqro + two real model-free Pi processes, exclusively in a new private root.
No installation, credentials, model calls, fake host, or human-session changes.
"""
import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import socket
import struct
import subprocess
import tempfile
import time

HERE = Path(__file__).resolve().parent


def birth(pid):
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
        return stat[stat.rfind(")") + 2:].split()[19]
    except FileNotFoundError:
        return None


class Demo:
    def __init__(self, root, vqro, pi):
        self.root, self.vqro, self.pi = root, vqro, pi
        self.process = None
        self.server = None
        self.owned = {}
        for directory in ("bin", "home", "config", "state", "run", "work", "commands", "receipts"):
            (root / directory).mkdir(mode=0o700)
        (root / "bin/vqro").symlink_to(vqro)
        (root / "bin/pi").symlink_to(pi)
        config = root / "config/config.toml"
        config.write_text("[session]\nresume_agents_on_restore = false\n[update]\nversion_check = false\nmanifest_check = false\n[advanced]\nscrollback_limit_bytes = 10000321\n")
        self.env = {"HOME": str(root / "home"), "XDG_CONFIG_HOME": str(root / "config"),
                    "XDG_STATE_HOME": str(root / "state"), "XDG_RUNTIME_DIR": str(root / "run"),
                    "VQRO_CONFIG_PATH": str(config), "VQRO_SOCKET_PATH": str(root / "api.sock"),
                    "VQRO_CLIENT_SOCKET_PATH": str(root / "client.sock"),
                    "PATH": f"{root / 'bin'}:/usr/bin:/bin", "TERM": "xterm-256color", "SHELL": "/bin/bash",
                    "LANG": "C.UTF-8", "PI_CODING_AGENT_DIR": str(root / "home/.pi/agent"),
                    "PI_SKIP_VERSION_CHECK": "1", "NODE_COMPILE_CACHE": str(root / "node-cache")}
        template = r'''import {execFileSync} from 'node:child_process';
import {readFileSync,writeFileSync} from 'node:fs';
import net from 'node:net';
const root=ROOT;
export default function(pi:any) {
 pi.registerCommand('taskcore', {description:'Approved private task-core demo action', handler: async(token:string,ctx:any)=>{
  if(!/^[a-z0-9-]+$/.test(token)) throw new Error('invalid demo token');
  const command=JSON.parse(readFileSync(`${root}/commands/${token}.json`,'utf8'));
  try {
   if(command.drop) {
    await new Promise<void>((resolve,reject)=>{
     const stream=net.createConnection(process.env.VQRO_SOCKET_PATH!);
     stream.on('error',reject);
     stream.on('connect',()=>stream.write(JSON.stringify({id:token,method:'extension.action.invoke',params:{service_id:'vqro.tasks.core',payload:command.drop}})+'\n',()=>{stream.destroy();resolve();}));
    });
    writeFileSync(`${root}/receipts/${token}.json`,JSON.stringify({code:0,stdout:'',dropped_reply:true}),{mode:0o600});
   } else {
    const stdout=execFileSync('vqro',['task',...command.args],{encoding:'utf8',timeout:15000});
    writeFileSync(`${root}/receipts/${token}.json`,JSON.stringify({code:0,stdout}),{mode:0o600});
   }
   ctx.ui.notify(`Task core ${token} recorded`,'info');
  } catch(error:any) {
   writeFileSync(`${root}/receipts/${token}.json`,JSON.stringify({code:error.status??1,stdout:String(error.stdout??''),stderr:String(error.stderr??error)}),{mode:0o600});
   ctx.ui.notify(`Task core ${token} rejected`,'info');
  }
 }});
}
'''
        (root / "demo-agent.ts").write_text(template.replace("ROOT", json.dumps(str(root))).replace("+'\\\\n'", "+'\\n'"))

    def save(self, name, value):
        (self.root / name).write_text(json.dumps(value, indent=2) + "\n")

    def cli(self, *args, expected=0):
        result = subprocess.run([str(self.vqro), *args], env=self.env, cwd=self.root / "work", capture_output=True, text=True, timeout=25)
        if result.returncode != expected:
            raise RuntimeError(f"{args}: {result.returncode}: {result.stdout} {result.stderr}")
        return json.loads(result.stdout) if result.stdout.strip().startswith("{") else result.stdout

    def start(self):
        console = open(self.root / f"server-{len(self.owned)}.log", "w")
        self.process = subprocess.Popen([str(self.vqro), "server"], env=self.env, cwd=self.root / "work", stdin=subprocess.DEVNULL, stdout=console, stderr=subprocess.STDOUT, start_new_session=True)
        console.close()
        self.owned[self.process.pid] = birth(self.process.pid)
        deadline = time.monotonic() + 12
        while time.monotonic() < deadline:
            try:
                with socket.socket(socket.AF_UNIX) as stream:
                    stream.connect(str(self.root / "api.sock"))
                    pid, uid, gid = struct.unpack("3i", stream.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
                self.server = (pid, birth(pid))
                self.owned[pid] = self.server[1]
                break
            except (FileNotFoundError, ConnectionRefusedError):
                if self.process.poll() is not None:
                    raise RuntimeError("private server exited during startup")
                time.sleep(.1)
        else:
            raise RuntimeError("private API startup timed out")
        logs = list((self.root / "config").rglob("*server.log"))
        assert any("pane_scrollback_limit_bytes=10000321" in log.read_text() for log in logs), "private config canary missing"
        self.save("owned-processes.json", self.owned)

    def stop(self):
        if self.server and birth(self.server[0]) == self.server[1]:
            self.cli("server", "stop")
        if self.process:
            self.process.wait(timeout=12)
        self.server = None

    def remember_pane(self, pane):
        info = self.cli("pane", "process-info", "--pane", pane)["result"]["process_info"]
        for pid in [info.get("shell_pid"), *(item["pid"] for item in info["foreground_processes"])]:
            if pid:
                self.owned[pid] = birth(pid)
        self.save("owned-processes.json", self.owned)

    def wait_file(self, path):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if path.exists():
                return
            time.sleep(.05)
        raise RuntimeError(f"private demo result did not arrive: {path}")

    def agent(self, name, token, args=None, expected=0, drop=None):
        self.save(f"commands/{token}.json", {"args": args, **({"drop": drop} if drop else {})})
        self.cli("agent", "prompt", name, f"/taskcore {token}")
        result_file = self.root / f"receipts/{token}.json"
        self.wait_file(result_file)
        result = json.loads(result_file.read_text())
        if result["code"] != expected:
            raise RuntimeError(f"agent action {token}: {result}")
        if result.get("stdout", "").strip().startswith("{"):
            return json.loads(result["stdout"])
        return result

    def human(self, pane, token, args):
        output, error, status = [self.root / f"receipts/{token}.{suffix}" for suffix in ("out", "err", "status")]
        command = shlex.join(["vqro", "task", *args]) + f" > {shlex.quote(str(output))} 2> {shlex.quote(str(error))}; printf '%s' \"$?\" > {shlex.quote(str(status))}"
        self.cli("pane", "run", pane, command)
        self.wait_file(status)
        result = {"code": int(status.read_text()), "stdout": output.read_text(), "stderr": error.read_text()}
        self.save(f"receipts/{token}.json", result)
        if result["code"] != 0:
            raise RuntimeError(f"human-shell action {token}: {result}")
        return json.loads(result["stdout"])

    def launch(self, name, pane):
        self.cli("pane", "wait-output", pane, "--regex", r"\$", "--timeout", "5000")
        result = self.cli("agent", "start", name, "--kind", "pi", "--pane", pane, "--timeout", "10000", "--", "--no-session", "--no-extensions", "--no-skills", "--no-prompt-templates", "--no-themes", "-e", str(self.root / "demo-agent.ts"))
        self.remember_pane(pane)
        return result

    def run(self):
        self.start()
        self.cli("plugin", "link", str(HERE))
        human = self.cli("workspace", "create", "--cwd", str(self.root / "work"), "--no-focus")["result"]["root_pane"]["pane_id"]
        a = self.cli("pane", "split", "--pane", human, "--direction", "right", "--no-focus")["result"]["pane"]["pane_id"]
        b = self.cli("pane", "split", "--pane", human, "--direction", "down", "--no-focus")["result"]["pane"]["pane_id"]
        self.remember_pane(human)
        self.launch("task-a", a)
        self.launch("task-b", b)
        owner_a = self.agent("task-a", "a-identity", ["whoami"])["caller"]
        owner_b = self.agent("task-b", "b-identity", ["whoami"])["caller"]
        assert owner_a["class"] == owner_b["class"] == "agent" and owner_a != owner_b
        root = self.agent("task-a", "a-root", ["create", "Finish the task core", "--op", "a-root"])["task"]
        root = self.agent("task-a", "a-start", ["start", root["id"], "--rev", "1", "--op", "a-start"])["task"]
        child = self.agent("task-a", "a-child", ["create", "Verify a delegated outcome", "--parent", root["id"], "--op", "a-child"])["task"]
        child = self.agent("task-a", "a-delegate", ["delegate", child["id"], "--owner", json.dumps(owner_b), "--rev", "1", "--op", "a-delegate"])["task"]
        child = self.agent("task-b", "b-start", ["start", child["id"], "--rev", "2", "--op", "b-start"])["task"]
        child = self.agent("task-b", "b-block", ["block", child["id"], "Need the final check", "--rev", "3", "--op", "b-block"])["task"]
        assert child["owed_to"] == owner_b and child["owed_action"] == "unblock"
        self.save("blocked-tree.json", self.cli("task", "tree", "--json"))
        child = self.agent("task-b", "b-resume", ["start", child["id"], "--rev", "4", "--op", "b-resume"])["task"]
        child = self.agent("task-b", "b-report", ["report", child["id"], "Real delegated work reported", "--rev", "5", "--op", "b-report"])["task"]
        rejected = self.agent("task-b", "b-accept-denied", ["accept", child["id"], "--rev", "6"], expected=1)
        assert "human-only" in rejected["stderr"]
        decision = self.agent("task-a", "a-decision", ["decision", "Keep the smaller task slice?", "--parent", root["id"], "--op", "a-decision"])["task"]
        tree = self.agent("task-a", "a-debt-tree", ["tree"])
        assert '"accept"' in tree["stdout"] and '"answer"' in tree["stdout"] and "human_unverified" in tree["stdout"]
        owner_human = self.human(human, "human-identity", ["whoami"])["caller"]
        child = self.human(human, "human-accept-child", ["accept", child["id"], "--rev", "6", "--op", "human-accept-child"])["task"]
        assert child["state"] == "accepted" and child["owed_to"] is None
        child = self.agent("task-b", "b-done", ["done", child["id"], "--rev", "7", "--op", "b-done"])["task"]
        decision = self.human(human, "human-answer", ["answer", decision["id"], "Yes, keep the smaller slice", "--rev", "1", "--op", "human-answer"])["task"]
        assert decision["state"] == "done" and decision["detail"] == "Yes, keep the smaller slice"
        root = self.agent("task-a", "a-report", ["report", root["id"], "Child accepted and decision answered", "--rev", "2", "--op", "a-report"])["task"]
        root = self.human(human, "human-accept-root", ["accept", root["id"], "--rev", "3", "--op", "human-accept-root"])["task"]
        root = self.agent("task-a", "a-done", ["done", root["id"], "--rev", "4", "--op", "a-done"])["task"]
        self.agent("task-a", "lost-reply", drop={"action": "create", "title": "Lost reply root", "operation_id": "lost-reply"})
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            receipt = self.cli("task", "op", "lost-reply", "--caller", json.dumps(owner_a))
            if receipt["found"]:
                break
            time.sleep(.1)
        else:
            raise RuntimeError("lost reply did not produce a recoverable receipt")
        lost = receipt["outcome"]["task"]
        replay = self.agent("task-a", "lost-replay", ["create", "Lost reply root", "--op", "lost-reply"])
        assert replay["task"]["id"] == lost["id"]
        self.save("lost-reply-receipt.json", receipt)
        before = self.cli("task", "tree", "--json")
        self.save("before-restart.json", before)
        self.cli("plugin", "disable", "vqro.tasks")
        self.cli("task", "list", expected=1)
        # Generic terminal control remains usable while task authority is absent.
        self.cli("pane", "run", human, "printf 'provider-independent terminal\\n'")
        self.cli("pane", "wait-output", human, "--match", "provider-independent terminal", "--timeout", "5000")
        self.cli("plugin", "enable", "vqro.tasks")
        after_provider = self.cli("task", "tree", "--json")
        assert before == after_provider
        self.save("after-provider-restart.json", after_provider)
        self.stop()
        self.start()
        after_server = self.cli("task", "tree", "--json")
        assert before == after_server
        self.save("after-server-restart.json", after_server)
        restored_receipt = self.cli("task", "op", "b-report", "--caller", json.dumps(owner_b))
        assert restored_receipt["outcome"]["task"]["state"] == "reported" and restored_receipt["caller"] == owner_b
        self.save("original-report-after-restart.json", restored_receipt)
        self.launch("task-replacement", a)
        replacement = self.agent("task-replacement", "replacement-identity", ["whoami"])["caller"]
        assert replacement["terminal_id"] == owner_a["terminal_id"] and replacement["execution"] != owner_a["execution"]
        refused = self.agent("task-replacement", "replacement-denied", ["start", lost["id"], "--rev", "1"], expected=1)
        assert "not owned" in refused["stderr"]
        reassigned = self.human(human, "human-reassign", ["delegate", lost["id"], "--owner", json.dumps(replacement), "--rev", "1", "--op", "human-reassign"])["task"]
        assert reassigned["creator"] == owner_a and reassigned["owner"] == replacement
        self.agent("task-replacement", "replacement-start", ["start", lost["id"], "--rev", "2", "--op", "replacement-start"])
        self.agent("task-replacement", "replacement-report", ["report", lost["id"], "Explicitly reassigned work", "--rev", "3", "--op", "replacement-report"])
        self.human(human, "human-accept-replacement", ["accept", lost["id"], "--rev", "4", "--op", "human-accept-replacement"])
        self.agent("task-replacement", "replacement-done", ["done", lost["id"], "--rev", "5", "--op", "replacement-done"])
        final = self.cli("task", "tree", "--json")
        assert len(final["tasks"]) == 4 and all(task["state"] == "done" and task["owed_to"] is None for task in final["tasks"])
        self.save("final-tree.json", final)
        result = {"result": "passed", "real_pi_agents": 2, "model_calls": 0, "task_count": 4,
                  "delegated_report_human_acceptance": True, "human_decision_answer": True,
                  "lost_reply_recovered_once": True, "provider_and_server_restart_exact": True,
                  "replacement_requires_explicit_reassignment": True, "human_class": "cooperative_unverified"}
        self.save("RESULT.json", result)
        return result

    def cleanup(self):
        try:
            self.stop()
        finally:
            for pid, original_birth in self.owned.items():
                if original_birth and birth(pid) == original_birth:
                    os.kill(pid, signal.SIGTERM)
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline and any(original_birth and birth(pid) == original_birth for pid, original_birth in self.owned.items()):
                time.sleep(.1)
            remaining = [pid for pid, original_birth in self.owned.items() if original_birth and birth(pid) == original_birth]
            self.save("cleanup.json", {"remaining_owned_processes": remaining, "data_retained": True})
            if remaining:
                raise RuntimeError(f"private processes remain (no broad kill): {remaining}")


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
        root = Path(tempfile.mkdtemp(prefix="taskdemo-", dir=base))
    if len(str(root / "api.sock").encode()) >= 108:
        parser.error("private root is too long for a Linux Unix socket")
    demo = Demo(root, args.vqro.resolve(), args.pi.resolve())
    print(f"Private task demo: {root}", flush=True)
    try:
        print(json.dumps(demo.run()), flush=True)
    finally:
        demo.cleanup()


if __name__ == "__main__":
    main()
