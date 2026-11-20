# Decisions

> Renamed prim -> vqro on 2026-11-18; earlier records keep the old name.

> Decisions guide a **working prototype, and fluid**: each one holds until a later entry supersedes it.

Each entry is binding until superseded. Format: decision, why, consequences. Add new ones at the end; don't edit
old ones except to mark them superseded.

## D0. No coordination policy in core (2026-11-16)

**Decision.** The core exposes neutral mechanisms. Anything that decides *when* or *how* a participant learns
about or acts on a change (wait, notify, polling vs push, queueing, routing, wake) is an extension.
**Why.** Agent tooling is early; the owner wants to change the agent system quickly. herdr-core's `send`/`wait`/
notes show how fused policy locks users in (`research/review-1-backends.md`, finding 3).
**Consequences.** Core gets a cursored change log where poll and push are the same primitive; parameters such as
retention are explicit (D6).

## D1. Split process model: frozen supervisor + restartable core (2026-11-16)

> **Superseded by D14.** prim does not run its own supervisor or own process lifetime; backends do.


> **Partly superseded by D9:** the split (long-lived execution layer, restartable core) stands. Committing to build
> prim's own supervisor does not; the core depends on a lifetime contract that prim-sup or herdr-core may satisfy.
> The claim below that a supervisor crash kills its children was wrong (see D9).

**Decision.** A tiny, long-lived supervisor per machine owns child processes and buffers raw bytes. The core, adapters,
extensions and UI restart freely and reattach by byte offset. The supervisor protocol is meant to be frozen; prim is
partly defining a standard for itself. Spec: now `spec/execution-v0.md` (renamed, D9).
**Why.** A single daemon makes every core change kill agents, is a single point of failure, and accumulates policy.
The alternatives considered were a per-agent `prim-host` (repeats the external-supervisor pattern vqro retired,
`research/review-2-plan.md` finding 3), herdr-core as supervisor (pipes problem and coupling), and no persistence.
**Consequences.** The supervisor must stay minimal because a supervisor crash kills its children.

## D2. prim is not "a wrapper over any multiplexer" (2026-11-16)

> **Superseded by D14.** prim *is* a standardized layer over any multiplexer.


**Decision.** prim is a primitives layer over execution hosts (D9). Multiplexers are optional backends for terminal nodes.
**Why.** Headless agents need clean pipes and emit no OSC 7501, so backends provide agents neither I/O nor status
(`research/agents.md`, Findings 2; `research/review-2-plan.md` finding 2).

## D3. Backend order: herdr-core first; tmux, RMUX and Rex deferred (2026-11-16)

> **Superseded by D14.** Order revised: herdr-core first, a second backend early to expose leaks; Rex stays in scope as a backend.


**Why.** herdr-core has events with seq and `missed`, a status store, versioned docs and remote machines. tmux and RMUX
lose status under load, need separate dialects and reset pane IDs; Rex's protocol is undocumented, its API revision
moves weekly, and its terms are unchecked (`research/review-1-backends.md`, findings 1, 2, 8 and 9).
**Consequences.** Never write into the `herdr-app` document; prim uses its own `prim` document (finding 7).

## D4. Agent model is capability-based, not mode-based (2026-11-16)

> **Refined by D28.** Delivery modes are now declared capability contracts in `spec/agent-v0.md`; adapters still
> declare only native behaviour, and compositions stay in extensions, labelled.

**Decision.** Agent adapters publish native capabilities (`inject_at_boundary`, `enqueue_after_settle`, `interrupt`,
`answer_request`, `fork`, `resume`, ...). Core does not define a universal `deliver(mode)`. Extensions name and compose
delivery strategies, and hold any queues.
**Why.** "Steer" means at least three different things across runtimes, and emulating missing modes needs a queue,
which is policy (`research/agents.md`, Findings 1; `research/review-2-plan.md` finding 4).
**Supersedes** the `deliver(mode: boundary|settle|preempt)` draft in `research/agents.md`.

## D5. ACP is an adapter, not the core model; first adapters are pi RPC and Claude Code (2026-11-16)

**Why.** ACP has the widest reach, but its steering is unstandardised, v2 is a draft, and the Rust crate went from 3.6
to 5.3 in a month (`research/agents.md`, Protocols). pi and Claude Code are both installed, cover two different
input semantics, and Claude Code is the owner's main agent. Codex app-server is third (best documented steer).
**Risk.** Claude Code's mid-turn behaviour is undocumented; E1 measures it.

## D6. Change log mechanism parameters (2026-11-16)

**Decision.** The log is bounded; readers that fall behind get `missed` and re-read current facts; ordering is
guaranteed per node only. Facts are observed, not authoritative.
**Why.** Retention, backpressure and ordering are unavoidable choices; naming them as mechanism parameters keeps them
out of policy (`research/review-2-plan.md` findings 6, 7; vqro "Volatile events are not state").

## D7. Rust; TUI client first (2026-11-16)

**Decision.** Supervisor and core in Rust. The first UI client is a TUI (ratatui). A GPU client may come later as another
client of the same core protocol.
**Why.** The owner prefers Rust. A TUI is the cheapest way to reach daily use and works over SSH.

## D8. Trust is cooperative, with safe defaults (2026-11-16)

> **Refined by D28.** Answering is a coordination grant like any other; the workspace owner principal holds every
> grant by default.

**Decision.** prim's permissions are a coordination protocol, not a security boundary. Sockets are 0600, every effect
records its caller, and answering permission requests is reserved to humans unless explicitly granted.
**Why.** An agent with prim access could otherwise answer another agent's prompts (`research/review-1-backends.md`
finding 9; vqro "Coordination authority is cooperative").

## D9. Process-lifetime contract before supervisor implementation (2026-11-16)

