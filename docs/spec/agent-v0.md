# vqro agent integration contract, v0 (draft)

> **Status (2026-11-19): draft, not implemented** (D28). vqro is a working prototype, and fluid. Prototype
> results P2–P6 are folded in. Nothing is stable until Pi and two genuinely different real runtimes
> (Codex app-server and Claude Code) pass Part A of `agent-v0-conformance.md`.

**Audience: adapter authors.** This contract says what an agent integration exposes to vqro: endpoint identity,
capabilities, facts, an item stream, send, interrupt and requests, honest evidence, optional modules and lifecycle
evidence. It says nothing about inboxes, grants, names, teams or pause: that is vqro's coordination standard
(`coordination-v0.md`), implemented by a default, replaceable extension on generic core primitives. An adapter only
needs this document.

It reuses `provider-v0.md` (facts, `resource.invoke` → `effect.invoke`, D10 outcomes, deadlines, credited streams) and
`core-protocol.md`, and needs generic core additions (§9). vqro reports what the layer below said and never
compensates (D10, D14). There are no per-protocol profiles; adapters are judged by conformance.

## 1. Shape

| Part | What | Who |
|---|---|---|
| Core (§2, §4) | endpoint model, ids, facts, evidence, capabilities, causality, item stream | every structured adapter |
| Interaction (§3, §5) | `agent.send`, `agent.interrupt`, requests | adapters that accept input |
| Modules (§6) | native queue, sessions, config/commands, history, children/background, display terminals, stop | as declared |
| Lifecycle evidence (§7) | live, stopping, disconnected, unknown, ended | every structured adapter |

A terminal (tier-1) agent needs none of this and stays usable by hand; its pull plugin follows
`coordination-v0.md` §7.

