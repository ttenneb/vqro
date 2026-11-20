# Controlled native ACP fixture (K1; fixture evidence only)

Launch from K1's staged `tests` tree:

```
/usr/bin/python3 tests/fixtures/acp_conformance.py --wire acp \
  --control /absolute/private/c --ledger /absolute/private/native.jsonl [--script /absolute/timeline.json]
```

Native stdin/stdout is LF JSON-RPC 2.0. The private socket implements K1's existing
`{id,verb,...}` -> `{id,result}` / `{id,error:{message}}` contract. It reuses
`tests/agent_conformance/runtime.py` from K1 `12e2aa1` for control, process lifetime,
nonblocking output, ledger, barriers and input-triggered timelines. The runner
owner must stage that dependency plus both `acp_conformance*.py` runtime files;
no provider wrapper, production test RPC, environment exception or network is used.

## Implemented slice

- One `initialize` negotiates protocol 1 or 2. `session/new` creates at most 32
  sessions. Nonempty MCP configuration is refused, not ignored or tunneled.
- `session/prompt` accepts an ordered, nonempty array of text blocks only. Actual
  native params are logged; no sender frame is stripped and blocks are never
  flattened. The P6 model admits before insertion. V2 replies with `messageId`
  only after insertion; v1 waits for observed foreground completion and replies
  with `stopReason`. P6's native v2 key semantics coalesce matching retained
  inputs; changed keyed input is refused. An absent key never permits replay.
- `session/cancel` cancels foreground work and pre-insertion pending inputs using
  P6's reducer. Cancellation before insertion produces no inserted record. Idle
  cancellation invents no completion. Native notifications receive no reply.
- `session/update` carries v1 user-message chunks, or v2 whole user messages,
  state/config updates and `replay_complete`, through the actual P6 Codec.
- Foreground `session/request_permission` emits allow-once/reject-once options.
  Valid selection uses P6 `Model.answer`; unknown/invalid choices are not approved
  and are cancelled. Native responses get no fabricated acknowledgement. Request
  identities cannot be reused in a session; only foreground/run scope is supported.
- Idle `session/load` (v1) replays retained history before its response.
  `session/resume` (v2) replays only for `replayFrom:{type:"start"}`, with a
  `replay_complete` fence before its response. It uses P6's staged replay reducer;
  unfinished replay is never installed. Original cwd must match exactly.
- V2 `session/set_config_option` exposes one boolean `fixture.auto` option, with
  validation before mutation and a full `config_option_update` before the reply.

Controls: `state`, `turn start|end|settled`, `model_call`, foreground permission
`request`, `pause_before/after`, `release`, `hang`, `drop_reply {operation:send}`,
`retain {action:prune,key}`, `reconnect`, `exit`, `host_exit`, `crash`.
All unsupported controls return an explicit control error, never fake success.
A control completion is a ledger/output barrier, not proof an adapter consumed it.
`reconnect` reports P6 state without invoking or reinserting any original input;
it does **not** simulate replacement of the native process or its pipe.
`retain` calls P6's retained-message/key pruning. No pressure-driven resend exists.
Only `on:input` timelines are supported; `after:tool` timelines are refused.

Ledger rows have `seq,kind,ns,pid`. Commands retain raw method, params, request id
and semantic `operation`; separate `input_received`, `inserted`, `model_call`
(with ordered model-visible typed messages), `request`, `answer`, `native_reply`,
`turn`, `replay`, `retention`, `reconnect` and `exit` rows are the oracle. The file
is append-only, in the same framing consumed by K1 `Native.records(after=...,kind,
operation)`; completed LF records alone are evidence. No inherited environment
values or bootstrap tokens are logged. `ready` records only booleans for hygiene.

## Bounds and gaps

- Shared engine: 1 MiB native/control input record, 16 MiB ledger, 64 concurrent
  native commands, 1024 timeline actions. Exhaustion is a visible fixture failure.
- Wrapper: 32 sessions, 256 prompt operations over its lifetime, 64 pending model
  admissions per session, 32 KiB encoded content per prompt, 256 KiB lifetime
  content charge per session (duplicates conservatively charged), 128-character
  keys/request ids, 64 lifetime requests per session. These are fixture bounds,
  **not adapter capability declarations**.
- `state` includes pid, active, session, sessions and a bounded message page:
  `after` is an offset, `limit` is 1..64 (default 16), at most 48 KiB of messages;
  `next` is the next offset or null. This remains below K1's control-reader bound.
- No full JSON Schema validation, media, native tools/children/background work,
  rich requests/authentication, display terminals, host services, session list,
  close/delete/fork, arbitrary replay cursors, or live mutations during replay.
  Replay is restricted to idle sessions so an in-flight admission cannot become
  part of an authoritative cut. Unsupported native methods/fields are errors.
- A native-process restart, credited provider streams, an adapter, and public-core
  certification are **not exercised**. This process is not a second implementation.

## Exact P6 provenance

`acp_conformance_model.py` starts with the **unmodified** bytes of
`tests/acp_matrix_model.py` at P6 `a726ede`:

```
sha256 661df1e8e754e2608719a4eda80ea2cb6761390abf8820beb38013968ad1d5a9
```

After the `ACP_CONFORMANCE_VOCABULARY` marker it contains generated field names
and SessionUpdate tags from the pinned v1/v2 schemas. This is the exact vocabulary
consumed by P6 Codec, not a replacement schema validator. Source schema hashes
are stored in `SCHEMA_SHA256`; schema commit:
`d8cd473e5a04e21448c01972c82f22d91e10cd94`, snapshot 2026-11-19. Immutable sources:
`https://github.com/agentclientprotocol/agent-client-protocol/blob/d8cd473e5a04e21448c01972c82f22d91e10cd94/schema/v1/schema.json`
and the corresponding `schema/v2/schema.json` (Apache-2.0; the full source snapshot
and license remain in P6 `docs/research/acp-matrix/sources/`).

P6's strict gate **still fails**, required gap exactly `semantic:mcp-stdio-v1`.
There is no MCP implementation, opaque tunnel, v1-equivalence or adapter claim.

## Self-check

```
env -i HOME="$HOME" PATH=/usr/bin:/bin PYTHONDONTWRITEBYTECODE=1 \
  taskset -c 16-23 nice -n10 python3 -W error tests/fixtures/acp_conformance_selftest.py
```

Uses only owned temporary roots under `<private-notes>`, a
private HOME and native child pipes/socket; no HERDR variables, real core/backend,
agent, model, credentials or spending. Each test stops its own child and verifies
the socket is removed. The checks cover exact vendor hash, handshake/session,
ordered native/model envelope, admission versus insertion/cancel barrier,
dropped reply with one native effect/no replay, v1 completion/v2 insertion,
valid/invalid/cancelled permission, boolean config validation and retention/replay/
reconnect fencing. This is fixture self-check evidence only.