> **Superseded by D14.** No execution host contract; backends declare capabilities through the adapter contract and prim never compensates.


**Decision.** The core depends on the execution host contract (`spec/execution-v0.md`), which states lifetime
guarantees explicitly (`survives_client_restart`, `child_dies_with_host`, `orphan_reclaim`, ...). M1 compares a
prim-owned supervisor with a herdr-core adapter against it before either is chosen.
**Why.** Building a supervisor is an irreversible commitment, and PTY recovery, process ownership and cross-platform
behaviour are hard. Children do not necessarily die with their parent: on Unix they are re-parented and may receive
SIGHUP or SIGPIPE or keep running. herdr-core already survives client and core updates (`research/backends.md`).
(Raised by the distillation response.)
**Supersedes** the supervisor commitment in D1.

## D10. Delivery has explicit failure states; no exactly-once (2026-11-16)

**Decision.** Every effect carries one of `recorded`, `dispatched`, `accepted`, `observed`, `indeterminate`.
`indeterminate` is never resolved by replay. Bounded buffers mean a long disconnect can lose history; prim reports the
gap instead of claiming reconstruction.
**Why.** Lost acknowledgements make general exactly-once impossible (distillation response; vqro "Delivery is not
acceptance" and "A crash ... does not justify automatic replay").

## D11. Agents may be headless or PTY-backed (2026-11-16)

**Decision.** An agent node is either headless (structured adapter) or PTY-backed (native TUI). Switching means ending
one process and resuming the session in the other. Resume restores saved history, not a turn in progress.
**Why.** A runtime's native TUI stays useful and not every runtime supports the same modes. This is also the
raw-terminal escape hatch (vision principle 8). (Distillation response.)

## D12. Contracts are platform-neutral; Unix ships first (2026-11-16)

**Decision.** No contract names a Unix-only concept: "local transport" (Unix socket or named pipe), `terminate` /
`kill` / `interrupt` (not signal numbers), PTY or ConPTY, process groups or Job Objects. Native Windows implementations
come later without contract changes.
**Why.** Unix assumptions that harden into a frozen contract cannot be removed later. (Distillation response.)

## D13. Programmable workspace is a co-equal goal (2026-11-16)

> **Superseded by D14.** There are no separate goals; coordination is workspace behaviour built from the same primitives.


**Decision.** prim's success criterion includes: *can an agent safely build and change meaningful workspace behaviour
(layout, views, input, commands, state, extensions) using only public primitives?* Execution and coordination are not
subordinate to it, but the workspace surface gets the same rigour (`spec/workspace-v0.md`, E0).
**Why.** The plan specified execution well and the workspace barely; a strong supervisor with a rigid TUI would miss
the point. The owner confirmed this is just as much the goal.
**Consequences.** Clients are renderers of workspace primitives (D7 stands, TUI first). Extensions carry workspace
behaviour as well as coordination policy.

## D14. The center: one lean, standardized primitives layer over any multiplexer (2026-11-16)

**Decision.** prim is one thing: a lean, standardized set of workspace primitives (nodes and views, layout, commands,
input and messaging, state, events) that humans and agents use to build and change their agent workspace, on top of
whatever multiplexer and agent runtimes are in use, through thin adapters. Coordination (messages, interrupts, routing)
is workspace behaviour built from the same primitives, not a separate goal.

**Backends own execution.** Processes, PTYs, persistence, recovery, remote machines and every lifetime guarantee belong
to the backend (herdr-core, Rex, tmux, RMUX, Zellij, ...). Each backend adapter declares what its backend can do as
capabilities (`spec/backend-adapter-v0.md`). prim exposes those capabilities and never compensates for missing ones,
and never runs its own process host.

**Why.** That execution work is hard and is what multiplexers such as Rex and herdr-core exist to solve. Taking it on
makes prim neither lean nor terminal-agnostic. Per-tool mods (Claude Code mods, cmux config) reshape one tool; a layer
across tools and multiplexers is still needed, and that layer is prim. The owner restated this center after the plan
drifted into supervisor design (2026-11-16).

**Judge every proposal by:** is it lean, is it standardized, and does it keep adding a new backend or runtime quick?

**Consequences.** `spec/execution-v0.md` is withdrawn. Headless agents run inside a backend terminal through an agent
bridge, so prim still hosts nothing. D10 (delivery states) stays as honest *reporting* of what a backend or runtime
says, not a guarantee prim builds.

**Tmux application (2026-11-17, owner-confirmed).** The prim-owned tmux server outlives its provider, including
safe mode, extension disable, crashes and core reinstalls. tmux supplies that native lifetime, not a prim watchdog.
Explicit, confirmed server stop is extension policy using a direct backend helper; configured external servers remain
untouched. See `research/tmux-server-lifetime.md` for controls and the legacy-server cutover warning.

## D15. Boundaries: resources and views, extension lifecycle, transactions, safe mode (2026-11-16)

**Decision.**
- **Backends** own execution, PTYs, process lifetimes and their actual guarantees. prim never fabricates durability
  or delivery semantics.
- **Adapters** normalize protocols, output, event ordering, identity and resize ownership, and handle buffering and
  recovery honestly without inventing guarantees.
- **prim** owns authoritative workspace state, composition, commands, bindings, interactions and the extension
  lifecycle. prim supervises **its own extensions** (launch, restart with a crash budget, disable); it does not host
  terminals or agents.
- **Resources and views have independent identities.** A resource (terminal, agent, ...) has an adapter-declared kind;
  any number of views can present it. The kind set is open.
- **Workspace mutations are transactions:** validated, all-or-nothing, versioned, recorded with author, revertible.
  External side effects (spawn, input, kill) report explicit outcomes (D10).
- **Safe extension replacement is foundational.** A broken extension, including one an agent wrote, must not destroy
  running work or lock the user out. The core has a small built-in, non-replaceable recovery surface (safe mode,
  disable extension, revert transaction, reset layout) reachable from every client.
- **Portability is a constraint from day one.** Develop on macOS; validate native Windows before locking any
  backend or extension contract.
**Refines** D14's "prim hosts no processes": prim hosts no *workspace* processes, but runs its own extensions.

## D16. Build prim now as a production-shaped vertical slice (2026-11-16)

**Decision.** No disposable prototypes. Every step builds a real prim component, a reusable contract, or a test that
stays valuable. Fake backends exist only as deterministic test fixtures. Start with the smallest complete, usable prim
(`build-plan.md`), dogfood it immediately, and grow it through real extensions and needs. Use a second, genuinely
different backend to challenge the abstractions, not to build a universal framework in advance. Remove or simplify
anything that proves unnecessary; keep tests and lessons, not bad code.
**Supersedes** the spike-first ordering in `test-plan.md`; its questions are answered inside the build.

## D17. Opinionated functionality ships as extensions (2026-11-16)

**Decision.** Anything that is a choice about how to work (opening shells as tabs, split/rename/close actions,
sidebars, settings menus, default layouts) is implemented as an extension using public primitives, even when it
is "base" functionality. The core and clients keep only mechanism (rendering, focus, mouse mechanics, forwarding
input, menus drawn from data, text prompts) and recovery (prefix, palette, undo/revert, safe mode, disable
extension, a neutral layout reset), which must work when every extension is broken.
**Why.** It forces prim to dogfood its own primitives, keeps every opinion easy to remove or replace, and is the
sidebar's model generalised. The owner asked for this explicitly.
**Consequences.** Needs a menu-contribution primitive (`menu/*` entries) and a set of default extensions
installed on first run.

## D18. Themes are data; clients own the palette mechanism (2026-11-16)

**Decision.** The active theme is one workspace entry, `doc/settings/theme` = `{name, palette: {token: colour}}`.
Clients resolve a small set of semantic tokens (`spec/core-protocol.md`, Themes) over a built-in default and draw
all their chrome from it; terminal content keeps its program's colours. Which themes exist and how one is chosen is
an extension (`themes`, shown in settings). Extensions never name colours: they use semantic styles, and a raw
colour appears only as data to show (a badge's `color`, e.g. a swatch).
**Why.** A theme is a choice about how to work (D17), but drawing is client mechanism, and prim must look right
with every extension broken. Tokens rather than a colour per element keep themes portable across clients (a GPU
client resolves the same tokens) and keep the set small enough to design for.
**Consequences.** A missing or unreadable token keeps the client's default and unknown tokens are ignored, so
themes and clients can evolve independently. A theme only recolours chrome. Recovery: clients ignore the theme in
safe mode, and a bad theme is undone by reverting its transaction (`prim.revert {txn}`; the default revert skips
extension changes, and `themes` writes as an extension) or deleting the entry.

## D19. Keys: a configurable prefix, binding priority, profiles as an extension (2026-11-16)

**Decision.** The prefix key is a client setting (`doc/settings/tui.prefix`) with a fallback to Ctrl-g. The
client's built-in prefix keys are default bindings to `client.*` commands. Bindings sharing a key are resolved by
`priority`, then the client's defaults, then entry id; a winning `client.none` unbinds a key. Keymap profiles
(tmux, screen) live in the `keys` extension.
**Why.** A keymap is an opinion (D17), but the prefix and how bindings combine are mechanism every client needs.
Priority lets a profile override an extension's key without deleting or editing that extension's entries, so
switching back, or turning the profile off, restores them. Deterministic tie-breaks keep the result independent of
load order.
**Consequences.** Recovery can't depend on any key: the prefix menu (and the `prim` badge that opens it) always
lists the recovery actions, keyed or not, and an unusable prefix falls back to Ctrl-g (D15). Live key feedback
uses terminal key-reporting extensions only while the prefix menu is open, so typing into programs never changes.

## D20. Icons are names; sets are data; clients own resolution and width (2026-11-16)

**Decision.** Extensions ask for an icon by name (`{"name": "repo", "text": "⎇"}`, or a list of names, most
specific first), keeping their own glyphs as the fallback. The active icon set is one workspace entry,
`doc/settings/icons` = `{name, width?, glyphs: {name: glyph}}`. Clients resolve a name through the set, then the
icon's own `nerd`/`text` by the client's `icons` setting, then nothing, and lay icons out in one column of the
set's width, measured in Unicode cells (`spec/core-protocol.md`, Icons). Which sets exist and how one is chosen
is an extension (`icons`, shown in settings). Rows, chips, menu items and status segments carry icons the same way.
**Why.** A set of icons is a choice about how to work (D17), like a theme (D18), but turning a name into a glyph
and keeping text aligned when glyphs differ in width is mechanism every client needs, and prim must look right
with every extension off. Names keep the choice in one place, so a set can restyle every extension at once
without any of them changing; fallbacks keep each extension readable when no set (or an older one) has the name.
A name list lets a set show an agent's state by shape, not only colour, while sets without state glyphs still
show the agent.
**Consequences.** With no set, an empty set, or in safe mode, every icon looks exactly as its extension drew it,
so adding the mechanism changed nothing for anyone who never picks a set. Unknown names and unusable glyphs
(empty, control characters, wider than two cells) fall back, so sets, extensions and clients evolve
independently. The `icons` setting (auto, Nerd Font, text) now only chooses among an extension's own glyphs; a set
needing a font says so in its description, since fonts can't be detected. Width follows the client's Unicode
tables; terminals that draw a glyph wider than its Unicode width (text-presentation symbols drawn as emoji,
ambiguous-width glyphs in CJK locales, non-Mono Nerd Fonts) will misalign, which is why the bundled emoji set uses
only emoji-presentation characters and a 2-cell column, and a roomy Nerd Font variant exists. Recovery: revert the
`icons` transaction, delete the entry, or pick Default.

## D21. Providers and explicit safe-mode recovery choices (2026-11-17, REVISED)

> **Human-confirmed safe-mode policy, 2026-11-17.** The provider migration continues; remote relay remains OPEN.
> The earlier proposal that privileged `essential` providers survive safe mode is withdrawn.

**Decision.**
- Backend adapters and agent-runtime bridges are **providers**: out-of-process programs speaking the core protocol,
  registered per connection with a generation, publishing resources with identity (provider, instance, ref,
  incarnation), serving effects with deadlines and D10 outcomes, and serving streams with credit that follows the
  viewer's consumption (`spec/provider-v0.md`). Adding a backend or runtime edits no core, server or client code.
- **Safe mode starts nothing:** no extensions or provider links run automatically. There is no `essential` flag.
  The built-in Rust recovery chooser lists declared extensions with descriptions and checkboxes, grouped into
  providers/backends (`kind: "provider"`, or remembered provider-registration owners) and UI extensions. The human
  chooses a minimal set, enables selected entries, or leaves safe mode. The prefix menu always reaches the chooser.
- Choices are a **session-only allowlist**, exposed in snapshots, not edits to `extension/<name>.enabled`. Explicit
  allowance may temporarily run a normally disabled declaration. Enter/re-enter clears choices; leaving restores
  ordinary enabled declarations. Stopping provider links never kills backend-owned terminals or replays input.
- **Remote bridges reach the core through their host's provider** (`provider.relay`, carried over the provider's own
  transport). Recommended default; the human confirms or picks an alternative (open question 2).

**Why.** D14 asks that a backend or runtime be a thin, quick addition; a built-in `Backend` enum in `server.rs` and
`main.rs` contradicts that, and two implementations per side must satisfy one contract before it is called stable.
The tmux check (2026-11-16) showed ids reused after a server restart and output dropped while paused, so identity
and gap reporting must be in the contract, not left to each adapter. An unknown broken extension/provider must not
be privileged during recovery: start empty, then let the human choose explicitly. The chooser itself cannot depend
on extensions. Relaying through the host's provider remains a recommendation, not a confirmed relay contract.

**Consequences.** Migration order in `spec/provider-v0.md` section 8: reshape in process, bounded streams, wire
protocol with an external fake provider and a conformance suite, herdr-core out of process, then tmux and the Pi
bridge in parallel. Provider declarations use `kind: "provider"`, `description`, and `default: true` for the default
spawn backend. `workspace.snapshot` exposes `safe_mode_allow` and remembered `provider_extensions` alongside
`providers`. `prim.safe_mode.allow {name,on}` is a built-in recovery command. `prim serve` now uses declared providers; explicit native test/embedder adapters stay stopped in safe mode and
have no chooser checkbox. Open: the exact `provider.relay` mapping.

## D22. The core endpoint is opaque and owner-only; stopping the core is a protocol request (2026-11-17, CONFIRMED)

> **Confirmed by the human, 2026-11-17; binding.** Applies D12 to two contract points found in the
> portability audit (`<private-notes>`, static, main 267a45a).

**Decision.**
- **Endpoint.** `PRIM_SOCKET` names an opaque local endpoint: an ordered byte stream per connection, reachable only by
  the user who started the core. On Unix it is a Unix socket path with mode 0600. On Windows it is a named pipe with an
  owner-only DACL that rejects remote clients. Clients, extensions, providers and bridges pass the value to their
  platform's connect call and never parse it. Messages are LF-delimited JSON-RPC in binary mode. Relays
  (`provider.relay`) carry this byte stream, never a path or handle. Stdlib Python on Windows, which lacks `AF_UNIX`,
  uses `asyncio`'s proactor pipe connection.
- **Stopping the core.** `core.shutdown` (and the built-in recovery command `prim.shutdown`) stops the core in
  order: extensions, connections, endpoint, exit. `prim stop` uses it. Pidfiles and signals are not part of the
  contract; Ctrl-C and SIGTERM remain extra local ways to run the same stop.

**Why.** Both were the only contract points that named a Unix mechanism: a socket path with file permissions, and
`prim stop` as pidfile plus `kill` plus SIGTERM. Everything else in `core-protocol.md` and `provider-v0.md` was
already neutral or needed wording only. Both are cheap to fix before the provider wire protocol freezes and awkward
after.

**Consequences.** Spec text updated (`core-protocol.md`, Endpoint and transport, Session; `provider-v0.md` sections 1,
3, 6 and 9). `core.shutdown` and `prim.shutdown` are implemented, including commit/reply draining. CLI stop is
protocol-first with a legacy no-answer signal fallback. Windows named-pipe transport and `prim_ext.py` on Windows
remain implementation work; confirmation of the contract is not a claim that Windows is implemented.

## D23. Multi-machine stays cheap, and no backend-specific remote transport (2026-11-17)

> **Confirmed by the human on 2026-11-17 for the last two bullets.** Platform priorities are not part of this
> decision: the human will set them when the roadmap is revisited after the current hardening and fixes.

**Decision.**
- **Platform priorities: to be revisited.** Other OS support (Windows native and WSL, macOS) and multi-machine are
  wanted; their order is decided after hardening. Multi-machine (prim on one machine driving another machine's
  terminals and agents) is a backlog feature.
- **Nothing now may make multi-machine hard.** Contracts and code keep these properties: the core endpoint stays local
  and owner-only (D22) and anything remote reaches it by forwarding that terminates locally; resource identity and
  provider names use permanent backend ids, never hostnames or a host's local aliases; facts and spawn arguments name
  machines neutrally (`machine`, `placement.machine`); a bridge is given a usable way to reach the core by the
  provider that places it, or the spawn is refused.
- **No backend-specific remote transport.** Remote agents reach the core by a backend-neutral mechanism, designed
  later. A backend's own channel may be one optional transport behind that neutral contract, never the contract.
  The herdr-core doc-mailbox relay is rejected; its design and spike stay as research only.

**Why.** The human wants prim on every OS they use, including WSL, before spreading across machines, and wants the
later multi-machine work to stay cheap. A relay built on one backend's documents would tie remote agents to that
backend, against D14 (any multiplexer) and D21 (providers are swappable). WSL is the first case of an agent on
another host (a WSL2 process can't open a Windows named pipe), so the neutral reachability rule is needed early.

**Consequences.** **Supersedes D21's earlier remote-bridges recommendation** (through the host's provider): `provider.relay` stays
OPEN and only as "carries a byte stream". Candidate neutral mechanisms, to be chosen later: a stdio connector
(`prim connect`, reached over ssh or WSL interop and given to bridges as `PRIM_CONNECT` argv), endpoint forwarding
started from the prim side (ssh `-R` of the local endpoint), or a backend byte channel as an optional transport.
`roadmap.md` lists the streams and constraints. Findings behind this: `platform-review.md` (scratch, static review of
main `b851d6c`).


## D24. Undo covers what the user started; agents may change anything (2026-11-17, CONFIRMED)

> **Refined by D28.** Default undo undoes the presser's own principal's changes; other principals' changes stay
> attributed and explicitly revertable; the inbox is outside global undo. Cooperative coordination grants now exist.

**Decision.** (1) A workspace change an extension makes because the user invoked its command (a theme, keymap or
icon set picked in Settings) is the user's change: the default undo (`u`) reverts it. Changes an extension makes on
its own (derived state, reconciling, reacting to events) stay outside the user's undo. (2) Agents may change
anything a user can (layout, commands, state, extensions); no grant system for now. D8 stays: permissions are
cooperative coordination, every effect records its caller.
**Why.** The human picks themes, keymaps and icons through extensions and expects `u` to take them back; the user's
intent, not the writer process, decides whose change it is. The human wants agents to drive the workspace fully.
**Consequences.** Command invocations carry who started them through to the extension's writes, so history can
attribute them; the mechanism is generic (any extension). Open question 6 (workspace authority) is answered.

## D25. Apache-2.0 for original prim material; DCO contributions (2026-11-18, CONFIRMED)

**Decision.** The core, bundled extensions, SDKs and protocol specifications, like all original prim material,
use Apache-2.0. Contributions use DCO 1.1 sign-off, not a CLA. The project distinguishes itself through its
name/branding, official builds and maintained extension repository, not copyright restrictions beyond Apache-2.0.
No trademark licence, registered-mark claim or invented distribution/repository URL is implied.
**Why.** The human chose a single permissive licence for the whole project and low-friction, accountable
contributions. Nothing is being published by this change.
**Consequences.** Root LICENSE is the official apache.org Apache-2.0 text, byte-identical; NOTICE identifies
Bennett Garcia. Bundled manifests/Cargo declare the established author/licence. Third-party terms remain their
own: offline SPDX policy and generated notices cover every Cargo metadata package, conservatively including
unbuilt optional, build, dev and target-specific packages (not a claim that all are shipped). Original notices are retained; missing archive
texts use clearly labelled standard text with actual declared metadata, never invented copyright holders.
The explicit dependency allowlist is documented in CONTRIBUTING.md; changes require maintainer approval.

## D26. Two agent tiers; backends keep agents alive, integration is optional (2026-11-18, CONFIRMED)

> **Refined by D28.** Terminal agents receive messages by pulling at safe points through a plugin or hook; vqro does
> not type into their terminal by default.

**Decision.**
1. **Terminal agents (default, every CLI agent).** A CLI agent runs in a backend terminal as standard, with its own
   interactive UI in the pane; the backend keeps it alive (D14). vqro needs no per-agent process for it. Integration is
   optional and comes from extensions on either side: a vqro-side extension using public primitives (status from
   OSC 7501 carried by the backend, `terminal.input` for actions) and/or a plugin/hooks inside the agent (e.g. a Pi
   extension, Claude Code hooks) that reports structured state using the existing runtime-token contract.
2. **Headless agents (opt-in, deeper).** For runtimes with an open protocol that can run headless (Pi / Pi durable and
   similar), vqro provides the interface (prompt, steer, abort, queues, requests). Not every agent needs this tier.
   Because nearly all headless protocols are stdio, a small co-located integration process is still required; it runs
   in the agent's terminal on the agent's host (so it survives vqro restarts and works remotely) and stays out of the
   core.
3. **Language of headless bridges.** Native (Rust, on `src/provider_client.rs`, like `vqro-herdr`/`vqro-tmux`), not
   Python, for memory/CPU at scale and platform reach. The Python provider SDK stays for extension authors and as the
   second implementation of the provider contract. No generic multi-runtime bridge until a second headless runtime.

**Why.** Lifecycle is already the backend's job; the bridge exists only for integration. Measured (bridge review,
2026-11-18): RPC through the backend PTY breaks (echo, CRLF, ~4 KiB line truncation, stderr mixing, ConPTY
re-rendering); at 200 agents Python bridges use ~2.6 GB private memory (Rust sketch ~57 MB); idle CPU was fixable
(114% -> 0% after event-driven idle). herdr already ships the tier-1 pattern (agent-side state reporting).

