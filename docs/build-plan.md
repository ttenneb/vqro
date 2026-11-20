# Build plan

D16: build vqro itself, smallest complete and usable first. The north star: an agent can substantially modify its own
workspace through reliable public primitives, without changing the host or endangering underlying processes.

## Final project name and offline state-copy migration (2026-11-18)

D27 fixes the lowercase name **vqro** before publication: crate/binaries (`vqro`, `vqro-herdr`, `vqro-tmux`),
`VQRO_*` environment, `vqro.*` recovery commands, `doc/vqro/*`, default state/share/socket paths, badge and current
documentation. Python SDKs use `vqro_ext`, `vqro_provider*`, `vqro_display`, `vqro_views`, `Vqro`/`VqroError` and
`Extension.vqro`. Historical research and earlier decisions stay byte-identical; no runtime old-name aliases.

The stdlib migration utility copies offline state to a new, exclusive private destination, translates only known
references (including undo values), preserves transaction metadata and opaque identity/credential files, and leaves
the source intact. It refuses active source locks, collisions and symlinks; it skips transient endpoints/PIDs and
never contacts a backend. The parent owns installation and the human's one-time endpoint cutover.
Twelve focused regressions pass. An actual pre-rename build's generated state resumes five resource IDs and a
runtime credential on the new build; migrated undo restores new command names. An authorized read-only copy of
the daily state (82 entries, 253 transactions) passes, with identity/credential bytes preserved and both private
copies deleted. No live configuration was executed, and no live migration/install was performed.

## Final fuzz P2 / scoped launch hardening (2026-11-18)

Clean request-input EOF now drains already-read requests and local reply flush before connection cleanup; a real
write failure still has indeterminate delivery, never rollback/replay. Connection-owned commands leave on close,
TUI caches withdraw them, and live command metadata has a 1 MiB encoded bound. Explicit noninteger revert targets
reject before undo selection. Pi launch uses settings/source/host scoped reads and preserves observed results on
post-spawn lookup/UI failure, without retrying spawn. Native post-gap input delivery is documented per implementation,
not overpromised; vqro.shutdown is documented as implemented. Red/green details: `research/final-fuzz-p2.md`.

## Bridge P2 import/environment/idle hardening (2026-11-18)

Default Pi launch now uses an isolated Python interpreter and explicit installed launcher, never the project cwd
or injected `PYTHONPATH` for bridge imports. Configured host argv/environment remain literal. Pi/tools keep user
Python paths but not bridge modules or supervision/core configuration; only the terminal endpoint/marker remains.
Native asyncio exit notification, a blocking latest-frame fallback queue and journal-triggered/coalesced updates
remove idle polling while retaining immediate controls, EOF and bounded inherited-pipe cleanup. Reviewer-shaped
200-bridge idle measurement improved 114.3% of one core to 0.0% observed over 30 s; interpreter memory is unchanged.
Mechanism, red/green regressions, load sample and limits: `research/bridge-p2.md` and
`../extensions/pi_bridge/PROGRAM.md`. No native port, runtime restart or new core transport.

## Native herdr drift hardening (2026-11-17)

Found the soak's load-dependent loss: hc event DATA coalesces NDJSON records, while the adapter parsed one JSON
object per frame and silently dropped multi-record payloads (and credit). Bounded incremental decoding, consumed
per-frame credit and subscribe/missed sequence cutoffs now preserve every observation. A second generic ordering
fix prevents spawn completion from overwriting newer facts for an already-published identity.

Controlled red/green: 600 one-second shells previously left 543 phantom live rows and 100 already-pruned rows;
decoder-only immediate exits exposed 78 creation-reply rollbacks. The permanent real-hc full-gate regression runs
three 600-terminal waves, converges to native's 500 retained finished rows each time, preserves 1800 distinct ids,
then converges to 497 after explicit forget. Same provider generation throughout: no reconnect or polling workaround.
Mechanism, finite-window/sequence tests and limits: `research/herdr-drift.md`.

## Identity/connection/registry hardening (2026-11-17)

Implemented active-only indexed identities with a durable monotonic high-water mark, one durable replace per
inventory/sync chunk, and batched host credential expiry. Observed closures/replacements fence stale routes even
when retirement persistence fails; no ids are reused or effects replayed. The registry now admits 1024 providers
under a 4 MiB aggregate metadata budget and reclaims only empty disconnected runtime records, retaining source/
generation fencing without a historical-name table. Idle event subscribers release on peer close; live content
caches encoded byte charges and rejects different-author replacement. Only existing managed-lifetime proof reserves
an `ext:name` author; SDK/D24 compatibility uses genuine private token fixtures.

