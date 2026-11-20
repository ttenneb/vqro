# vqro coordination standard, v0 (draft)

> **Status (2026-11-19): draft, not implemented** (D28). vqro is a working prototype, and fluid. Prototype results
> P1–P6 and the R design are folded in. Open points are marked with the implementation stream that settles them:
> `(pending C1)`, `(pending C2)`, `(pending C4)` or `(pending K2)` (the default coordination extension). Nothing is
> stable until it passes Part B of `agent-v0-conformance.md` with real runtimes behind it.

**Audience: whoever coordinates agents through vqro**, and authors of a replacement coordination extension. This is
how vqro delivers messages to agents and organizes them: inbox, delivery strategies and composition, pull, handover,
pause and scope resume, principals, names, membership, grants, tombstones and control. It is normative for vqro and
**implemented by a default, replaceable extension** on generic core primitives (a protected operational domain,
attribution, streams; §9). The core has no inbox, team or grant special case: another extension may replace this one
using the same primitives. The default extension's id is `coord`, so its commands are `coord.*` (D29 owner prefixes);
a replacement uses its own prefix with the same semantics. Agents are reached through the integration contract
(`agent-v0.md`); this document never asks an adapter for more.

## 1. Shape

| Part | What |
|---|---|
| Model (§2) | ids, coordination stages on top of the adapter's evidence |
| Principals, names, membership, grants (§3, §4) | who acts, how agents are named and grouped, who may do what |
| Inbox (§5) | durable per-agent messages: send, edit, withdraw, claim, retry, order, fairness, composition, handover, pause and scope resume |
| Control and lifecycle policy (§6) | interrupt, answers, module ops, stop, retire; unavailable agents and tombstones |
| Pull (§7) | tier-1 agents and plugins take messages at safe points; vqro never types into a terminal by default |
| Causality (§8), core additions (§9), bounds (§10) | |

## 2. Model

| Id | Meaning |
|---|---|
| `delivery` | one inbox message; globally unique, minted at admission; the id passed to `agent.send` and that keys receipts, lookups and replies |
| `submission`, `op` | caller-chosen per principal, for idempotency only: one send / one coordination call (§5.3) |
| `control_epoch` | core boot + inbox-owner incarnation (§5.5) |
| `principal` | coordination identity (§3) |
| `@handle`, `project:<id>`, `team:<rN>` | names and scopes (§4) |

Coordination stages come before the adapter's evidence (`agent-v0.md` §2.2):

| Stage | Means |
|---|---|
| `recorded`, `held` | intent saved, editable; `held` carries a reason (§5.6) |
| `claimed` | durable claim: the runtime may have been invoked |
| `unknown` | outcome lost; a fact about evidence, not proof either way; never resent automatically (D10) |
| `withdrawn`, `expired`, `superseded` | ended before claim; never delivered |

## 3. Principals

Accident-proofing, not security (D8): any same-user process can still bypass it, but ordinary use never misattributes.
This section is the one definition both standards use; `core-protocol.md` adopts it when C2 lands. The core owns only
the generic keys below (`doc/principals/…`) and never reads coordination grants (§4).

**Principal-id domain.** Principal ids are opaque, case-sensitive strings of **1–256 UTF-8 bytes**, with no
whitespace, Unicode `Cc` controls or the pinned **Unicode16 `Cf`** format characters used by the core's shared
text-control guard. Ordinary combining/variation characters are neither stripped nor normalized; names compare
exactly, without fallback. An invalid legacy owner name remains unresolved/`owner-unset`, never normalized or
replaced implicitly; only the explicit guarded owner repair below can establish a valid owner.

**Recorded fields** on every transaction, event and `effect.invoke` (P2 built the first four as
`principal`/`actual_author`/`on_behalf_of_principal`; identifiers ≤ 256 bytes):

| Field | Meaning | Set by |
|---|---|---|
| `author` | the actual connection (`client:<epoch>:<id>`, `ext:<name>`); never settable by a request | core |
| `actor` | the connection's resolved principal (below) | core |
| `principal` | the **effective** principal: `actor`, or the `on_behalf_of` target under a delegation, or the effective principal a callback proof carries | core |
| `initiator` | the original invoker's principal, carried unchanged through delegation and callbacks | core |
| `label` | a label the caller declared; recorded only, never authority | caller |

- **Resolving `actor`** (the human's choice AS6) `(pending C2)`: the TUI is the owner principal; an extension is
  itself; a pane vqro spawned is the principal its launcher injected. A host that serves several endpoints (an
  app-server) passes a per-endpoint tag to each tool it runs where the runtime evidences which endpoint ran it;
  otherwise a verified host-context route may bind the host's own principal (`host:<rid>`), never one of its
  endpoints by guess. A call with nothing known is `undeclared`: accepted for raw and recovery primitives, never the owner, no coordination
  grants, no default `u`. An agent started by hand inside a human-opened shell inherits that shell's principal
  (`VQRO_PRINCIPAL`, possibly the owner) by design; its process or app name does not retag it. Use the launcher
  to obtain a launcher-tagged agent principal. Principal ids beginning `extension:`, `host:` or `system:` are
  core-assigned only; asserting one at `hello` is rejected `invalid`. Backends and supervised children never
  inherit `VQRO_PRINCIPAL` or other caller-identity variables from the parent environment; launcher tagging is
  explicit, not ambient identity inheritance.
- **Launcher-assignment observation.** A terminal's `resource.principal` is a core-recorded launch observation,
  not its current process identity or an authority/authentication proof. An explicit native `VQRO_PRINCIPAL` value
  stays opaque and unchanged in the environment; it is recorded as a known assignment only through the shared
  `validate_assertable` guard (including the id domain above and rejection of reserved namespaces/markers and their
  case variants). Exact `undeclared` is the explicit no-identity exception, not authority. An invalid/reserved
  explicit value leaves the observation null, never a guessed replacement principal. Provider/OSC/child reports
  and opaque `data.principal` do not populate or overwrite it. Assignments are in-memory: after **every core
  restart**, existing terminals have unknown/null assignments until relaunch or a new observed launch assignment.
  No surviving process environment, restored registry/provider fact or child report backfills them; no new
  persistence or lifetime guarantee follows from the field.
- **Tool-process attribution** `(pending C2)`: the route is the environment of the process running the tool.
  For tier-1 agents, the launcher tags the agent's pane with `VQRO_PRINCIPAL`, so a tool's `vqro` CLI call
  inherits that agent principal. Headless adapters declare `data.agent.caps.agent.tool_principal: "env"` only
  when each endpoint's
  tool processes receive its own launcher-tagged principal and `VQRO_SOCKET` explicitly. The agent principal
  is a launcher-assigned id independent of `rN`. A single-endpoint host passes its own launcher-given principal
  and socket to its runtime child; an inherited owner principal from a human shell is not this route.
  A many-endpoint runtime needs native
  per-endpoint/per-thread environment support (or a child per endpoint), not one shared principal for all tools.
  Without that native route, the capability is absent: host attribution, where verified, remains a visible
  per-endpoint gap, never guessed as one endpoint. **Opt-in host context:** a provider serving many endpoints may
  explicitly export a verifiable host token to tool child processes it launches. The core verifies this context
  like an extension token and binds their CLI calls to its core-assigned `host:<rid>` principal. This is not a
  `hello` identity assertion and does not satisfy per-endpoint `tool_principal: "env"`. Human-opened shells never
  inherit the host token; without a verified context or another connection-known identity the call remains
  `undeclared`. G11 selects its shared-host branch on this verified opt-in route, not a caller's claimed host id.
- **Callback proofs** travel in the `invocation` request field (an opaque proof). A live invocation proof captures
  the invoking call's effective principal and its original initiator separately; writes made under the proof carry
  that effective principal and that initiator. Example: A holds a B→A delegation and invokes `on_behalf_of` B; the
  handler's writes have effective principal B, initiator A (and `author`/`actor` the handler's extension); `u` by B
  undoes them, `u` by A does not. Propagation is scoped, as in D24: it lasts only for that callback and gives the
  handler no standing ability. Only an explicit `on_behalf_of` needs a delegation row. The legacy
  `on_behalf_of: <callback proof>` form is refused: callbacks use `invocation`; `on_behalf_of` names only an
  exact principal under the delegation rule below.
