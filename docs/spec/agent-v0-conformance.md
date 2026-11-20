# vqro agent standards v0: conformance (draft)

> **Status (2026-11-19): draft, not implemented** (D28). Part A checks adapters against the integration contract
> (`agent-v0.md`); Part B checks a coordination extension against `coordination-v0.md`. This file also holds the
> traces both rely on. The suite joins `scripts/check.sh full` when the first adapter runs it.

Conformance checks that an implementation does what it declares and nothing it doesn't. A declared capability that
fails its test fails its module, whatever else passes; declaring less is always allowed. A result is never "level N"
alone: it records the exact capability map, native or composed provenance, limits, runtime and adapter versions, and
every omission. Tests select scenarios by declaration, never by fixture limits or unknown facts: a scenario whose
declaration doesn't apply is not-applicable, which is neither `unsupported` nor a gap. Every resource's caps are
checked against its provider's declaration field by field as a subset (value in the provider's set, number at most
its maximum, `true` only where the provider allows it, ops and modes present at provider level), not by deep
equality, and every exercised behaviour must lie within the resource's current caps (C2).

**Fixtures (both parts).**
- Drive only the public core and provider wire, in a private root (`VQRO_STATE_DIR`, `VQRO_SOCKET`,
  `VQRO_EXTENSIONS_DIR`, private `XDG_RUNTIME_DIR`/`HC_*`, no `HERDR_*`), with fake models only.
- A **scripted fake runtime** per wire shape (Pi-like JSONL, ACP-like JSON-RPC, later others) runs one timeline format
  in runtime-neutral verbs: `turn`, `tool_call`, `model_call`, `request`, `queue`, `crash`, `hang`, `drop_reply`,
  `pause_before`/`pause_after` (acceptance, insertion, tool), `retain`, `session_switch`, `exit`, `host_exit`.
- The fake runtime's **native effect ledger is the oracle**: duplicates and insertion points are asserted from what
  the runtime received, never from vqro's view. Provider replies alone never prove "no duplicate".
- A client fixture provides two simultaneous viewers/controllers; core, adapter, inbox owner and plugin restart
  independently. Deadlines are short (0.5–2 s). Timelines needing an undeclared capability are skipped, not failed.

```jsonc
{"name": "D3-boundary", "requires": {"agent.send.modes": ["boundary"]},
 "script": [{"on": "input", "match": {"text": "go"}, "do": [{"turn": "start"}, {"tool_call": "tc1", "hold_ms": 300}]},
            {"after": "tc1", "do": [{"model_call": "m2"}, {"turn": "end", "stop": "end_turn"}]}],
 "steps": [{"invoke": "agent.send", "args": {"mode": "start", "content": [{"type": "text", "text": "go"}]},
            "expect": {"outcome": "accepted"}},
           {"wait": {"state": "running"}},
           {"invoke": "agent.send", "args": {"mode": "boundary", "content": [{"type": "text", "text": "focus"}]},
            "expect": {"outcome": "accepted"}}],
 "assert": [{"runtime_log": "input 'focus' consumed after tc1 completes and before m2"}, {"runtime_inputs": 2}]}
```

# Part A: adapters (`agent-v0.md`)

## A1. What must be declared

| Declaration | Where | Checked by |
|---|---|---|
| `agent {v, modules}` (`modules` an object map name → `{v, …}`; a list is invalid), ops | `provider.register` | C1 |
| per-resource `caps` with `rev`; `caps.host {kind, endpoints}`; facts `host` with `owns_lifecycle` | `data.agent` | C2, C10, D9 |
| per mode: `busy`, `idle` (`start \| reject \| park`), `point`, `batch`, `batch_at`, `preflight` | `agent.send.modes` | D1–D5, D9, D12, D13 |
| `content {types, shape}`, `correlation`, `keys {scope, retention}`, `guards`, `receipts.retain`, `literal`, `handled`, `origin` | `agent.send` | C8, D6–D8, D14–D17 |
| `pending`, `scope`, `waits` | `agent.interrupt` | D10 |
| `items` bounds (per entity and aggregate), `retain_bytes`, optional `snapshot_buffer_bytes`; snapshot coverage | `agent`, snapshot begin | O1–O14 |
| request `methods`, `scopes`, `background_requests`, answer acknowledgement | `agent.respond` | R1–R7 |
| module actions (queue, session, config, command, history, object, display terminals) | each op | M1–M7 |
| `covers`, `waits`; per-call reply `covers`, `covers_evidence` when requested-only | `agent.stop` | L2 |
| `agent.tool_principal: "env"` (absent = not provided); provider upper bound `["env"]` | `data.agent.caps`, provider agent descriptor | G11 |

## A2. Levels

Levels describe usable combinations; module claims are separate ("I + queue + sessions").

| Level | Requires | Does not imply |
|---|---|---|
| **O** observed | Core: endpoint model, facts, capabilities, item stream with snapshot cuts and gaps (C, O, L) | any control |
| **I** interactive | O + `agent.send` with `start` or `admit`, `agent.interrupt`, requests for every interaction the runtime exposes (D, R) | `boundary`/`settle`/`preempt`, guards, keys |

## A3. Tests

