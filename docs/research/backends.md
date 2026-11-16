# Backend research

Status: in progress (2026-11-16). Sources: local clones in `~/Projects`
(`herdr-core`, `rmux`, `herdr-app`, `rodeo`) at HEAD, tmux 3.8 (Homebrew),
and the OSC 7501 spec. Rex: beta app with server 0.1.0-preview.19 (API revision 14), tested hands-on.

## Summary

| | herdr-core (`hc`) | RMUX 0.10 | tmux 3.8 | Rex (beta) |
|---|---|---|---|---|
| Licence | Apache-2.0 | Apache-2.0 / MIT | ISC | unknown, closed for now |
| Transport | JSON-RPC 2.0, line or framed, Unix socket / `hc connect` stdio | bincode typed IPC (Rust SDK); tmux control mode `-C` | control mode `-C` text protocol | CLI with `--json`, `rex api call`, `rex events --json`; WebSocket on a Unix socket underneath |
| Machine-readable schema | yes, OpenRPC, append-only with compat test | Rust types only | no | `rex api describe` per method; revision 14, changes fast |
| Start process | `terminal.run` (bg, name, template, `on` machine) | new-session / split / SDK `split` | new-session / split-window | `rex new`, `rex run`, `rex split` |
| Send input | `terminal.send` (paste+Enter, waits for human quiet), `terminal.keys` | send-keys, SDK `send_text` / `send_key` | send-keys | `rex send`, `rex send-key` |
| Read output | `terminal.read` (screen + N lines), attach stream | capture-pane, SDK `snapshot` / `output_stream` | capture-pane, `%output` | `rex capture` |
| Wait | `terminal.wait` (until not working / exit) | SDK `wait_for_text`, `wait_exit`, `send-keys --wait` | none built in | `run --wait`, `split --wait` |
| Events | `events.subscribe` with seq, heartbeat, `missed` resync | SDK `state_events`; control-mode `%` notifications | `%` notifications, format subscriptions (≤1/s) | `rex events --json` |
| Agent status | OSC 7501 only, stored as reported, plus `turn` counter | none native; 7501 bytes pass through control mode | none native; 7501 bytes pass through control mode | OSC 7501 (spec author), `program_status_changed` events |
| Parent/child | yes (`children_of` filter, stop with parent) | no | no | not seen |
| Metadata | labels (`key=value`), versioned JSON docs with compare-and-set | pane options (`@user` options) | user options `@foo` | labels on split |
| Layout | **none in core**; clients store it in doc `herdr-app` | full tmux model | full | blocks, layers, Lua layouts |
| Remote machines | built in (`machine.add`, `t7` addressed as `rohan/t7`) | no (web share only) | no | Tailscale built in |
| Persistence | survives client and core update; `keep` restarts as shell | daemon | server | server |

## Key findings

1. **herdr-core is a process runtime, not a multiplexer.** No panes, tabs or
   layout. The desktop app and the third-party TUI `rodeo` both store their
   spaces/tabs/split tree in one shared versioned document (`herdr-app`, schema
   in `rodeo/crates/doc/src/schema.rs`) and shared per-terminal state in labels
   (`herdr.seen`, `herdr.pinned`, see `herdr-app/docs/labels.md`). That is
   already the "small core, everything else in conventions" model prim wants.
2. **RMUX speaks tmux control mode** (`src/cli_args.rs`, `crates/rmux-server/src/control/`).
   One control-mode adapter may cover tmux and RMUX; the Rust SDK is an
   optional richer path (waits, state events). Its native IPC is bincode, so
   non-Rust clients should use control mode or the published Python/TS SDKs.
3. **OSC 7501 is the status standard to adopt.** Spec rev 0.4 (2026-10-10) at
   superlogical.com/rex/docs/build/program-status. States: idle, working, done,
   blocked (kind: permission, question, auth), error, plus clear. Records are
   hierarchical by `id` (`a/b/c`), `app` is inherited. Implemented by herdr-core,
   Rex, and tuios (`tuios.dev/progstatus`). The spec says nothing about
   multiplexer passthrough.
4. **Layout is the axis where backends disagree most.** herdr-core has none, Rex
   puts it in Lua, tmux/RMUX have a split tree in the server.

## Open questions (to answer by spike)

### Cross-cutting
- [ ] Does OSC 7501 survive tmux and RMUX? Is it visible in `%output`, or eaten
      by their parser? If eaten, prim cannot get status on those backends
      without a wrapper process (e.g. `prim exec` that tees the PTY).
- [ ] Which agents emit 7501 today (Claude Code yes; Codex, Pi, OpenCode?)
- [ ] Should prim reuse the `herdr-app` layout document format, or define its own?
      Reuse means rodeo and the desktop app see prim's layouts for free.

### herdr-core
- [ ] Run the spike: run, send, read, wait, subscribe, 7501 status.
- [ ] Protocol generation 1, "preview, 0.x": how often has `released.openrpc.json` changed?
- [ ] Templates (`templates.list`): could they serve as prim's agent definitions?