**Consequences.**
- Now: Python bridge hardened (isolated imports, no PYTHONPATH/supervision leak into the agent, event-driven idle).
- After hardening: tier-1 MVP as extensions (OSC 7501 status, optional in-agent plugin); reachability for agent-side
  plugins on other hosts/WSL follows D23 (neutral, no backend-specific transport).
- With the platform work: Rust `vqro-bridge-pi` replacing the Python bridge (~6-8 h); bring the Rust provider SDK to
  parity (resume token, error data, dispatched).
- Evidence: the bridge language review (2026-11-18, scratch), `docs/research/bridge-p2.md`.

## D27. Project name: vqro (2026-11-18, CONFIRMED)

**Decision.** The product name is **vqro**, lowercase. Rename current binaries, crate/SDKs, environment, paths,
commands and documentation now, before publication. A one-time offline copy migration replaces old install/state
names; the runtime has no long-term old-name aliases. Earlier decision/research records stay as written.
**Why.** The human chose the final name while nothing is published.
**Consequences.** Brand identifies the project, official builds and maintained extension repository under D25;
Apache-2.0/DCO terms do not change. No registered-mark claim or public repository/distribution URL is implied.

## D28. vqro's own agent standard: inbox, delivery modes, grants, restart (2026-11-19, CONFIRMED decisions; spec draft)

