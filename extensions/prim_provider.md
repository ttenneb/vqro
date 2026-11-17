# Python provider helper (stdlib, Python 3.11+)

`prim_provider.py` implements **provider-v0.md section 9** (60716f3); `prim_provider_stream.py` owns bounded
stream queues/credit. This is the provider side only. No core registration implementation, default extension
entry, real backend integration or reconnect policy is installed by importing it. `prim_ext` is imported solely
to reuse the managed extension stdin lifetime watcher; it does not open a socket on import.

## API

```python
from prim_provider import Provider, ProviderError, Limits

provider = Provider("backend@host", "backend-incarnation", capabilities,
                    software={"name": "backend", "version": "1"})

@provider.effect("terminal.spawn")
async def spawn(effect, args):
    # effect.id/op/ref/incarnation/deadline; effect.remaining and effect.expired
    # Call the native backend once. Return only its observed acknowledgment.
    return {"outcome": "observed", "data": {"facts": native_facts}}

async def attach(stream):
    stream.on_input = write_native_input
    stream.on_resize = resize_native
    stream.on_close = detach_native
    stream.on_credit = return_native_output_credit
    stream.on_end = forget_local_stream  # synchronous bookkeeping, no I/O
    # Start a producer that awaits stream.send(bytes), or enqueue bounded initial
    # output with stream.offer(bytes). Nothing is sent before the open response.
    return "history"  # or "repaint" / "none", exactly what backend supplies

provider.open_stream = attach
try:
    await provider.connect()  # PRIM_SOCKET; or explicit path / connected sock=
    await provider.full_sync(native_resources)
    await provider.wait_closed()
finally:
    await provider.close()
```

This is a shape example, not an executable backend. `tests/fixtures/fake_provider.py` is a complete example.
Runtime bridges use `role="runtime", token=...` and must themselves run inside backend-owned terminals; the
helper does not launch runtimes or turn prim into a workspace-process host.

- `connect(path=None, sock=None)`: hello + exactly one register, stores negotiated generation/window/record cap.
  A connected socket supports in-test socket pairs and other transports. Default path is PRIM_SOCKET.
- `sync(resources, final=True)`: acknowledged chunk; `full_sync(iterable)` chunks a whole listing to the negotiated
  record bound. First sync must be acknowledged before SDK upserts; further upserts during chunked sync work.
- `upsert(facts)` / `gone(ref, incarnation="", why={"reason":...})`: notifications, await socket write, **not core
  application**. Required facts/types and the opaque data size are validated. No hidden resource persistence.
- `effect(op)` registers a native handler. Declared capabilities and handlers must both include the operation.
  Callback receives an Effect plus args. Valid results are accepted/observed/rejected; rejected needs
  `{error:{kind,message}}`. Raise ProviderError for JSON-RPC errors. Unexpected handler failures are backend errors,
  not invented rejections. The core maps provider errors/deadlines to indeterminate.
- The forwarded remaining `deadline_ms` becomes a monotonic context deadline. **Expiry does not cancel or retry
  a possibly dispatched effect.** The handler replies exactly once when it knows, even late. The core owns the
  caller deadline and ignores a late original response. No invented late-outcome notification or replay.
- Duplicate effect IDs cannot execute twice. Both effect and stream-ID ledgers are bounded; when full, new work
  gets overloaded rather than forgetting IDs. A new connection requires a fresh Provider, full sync, and an explicit
  caller decision; no stream/effect resume is attempted.
- Generation is automatically attached to outgoing request/notification params. Incoming mismatched requests get
  stale_generation; mismatched notifications close the connection. Replies use normal result/error, no generation.

## Stream API and credit

- `stream.params`, `id`, `ref`, `incarnation`, `offset`, `credit`, `outstanding`, `closed` expose current mechanism.
- `await stream.send(data)` accepts at most 64 KiB bytes and waits until the chunk is written under credit. It
  returns false if closed before completion. `stream.offer(data)` is bounded nonblocking enqueue and returns a
  receipt Future. An input callback that echoes should **offer and return None**, not await output credit; input,
  resize and close are processed in order on their own worker, while credit must be able to unblock output.
- `on_input(bytes)`, `on_resize(cols,rows)`, `on_close()` may be async. Local capacity/input errors send
  stream.error with rejected. Unexpected native callback failures close the stream without fabricating no-effect.
- `on_credit(decoded_bytes)` returns native backend credit **only when core credit arrives**, never on SDK enqueue.
  Calls may coalesce. v0's core credit point is its bounded writer accepting output for the viewer, **not proof the
  viewer parsed it**. Excess credit is ignored and cannot inflate the negotiated window.
- `await stream.close(reason_object)` emits an ordered final stream.closed; `await stream.gap(kind)` emits final
  stream.gap at the current sent offset. Kinds: backend_dropped/paused/viewer_slow. No later data; recovery is a new
  stream ID/open, never replay. Initial history/repaint bytes are ordinary credited data from offset zero, labelled
  by the stream.open result. No standalone repaint message exists in v0.
- A synchronous `on_end()` can release local bookkeeping for every close/gap/disconnect. Backend connection/process
  cleanup remains owned by the provider; this SDK does not kill native backend resources on disconnect.

Defaults: 1 MiB wire record; 64 KiB decoded data chunk; 512 streams; 256 KiB stream window and each stream input/output
queue; 128 queued items per stream (plus a reserved close marker); 64 pending calls/handlers; 2 MiB outstanding
serialized writes; 65,536 identities per ledger; 30-second transport/call deadline. Counts/byte limits are configurable
via Limits. Python object/asyncio/kernel buffers add bounded overhead beyond encoded-byte counters.

The socket writer is serialized and byte-accounted; per-stream wire locks preserve data-before-final-marker order.
Slow streams wait for their own credit without blocking others. Queue exhaustion is explicit overloaded/error/gap,
not silent input loss. A write timeout/cancellation after transmission starts closes the connection rather than
letting abandoned transport buffers grow. Close cancels local pending work; it never resends anything.

## Fake provider / tests

Launch (once core-side wire lands), as an ordinary managed extension:

```
/usr/bin/python3 <checkout>/tests/fixtures/fake_provider.py --name fake --socket <private-core-socket>
```

`--socket` defaults to PRIM_SOCKET. `--instance` optionally supplies a stable instance for a test; default is one UUID
per process. Several hosts/providers use distinct names/instances. No actual terminal or command is launched.

Resource spawn argv supports the existing fake load flags:
`--status-every-ms N`, `--status-phase-ms P` (positive milliseconds), `--output-bytes-per-sec N` (10 ms ticks).
Other argv is recorded/ignored. Spawn returns f1/f2... facts; streams greet `fake fN\r\n`, echo exact input, ignore
resize, provide bounded credited floods; status toggles working/idle with n and Unix at_ms. Kill publishes finished
facts, closes streams, stops future load. No native backend persistence/lifetime guarantee is simulated.

```
env -i HOME="$HOME" PATH=/usr/bin:/bin python3 -W error -m unittest discover -s tests -p 'test_*.py' -v
```

Nineteen provider tests use an in-test fake core and socket pairs: no real core, hc, tmux, credentials, model or
provider calls. They cover registration/sync chunks, facts, D10/native errors, late single replies and duplicate
IDs, generation fencing, LF Unicode/CRLF, open/data ordering, credit units/over-return, slow-stream isolation,
ordered input/resize/close, input/output/record/call/ledger/stream bounds, final gaps and disconnect cleanup.
Real core/provider conformance, default scale and Windows remain the next layer's validation, not claimed here.
