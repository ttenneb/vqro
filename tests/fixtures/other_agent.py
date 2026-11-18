"""Native fake non-Pi agent: only abort is declared, no run/queue/transcript schema."""
import asyncio
import os
import sys

sys.path.insert(0, os.environ["VQRO_EXTENSIONS_DIR"])
from vqro_provider import Provider


async def main():
    provider = Provider("other-agent", "fixture-other-v1", {"agent.abort": {}}, role="backend")
    count = 0
    def facts():
        return {"ref": "other", "incarnation": "", "kind": "agent", "state": "live", "name": "Other agent",
                "status": {"state": "idle"}, "data": {"custom": "other native runtime", "abort_count": count}}
    @provider.effect("agent.abort")
    async def abort(effect, args):
        nonlocal count
        count += 1
        await provider.upsert(facts())
        return {"outcome": "observed", "data": {"abort_count": count}}
    await provider.connect(os.environ["VQRO_SOCKET"])
    await provider.full_sync([facts()])
    await provider.wait_closed()


asyncio.run(main())
