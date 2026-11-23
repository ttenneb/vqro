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
  a server for the user. Stop requires non-empty instance IDs on both status reads and confirmation. This is
  **not atomic**: native `server stop` is unconditional; replacement after the second read can stop a different
  server. No conditional-stop API is evidenced. The owned directory must contain the `vqro-owned` marker,
  written only after a successful new owned-server start; an unmarked reachable endpoint is refused, never adopted.
  Markers and socket reachability are coordination evidence, not ownership authentication; same-user path/symlink
  replacement remains a boundary. On every platform, only a positive absence witness permits owned startup:
  the [documented CLI exit4](https://www.superlogical.com/rex/docs/automate/cli#machine-readable-output)
  ("the server could not be reached"), or Unix socket NotFound/ConnectionRefused. PermissionDenied, other socket
  errors, other CLI failures, malformed JSON or invalid status shapes are **status unknown**, never absence.
  Startup refuses without running start or creating a marker on an unknown initial probe. Admin status reports
  `running:"unknown"` and a reason; stop refuses it. A false result means the recognised negative reachability
  witness, not an invented program exit. CLI/protocol status failures after a reachable probe stay unknown too.
- **Terminal environment.** The owned server inherits selected PATH, HOME, LANG, SHELL, USER and LOGNAME values
  from the provider process, not vqro's own variables. These are not a freshly acquired login environment.
- **Ownership.** Sizing a terminal makes vqro its Rex owner (like any Rex app that resizes it).
- **Native robustness (ghostty-audit review, 2026-11-22).** A malformed snapshot could reach an `unreachable` in the
  formatter's charset path (GR = G0, which VT input never produces): charsets are therefore left out of the repaint
  (a DEC line-drawing designation left active isn't restored). With that, about 160000 structure-aware fuzz inputs
  under ReleaseSafe/ReleaseFast were clean apart from **one library decoder bug** (index out of bounds while decoding a
  hyperlink string, 1 input; to report upstream; fixed by a later pin). Decoded memory can be far larger than the
  snapshot (blank history about 700x). Decoding now uses `ghostty_snapshot_decoder_ready`/`next`: it refuses if the
  sum of both screens' advisory history plus two screens exceeds the row budget, and independently counts actual
  progress rows because the advisory counts are untrusted. The allowance is four times (requested history + two
  screens + 256 rows for page granularity), capped at 64,000 rows and 4,000,000 cells. Refusal fails stream open with
  `Rex snapshot exceeds decoded row/cell budget`, before any repaint is published. This bounds history amplification,
  not exact bytes: READY and one incremental page are allocated before their row counts can be checked, and cell
  metadata has variable cost. Existing bounded replay still limits the emitted history. Regression tests encode
  20,000 blank lines at 2000×500 (about 91 KB) and decode in fresh child processes, both with honest advisory counts
  and CRC-correct zero counts; both must refuse below 64 MiB peak RSS on Linux. The ReleaseSafe gate's
  `tui::ghostty` filter includes these controls and the malformed GR=G0 fixture. The snapshot comes from the same
  user's Rex, so these are robustness issues, not a privilege boundary.
- **Alpha API.** Pinned to API revision 19; the data format is undocumented and may change. The provider fails a
  stream (never shows garbage) when the snapshot or metadata framing doesn't parse.
- **Not done (v0):** no data-channel resume, no file upload/download, no Rex Lua actions, no remote Rex hosts
  (D23: Rex's own remote channel could only be an optional transport behind the neutral contract).

## Review corrections (2026-11-22)

- Reconcile requires a valid session list and complete per-session block arrays before retiring that session's
  missing blocks. Failed attach/list or malformed replies preserve prior facts and streams. A successful session
  list that omits a session retires its blocks. An incomplete initial observation cannot send a full sync.
- Reconcile refreshes stored title, pwd, program status, process and exit facts for known blocks too, repairing
  lost events. `src/bin/vqro-rex/facts.rs` is the **single writer** for facts and publication: typed messages enter
  `write_facts` under one async owner mutex. This includes native events, refresh/status observations, acknowledged
  kills, discovery/spawn snapshots, removal/full sync, and effect replies. Native Rex I/O stays outside the owner;
  no blocking mutex crosses I/O. Failed queries retain prior facts.
- Owner rules: events apply to current known blocks; refs that are neither active nor pending allocate nothing.
  Before querying a new discovery, `Begin` admits a bounded pending token/revision in the owner (not a Block).
  Events for that pending ref advance its revision, invalidating older live candidates. Commit requires both the
  exact token and revision; stale discovery is dropped and scheduled for re-observation. Refresh/status/kill
  observations carry their captured Block revision and apply only if it is still current. Each mutation gets a
  checked, globally monotonic revision stored in the Block, never reset/reused in a registration. Kill acknowledgement
  is committed and published by this owner, not returned as an out-of-band lifecycle snapshot. If a newer witness
  invalidates the kill observation, the effect reports acceptance without claiming current lifecycle facts.
- Effect replies are also enqueued under the owner and resolve current facts again, so a newer native event or gone
  cannot be followed by an older spawn/kill reply snapshot. Removal is final for that ref in this registration:
  rediscovery and delayed candidates cannot republish it. Active, retired and pending identities together are
  limited to 65,536, with at most 1,024 pending and refs limited to 256 bytes; exhausting a budget/counter closes
  registration without evicting history or reusing revisions.
  This explicitly bounds retirement bookkeeping rather than retaining unlimited tombstones. Initial discoveries
  remain unpublished until the owner emits the first full sync. The event-triggered status query uses the same rules.
- Created-but-unobserved argv is stored only in the owner's pending identity entry, never a separate unbounded map.
  Its estimated storage (string bytes plus per-argument metadata) is capped at 64 KiB; oversized argv is refused
  before native creation. Creation metadata has a creator token owned by the spawn until its observation attempt
  completes, independent of discovery tokens in the same bounded identity entry. Observations may read it but cannot
  adopt or clear it: discovery cancel/commit ends only its stamp. Only creator completion/cancellation, removal or
  registration end releases creation metadata. Registration end also refuses late admissions from unfinished effects.
  A successfully observed argv lives in the Block's normal facts. Native creation followed by unavailable observation
  still reports the created ref with an outcome-unknown/no-retry error, never a fabricated lifecycle.
- **R1 argv provenance.** The generic provider facts contract now represents unknown creation input as
  `argv: null` (lost metadata, externally created blocks, provider-restart rediscovery). Known creation input,
  including `[]` for default input, stays a list. `data.argv_known` is removed: all providers and clients use
  the same representation (`provider-v0.md` §9.3), not a Rex-specific provenance flag.
- The control reader emits pending lag after queue drain even without another native event. It does not cancel
  a partially parsed WebSocket read to do so.
- Status replacement requires object records, nullable/string id and textual fields, and nullable/numeric
  progress; invalid entries or supported field types preserve the old table. Sparse process replies preserve
  missing names, while explicit nullable names may clear them. An exit requires an integer `last_exit.exit_code`.
- Newly listed blocks without prior lifecycle evidence are **deferred**, not published as live or finished.
  A valid process observation is either an integer last-exit code, or a current child with non-empty name/positive
  pid and no last-exit record. Failed/malformed lifecycle observations cannot complete initial full sync; a spawn
  with only creation evidence remains outcome-unknown, with its created ref named in the error. Existing blocks
  retain their prior lifecycle when queries fail. No unavailable state is fabricated into the live|finished contract.
- Percent escapes are decoded from bytes; malformed Unicode-containing escapes do not panic.
- A created terminal whose follow-up observation is unavailable produces a backend outcome-unknown error naming
  the created ref and warning not to retry spawn, never a fabricated observed exit.
- Resize first takes ownership, then resizes. A first-step native refusal is a definite rejection; a lost/timeout
  response or any second-step error is uncertain/partially effective. Since every `stream.error` is a rejection
  regardless of kind (`src/provider_wire.rs:402`, `src/server.rs:1068`), these cases end output with a
  `backend_dropped` gap and log uncertainty, not a no-effect claim. The core currently has no unknown-outcome
  stream-error mechanism; a gap is the available honest failure surface.
- Owned-server stop holds `<state>/rex/vqro-rex.lock` across its final instance recheck, inventory and stop.
  Owned ensure/start, control connect, attach, session/window creation and viewer attach/ownership use the same
  lock; external `--rex-socket` mode never uses it. Acquisition is asynchronous polling of stdlib file try-lock
  (flock on Unix, Windows equivalent; Windows runtime unverified). Human confirmation occurs before acquisition.
  The stop reports checked session/terminal counts and warns that clients outside vqro can attach/create in the
  window and would be stopped too. The lock excludes vqro, not those clients: the unconditional-stop TOCTOU remains.
- Powerless fake-control/fake-core tests cover all six review findings; a separate unit control proves lag delivery
  after queue drain. No real Rex server is needed for these regression controls.

## Windows transport (2026-11-22)

- Control and data channels use the shared `vqro::transport::connect` (`ClientStream`, Tokio split halves),
  not a second named-pipe implementation. Rex-specific `unix://` normalization is in `src/rex/transport.rs`;
  the prefix is stripped **only on Unix**. Windows values pass byte-for-byte to the shared connector: a
  `unix://`-prefixed pipe spelling is refused, not rewritten into an accepted pipe. The provider's core connection
  uses that same shared connector without normalization (D22: the core endpoint is opaque).
- [Rex's Windows running guide](https://www.superlogical.com/rex/docs/windows/running.md) documents one personal
  server per account, no shared/login server; `rex server start` needs no administrator, keeps running when empty,
  and ends when explicitly stopped or the account signs out. vqro does not install a service or sign-in entry.
- The shared Windows client authenticates the pipe before exposing a stream and requires its object owner to
  be **exactly the current user's TokenUser SID**, not Administrators or another group. Rex's actual pipe owner
  is **unverified**: account-only access in its documentation does not establish the object's owner SID. An explicitly
  elevated `rex server run` may produce a group-owned pipe that is refused. The guide says `server start` normally
  reduces elevated tokens; this is not a claim that every elevated launch is incompatible or that compatibility
  is verified. Shared owner/namespace verification is unchanged, with no bypass or broader acceptance.
- Permanent connector verification refusal is preserved as a distinct control error. Startup and reconnect stop
  with a clear stderr diagnostic (visible in the extension log), rather than silently retrying before provider
  registration. No registered provider-unavailable fact is fabricated when startup never registered.
- On Windows, `vqro-rex server status` (and the owned-server checks before start and stop) ask Rex's own CLI
  (`rex server status --json`), not the shared connector's owner verification. Its `running: true` therefore means
  only that Rex answered; it is not proof that the server is vqro's or the user's. Only connections through the
  shared connector are owner-checked (windows-lead review of 871bedc0).
- Owned marker publication uses `store::write_atomic` on Unix and Windows, under the existing owned-server lock;
  atomic/durability errors propagate as startup failure. This strengthens publication, not marker authentication.
- The guide documents `\\\\.\\pipe\\rex-<SID>` and a suffix when `REX_RUNTIME_DIR` is set. vqro **does not derive**
  either: owned Windows servers use the private runtime directory and `rex server status --json`'s `endpoint`.
  The Unix socket path-length restriction applies only on Unix. The owned command retains a small platform-specific
  login environment allowlist, never inherited vqro/Herdr endpoint or credential variables.
- Verified on a private Linux server (Rex snapshot.2506/API 19): `server start --json` and `server status --json`
  both report a top-level `endpoint` (`unix://...`), and `--state-dir` plus `REX_RUNTIME_DIR` isolates the server;
  that probe was explicitly stopped. Windows CLI JSON shape, isolated pipe selection, startup flags/environment,
  sign-out lifetime and real control/data handshakes remain **unverified at runtime**: no Windows host is available.
  Cross-compilation alone cannot establish those behaviours.
- Checks after rebasing onto the corrected Rex provider plus integrated Windows transport:
  Linux `cargo test --lib rex::` (9) and `cargo test --bin vqro-rex` (38) pass. The corrected provider's
  private-server `tests/e2e_rex.py TARGET` passes all 27 checks.
  `cargo check --target x86_64-pc-windows-gnu --all-targets` passes, including the complete provider
  and owned-server Windows code. An earlier module-only dependency check was blocked by then-unmigrated
  core/client Unix imports; that limitation is resolved by the integrated Windows transport.

## Evidence

- `tests/e2e_rex.py` (gate step `e2e-rex`, SKIP without Rex): owned server under the private root; capabilities;
  env refused; attach repaint shows pre-attach coloured output; Rex metadata never reaches the viewer;
  input/output; resize (`stty size`); stored OSC 7501 status; exit code; kill; provider restart keeps ids; server
  restart retires ids; the user's-own-server mode (existing session appears; turning the provider off leaves the
  server running); the default runtime directory's existence unchanged (not proof of no access); nothing left
  running. The harness uses explicit private runtime selectors for every Rex command; directory existence alone
  cannot prove that an existing user runtime was untouched.
- Unit tests: `src/rex/{ws,data}.rs` (framing, fragmentation, ping, bounds, snapshot boundary at every split,
  metadata nonce filtering at every split), `tui::ghostty::tests::a_rex_snapshot_becomes_a_vt_repaint` (real Rex
  snapshot fixture; history bound), `vqro-rex` status/pwd mapping.
- Investigation notes and probe captures were kept outside the repository (private scratch).
