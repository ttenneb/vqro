"""Raw tmux fixture; runs only in a test-owned server and never talks to prim/Herdr."""
import json
import os
import pathlib
import sys
import time
import tty

tty.setraw(0)

def emit(b):
    while b:
        b = b[os.write(1, b):]

emit(b"WORKER-READY\r\n")
buf = b""
while True:
    data = os.read(0, 4096)
    if not data:
        break
    buf += data
    while b"\n" in buf:
        command, buf = buf.split(b"\n", 1)
        kind, _, arg = command.partition(b" ")
        if kind == b"ECHO":
            emit(b"ECHO:" + arg + b"\r\n")
        elif kind == b"OSC":
            payload = json.dumps({"state": "working", "tag": arg.decode()}).encode()
            cut = len(payload) // 2
            emit(b"BEFORE\x1b]7501;" + payload[:cut])
            time.sleep(0.03)
            emit(payload[cut:] + b"\x07AFTER\r\n")
        elif kind == b"FLOOD":
            n = int(arg)
            block = b"".join(f"L{i:06d} ".encode() + b"x" * 108 + b"\r\n" for i in range(n))
            emit(block + b"\x1b]7501;{\"lost\":true}\x07FLOOD-END\r\n")
            pathlib.Path(sys.argv[1]).write_text("done")
        elif kind == b"PING":
            emit(b"PONG\r\n")