**Decision.** The human's binding decisions for the agent standard (`spec/agent-v0.md` and `spec/coordination-v0.md`,
drafts; conformance in `spec/agent-v0-conformance.md`):
1. **vqro's own standard** is the baseline: core + modules + conformance, no per-protocol profiles, no external
   baseline. A declared subset is at least equivalent to ACP, measured against a pinned, dated ACP v1 + v2-base
   snapshot by an executable coverage matrix, not asserted.
2. **Per-agent inbox.** Any principal with the right grant can send to any agent. Delivery modes `start`, `boundary`,
   `settle`, `preempt` and `interrupt`, plus the weaker `admit`, are declared capability contracts: adapters declare
   only native behaviour; compositions (preempt as interrupt + start) live in extensions and are labelled with their
   steps; no silent fallback to another mode. No replay (D10).
3. **Restart.** After a vqro restart agents come back paused and unsent mail is held; resume is explicit and
   cascading (workspace, project, team = coordinator and everyone under it, or one agent). A message caught mid-send
   stays `unknown` and is never resent.
4. **Undo.** `u` undoes the presser's own changes. Other principals' changes stay attributed and explicitly
   revertable. The inbox is outside global undo: withdraw or edit while pending, "too late" after dispatch.
5. **Grants are coordination, not security.** No human/agent split; grants are additive; each call declares its
   principal (cooperative, D8); the workspace owner principal holds every grant by default. Sandboxing and operation
   permissions are out of scope.
