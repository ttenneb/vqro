#!/usr/bin/env python3
"""Real Vqro + two real model-free Pi processes, exclusively in a new private root.
No installation, credentials, model calls, fake host, or human-session changes.
"""
import argparse
import hashlib
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


def image_hash(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


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
        self.agent_bindings = {}
        self.human_binding = None
        for directory in ("bin", "home", "config", "state", "run", "work", "commands", "receipts"):
            (root / directory).mkdir(mode=0o700)
        # Retain exact images: a later build/source edit must not change this run.
        native_hash = image_hash(vqro)
        self.vqro = root / "bin/vqro"
        shutil.copy2(vqro, self.vqro)
        if image_hash(self.vqro) != native_hash:
            raise RuntimeError("native image changed during private copy")
        self.package = root / "package"
        shutil.copytree(HERE, self.package, ignore=shutil.ignore_patterns("__pycache__"))
        files = {str(path.relative_to(self.package)): image_hash(path) for path in sorted(self.package.rglob("*")) if path.is_file()}
        source_head = subprocess.check_output(["git", "-C", str(HERE), "rev-parse", "HEAD"], text=True).strip()
        self.save("IMAGES.json", {"source_head": source_head, "native_source": str(vqro),
                                 "native_image": "bin/vqro", "native_sha256": native_hash,
                                 "provider_package": "package", "package_sha256": files})
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
    const stdout=execFileSync('vqro',[command.tool||'task',...command.args],{encoding:'utf8',timeout:15000});
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

    def agent(self, name, token, args=None, expected=0, drop=None, tool="task"):
        if tool == "task" and args and "--op" in args and "--caller" not in args:
            args = [*args, "--caller", json.dumps(self.agent_bindings[name])]
        self.save(f"commands/{token}.json", {"tool": tool, "args": args, **({"drop": drop} if drop else {})})
        self.cli("agent", "prompt", name, f"/taskcore {token}")
        result_file = self.root / f"receipts/{token}.json"
        self.wait_file(result_file)
        result = json.loads(result_file.read_text())
        if result["code"] != expected:
            raise RuntimeError(f"agent action {token}: {result}")
        if result.get("stdout", "").strip().startswith("{"):
            value = json.loads(result["stdout"])
            if args == ["whoami"]:
                self.agent_bindings.setdefault(name, value["caller"])
            return value
        return result

    def human(self, pane, token, args, tool="task"):
        if tool == "task" and "--op" in args and "--caller" not in args:
            args = [*args, "--caller", json.dumps(self.human_binding)]
        output, error, status = [self.root / f"receipts/{token}.{suffix}" for suffix in ("out", "err", "status")]
        command = shlex.join(["vqro", tool, *args]) + f" > {shlex.quote(str(output))} 2> {shlex.quote(str(error))}; printf '%s' \"$?\" > {shlex.quote(str(status))}"
        self.cli("pane", "run", pane, command)
        self.wait_file(status)
        result = {"code": int(status.read_text()), "stdout": output.read_text(), "stderr": error.read_text()}
        self.save(f"receipts/{token}.json", result)
        if result["code"] != 0:
            raise RuntimeError(f"human-shell action {token}: {result}")
        value = json.loads(result["stdout"])
        if args == ["whoami"]:
            self.human_binding = value["caller"]
        return value

    def launch(self, name, pane):
        self.cli("pane", "wait-output", pane, "--regex", r"\$", "--timeout", "5000")
        result = self.cli("agent", "start", name, "--kind", "pi", "--pane", pane, "--timeout", "10000", "--", "--no-session", "--no-extensions", "--no-skills", "--no-prompt-templates", "--no-themes", "-e", str(self.root / "demo-agent.ts"))
        self.remember_pane(pane)
        return result

    def run(self):
        from demo_graph import run
        return run(self)

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