Before/after: 600-new-id core hold 1071→39 ms (43 ms on home btrfs); 10k idle Core RSS 16→127 MiB becomes 14.3→14.6 MiB.
A real SDK probe admits 600 runtime providers plus a backend, then 2000 completed lifetimes: 868759-byte snapshot 189 ms,
registration p99 16 ms, active Core RSS 88.7 MiB; registry aliases, clients, identity and credential records drain.
600-host retirement improved 1.65 s→40 ms. These are target fixtures, not a 600-provider status-flood claim.
Evidence, fault/restart regressions and final full-gate authority: `research/identity-hygiene.md`.
Architecture is current; the old backend-adapter sketch is explicitly superseded by the implemented provider wire.

## Slice 1: first working vqro

| Component | Scope |
|---|---|
| `vqro serve` (core) | JSON-RPC 2.0 lines over a local socket. Workspace store: versioned entries (views, layouts, bindings, docs, extensions) changed only by transactions, persisted atomically, with history and revert. Resources from adapters. Event log with seq, cursors and `missed`. Command registry routed to owners. Extension supervisor with crash budget. Built-in recovery commands. |
| herdr-core adapter | Through `hc connect` (stdio, works locally and over ssh): list, spawn, kill, raw attach streams for input and output, resize, events (status from OSC 7501, exit). |
| Fake adapter | Deterministic test fixture only. |
| `vqro tui` | Renders the active layout (splits, tabs) of terminal views (VT emulator over the raw stream) and declarative panel views; key prefix with built-in recovery menu that workspace bindings can't override; command palette. |
| `vqro call` | CLI for scripts and agents: any core method from the shell. This is how agents modify the workspace. |
| First extension | A real extension, outside the core, that builds part of the workspace through public primitives only. |

Done when: the owner can open vqro over herdr-core, run shells and agents in it, an extension adds a panel, command and
binding, an agent changes the layout with `vqro call`, a broken extension is contained and reverted, and the core
restarts without disturbing any terminal.

## Then, driven by use

2. Agent bridges for headless runtimes (pi RPC, then Claude Code), with agent views.
3. Second backend (tmux or Zellij) through the same tests.
4. Windows validation of the contracts.
5. Whatever dogfooding shows is missing.

## Slice 1 status (2026-11-16)

Working and tested (`cargo test`: store unit tests, key encoding, end-to-end core tests over the fake backend):
- `vqro serve` over herdr-core via `hc connect`: spawn, attach, immediate input, resize, OSC 7501 status, exit.
  Core restarts leave terminals running and keep resource ids.
- Workspace store: validated, all-or-nothing transactions with compare-and-set, history, revert, atomic persistence.
- Commands routed to owning connections; `vqro.*` recovery commands built in.
- Extension supervisor: live replacement on spec change, crash budget, safe mode, per-extension logs.
- `vqro tui`: tabs and splits from the layout, terminal views (vt100) and panel views, palette, resource picker,
  built-in prefix that bindings can't override, status bar showing safe mode and failed extensions.
- `vqro call` / `vqro run`: the agent path for changing the workspace.
- `extensions/sidebar.py`: a real extension using only public primitives (panel view, commands, binding, layout CAS).

Verified by hand against herdr-core: an agent-style `vqro call` installed the sidebar; a broken extension
written the same way was contained by the crash budget and removed with one revert.

## Foundation hardening (2026-11-16)

- Store commits persist a candidate before publishing entries/history/counters, using unique temporary files,
  file sync, replacement and directory sync. Pre-replacement failures are effect-free; a post-replacement sync
  failure adopts and publishes the transaction but reports `durability_unconfirmed` (never replay automatically).
- Resource-id persistence failures are logged and reported as `persistence.error`, without hiding backend effects.
- Command registrations are connection-bound, so an old extension connection closing cannot disable its replacement.
- Regression coverage: failed write rollback and retry, uncertain directory sync and event publication, temporary-file
  cleanup, resource-map error reporting, and same-identity extension replacement routing/live-content cleanup.

## Bounded terminal streams and deadlines (2026-11-17)

- Terminal and herdr frame queues now have byte budgets. Backend CREDIT follows downstream handoff into the
  connection's bounded writer queue, rather than an unbounded adapter enqueue.
- Connection control/data budgets are independent. Slow streams close explicitly; their queued output is
  discarded without replay, while normal final output remains ordered before closure. Control overflow disconnects.
- Forwarded command deadlines report `timeout` with an indeterminate outcome, never retry, and clean pending calls.
- The TUI resets parser state and retries closed live terminal views with backend history, including closure racing
  the attach reply. Event-resync connection fencing is preserved.