- **Delegation** `(pending C2)`: `on_behalf_of` carries only the target principal P. The core admits it only if a
  row `doc/principals/delegations/<id> = {"from": P, "to": <actor>, "until_ms"?, "note"?}` exists and is unexpired:
  `to` may act on behalf of `from`. Exact principals only: no wildcards and no chains in v0. Rows are ordinary
  attributed configuration. The core records `actor`, `initiator` and the effective `principal` P; what P may do is
  still checked by the coordination extension's own grants. A row carries no per-right or per-target scope in v0: `to`
  acts as `from` for everything until `until_ms`. Scoped delegation is a possible later extension.
- **Write ownership** for `doc/principals/*` is enforced by the core at commit prepare against the state before
  the transaction and the request's fixed **actor** (the connection's own principal), never its effective/delegated
  principal; creates, replacements, deletes and reverts alike. Ordinary writes of `doc/principals/owner` require
  the owner actor; the explicit owner-unset bootstrap below is a separate guarded system transaction.
  For `doc/principals/delegations/<id>`, a create needs the actor to be the new row's `from` (or the owner); a delete
  or revert needs the existing row's `from` (or the owner); a replacement needs both existing and new `from`
  (or the owner), so only the owner actor can change `from`. A delegation or callback proof never lets a delegate
  extend another principal's delegation or create a chain. Rows written in a transaction grant nothing within it.
  Initializing the owner is an audited system bootstrap, not a `u` target.
- **Managing principals in v0.** An extension command invoked by the owner is still the extension's actor. The
  owner's invocation gives it no authority to create or change the owner key or delegation rows whose `from`
  is the owner (or any principal other than that extension). The owner manages those directly through TUI/CLI,
  not by lending authority to an extension command. Rows whose `from` is the extension's own principal remain
  governed by the actor-ownership rule above.
- **Times** are integer Unix epoch milliseconds in fields ending `_ms`, as in the core (`until_ms`, `expires_at_ms`,
  `last_seen_ms`).
- **`u` acts for the presser's effective principal** and selects the latest transaction whose `principal` equals it:
  delegated and forwarded changes are undone by the principal they were made for, never by the delegate's or the
  extension's `u`; others' changes stay attributed, explicitly revertable and CAS-protected (D28, refining D24).
  Inbox state is never in global undo.
- The **workspace owner principal** is `doc/principals/owner = "<principal>"` and holds every coordination right;
  other rights come only from grants (§4). If that row is missing or invalid, the TUI acts as `owner-unset` with
  no coordination grants. Raw/recovery primitives stay usable (D15) and a visible notice requests audited owner
  bootstrap; no owner is guessed from a connection, environment or process. The audited startup bootstrap stays.
  An explicit public owner-bootstrap request may supply a principal to repair a missing/invalid owner row, but
  only while that row is still missing/invalid at serialized commit prepare. This is an audited **system
  transaction**, not an ordinary caller write or a `u` target. A request prepared after a valid owner exists is
  refused without changing it; concurrent requests cannot both establish an owner. The request supplies the
  principal explicitly: neither startup recovery nor this repair guesses one.

*Evidence (P2, `proto/principal-undo` f12f4e2, gate 29/29):* a pane's CLI writes as Alice through the launcher
environment; Alice's TUI `u` removes it while Bob's later change stays; another principal's change is revertable by
explicit id; restart keeps principals and grants; history is unaffected (one entry, 202 bytes, after 900 unknown
assertions); grant lookups over 1024 grants p99 3.6 ms in a debug build. The connection-bound default, the separate
label and the delegation rows are not built yet.

## 4. Names, membership, grants

**Names.** Handles (at most 4 per agent): `@` + 1–3 `/`-separated segments of `[a-z0-9][a-z0-9-]{0,31}`;
`doc/agents/names/<handle> = {agent, principal?}`, changed through `coord.names` (right `names`). A handle identifies
and confers no rights. It is resolved once when an action starts; records keep the exact `rN`. A handle whose agent
ended or was retired resolves to `ended` or `retired`, and new sends are rejected until it is rebound.

**Membership** `(pending K2)`. `doc/agents/members/<rN> = {project, reports_to}` is the one membership authority:
ordinary attributed workspace configuration, changed through `coord.membership.set` (right `membership` on the moved
subtree and its source and destination), which validates the whole result in one write. Like any ordinary change it is
undone by its author's `u` and revertable explicitly. The protected operational domain never owns or copies
membership; its records only reference it by `{rN, revision}`. It is a bounded forest: at most one parent, no cycles,
parent and child in the same project. Moving an agent moves its subtree. `team:<rN>` selects that coordinator and its
transitive reports. Names, panes, process ancestry and parentage never create membership or rights.

**Grants.** Coordination, not security. Rights come only from the owner rule and explicit additive grants (union of
matching unexpired grants; no denies, no order), changed through `coord.grants` (attributed).

```jsonc
// doc/agents/grants/<id>
{"who": "p:coord" | "@reviewer" | "team:r7" | "project:p1" | "*", "can": ["send:boundary", "interrupt"],
 "on": "@worker-3" | "agent:r42" | "team:r7" | "project:p1" | "*", "until_ms": null}
```

- Resolve handle selectors **once at grant issuance**: `on: @h` becomes exact `agent:rN`, and `who: @h` becomes
  the handle's bound principal (reject if it has no unambiguous principal). Later rebindings transfer no rights;
  the original handle may remain a display label. `team:` and `project:` selectors stay dynamic over recorded
  membership, not over names.
