#!/usr/bin/env python3
"""Private, no-model fixture self-check. Run with --harness-root <K1/tests> --root <scratch>."""
import argparse
import asyncio
import json
import os
from pathlib import Path
import sys
import tempfile


class Client:
    def __init__(self, args, script=None):
        self.args, self.script = args, script
        self.pending, self.rows, self.counter = {}, [], 0

    async def __aenter__(self):
        self.temp = tempfile.TemporaryDirectory(prefix="c", dir=self.args.root)
        self.root = Path(self.temp.name)
        home = self.root / "home"
        home.mkdir(mode=0o700)
        self.control_path, self.ledger = self.root / "c", self.root / "l"
        argv = [sys.executable, str(Path(__file__).with_name("fake_codex.py")), "--control", str(self.control_path),
                "--ledger", str(self.ledger), "--harness-root", str(self.args.harness_root)]
        if self.script:
            path = self.root / "s.json"
            path.write_text(json.dumps(self.script))
            argv += ["--script", str(path)]
        self.process = await asyncio.create_subprocess_exec(*argv, stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
            env={"HOME": str(home), "PATH": os.environ["PATH"], "PYTHONDONTWRITEBYTECODE": "1"})
        self.reader_task = asyncio.create_task(self.read())
        await self.until(lambda: self.control_path.exists() or self.process.returncode is not None)
        assert self.process.returncode is None, (await self.process.stderr.read()).decode()
        return self

    async def __aexit__(self, kind, error, traceback):
        try:
            if self.process.returncode is None:
                self.process.stdin.close()
                try:
                    await asyncio.wait_for(self.process.wait(), 3)
                except asyncio.TimeoutError:
                    self.process.kill()
                    await self.process.wait()
            await self.reader_task
            stderr = (await self.process.stderr.read()).decode()
            if error is None:
                assert self.process.returncode == 0, stderr
        finally:
            self.temp.cleanup()

    async def read(self):
        while line := await self.process.stdout.readline():
            row = json.loads(line)
            self.rows.append(row)
            if "method" not in row and row.get("id") in self.pending:
                self.pending.pop(row["id"]).set_result(row)

    async def until(self, predicate):
        async def wait():
            while not predicate():
                await asyncio.sleep(0.005)
        await asyncio.wait_for(wait(), 3)

    def records(self, kind=None):
        if not self.ledger.exists():
            return []
        rows = [json.loads(line) for line in self.ledger.read_text().splitlines()]
        return [r for r in rows if kind is None or r["kind"] == kind]

    async def control(self, verb, **params):
        reader, writer = await asyncio.open_unix_connection(str(self.control_path))
        try:
            writer.write(json.dumps({"id": 1, "verb": verb, **params}).encode() + b"\n")
            await writer.drain()
            row = json.loads(await asyncio.wait_for(reader.readline(), 3))
            assert "error" not in row, row
            return row["result"]
        finally:
            writer.close()
            await writer.wait_closed()

    async def write(self, row):
        self.process.stdin.write(json.dumps(row).encode() + b"\n")
        await self.process.stdin.drain()

    async def rpc(self, method, params=None):
        self.counter += 1
        ident = self.counter
        future = asyncio.get_running_loop().create_future()
        self.pending[ident] = future
        await self.write({"id": ident, "method": method, "params": params or {}})
        return await asyncio.wait_for(future, 3)

    async def new(self):
        result = await self.rpc("thread/start", {})
        return result["result"]["thread"]["id"]