- Release scale harness at the default 600-terminal/200-agent profile: stalled-only budgets passed; with both a
  stalled client and a 64 KiB/s reader, RSS peaked at 33.9 MiB and no slow echoes timed out, but the placeholder
  slow-echo p99 budget still failed (1.10 s versus 250 ms). Bounded memory does not imply bounded transport latency;
  existing socket bytes and status-event bursts cannot be preempted. Final teardown leaked no processes.
  Evidence and the explicit scale-test failure: `research/bounded-streams.md`.

## In-process provider reshape (2026-11-17)

- Registry exports provider instance, generation, capabilities and health. Disconnect keeps unavailable stale facts;
  fresh same-instance facts restore ids, while changed identities retire ids durably with tombstones.
- Native queued effects, replies and streams are generation-fenced; spawn/kill/attach use shared deadlines and D10
  outcomes without replay. Late replies are ignored; ordinary effect audits retain caller attribution.
- Identity persistence errors publish no unpersisted ids, retain native facts for recovery, coalesce failures and
  expose recovery. Legacy two-field ids are tombstoned rather than guessed; layout reset/rebinding is explicit.
- TUI merges partial facts before checking availability and reattaches only when the resource is available.
- Provider-v0 step 1 only: no external provider wire methods, tokens or relay. Evidence: `research/provider-reshape.md`.

## Core provider wire (2026-11-17)

- Implemented pinned provider-v0 section 9 registration/generation, chunked sync, facts, forwarded effects,
  deadlines/duplicate-id bounds, credited/offset streams, disconnect fencing and durable runtime resume credentials.
- Native and wire resource admission enforce the shared snapshot allowance. Opt-in bridge spawn injects env;
  ordinary shells do not receive tokens. (Superseded 2026-11-17: backends run as provider extensions; every
  terminal gets `VQRO_SOCKET`; see "Backends as extensions".) No essential/safe-mode policy change.
- 125 Rust tests pass with zero warnings; Python SDK/fake and private real vqro-tmux extension interop pass.
  External default 600/200/8 scale passes all budgets, 49.88 MiB peak RSS and zero leaked processes.
- Details and limits: `research/provider-wire.md`. Relay, Windows and provider-backed Pi turns remain open.

## Pi provider/core end-to-end (2026-11-17)

- Private real vqro + herdr-core, fake Pi: token-injected bridge spawn, one agent/live panel, all six native effects
  via `vqro call`, dispatched UI answer, exact rejected/indeterminate event audits, core SIGKILL/restart with the
  same host/agent/native process and current run, owned-view reuse and no native command replay; Pi exit -> gone.
- Installed Pi 1.5.0: private metadata/state, core restart with stable native process, public stream EOF and clean
  exit 0. No prompt/model/provider call or real credential/session involved; network namespace blocks networking.
- Fixed a bridge daemon stdin BufferedReader lock that caused CPython SIGABRT on native Pi exit. Raw `os.read`
  keeps EOF handling without holding Python I/O locks at interpreter shutdown; native exit code is preserved.
- Opt-in reproducible harness: `tests/pi_provider_e2e.py`. Native herdr now explicitly declares stream-only input;
  `--probe-terminal-invoke` verifies unsupported/rejected before dispatch; public stream input is dispatched.
- Details, receipts and unverified limits: `research/pi-provider-e2e.md`.

## Core shutdown and input transports (2026-11-17)

- `core.shutdown`, recovery `vqro.shutdown`, CLI protocol-first stop and signals share ordered cleanup; no backend
  terminal kill or input replay. Extension EOF grace/reaping and provider-link stop are fixture/subprocess tested.
- Native kill replies reconcile finished facts even with open attach and no finished event. Native event DATA
  CREDIT follows a FIFO consumption barrier; evidence: `research/core-shutdown.md`.
- Native and current wire terminal providers distinguish input transports: `{stream:true,effect:false}`. Generic
  invoke rejects explicit unsupported effects before dispatch; normal literal stream input remains available.
  Evidence and hc schema rationale: `research/native-input-transports.md`.

## Agents extension UI (2026-11-17)

- Bundled `agents`: default pi1 RPC runtime settings, New Pi agent tabbar/prefix a, public tabs.show integration,
  prompt/steer/follow-up/abort/clear-queue pane actions and a pending-request popup covering native answer kinds.
- Native outcomes appear in notifications without treating acceptance as settlement or dispatched answers as ack.
  Runtime settings are editable from Agents and Settings; existing agents and hidden tabs keep running.
- Bridge live panel now shows semantic status badge, observed run, queues, pending requests and bounded transcript.
- 15 fake-core unit tests plus private real-core/backend extension-command and physical PTY flows. Prefix launch,
  typed prompt, abort, request/Yes-No answer, queues and final transcript verified with fake Pi only. Quick gate
  passes (90 Python/154 Rust), no warnings, no leftovers; no Rust edits, model/provider calls or live install.
