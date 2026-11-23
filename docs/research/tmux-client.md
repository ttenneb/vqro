# Standalone tmux control client (2026-11-17)

Implemented in `src/tmux/`, exported by `prim::tmux`. This is a backend library, **not a provider**:
no core, server, RPC, adapter registration, layout policy, bridge or extension wiring changed. A module
rather than another crate reuses the existing Tokio/getrandom dependencies and keeps the next provider
integration a small caller of a backend-specific API. No new dependencies.

## API and ownership

`Client::connect(Config::new(absolute_private_socket, session))` attaches a `tmux -N -S … -f /dev/null -C`
client to an **existing** session. It never starts a server and never chooses the default socket. The
socket must be under 103 bytes. A subprocess environment allowlist drops Herdr/TMUX/credential context.
The caller creates/owns the private server; closing/dropping this library's handles terminates only its
control client. There is no automatic reconnect or effect resend.

- `spawn(argv, cwd)` creates a native one-pane window in that session; `input(pane, bytes)` uses
  `send-keys -H` (including NUL/backslash/non-UTF8); `kill` kills the native pane and closes its watchers.
  Spawn uses tmux's native command semantics: multiple arguments execute directly; a single argument
  is tmux's shell-command form. Pass an explicit shell plus arguments when requesting shell behaviour.
- `resize_pane` is native layout resizing, constrained by the containing window. `resize_window`
  invokes control-client `refresh-client -C @N:COLSxROWS`; arbitration with other clients is tmux's.
- `watch(PaneId)` subscribes from now, **not from spawn**; call `repaint` for an initial visible screen.
  Watchers receive `Data {offset, bytes}`, `Osc7501`, `Gap`, labelled `Repaint`, `Continued`, `Closed`.
  Output bytes are not stripped or rewritten by OSC observation. Status is lossy, observed, not stored.
- `notifications()` carries typed layout/window/pane/format/pause/continue/exit records and opaque
  unknown notifications. A client-generated `Disconnected` also covers EOF without `%exit`.
  Notification receiver lag requires listing/querying current facts; tmux has no cursored replay.
- `subscribe_format(name, Some((target, format)))` uses native `refresh-client -B`; `None` removes it.
- `command(&[args…])` quotes one native command for tmux, not a shell. It returns that command's first
  reply block. Random begin/end barriers absorb additional synchronous hook/conditional blocks and
  prevent a cancelled caller from shifting the next reply. This is not completion tracking for
  detached/asynchronous tmux jobs. Native rejection is distinct from timeout/disconnect after a write
  (`Indeterminate`); neither is replayed. Newline/NUL command arguments fail locally.
- **Timeouts (2026-11-17).** A *read-only* command (`list-*`, `display-message`, `capture-pane`, `show-*`,
  `has-session`) that gets no reply within `command_timeout` fails with `TimedOut`: no effect, and the control
  client stays attached. The request stays at the head and absorbs its late reply blocks up to its end barrier, so
  the next command is never paired with them. Drain grace is `max(10 * command_timeout, 5 seconds)`: short caller
  deadlines must not misclassify healthy CPU-delayed FIFO backlog as desync. The existing five-second default keeps
  its fifty-second drain; a30ms caller still times out at30ms, with a bounded five-second absorption window.
  A peer that exceeds that drain window is treated as desynchronised and dropped. A *mutating* command that times out is `Indeterminate` and drops the
  attachment as before. Nothing is resent. Tests: `tests/tmux_timeout.rs` (deterministic fake control client,
  `tests/fixtures/fake_tmux_cc.py`) and `tests/tmux.rs` (real 3.7b flood with a 30 ms timeout).
  Linux CPU-pressure repro: build `cargo test --test tmux --no-run`, then run
  `env -i HOME=$HOME PATH=$PATH python3 tests/tmux_flood_load.py <printed-test-executable> <new-private-evidence-dir> --runs 20`.
  It constrains only the reader after native clients attach, with four owned busy workers on that CPU; native
  producers retain their normal affinity. Pinning the whole process tree instead can hide the backlog by throttling
  the producer. Failure diagnostics retain the native/actor disconnect reason. Caller and mutation deadlines stay
  unchanged; a silent fake peer still must disconnect within the bounded drain window, and no command is resent.

One attachment sees panes **linked into its session**, not all server sessions. The next provider must
choose its attachment topology and list/query facts after structural notifications; tmux does not
promise a pane-exited notification or an event history. External pane/window closure does not by itself
supply a complete pane inventory: reconcile facts and `unwatch` removed panes. Pane respawn/process
lineage and placement policy are not inferred by this library.

## Identity

