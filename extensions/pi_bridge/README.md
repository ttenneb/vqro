# Pi RPC bridge launch and legacy Python client

The default trusted `python3 -I /installed/pi_bridge/launch.py` shim execs the terminal host's `vqro-bridge-pi`
Rust binary. It must be installed/on the host PATH; failure never silently launches another implementation.
The Rust adapter implements agent-v0 and bounded item streams ([declaration notes](../../docs/research/pi-adapter.md)).
An explicit `python3 -I /installed/pi_bridge/python_launch.py` keeps the legacy Python provider available.
Launch details: [PROGRAM.md](PROGRAM.md).

The stdlib-only Python 3.11+ **Pi 1.5 RPC** client API below remains independent of vqro. Pi's JSONL protocol is
not JSON-RPC. The remaining provider/client notes describe the explicit legacy implementation.

The bridge runs **inside a backend-owned terminal**, instantiating the client over fresh child pipes.
Do not instantiate it in vqro core or its extension supervisor. No PTY is used for Pi RPC. The program
uses `vqro_provider.py` for its separate vqro connection; it is not a generic agent process host.

## Interface

Import with `extensions/` on `sys.path`:

```python
from pi_bridge import PiRpcClient, Limits

async def inspect(argv, private_cwd, explicit_child_env):
    async with await PiRpcClient.start(
        argv, cwd=private_cwd, env=explicit_child_env, limits=Limits()
    ) as pi:
        result = await pi.request("get_state", timeout=2)
        facts = pi.snapshot()
        batch = pi.events(after=0)
        return result, facts, batch
```

- `start(argv, *, cwd, env, limits=..., supported_dialogs=...)`: launch one owned child. An explicit
  environment is mandatory; it does not implicitly inherit the caller's credentials or Herdr variables.
  Spawn errors raise normally: no effect has been sent. Runtime selection/configuration belong to the caller.
- `await invoke(op, args=None, timeout=None)`: native `prompt`, `steer`, `follow_up`, `abort`, `clear_queue`,
  `answer`. Arguments retain Pi's native field names, e.g. `streamingBehavior: "followUp"`.
  `prompt` goes through a start gate: one at a time until Pi shows the previous one started (PROGRAM.md).
- `await request(command, args=None, timeout=None)`: metadata/native-command escape hatch. IDs and `type`
  are assigned by the client; callers cannot override them through args.
- `snapshot()`: detached facts `{available, stale, status, run, queues, requests, start_pending, session?, exit_code?}`.
  `status` is `unknown | idle | working | blocked | done | error`. Failed assistant turns remain `error` after settlement; observed retry activity stays `working`. Optional `failure.message` and `retry` facts retain bounded error text and native attempt/max/delay/success observations. A successful retry or new turn clears the current failure, not transcript history. Before an observation, `run` values can be `None`;
  absence of evidence is not idle/settled. Queues use `steering` and `follow_up`; requests contain
  `{id, kind, prompt, data}` with the original UI request in `data`.
- `events(after=cursor)`: bounded batch `{cursor, gap, events}`. Entries have a local monotonic `seq`.
  A gap means refresh facts and abandon any partial transcript reconstruction; this is not a durable log
  or backend replay. Events can include raw Pi records, `late_response`, `dialog_timeout`,
  `answer_dispatched`, `disconnected`, and `child_exit`.
- `await wait_for_facts(predicate, timeout=...)`: fixture/consumer observation barrier, not a provider task wait.
- `await wait_changed(after=cursor)`: edge-triggered journal revision notification, with no idle polling. A cursor
  no longer matching the current revision returns immediately; read `events(after=cursor)`/facts to catch up.
- `diagnostics()`: bounded stderr tail/total, event bytes, pending requests, stdin-buffer bytes and failure.
- `await close(grace=1)`: close stdin for orderly EOF shutdown; terminate/kill only this owned child if it
  does not exit during the grace periods. Context manager exit does this too. No process tree/lifetime
  guarantee is invented for arbitrary programs a runtime launches. `wait_closed()` observes child exit/drains.

## Effects are acknowledgments, not task completion

Native successful command response:

```json
{"outcome":"accepted","id":"pi-1","data":{"disposition":"queued"}}
```