- Limits and receipts: `research/agents-extension.md`; usage: `../extensions/agents.md`. Editor answers use the
  existing one-line input; multiline UI, Windows and provider-backed turns are not claimed.

## Real TUI scale (2026-11-17)

- Opt-in `tests/e2e_tui_scale.py`: private fake core + real sidebar/TUI/tmux; 600 terminals, 200 status producers,
  three visible terminal views and a byte-validated 1 MB/s flood. Added to `scripts/check.sh full` with explicitly
  non-enforced placeholder performance budgets.
- Background redraws are frame-budgeted; keys retain priority. Unchanged panel values/flattened rows are shared,
  with invalidation for content, shape and snapshots; geometry/themes still render fresh.
- Sidebar coalesces derived publication in its own worker without holding the SDK cache lock across RPC.
- Corrected full-load probe: sidebar failed after three crashes before, stays running after; TUI CPU23.18→17.55%
  of one core, background peak55fps,37.65MiB RSS, marker p9952.07ms; no lost input/missed events/leaks. Earlier
  wrong-flag receipts are not flood evidence. Methodology/caveats: `research/tui-scale.md`.

## Granular agents/Pi split (2026-11-17)

- `agents` now owns runtime-neutral, capability-driven controls/list/panels; `pi` owns launch/runtime settings,
  prefix a and settings migration. Both are bundled; disabling either leaves backend execution independent.
- Neutral panels use resource/provider facts and optional read-only native output, with no runtime-specific import.
  Referenced panels remain valid while provider inventory restores. Backend naming is left to the backend.
- Full check gate passes17/17 with99 Python tests, all opt-in suites/UI/scale and no leftovers. No Rust changes,
  real prompts/model calls or live install. Details/receipts: `research/agents-split.md`.

## Searchable Extensions catalogue and static metadata (2026-11-17)

- Fourteen static bundled manifests; disabled/failed/safe declarations remain inspectable without execution.
  Bounded file/cache admission, refreshable metadata and pid-free runtime facts through public APIs.
- Generic optional `list_detail`, wrapped text and responsive panes, per-client interaction state, filter/count,
  viewport paging, readable groups and explicit actions. Older clients retain ordinary list content.
- Settings uses those primitives uniformly, including its own controls and manifest settings commands; D21
  session choices remain separate from normal enabled declarations. No registry/install or log viewer added.
- Private real-TUI harness 17/17; full gate 18/18, zero warnings/leftovers. Guide: `extensions.md`;
  captures, evidence and limits: `research/extensions-page.md`.

## QA3 upgrade/recovery corrections (2026-11-17)

- User-path in-process-herdr -> CLI defaults/provider migration and approved sanitized realistic-copy private-hc proof.
- Provider names resolve at start, unchanged pinned defaults repair safely, v1 gains Pi once, and declared-default
  startup cannot misroute spawns. Missing backends are visible.
- Lifetime-authenticated safe providers, persistent chooser drafts, scratch endpoint isolation, running-image
  preference, retained failure evidence and explicit embedder restoration. Pi resumes across safe mode without replay.
- Full scoped gate: 18/18, 204 Rust and 112 Python tests, zero warnings/leftovers. Evidence/limits:
  `research/qa3-upgrade-recovery.md`.

## Empty-start safe-mode chooser and reinstall recovery (2026-11-17)

- Human-confirmed D21 revision: no extensions/providers start in safe mode; a built-in grouped checkbox popup
  enables an explicit session-only subset. No essential exemption or persisted enabled edits. D22 is confirmed.
- Rebased onto declared backend providers; herdr/tmux appear in recovery choices. Old source generations are fenced
  on stop/revoke; provider registration rechecks safe admission after asynchronous token waits.
- Replaced Linux executable paths no longer prevent core restart/re-exec; CLI closed stdout is quiet. Private
  replacement smoke and full gate pass18/18,187 Rust/104 Python tests, zero warnings/leftovers.
- Evidence and limitations: `research/safe-mode-recovery.md`.

## Platform launcher/identity fixes (2026-11-17)

- Host-resolved/configured bridge argv and environment replace the core interpreter/OS assumption. Native backend
  ids/nonces, not display hostnames, choose provider defaults. Herdr remote refs use Machine.id/session/terminal;
  aliases are display metadata and identity-unsafe proxied effects are explicitly unsupported pending upstream.
- vqro-herdr now takes JSON argv or repeated literal prefix args, preserves spaced paths and uses split_paths.
  Red/green, durable migration/tombstone tests, scoped quick gate and real-private herdr/tmux opt-ins pass.
  No live install/remote/model calls. Migration limits and upstream ask: `research/platform-small-fixes.md`.