6. **Terminal (tier-1) agents pull** their next message at safe points through a plugin or hook. vqro never types into
   their terminal by default; guarded typing is an optional extension.
7. **Adapters in Rust** (D26). No MCP.
8. **Shape:** A's presentation, C's evidence ladder (recorded → claimed → dispatched/accepted → inserted/handled →
   completed, plus `admit`; requests with revision and first valid answer), B's exact ids + handles (resolved once;
   the message pins its target; successors by an explicit recorded handover), additive grants, per-viewer credited
   item streams with fenced snapshots. The inbox claim lives in the message record, is written outside undo history
   and is durable before the runtime is invoked; the inbox has its own byte budget.

**Why.** No existing standard covers in-session delivery, a multi-sender inbox, honest evidence and agents that outlive
their viewers (agent-spike 2026-11-19: three independent proposals, adversarial reviews, two panels, adjacent-domain
scan, lessons from the Herdr mailbox and old vqro). All three proposals kept dispatch evidence in revertable
workspace state under global undo; connection ids restart every boot; typing into an agent's terminal corrupted the
human's draft in the Herdr fork.

**Layering (2026-11-19, after the independent audit).** The human wants both standardised: what an agent integration
exposes, and how vqro coordinates agents. They are two layered standards with separate audiences, not one fused
document and not a demotion. `agent-v0.md` is the **agent integration contract** for adapter authors (endpoint model,
capabilities, facts, evidence, item stream, send, interrupt, requests, modules, lifecycle evidence) and says nothing
about inboxes, grants, names, teams or pause. `coordination-v0.md` is **vqro's coordination standard** (inbox,
delivery strategies, pull, handover, pause and scope resume, principals, names, membership, grants, tombstones,
control), normative for vqro and implemented by a default, replaceable extension on generic core primitives; the core
has no inbox, team or grant special case. Conformance has a part for each.

