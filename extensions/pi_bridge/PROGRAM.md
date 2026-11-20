# Backend-owned Pi bridge launch

The default is the Rust `vqro-bridge-pi -- <literal native argv...>` agent-v0 adapter. The trusted
`python3 -I /installed/pi_bridge/launch.py` shim execs that host-PATH binary without a shell or fallback. The
installer includes it beside vqro; this change does not run an install. Rust explicitly selects RPC mode and
controlled no-extension/skill/template discovery for its literal route. No initial prompt is sent.
See [Rust declaration/evidence notes](../../docs/research/pi-adapter.md).

To choose the legacy Python provider explicitly, set runtime bridge_argv to
`["python3", "-I", "/installed/pi_bridge/python_launch.py"]`. Legacy regression tests name this profile explicitly;
the default-Rust E2E exercises the actual default shim and the v0 API separately.

## Explicit legacy Python provider

Run as a backend terminal's program, never as a core-managed extension or process host:

```sh
python3 -I /path/to/vqro/extensions/pi_bridge/python_launch.py -- \
  /absolute/path/to/node /absolute/path/to/pi/cli.js --mode rpc
```

The Pi argv after `--` is mandatory and used verbatim. There is **no installed pi1 default**, runtime/provider
selection, credential copying or automatic prompt. `-I` excludes the project cwd, `PYTHONPATH` and user site
packages before bridge/stdlib imports; the launcher adds only its own trusted installation. Do not use plain
`python -m pi_bridge` in an agent project. The default `pi` extension uses the Rust shim; this legacy launcher is
an explicit alternative, also without exporting `PYTHONPATH`. Existing `bridge_argv`/`bridge_env` stay literal and configurable on the terminal host (for example,
`["py", "-3", "-I", "C:\\installed\\vqro\\extensions\\pi_bridge\\launch.py"]`); no shell or core-OS inference.
A custom launcher is responsible for its own import isolation. `scripts/dev-install.sh` copies the package,
including the launcher; this task does not run the installer.

The `pi.new` launcher reads only its settings entry and scoped source/host resources, not full workspace snapshots
while waiting for registration. It retries transient status reads only; an already-observed host/agent remains in
the command result even if registration lookup or UI presentation is unavailable. The terminal fallback warns not
to blindly retry; the spawn effect is sent once.

Spawn through the public primitive with opt-in `bridge: true`. The core supplies opaque `VQRO_SOCKET` and
single-use `VQRO_BRIDGE_TOKEN`; do not type/log/copy real bootstrap secrets manually. Runtime registration uses
role runtime and token. The bridge starts Pi over clean child pipes, inspects get_state without a model call,
registers, syncs one agent (`ref: agent`), finds its assigned rN through scoped resource.list, and declares
`view/agent-rN` once. Live content uses the existing bare `view: agent-rN` API. No automatic layout change.

## Native effects and facts

Capabilities: agent.prompt, steer, follow_up, abort, clear_queue, answer, correlation, command and configure
(`control.py`: agent v0 6.3 `data.agent.config`/`.commands`, read back with `get_state`; provider-v0 section 6).
A plain send starting with an advertised `/command` is rejected: Pi would expand it. Effects carry native
Pi args/dispositions. Accepted is not settled; only native agent_settled marks settlement. Abort does not silently
clear queues. UI answer is dispatched, not acknowledged. Runtime deadline/exit uncertainty is a backend error,
which the core maps to indeterminate. No prompt/input/effect is automatically resent.

Start gate (stopgap for Pi0.87, P4 report): Pi has no atomic start guard; two prompts in overlapping start
preflight can both ACK success while only one reaches the model, and the loser emits a spurious agent_settled.
The client therefore sends one `prompt` at a time: the next is sent only after the previous one shows agent_start
(or answered handled/queued/rejected, or `Limits.start_confirm`, 30 s, passes; journaled `start_unconfirmed`).
Waiting prompts keep their order and send nothing while waiting: a deadline or an abort before dispatch rejects them
as not sent. While a start is unresolved, status is `working` and `start_pending` is true even if Pi reports idle or
settled. Abort reports `start_unresolved: true` when a sent prompt's preflight had not resolved: Pi0.87 still starts
it afterwards.

Facts put coarse state in status.state and run/queues/requests in data. Assistant `message_end` with
`stopReason: error` records bounded `failure.message`, sets error status even after settlement, and adds an
error-styled transcript row even when message content is empty. Native `auto_retry_start/end` records optional
`retry` attempt/max/delay/success facts and readable transcript entries; the bridge never performs those retries.
An active native retry remains working, final failure remains error, and successful retry/new turn clears the
current failure while preserving transcript history. These Pi event mappings stay in the bridge; neutral agent
panels render the optional failure/retry facts without runtime-specific imports. Rich pending UI details (options and
confirmation body) remain available. Data is capped below 64 KiB: oversized fields are null with an explicit
incomplete marker, never falsely empty. Dialogs that cannot be represented are cancelled once, with no automatic
resend if that cancellation is uncertain. Unknown UI kinds are cancelled by the runtime client.

