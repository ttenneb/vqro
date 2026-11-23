# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Owned stdlib PTY launcher: real CLI/TUI clients, configured identity, no native model.

The fake launcher's input is VQRO_PRINCIPAL, not a principal inferred from PID,
placement or argv. Every child uses the private core's exact environment.
"""
import asyncio
from contextlib import asynccontextmanager
import json
import os
from pathlib import Path
import struct
import subprocess
import sys

if sys.platform != "win32":
    import fcntl
    import pty
    import termios

from pi_provider_e2e import identity
from ..results import FixtureFailed, HarnessGap


class OwnedPty:
    def __init__(self, ctx, label, argv, principal):
        if sys.platform == "win32":
            raise HarnessGap("owned PTY conformance launcher is Unix-only; no ConPTY fixture is implemented")
        self.ctx,self.label,self.argv=ctx,label,argv
        self.path=ctx.root/(label+".raw")
        self.output=bytearray();self.error=None
        env=dict(ctx.backend.env);env["TERM"]="xterm-256color"
        env.pop("VQRO_PRINCIPAL",None)
        if principal is not None: env["VQRO_PRINCIPAL"]=principal
        master,slave=pty.openpty()
        fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack("HHHH",30,120,0,0))
        try:
            self.process=subprocess.Popen(argv,stdin=slave,stdout=slave,stderr=slave,
                cwd=ctx.root/"work",env=env,start_new_session=True)
        finally: os.close(slave)
        self.master=master;os.set_blocking(master,False)
        ctx.backend.tracked.append(identity(self.process.pid))
        self.reader=asyncio.create_task(self._drain())
        self.principal=principal

    async def _drain(self):
        while True:
            try: chunk=os.read(self.master,65536)
            except BlockingIOError:
                await asyncio.sleep(.005);continue
            except OSError: return
            if not chunk: return
            if len(self.output)+len(chunk)>4<<20:
                self.error=FixtureFailed("owned PTY output exceeds 4 MiB");return
            self.output.extend(chunk)

    async def text(self, needle, *, after=0):
        def ready():
            if self.error: raise self.error
            if needle in self.output[after:]: return True
            if self.process.poll() is not None:
                raise FixtureFailed(f"owned {self.label} exited {self.process.returncode}: {bytes(self.output[-2048:])!r}")
            return False
        await self.ctx.wait(ready,label=self.label+" frame "+repr(needle))

    def send(self, data):
        if self.process.poll() is not None: raise FixtureFailed("owned PTY exited before input")
        if os.write(self.master,data)!=len(data): raise FixtureFailed("short PTY input write; not replayed")

    async def close(self):
        if self.process.poll() is None:
            self.process.terminate()
            try: await asyncio.to_thread(self.process.wait,3)
            except subprocess.TimeoutExpired:
                self.process.kill();await asyncio.to_thread(self.process.wait,3)
        await asyncio.sleep(.01)
        self.reader.cancel()
        try: await self.reader
        except asyncio.CancelledError: pass
        os.close(self.master)
        self.path.write_bytes(bytes(self.output))
        self.ctx.evidence.append({"kind":"owned_pty_launcher","argv":self.argv,"configured_principal":self.principal,
            "pid":self.process.pid,"exit":self.process.returncode,"output_bytes":len(self.output),"artifact":str(self.path)})


async def cli_write(ctx,artifact_label,principal,operations,**fields):
    args={"ops":operations,**fields}
    client=OwnedPty(ctx,artifact_label,[str(ctx.vqro),"call","workspace.apply",json.dumps(args)],principal)
    try:
        await ctx.wait(lambda:client.process.poll() is not None,label=artifact_label+" CLI exit")
        ctx.check(client.process.returncode==0,"real launcher-tagged CLI write succeeds",component="core",
                  output=bytes(client.output[-4096:]).decode(errors="replace"))
    finally: await client.close()
    rows=(await ctx.peer.call("workspace.history",{"limit":1}))["txns"]
    return rows[-1]


@asynccontextmanager
async def tui(ctx,label,principal=None):
    client=OwnedPty(ctx,label,[str(ctx.vqro),"tui"],principal)
    try:
        await client.text(b"vqro")
        yield client
    finally: await client.close()


async def undo(ctx,client,key):
    # Source: src/tui/mod.rs recovery binding u -> vqro.revert, label "undo last change".
    mark=len(client.output);client.send(b"\x07")
    await client.text(b"undo",after=mark)
    client.send(b"u")
    async def absent(): return key not in (await ctx.peer.call("workspace.snapshot"))["entries"]
    await ctx.wait(absent,label="physical TUI caller-scoped undo")
