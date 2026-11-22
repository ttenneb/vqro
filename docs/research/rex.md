# Rex as a vqro backend (2026-11-22)

Rex is Superlogical's terminal multiplexer: proprietary alpha software (`rpm -qi rex`: License: Proprietary;
`rex version`: 0.1.0-snapshot.2506, API revision 19). It runs as a server (Linux, FreeBSD, Windows for enrolled
testers) that Rex's own apps connect to. vqro speaks only to its local server socket; nothing of Rex is linked or
shipped. The provider is `vqro-rex` (`src/bin/vqro-rex.rs`, `src/rex/`), a **separate optional extension**
(`extensions/optional/rex/`), not a default one (human, 2026-11-22). Every observation below was made on private Rex
servers (own `HOME`, `XDG_RUNTIME_DIR`/`REX_RUNTIME_DIR`, `--state-dir`, `--autostart=false`), never the user's.

## What Rex documents and what was observed

| Piece | Source |
|---|---|
| Control channel: WebSocket `/api/control`, subprotocol `rex.control.v1`; JSON `hello`/`welcome`, then `{type: request, id, method, payload}` → `{type: response, id, seq, payload \| error}`, and `{type: event, payload}` | Observed (server log names the protocol); request/event shapes match community clients (`rx`, MIT; `rex-attach`) |
| 75 control methods with JSON Schemas, examples and validation notes (`rex api list`, `help.all`); events (`rex events --all --json`) | Documented by the server itself and https://www.superlogical.com/rex/docs (llms-full.txt) |
| Data channel setup: `data.connect` ticket, WebSocket `/api/data`, HELLO with the ticket, REJECT reason codes, subprotocol `rex.data.terminal.v1` | Documented (`rex api describe data.connect`) |
| HELLO = `0x01` + 16 decoded ticket bytes; WELCOME = 32 bytes starting `0x81`, nonce = little-endian bytes 24..32; REJECT starts `0x82`, reason in byte 1 | Not documented; community clients; observed |
| Stream = a Ghostty snapshot (`GHOSTSNP` v1; records of tag u16, length u32, 4 more header bytes; FINISH tag 6), then raw VT | Not documented; community clients; observed (`tests/fixtures/rex/snapshot-40-red-lines.bin`) |
| Rex metadata inside the VT: `ESC _ rex1;<nonce>;<json> ESC \` (resize, theme, exit) | Not documented; community clients; observed (resize records) |
| `rex block stream` | Listed in the CLI reference; "not implemented" in this build |

The human approved reverse engineering where needed (2026-11-22). None was needed beyond reading the open-source
community clients and observing our own private server's traffic; the Rex binary was not disassembled.

## Mapping to the provider contract

| vqro | Rex | Declared |
|---|---|---|
| instance | server `host_id` (stable across restarts; per state directory) | |
| ref | block id (UUIDv7, never reused) | |
| incarnation | server `instance_id` when the block was seen: after a server restart the same block runs a new program | `identity {refs_survive_backend_restart: true}` |
| `terminal.spawn` | `session.new_window` with a `com.superlogical.terminal.shell` block: `shell: none` + `command` (argv), `cwd`, `size`, label | `{env: false, cwd: true, name: true}`: Rex has no per-terminal environment, a spawn with `env` is rejected |
| `terminal.input` | `com.superlogical.terminal.write {data}` (UTF-8 text) | `{stream: true, effect: false, bytes: "utf8"}`: other bytes refused, never altered |
| `terminal.output` | data channel; snapshot → VT repaint (`vqro::tui::snapshot_to_vt`, libghostty), then live raw VT, Rex records removed | `{raw: true, history: false, repaint: "screen", lossy_when_paused: false}` |
| `terminal.resize` | `session.own_block` (take) + `terminal.resize`; Rex follows the owner's size | `{owner: "latest_viewer", scope: "terminal"}` |
| `terminal.kill`, `terminal.exit` | `block.close`; `child_exited {exit_code}` | `terminal.exit {exit_code: true}` |
| `status.osc7501` | Rex parses and stores program status (`program_status`, events) | `{stored: true, lossy: false}` |
| `events.resync` | events have no history across reconnects | absent: a full sync after reconnect, and a periodic re-list |
| `layout.native` | windows, splits, floating layers | present (informational; vqro never mirrors it, D14) |

## Choices and limits

- **Repaint, not replay.** The snapshot is the terminal's state when the data channel opened; output after it is
  live and complete. The repaint is VT from Ghostty's own formatter (screen, history up to `--history`, styles,
  modes, cursor), without the terminal's default palette (the viewer keeps vqro's theme, D18). History is bounded by
  replaying into a fresh terminal with a line limit, because a limit set on the decoded terminal keeps what it
  already holds.
- **Native surface.** Decoding needs libghostty's `snapshot` and `formatter` features, which the TUI build had left
  out. Both are now built (same archive members; `vendor/libghostty-vt.notices.md`); only `vqro-rex` calls them.
  Bindings follow the existing rules (`bindings_check.c`, constant test).
- **Fallback if the native features are refused** (review condition, vqro-lead 2026-11-22): build the repaint with
  Rex's own `com.superlogical.terminal.format {format: "vt"}` instead of decoding the snapshot, and skip the
  snapshot bytes using the documented-by-observation framing (`rex::data::Snapshot`). That needs no libghostty
  snapshot/formatter, but Rex formats the screen a moment *after* the data channel's snapshot point, so output in
  between may be drawn twice; a provider choosing it must declare that (e.g. `terminal.output {repaint:
  "screen_approximate"}`).
- **Servers.** Default: a vqro-owned server at `<vqro state>/rex/server.sock` (`REX_RUNTIME_DIR`), started with
  `rex server start` (idempotent, keeps running when empty), outliving the provider and core (D14, as the owned tmux
  server); stopped only by `vqro-rex server stop` after the instance is confirmed. Option: the user's own server
  (`--rex-socket`), never started or stopped. Every `rex` call passes `--autostart=false`: by default `rex` starts
  a server for the user.
- **Terminal environment.** The owned server starts with a login-session environment (PATH, HOME, LANG, SHELL,
  USER, LOGNAME), not vqro's own variables.
- **Ownership.** Sizing a terminal makes vqro its Rex owner (like any Rex app that resizes it).
- **Alpha API.** Pinned to API revision 19; the data format is undocumented and may change. The provider fails a
  stream (never shows garbage) when the snapshot or metadata framing doesn't parse.
- **Not done (v0):** no data-channel resume, no file upload/download, no Rex Lua actions, no remote Rex hosts
  (D23: Rex's own remote channel could only be an optional transport behind the neutral contract).

## Evidence

- `tests/e2e_rex.py` (gate step `e2e-rex`, SKIP without Rex): owned server under the private root; capabilities;
  env refused; attach repaint shows pre-attach coloured output; Rex metadata never reaches the viewer;
  input/output; resize (`stty size`); stored OSC 7501 status; exit code; kill; provider restart keeps ids; server
  restart retires ids; the user's-own-server mode (existing session appears; turning the provider off leaves the
  server running); the user's `/run/user/<uid>/rex` untouched; nothing left running. 26/26 on 2026-11-22.
- Unit tests: `src/rex/{ws,data}.rs` (framing, fragmentation, ping, bounds, snapshot boundary at every split,
  metadata nonce filtering at every split), `tui::ghostty::tests::a_rex_snapshot_becomes_a_vt_repaint` (real Rex
  snapshot fixture; history bound), `vqro-rex` status/pwd mapping.
- Investigation notes and probe captures were kept outside the repository (private scratch).
