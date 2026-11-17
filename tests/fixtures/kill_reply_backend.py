"""Private framed hc fixture: kill replies finished, deliberately emits no finished event."""
import json
import struct
import sys
inp, out = sys.stdin.buffer, sys.stdout.buffer
terminal = {"address":"t1", "created_ms":7, "state":"live", "argv":["sh"], "status":{"state":"working"}}
def exact(n):
    data = b""
    while len(data) < n:
        part = inp.read(n-len(data))
        if not part: raise EOFError
        data += part
    return data
def frame(channel, kind, payload):
    out.write(struct.pack("<IBI", channel, kind, len(payload))+payload); out.flush()
json.loads(inp.readline())
out.write(b'{"jsonrpc":"2.0","id":0,"result":{"framed":true,"core":{"id":"kill-fixture"},"version":"fixture"}}\n'); out.flush()
try:
    while True:
        channel, kind, size = struct.unpack("<IBI", exact(9)); payload=exact(size)
        if channel != 0: continue
        request=json.loads(payload); method=request["method"]
        if method == "events.subscribe": result={"channel":1,"terminals":[terminal]}
        elif method == "terminal.attach": result={"channel":2,"terminal":terminal}
        elif method == "terminal.kill":
            terminal = dict(terminal,state="finished",reason="killed",exit_code=None,signal="SIGKILL")
            result=terminal
        else: raise AssertionError(method)
        frame(0,0,json.dumps({"jsonrpc":"2.0","id":request["id"],"result":result}).encode())
except EOFError:
    pass
