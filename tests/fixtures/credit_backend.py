"""Private stdio backend fixture: record CREDIT, never connect to a real backend."""
import json
import struct
import sys

inp, out = sys.stdin.buffer, sys.stdout.buffer
record = sys.argv[1]


def frame(channel, kind, payload):
    out.write(struct.pack("<IBI", channel, kind, len(payload)) + payload)
    out.flush()


def exact(n):
    data = bytearray()
    while len(data) < n:
        chunk = inp.read(n - len(data))
        if not chunk:
            return None
        data.extend(chunk)
    return bytes(data)


json.loads(inp.readline())
out.write(b'{"jsonrpc":"2.0","id":0,"result":{"framed":true,"core":{"id":"credit-fixture","name":"private"},"version":"fixture"}}\n')
out.flush()
while True:
    header = exact(9)
    if header is None:
        break
    channel, kind, size = struct.unpack("<IBI", header)
    payload = exact(size)
    if payload is None:
        break
    if channel == 0:
        msg = json.loads(payload)
        method = msg["method"]
        if method == "events.subscribe":
            result = {"channel": 1, "terminals": []}
        elif method == "terminal.attach":
            result = {"channel": 2}
        else:
            raise AssertionError(method)
        frame(0, 0, json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}).encode())
        if method == "terminal.attach":
            frame(2, 0, b"abc")
    elif channel == 2 and kind == 1:
        with open(record, "ab") as receipt:
            receipt.write(payload)
    elif channel == 2 and kind == 2:
        break
