"""Opt-in installed Pi 1.5 metadata-only smoke, isolated even from networking.

    python3 tests/pi_bridge_smoke.py --run

No prompts/tools/compaction/provider operations. Failure to create a network
namespace is a blocker, never a reason to fall back to the real environment.
The generated private root is retained with a JSON receipt for inspection.
"""
import argparse
import asyncio
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
from pi_bridge import PiRpcClient

NODE = "/usr/bin/node"
REPO = Path(__file__).resolve().parents[1]
# Resolve the installed CLI, or opt into a different package explicitly; never pin a user's home.
PACKAGE = Path(os.environ.get("PI_TEST_PACKAGE") or Path(shutil.which("pi") or "pi").resolve().parents[2])
CLI = PACKAGE / "dist/bundle/cli.js"


async def smoke(root):
    assert (root / ".private-pi-smoke").is_file()
    version = json.loads((PACKAGE / "package.json").read_text())["version"]
    assert version == "1.5.0", f"unreviewed installed version: {version}"
    for name in ("home", "agent", "config", "state", "runtime", "work", "sessions"):
        (root / name).mkdir(exist_ok=True)
    env = {
        "HOME": str(root / "home"), "PATH": "/usr/bin:/bin", "LANG": "C.UTF-8",
        "PI_CODING_AGENT_DIR": str(root / "agent"), "PI_SKIP_VERSION_CHECK": "1",
        "XDG_CONFIG_HOME": str(root / "config"), "XDG_STATE_HOME": str(root / "state"),
        "XDG_RUNTIME_DIR": str(root / "runtime"),
    }
    argv = [NODE, str(CLI), "--mode", "rpc", "--no-session", "--session-dir", str(root / "sessions"),
            "--no-extensions", "--no-skills", "--no-prompt-templates", "--no-themes", "--no-tools"]
    receipt = {"version": version, "argv": argv, "environment": env,
               "network_namespace": os.readlink("/proc/self/ns/net"), "responses": []}
    client = await PiRpcClient.start(argv, cwd=root / "work", env=env)
    try:
        for command in ("get_state", "get_messages", "get_commands", "get_session_stats"):
            response = await client.request(command)
            receipt["responses"].append(response)
            assert response["outcome"] == "accepted", response
        state, messages, commands, stats = [r["data"] for r in receipt["responses"]]
        assert not state["isStreaming"] and state["messageCount"] == state["pendingMessageCount"] == 0
        assert messages["messages"] == [] and commands["commands"] == []
        assert stats["tokens"]["total"] == stats["cost"] == 0
        receipt["facts_before_close"] = client.snapshot()
    finally:
        await client.close()
        receipt["exit_code"] = client.process.returncode
        receipt["events"] = client.events()
        diagnostics = client.diagnostics()
        diagnostics["stderr"] = diagnostics["stderr"].decode("utf-8", errors="replace")
        receipt["diagnostics"] = diagnostics
        receipt["files"] = [str(p.relative_to(root)) for p in root.rglob("*") if p.is_file()]
        (root / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    assert client.process.returncode == 0
    assert client.diagnostics()["stderr"] == b""
    assert not any(e["type"].startswith("agent_") for e in receipt["events"]["events"])
    print(json.dumps({"result": "PASS", "receipt": str(root / "receipt.json")}, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="explicitly opt in; no provider calls")
    parser.add_argument("--scratch-parent", type=Path, default=REPO / ".run")
    parser.add_argument("--isolated-root", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--parent-net", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.isolated_root:
        assert args.parent_net and os.readlink("/proc/self/ns/net") != args.parent_net, "network isolation missing"
        assert not any(k.startswith("HERDR_") for k in os.environ)
        asyncio.run(smoke(args.isolated_root))
        return
    if not args.run:
        parser.error("requires --run; this smoke is never part of automatic unit discovery")
    args.scratch_parent.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="pi-bridge-", dir=args.scratch_parent))
    (root / ".private-pi-smoke").touch()
    (root / "home").mkdir()
    clean = {"HOME": str(root / "home"), "PATH": "/usr/bin:/bin", "LANG": "C.UTF-8",
             "PI_TEST_PACKAGE": str(PACKAGE.resolve())}
    command = ["/usr/bin/unshare", "-Urn", sys.executable, str(Path(__file__).resolve()),
               "--isolated-root", str(root), "--parent-net", os.readlink("/proc/self/ns/net")]
    # Explicit allowlist is env -i equivalent; no credentials/Herdr identity pass.
    result = subprocess.run(command, cwd=root, env=clean, timeout=30, capture_output=True, text=True)
    (root / "driver.json").write_text(json.dumps({"argv": command, "environment": clean,
        "code": result.returncode, "stdout": result.stdout, "stderr": result.stderr}, indent=2) + "\n")
    print(result.stdout, end="")
    if result.returncode:
        print(result.stderr, file=sys.stderr, end="")
        raise SystemExit(f"isolated smoke failed (no fallback); receipt root: {root}")


if __name__ == "__main__":
    main()