- Rights: `send:<mode>`, `send:*`, `send:interrupt_then_start`, `receive` (pull and report), `interrupt`, `answer`,
  `queue`, `session`, `configure`, `command`, `inbox.edit_any`, `recover`, `handover`, `names`, `membership`,
  `pause`, `resume`, `stop`, `grants.manage`. Acting for another principal is not a right here: it is a core
  delegation row (§3).
- `grants.manage` on X grants or revokes rights on X only, never on a wider `on`, never rights it does not hold.
  Launchers seed `receive` for an agent's plugin through ordinary grants.
- A child endpoint's effective controls are the **intersection** of grants and its native capabilities.
- Checked at `coord.inbox.send` (requested mode), at claim (resolved strategy and each composed step), at take, and in
  Control. Revocation after claim cannot recall work. These are inbox policy, not enforcement on every possible input:
  direct `resource.invoke` remains the D24 escape hatch, visible in the core's effect audit.

## 5. Inbox

One extension owns every inbox and is the only dispatcher and waker.

### 5.1 Store

Records, attempts, gates, tombstone summaries and scope-control receipts live in a **protected operational domain**: a
bounded, non-undo persistence domain written through the same committer but persisted separately from ordinary
workspace state and its history (P1). A record, its payload revision and its attempt form one atomic versioned record.
Only the owning extension's operations change it; ordinary `workspace.apply`, reverts, imports and legacy history never
create, replace or resurrect its records; a transaction mixing protected and ordinary keys is rejected whole. Its
commits are durable and attributed, never appear as workspace history events and never take a global undo slot; an
uncertain write publishes the transaction actually adopted. Inbox persistence must not starve workspace writes. The
wire contract does not fix physical storage.

*Evidence (P1, `proto/inbox-seam` b26cf0a; a measurement model, not a shipped multi-domain core):* with the inbox in
the shared 8 MB workspace file at 100 txn/s (90% inbox), 5918/9000 commits succeeded, accepted p99 was about 2.7 s and
276 layout writes were refused; in a separately persisted 2 MB domain on the same committer code, 9000/9000 succeeded,
inbox p99 6–8 ms, layout p99 31–34 ms. Fences that passed, with no duplicate native write: kills before and after the
durable claim and after the native effect, injected `durability_unconfirmed`, settled-record GC + restart, old-delete
ABA, a full 8 MiB undo horizon untouched by 520 inbox writes, 60 edit/withdraw/claim races. Cross-domain atomicity and
shared publication fences are not implemented.

### 5.2 Record

```jsonc
{"v": 0, "delivery": "d:7c1e…", "submission": "01JF8Q…",
 "to": {"agent": "r42", "incarnation": "…", "session": "s1", "as": "@reviewer"},  // pinned at send
 "follow_current_session": false,
 "from": {"principal": "p:worker", "actor": "p:worker", "author": "client:12"}, // admission-time, unchanged
 "origin": {"principal": "extension:coord", "actor": "extension:coord", // current core attribution; initiator only if core supplied
            "sender": {"principal": "p:worker", "actor": "p:worker", "author": "client:12", "provenance": "inbox-record"},
            "agent": "r7", "kind": "agent", "claimed": ["kind", "agent"]}, // sender/classification are extension assertions, not core attestations
 "quoted": null, "render": null,                                                  // sender-supplied; see §5.6
 "mode": "boundary", "priority": "normal",  // low | normal (default) | high; pending selection only
 "content": [{"type": "text", "text": "Run the linux tests too."}],  // as agent.send
 "guard": null, "key": null,
 "reply_to": null, "thread": null, "retry_of": null, "cause": null, "correlation": null,   // deliveries / §8
 "expires_at_ms": "…", "supersedes": null, "order": 41,
 "state": "held", "reason": "agent_busy", "held_by": null,
 "attempt": null, "result": null, "handed_over_from": null}
```

### 5.3 Commands and operation identity

Commands are attributed (§3). Every mutating call carries `op`, and `coord.inbox.send` a `submission`: caller-chosen,
scoped by principal, idempotency only. The inbox advertises its **operation window** (`coord.inbox.window` → `{epoch,
from, to}`, also in every reply); callers prefix ids with that epoch. An identical repeat inside the window returns
the original result in its stored compact form (§10); the same id with another payload is `conflict`; an id outside
the window returns its historical result or `expired_operation` and **never creates a mutation or a native attempt**.
Result eviction never makes an id admissible again. The window governs call ids only: a pending record stays editable
by its `delivery`. `coord.inbox.op {op}` returns the result or `unknown`, never "did not happen".

The window also caches well-formed refusals (validation, grant and quota), subject to the protected recovery margin
below. For a **cached** refusal, an identical repeat returns the original compact refusal even if grants or capacity
changed; retrying under changed conditions needs a new `op`. Malformed calls and calls from an undeclared principal
occupy no slot. Cached refusal entries have a fixed small bound (§10); the full message appears only in the live
reply. Slot preflight and refusal persistence use the same existing control/recovery path: the 10% reserve excluding
archive-only escrow, not content capacity or the normal-ledger 64 KiB / 16-record control-intent headroom; no new pool.

Refusal slots may consume that reserve **only down to a protected recovery margin** sized to always fund the owner's
recovery: at least one maximum workspace-scope pause plus resume, with archive funding/escrow preserved as in §6.1.
The operation-window bound alone is not enough to protect recovery. If caching a refusal would cross this margin,
return the refusal **live and explicitly marked uncached**, without persisting its result; never turn this into
persistent pre-evaluation `busy`. The cached-refusal repeat guarantee does not apply to that uncached result; a repeat
may be evaluated afresh, without relaxing existing operation-identity or native-attempt replay fences. A quota refusal
thus remains reportable at the durable content budget. A genuinely full operation window still refuses `busy`
**before evaluation**; content saturation or preserving the recovery margin alone does not make the window full.

| Command | Effect |
|---|---|
| `coord.inbox.send {op, submission, to, mode, priority?, strategy?, content, quoted?, render?, send_new?, …}` | grant and quota check, resolve `to` once, admit or reject atomically, return `delivery`; `pending_exists` while the sender already has a pending record for that target, unless `send_new: true`; `strategy: interrupt_then_start` selects that recipe (§5.7) |
| `coord.inbox.edit` / `coord.inbox.withdraw {op, delivery, expect, …}` | while `recorded`/`held`, CAS on the record; after claim `too_late` |
| `coord.inbox.take` / `coord.inbox.report` / `coord.inbox.reconcile` | pull (§7) |
| `coord.inbox.peek {agent, at?, limit?}` | what a take at that point would return, without claiming (needs `receive`) |
| `coord.inbox.resolve {op, delivery, as: delivered \| not_delivered}` | judgement on an `unknown` record by its sender, recipient or a `recover` holder; recorded beside the evidence, never invokes |
| `coord.handover {op, …}` | §5.8 |
| `coord.scope {op, action: pause \| resume, …}` | §5.9 |
| `coord.inbox.list {target \| sender, state?, page?}` / `coord.inbox.get {delivery}` | bounded pages, indexed lookups; reading consumes nothing |

