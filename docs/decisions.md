# Decisions

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

## D21. Adapters and runtime bridges are out-of-process providers (2026-11-17, PROPOSED)

> **Proposed, not binding** until the human confirms it. Until then D14–D20 stand and the in-process adapter stays.

**Decision.**
- Backend adapters and agent-runtime bridges are **providers**: out-of-process programs speaking the core protocol,
  registered per connection with a generation, publishing resources with identity (provider, instance, ref,
  incarnation), serving effects with deadlines and D10 outcomes, and serving streams with credit that follows the
  viewer's consumption (`spec/provider-v0.md`). Adding a backend or runtime edits no core, server or client code.
- Providers marked `essential` keep running in **safe mode**, so recovery never makes every terminal unavailable. They
  can still be disabled explicitly, and the recovery surface never depends on them.
- **Remote bridges reach the core through their host's provider** (`provider.relay`, carried over the provider's own
  transport). Recommended default; the human confirms or picks an alternative (open question 2).

**Why.** D14 asks that a backend or runtime be a thin, quick addition; a built-in `Backend` enum in `server.rs` and
`main.rs` contradicts that, and two implementations per side must satisfy one contract before it is called stable.
The tmux check (2026-11-16) showed ids reused after a server restart and output dropped while paused, so identity
and gap reporting must be in the contract, not left to each adapter. Safe mode exists for recovery; dropping the
providers would remove the terminals the user is trying to recover. Relaying through the host's provider keeps
bridges independent of where they run, without a separate prim process per host.

**Consequences.** Migration order in `spec/provider-v0.md` section 8: reshape in process, bounded streams, wire
protocol with an external fake provider and a conformance suite, herdr-core out of process, then tmux and the Pi
bridge in parallel. `extension/<name>` gains `essential`. `workspace.snapshot` gains `providers` (replacing
`adapters`). Open: the exact `provider.relay` mapping onto `hc connect` channels and ssh.

## D22. The core endpoint is opaque and owner-only; stopping the core is a protocol request (2026-11-17, PROPOSED)

> **Proposed, not binding** until the human confirms it. Applies D12 to two contract points found in the
> portability audit (`~/.local/share/herdr/scratch/prim-independent-review/portability-audit.md`, static, main 267a45a).

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
3, 6 and 9). Implementation pending: `core.shutdown` and `prim.shutdown`; a transport module with a Windows
named-pipe arm; `prim_ext.py` on Windows. Known Windows runtime work is listed in `build-plan.md` ("Windows work").
Unix behaviour is unchanged.

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

**Consequences.** **Supersedes D21's third bullet** (remote bridges through the host's provider): `provider.relay` stays
OPEN and only as "carries a byte stream". Candidate neutral mechanisms, to be chosen later: a stdio connector
(`prim connect`, reached over ssh or WSL interop and given to bridges as `PRIM_CONNECT` argv), endpoint forwarding
started from the prim side (ssh `-R` of the local endpoint), or a backend byte channel as an optional transport.
`roadmap.md` lists the streams and constraints. Findings behind this: `platform-review.md` (scratch, static review of
main `b851d6c`).