**Refines.** D4: modes are declared capability contracts, compositions stay in extensions (no emulation, no core
policy). D8: answering is a grant like any other; the owner principal holds all grants by default. D24: (1) default
undo selects the presser's principal, not every client; (2) a cooperative grant system now exists for coordination.
D26: tier-1 agents receive messages by pull, not by `terminal.input`.

**Consequences.** The specs are drafts. Prototypes P1–P6 (inbox writer seam, principal + undo, item streams, Pi
semantics, pull at safe points, ACP matrix) and the pause/resume design are folded in; the remaining open points are
marked with the implementation stream that settles them (C1 budgets, C2 connection-default principals, C4 identity
reservation, K2 the default coordination extension). Four generic
core additions follow (item channel on streams, a protected inbox namespace that ordinary transactions and reverts
cannot reach, per-call principal, identity reservation for tombstoned agents). Handles identify and confer no rights;
rights come only from the owner rule and explicit grants. ACP is pinned at commit `d8cd473e` (2026-11-19) and claimed
row by row. Team membership is a recorded forest (project, reports_to). Agent lifecycle: a `stop` control, distinct
from interrupt (the turn) and pause (the gate); an endpoint is `ended` only on observed native end evidence from the
runtime or a host that declares it owns the endpoint's lifecycle (D10, D14). An explicit retire is a separate
`retired` state: routing off, cleanup and handover allowed, native execution unknown, never a claim that the agent
stopped. Silence is `disconnected`, then after a declared grace (60 s) `unavailable`. Each inbox message gets a
globally unique delivery id at admission, which is what crosses the runtime seam. `provider-v0.md` section 6 ops are
replaced when the Pi bridge is ported.