## EOF retirement race (2026-11-17)

- Fixed accepted provider resource.gone tails being rejected when EOF marked routes disconnected before ingress
  drained. Retirement now checks source/generation atomically even across EOF; upserts/effects remain unavailable
  and an old provider cannot retire a replacement. No timeout increase or replay.
- Deterministic lifecycle regressions red/green; exact agents UI flake reproduced 3/6 under owned single-CPU
  contention, then 6/6 pass. Full gate 18/18 passes, zero warnings/no leftovers; live vqro not touched.
- Evidence and bounds: `research/agent-exit-flake.md`.

## tmux sizes and backend readiness (2026-11-17)

- Spawn/open/resize submit native window and layout-constrained pane sizes; new owned servers keep only a marked,
  exited internal bootstrap pane rather than a login shell/resource. Existing native work remains untouched.
- TUI renders authoritative live backend notices even with an alternative backend. Provider display readiness is
  separate from supervisor process status; Settings refreshes and keeps configuration recovery available.
- Red/green native layout and kernel PTY assertions, core/TUI/settings regressions, and physical provider/TUI E2Es.
  Native resize timers are observed, not mistaken for settled delivery; `research/tmux-size-notice.md`.

## Full key labels (2026-11-17)

- Native `key_labels` display option defaults to full Ctrl-/Alt-/Shift- names; short restores compact labels.
  Prefix/menu/status/help/panel hints and extension Settings/profile labels follow the shared choice, without
  changing stored bindings or terminal input. Built-in fallback works in safe mode/with extensions off.
- One formatter per language with the same JSON parity table; native render and private physical full/short/
  unknown captures verify live updates and unchanged bindings. `research/key-labels.md` records the route.

## Soak-4 recovery and tab contention (2026-11-18)

- Failed extensions retain frozen cause/time/count and bounded last exits beside recent/lifetime totals. Explicit
  native badge/prefix recovery works even with Settings off; no automatic retry or safe-mode admission change.
- tabs.show retries bounded CAS contention beyond the old three attempts; coherent SDK layout snapshots prevent
  mixed version/deleted-value races. Regressions preserve concurrent tabs and never replay spawn/unknown effects.
- Policy/evidence and historical-cause limitation: `research/soak4-p3.md`.

## Snapshot admission must not close recovery UI (2026-11-18)

- Startup/reconnect/post-install re-exec now open responsive built-in recovery on temporary snapshot refusal.
  Read-only snapshot retries use a5s attempt deadline and capped jittered backoff with a persistent core-busy badge.
- Resync keeps the peer, terminal streams and last workspace; events remain fenced, and event traffic cannot
  starve the retry deadline. CLI diagnostics report refusals/deadlines without replaying requests.
- Red/green, real3MB/6-caller startup repro and physical private replacement/re-exec evidence:
  `research/tui-startup-busy.md`.

## Licence and contribution policy (2026-11-18)

- D25: Apache-2.0 for all original material, DCO1.1 sign-off/no CLA; established Cargo/bundled-manifest
  author/licence metadata, canonical root LICENSE, NOTICE and contribution/brand guidance.
- Offline stdlib SPDX check covers214 metadata packages. Deterministic notices preserve original legal texts
  for all213 third-party packages (including unbuilt/target variants), with labelled reviewed fallback text where absent.
  Quick/full gates reject disallowed declarations or stale notices. No speculative dependency-feature trim.
- Policy, sources, target scope and evidence: `research/licensing.md`.

## Declarations and UI clients, step 1 (2026-11-19, D29)

- Manifests declare `provides`/`needs`/`works_with`; the core reports `dependencies` (and reverse `used_by`) in
  `extension.list`/snapshots and in enable/disable replies, never acting itself. Settings > Extensions shows them,
  offers needed extensions on turn-on and warns what breaks on turn-off; the safe-mode chooser names needs.
  `scripts/check_manifest_deps.py` keeps the 15 bundled manifests honest.
- `src/ui_model`: the renderer-independent client model (cache and event application, contributions, panel
  flattening, key names, shared recovery content). The ratatui TUI renders over it and declares `hello` profiles
  and features. Base-profile conformance fixtures: `tests/fixtures/client/`.
- `vqro ui [<client>]` (built-in TUI by default; client manifests in `<extensions dir>/clients/`). Second
  implementation: `extensions/clients/text.py` (stdlib Python, base profile; `vqro_client_model.py` passes the
  same fixtures). Next: element/view-kind `fallback`, an unfamiliar real extension (review P2), OpenTUI spike
  (needs a runtime install decision). Design: `research/declarations-and-ui-clients.md`.