Results use one **coordination envelope** `{state, reason?, attempt?: {id, steps: [{op, outcome}]}, native?}`, where
`native` is the provider result, unchanged **in the live reply only**; the inbox stores only the compact outcome
(§10). "Undo last send" in a client is `coord.inbox.withdraw` of the caller's newest pending record.

### 5.4 Retry

- An `unknown` record is never resent. A principal may send a new submission with `retry_of: <delivery>`; the
  uncertainty stays visible on both. An adapter with declared, proven native idempotency (`agent-v0.md` §3.4) may
  allow a reconciled retry, declared separately.
- A definite `busy` rejection returns the record to `held` with a new attempt id at the next idle, same exact target
  and guard, no extension of `expires_at_ms`, no immediate retry (at most 3) `(pending K2)`.
- A `stale` guard or session is held (`session_changed` / `stale_guard`) for explicit edit or review.

### 5.5 Claim and invoke

1. One protected write moves the record from `recorded`/`held` to `claimed`, with `attempt` (id, strategy, effective
   session, `control_epoch`, claimant), as a CAS on that record: it and any edit or withdraw exclude each other. A
   durable claim means "the runtime may have been invoked".
2. Invoke only after that write is **confirmed durable**, and only if the current owner incarnation still matches the
   attempt's `control_epoch`, checked just before dispatch (a stale owner callback cannot act). On
   `durability_unconfirmed`, do not invoke; CAS the record back to `held` (`claim_unconfirmed`) and confirm that write
   before any later dispatch. If vqro restarts first, the record is `unknown`.
3. Invoke `agent.send` once with `delivery`, `origin`, `cause`, the pinned incarnation and the effective session as
   guard. `accepted` or `dispatched` → the compact outcome and receipt stage (§10); `rejected` → §5.4 or final;
   `indeterminate` → `unknown`.
4. Later receipts update `result.receipt`; a gap changes nothing.

### 5.6 Eligibility, order, fairness

- One claim, or one take batch (§7), in flight per target; each record has its own attempt. Controls never enter the
  inbox (§6).
- **Session.** A record is eligible only while the agent's current session is `to.session`, unless the sender set
  `follow_current_session: true` (visible; rejected together with an explicit session guard). The claim records the
  effective session and sends it as the guard; a further switch before invoke makes it `stale`. Nothing rewrites
  `to.session`.
- **Origin.** The record's `from` preserves the original core attribution captured at admission, unchanged; the
  sender cannot forge it. Dispatched `origin.principal`, `actor` and `initiator` are bound to the **current** core
  effect attribution (`agent-v0.md` §3.2), not equated with that original sender. For example, an autonomous
  dispatch can have transport actor/effective principal `extension:coord` while the original sender is P.
  `origin.sender` carries the record's `from` fields plus `provenance: "inbox-record"`, asserted by the coordination
  extension from its protected record, not attested by the core. Render **from `<sender>` via `<actor>`**. Caller
  `kind`/`agent` classifications remain asserted claims, never inferred from ids/prefixes. Asynchronous dispatch
  has no end-to-end core proof of the original sender in v0; no invented proof retention or implicit delegation.
  A sender may add `quoted` (asserted provenance of forwarded content, shown as a claim) and `render` (the renderings
  it accepts). If the target declares no accepted rendering, the record is held `origin_unsupported`.
- **Lanes:** `boundary`, `preempt` and `admit` are never blocked by `start`/`settle` records waiting for idle.
  Priority never crosses lanes. An `unknown` blocks only records whose ordering depends on it; priority never
  makes an otherwise ineligible record eligible. A record blocked by an unknown predecessor is shown as held
  `ordering_unknown` with the blocking predecessor's delivery id, in operator views and bounded lookups/lists;
  the stall is never hidden as generic busy or absence. Displaying the dependency neither resolves the unknown
  nor authorizes replay.
- **Priority and order.** Records carry `priority: low | normal | high`, default `normal`, editable while pending.
  Within one target and lane, choose (1) highest priority among eligible waiting records **across senders**;
  (2) sender round-robin among equal priority; (3) FIFO by `order` within one sender and equal priority. A sender's
  higher-priority record may overtake its own lower ones. Priority never bypasses quotas, gates or controls and
  never changes an already claimed record. Sustained higher-priority traffic can starve lower-priority mail;
  quotas bound admissions, not the wait. There is no aging in v0.
- **Fairness and quotas.** Within equal priority and one lane, round-robin prevents one sender from starving
  another. Quotas per sender per target and per sender overall (open records and bytes; `send_new` counts too)
  reject over the limit with `quota` `(pending K2 for numbers)`.
- **Hold reasons** (always shown): `paused`, `agent_busy`, `agent_absent`, `agent_ended`, `agent_retired`,
  `session_changed`, `stale_guard`, `unsupported_mode`, `unsupported_content`, `too_large_for_consumer`,
  `origin_unsupported`, `integration_unavailable`, `no_grant`,
  `quota`, `awaiting_route`, `handover_pending`, `claim_unconfirmed`, `ordering_unknown`.
- **Mode resolution:** the native mode if declared. `start` with `busy: reject` waits for idle. `settle` without native
  settle waits for idle and goes as `start`, `held_by: vqro`. `boundary` without native support stays held. `preempt`
  without native support only through §5.7.