**Further implementation rulings (2026-11-19, spec batch 2).**
- Tier-1 `agents.status {terminal, app, state, seq}` is lossy advisory live data, like OSC 7501, for hooks without
  a terminal status path. Reports use last sequence per terminal and disappear on terminal end/removal or extension
  restart. The sidebar uses `agents.terminal_status` only when the terminal has no OSC status; unknown stays unknown.
  Reporter ownership `(pending C2)`: only the terminal's launcher-tagged principal or owner may report; others get
  `not_authorized` and nothing is applied. The current unattributed command is a known v0 gap. Reports are bounded
  by 1024 terminals and 960 KiB encoded data, printable app length 1–64, terminal id length ≤ 64, and seq 0..2^53−1;
  refusal records nothing. Serialized fresh-snapshot publication prevents an older table overwriting a newer one.
- Every `agent.stop` reply's data includes per-call `covers`, never wider than declared. When the adapter cannot
  evidence effective coverage, `covers_evidence: "requested"` marks requested scope only, not proof of stopped work.
- Agent-view facts may carry runtime-reported usage (context, tokens/cache, cost with currency and reported/estimated
  basis), session cwd and read-only model/effort. Tools/runs may carry start/end times and messages an `at_ms`;
  absent values stay unknown, never inferred. Text format is per block (`plain` default or `markdown`, fallback
  plain). Runtime/vqro notices use message role `system`, not `coordination`; send authority is unchanged.
- Inbox priority is `low | normal | high` (default normal), editable pending only. Per target/lane, highest
  eligible priority wins across senders, then sender round-robin, then equal-priority per-sender FIFO. It never
  bypasses lanes, gates, quotas, controls or an existing claim; lower priorities may starve under sustained higher
  traffic, with no aging in v0. This refines the earlier unconditional per-sender FIFO/fairness promise.
- Pull `max_bytes` is a per-record consumer budget; oversized mail stays held `too_large_for_consumer` for the agent,
  like unsupported content, without claim/release churn or truncation. Aggregate batch bounds still apply.
- Slow-viewer memory accounting includes journal `retain_bytes + snapshot_buffer_bytes`, old shared entity
  versions pinned by snapshots, current entities and other overhead; journal retention alone is not the bound.

**Further implementation rulings (2026-11-19, spec batch 3).**
- Optional usage on a run or native thought message uses the facts usage shape, only when the runtime reports it;
  absent means unknown. A reported context window alone is `{window}`, not invented occupancy: `used` may be
  absent and is never derived from last-turn token totals.
- Generic causality may come from `resource.invoke` root params or args; dual forms must match or reject invalid.
  The core lifts bounded cause/correlation into effect metadata/audits with args unchanged, required typed
  qualifiers, and provenance/unknown bounded fields preserved exactly; no inference or provenance upgrade.
  Cause ≤ 4 KiB, known strings ≤ 256 bytes, depth ≤ 64; correlation is non-empty and ≤ 256 bytes.
- Authorization of `doc/principals/*` writes uses the connection's actor, not its effective/delegated principal;
  delegation/callbacks cannot extend another principal's delegation or form chains. A hand-started agent inherits
  its human-opened shell's `VQRO_PRINCIPAL` by design; the launcher supplies a tagged agent principal.
  Callback proofs use `invocation`; the legacy `on_behalf_of` proof form is refused (explicit principal delegation
  through `on_behalf_of: P` remains).
- `extension:`, `host:` and `system:` principals are core-assigned, never asserted at hello (`invalid`). Backend
  and supervised child processes do not inherit caller identity variables. A missing/invalid owner row gives the
  TUI `owner-unset` without coordination grants, with a visible bootstrap notice and raw/recovery still usable;
  no fallback guesses owner authority.
- Request routing is ordinary attributed agent configuration: human, recorded-coordinator inbox, or both;
  launchers record defaults. The exact `reports_to` coordinator must hold answer; absent/unavailable/ended or
  ungranted routing falls back visibly to human. Route never auto-approves or weakens native response fences.

**Further implementation rulings (2026-11-19, spec batch 4).**
- An owner invocation does not lend its actor's authority to an extension: owner/other-principal delegation rows
  and the owner key are managed directly by the owner through TUI/CLI in v0. Extension-owned rows keep the
  extension actor's existing ownership rights.
- An unknown-predecessor stall is operator-visible: held `ordering_unknown` includes the blocking delivery id in
  views and bounded lookups/lists. Visibility is not resolution, negative evidence or permission to replay.
- Pull points form a map with per-point `precision: exact | approximate`; `pull.framing` uses the existing
  role/frame/authority descriptors per safe point. Fixed native framing (including `NOT USER INPUT` on Claude
  rewake) is declared/shown, never silently upgraded. Approximate idle must preserve the record's boundary/settle
  contract if a human prompt wins the race; it never authorizes a start that assumes idle.
- `caps.tool_principal: "env"` promises each tool-running process receives its own endpoint principal and socket
  explicitly. Multi-endpoint hosts need native per-endpoint/thread environment support or a child per endpoint;
  otherwise the capability is absent and G11 records visible host attribution, never guessed or emulated tags.

**Human answers (2026-11-19, CONFIRMED).**
- **AS1, pause:** "vqro stops delivery": paused means vqro sends, hands out and wakes nothing until resumed; an agent
  still working shows as working; a runtime with a real pause may offer it as an extra capability. Physical
  suspension is not promised.
