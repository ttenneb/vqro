# Pi provider: private real-core/backend conformance (2026-11-17)

## Result

The backend-owned Pi bridge works against real prim provider-wire and herdr-core 0.1.21. Both the source
build from main `a7713f2` and the installed prim binary passed in private test roots. Fake Pi exercises native
effects without a model; installed Pi **1.5.0** exercises metadata/state/restart/EOF only. No real credentials,
sessions, services or install were changed. No provider/model request was made.

## Reproduce

```sh
env -i HOME="$HOME" PATH=/usr/bin:/bin python3 tests/pi_provider_e2e.py --run \
  --prim /absolute/path/to/prim --hc /absolute/path/to/herdr-core
# Optional installed Pi 1.5.0, metadata only:
# add --real-pi-metadata
# Assert native-herdr declares stream-only input and rejects raw effects:
# add --probe-terminal-invoke
```

This is opt-in, not ordinary unittest discovery. The harness creates its own scratch root and re-execs under
`unshare -Urn`; inability to create the namespace is a failure, never a live-environment fallback. It asserts
network namespace separation and removes all inherited HERDR_* and credential/config environment. Core/backend
PATH is an empty private directory; binary and runtime argv are absolute. HOME, config, state, runtime,
HC_SESSION, HC_SOCKET, PRIM_STATE_DIR, PRIM_SOCKET, extensions, work and Pi agent/session roots are private.
Sockets stay under103bytes. A missing default hc address is never queried. Bundled extensions are disabled.

Each receipt records the executing core/backend image SHA256 through its owned `/proc/PID/exe`, exact public
CLI calls, native PID/birth identities and cleanup. It retains logs/scratch evidence, but stops only its own
processes/server/terminals. It never runs the installer or copies real credentials. Real Pi mode disables
extensions, skills, templates, themes, tools and sessions; the harness does not invoke any agent effects in
that mode. The program itself only inspects get_state at startup. No claim about provider-backed turns.

## Verified paths

| Path | Evidence/assertion |
|---|---|
| resource.spawn bridge:true | Core injects token/socket; Python module starts in a real backend PTY, registers one runtime provider and one agent. Fake child confirms no bootstrap token or HERDR_* env. |
| Agent/panel | Agent r2 hosted by r1; view/agent-r2 is declared once and live agent-r2 content is populated, bounded below32KiB. |
| Native effects | prompt/steer/follow_up/abort/clear_queue/answer all travel through `prim call resource.invoke`. Prompt is accepted/started, queue facts carry steering/follow-up, explicit clear returns removed work, abort settles aborted, UI answer is dispatched rather than accepted. |
| Rejection and uncertainty | CLI errors are nonzero. A separate public events.subscribe observer reads their exact rejected/indeterminate effect audits, never resending input to discover a result. |
| Restart | SIGKILL only the private core; HC terminal/bridge/Pi stay alive. Fresh core uses persisted token/identity state. Host ID/ref/incarnation, agent ID/provider instance, native PIDs and current run are unchanged. Owned view entry/version is reused, live content republished. |
| No replay | Fake Pi's native command journal is byte-for-byte unchanged through restart; each prompt and UI response occurs once. Abort does not invent additional clear_queue commands. |
| Native exit | Fake Pi exits23 before acknowledging a prompt: caller sees indeterminate, resource.gone arrives, bridge terminal exits23 with no signal. |
| Real Pi metadata | Idle/empty run facts, same native child through core restart, public resource.attach/stream.input EOT, gone and clean terminal exit0. stream.input correctly reports dispatched. |
| Teardown | Remove only this private session's terminals, stop its server, and assert no live PID/birth identity remains (including core stdio helpers, bridge and Pi). Private HC socket is removed. |

## Bridge bug found and fixed

A daemon lifetime thread read `sys.stdin.buffer.read(4096)`. When Pi exited first while terminal stdin remained
open, CPython finalization tried to close a BufferedReader whose lock that daemon still held. Real PTY E2E
showed signal6, not native exit23. The focused CLI regression independently reproduced:

```
Fatal Python error: _enter_buffered_busy: could not acquire lock for
<_io.BufferedReader name='<stdin>'> at interpreter shutdown, possibly due to daemon threads
```

`extensions/pi_bridge/__main__.py:watch_stdin` now uses `os.read(stream.fileno(),4096)` on the dedicated lifetime
handle. This preserves portable pipe/terminal EOF semantics without holding a Python buffered-I/O lock. The new
`test_cli_native_exit_with_open_stdin_is_clean` waits for process exit **without communicate()**, which would
close stdin and hide the bug. It is red before the fix (-6), green after (23). The existing EOF regression also
continues to exit0. No core source changes.

## Native input transport decision (resolved after initial report)

Herdr-core has no raw input RPC. The approved contract now declares terminal.input `{stream:true,effect:false}`:
generic `resource.invoke` rejects an unsupported raw input effect before dispatch with outcome rejected.
`--probe-terminal-invoke` now asserts that declaration and exact rejection, rather than expecting a raw effect.
No terminal.send/keys or hidden attach emulation is added. The established public attach/stream.input path is
the legitimate EOF route; its outcome remains dispatched. See `native-input-transports.md`.

## Test results and receipts

- 71 Python tests pass with warnings treated as errors, including the new native-exit CLI regression.
- 134 Rust tests pass, zero warnings;15 opt-in tests remain ignored in ordinary cargo test.
- Repeated fake and real metadata E2Es pass with clean teardown. Real Pi uses no agent effects or model calls.
- Final installed-binary fake E2E: `~/.local/share/herdr/scratch/ep-9bnij3qr/receipt.json`.
- Final installed-binary real Pi metadata E2E: `~/.local/share/herdr/scratch/ep-vzhpy4ay/receipt.json`.
- Source-build repeats: `ep-yhwb804h`, `ep-7qzv2cfk` (fake), `ep-24t9fzkd` (real metadata), under the same scratch parent.
- Historical pre-decision terminal.input probe: `ep-yegs6ffq/receipt.json`; exit1, cleanup PASS. Current probe passes with the explicit stream/effect declaration.
- Native PTY exit regression before fix: `ep-nvg2y8tk/receipt.json`; signal6 instead of exit23, cleanup PASS.

No UI-driving/layout/tmux/remote/Windows or provider-backed model-turn validation is claimed. The existing
unrelated-view collision behavior remains unchanged: never overwrite someone else's view. SIGKILL of the bridge
itself cannot publish gone; the core must honestly report its disconnected provider/hosting terminal.