- **Expiry:** `boundary` and `preempt` records carry a finite `expires_at_ms` (sender's or a declared default), checked
  at admission and before claim or a first composed step. Nothing renews it; expiry after dispatch never means "not
  delivered". `supersedes` replaces the sender's own unclaimed record.

### 5.7 Composition

Only this extension composes; steps and their outcomes go in the coordination envelope, each step stored as a compact
outcome reserved at admission (§10).
- A composition claims a mode only if it meets that mode's contract under its declared prerequisites. Composed
  `preempt` = `interrupt` then `start`; with runtime-held input that survives an interrupt it qualifies only if the
  recipe clears that input first (needs `queue`) and nothing else can enter in between; otherwise it is the separately
  named best-effort strategy `interrupt_then_start`. Pi gets only `interrupt_then_start` (clear, abort, start), partial
  under interference (P4).
- Needs `send:preempt` (or `send:interrupt_then_start`) plus each step's rights. One monotonic recipe deadline; before
  each step it checks the deadline, the gate, exact target, session and run, and the rights. It stops after any
  unsuccessful or uncertain step; a restart never continues a recipe. Interrupt observed + start rejected is a known
  partial result, never `rejected`.

### 5.8 Successor handover

`coord.handover {op, from: {agent, incarnation}, to: {agent, incarnation}, deliveries: […], bind?, membership?,
grants?}` moves pending intent from an ended, retired or replaced agent to a successor; `deliveries` is the exact
inspected list (bounded; oversized is rejected before any change).
- Needs `handover` on source and destination, plus `names`, `membership` or `grants.manage` for each change. It moves
  only listed records still `recorded`/`held`; `claimed` and `unknown` stay.
- The source's admission is fenced while the move commits. Moved records keep sender, evidence, expiry and session
  constraints and are held `handover_pending` (not eligible, even at an open destination) until `coord.handover {op,
  action: complete, plan}` by a `handover` holder on the destination, after every plan stage succeeded. Completing
  rechecks the destination and keeps any pause; handover never opens a gate.
- The protected move is one write; name, membership and grant changes are ordinary writes in the same recorded plan,
  each with its own `op` and expected revision; a failed or stale stage stops the plan, which shows what remains.
- A plain handle rebind only renames.

### 5.9 Pause and scope resume

- **Pause is an admission gate, not suspension** (confirmed by the human, 2026-11-19). A new `control_epoch` (core
  boot or inbox-owner change) closes every agent's gate before serving sends or takes. A paused agent gets no new
  content invocation, pull claim, wake or input-bearing composition step. An agent still working shows as working;
  clients show "inbox paused" beside the runtime state. A runtime's real pause may be offered as an extra capability.
  Control, observation and lookups never wait behind the gate.
- **Restart.** Never-attempted records stay held (`paused`); resuming makes them eligible, a first attempt, not a
  replay. Claimed records with no outcome become `unknown` and are never made pending again.
- **Scopes** select over membership: `workspace`, `project:<id>`, `team:<rN>`, `agent:<rN>`.
- **`coord.scope {op, action, scope, expect: {control_epoch, control_revision, membership_revision}}`** freezes the
  exact target set at one membership revision (its selection point), records it, and checks `pause` or `resume` on
  every target, the caller's own agent included, before changing anything (all or nothing; stale revision →
  `conflict`). A scope action is not a page: it is bounded by the declared **scope capacity** (≥ 200 targets at the
  target topology); expansion may be paged internally but is frozen atomically, and its results are paged. Over
  capacity is rejected before any change. Live permits exist only after acceptance is confirmed durable; replay,
  restore, revert, adapter registration or an old epoch's result never create permits; a non-revertible
  `control_revision` makes stale requests fail.
- **Cascade means parent-first release of gates, not coordinator-first native execution.** Each target reports
  readiness or its hold reason.
- Membership is ordinary configuration and gates are protected state; there is no atomicity across them. Later
  membership edits affect future selections only and inherit nothing from earlier resumes. A resume is undone only by
  a new pause, never by `u`.

## 6. Control and lifecycle policy

`coord.control {op, target, action, args}` (`interrupt | respond | queue | session | configure | command | object |
stop | retire`) checks the matching right, records principal and caller, invokes the native op once and returns the
coordination envelope. It never waits behind inbox records and runs on reserved control capacity (§9.4).

| Coordination lifecycle | From | Inbox | Handle, membership |
|---|---|---|---|
| `live`, `stopping`, `disconnected`, `unknown`, `ended` | the adapter's lifecycle evidence (`agent-v0.md` §7) | normal; `stopping` closes the gate; otherwise held `agent_absent` / `agent_ended` | `ended` resolves handles to `ended`, membership marked ended |
| `unavailable` | still no end evidence after a declared grace (default 60 s): cached live facts forgotten, tombstone kept | held `agent_absent` | kept in the tombstone |
| `retired` | a principal's explicit `retire`, recorded `{by, reason}`; native execution unknown | routing off, held `agent_retired` | handles resolve to `retired` |

- **Stop:** `coord.control {action: stop, args: {how: graceful | host_kill}}`, right `stop`. The gate closes first.
  `graceful` invokes `agent.stop`. If it is not declared, vqro offers `host_kill` only through an addressable,
  declared route: the endpoint's `host.resource` is a core resource of the same incarnation whose provider declares a
  termination primitive (`terminal.kill`), and everything that host serves is shown first. Otherwise `host_kill` is
  `unsupported` and the endpoint's state stays as observed; vqro never escalates silently. `ended` needs end evidence.
- At stop or end, held records stay held; claimed records without an outcome become `unknown`. Nothing is resent or
  rerouted.
- **Retire** (right `stop`) is an administrative judgement, never a claim that the agent stopped: routing is off,
  cleanup and handover are allowed, native execution stays unknown until end evidence makes it `ended`.
- **Owning-host exit is end evidence.** When the bound owning host terminal finishes with an exit, the endpoint
  is `ended`; retention may then clean up cached facts and tombstones under these bounds. It never changes that
  evidence to `retired` or `unknown`. A non-owning host's disappearance is not end evidence.
- **Silence never ends an agent.** After the grace, retention may forget cached live facts (`resource.forget_cached`,
  never a native effect); the agent becomes `unavailable`.
- **Two separate stores** (capacity in §10):
  - an **identity-reservation cache** in the core, bounded and evictable: while an agent's reservation is present, the
    same identity reappearing gets its original `rN` back, still paused if it was restart-held; eviction only means a
    return becomes a new agent with a new `rN` that inherits nothing `(pending C4 for the core change)`;
  - **durable evidence** in the protected domain: a tombstone summary `{agent, identity, handles, last_seen_ms,
    last_evidence, held, unknown, membership_ref: {rN, revision} | null}`, reserved when the agent first gets
    coordination state (so creating it needs no new space), and its held and unknown records. The agent's membership
    stays ordinary configuration (§4); the summary only references it.
- **At the durable budget**, admissions that need protected space (new sends, coordination state for a new agent) are
  refused with a visible `quota` reason. Evidence is never dropped silently: tombstones with no held or unknown
  records, no membership reference and no pending handover are pruned oldest first (they hold no evidence); the rest
  stay until a handover, an explicit withdraw or removal, or an explicit **archive** (§6.1). Agent registration
  itself always succeeds.

### 6.1 Archive

`coord.inbox.archive {op, agents, destination}` (right `recover`) moves durable evidence out of the protected domain
without ever losing it:
1. **Freeze:** record a pending archive in the protected domain with the exact bounded set of records and their
   versions. An open handover must complete or abort first; its members cannot be frozen. Records in the set are
   not changed except by evidence that arrives later (step 4).
2. **Export:** write a manifest (archive id, delivery ids, versions, content hash) and the records to the destination,
   and confirm it is durable (synced and read back against the hash).
3. **Free:** one CAS against the frozen versions replaces the records with a compact archive marker `{archive,
   locator, manifest_hash, deliveries}`. A record whose version changed meanwhile (genuine newly observed receipt
   evidence) is left in place and reported. Intent edits and handover writes on frozen records are refused.
   Completing/aborting a legacy handover skips and reports members already archived/freed, never recreates them,
   and still transfers/restores its remaining pending members.