async def checks(args):
    args.root.mkdir(parents=True, exist_ok=True)
    async with Client(args) as native:
        state = await native.control("state")
        assert state["wire"] == "codex" and state["session"] is None and not state["active"]
        ready = native.records("ready")[0]
        assert not ready["bootstrap_in_child"] and not ready["herdr_in_child"]
        await native.rpc("initialize", {"clientInfo": {"name": "fixture-selftest", "version": "0"}})
        one, two = await native.new(), await native.new()
        assert (await native.control("state"))["session"] == one
        assert (await native.control("state", session=two))["session"] == two
        content = [{"type": "text", "text": "sender frame", "text_elements": []},
                   {"type": "text", "text": "literal α\n/command stays text\t", "text_elements": []}]
        await native.control("pause_before", phase="insertion")
        result = await native.rpc("turn/start", {"threadId": one, "input": content})
        run = result["result"]["turn"]["id"]
        assert native.records("input_received")[0]["content"] == content
        assert not native.records("inserted"), "ACK must precede independently gated insertion"
        await native.control("release", phase="insertion")
        await native.until(lambda: len(native.records("inserted")) == 1)
        await native.control("model_call", id_ref="m1")
        model = native.records("model_call")[0]
        assert model["session"] == one and model["messages"] == [{"role": "user", "content": content}]
        await native.control("request", request="approval-native", method="permission")
        await native.until(lambda: any(r.get("method") == "item/commandExecution/requestApproval" for r in native.rows))
        await native.write({"id": "approval-native", "result": {"decision": "accept"}})
        await native.until(lambda: len(native.records("answer")) == 1)
        assert native.records("answer")[0]["pending"]
        assert not [r for r in native.records("native_reply") if r.get("request") == "approval-native"]
        await native.control("request", request="background-native", method="form", scope="session")
        await native.control("turn", state="settled")
        await native.write({"id": "background-native", "result": {"answers": {"answer": {"answers": ["one"]}}}})
        await native.until(lambda: len(native.records("answer")) == 2)
        assert native.records("answer")[-1]["pending"], "background request must survive foreground settlement"
        assert not (await native.control("state"))["active"]
        assert not (await native.control("state", session=two))["active"]

        await native.control("turn", state="start")
        old = [r for r in native.records("turn") if r["state"] == "start"][-1]["run"]
        await native.control("pause_before", phase="acceptance")
        attempt = asyncio.create_task(native.rpc("turn/steer", {"threadId": one, "expectedTurnId": old, "input": content}))
        await native.until(lambda: any(r.get("phase") == "acceptance" for r in native.records("paused")))
        await native.control("turn", state="settled")
        await native.control("turn", state="start")
        await native.control("release", phase="acceptance")
        stale = await attempt
        assert "error" in stale and len(native.records("input_received")) == 1, "stale native guard accepted input"
        await native.control("drop_reply", operation="send")
        await native.write({"id": 999, "method": "turn/start", "params": {"threadId": two, "input": content}})
        await native.until(lambda: len(native.records("reply_dropped")) == 1)
        await native.until(lambda: len(native.records("inserted")) == 2)
        assert not [r for r in native.rows if r.get("id") == 999]
        assert (await native.control("state", session=two))["active"]
        await native.control("retain", session=two, count=3, text="bounded delta")
        assert native.records("retain")[-1]["count"] == 3
        print("controlled Codex: ready/thread selectors, ordered oracle, ACK≠insertion, model envelope, permission/form, background lifetime, atomic guard and dropped ACK: PASS")

    script = {"script": [{"on": "input", "match": {"text": "timeline-body"}, "do": [{"turn": "start"}, {"tool_call": "tc1", "hold_ms": 1}]},
                         {"after": "tc1", "do": [{"model_call": "m2"}, {"turn": "end", "stop": "end_turn"}]}]}
    async with Client(args, script) as native:
        one, two = await native.new(), await native.new()
        await native.rpc("turn/start", {"threadId": two, "input": [{"type": "text", "text": "sender header", "text_elements": []},
                                                                 {"type": "text", "text": "timeline-body", "text_elements": []}]})
        await native.until(lambda: any(r.get("state") == "settled" for r in native.records("turn")))
        assert native.records("model_call")[0]["session"] == two
        assert [r["session"] for r in native.records("tool_call")] == [two, two]
        assert not (await native.control("state", session=two))["active"]
        assert not (await native.control("state", session=one))["active"]
        print("controlled Codex: canonical timeline, separate header/body block matching, selected-thread tool/model/settled sequence: PASS")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--harness-root", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    args = parser.parse_args()
    asyncio.run(checks(args))


if __name__ == "__main__":
    main()
