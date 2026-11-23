r"""Windows-only smoke against an ALREADY RUNNING private `vqro serve --fake`.

PowerShell, in a disposable directory (do not reuse your daily core/state):
  New-Item -ItemType Directory -Force state,empty-extensions | Out-Null
  $env:VQRO_STATE_DIR = "$PWD\state"
  $env:VQRO_EXTENSIONS_DIR = "$PWD\empty-extensions"
  $env:VQRO_SOCKET = '\\.\pipe\vqro-python-smoke'
  $env:VQRO_SOCKET_SOURCE = ''
  vqro serve --fake
In another terminal, from the checkout:
  py -3 tests/windows_pipe_smoke.py --endpoint '\\.\pipe\vqro-python-smoke'

Checks both transports: hello, workspace.snapshot, close. Does not stop the core,
spawn providers, change state, or replay requests. Stop the private core yourself.
"""
import argparse
import asyncio
import json
import os
import pathlib
import socket
import sys
import threading

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "extensions"))
from vqro_transport import connect_endpoint, open_endpoint

LIMIT = 64 * 1024 * 1024


def request(identifier, method):
    params = {"client": "windows-python-smoke"} if method == "hello" else {}
    return (json.dumps({"jsonrpc": "2.0", "id": identifier, "method": method, "params": params}) + "\n").encode()


def check(line, identifier):
    if not line or len(line) > LIMIT:
        raise RuntimeError("core EOF or oversized reply")
    row = json.loads(line)
    if row.get("id") != identifier or "error" in row or "result" not in row:
        raise RuntimeError(f"unexpected response: {row}")


def sync_smoke(endpoint):
    pipe = connect_endpoint(endpoint, timeout=10)
    file = pipe.makefile("rwb", buffering=0)
    try:
        for identifier, method in enumerate(("hello", "workspace.snapshot"), 1):
            rows = []
            ready = threading.Event()
            def read():
                ready.set()
                try:
                    rows.append(file.readline(LIMIT + 1))
                except BaseException as exc:
                    rows.append(exc)
            reader = threading.Thread(target=read, daemon=True)
            reader.start()
            ready.wait(1)
            pipe.sendall(request(identifier, method))
            reader.join(12)
            if reader.is_alive():
                raise TimeoutError("reader stalled; full-duplex pipe I/O failed")
            if isinstance(rows[0], BaseException):
                raise rows[0]
            check(rows[0], identifier)
        pipe.shutdown(socket.SHUT_RDWR)
    finally:
        pipe.close()
        file.close()
    print("overlapped: hello, snapshot, close OK", flush=True)


async def async_smoke(endpoint):
    async with asyncio.timeout(15):
        reader, writer = await open_endpoint(endpoint, limit=LIMIT + 1)
        try:
            for identifier, method in enumerate(("hello", "workspace.snapshot"), 1):
                writer.write(request(identifier, method))
                await writer.drain()
                check(await reader.readline(), identifier)
        finally:
            writer.close()
            await writer.wait_closed()
    print("proactor: hello, snapshot, close OK", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--endpoint", required=True, help="opaque endpoint of a private running fake core")
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("this smoke requires native Windows CPython 3.11+")
    sync_smoke(args.endpoint)
    asyncio.run(async_smoke(args.endpoint))


if __name__ == "__main__":
    main()