4. Uncertain export, a failed write or a CAS conflict keeps the evidence and leaves the archive visibly pending;
   nothing is freed. Repeating the `op` inspects that archive, never exports a different set or frees blindly.
5. **Abort:** the workspace owner's explicit `coord.inbox.archive {op, action: abort, archive}` records the caller,
   releases the freeze and pending-plan slot, and retains source evidence and existing export files. Genuine late
   receipts are never suppressed to keep a frozen version unchanged: after abort, inspect and create a fresh plan.
   Abort changes no admission permit and invokes no native agent operation. It does not cancel filesystem I/O already
   running; a finishing exporter reports any file it wrote but cannot resurrect an aborted/completed plan or free data.

A lookup of an archived delivery returns its locator. A missing or unreadable archive answers `unknown`, never "no
effect". Archive is never triggered by memory pressure. At most four archive plans are pending. Every accepted
plan pre-reserves **16 KiB / 4 future records** for its remaining recovery metadata; after export this is
**8 KiB / 2 future records**. Reject admission if the funding does not fit, before external effects (§10).

### 6.2 Request routing

Each agent has a request route: `human`, `coordinator` or `both`. This is ordinary attributed agent
configuration, not native request state; the owner or a principal holding `configure` on that agent may change
it through transactions. Launchers record the route: default `human` for agents the owner starts,
`coordinator` for agents launched by a coordinator.

- `human`: show the waiting notice and request popup in the requesting agent's pane.
- `coordinator`: deliver an inbox message about the request to the requesting agent's **recorded coordinator**,
  its membership `reports_to` exact agent endpoint, not every principal holding `answer`. That coordinator must
  currently hold `answer` on the requester; team launchers seed this grant. The message identifies the requesting
  agent and the native request's epoch, id and revision so a response targets that exact request.
- If there is no `reports_to`, the coordinator is ended/unavailable, or it lacks `answer`, fall back to `human`
  with a visible reason. There is no guessed coordinator or automatic successor routing.
- `both`: coordinator delivery plus the human waiting notice/popup (or the visible human fallback above).

Routing never answers or auto-approves. The coordinator answers through `coord.control {action: respond}` with
`agent.respond`'s epoch/revision/scope/expiry fences (`agent-v0.md` §5), checked along with its current `answer`
right. Any other principal holding `answer`, including the owner, may still respond through Control regardless
of route. **First valid answer wins**; other viewers/controllers see it answered and cannot write a second native
response. Unanswered requests stay pending under their native expiry rules; routing creates no new expiry,
renewal or weaker response fence. Inbox delivery or a human notice is never evidence of an answer.

## 7. Pull at safe points (P5)

A terminal (tier-1) agent receives messages only by pull, through an in-agent plugin or hook; a structured adapter may
use it too. The plugin needs the `receive` right on its agent and never reads or writes the runtime's input editor.
vqro never types into an agent's terminal by default; guarded typing is an optional extension that needs fresh
editor-empty evidence, reports `dispatched`, labels the mode unverified and obeys the gate.

- **Declaration** `pull {points, insert, evidence, framing}` and a `consumer` incarnation id per load. `points`
  is a map `{<point>: {precision: "exact" | "approximate"}}`, not a list. Point names: `turn` (tool results
  recorded, before the next model request), `settle` (work would otherwise end), `idle` (nothing running),
  `tool` (the model asks for its mail). Each served point declares its precision; approximate never claims an
  atomic native safe-point guarantee. `insert`: `user` (attributed user message) or `context`. `evidence`:
  `entry` (a durable runtime record carrying the delivery id) or `none`.
- **Framing per path.** `pull.framing` maps safe points to `{role, frame, authority?}` using the rendering
  descriptors of `agent-v0.md` §3.2. Plugins/renderers/adapters declare the framing each path adds, including
  fixed native wrappers, rather than claiming all paths have the same role or authority. The route must fit
  the sender's accepted rendering; framing never silently promotes authority or rewrites the recorded origin.
  Unknown framing is shown generically, not assumed to be plain user input.
- **Eligibility is the inbox's:** `turn` serves `boundary`; `settle` serves `boundary` and `settle`; **exact** `idle`
  serves `start`, `boundary` and `settle`; **approximate** `idle` serves only `boundary` and `settle`, never `start`.
  `tool` serves `admit`, plus `boundary` where declared an evidenced boundary. `preempt` is never pulled.
  At **approximate idle**, a human prompt may have started work by the time insertion occurs. Only records safe
  under their declared `boundary`/`settle` insertion contract may be taken there;
  preserve that mode if the agent is busy, never treat the take as `start` on an assumed idle runtime. If the
  plugin cannot maintain that contract it must not claim the record at that point.
- **Take:** `coord.inbox.take {op, agent, consumer, at, accept: [block types], max_bytes, limit ≤ 16}` claims, as
  one batch, up to `limit` eligible records in §5.6 selection order within the page bound (the rest stay held), durably before replying,
  and returns `{records:
  [{delivery, from, origin, mode, content, claim}]}`. Each claim binds delivery and version, target and effective
  session, `control_epoch`, `consumer` and `at`. A paused, ended or unauthorized target returns `{records: [], held:
  <reason>}`. The plugin inserts every returned record at that point, in order, with its origin. A `turn` take may
  extend the run by one request. Every content block must have a type in `accept`; other records stay held
  `unsupported_content` for that consumer. `max_bytes` is the consumer's per-record byte budget: a larger record
  stays held `too_large_for_consumer` for that agent. Neither filter claims then releases/reclaims the record at
  each safe point. No block is dropped or flattened; the aggregate page/batch byte bound (§10) still applies.
- **Report** `coord.inbox.report {op, delivery, claim, stage}` names the current claim and consumer and only moves
  forward; a stale claim is rejected unchanged. `inserted` (`evidence: entry` only), `supplied` (`insert: context,
  evidence: none`), `read`, `handled`, as in `agent-v0.md` §2.2. `released`: the plugin returned from the point
  without inserting it in a handler it controls; the record returns to `held`. Anything else is `unknown`.
- **Recovery.** A lost reply is recovered with `coord.inbox.op`; a repeated take returns claim history, never content.
  On every (re)start, before taking, a new consumer calls `coord.inbox.reconcile {op, agent, consumer, records:
  [{delivery, entry?}]}`: a claim with a durable runtime entry carrying that delivery id becomes `inserted` on its
  original attempt; every other one becomes `unknown` (always with `evidence: none`). Reconcile grants no claim; later
  reports from the old consumer are rejected.
- **Wake:** pending counts and a revision, never content, on admission, edit, resume and handover; none while
  paused. An edit of held mail advances the revision even when counts are unchanged, so unchanged-publication
  suppression cannot lose the wake.
- **Terminal agents.** A terminal is an agent target when it reports OSC 7501 with an `app` key or
  `doc/agents/terminals/<rid>` marks it; without a plugin its records stay held `integration_unavailable` and it stays
  usable by hand. A plugin's structured endpoint supersedes the marker by a recorded handover.