`instance()` includes native server PID, `start_time`, and a 128-bit random **server-only** user option,
`@prim_control_instance`. `set-option -s -o` installs it atomically without replacing an existing value;
concurrent/reconnected clients share it. A replacement server gets a new nonce even if native `%0`,
PID and coarse start-time values collide. This is ephemeral backend metadata, not a persistence store.
Do not manually reuse this reserved option in a server configuration.

Paused capture uses an independent short-lived `tmux -N -S` CLI. Its native `if-shell -F` identity
check and capture execute on the same server queue, so a recreated socket cannot silently repaint a
reused pane ID from another server. A changed nonce is rejected (integration-tested). No generic core
identity or provider protocol is implemented here.

## Gaps, pause-after, repaint and the 3.7b quirk

The connection enables `pause-after` (default one second). Native `%pause` becomes `Gap(Paused)` and
resets partial OSC state. A local slow subscriber gets `Gap(SubscriberLagged)` and its stale queued
tail is discarded; other subscribers keep consuming. Offsets count bytes actually observed by this
attachment, not unknown bytes discarded by tmux. Missing byte counts are never invented.

For subscriber lag while the native feed is running, `repaint(pane)` inserts a capture at its actual
reply boundary, before later raw data. For a **native paused feed**, `resume(pane)`:

1. Emits `Gap(RecoveryRequested)` and fences old raw output/OSC for that pane.
2. Captures the current visible screen through the independently scoped CLI; a paused control
   attachment can delay its own command replies, so recovery must not depend on such a reply first.
3. Publishes labelled `Repaint`, then requests native `continue` and waits until it is observed.
4. Only then admits fresh raw output; callers can send new input after the method returns.

Late pause notices are still exported as notifications, but cannot emit a second gap that invalidates
an already published recovery repaint. Recovery failure/cancellation leaves the pane fenced until
an explicit subsequent recovery/continue or connection close. Do not call `resume` for an unpaused
pane: absence of a continuation is reported, never fabricated. Successful recovery cannot restore
missing OSC status or terminal modes/cursor state from capture. `Repaint.screen` is visible ANSI-styled
rows joined with CRLF, not a VT save-state, raw output or scrollback replay. The provider/viewer must
reset its presentation and treat it as a screen snapshot.

**Verified tmux 3.7b quirk:** explicit `refresh-client -A %N:pause/continue` prints `%pause`/`%continue`
*inside that command's reply block*, despite the installed manual's general statement that
notifications never occur inside blocks. The parser keeps block bodies opaque. Only the client,
knowing the exact issued `-A` action and pane, recognizes those exact matching reply lines. Arbitrary
capture/display output that looks like a notification is never reclassified. Pure and real tests
cover opaque bodies, synchronous extra blocks, native automatic pause, and manual continuation.

## Bounds

- Incoming line 1 MiB; command reply 4 MiB or 65,536 lines; truncated/mismatched records close the client.
- 16 parsed records and 32 commands in bounded transport queues; command text at most 64 KiB.
- Input at most 16 KiB per call; callers explicitly chunk larger requests (no hidden input replay).
- At most 1,024 watched panes; per-pane broadcast ring defaults to 32 slots (Tokio rounds capacity
  upward), data chunks at most 16 KiB, OSC assembly at most 64 KiB. Capture is capped at 4 MiB.
- Capture stderr capped at 8 KiB; command/capture deadlines default to five seconds. Timed-out control
  clients and capture subprocesses are killed and reaped. These are local bounds, **not** the future
  provider's byte-credit protocol or a global scale-budget claim.

## Reproduce

```sh
W=<worktree>
T=<private-notes>
env -i HOME=$HOME PATH=$PATH CARGO_TARGET_DIR=$T cargo test --manifest-path "$W/Cargo.toml"
env -i HOME=$HOME PATH=$PATH CARGO_TARGET_DIR=$T cargo test --manifest-path "$W/Cargo.toml" --test tmux -- --ignored
```

The four opt-in tests require `/usr/bin/tmux` reporting **3.7b** and `/usr/bin/python3`. Each uses its own
short temporary socket and private HOME, no default tmux server/config, and kills only that server.
They verify binary input/output attribution and split OSCs in two panes; subscriptions, layout and
window changes; resize/kill; concurrent/reconnected identity and ID reuse after server restart;
caller cancellation, synchronous extra blocks, rejection, timeout and no resend; and an actual
3.2-second stalled-reader/80,000-line flood with native automatic pause, lost final OSC/raw tail,
visible capture repaint, continuation and fresh output. The stall is deliberate test stimulus;
producer completion is separately fenced by a file and bounded polling after reading resumes.

No live prim/Herdr sessions, installations or default tmux server were touched. Provider integration,
credit translation, multi-session topology, multiple-viewer resize policy, remote transport and
Windows runtime support remain outside this standalone backend slice.
