"""Fake terminal provider captures its private spawn for the owning RPC test harness.

The harness, not a live backend, executes the captured argv exactly once. Bootstrap
credentials remain in its private 0700 root and never enter published facts.
"""
import argparse
import asyncio
import json
import os
from pathlib import Path
import sys
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
from fake_provider import Fake, CAPABILITIES, Provider


class Capture(Fake):
    def __init__(self, provider, root):
        self.root = root
        super().__init__(provider)

    async def spawn(self, effect, args):
        answer = await super().spawn(effect, args)
        path = self.root / ("spawn-" + answer["data"]["facts"]["ref"] + ".json")
        with open(path, "x", opener=lambda p, flags: os.open(p, flags, 0o600)) as stream:
            json.dump(args, stream)
        return answer


async def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, required=True)
    args = parser.parse_args()
    provider = Provider("rpc-fixture", str(uuid.uuid4()), CAPABILITIES,
                        software={"name": "model-free-rpc-fixture", "version": "1"})
    fake = Capture(provider, args.root)
    try:
        await provider.connect()
        await provider.full_sync([])
        await provider.wait_closed()
    finally:
        await fake.close()
        await provider.close()


if __name__ == "__main__":
    asyncio.run(main())
