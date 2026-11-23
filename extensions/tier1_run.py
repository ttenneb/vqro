#!/usr/bin/env python3
"""tier1 wrapper: the first program in a tier-1 agent's terminal (see tier1.py).

It learns the terminal's own resource id, which a launcher can only know after the spawn, then execs the runtime
with `VQRO_AGENT` set:
1. find this terminal: the resource named `<runtime>-<VQRO_TIER1_LAUNCH>` (the launcher chose that name);
2. wait for `doc/agents/terminals/<rN>` carrying this launch nonce, and for the plugin's receive grant;
3. exec the runtime argv given after `--`.

Everything is bounded (10 s). On any problem the runtime still starts, without `VQRO_AGENT`, so its plugin stays
inert, and one line says why: the human is never blocked. Stdlib only; no state is kept.
"""
import json
import os
import sys
import time

from vqro_transport import connect_endpoint

DEADLINE_S = 10


class Core:
    def __init__(self, path):
        self.sock = connect_endpoint(path, timeout=3)
        self.file = self.sock.makefile("rw", encoding="utf-8")
        self.next = 0
        principal = os.environ.get("VQRO_PRINCIPAL")
        hello = {"client": "vqro-tier1-run", **({"identity": {"kind": "principal", "principal": principal}} if principal else {})}
        self.call("hello", hello)

    def call(self, method, params):
        self.next += 1
        self.file.write(json.dumps({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}) + "\n")
        self.file.flush()
        while True:
            line = self.file.readline()
            if not line:
                raise OSError("core closed the connection")
            row = json.loads(line)
            if row.get("id") == self.next:
                if "error" in row:
                    raise OSError(row["error"].get("message", "error"))
                return row.get("result")

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


def resolve(core, nonce):
    """The terminal's rN once its declaration (with this nonce) and the receive grant are recorded, else None."""
    rid = None
    end = time.monotonic() + DEADLINE_S
    while time.monotonic() < end:
        if rid is None:
            rows = core.call("resource.list", {}).get("resources", [])
            rid = next((r["id"] for r in rows if isinstance(r.get("name"), str) and r["name"].endswith("-" + nonce)), None)
        if rid is not None:
            declared = core.call("workspace.get", {"key": "doc/agents/terminals/" + rid})
            granted = core.call("workspace.get", {"key": "doc/agents/grants/tier1-" + rid})
            if declared and (declared.get("value") or {}).get("launch") == nonce and granted:
                return rid
        time.sleep(0.1)
    return None


def main(argv):
    if "--" not in argv or argv.index("--") == len(argv) - 1:
        print("usage: tier1_run.py -- <runtime argv>", file=sys.stderr)
        return 2
    command = argv[argv.index("--") + 1:]
    env = dict(os.environ)
    env.pop("VQRO_ACCOUNT", None)  # internal nonsecret launch fact is already recorded in the declaration
    nonce, path = env.pop("VQRO_TIER1_LAUNCH", None), env.get("VQRO_SOCKET")
    why = None
    if not nonce or not path:
        why = "no launch nonce or core endpoint"
    else:
        try:
            core = Core(path)
            try:
                rid = resolve(core, nonce)
            finally:
                core.close()
            if rid:
                env["VQRO_AGENT"] = rid
            else:
                why = "this terminal's agent declaration was not recorded within 10 s"
        except (OSError, ValueError) as error:
            why = f"core unavailable ({error})"
    if why:
        print(f"vqro tier1: starting {command[0]} without its inbox plugin: {why}.", file=sys.stderr, flush=True)
    try:
        os.execvpe(command[0], command, env)
    except OSError as error:
        print(f"vqro tier1: cannot start {command[0]}: {error}", file=sys.stderr)
        return 127


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