**ACP.** Compatibility is published row by row: each row of the coverage matrix (conformance §A5) binds an ACP feature
at commit `d8cd473e` (2026-11-19; separate v1 and v2 schemas) to normative fields here and passes its trace. P6's
model walks 406 rows (386 native, 6 composed, 14 unsupported). The claim is **ACP v2 base, plus v1 except MCP**: v1
requires agents to support MCP stdio servers, vqro uses no MCP and offers no pass-through, and the gap is stated
openly (the human's decision). Authentication is a visible gap (§6.8); fork and cursors are outside the v2 base. An
ACP fake is a fixture, not an implementation. A vqro **ACP facade** replies to v1 prompts at observed foreground
completion and to v2 prompts at insertion with the retained native message id; these waits are not long-running core
effects.

## 2. Core

### 2.1 Endpoint model and ids

Three dimensions stay separate:
- the **endpoint** `rN`: one logical agent vqro addresses, a provider resource of kind `agent`;
- its **session**: the runtime conversation it currently holds (at most one at a time, none before the first `new`;
  an endpoint may switch sessions);
- its **host** `{id, provider, instance, incarnation, kind: terminal | process | server | _, owns_lifecycle,
  resource?}`: the execution host, qualified by the provider instance and incarnation that reported it (`resource` is
  its core id when it is one). One host may serve many endpoints (an app-server with many threads);
  `owns_lifecycle: true` declares that the host's end entails this endpoint's end (true for a bridge that owns its
  runtime child). Only an end event for the bound host incarnation counts; a late event from an old one does not.

A **run** is an interval of foreground work, possibly started without vqro input.

| Id | Meaning |
|---|---|
| `rN` (printed `agent:rN`) + `incarnation` | exact endpoint address; core resource id, never reused (provider-v0 §2) |
| `session`, `host.id` | runtime conversation id; execution host id |
| `delivery` | the caller's opaque id for one input (the coordination standard mints it); keys receipts |
| `epoch` | observation epoch, minted by the adapter (§4) |
| entity, request ids | runtime-native or adapter-minted, unique within their epoch |
| `cause`, `correlation` | optional causal links (§2.5) |

Ids are strings ≤ 128 bytes, compared exactly, opaque except to their minter. Panes, processes and hostnames are never
addresses (D23). Times are integer Unix epoch milliseconds in fields ending `_ms`, as in the core.

### 2.2 Evidence

A stage is recorded only from evidence for that stage, never inferred from silence, text, order, an emptied queue, a
hook firing or a run completing. `queued`, `inserted` and `handled` are alternatives, not a line.

| Stage | Source | Means |
|---|---|---|
| `dispatched` | provider outcome | handed over; no acknowledgement exists |
| `accepted` | provider outcome | acknowledged; `receipt.stage` says what |
| `admitted` | receipt | acknowledged, placement unknown (Pi's `success: true`, which does not tell `handled` apart) |
| `queued` / `inserted` / `handled` | receipt | held by the runtime / in the conversation, native evidence only / consumed by a command or hook, no model input |
| `supplied` | receipt | given to the runtime as context for its next request, with no durable record |
| `read` | receipt | a model request containing it completed normally; evidence of inclusion, not of attention or obedience |
| `completed` | observation | a completion whose run is evidenced to contain this input |
| `rejected` | provider outcome | definitely no effect |

`indeterminate` (core) is unknown, never "not delivered". The coordination standard adds its own pre-dispatch stages.

### 2.3 Facts

The resource keeps the coarse OSC-7501-compatible `status` and carries `data.agent` (≤ 64 KiB). Facts alone render
current state; they carry no per-token stream position.

```jsonc
"data": {"agent": {"v": 0,
  "state": "running",                  // idle | running | requires_action | unknown
  "lifecycle": "live",                 // §7
  "host": {"id": "t3", "provider": "herdr", "instance": "…", "incarnation": "…", "kind": "terminal",
           "owns_lifecycle": true, "resource": "r12"},
  "completion": null,                  // last {id, reason, error?}; ids never reused
  "session": {"id": "s1", "title": null}, "epoch": "e7",
  "caps": {"rev": 3, …},               // §2.4
  "queue": null,                       // native held input (§6.1); null = not observable
  "requests": [{"request": "q4", "revision": 1, "method": "permission", "title": "Run git push?"}],
  "background": null,                  // null = unknown, [] = known none
  "config": [], "commands": [],        // §6.3
  "usage": null, "incomplete": []}}    // incomplete: fields cut to fit; unknown, not empty
```

`state` never becomes `idle` without evidence; consumers treat unknown values as busy. Completion reasons are
`end_turn | cancelled | error | max_tokens | max_turn_requests | refusal | unknown`, open; unknown native reasons are
preserved, never mapped to success. Coarse mapping: `running` → `working`, `requires_action` → `blocked`, idle after
`error` → `error`, else `idle`.

### 2.4 Capabilities, versioning, common arguments

- The provider declares `agent {v: 0, modules}` and each op at `provider.register`: the upper bound. Each resource
  publishes its restriction in `data.agent.caps` with a `rev`, republished whenever a runtime setting changes what a
  declaration means (a queue mode, say). A declaration covers only input the runtime has not yet accepted; where the
  runtime applies its current setting at consumption (Pi), the declaration says so (`batch_at: consume`).
- **Provider bound vs resource caps.** The provider's declaration has the same structure as resource caps but is a
  superset descriptor: every enumerated field holds the **set** of values any of its resources may publish, every
  numeric limit the **maximum**, every boolean `true` if any resource may be true. A resource's caps give **one**
  value per field and are valid iff, field by field, the value is in the provider's set, a number is at most the
  provider's maximum, `true` only where the provider says `true`, and every op, mode and sub-object is present at
  provider level. Moving between allowed values (Pi's batch `one` ↔ `all`) is a `caps.rev` republish, never a
  re-registration. `"agent.send": {}` at provider level means the op exists with no sub-features (no modes, so
  unusable), never "unrestricted"; an absent field means not supported.

  ```jsonc
  // provider.register:  "agent.send": {"modes": {"settle": {"batch": ["one", "all"], "idle": ["start"]}}}
  // data.agent.caps:    "agent.send": {"modes": {"settle": {"batch": "one", "idle": "start"}}}
  ```
- `caps.host {kind: terminal | process | server, endpoints: "one" | "many"}` declares whether the adapter's host can
  serve several endpoints (an app-server) or exactly one (a bridge that owns its runtime child).
- Every `agent.*` op takes `args {v, caps_rev?, epoch?, guard?, cause?, …}`. The core checks only that the op is
  declared. **The adapter rejects** before any native write (`unsupported`, `stale` or `invalid`): an unknown action,
  args outside its `caps`, a stale `caps_rev` or `epoch`, a precondition it cannot check as strongly as declared, and
  undeclared content types. It never drops, flattens or weakens.
- Adapters declare only native behaviour; compositions belong to the coordination layer, labelled with their steps.
- Enums are open and shown generically when unknown; `_` names are implementation-specific. Additive changes keep
  `v: 0`; breaking ones bump it; a module may carry its own `v`.

### 2.5 Causality

Optional links: `cause {kind, id, endpoint?, epoch?, boot?, provenance}` (what directly led to this: a delivery, a
request, an entity, an effect) and `correlation` (a caller-chosen thread through many operations). References are
qualified: entity and request ids by `endpoint` and `epoch`, effect ids by the core's `boot` epoch; delivery ids are
global. `provenance` says who stands behind the link: `asserted` (supplied by a caller), `adapter` (the adapter
proved it, e.g. it inserted this delivery) or `native` (runtime evidence). Callers may pass them on `agent.*` ops;
adapters echo them on receipts and on the items they can prove the op produced; the core carries them on effects and
effect events. Nothing is inferred from timing or text; absent means unknown.

## 3. Interaction

### 3.1 Modes

A mode says when the runtime takes input relative to current work. Every declared mode states both edges.

| Mode | Agent busy | Agent idle |
|---|---|---|
| `start` | `busy: reject` | starts work |
| `boundary` | joins current work at the declared `point`, `batch: one \| all` per point | `idle: start \| reject \| park` |
| `settle` | waits until current work would stop (`point: settled` after final settlement, or `would_stop`, which may precede it); `batch: one \| all` | `idle: start \| reject \| park` |
| `admit` | runtime admits it in its own order; when it is read is unspecified | starts or admits |
| `preempt` | stops current work, then continues the same conversation with this input | as `start` |

- `admit` is the weakest mode. No mode falls back to another. `park` means the runtime holds idle input until a later
  start; it is not a start.
- `point` names the boundary (`after_tool_calls`: current tool calls finish, before the next model request) or a
  documented `_` point. A native op with an unspecified point must not claim a precise one.
- `preempt` is declared only if native. It never implies rollback.

```jsonc
// Pi, as measured (P4, proto/pi-semantics de2a7db tests/prototypes/pi_semantics/capabilities.json)
"agent.send": {"modes": {"start": {"busy": "reject", "preflight": "serialized"}, "admit": {},
                         "boundary": {"point": "after_tool_calls", "batch": "one", "batch_at": "consume", "idle": "park"},
                         "settle": {"point": "would_stop", "batch": "one", "batch_at": "consume", "idle": "park"}},
               "content": {"types": ["text", "image"], "shape": "text_then_media"}, "correlation": "none", "handled": "none", "literal": "controlled_launch",
               "origin": [{"as": "user", "role": "user"},
                          {"as": "attributed_user", "role": "user", "frame": "sender_header"}],
               "keys": null, "guards": {"session": "adapter", "run": "none"}, "receipts": {"retain": 256}},
"agent.interrupt": {"pending": "unknown", "scope": "foreground", "waits": "idle"}
```

### 3.2 `agent.send`

`args {v, caps_rev?, mode, content, delivery, origin, quoted?, render?, key?, guard?, cause?, correlation?}`; result
`{receipt:
{delivery, stage, native?, run?}}`. `content` is **always an array of typed blocks**, each with a `type` field, e.g.
`"content": [{"type": "text", "text": "go"}]`; never a bare string or a single object. Block types: `text {text}`,
`image {mime, data}`, `audio {mime, data}`, `resource_link {uri, name?, mime?}`, `resource {uri, mime?, text | data}`.
The adapter declares `content {types: [...], shape: "ordered" | "text_then_media"}`: `ordered` accepts any sequence
of the declared types; `text_then_media` (Pi) accepts only zero or one text block followed by zero or more media
blocks. Anything else is rejected `unsupported` before any native write; blocks are never reordered, flattened or
concatenated.

- **Origin.** `origin {principal, actor, agent?, kind: human | agent | tool | _}` is the effective sender, taken by
  the caller layer from the recorded caller (`coordination-v0.md` §3), never from request text. `quoted? {label,
  principal?, kind?}` is separately asserted provenance (a forwarded message's original author) and is rendered as a
  claim, never as the sender.
- **Rendering** is declared per runtime, each with its native role and resulting authority:
  `"origin": [{"as": "user", "role": "user"}, {"as": "attributed_user", "role": "user", "frame": "sender_header"},
  {"as": "participant", "role": "<native>"}, {"as": "context", "role": "system_reminder", "authority":
  "above_user"}]`. Plain `user` is only for content whose effective sender is the workspace owner acting as a human.
  Anything else is framed with its sender (`attributed_user`, `participant`) or, only if the call's `render` list
  names it, `context`; it is never placed in a system or developer role. The adapter uses the first rendering in
  `render` (default `[participant, attributed_user]`) that it declares, and rejects (`unsupported`) if none fits. The
  inserted message carries `origin`, `quoted` and the rendering used. This is provenance at the model-request
  envelope, not prompt-injection immunity.
- Busy rejects with `error.kind: "busy"`, a failed guard with `"stale"`: both before any native write, as `rejected`,
  never as an error the core would report `indeterminate`.
- An adapter never injects Enter, runs a slash command found in literal text, or changes mode, model or queue
  settings as a side effect. It declares `literal`: `always`, `controlled_launch` (literal only when the runtime is
  launched without template expanders or input interceptors; Pi, whose per-call `expandPromptTemplates: false` is
  ignored) or `none`, and rejects text it cannot send literally; it never escapes or rewrites it. Explicit commands
  use `agent.command` (§6.3).
- A native acknowledgement maps to `admitted` unless the runtime evidences more; never to `inserted`, a run start or
  completion. `handled` needs an explicit witness; `handled: none` (Pi) means it is never reported.
- **Start preflight.** An unresolved native start preflight counts as busy; an idle-looking snapshot is not a start
  guard. Without an atomic native start guard the adapter serializes starts, one preflight in flight per endpoint
  (`preflight: serialized`; Pi: two concurrent preflights both acknowledged while only one input arrived). Interrupt
  does not cancel an unresolved preflight, and a native "settled" seen under such interleaving is no completion.
- **Long native admission.** The provider deadline bounds the provider reply, not the native lifecycle. Without
  acceptance or insertion evidence by the deadline, the adapter replies `dispatched`, keeps bounded correlation for
  that delivery and publishes the later receipt; it never fabricates `accepted`, holds the effect for the whole
  native request, or writes the input again.

### 3.3 `agent.interrupt`

Stops current foreground work; sends no content. Declared: `pending` (runtime-held input afterwards:
`kept_continues | kept_parked | cleared | unknown`; Pi is `unknown`: a steer can enter the session during a tool
abort without reaching a model, while follow-ups stay parked), `scope` (`foreground | all_work`), `waits`
(`idle | none`). Outcome `accepted` (`observed` with `waits: idle`); completion reason `cancelled`. It withdraws only
the requests the runtime stops waiting on. Interrupting an idle endpoint is a no-op.

### 3.4 Correlation, keys, guards, receipts

- `correlation` (capability): `insertion` (the inserted message carries the delivery id), `acceptance` (only the
  reply identifies it) or `none`. With `none`, an observed user message has no `delivery`; consumers must not guess.
- `keys`: `{scope, retention}` of native insertion keys, or `null`: scope is the session, retention the runtime's own
  retained-message lifetime including its restarts; an adapter's recent-effect cache is not that. A key never
  authorises a resend and never deduplicates downstream tool effects; a lookup that finds nothing is `unknown`. Safe
  automatic retry is possible only where such native idempotency is declared and proven.
- `guards`: `session` and `run` (`expectedTurnId`-style) preconditions with strength `runtime` (atomic) | `adapter`
  (serialized in the bridge) | `none`. A stale guard rejects; it never applies to new work.
- `agent.receipt {v, delivery}` → `{stage}` or `{stage: "unknown"}`. Adapters keep at least `receipts.retain`
  receipts per endpoint through journal pruning; absence is `unknown`, never "not delivered".

## 4. Observation: item stream

A per-viewer credited stream of JSON records (core addition §9.1). P3 confirmed it at 200 agents × 3 viewers (§8).

- **Attach:** `resource.attach {resource, items: {after?: {epoch, seq}}}` → `{stream, epoch, initial: "continue" |
  "reset"}`. `continue` only if `after.epoch` is the current epoch and `after.seq + 1` is still retained; records
  then start at `after.seq + 1`. Otherwise `reset`: a snapshot at a cut, then only records after it. The provider
  gets `stream.open {…, channel: "items", after?}` with terminal-stream credit, window and `slow_consumer` rules and
  no dimensions. UTF-8 JSON Lines, ≤ 64 KiB per whole record; a line may span frames. A viewer opening many streams
  (first paint, reattach after a restart) windows its attaches under the core's limit of 128 in-flight requests per
  connection rather than sending them all at once.
- **Records:** `put {seq, kind, id, rev, value}` (whole entity); `append {seq, kind, id, base, rev, field, text}`
  (applies only if the current `rev` is `base`; `field` is a JSON Pointer to an existing string) or, for a bytes field,
  `append {seq, kind, id, base, rev, field, bytes}`: `bytes` is one base64 chunk that the consumer decodes and appends
  to the field's byte buffer. String append never applies to a bytes field, and base64 text is never concatenated (the
  chunks `/w==` and `AA==` are the bytes `ff 00`, not the string `/w==AA==`); a `put` carries a bytes field as one
  base64 encoding of the whole buffer; `remove {seq, kind,
  id, rev}`; `notice {value}` (live only, no `seq`); `snapshot {phase: begin, epoch, coverage, session?}` …
  `snapshot {phase: end, seq, session?}`. A native first chunk with no prior entity arrives as a `put` of a typed
  base, then `append`s. Every record names its kind in `"op"` and carries exactly these fields; there is no other
  record shape and no dual acceptance (the P3 prototype's `reset`/`snapshot_end`, `remove` without `rev` and string
  values do not carry over):

  ```jsonc
  {"op": "put", "seq": 812, "kind": "message", "id": "m41", "rev": 3, "value": {"position": 40, "role": "assistant", "content": [{"type": "text", "text": "Runn"}], "final": false}}
  {"op": "append", "seq": 813, "kind": "message", "id": "m41", "base": 3, "rev": 4, "field": "/content/0/text", "text": "ing…"}
  {"op": "append", "seq": 815, "kind": "terminal", "id": "x2", "base": 7, "rev": 8, "field": "/output", "bytes": "/w=="}
  {"op": "remove", "seq": 814, "kind": "tool", "id": "t9", "rev": 5}
  {"op": "notice", "value": {"severity": "warn", "text": "rate limited"}}
  {"op": "snapshot", "phase": "begin", "epoch": "e8", "coverage": "all_retained"}
  {"op": "snapshot", "phase": "end", "seq": 951}
  ```
- **Snapshot cut.** The adapter takes **one serialized cut**, sends `snapshot begin`, a `put` per retained entity as
  of that cut (no `seq`), `snapshot end` with the cut's `seq`, then every later change in order. Changes during the
  snapshot are buffered within the declared bound; past it the stream closes with a gap and the viewer installs
  nothing. The viewer installs the snapshot only at `end`, and never continues from an unfinished one.
- **Replay.** A runtime's replacement replay (resume, child load) is fenced like a snapshot, with its own begin and
  end per replayed session; a truncated replay is never installed as authoritative.
- **Epoch.** `seq` is contiguous within an epoch; records carry no epoch. The epoch changes when the adapter loses its
  journal and on every session switch; an open stream then gets a new `snapshot begin` before any new-epoch record, or
  is closed. Entity identity is `(epoch, kind, id)`.
- **Order.** Timeline entities carry `position` (fixed at first publication); snapshots and viewers order by it.
  Revisions increase per entity; an `append` with the wrong `base` is a gap (reattach). Records at or below the
  applied `seq` are ignored.
- **Slow viewers** close `slow_consumer` and reattach from their last applied `{epoch, seq}`; others and native event
  draining never wait. Closure follows the per-stream rule within one stream budget shared by the connection, so it
  may end all of a viewer's streams at once. A viewer discards a partial line when a stream ends. Falling out of
  retention while attached ends the stream with gap reason `retention`; an unknown gap reason is a protocol error that
  still ends the stream. Adapters declare `items.retain_bytes` in facts. **Coverage** `tail`/`state_only` is never
  shown as complete; current state and pending requests survive pruning.

**Entity kinds** (unknown kinds preserved and shown generically; `native` holds lossless extras; `thought` is only
reasoning the runtime exposes for display):
- `state` (id `current`): `state`, `run?`, `completion?`, `background?`.
- `message`: `position`, `role: user|assistant|thought|coordination`, `content`, `final`, `delivery?`, `origin?`,
  `cause?`, `participants? {sender_session, recipient_session, sender_message?, recipient_message?}` (native
  inter-session messages: observation only).
- `tool`: `position`, `title`, `status: pending|running|completed|failed|cancelled`, `input?`, `content?`,
  `locations?`, `diff?`, `terminal?` (a display-terminal id, §6.6).
- `terminal` (display terminal): `output` (a bytes field: base64 in a `put`, decoded-byte `append` with `bytes`),
  `exit? {code?, reason?}`,
  `cols?`, `rows?`.
- `receipt` (id = its `delivery`): `delivery`, `stage`, `native?`, `run?`, `cause?`; one entity per delivery, whose
  later stages are revisions of it.
- `plan` (whole), `compaction`, `request` (§5), `session` (id `current`), `association` (§6.5).

## 5. Requests

A request is an entity plus a summary in facts: `{request, revision, method, title, params, choices?, schema?, scope,
status: pending|answered|cancelled|expired, expires_at_ms?, incomplete}`. `scope` is exactly one of `{session, run?,
subject?}` or `{operation}` (e.g. authentication before any session); it is independent of foreground state.

| `method` | Response (every method also accepts `{action: "cancel"}`) |
|---|---|
| `permission` | `{choice}`; each choice has `effect: allow_once\|allow_always\|reject_once\|reject_always` |
| `choice` / `confirm` / `text` | `{choice}` / `{value: bool}` / `{value: string}` (`multiline?`) |
| `form` | `{action: accept\|decline, content?}`; `schema` is a JSON Schema object |
| `url` | `{action: accept\|decline}`; accept is consent to navigate, not completion |

- Declared: `agent.respond {methods: [...], scopes: [session | run | operation ...], background_requests: true |
  false}`; `background_requests: true` means requests outside the foreground run can be pending at the same time.
- `agent.respond {v, epoch, request, revision, response}`. The adapter validates epoch, revision, `pending` status,
  expiry, that the scope is still live and that the response itself is valid (choice, schema, enum, range), and only
  then claims the revision: the **first valid answer wins**; anything else is `rejected` (`stale` or `invalid`) before
  any native write. A reused native request id never reuses a standard `request` identity within an epoch. Cancel is
  distinct from denial, `false` and empty text.
- `accepted` only with a native acknowledgement, else `dispatched` (Pi UI responses today).
- After reconnect the adapter verifies, reissues or cancels pending requests; an unknown native outcome is never
  assumed resolved. Unrepresentable and unknown methods are cancelled once and reported, never auto-approved. An
  `incomplete` request is never approved; if its full entity exceeds the entity bound it is cancelled as
  unrepresentable. Who may answer is the coordination layer's concern.

## 6. Modules (declared per resource in `caps`)

1. **Native queue.** `agent.queue {v, epoch, action: list|clear|withdraw|replace, id?, content?}`; facts `queue:
   [{id?, mode, preview}]`. `clear` returns removed content once; null ids allow list and clear only. Native ids are
   distinct from delivery ids; an emptied queue never proves insertion.
2. **Sessions.** `agent.session {v, action, …}`; each action is declared separately. Results are published in facts
   before the reply. A switch keeps `rN` and starts a new epoch.

   | Action | Args | Result |
   |---|---|---|
   | `new` | `cwd?`, `roots?`, `config?` | `{session}` |
   | `resume` | `session` (never sends a prompt) | `{session}` |
   | `fork` | `session?`, `at?` (an advertised position) | `{session}`; invalid position rejects |
   | `list` | `page?` | `{sessions: [{id, title?, cwd?, updated_ms?}], next?}` |
   | `close` / `delete` | `session` | ends the conversation's work / discards retained data |

   Session connection descriptors (MCP servers) are not part of v0.
3. **Config and commands.** Facts `data.agent.config` and `data.agent.commands`, republished on change, cut with
   `incomplete` over the facts bound.
   - Config options: `{id, label, description?, category?, type: select|boolean|string|number, value, choices?:
     [{value, label, description?}], rev}`; `category` is open (`model | mode | thought_level | _…`).
     `agent.configure {v, id, value, rev?}`: a stale `rev` or a value outside `choices` rejects before any native
     write; `observed` only once the runtime reports the new value, else `accepted` or `dispatched`. A change that
     alters what a declaration means bumps `caps.rev` (§2.4).
   - Advertised commands: `{name, description?, input?: {hint}}`. `agent.command {v, name, args?}` runs only an
     advertised name; `args` is a string. Slash text inside a send is never executed (§3.2).
4. **History.** `agent.history {v, page?}`: retained native history as snapshot records from one frozen history cut,
   in bounded pages, with coverage and gaps; the last page gives the live `{epoch, seq}` at that cut. Separate from
   `resume`; supported only where the adapter can prove the cut.
5. **Children and background.** Exposed children are endpoints with their own `rN` and `caps`; an `association
   {child, relation, controls}` entity links them. A child's native controls stand alone: nothing (prompt, resume,
   close, config) is inferred from the root, and a child may allow cancelling its own tool or terminal without
   allowing session cancel. Parentage does not prove that cancelling the parent cancels the child. `agent.object {v,
   epoch, action: cancel, kind, id}` cancels one declared item; an accepted cancel is not an observed end.
6. **Display terminals and content.** Agent-owned terminals are `terminal` entities (§4); they are not backend
   terminals unless the adapter publishes one; `agent.object {action: cancel, kind: "terminal"}` stops one if
   declared. A `tool.diff` is a list of `{path, kind: add | modify | delete | rename, old_path?, before?, after?,
   patch?, binary?}`. ACP v1 client services (`fs.*` with an explicit host and buffer source, `terminal.*`) are
   reserved, separate from display entities.
7. **Stop.** `agent.stop {v, covers: [...]}` asks the runtime to end this endpoint; declared `covers`
   (`foreground`, `queued`, `background`, `children`) and `waits: exit | none` (§7).
8. **Reserved names, no v0 semantics:** `agent.authenticate`, `agent.blob`, rewind/checkpoints.

## 7. Lifecycle evidence

| `lifecycle` | Evidence (only) |
|---|---|
| `live` | `available`, facts present |
| `stopping` | a stop was accepted, no end evidence yet; a refused stop returns to `live` |
| `disconnected` | `available: false, stale: true` (provider or bridge gone), no end evidence |
| `unknown` | the host is gone and does not declare `owns_lifecycle`: the endpoint may still run elsewhere |
| `ended` | the native runtime's own end report for this endpoint, or the end of a host that declares `owns_lifecycle` (terminal `finished` with `exit`, `resource.gone` `exited`/`killed`/`closed`) |

- Interrupt ends the current turn (§3.3); stop ends the endpoint (`agent.stop`, §6.7). An idle completion is never end
  evidence. A viewer, bridge or terminal closing proves only that object's lifecycle.
- At stop or end, runtime-held input is `dropped` only if the runtime reports it, else `unknown`. Nothing is resent.
  A reconnect never undoes `ended`.
- What vqro does with a disconnected or ended endpoint (retention, tombstones, retire, handover) is coordination
  (`coordination-v0.md` §6).

## 8. Bounds

Every read is bounded by count, bytes and work. Stream numbers are P3's measurements (200 agents × 3 viewers plus a
stalled and a slow viewer, at 400, 4,000 and 10,000 records/s, with a session switch on 50 agents and an adapter
SIGKILL: identical reconstruction, 0 errors; one run each on a shared 8-core set):

- retention 128 KiB per agent: the retained log plateaued at exactly 200 × 128 KiB = 26.2 MB (about 580 records,
  12–290 s depending on rate); nothing grew after that;
- while one viewer was slow or stalled, the other viewers' p99 latency stayed 3–11 ms;
- core RSS ≤ 62 MB with 600–678 streams; runtime RSS ≤ 98 MB; core CPU ≤ 44% of one core at 10,000 records/s;
- untested: the provider-side `retention` gap while attached, and a stalled viewer holding more than 78 streams.

| Item | Per entity / record | Aggregate |
|---|---|---|
| Ids | 128 B | — |
| Facts `data.agent` | 64 KiB | provider-v0 resource budget |
| Stream record / entity | 64 KiB / 64 KiB | per endpoint, declared in `items`: journal `retain_bytes` (default 128 KiB), retained entities `entities` and `entity_bytes` in aggregate (defaults 512 and 512 KiB), receipts and request maps; per provider: snapshot buffers and readers. The adapter's steady memory is the sum of all of these plus indexes, not the journal alone (P3's 26.2 MB is the journal). Item streams ≥ agents × max viewers per provider (`limits.max_item_streams`, §9.2); per-viewer connection stream budget 1 MiB |
| Receipts | — | ≥ 256 per endpoint |
| Pages (history, session list) | ≤ 64 items, ≤ 256 KiB | — |