A case definition starts with its bold case label; plain IDs are references.

**C. Core.** **C1** registration and declarations. **C2** per-resource caps narrower than the provider map, a stale `caps_rev`
or `epoch`, an unknown action, or a precondition the adapter cannot check as declared: all reject before any native
write. **C3** state transitions match the script; silence never yields `idle`; unknown states render busy. **C4** lost reply →
`indeterminate`; the ledger shows one delivery, including across adapter and core restarts. **C5** failure after
acceptance is a completion; the earlier `accepted` stands. **C6** facts over their bound are cut with `incomplete`, never
silently. **C7** core restart while the runtime works: same `rN`, facts resync, no input resent. **C8** literal text: slash
text, control characters and Enter have no side effects; only text the adapter declared it cannot send literally is
rejected; `literal: controlled_launch` holds only for a launch without expanders or interceptors (Pi: `/command` and
`/template` run or expand under ordinary config; `expandPromptTemplates: false` is ignored). **C9** causality: a `cause`
passed on `agent.send` comes back on the receipt and on the inserted message only when the adapter can prove the link,
with its `provenance`; no link is ever inferred; request `q1` on two endpoints and an effect id reused after a core
restart stay distinct through their `endpoint`/`epoch` and `boot` qualifiers. **C10** endpoint model, by
`caps.host.endpoints`: with `"many"`, one server host with three endpoints shows one `host.id` for all three and each
endpoint keeps its own `rN`; with `"one"` the multi-endpoint scenario is not-applicable. Session switch keeping `rN`
is tested in M2. **C11** optional view facts: runtime-reported usage preserves context, token/cache counts, currency,
cost basis and plan; absent sections stay unknown, never zero or locally priced estimates. A window-only runtime
reports `context: {window}` with no `used`; last-turn token totals never become context occupancy. Optional run and
native thought-message usage preserve that same shape and only native reports; absent values are not apportioned
from other usage. Session cwd/model/effort
match native reports, never the bridge/viewer's cwd or a guessed model. Model changes still go through the
advertised model-category config option, not a facts write. **C12** generic causality wire: root-only, args-only and
matching dual `cause`/`correlation` arrive unchanged in effect metadata/audits, with args unchanged. Conflicting
forms reject `invalid` before dispatch. Require string kind/id/provenance and the entity/request endpoint+epoch
or effect boot qualifiers (presence/type only); do not resolve them as authority. Unknown enums, extra bounded
fields and provenance pass unchanged. Exercise cause 4 KiB, known strings 256 bytes, depth 64, and non-empty
correlation 256 bytes; invalid types, missing qualifiers and over-bound input reject without native writes.