## Fallbacks and the log viewer (2026-11-19, D29, review P2)

- Rows, chips and view kinds degrade to declared fallbacks (Rust model, TUI, Python text client, fixtures).
- `logs` extension: in-app extension log viewer (Settings > View log), its own view kind with a panel fallback;
  forced exactly two host changes, view-kind fallback and the `follow` panel hint (`research/log-viewer-p2.md`).

## Known gaps found while building

- **History churn:** extensions that refresh views on every event (the sidebar on status changes) fill the
  transaction history. Consider a separate, unversioned channel for derived view content, or coalescing.
- **Resize ownership:** with several viewers of one terminal, the latest active one sets the size (herdr-core's
  rule). No vqro-level rule yet.
- **Extension views** are only `panel` (declarative) or `terminal`; the hybrid option is untested.
- **Portability:** Unix socket only; Windows (named pipe) not yet validated (D12, D22 proposed). See "Windows work".
- **Agents** run PTY-backed for now; the headless agent bridge (pi RPC, Claude Code) is the next slice.
- **Adapters are in-process** Rust tasks behind a channel interface; the out-of-process adapter protocol from
  `spec/backend-adapter-v0.md` waits until a second backend shows what it needs.

## Slice 2: opinions as extensions (D17), 2026-11-16

- Core lost `vqro.open`; keeps only recovery commands. New `menu/*` entries and gesture bindings (`mouse.right`,
  `mouse.middle`), contributions carry `owner` and disappear when their extension is off.
- TUI draws pane/tab menus, tab-bar and status-bar buttons from entries; prompts (`input`) and pickers (`pick`);
  passes `context` to commands; `client.menu|prefix|palette` are its only commands.
- Bundled default extensions, installed on first run from `defaults.json`: `tabs`, `panes`, `gestures`, `sidebar`,
  `settings` (helper `extensions/vqro_ext.py`). Settings toggle extensions and client options (`doc/settings/tui`).
- Verified through a pty: prefix `n`, `+`, right-click pane/tab menus, rename prompt, middle-click close, status
  buttons, settings tab, and disabling the gestures extension (then `mouse`) removing right-click behaviour.
- Seen once, not reproduced: one `+` click opened two tabs.

## Slice 3: richer UI, workspaces (2026-11-16)

- Panel catalogue: pages, trees (client-side expansion), badges, scrolling with scrollbar, `/` filter, row menus,
  inline input rows; toasts (`ui.notify`); live status-bar segments. Resources gained `cwd`.
- New `workspaces` extension (manual list in `doc/workspaces/list`, git worktrees, open shell here, new worktree),
  published as live data that the sidebar consumes. Sidebar is one tree (workspaces → worktrees → terminals,
  other terminals) with presets in `doc/settings/sidebar`; OSC 7501 status as badges rolled up to parents.
- Settings popup has pages: Extensions, Interface, Sidebar, Keys (all bindings, filterable), About.

## Slice 4: polish streams (2026-11-16)

Built as parallel streams on git worktrees, reviewed and merged by a coordinator:
- **Sidebar:** one-line toolbar (`Show: … ▾`, `/`), view popup with toggles and presets, type icons coloured by state.
- **Themes (D18):** palette tokens with a built-in default; `themes` extension with 14 themes (Catppuccin with
  accent choice, Dracula, Tokyo Night, Nord, Gruvbox, Solarized, One Dark, Rosé Pine, Kanagawa, Light);
  tab bar band; safe mode ignores the theme.
- **Keys (D19):** rebindable prefix, binding priority and `client.none`, `client.focus|tab|quit|zoom`, keymap
  profiles (vqro, tmux, screen) in the `keys` extension, live key feedback (kitty protocol, prefix only).
- **UI:** zoom, popups that survive their dropdowns, Nerd Font icons with fallback, self-restart on reinstall,
  client build tracking in `vqro status`.

Known gaps: undo skips extension-made changes (e.g. theme picks); agents still run PTY-backed (headless agent
bridge not built); second backend (tmux) not started; Windows untested.

## Pi runtime bridge half (2026-11-17)

- `extensions/pi_bridge/`: standalone stdlib async Pi 1.5 RPC child-pipe client. Bounded LF framing,
  independent stdout/stderr drains, ID correlation, native effects with deadlines/no resend, and observed
  run/queue/dialog facts. Acceptance is not settlement; answering a dialog is dispatched, not acknowledged.
- `tests/fixtures/fake_pi.py` and `tests/test_pi_bridge.py`: deterministic tool/queue/settlement/dialog,
  crash, late-response, Unicode framing and backpressure regressions without any provider/model calls.