**Claude Code example (informative, A3; measured with 2.1.329 and a fake model).** Hooks pull at `turn`
(`PostToolBatch`), `settle` (`Stop`) and interactive-only `idle` (`asyncRewake`), with `insert: context`,
`evidence: none`: a flushed hand-off is `supplied`, never `inserted`. `read` requires a model-visible transcript
request containing that hand-off and normal completion evidence; hook success is never evidence. Framing differs
by path: tool-result / Stop context uses native system-reminder framing; rewake is framed `SYSTEM NOTIFICATION -
NOT USER INPUT`. These are per-path framing declarations, not permission to present a rewake as plain user
input or upgrade its authority. Claude's hook-based idle declares **`pull.points.idle.precision: "approximate"`**
(it can race a human prompt). Records pulled there retain their `boundary`/`settle` contracts; the hook never
claims or handles them as `start`. Pi declares **`pull.points.idle.precision: "exact"`** for its measured idle point.
Senders must accept the route's context rendering. Non-interactive Claude sessions do not support this idle wake.
Starting activity in an idle Claude Code session uses its native session inbox socket (`admit`) or the human,
not an assumed-idle hook start. That socket is a separate, explicitly selected `admit` route without vqro-side
edit after posting; it does not turn a held `start` into `admit` or imply a conditional-start guarantee.

### 7.1 Advisory terminal status

The agents extension provides `agents.status {terminal, app, state, seq}` for tier-1 hooks without a tty or an
OSC 7501 path (Claude Code hooks, for example). This is lossy observation, not a coordination mutation or native
effect. `terminal` names a live terminal resource with an id of at most 64 characters; `app` is 1–64 printable
characters (no controls); `seq` is an integer from 0 to 2^53−1 (Unix epoch milliseconds recommended).
`state` is `idle | running | requires_action | unknown`;
an absent or unknown value becomes `unknown`, never inferred idle.

Reports are last-writer-by-sequence **per terminal**, across apps: only `seq` strictly above the last report applies.
The reply is `{ok: true, applied: true}`, or `{ok: true, applied: false, reason: "stale", seq: <last>}`. Invalid
arguments and a missing/ended terminal are refused to the caller only, without a notice. At most 1024 terminals
may report, with an additional 960 KiB encoded-table admission budget to fit the core's 1 MiB live-value cap;
exceeding either bound returns `overloaded` and records nothing.
Sequence is not scoped to a reporting process or incarnation; a restarted reporter must advance it. Reports live
only in extension memory, disappear when the terminal ends or is removed, and do not survive an extension restart.

A report may carry optional `facts {session?: {cwd?, model? {id, label?, effort?}}, usage?}`. These are the view-fact
shapes of `agent-v0.md` §2.3, holding only what the plugin read from its runtime: absent is unknown, never inferred.
- Unknown keys are dropped; a known key with the wrong shape refuses the report (`invalid`).
- At most 1 KiB encoded per terminal, counted in the table budget, so terminals with facts reduce how many fit
  (about 520 with full facts).
- A report without `facts` keeps the terminal's last ones while its `app` is unchanged (a different app starts with
  none); a report with `facts` replaces them (never merged).
- They are published as the entry's `facts` in `agents.terminal_status`, with the same seq rule as `state`.
- A plugin whose facts are refused reports its state without them.