**D. Delivery.** **D1–D5** each declared mode × {idle, busy}: the declared edge happens at the declared point (ledger
records the insertion point), or `busy`/`unsupported` with an empty ledger; `batch: one|all` honoured; no fallback,
`admit` included. **D6** undeclared content rejects before native write. **D7** receipts: a native acknowledgement maps to
`admitted` (Pi's `success: true` included); `inserted` only with correlated native evidence; under `correlation:
none` observed user messages carry no `delivery`. **D8** keys: same key twice (in flight, after insertion, across an
adapter restart, other session, past retention) gives what `keys` declares; a key never deduplicates a tool effect.
**D9** changing a runtime setting (Pi steering or follow-up mode) republishes `caps`; with `batch_at: consume`, three
steers accepted under one-at-a-time and then switched to all land in one request (and the reverse splits them). **D10**
interrupt per `pending`/`scope`/`waits`; idle interrupt is a no-op; Pi declares `pending: unknown` (a steer can enter
the session during a tool abort with no model request; follow-ups stay parked). **D11** guards: stale session/run rejects
before native write; `runtime` vs `adapter` strength distinguished under two concurrent controllers. **D12** idle `park`:
a Pi steer or follow-up sent while idle is parked, not started. **D13** start preflight: two overlapping starts are
serialized (one preflight in flight); a native settled event while a preflight is gated is no completion; interrupt
does not cancel a gated preflight. **D14** `handled: none`: a hook-consumed Pi input is `admitted`, never `handled` or
`inserted`. **D15** origin, asserted against the model-request envelope (not a UI badge): content with `origin.kind:
agent` arrives in the declared role, framed with its sender, never as plain user, system or developer text; `quoted`
provenance is shown as a claim; a user-role-only runtime and a context-hook runtime each use only renderings the call's
`render` accepts (`context` only when named); nothing acceptable → `unsupported`. **D16** content shape: with `shape:
"text_then_media"` (Pi), `[text, image]` and `[image]` pass, and `[image, text]` and `[text, image, text]` are rejected
`unsupported` with an empty ledger; nothing is reordered, flattened or concatenated; with `ordered`, any sequence of
the declared types passes in order. **D17** core-bound origin: invoke with core effective principal P, transport actor
`extension:coord`, and initiator W. Supplied `origin` must identify P/`extension:coord`/W separately; substituting W
for actor, changing principal or changing initiator rejects `forbidden` with an empty native ledger. Missing core
identity stays `undeclared`, never owner or a caller's assertion. Caller-classified `kind`/`agent` remain asserted
claims in the model envelope and receipts; changing principal spelling/prefix never invents a kind or agent, and
claiming `kind: human` alone cannot authorize plain human-owner rendering. For inbox dispatch, the unchanged
admission-time `from` appears in `origin.sender` with `provenance: "inbox-record"`; the envelope shows from that
sender via the current actor and labels protected-record provenance as the extension's assertion, not a core
attestation. An autonomous dispatch's current core principal/initiator are not rewritten to the original sender;
no end-to-end core proof or implicit delegation is claimed.

**O. Observation** (P3 prototype `proto/item-stream` fc1dd12 passes O1–O10 at 200 agents × 3 viewers; O11 untested).
**O1** put/append/remove algebra; append with a wrong `base` is a gap, never applied. **O2** snapshot cut, through two public
viewers: a large snapshot is sent while entities (an old A, a new B) keep changing; each viewer reconstructs at its
own cut, the second cut included, equal to the adapter's state at that cut, with every later change exactly once; the
overlap evidence is kept. **O3** producer-side follow-on overflow (that viewer gets a gap and installs nothing; others
continue) is tested deterministically in the shared Rust item producer's unit tests (C3); an adapter using that
producer inherits the evidence, and its result records which producer it uses. **O4** `after` outside retention or a
foreign epoch → `reset`, never a silent join. **O5** a session switch on an open stream: a new `snapshot begin` with the
new epoch precedes any new-epoch record (or the stream closes); a viewer at old `seq` 500 never ignores new records as
duplicates. **O6** two viewers attaching at different times show the same timeline order (by `position`). **O7** three
viewers, one stalled: only the stalled stream closes (`slow_consumer`); its connection, its other streams and
other viewers stay open. Native draining continues; the viewer reattaches from its last applied record.
**O8** JSONL lines split across frames and UTF-8 boundaries. **O9** coverage `tail`/`state_only` never shown as complete;
current state and pending requests survive pruning. **O10** retained memory stays within the declared bounds: measure
journal `max(retain_bytes + snapshot_buffer_bytes, max_record_bytes)` (the one-record floor) and old shared
entity versions held by active snapshot cuts, current
entities and adapter/transport overhead separately. A mid-snapshot cursor may retain up to `entity_bytes` of old
versions; include that per cursor and verify release on close/gap or the next cursor step after epoch change.
Optional `items.snapshot_buffer_bytes` exposes the configured buffer; absence means not declared, not zero.
P3's journal plateau of 26.2 MB is not an all-in slow-viewer cap (agent-v0 §8). **O11** (C3 covers attached retention
gaps and a 200-stream stall) a viewer draining steadily but slower than generation gets gap reason `retention`;
a stalled viewer holding all 200 streams loses them together and harms nobody; an unknown gap reason ends the
stream. **O12** producer append bounds: a small append whose resulting whole snapshot `put` exceeds
the record bound returns `too_large`, leaving entity, revision and `seq` unchanged; both text and decoded-byte
appends are covered. Adapters record how they handle this refusal without silently losing content. **O13** optional
view metadata: plain/default, markdown and unknown text formats render independently per block (unknown falls back
to plain). Runtime/vqro notices use role `system`, distinct from `coordination`, without upgrading a send's authority.
Tool/run `started_ms`/`ended_ms` and message `at_ms` match supplied evidence; missing times remain unknown, not
filled from observation order or the viewer's clock. **O14** incompatible viewers: repeat `bad_record` or
`over_bound` at the same continued position, and an over-bound entity in a reset snapshot. The viewer backs
off, reports incompatible stream with the cause/limit mismatch, and never enters an immediate reattach loop,
skips the record, advances its last-good cursor, relaxes bounds or installs a partial cut. An unknown newer op
gets the same visible incompatibility handling; unchanged rejection is not counted as recovery.

**R. Requests.** **R1** a request appears in facts and stream with its scope; `requires_action` when it blocks. **R2** two
simultaneous answers: exactly one native response; stale revision, foreign epoch, expired, no longer pending, dead
scope or invalid response → rejected before native write. **R3** `accepted` only with native ack, else `dispatched`. **R4**
unrepresentable or unknown methods cancelled once, never auto-approved; `incomplete` never approvable. For Pi, a
dialog with a positive native relative timeout is cancelled once as unrepresentable (no fabricated absolute expiry);
an untimed dialog remains answerable. **R5** explicit
cancel for permission, choice, confirm and text is distinct from denial, false and empty text. **R6** requests are tested
only in the declared `scopes`; with `background_requests: true`, a background request and a foreground permission are
pending and interrupt withdraws only the one the runtime stops waiting on; with `false` that scenario is
not-applicable. **R7** reconnect verifies, reissues or cancels pending requests; an authentication request before any
session is operation-scoped. Pi's native UI dialogs have distinct adapter-minted operation scopes, never guessed
session/run associations, and `background_requests: false`; their answers remain `dispatched` without a native ack.

**M. Modules.** **M1** native queue list/clear/withdraw/replace as declared; clear returns content once; null ids allow no
per-item ops; an emptied queue never yields `inserted`. **M2** sessions: `new` on a sessionless endpoint; switch keeps `rN`
and starts an epoch; resume sends no prompt; fork at an invalid position rejects; results appear in facts before the
reply. **M3** config: `observed` only after the runtime reports the value; a stale `rev` or a value outside `choices`
rejects before native write; a runtime-side change republishes the list and, if it alters a declaration, bumps
`caps.rev`. **M4** commands: only advertised names run; slash text in a send stays literal. **M5** history: pages from one
frozen cut with coverage; the last page's live cursor continues the stream with no gap or overlap. **M6** children have
their own `caps`; nothing is inferred from the root; object cancel affects one item. **M7** display terminal output (bytes)
appends and exit; a display terminal is not treated as a backend terminal. **M8** byte-safe append: chunks `/w==` then
`AA==` give the bytes `ff 00` (never the string `/w==AA==`); 1-, 2- and 3-byte chunks across padding boundaries and
non-UTF-8 output reconstruct exactly, through a snapshot `put` and live continuation; a `text` append on a bytes field
is rejected as a gap.

**L. Lifecycle evidence.** **L1** interrupt ends the turn only; stop ends the endpoint. **L2** `agent.stop` is invoked once
and its reply data includes this call's `covers`, equal to or narrower than declared and never wider. If effective
coverage is not natively evidenced, `covers_evidence: "requested"` is present and no stopped-work claim is inferred.
`ended` only after end evidence; a refused stop returns to `live`. **L3** the host terminal of
a bridge that declares `owns_lifecycle` finishes → `ended`; an app-server host that does not declare it disappears →
its endpoints `unknown`, never `ended`; a late end event from an old host incarnation ends nothing. **L4** a bridge
disconnect with no end evidence is `disconnected`, never `ended`; a reconnect never undoes `ended`; an idle completion
is never end evidence. An observed exit from the bound owning host terminal is end evidence, permitting cache
cleanup; it never relabels the endpoint `retired` or `unknown`.

## A4. Two implementations

A module, and each core addition it needs, is called stable only when genuinely different **real** runtimes pass its
tests: Pi RPC first, then **Codex app-server** and **Claude Code** (through its hooks), built in parallel (the human's
decision AS3). An ACP fake is a fixture for representability and wire-shape tests, not an implementation. Each
adapter's definition of done includes a small real-runtime smoke run on a cheap model, logged with its cost (smoke only;
about $20 approved in total). Core additions also need the Rust and Python provider SDKs (D26). Until then the module
is `draft`.

## A5. ACP coverage matrix (per-feature compatibility, not an equivalence score)

- **Matrix:** P6, branch `proto/acp-matrix` at `a726ede` (`docs/research/acp-matrix/`, `tests/test_acp_matrix.py`,
  `tests/acp_matrix_surface.py --strict`). 406 source-enumerated rows: 386 native, 6 composed, 14 unsupported in the
  model. Passing that model harness is representability evidence, not adapter certification.
- **Pinned sources:** ACP commit `d8cd473e5a04e21448c01972c82f22d91e10cd94` (2026-11-19T20:58:42Z), v1 and v2
  `schema.json` + `meta.json` kept separate, every input hashed in `sources.lock.json`. Optional RFDs (fork, cursors)
  are rows only when explicitly included; v1 client services (fs, terminal) and v2 display terminals are separate rows.
- **Unsupported rows are listed in two groups:** required by the pinned baseline (exactly v1 MCP stdio,
  `semantic:mcp-stdio-v1`, so the strict gate exits 1) and optional (authentication and the rest). The MCP gap is
  stated openly: vqro uses no MCP and offers no pass-through (the human's decision AS2), so the claim is ACP v2 base
  plus v1 except MCP.
- **One row per ACP feature:** `ACP feature | source ref | normative field or op (section) | fixture trace | status:
  native | composed | unsupported | evidence`. A row passes only if it binds to fields the contract defines.
- **Traces cover at least:** busy prompt → `admit`; v1 delayed completion vs v2 insertion; held input past long
  deadlines (901 logical seconds: insertion receipt, no second native write); same key in another session; root +
  restricted child; URL and rich forms; display terminal output and exit; inter-session participants; authentication
  before a session; history cut to live cursor; replay reset and end; config options, `set_mode`/`set_model` →
  `agent.configure`, available commands → `agent.command`; structured diffs; the MCP row as unsupported.
- **Pass:** no required behaviour lives only in opaque `native` payloads; unsupported precision stays unsupported; a
  compatibility claim names the snapshot and holds only for rows a real adapter passes. Failing traces are published.

# Part B: coordination extension (`coordination-v0.md`)

Checked against fake adapters. Level **P** (pull): a terminal agent plus a plugin serving `coord.inbox.take` /
`coord.inbox.report` at declared safe points (T); it does not imply timing beyond those points, or "read".

| Declaration | Where | Checked by |
|---|---|---|
| operation window, scope capacity, quotas, protected-domain budget, identity cache size | the extension | I10, I17–I19, I21–I23, H14, H16, L7–L9 |
| `pull {points: {<point>: {precision}}, insert, evidence, framing}`, `consumer`; advisory terminal status | plugin's endpoint, agents extension | T1–T15 |
| request route `human` / `coordinator` / `both` | ordinary attributed agent configuration | Q1–Q4 |

**I. Inbox.** P1's prototype (`proto/inbox-seam` b26cf0a, fake native ledger recording every invocation) passes I1–I6
for one fake agent; the rest need the full extension.
- I1 the claim is durable before the adapter sees the invoke (the fake adapter reads the store when called).
- I2 kill at each boundary (before claim, after durable claim before invoke, after invoke before result, after
  result): restart shows held / unknown / unknown / settled; the ledger shows zero or one delivery, never two.
- I3 `durability_unconfirmed` on claim → no invoke; the record goes back to held with that write confirmed, or is
  `unknown` if vqro restarts first; the event published is the transaction actually adopted.
- I4 ordinary `workspace.apply`, a revert of any earlier transaction (an old delete after pruning included, **with
  gates open**), imports and `u` by every principal never write or resurrect an inbox record; a transaction mixing
  protected and ordinary keys is rejected whole; history record and byte counts are exactly unchanged by inbox
  writes, also with a full undo horizon.
- I5 edit/withdraw racing claim: exactly one wins; no stale payload is ever sent after a successful pre-claim edit.
- I6 more than the retained number of settled records + restart: none becomes eligible again.
- I7 lanes: a held `start` never blocks `boundary`/`preempt`/`admit`; controls never wait behind records.
- I8 `busy` rejection returns to held with a new attempt id, same target and guard, no expiry extension, at most 3
  times, never in a hot loop; a `stale` guard or session is held for review.
- I9 budgets reject atomically and never block layout writes; space for gates and recovery stays available; each
  attempt and each recipe step stores a compact outcome of at most 1 KiB reserved at admission (a recipe whose steps ×
  1 KiB doesn't fit is refused `quota`); a native result larger than that is stored as its size, sha256 and
  `incomplete: true` while the live caller gets it unchanged; a compact-outcome write failure leaves the attempt
  `unknown`, never resent.
- I10 operation window: evict a settled record and its op result, then replay the original `send` and `report`: no
  new record, no native write; the same after a new `control_epoch`; same `op` other payload → `conflict`;
  `coord.inbox.op` answers `unknown` when it doesn't know. Well-formed validation, grant and quota refusals are
  cached and budgeted: after a grant or capacity change, the identical `op` returns its original refusal; a new
  `op` is evaluated afresh. Malformed and undeclared-principal calls take no slot; a full window refuses `busy`
  before evaluating the call.
- I11 composition: Pi gets only `interrupt_then_start` (clear, abort, start), never `preempt`; with exclusive writers
  it succeeds; a steer after clear or a competing start is reported as partial with step outcomes; pause or deadline
  after the interrupt stops the input step; a definite prerequisite failure stops the recipe; no continuation after
  restart.
- I12 session pinning: pending S1 mail, a switch to S2 while paused, then resume: held `session_changed`; with
  `follow_current_session` delivered to S2 with S2 as the guard; a switch to S3 before invoke makes it `stale`.
- I13 handover: source paused and destination open: moved records are `handover_pending` and a scheduling pass after
  the move, before a failed name/membership stage, claims nothing; only `complete` after every stage releases them; a
  pause stays; a late stage retry cannot overwrite a newer binding; `claimed`/`unknown` never move.
- I14 a stale owner callback (owner replaced while a callback runs) cannot invoke.
- I15 `coord.inbox.resolve` without sender, recipient or `recover` standing is refused; a resolution never invokes.
- I16 a second pending send by the same sender to the same target returns `pending_exists` unless `send_new: true`.
- I17 two principals both choose submission `s`: different `delivery` ids, and receipts, lookups and replies never mix
  them; a rolled operation window leaves both pending records editable.
- I18 fairness: one sender floods a target; another sender's **equal-priority** record is still served within one
  round of that priority in the lane; over-quota sends (including `send_new`) are rejected `quota`.
- I19 control headroom: during a reconnect burst of 200 stream opens and an inbox flood, interrupts and answers are
  still admitted within the declared latency bound; stream opens use their own budget and never exhaust the control
  reservation.
- I20 origin: agent A, holding only `send:start`, sends with `quoted {principal: owner, kind: human}`; admission-time
  `from` stays A, unchanged. Dispatch binds origin identity to current core attribution (for example actor
  `extension:coord`) and carries A in `origin.sender` with asserted `inbox-record` provenance. The model envelope
  shows from A via that actor, with both sender/classification provenance and the quote visibly asserted, never
  as the owner's instruction or a core proof of asynchronous forwarding. If B declares no accepted rendering,
  the record is held `origin_unsupported`.
- I21 priority: highest eligible priority wins across senders within one target/lane; equal priorities use sender
  round-robin and then per-sender FIFO. A sender's high mail can overtake its earlier low mail. Default is normal;
  a pending priority edit races claim by CAS, never modifies a claimed record. High never bypasses a gate, quota,
  eligibility hold, lane or control reservation. Sustained high traffic may leave low held; no aging is promised.
- I22 ordering visibility: an unknown predecessor blocks dependent waiting mail; operator views and bounded
  get/list results show held `ordering_unknown` plus that predecessor's delivery id. Observation does not resolve
  or resend it. An explicit authorized resolution lets scheduling reevaluate the successor's other eligibility
  checks, never replay the unknown predecessor.
- I23 recovery funding: saturate normal content, then admit a bounded interrupt from the normal control share
  (64 KiB / 16 records unavailable to content). Potentially uncertain controls stop at normal quota before
  another native invocation; an explicit one-record archive still freezes/exports/frees with no resend.
  Non-archive recovery cannot consume the defensive archive-only 64 KiB / 16-record margin inside the 10%
  reserve. Accepted plans pre-fund 16 KiB / 4 future records, reduced to 8 KiB / 2 after export; scope receipts
  retain at most 16. A known-metadata negative control need not exhaust without the defensive margin and is
  not evidence that the margin is necessary.

**H. Pause and scope resume** (R's traces). H1 crash before claim: no effect until an explicit current-epoch resume,
then at most one claim. H2 crash after durable claim: `unknown` through any resume, never invoked again. H3 the runtime
inserts natively held input while the inbox is paused: no duplicate, no claim of physical suspension. H4 pause/claim
race: pause wins → no claim; claim wins → pause reports the outstanding attempt. H5 a restored open-gate record or a
stale resume request creates no permit. H6 a member added after the preview makes the resume `conflict`; repeating a
committed `op` returns the original target set. H7 moving an active member keeps its gate; no earlier resume is
replayed; undoing the move changes selection only. H8 absent, unsupported and grant-blocked members report their
reasons while ready ones admit; no wait for a parent's work. H9 a caller lacking `resume` on one member, its own agent
included, is refused for the whole scope before any change. H10 an expired or stale-guard `preempt` is never renewed
or run by resume. H11 an owner restart during resume starts closed; the lookup reports the historical result and
creates no permit. H12 controls and answers are not queued behind held records; resume never answers an old request.
H13 a handle rebound to a new agent moves no tree, gate or unknown claim. H14 deep nested teams with absent members and
a stalled viewer: expansion and results stay bounded, excess is refused before changes. H15 read-only startup shows
membership, held records and unknown operations before any write. H16 a team of 200 agents resumes in one scope
action, frozen at one membership revision, with paged results.

**G. Principals and grants.** P2's prototype (`proto/principal-undo` f12f4e2, gate 29/29) passes G2, G3 (owner and
additive grants), G5, G6 and G10; G1, G9, G11, G12 and G14 are `(pending C2)`. G1 the default principal comes from the
connection's known identity (TUI → owner, a spawned pane → its launcher's agent principal); a hand-started agent
in a human-opened shell inherits that shell's `VQRO_PRINCIPAL` (including owner), not a guessed agent identity.
A declared label is recorded separately and never changes it. G2 a pane's CLI writes as Alice, Bob writes later, Alice's TUI `u` removes
only Alice's change; another principal's change is revertable by explicit id and stays attributed. G3 additive union,
expiry, team and project subjects; no grant → `no_grant`; the owner holds all rights. G4 the resolved strategy is
checked at claim. G5 a raw call with no principal is accepted, recorded `undeclared` (never owner), holds no grants
and has no default `u`. G6 restart keeps grants and attribution. G7 a non-owner worker bound to `@worker` with no
grants: self-resume, grant issuance, membership move and handover are all refused; an explicit `resume` grant on
itself enables only that. G8 `grants.manage` on X cannot grant on `*`, on Y, or rights it does not hold. G9 delegation
and propagation: owner P invokes a Settings command in extension E; E's writes during the callback have `principal` P
and `author` E, and P's `u` undoes them; a later autonomous E write belongs to E; an expired or cross-connection
`invocation` proof fails; worker W with no delegation row cannot become P by label or `on_behalf_of: P`; with
`doc/principals/delegations/<id> = {from: P, to: W, until_ms}` W's `on_behalf_of: P` call records `actor` W, `initiator`
W and `principal` P until expiry, and P's `u`, not W's, undoes it; a row naming a wildcard or a chain (W acting for P
through Q) is refused; the row has no per-right or per-target scope, and what W may do as P is limited only by P's
coordination grants; two callers using the same local
op id through E stay separate; combined: W, acting `on_behalf_of: P`, invokes E's command, and E's writes during the
callback record `author` E, `actor` E, `initiator` W, `principal` P; P's `u` undoes them, W's and E's do not; E has no
standing ability afterwards. An opaque callback proof in legacy `on_behalf_of` is refused; `invocation` remains
the callback proof field, while explicit `on_behalf_of: P` uses the delegation row rule.
G11 tool-process attribution: a tier-1 tool's CLI inherits the launcher-tagged pane principal. Select the
headless per-endpoint environment case only for `data.agent.caps.agent.tool_principal: "env"`: each tool
process receives its
own endpoint's launcher-tagged `VQRO_PRINCIPAL` and workspace `VQRO_SOCKET`. That id is independent of `rN`;
a single-endpoint host passes its launcher-given values to the child, never an inherited human-shell owner
principal under an `env` claim. With a many-endpoint host, tools from two endpoints
must still resolve to distinct correct endpoint principals using native per-endpoint/per-thread support or
separate runtime children; a shared process-wide principal fails that declaration. If the native route is not
provided, omit the capability and record the per-endpoint attribution gap. Select the **shared-host** case only
when the host provider opts in to export a verifiable host-context token to tool children it launches: CLI calls
from tools of two endpoints both bind the correct core-assigned `host:<rid>`, verified like an extension token,
never guessed as either endpoint. A raw `hello.identity` host assertion still rejects; absent/unverified context
never establishes host identity. Human-opened shells do not inherit the host token and receive no host attribution
from it. `host:<rid>` acts for an endpoint's principal only through a delegation row with `on_behalf_of`; this
route never upgrades the missing per-endpoint `env` declaration.
G12 write ownership, checked against the state before the transaction and the connection's **actor**, never its
effective/delegated principal: a non-owner actor's write, delete or revert of `doc/principals/owner` is refused;
W cannot create `{from: P, to:
W}`, cannot replace its own `{from: W, to: W}` with `{from: P, to: W}`, and cannot overwrite P's row by changing
`from` to W; deleting or reverting a row needs its existing `from` or the owner; a row created in a transaction grants
nothing within it. Even when acting for P/owner through a delegation or callback, actor W/E cannot create,
replace, delete, extend or revert P's delegation rows or write the owner row on that effective principal's authority;
only its own actor ownership (or the owner actor) permits the write. The owner's
bootstrap is audited and not a `u` target; an expired `until_ms` row admits nothing. An owner-invoked extension
cannot manage the owner key or rows whose `from` is another principal; direct owner TUI/CLI management works,
while rows owned by the extension's own actor still follow the same ownership rules. G10 a grant query is not
authority: a coordinated operation
with a missing, unknown or revoked grant dispatches zero native writes; raw primitives still work.
G13 handle grants pin both ends at issuance: grant on `@worker` to `@reviewer`, rebind both handles, and verify the
right stays on the original exact agent for the original bound principal; the new bindings receive nothing. A
subject handle without an unambiguous principal is refused. Team/project grants still follow recorded membership.
G14 principal recovery and inheritance: `hello` cannot assert `extension:`, `host:` or `system:` ids (`invalid`);
backend/supervised child environments do not inherit caller identity variables, including `VQRO_PRINCIPAL`.
Missing/invalid owner configuration gives the TUI `owner-unset`, no coordination grants and a visible bootstrap
notice, while raw/recovery still works. No connection/environment/process is guessed to be the owner. The audited
startup bootstrap is unchanged. With a missing/invalid owner, an explicit public bootstrap supplying P succeeds
only if serialized prepare still sees a missing/invalid row; it records an audited system transaction, not a `u`
target. Race two supplied principals: at most one establishes the owner; the other is refused once the owner is
valid. A later bootstrap cannot replace a valid owner, and ordinary non-owner writes/reverts still fail G12.

**T. Pull** (P5; Pi pinned per run). T1 OSC 7501 and marker mapping; nothing typed into the terminal by default;
without a plugin, records stay held `integration_unavailable`. T2 eligibility comes from the point: a `turn` take
never returns `settle` or `start`; a `tool` take mid-run leaves a queued `settle` pending; `preempt` is never pulled;
one take batch in flight per target; a batch over the page bound returns what fits; records arrive in selection order in exactly
the next model request. T3 an abort right after take leaves the message inserted and unread; a context hook or
`message_start` never produces `read`. T4 `released` only when the handler returned without the entry; a crash between
take and insertion → `unknown`. T5 restart: `coord.inbox.reconcile` marks open claims `inserted` from the session
entry carrying the delivery id, else `unknown`; it grants no claim; a later report from the old consumer is rejected;
a repeated take returns history, never content. T6 a delayed report for claim C1 after release and re-take as C2 is
rejected. T7 a paused target's take returns `held: paused`; the wake carries only counts and a revision. Editing
held mail advances that revision and wakes the plugin even when counts are unchanged. T8 edits up to the take
reach the model; later ones are `too_late`; a human draft in the editor is byte-identical afterwards. T9 a plugin
endpoint supersedes the marker by a recorded handover. T10 `insert: context` with `evidence: none` reports `supplied`,
never `inserted`, and is shown as context with its origin. T11 content filtering: with `accept: ["text"]`, a
record containing an image stays held `unsupported_content` for that consumer through repeated safe points, with
zero claims/releases; text-only mail remains eligible. A consumer accepting images may take it, without flattening.
T12 Claude Code context evidence: PostToolBatch/Stop and interactive rewake hand-offs report `supplied`, never
`inserted`; `hook_success`, a tool result or a human quote of the hand-off never yields `read`. Only a correlated
model-visible request plus normal completion may yield `read`; verify native framing on each path, including
`NOT USER INPUT` on rewake. Claude declares `pull.points.idle.precision: "approximate"`; its idle hook takes only
`boundary`/`settle`, never `start`. Starting an idle Claude session uses an explicitly selected native session
socket `admit` call or the human, with no implicit mode fallback. Pi's measured idle declares `precision: "exact"`.
Non-interactive idle wake is not declared.

T13 advisory terminal status: `agents.status` publishes OSC-shaped live data; unknown/missing states become
`unknown`. Duplicate/lower sequence reports are ignored per terminal, even from a different app; invalid reports
produce no notice or native effect. Terminal ids are at most 64 characters, app is 1–64 printable characters,
seq is 0..2^53−1: a huge sequence (including 10^100) is invalid and cannot poison later reports. Count over 1024
or encoded-table size over 960 KiB returns `overloaded` without recording anything. Concurrent publication never
lets an older table overwrite a newer one. Reports are dropped at terminal end/removal and lost on extension
restart. Stored sequence advances for unchanged state/app without a publication; published `seq`/`at_ms` may lag
until a publication (including another terminal's change). The sidebar prefers terminal OSC status and falls back
only without it; no report never
implies idle. Reporter ownership `(pending C2)`: the terminal's launcher-tagged principal and owner may report;
other principals get `not_authorized` without applying a report.
T14 per-record consumer budget: a take with `max_bytes` below a record's size leaves it held
`too_large_for_consumer` for that agent through repeated safe points, with zero claims/releases and no truncation;
smaller eligible mail remains selectable, aggregate batch bounds still hold, and a sufficient budget permits a
first claim only when the other eligibility checks pass.
T15 path framing/precision: every served point declares exact/approximate precision and its added framing via
`pull.framing`. Check native model envelopes for turn, settle and idle independently, including Claude's rewake
`NOT USER INPUT`; renderers never hide a path's actual role/authority or relabel it plain user input. At an
approximate idle point, race a human prompt: the claimed record still meets its own boundary/settle contract,
never an assumed-idle start. Offer a `start` record to an approximate idle hook: it stays unclaimed; it is not
converted to `boundary`, `settle` or `admit`. A plugin unable to preserve the boundary/settle contract makes no
claim. Exact points retain their native guarantee; observation gaps are not interpreted as exactness.

**L. Lifecycle policy.** L5 stop closes the gate first; without a declared `agent.stop`, `host_kill` is offered only
when the host is a core resource of the bound incarnation with a declared termination primitive, after showing
everything it serves; a process or app-server host without one gets `unsupported`. L6 at end: held records stay held
`agent_ended`; claimed without outcome become `unknown`; nothing is resent or rerouted; a send to the ended handle is
rejected. L7 at ~900 unavailable agents per hour (the measured soak rate): the identity cache stays within `N`;
evicting an entry makes a return a new agent; durable evidence is never dropped silently; tombstones without evidence
are pruned oldest first; identities and summaries of maximum legal encoded size (256 control characters in the
instance, 4 handles) stay within 2 KiB and 3 KiB; a native result larger than the compact outcome is stored as its
size, sha256 and `incomplete: true`, with the full result only in the live reply and `agent.receipt`. L8 at the
durable budget, after GC, operation-window eviction and alias growth: a new send is refused `quota`; a late native
reply to an already admitted record still records its outcome (its space was reserved at admission); agent
registration still succeeds; an explicit `coord.inbox.archive` exports and frees space. L9 an explicit `retire` of a
disconnected agent shows `retired` with native execution unknown, never `ended`; routing stops; later end evidence
makes it `ended`. L10 membership is ordinary configuration only: with the protected domain full, a membership move
still succeeds, its author's `u` and an explicit revert undo it, a scope action frozen before the undo keeps its
recorded set, and the tombstone keeps only `{rN, revision}` as its reference. L11 archive: crash or uncertain
durability at each step (after freeze, during export, after export before free) keeps every record; a late receipt or
an edit during export leaves that record in place; a failed destination write frees nothing; a repeated archive `op`
inspects the original archive; an archived delivery's lookup returns its locator, and a missing archive answers
`unknown`.

**Q. Request routing.** Q1 launch defaults: owner-started agents record `human`, coordinator-launched agents
record `coordinator`; changing the ordinary attributed configuration needs the owner or `configure` on that
agent and follows transaction/undo rules. Q2 `coordinator` delivers request mail to the exact recorded
`reports_to` agent currently holding `answer`, not every grant holder; the mail identifies the request's agent,
epoch, id and revision. Missing coordinator, ended/unavailable endpoint or missing grant falls back to the human
notice/popup with a visible reason. Q3 `both` provides coordinator mail plus the human notice; racing valid
answers from the coordinator, owner and another answer holder produce one native response, with answered state
visible to the others. Q4 invalid/stale epoch/revision/scope, expired request or revoked answer grant rejects
before a native response; unanswered requests keep native expiry and are never auto-approved or renewed.

**S. Scale gate.** Streams measured by P3, inbox persistence by P1; the combined end-to-end gate is still to run: 600
terminals / 200 agents, 1 and 3 viewers per agent (3 needs the stream-capacity change when one provider serves all
agents, `agent-v0.md` §9.2), one stalled viewer, a 200-attach burst that must not hit the effect limit, steady and
burst inbox traffic, an inbox flood, reconnect storms. Record CPU, RSS, commit p50/p99 (inbox and layout), event-log
retention and `missed`, workspace and inbox occupancy, control/answer latency. Pass: the supported envelope leaves
layout, control and recovery headroom, excess is rejected predictably, and a slow viewer harms no one.

## Running it

Fakes: `tests/fixtures/fake_pi.py` extended with the timeline verbs, and a fake ACP agent. Every run uses a private
root and removes every process and file it created. Optional real-runtime smoke runs record metadata only and never
spend model calls in `check.sh`.