## 9. Core additions (generic)

1. **Item channel on streams** (§4), with gap reason `retention`; an unknown gap reason ends the stream as a protocol
   error instead of leaving it silently open (today `src/provider_client.rs:370`).
2. **Stream capacity sized to agents × viewers** (P3), per provider. Today the core caps a provider at 512 streams
   (`src/provider_wire.rs:193`), so 200 × 3 = 600 failed: 43–45 agents per extra viewer never attached. A provider
   declaring item streams accepts at least `agents × max_viewers` (target 200 × 4 = 800) through a separate
   item-stream budget, `limits.max_item_streams` (SDK default 1024, core cap 8192, the granted value reported back at
   registration), separate from terminal streams. **Stream opens do not count against the effect limit**: the Rust
   SDK's 64 in-flight requests (default, configurable, `src/provider_client.rs:106`; the core admits 128 provider
   calls, `src/provider_wire.rs:23`) turned first paint and every reattach after a restart into retry storms.
   `stream.open` gets its own concurrency budget.
3. **Caller attribution in `effect.invoke`**: `author` carries the recorded fields `author`, `actor`, `principal`,
   `initiator?` and `label?` as defined in `coordination-v0.md` §3 (P2: Rust `EffectRequest` and Python `Effect`
   expose them), so an adapter can record who acted; adapters treat them as data, not authority.
4. **Causality fields** (§2.5) on effects and effect events, passed through unchanged.

## 10. Out of scope (v0)

Inboxes, grants, names, teams, pause (`coordination-v0.md`); operation permissions and sandboxing; physical
suspension; exactly-once; rollback of files or tool effects; MCP.
