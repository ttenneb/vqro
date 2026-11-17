"""External deterministic fake backend using only prim_provider (no actual programs).

Resource argv flags mirror src/adapter/fake.rs: --status-every-ms,
--status-phase-ms, --output-bytes-per-sec. This is a fixture, not a backend product.
"""
import argparse
import asyncio
from copy import deepcopy
from pathlib import Path
import sys
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "extensions"))
from prim_provider import Provider, ProviderError

CAPABILITIES = {"terminal.spawn": {"env": True, "cwd": True}, "terminal.kill": {},
                "terminal.input": {"stream": True, "effect": False}, "terminal.output": {"raw": True, "history": False, "repaint": "none"},
                "terminal.resize": {"owner": "per_client"}, "terminal.exit": {"exit_code": False}}


def load_arg(argv, flag):
    try:
        value = int(argv[argv.index(flag) + 1])
        return value if 0 < value <= 2**64 - 1 else None
    except (ValueError, IndexError):
        return None


class Fake:
    def __init__(self, provider):
        self.provider = provider
        self.terms = {}
        self.tasks = set()
        self.counter = 0
        provider.effect("terminal.spawn")(self.spawn)
        provider.effect("terminal.kill")(self.kill)
        provider.open_stream = self.open

    def task(self, coroutine):
        task = asyncio.create_task(coroutine)
        self.tasks.add(task)
        task.add_done_callback(self.tasks.discard)
        return task

    def term(self, ref, incarnation):
        term = self.terms.get(ref)
        if term is None or incarnation != "":
            raise ProviderError("not_found", "no matching fake terminal")
        return term

    async def spawn(self, effect, args):
        argv = args.get("argv", [])
        if not isinstance(argv, list) or any(not isinstance(x, str) for x in argv):
            raise ProviderError("invalid", "argv must be strings")
        if effect.ref is not None:
            raise ProviderError("invalid", "spawn has no ref")
        for key in ("name", "cwd"):
            if args.get(key) is not None and not isinstance(args[key], str):
                raise ProviderError("invalid", f"{key} must be string")
        env = args.get("env", {})
        if not isinstance(env, dict) or any(not isinstance(k, str) or not isinstance(v, str) for k, v in env.items()):
            raise ProviderError("invalid", "env must map strings to strings")
        self.counter += 1
        ref = f"f{self.counter}"
        facts = {"ref": ref, "incarnation": "", "kind": "terminal", "state": "live",
                 "name": args.get("name"), "title": None, "cwd": args.get("cwd"),
                 "argv": argv, "status": None, "exit": None}
        term = {"facts": facts, "streams": set(), "status_task": None}
        self.terms[ref] = term
        period = load_arg(argv, "--status-every-ms")
        if period:
            term["status_task"] = self.task(self.status(term, period, load_arg(argv, "--status-phase-ms") or period))
        await self.provider.upsert(deepcopy(facts))
        return {"outcome": "observed", "data": {"facts": deepcopy(facts)}}

    async def kill(self, effect, args):
        term = self.term(effect.ref, effect.incarnation)
        term["facts"].update(state="finished", exit={"reason": "killed"})
        # Like the in-process fake, the status timer observes finished at its
        # next tick. Do not cancel a metadata write halfway through socket drain.
        await self.provider.upsert(deepcopy(term["facts"]))
        for stream in list(term["streams"]):
            await stream.close({"reason": "killed"})
        term["streams"].clear()
        return {"outcome": "observed", "data": {"facts": deepcopy(term["facts"])}}

    async def open(self, stream):
        term = self.term(stream.ref, stream.incarnation)
        term["streams"].add(stream)
        def echo(data):
            stream.offer(data)  # enqueue; don't await output credit in the input worker
        stream.on_input = echo
        stream.on_resize = lambda cols, rows: None
        stream.on_end = lambda: term["streams"].discard(stream)
        stream.offer(f"fake {stream.ref}\r\n".encode())
        bps = load_arg(term["facts"]["argv"], "--output-bytes-per-sec")
        if bps:
            self.task(self.flood(term, stream, bps))
        return "none"

    async def status(self, term, period, phase):
        try:
            loop = asyncio.get_running_loop()
            next_tick = loop.time() + phase / 1000
            await asyncio.sleep(max(0, next_tick - loop.time()))
            n = 0
            while term["facts"]["state"] == "live":
                n += 1
                term["facts"]["status"] = {"state": "working" if n % 2 else "idle", "n": n,
                                          "at_ms": int(time.time() * 1000)}
                await self.provider.upsert(deepcopy(term["facts"]))
                next_tick += period / 1000
                await asyncio.sleep(max(0, next_tick - loop.time()))
        except ProviderError:
            await self.provider.close()

    async def flood(self, term, stream, bps):
        line = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ flood output\r\n"
        per_tick = max(1, min(bps * 10 // 1000, 16 * 1024 * 1024))
        chunk = (line * (min(per_tick, 65536) // len(line) + 1))[:min(per_tick, 65536)]
        try:
            loop = asyncio.get_running_loop()
            next_tick = loop.time()
            while term["facts"]["state"] == "live" and not stream.closed:
                await asyncio.sleep(max(0, next_tick - loop.time()))
                remaining = per_tick
                while remaining and not stream.closed:
                    n = min(remaining, len(chunk))
                    if not await stream.send(chunk[:n]):
                        return
                    remaining -= n
                next_tick = max(next_tick + .01, loop.time())
        except ProviderError:
            pass
        finally:
            term["streams"].discard(stream)

    async def close(self):
        for task in self.tasks:
            task.cancel()
        await asyncio.gather(*list(self.tasks), return_exceptions=True)


async def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket")
    parser.add_argument("--name", default="fake")
    parser.add_argument("--instance", default=None)
    args = parser.parse_args()
    provider = Provider(args.name, args.instance or str(uuid.uuid4()), CAPABILITIES,
                        software={"name": "fake", "version": "0.1.0"})
    fake = Fake(provider)
    try:
        await provider.connect(args.socket)
        await provider.full_sync([])
        await provider.wait_closed()
    finally:
        await fake.close()
        await provider.close()


if __name__ == "__main__":
    asyncio.run(main())
