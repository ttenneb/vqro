# Local Pi runtime provider program

Run as a backend terminal's program, never as a core-managed extension or process host:

```sh
PYTHONPATH=/path/to/prim/extensions python3 -m pi_bridge -- \
  /absolute/path/to/node /absolute/path/to/pi/cli.js --mode rpc
```

The Pi argv after `--` is mandatory and used verbatim. There is **no installed pi1 default**, runtime/provider
selection, credential copying or automatic prompt. The installed extensions directory can replace the source
PYTHONPATH after `scripts/dev-install.sh`; that script now copies Python packages and replaces only the known
pi_bridge package directory to remove old installed modules. This task does not run the installer.

Spawn through the public primitive with opt-in `bridge: true`. The core supplies opaque `PRIM_SOCKET` and
single-use `PRIM_BRIDGE_TOKEN`; do not type/log/copy real bootstrap secrets manually. Runtime registration uses
role runtime and token. The bridge starts Pi over clean child pipes, inspects get_state without a model call,
registers, syncs one agent (`ref: agent`), finds its assigned rN in workspace.snapshot, and declares
`view/agent-rN` once. Live content uses the existing bare `view: agent-rN` API. No automatic layout change.

## Native effects and facts

Capabilities: agent.prompt, steer, follow_up, abort, clear_queue, answer and correlation. Effects carry native
Pi args/dispositions. Accepted is not settled; only native agent_settled marks settlement. Abort does not silently
clear queues. UI answer is dispatched, not acknowledged. Runtime deadline/exit uncertainty is a backend error,
which the core maps to indeterminate. No prompt/input/effect is automatically resent.

Facts put coarse state in status.state and run/queues/requests in data. Rich pending UI details (options and
confirmation body) remain available. Data is capped below 64 KiB: oversized fields are null with an explicit
incomplete marker, never falsely empty. Dialogs that cannot be represented are cancelled once, with no automatic
resend if that cancellation is uncertain. Unknown UI kinds are cancelled by the runtime client.

The view is coalesced at 5 Hz by default (minimum interval 0.1 seconds), <=32 KiB, with a 16 KiB transcript tail
and 128 rows. Final messages replace provisional deltas. Native journal gaps discard partial reconstruction and
show an output-loss warning; no durable transcript/history claim. Terminal control bytes are escaped, including
in the fallback. The fallback has a one-frame mailbox and never blocks RPC on a terminal that stopped reading.
It is a read-only status/output tail: input is reserved for lifetime EOF; it does not secretly forward typed text.
Ctrl-C or stdin EOF exits the bridge and closes Pi. Runtime argv/cwd are configurable (`--cwd`, `--interval`,
`--transcript-bytes`); no model/provider call is made until an explicit agent effect asks for one.

An existing unrelated view is never overwritten or published to. The agent remains controllable via its resource
and fallback and reports a view_error. An owned marker permits reuse after reconnect without transaction churn.

## Core restart and authentication

Core loss **does not stop Pi**. The bridge retries the same endpoint with bounded exponential backoff
(0.25 seconds to 5 seconds), keeping the same provider name/instance/ref and the same child process.
Registration result.resume_token replaces the credential used in the next registration's existing params.token.
The core owns durable token storage, host binding and current/previous-token recovery after a lost reply; the
bridge only remembers credentials it actually observed. After registration it full-syncs current facts and
reuses its agent view. No stream/effect/prompt replay. Native pending work continues, even if the caller's old
core request became indeterminate.

SDK reconnection remains explicit: each attempt creates a fresh Provider, never reuses an old generation.
A missing required resume token leaves Pi/fallback running but cannot justify reusing a known-consumed bootstrap
credential. Failed auth/endpoint attempts are bounded; user/runtime exit still interrupts them. Resume tokens
and HERDR_* / extension-lifetime markers are not passed to Pi. A runtime bridge owns stdin shutdown itself,
rather than letting the generic extension watcher's os._exit bypass child/resource cleanup.

## Verification and limits

`tests/test_pi_provider.py` uses the SDK socket-pair fake core plus fake Pi, including token registration,
acceptance vs settled, steering, abort/clear behavior, dispatched answers, bounded/coalesced/sanitized views,
child crash/gone, EOF cleanup, stalled fallback, auth rejection, resumed core with unchanged Pi PID, current facts,
owned-view reuse, lost registration reply and bounded reconnect backoff. A CLI subprocess check uses a private
in-test Unix-socket fake core and fake Pi; it confirms module argv and graceful managed-stdin EOF cleanup.

Run with `env -i HOME="$HOME" PATH=/usr/bin:/bin python3 -W error -m unittest discover -s tests -p 'test_*.py' -v`.
Those unit tests use no-model fixtures. `tests/pi_provider_e2e.py --run --prim /absolute/prim --hc /absolute/herdr-core`
also exercises a real private core/backend and fake Pi through public CLI effects, durable token/core restart,
unchanged native processes, live view and native exit. Add `--real-pi-metadata` for installed Pi 1.5.0 state/restart/EOF
only, never agent effects or model calls. Runs require a fresh user/network namespace and retain private receipts.
The CLI lifetime watcher uses raw descriptor reads: a blocked daemon BufferedReader otherwise aborts interpreter
shutdown when Pi exits first. The native-exit-with-open-stdin regression asserts the real child exit status.
Local bridges only; no relay. Windows and provider-backed Pi turns remain unverified. SIGKILL cannot send resource.gone;
the core must report the disconnected provider/hosting terminal honestly in that case.