- `tests/pi_bridge_smoke.py --run`: opt-in real installed Pi 1.5.0 metadata-only check in a fresh network
  namespace and private environment/state/config/session roots. No real credentials or sessions used.
- Not wired to vqro/provider registration yet. The future bridge must run inside a backend-owned terminal;
  this library does not turn vqro into an agent process host. API and limits: `extensions/pi_bridge/README.md`.

## Extension lifetime (2026-11-17)

- Every managed extension receives a reserved stdin lifetime pipe and `VQRO_EXTENSION_LIFETIME=stdin`.
  The core retains its writer; core death closes it without requiring cleanup code. Extensions must exit on EOF.
- `vqro_ext.py` watches from import on an independent thread, so startup/command code need not reach socket EOF.
  A plain non-Python pipe reader follows the same contract; no Unix-only parent-death API is required.
- Real fake-core regressions cover SIGKILL, SIGTERM and clean SIGINT shutdown, including blocked-before-connect
  helper startup. The scale cleanup check still reports zero leaked extensions under restart/flood load.
  Backend-owned terminal/agent lifetimes are not changed.

## tmux provider, provider side (2026-11-17)

- `src/provider_client.rs`: Rust provider-side helper for `spec/provider-v0.md` section 9 (register and generation,
  chunked sync, effects with deadlines and a duplicate-id ledger, one reply per request, credited streams with
  contiguous offsets, gap ends stream). Mirrors `extensions/vqro_provider.py`.
- `src/bin/vqro-tmux.rs`: tmux as an out-of-process provider: server identity as instance, panes as terminals, one
  control client per session, reconcile after structural events, lossy OSC 7501 status, repaint-first streams,
  gap on pause/lag/dropped client. Details and findings: `research/tmux-provider.md`.
- Tested against an in-test fake core and private tmux 3.7b (`tests/provider_client.rs`, `tests/tmux_provider.rs`,
  opt-in). Real core section 9 spawn/repaint/input/echo/kill extension interop now passes (`tests/provider_wire.rs`).

## herdr-core provider, provider side (2026-11-17)

- `src/bin/vqro-herdr.rs`: herdr-core as an out-of-process provider, wrapping the in-process adapter unchanged
  (instance = `core.id`, spawn/kill/attach, history streams with credit after send, status, exit codes, re-register
  per herdr-core connection). Opt-in tests on a private herdr-core (`tests/herdr_provider.rs`). Notes and findings:
  `research/herdr-provider.md`. Native adapter env now reaches hc; the external wrapper still declares env unsupported.

## Windows work (known, 2026-11-17)