### RMUX
- [ ] Run the spike through control mode and through `rmux-sdk`.
- [ ] Control-mode parity with tmux: which notifications and format
      subscriptions are implemented? (`docs/tmux-compat-decisions.md`)
- [ ] How do the Python/TS SDKs connect (FFI to Rust, or own client)?

### tmux
- [ ] Run the spike through control mode.
- [ ] Minimum version to support (format subscriptions need 3.2+).

### Rex (backlog)
- [ ] Dump `rex api` with beta access; record API revision.
- [ ] Check terms: is a third-party client permitted?

## Spike results

Scripts: `spikes/hc_spike.py`, `spikes/cm_spike.py`, `spikes/fake-agent.sh`.

### herdr-core 0.1.21 (2026-11-16): all steps pass
- `hello` reports generation 1 and features `framed, attach, events, status, shell, resize_notifications, automatic_input, restart_environment`.
- run / send / read work over plain JSON-RPC; `hi-from-prim` read back.
- `terminal.label` emits a `labels` event; `doc.set`/`doc.get` work with versions and emit a `doc` event.
- OSC 7501 from the fake agent arrives as `status` events with `app`, `state`, `kind`, decoded `msg`.
  A `turn` event follows the first non-working state.
- `terminal.wait` returned as soon as the agent went `blocked` (`reason: "status"`),
  so it means "needs attention", not "finished".
- Spec deviation: `done` was cleared (`status: null`) on process exit, before `finished`.
  The spec says `done` survives process exit. Not a problem for prim, but worth knowing.
- Gotcha: the Unix socket path limit (103 bytes on macOS) rejects long `XDG_RUNTIME_DIR`s.

### tmux 3.8 control mode (2026-11-16)
- send-keys and `%output` work; pane user options (`@prim_role`) set and read back.
- **OSC 7501 bytes pass through unmodified in `%output`** (all three states seen).
  prim can parse status itself on tmux. `capture-pane -e` does not keep it, so a
  prim adapter must hold a control-mode connection to track status.
- Notifications seen: `%sessions-changed`, `%session-changed`, `%window-renamed`,
  `%window-pane-changed`, `%layout-change` (with a full layout string), `%exit`.
- The format subscription (`refresh-client -B`) returned `%error` as written. Fix the syntax and retest.

### RMUX 0.10.0 control mode (2026-11-16): same as tmux
- Built from source (`~/Projects/rmux`, 17 min release build).
- `cm_spike.py rmux` gives the same result as tmux: send/`%output` work, OSC 7501 passes
  through `%output`, `capture-pane -e` drops it, same notification set (plus `%window-add`).
- ~~Same `%error` on the format subscription as tmux, so the bug is likely in the spike's syntax.~~
- ~~Conclusion: one control-mode adapter can cover tmux and RMUX.~~ **Withdrawn after review (2026-11-16):**
  tmux accepts `refresh-client -B` once the argument is quoted. RMUX rejects `-B` ("unknown flag")
  and `refresh-client -f pause-after` ("requires an attached client"), so RMUX has no format
  subscriptions or flow control in control mode. tmux and RMUX need separate dialects, or RMUX uses its SDK.

See `review-1-backends.md` for the full adversarial review.

### Rex 0.1.0-preview.19, API revision 14 (2026-11-16): all steps pass
- Install issue: the app reported "server won't start" on first launch. `rex server start`
  from the copied server bundle worked, and nothing was logged. Root cause unknown.
- API: `rex api list` shows ~70 server methods plus per-block methods from pluggable
  "block creators" (`com.superlogical.terminal.*`). `rex api describe` gives schemas.
  Revision went from 7 (late Oct write-up) to 14 in about two weeks.
- Model: session > window (tab) > block (pane), plus floating layers, detached blocks,
  block ownership per client, labels on sessions/windows/blocks. `session.view` returns
  the full layout tree with a revision number.
- new / send / capture / split all work with `--json` and return full IDs.
- `rex events -s SESSION --json` delivers `block_event`s named `program_status_changed`
  (working, blocked with kind and msg, done), `process_changed`, `child_exited`, and
  `session_view_changed` with a revision.
- `block call ... program_status` returns all 7501 records. `done` stays after exit (follows the spec).
- Each CLI call shows up as a client connect/disconnect in the event stream. A long-lived
  adapter should talk to the API socket directly (a WebSocket on a Unix socket at
  `~/Library/Application Support/rex/server.sock`, path `/api/control`), but that protocol is undocumented.
- Licence and third-party-client terms are still unchecked.

## Spike definition

Same script per backend:

1. Start `bash` in a named, detached terminal.
2. Subscribe to events.
3. Send `echo hi`, wait for `hi` in output, read it back.
4. Run a fake agent that prints
   `ESC ] 7501 ; state=working:app=fake ESC \`, sleeps, then `state=blocked:kind=question`,
   then `state=done`. Record whether and how each state reaches the client.
5. Kill it; record the exit event.