- **AS2, MCP:** "State the gap openly": no MCP pass-through; the ACP claim is v2 base plus v1 except MCP, with the gap
  stated; session connection descriptors are not part of v0.
- **AS3, second implementations:** "Do both in parallel; install is fine": Codex app-server and Claude Code (hooks),
  both real runtimes, after Pi; an ACP fake is a fixture. Each adapter's definition of done includes a small
  real-runtime smoke run on a cheap model (about $20 in total approved, smoke only, logged). This settles D5's
  sequencing question for the standard.
- **AS4, disappeared agents:** "Keep mail + identity": durable evidence in the protected domain (membership stays
  ordinary attributed workspace configuration, only referenced from there), a separate bounded and evictable
  identity-reservation cache (eviction only means a return is a new agent), and
  refusal of new admissions at the durable budget; evidence is never dropped silently.
- **AS5, size:** "Accept the split": two standards, no length target.
- **AS6, principals:** "Default from connection": the principal defaults from the connection vqro already knows (the
  TUI is the owner, launcher-tagged panes are their agents), a declared label is recorded alongside and grants
  nothing, and acting for another principal needs an explicit delegation (a core row
  `doc/principals/delegations/<id> = {from, to, until_ms?, note?}`, exact principals, written only by actor `from`
  or the owner actor, never merely by an effective/delegated principal). The owner is `doc/principals/owner`,
  written only by the owner actor after an audited bootstrap. A live command
  invocation proof carries the invoking call's effective principal and original initiator separately, for that
  callback only. This refines decision 5's "each call declares its principal".

**Implementation rulings (2026-11-19, spec batch 1).**
- Handle selectors in grants resolve once at issuance: targets to exact agents, subjects to their bound principals;
  rebinding transfers nothing. Team/project selectors remain dynamic over recorded membership.
- The operation window caches well-formed refusals as well as successes; changed conditions need a new op id.
  Malformed and undeclared-principal calls take no slot; a full window refuses `busy` before evaluation.
- Pull takes declare accepted content types; unsupported mail stays held for that consumer rather than churning
  through claims. Counts-only wakes include a revision so edits cannot disappear behind unchanged-count suppression.
- The default coordination extension declares **4 MiB** (generic core default 2 MiB). C1's measured 4 MiB workload
  passed 9000/9000 at 100 txn/s (operational p99 32–36 ms, layout p99 42–48 ms); 8 MiB is not certified at that rate.
  The capacity table charges the fixed 64 KiB domain metadata before payload space.
- Pi dialogs use distinct operation scopes and `background_requests: false`; answers stay `dispatched` without
  native acknowledgement. A positive native relative timeout cannot yield an evidenced absolute expiry: cancel
  once as unrepresentable instead of guessing; untimed dialogs remain answerable.
- Item adapters declare snapshot coverage (`all_retained`, `tail`, `state_only`). Appends must also fit their
  resulting whole snapshot entity; the shared producer refuses `too_large` without changing state.
- An observed exit from the bound owning host terminal makes the endpoint `ended` and allows retention cleanup;
  `retired` remains an explicit administrative judgement, never inferred from exit or silence.
- Claude Code hook hand-offs are context (`supplied`, not `inserted`); `read` needs model-request and normal
  completion evidence, never hook success. Idle `asyncRewake` is interactive-only, with native `NOT USER INPUT`
  framing distinct from turn/settle context.

- **AS10c', request routing:** "Yes, add it." Each agent routes requests to `human` (notice/popup), `coordinator`
  (inbox mail to its recorded `reports_to` endpoint holding `answer`) or `both`. Default human for owner-started
  agents, coordinator for coordinator-launched agents; the launcher records it. The owner or a configure holder
  may change this ordinary attributed configuration. An absent/ended/unavailable/ungranted coordinator falls
  back to human with a visible reason. Other answer holders may still answer; first valid answer wins with the
  same epoch/revision/native expiry fences, never auto-approval or renewal.

Nothing is stable until Pi and the two other real runtimes pass conformance.

## D29. One declaration model for extension dependencies and client UI features (2026-11-19, CONFIRMED)

**Decision.**
- Extensions and clients declare what they **provide** and what they **need** or merely **work with**, as
  capabilities `kind/name[@major]`: commands, views, live data, workspace documents, abilities ("hosts terminals")
  and client UI features. Dependencies name **what is provided**, with a suggested provider for enabling or
  installing; any provider satisfies them. Shared data counts as a dependency. Command dependencies are **per
  command** (`command/tabs.show`), not per group.
- The core resolves and **reports** (satisfied, off, missing, other version; reverse users). It never enables or
  disables anything by itself; enable/disable replies say what stays unmet or breaks.
- Turning an extension on **offers to enable** its off needs in one confirmation. A need with no installed provider
  is a warning, not a refusal. Turning off a depended-on extension **warns what breaks** first; whoever may disable
  extensions may do it (everyone today, D24; with the agent standard's grants, whoever holds that grant).
- Clients declare **profiles** (`base`, including recovery, `terminal`, `keys`) and optional namespaced **features** in
  `hello`. Optional UI features need an in-content fallback; recovery never depends on any of this. The built-in
  safe-mode chooser only shows "needs X" text; the offer lives in Settings.
**Why.** The human asked for one "declare what you provide/need, with fallbacks" model for dependencies and UI
features (2026-11-19), and chose per-command granularity, allow-with-warning and the minimal chooser.
**Consequences.** Manifests gain `provides`/`needs`/`works_with`; `extension.list` gains `dependencies`; a release
check (`scripts/check_manifest_deps.py`) keeps bundled manifests honest. Design: `research/declarations-and-ui-clients.md`.
The OpenTUI client stays optional and separately installed, never bundled (human, 2026-11-19, Q-UI-5;
`research/opentui-client-trial.md`).