Claude Code hooks report `cwd` (hook input) and the model id (the transcript's latest assistant entry). Pi reports
`cwd`, the model with label and thinking level as `effort`, and its own context figure
(`usage.context {window, used?}`). Neither exposes session token totals or cost to a plugin, so those are absent.
**Reporter ownership (bundled tier-1 extension policy).** Only the terminal's own known launcher-tagged principal
or the **valid workspace owner** may report; other callers get `not_authorized`, never an applied report. With a
null/unknown or `undeclared` `resource.principal`, only the valid owner may report until relaunch or a new observed
launch assignment. In particular, core restart does not recover an agent reporter from a surviving environment,
provider fact or child assertion. Owner names use §3's exact validation domain; an invalid legacy owner does not
become authorized by fallback. This is the bundled agents extension's policy over a core observation, not a core
ownership policy, authentication proof or persistence promise.

Live data `agents.terminal_status` is `{data: {<terminal>: {app, state, seq, at_ms, status}}}`, where `at_ms` is
receipt time and `status` is OSC 7501-shaped `{app, state}`: `running` → `working`, `requires_action` → `blocked`,
`idle` / `unknown` unchanged. Publish only when app/state changes or a report is dropped. Stored sequence advances
on every applied report; published `seq` and `at_ms` may lag the last accepted report. A publication contains all
current rows, so another terminal's change can refresh them too. Only one publisher runs at a time, taking fresh
snapshots and republishing changes that arrive during publication; an older table never overwrites a newer one.
This is not a report log.
The sidebar uses this fallback **only without the terminal's own OSC status**. No report means unknown, not idle;
a report alone proves neither delivery, a model read nor lifecycle end.

## 8. Causality

Admissions record `cause` and `correlation` in the qualified form of `agent-v0.md` §2.5 and pass them to
`agent.send`: a cause the caller supplies is recorded with `provenance: asserted`; one the inbox can prove (a reply to
a delivery it holds, `retry_of`) as `adapter`. `reply_to`, `thread` and `retry_of` stay as they are. Nothing is
inferred from timing or text; absent means unknown.

## 9. Core additions (generic; none is agent-specific)

1. **Protected operational domain** (§5.1): one per owning extension, its size declared at registration (§10);
   bounded, non-undo, separately persisted, written only by its owning extension, fenced against ordinary writes and
   reverts; production registration, cross-domain rules and publication
   fences still need design (P1 evidence).
2. **Attribution** (§3; P2 built `principal`, actual author and initiator): the recorded fields on transactions,
   events and `effect.invoke`; undeclared calls recorded as such; default undo by effective principal. The connection
   default, per-endpoint host tags, `label`, the `invocation` proof field and `doc/principals/delegations` rows
   `(pending C2)`.
3. **Identity-reservation cache** in `resource.forget_cached` (§6), bounded and evictable `(pending C4)`.
4. **Reserved admission and control capacity**: the core keeps commit-queue and provider-call capacity for control
   commands and answers, and a separate budget for stream opens, so neither reconnect bursts nor inbox floods can use
   up the capacity interrupts and answers need (P1, P3; the C1 commit-worker lanes in §10). Answers and interrupts
   meet a declared latency bound under that load `(pending C1)`.
5. **Owner fencing**: an extension's writes and dispatches are checked against its current supervised generation at
   dispatch and at commit (audit 2026-11-19; the fix is a separate main-branch change and is not evidenced by this
   draft).

## 10. Bounds and capacity

Every read and action is bounded by count, bytes and work: indexed lookups, bounded traversal, size preflight;
oversized atomic actions are rejected before any change; read-only recovery works before any migration-triggering
write.

**Capacity** `(pending C1)`. Each owning extension has **one** operational domain and declares its size when it
registers it: by default 2 MiB and 1024 records, at most 8 MiB with records scaled (e.g. 4096), always with at least
10% reserved for control; the core admits at most 16 domains and 32 MiB in aggregate. The commit worker admits 128
requests / 8 MiB on its normal lane plus 16 requests / 1 MiB on a control lane, with operational (inbox) work on the
normal lane capped at 80 requests / 4 MiB so it cannot starve layout writes. The default coordination extension
declares **4 MiB** and 1024 records (the generic core default remains 2 MiB). C1 measured 9000/9000 commits at
100 txn/s with 4 MiB: operational p99 31.62–35.50 ms, layout p99 41.82–48.32 ms. This was three 30 s runs on a
shared host with 90% operational / 10% layout transactions and an 8 MB layout workspace; the 2/8 MiB runs used a
different base, so no controlled equivalence is claimed. The 2 MiB case also passed 9000/9000; 8 MiB remains allowed
capacity, **not certified at 100 txn/s** (normal traffic 8927/9000, layout p99 up to 1.27 s). Every retained item is
charged at its encoded size:

**Recovery partition (K2 `476e43a`).** Potentially uncertain external intents are charged to the **normal storage
budget before invocation**, even though their writes use reserved control-lane priority. Content cannot consume
a **64 KiB / 16-record share within that normal budget** kept for bounded control intents. The separate 10%
recovery reserve keeps its full size; its last **64 KiB / 16 records are archive-only**, leaving the remainder
for other recovery controls. This archive-only share is a defensive margin, not a second proved exhaustion case:
the known-metadata negative control was already covered by retaining only **16 scope receipts**. Archive plans
also fund their future metadata (§6.1); this charge is separate from their actual retained manifest/plan bytes.
Refusal caching must additionally leave a protected recovery margin sufficient for at least one maximum workspace
pause plus resume, with existing archive funding preserved (§5.3). A window's worth of refusals must not exhaust
owner recovery. Below that margin refusals remain live but explicitly uncached, not additional durable charges.

| Retained item | Bound each | Count | `B` = 2 MiB, 200 agents | `B` = 4 MiB | `B` = 8 MiB |
|---|---|---|---|---|---|
| domain audit/header metadata (C1, fixed) | 64 KiB | 1 | 65,536 | 65,536 | 65,536 |
| control and recovery reserve (includes archive-only 64 KiB / 16 records, not added again) | | `ceil(B/10)` | 209,716 | 419,431 | 838,861 |
| normal-ledger control headroom (content excluded) | 64 KiB | 16 record slots | 65,536 | 65,536 | 65,536 |
| tombstone summary (encoded identity ≤ 2 KiB, ≤ 4 handles, evidence, counts, membership ref; legal maximum 2,503 B) | 3 KiB | agents with coordination state | 614,400 | 614,400 | 614,400 |
| gate | 128 B | agents | 25,600 | 25,600 | 25,600 |
| operation-window result or refusal (compact: state, outcome, receipt stage, error kind, ids) + fingerprint | 256 B | window `W` (1024) | 262,144 | 262,144 | 262,144 |
| settled index entry (delivery → outcome) | 64 B | 1024 | 65,536 | 65,536 | 65,536 |
| final record, compact (metadata, attempt, compact result; content dropped) | 1 KiB | 256 retained | 262,144 | 262,144 | 262,144 |
| handover plan | 4 KiB | 16 open | 65,536 | 65,536 | 65,536 |
| archive manifest (pending archive) | 4 KiB | 4 | 16,384 | 16,384 | 16,384 |
| future archive metadata funding (frozen/exporting plan; 8 KiB / 2 records after export) | 16 KiB / 4 records | 4 plans | 65,536 | 65,536 | 65,536 |
| scope receipt | 8 KiB | 16 retained | 131,072 | 131,072 | 131,072 |
| **fixed subtotal** | | | **1,849,140** | **2,058,855** | **2,478,285** |
| open record: metadata 2 KiB + content ≤ 32 KiB + compact outcome reserved at admission 1 KiB (per step for a recipe) | ≤ 35 KiB | the rest | 248,012 left | 2,135,449 left | 5,910,323 left |

C1 charges every domain a fixed 64 KiB audit/header reservation inside `B`. With 200 agents, the default **4 MiB**
domain leaves room for about **59 maximum-size open messages or 521 small ones** (1 KiB content, 4 KiB charged),
beyond which admission is refused `quota`; a 2 MiB domain holds about 6 or 60, and an 8 MiB domain about 164 or
1,442 under its 4096-record ceiling. This conservative example includes four frozen plans' future funding and
normal control headroom; the archive-only share is already inside the 10% reserve. A record count is a ceiling,
not room for that many maximum payloads.
Bounds are on the **encoded** size, not raw string limits (an identity of legal strings can encode to 1,989 B). Each
open record reserves a **compact outcome** per attempt and per recipe step (steps × 1 KiB) at admission, and admission
is refused `quota` if the reservation doesn't fit. A compact outcome is at most 1 KiB: state, receipt stage, outcome
kind, error kind, native op, byte size and sha256 of the full native result, and `incomplete: true` whenever native
data did not fit; an explicit, marked bound, never silent truncation. The full native result is returned unchanged
only to the live caller (`native`) and stays retrievable only where the adapter retains it (`agent.receipt`, native
records). A late reply always fits its reservation by construction; if even the compact form cannot be written (a
store failure), the attempt stays `unknown` and visible for attention, never resent. Beyond the declared size,
admission is refused `quota`; evidence is never dropped. The **identity-reservation cache** is separate core memory:
each entry holds the full identity (provider ≤ 128 B, instance ≤ 256 B, ref and incarnation ≤ 128 B each, `rN`) in ≤ 2
KiB encoded (legal maximum 1,989 B), so `N` = 4096 entries is 8 MiB.

| Item | Bound |
|---|---|
| Record content | 32 KiB (larger by link) |
| Open records | per sender per target, per sender, per target; protected domain total `B` |
| Tombstones | summaries (≤ 3 KiB encoded) counted in `B`; unprotected pruned oldest first; identity cache `N` entries of ≤ 2 KiB |
| Scope action | declared scope capacity ≥ 200 targets; results paged |
| Lists, pages, take batches, handover lists | ≤ 64 items (take ≤ 16), ≤ 256 KiB per page or batch |
| Composed recipe | one monotonic deadline |

## 11. Out of scope (v0)

Operation permissions and sandboxing; security against same-user processes; task engines and automatic task
completion; automatic agent activation; physical suspension; cross-workspace or cross-host delivery (D23);
exactly-once; read markers; priority aging; blocking waits for agents (send, end the turn, the
reply wakes you).
