#!/usr/bin/env python3
"""A send from the agents UI reaches a v0 adapter with the core's recorded identity (C2, agent-v0 section 3.2).

A private core (--fake, only the agents extension), a fake v0 agent provider that binds origin identity as an adapter
must (any supplied principal/actor/initiator that differs from the effect's recorded attribution is `forbidden`
before any native write), and one client that sends through `agents.send`. Passes when the adapter receives
`origin: {kind: human}` (a claim) and the effect's recorded actor is the agents extension's connection identity.
Usage: e2e_agent_origin.py [--vqro target/debug/vqro]
"""
import asyncio
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
VQRO = sys.argv[sys.argv.index("--vqro") + 1] if "--vqro" in sys.argv else str(ROOT / "target/debug/vqro")
SCRATCH = Path.home() / ".local/share/herdr/scratch"
SCRATCH.mkdir(parents=True, exist_ok=True)

PROVIDER = r'''
import asyncio, json, sys
sys.path.insert(0, sys.argv[1])
from vqro_provider import Provider, ProviderError
CAPS = {"agent": {"v": 0, "modules": {}}, "agent.send": {"modes": {"start": {"busy": ["reject"]}}}}
FACTS = {"ref": "a1", "incarnation": "i1", "kind": "agent", "state": "live", "name": "origin-probe",
         "data": {"agent": {"v": 0, "state": "idle", "lifecycle": "live", "epoch": "e1",
                            "caps": {"rev": 1, "agent.send": {"modes": {"start": {"busy": "reject"}}}}}}}
async def main():
    provider = Provider("v0probe", "inst", CAPS, software={"name": "v0probe", "version": "0"})
    @provider.effect("agent.send")
    async def send(effect, args):
        origin = args.get("origin") or {}
        recorded = {"principal": effect.principal, "actor": effect.actor, "initiator": effect.initiator}
        verdict = "ok"
        for key, value in recorded.items():  # an adapter binds identity to the core's attribution
            if key in origin and origin[key] != value:
                verdict = f"forbidden: origin.{key}={origin[key]!r} but the core recorded {value!r}"
        print(json.dumps({"origin": origin, "recorded": recorded, "verdict": verdict}), flush=True)
        if verdict != "ok":
            return {"outcome": "rejected", "error": {"kind": "forbidden", "message": verdict}}
        return {"outcome": "accepted", "data": {"receipt": {"delivery": args.get("delivery"), "stage": "admitted"}}}
    await provider.connect()
    await provider.full_sync([FACTS])
    await provider.wait_closed()
asyncio.run(main())
'''


def rpc(path, calls):
    s = socket.socket(socket.AF_UNIX)
    s.connect(str(path))
    f = s.makefile("rb")
    out = []
    for i, (method, params) in enumerate([("hello", {"client": "origin-probe"}), *calls], 1):
        s.sendall((json.dumps({"jsonrpc": "2.0", "id": i, "method": method, "params": params}) + "\n").encode())
        while True:
            msg = json.loads(f.readline())
            if msg.get("id") == i:
                out.append(msg)
                break
    s.close()
    return out[1:]


def main():
    root = Path(tempfile.mkdtemp(prefix="ao", dir=SCRATCH))
    procs = []
    failures = []
    succeeded = False
    try:
        for d in ("home", "st", "ext", "run"):
            (root / d).mkdir()
        (root / "run").chmod(0o700)
        for f in (ROOT / "extensions").glob("*.py"):
            shutil.copy(f, root / "ext")
        shutil.copytree(ROOT / "extensions/manifests", root / "ext/manifests")
        (root / "ext/defaults.json").write_text('{"version":5,"extensions":[{"name":"agents","argv":["python3","agents.py"]}]}')
        sock = root / "v.sock"
        env = {"HOME": str(root / "home"), "PATH": "/usr/bin:/bin", "VQRO_STATE_DIR": str(root / "st"), "VQRO_SOCKET": str(sock),
               "VQRO_EXTENSIONS_DIR": str(root / "ext"), "XDG_RUNTIME_DIR": str(root / "run"), "HC_SOCKET": str(root / "h.sock"),
               "HC_SESSION": "ao", "VQRO_HERDR": '["/bin/false"]'}
        procs.append(subprocess.Popen([VQRO, "serve", "--fake"], env=env, stdout=open(root / "core.log", "wb"), stderr=subprocess.STDOUT))
        for _ in range(100):
            if sock.exists():
                break
            time.sleep(0.05)
        if not sock.exists():
            raise AssertionError(f"core socket missing after unchanged startup deadline; exit={procs[0].poll()}; evidence={root}")
        (root / "probe.py").write_text(PROVIDER)
        out = open(root / "provider.out", "wb")
        procs.append(subprocess.Popen([sys.executable, str(root / "probe.py"), str(ROOT / "extensions")], env=env, stdout=out, stderr=subprocess.STDOUT))
        rid = None
        for _ in range(200):  # the agent resource, and the agents extension's command
            resources = rpc(sock, [("resource.list", {})])[0]["result"]["resources"]
            rid = next((r["id"] for r in resources if r.get("kind") == "agent"), None)
            snap = rpc(sock, [("workspace.snapshot", {})])[0]["result"]
            if rid and f"view/agents-{rid}" in snap["entries"]:
                break
            time.sleep(0.1)
        if not rid:
            failures.append("no agent resource or agents panel")
        else:
            reply = rpc(sock, [("command.invoke", {"name": "agents.send", "args": {"resource": rid, "mode": "start", "text": "hello"}})])[0]
            if "error" in reply:
                failures.append(f"agents.send failed: {reply['error']}")
            time.sleep(0.5)
            lines = [json.loads(l) for l in (root / "provider.out").read_text().splitlines() if l.startswith("{")]
            if not lines:
                failures.append("the adapter received no agent.send")
            else:
                seen = lines[-1]
                if seen["verdict"] != "ok":
                    failures.append(seen["verdict"])
                if seen["origin"] != {"kind": "human"}:
                    failures.append(f"origin should carry only the claim, got {seen['origin']}")
                if seen["recorded"]["actor"] != "extension:agents":
                    failures.append(f"recorded actor {seen['recorded']['actor']!r}, expected 'extension:agents'")
                print(json.dumps(seen))
        succeeded = not failures
    finally:
        try:
            rpc(sock, [("core.shutdown", {})])
        except Exception:
            pass
        time.sleep(0.5)
        for p in procs:
            if p.poll() is None:
                p.kill()
        if succeeded:
            shutil.rmtree(root, ignore_errors=True)
        else:
            print(json.dumps({"evidence": str(root), "core_exit": procs[0].poll() if procs else None}), file=sys.stderr)
    print(json.dumps({"result": "pass" if not failures else "fail", "failures": failures}))
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