From the static portability audit (D22 proposed; nothing compiled for Windows yet; installing a Windows target needs
the human's OK). Contracts are updated; this is implementation.
- **Compile:** a transport module with a named-pipe arm for `src/server.rs` / `src/client.rs` (tokio has
  `net::windows::named_pipe`; `src/rpc.rs` is already generic over the byte stream); `cfg(unix)` around SIGTERM in
  `src/main.rs`; a Windows branch for the TUI re-exec (today an unused variable there would break zero warnings);
  `tests/core.rs:141` and `tests/scale.rs` onto the transport or gated to Linux. Estimate about 2-3 h with the runtime
  fixes below.
- **Runtime blockers that compile but fail:**
  - `src/store.rs` opens the state directory to fsync it. On Windows `File::open(dir)` fails, so the core would not
    start. Use a write-through replace (`MoveFileExW`) and skip the directory sync there.
  - `src/paths.rs` defaults use `HOME`, usually unset on Windows, so state would land in the current directory. Use
    `%LOCALAPPDATA%\vqro`.
  - `extensions/vqro_ext.py` connects with `AF_UNIX`, which CPython on Windows lacks. Use the `asyncio` proactor
    pipe connection. About 1-2 h, testable only on Windows.
- **Defaults and CLI:** `defaults.json` names `python3` (use a portable interpreter choice); `resource.spawn`'s default
  shell moves to the provider (`$SHELL` / `/bin/sh` in `src/server.rs` today); `vqro stop` moves to `core.shutdown`;
  the background core needs detached creation flags instead of `process_group(0)`; the herdr-core default path in
  `src/main.rs` is a macOS bundle path.
- **Optional:** Job Objects for extensions (also covers wrapper grandchildren), and the named pipe's out-buffer size as
  the counterpart of the 16 KiB send buffer (re-measure the first-echo case).
- **Validation:** `cargo check` for `x86_64-pc-windows-msvc` and `-gnu` as a CI gate once the target is approved; a real
  Windows smoke run of the provider transport before any contract is a freeze candidate (`provider-v0.md` step 6).

## Provider SDK half (2026-11-17)

- `extensions/vqro_provider.py` and `vqro_provider_stream.py`: stdlib async provider-side implementation of
  `spec/provider-v0.md` section 9: registration/generation, chunked sync, native effect handlers and bounded,
  credited/ordered streams. Reuses the managed stdin lifetime watcher; never reconnects or resends automatically.
- `tests/fixtures/fake_provider.py`: standalone external fake, with echo/greeting and the same status-period,
  status-phase and output-rate argv flags as the in-process fake. No actual terminals/programs are spawned.
- Socket-pair fake-core tests exercise the SDK without a real core. Core wire integration and external-provider
  default scale now pass too (`research/provider-wire.md`). API/bounds: `extensions/vqro_provider.md`.

## Backends as extensions (2026-11-17)

Human decisions: adapters run as extensions; tmux only as a private server; every terminal reaches vqro.
- `vqro serve` runs no backend in process (`Backend::Extensions`); `--fake` adds the in-process fake for tests.
- Default extensions v2: `herdr` (vqro-herdr, `kind: provider`, `default: true`) and `tmux` (vqro-tmux, private
  server, off). Existing installs gain them once through versioned defaults; removed entries stay removed.
- The default spawn provider is the extension marked `default`, else the first backend, and never a runtime bridge.
- `VQRO_SOCKET` goes into every spawned terminal's environment; the bridge token stays opt-in.
- Resource ids carry over from the in-process herdr adapter to vqro-herdr (same provider name, instance, ref and
  incarnation): `tests/herdr_provider.rs` switches a state dir and keeps ids and tabs.

## Audit correctness fixes (2026-11-19)

- Supervised lifetime credentials now fence ordinary mutation/publication, provider facts/routes, command handlers,
  queued commits and native dispatch. Shared author names do not revive retired connections. Irreversible writer
  admission is ordered with retirement; an already-dispatched candidate is still adopted and reported honestly.
- Extension diagnostics are pipe-drained off the core mutex into a 1 MiB active log plus one 1 MiB backup,
  including within a single generation and overlapping cleanup. Failed logging discards output while draining.
- Resource-removal events preserve provider-reported reasons separately from mechanical retirement; effect events
  identify the addressed resource, native operation and exact native target without retaining input content.
- Regressions: `tests/extension_lifetime.rs` (red on the old main), `tests/commit_off_lock.rs` (queued versus active
  writer retirement), `tests/provider_wire.rs` (reason fidelity, all effect outcomes, administrative forget), and
  `src/ext_log.rs` (bounded rotation across overlapping writers). All tests use private fixtures, no model calls.

## Workspaces own their tabs (2026-11-19)

Human decision: each workspace has its own tabs; one shared current workspace across windows for now.
- Each workspace folder (and each git worktree) has its own layout `layout/ws-<id>` (id from the folder's real
  path), created on first open with one shell there and selected through `workspace/active`. `main` keeps
  terminals not in a workspace. Removing a workspace removes its layouts and their terminal views, never programs.
- Generic layout settings `doc/layout/<name>` = `{cwd?, title?}` (`core-protocol.md`, Workspace): `tabs.open`,
  `panes.split` and `pi.new` start in the layout's folder; they never read the workspaces extension.
- New layouts keep the active layout's panels around the tabs (the sidebar); `sidebar.toggle` applies to every
  layout. The sidebar lists each workspace's tabs, and other layouts (all of them with workspaces off); a click
  switches. Prefix `w` (tmux profile `s`), a tab-bar menu item and a status-bar segment open the switcher.
- Tests: `tests/test_workspace_tabs.py`, private e2e `tests/e2e_workspace_tabs.py` (in `check.sh full`).
- Per-window active workspace (later): see the report; switching goes through one function per extension.

## Each window has its own workspace (2026-11-19)

Human decisions: new windows open on the workspace last switched to; prefix R resets only that window.
- The TUI keeps the layout it shows as window state (like zoom), starting from `workspace/active`. It moves
  only on its own switches: a reply's `{"client": {"layout"}}`, `client.layout {name?}`, its own
  `vqro.layout.reset`, or its layout being deleted (fallback to the default). Re-exec keeps it.
- Switching still writes `workspace/active` (the default for new windows; agents/CLI and older clients).
- Every invocation carries `args.context.layout`; `vqro_ext` passes it to nested commands and offers
  `context_layout(args)`. tabs, sidebar.toggle and new workspace layouts act on the invoking window's layout.
- Per-window marks with generic fields: rows `current_when {layout}`, status items `label_context`.
- Tests: TUI unit tests, `tests/test_workspace_tabs.py`, `tests/test_vqro_ext.py`, two-window pty e2e
  `tests/e2e_tui_workspaces.py` (in `check.sh full`).
