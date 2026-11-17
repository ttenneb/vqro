"""Run a local Pi bridge as a backend-owned terminal program, never an extension host."""
import argparse
import asyncio
import os
from pathlib import Path
import sys
import threading

# This program owns its stdin EOF and must close its Pi child cleanly. It is not
# a managed backend extension; don't let prim_ext's import watcher os._exit first.
os.environ.pop("PRIM_EXTENSION_LIFETIME", None)
os.environ.pop("PRIM_EXTENSION", None)

from .program import Bridge
from .display import plain, tail
from prim_provider import ProviderError


def watch_stdin(stream, loop, stop):
    """Only lifetime/quit, never forwarded input or a second agent input channel."""
    # Do not hold sys.stdin's BufferedReader lock in a daemon thread: native
    # Pi can exit while this read is blocked, and interpreter shutdown would
    # abort trying to close the locked buffered stream. Raw descriptor reads
    # work for backend terminal handles and pipes without that Python I/O lock.
    fd = stream.fileno()
    def read():
        try:
            while os.read(fd, 4096):
                pass
        finally:
            try:
                loop.call_soon_threadsafe(stop.set)
            except RuntimeError:
                pass  # the loop has already finished
    thread = threading.Thread(target=read, daemon=True, name="pi-bridge-lifetime")
    thread.start()
    return thread


async def run(args):
    stop = asyncio.Event()
    watch_stdin(sys.stdin.buffer, asyncio.get_running_loop(), stop)
    bridge = Bridge(args.argv, cwd=args.cwd, env=os.environ, token=os.environ["PRIM_BRIDGE_TOKEN"],
                    endpoint=os.environ["PRIM_SOCKET"], interval=args.interval,
                    transcript_bytes=args.transcript_bytes)
    result = await bridge.run(stop)
    code = result.get("exit_code", 0)
    return code if isinstance(code, int) and code >= 0 else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cwd", type=Path, default=Path.cwd())
    parser.add_argument("--interval", type=float, default=.2, help="seconds between coalesced updates (>=0.1)")
    parser.add_argument("--transcript-bytes", type=int, default=16384)
    parser.add_argument("argv", nargs=argparse.REMAINDER, help="-- <Pi command and RPC arguments>; no runtime default")
    args = parser.parse_args()
    if args.argv and args.argv[0] == "--":
        args.argv = args.argv[1:]
    if not args.argv:
        parser.error("explicit Pi argv is required after -- (include --mode rpc)")
    if not os.environ.get("PRIM_BRIDGE_TOKEN") or not os.environ.get("PRIM_SOCKET"):
        parser.error("PRIM_BRIDGE_TOKEN and opaque PRIM_SOCKET must come from bridge:true spawn")
    if not 256 <= args.transcript_bytes <= 65536:
        parser.error("transcript byte limit must be 256..65536")
    try:
        code = asyncio.run(run(args))
    except KeyboardInterrupt:
        code = 130
    except (ValueError, OSError, ProviderError) as error:
        print("Pi bridge could not start: " + tail(plain(error), 1024), file=sys.stderr)
        code = 1
    raise SystemExit(code)


if __name__ == "__main__":
    main()