- Preserve Pi's `started | queued | handled` disposition. A handled prompt need not start a run.
- Native refusal returns `rejected` (including its error/data); local validation/capacity rejection is marked
  not sent where applicable. Once transmission may have occurred, timeout, cancellation, pipe loss or exit
  produces `indeterminate`. A late native response remains visible in the event journal. **Nothing resends.**
- `answer` takes the original dialog ID and either `value`, `confirmed`, or `cancelled: true`. Pi emits **no
  normal response** to `extension_ui_response`, so successful writing is `dispatched`, never fictitious
  `accepted`. Concurrent duplicate answers are rejected. A dispatched answer is not proof it was consumed.
- `agent_start` marks an active run. `agent_end` does **not** settle it. Only `agent_settled` reports settled
  and its `aborted` value. `get_state` may observe activity/idle but does not manufacture a settled event.
- `queue_update` replaces the complete queue facts. Steer is after current tool calls; follow-up is when Pi
  otherwise finishes. `abort` **does not clear queues**. To cancel queued continuation, explicitly call
  `clear_queue` before `abort`; the client does not silently choose that policy for the caller.
- Dialogs block facts while pending. Supported kinds are select/confirm/input/editor; a caller without UI can
  pass `supported_dialogs=()` to cancel them all. Unknown/unsupported kinds are explicitly cancelled.
  Native declared dialog timeouts are mirrored locally and labelled `dialog_timeout`, not answer acceptance.
  Fire-and-forget UI notices remain events; custom TUI components are not emulated.
- Disconnect makes facts unavailable/stale/unknown and resolves in-flight requests indeterminate. Previously
  observed run/queue data stays marked stale; no automatic restart, reconnect, delivery or recovery.

## Bounds and framing

Defaults (`Limits`): 256 KiB per outgoing command record, 4 MiB per incoming record (real Pi 1.5.2's
`get_available_models` is ~300 KB with a real configuration; `tests/fixtures/pi_metadata_1.5.2.json` records it), 32 pending commands, 512 KiB retained serialized
events **and at most 512 event objects**, 64 KiB stderr tail, 32 outstanding dialogs, 5-second effect deadline. Python object overhead and
asyncio/kernel pipe buffers are additional bounded overhead, not included in the retained-byte counters.

Owned exit is observed through the native asyncio subprocess protocol callback, independently of pipe EOF; no
PID polling is needed. After exit, pending stdout/stderr readers drain only for the configured `Limits.drain`,
then inherited pipes are abandoned without signalling unowned descendants. Stdout and stderr drain independently. JSON records split **only on LF**, accept CRLF, and permit literal
U+2028/U+2029 and UTF-8 split across reads. Malformed/oversized records fail the connection rather than
retain unbounded data. Stdin records serialize under a lock and honor `drain()` backpressure. A drain
timeout fails the write side closed: repeated deadlines cannot accumulate an unbounded transport buffer.
Journal overflow evicts old events with an explicit cursor gap, but current facts continue to update.

## Verification

```sh
# Deterministic fake runtime, no model/provider, no server needed:
env -i HOME="$HOME" PATH=/usr/bin:/bin python3 -W error -m unittest discover -s tests -p 'test_*.py' -v

# Explicit opt-in: installed absolute Node/Pi 1.5.0, metadata commands only:
env -i HOME="$HOME" PATH=/usr/bin:/bin python3 tests/pi_bridge_smoke.py --run
```

The fixture in `tests/fixtures/fake_pi.py` uses documented Pi records and extra **test-only** barrier commands.
Tests cover acceptance vs settlement, tool-boundary steering, queued continuation on abort, clear-before-abort,
correlation/out-of-order replies, native rejection, dialogs/unsupported cancellation/timeouts, Unicode framing,
bounds/backpressure, late replies, child crashes and truncated/invalid records. This validates our handling of
scripted native semantics, not provider-backed run behavior in the installed Pi.

The opt-in smoke reexecutes under `unshare -Urn` with an environment allowlist (env-i equivalent), fresh private
HOME/cwd/config/state/session directories, disabled extensions/tools/resources and the absolute installed CLI.
It refuses to fall back if network namespace isolation is unavailable, sends only get_state/get_messages/
get_commands/get_session_stats, closes stdin and records a scratch receipt. No credential copy/read from real
configuration, provider prompt, model call, networking or real vqro/Herdr connection. Created scratch files,
including Pi's empty auth store, are retained for inspection. No provider-wire compatibility is claimed yet.