Two independently deduplicated views separate status/queues/dialogs (`agent-status-rN`) from the stable
transcript (`agent-rN`). Resource data advertises their ordered names in `views`; neutral agents references them
read-only rather than copying their rows. Older runtimes without `data.views` retain the canonical `agent-rN`
fallback convention. No relative times are inserted into transcript rows. Dirty runtime output is coalesced at 5 Hz
by default (minimum interval 0.1 seconds); idle has no sampling timer. Runtime journal revisions wake publication,
while user-control and connection diagnostics remain immediate. Owned-child exit uses asyncio's native process-exit
callback independently of inherited-pipe EOF, not PID polling. The views are <=32 KiB, with a 16 KiB transcript tail
and 128 rows. The SDK journal retains at most 512 events / 512 KiB (whichever fills first), bounding tiny-record
Python overhead as well as wire bytes. The bridge's provider keeps at most 2048 active/recent effect identities;
active identities are never evicted, and a full active ledger rejects before native execution. Custom injected
provider limits remain explicit caller choices. Final messages replace provisional deltas. Native journal gaps discard partial reconstruction and
show an output-loss warning; no durable transcript/history claim. Terminal control bytes are escaped, including
in the fallback. The fallback blocks on its one-frame mailbox until changed/closed, with no idle polling. A terminal that stops
reading never blocks RPC.
It is a read-only status/output tail: input is reserved for lifetime EOF; it does not secretly forward typed text.
Ctrl-C or stdin EOF exits the bridge and closes Pi. Runtime argv/cwd are configurable (`--cwd`, `--interval`,
`--transcript-bytes`); no model/provider call is made until an explicit agent effect asks for one.

An existing unrelated view is never overwritten or published to. The agent remains controllable via its resource
and fallback and reports a view_error. An owned marker permits reuse after reconnect without transaction churn.

## Core restart and authentication

Core loss **does not stop Pi**. The bridge retries the same endpoint with capped exponential equal-jitter
windows (initial 0.125–0.25 seconds, maximum 15–30 seconds), keeping the same provider name/instance/ref and
child process. A connection must stay healthy at least 30 seconds before it resets the backoff; briefly flapping
successful registrations cannot restart a rapid retry loop.
Registration result.resume_token replaces the credential used in the next registration's existing params.token.
The core owns durable token storage, host binding and current/previous-token recovery after a lost reply; the
bridge only remembers credentials it actually observed. After registration it full-syncs current facts, reads
only its exact provider/ref via `resource.list` and its one view via `workspace.get`, then reuses its owned view.
It never needs `workspace.snapshot` over the 1 MiB provider wire. An oversized/foreign/unreadable view is
reported as `view_error` while resource controls and the backend terminal remain available, not used as a reason
to repeat registration/sync. A declaration CAS conflict is resolved by reading the one entry again, never blindly
overwriting it. No stream/effect/prompt replay. Native pending work continues, even if the caller's old
core request became indeterminate.

SDK reconnection remains explicit: each attempt creates a fresh Provider, never reuses an old generation.
An explicit registration refusal (including overload/auth rejection) or a missing required resume token reports
the error and exits through owned-Pi cleanup, rather than keeping an unreachable runtime alive. The sole
transient refusal exception is structured `data.reason: safe_mode`: it keeps the owned Pi while backing off,
not treating every forbidden error or matching message text as transient. Transport gaps and that explicit
safe-mode state retry the last observed credential; user/runtime exit interrupts them. Resume tokens
are not passed to Pi. Pi/tools receive no `HERDR_*` or `VQRO_*` internals except the opaque `VQRO_SOCKET` and
`VQRO_SOCKET_SOURCE=terminal`, preserving scratch-state endpoint selection. The bridge installation is removed
from legacy/custom `PYTHONPATH`; unrelated user entries are preserved. Default launch injects no Python path at all.
This is environment hygiene, not a same-user security sandbox: bridge-process memory/environ remains inside that
trust boundary. A runtime bridge owns stdin shutdown itself,
rather than letting the generic extension watcher's os._exit bypass child/resource cleanup.

## Verification and limits

`tests/test_pi_provider.py` uses the SDK socket-pair fake core plus fake Pi, including token registration,
acceptance vs settled, steering, abort/clear behavior, dispatched answers, bounded/coalesced/sanitized views,
child crash/gone, EOF cleanup, stalled fallback, auth rejection, resumed core with unchanged Pi PID, current facts,
owned-view reuse, lost registration reply, >1 MiB workspace startup, oversized foreign views, and capped jittered
reconnect/flapping-link backoff. A CLI subprocess check uses a private
in-test Unix-socket fake core and fake Pi; it confirms isolated launcher argv and graceful managed-stdin EOF cleanup.
Project-module shadow, actual child environment capture, no-idle-rebuild, native exit-notification and blocking
fallback regressions are included. CPU/memory measurement evidence: `docs/research/bridge-p2.md`.

Run with `env -i HOME="$HOME" PATH=/usr/bin:/bin python3 -W error -m unittest discover -s tests -p 'test_*.py' -v`.
Those unit tests use no-model fixtures. `tests/pi_provider_e2e.py --run --vqro /absolute/vqro --hc /absolute/herdr-core`
also exercises a real private core/backend and fake Pi through public CLI effects, durable token/core restart,
unchanged native processes, live view and native exit. Add `--real-pi-metadata` for installed Pi 1.5.0 state/restart/EOF
only, never agent effects or model calls. Runs require a fresh user/network namespace and retain private receipts.
The CLI lifetime watcher uses raw descriptor reads: a blocked daemon BufferedReader otherwise aborts interpreter
shutdown when Pi exits first. The native-exit-with-open-stdin regression asserts the real child exit status.
Local bridges only; no relay. Windows and provider-backed Pi turns remain unverified. SIGKILL cannot send resource.gone;
the core must report the disconnected provider/hosting terminal honestly in that case.
