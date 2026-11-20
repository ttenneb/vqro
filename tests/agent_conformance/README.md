# Agent-standard executable conformance (K1, in development)

Part A drives a production adapter through public `resource.*` / provider wire in
an isolated vqro + herdr-core, never adapter internals. The existing
`pi_provider_e2e.E2E` private-root/bootstrap/lifetime setup is reused. Unit tests
also drive bounded JSON-RPC peers without starting a real backend. Part B will
bind the same result/fixture format to the coordination extension when available.

A fixture is **not** an implementation. A passing fake/ACP model never counts as
a second runtime. P6's ACP model is fixture input only. No smoke/model calls are
part of this harness. A target must start our controlled fake native runtime and
prove the fixture control channel/ledger is ready before any input is sent.

## Adapter launch seam (A1 / A2)

- Pi: `vqro-bridge-pi -- <literal native argv...>`.
- Codex: `vqro-bridge-codex --slots 3 --instance <test-id> -- <literal native argv...>`.
- The runner uses `resource.spawn {bridge:true,argv:[...]}`. The real private core
  supplies opaque `VQRO_SOCKET` and the test root's `VQRO_BRIDGE_TOKEN`. The adapter
  registers normally and owns native child pipes. No special test provider RPCs.
- Native argv: `/usr/bin/python3 <fixture runtime.py> --wire pi|codex|acp
  --control <private socket> --ledger <private JSONL> [--script <JSON>]`.
  These are **fixture-program** arguments, not new adapter flags. No production
  environment-filter exception is required. Environment aliases
  `AGENT_CONFORMANCE_CONTROL`/`LEDGER` and `PI_FIXTURE_CONTROL`/`LEDGER` are optional.
- Native protocol remains native (Pi JSONL, Codex/ACP JSON-RPC). The adapter never
  sees fixture-control commands on its provider effect surface.
- Standard input is always `content:[{type:"text",text:"go"}]`, never a string or
  single block. Each call pins current `epoch`, `caps_rev` and its exact endpoint.
- Multi-endpoint fixtures use one app-server host, sessionless slots, then
  `agent.session {v:0,action:"new"}` independently on three endpoint resources.

## Private runtime control / oracle

One UTF-8 JSON request/response per LF on the separate private control socket:

```
{"id":1,"verb":"state"}
{"id":2,"verb":"turn","state":"start"}
{"id":3,"verb":"tool_call","id_ref":"tool-1","state":"start"}
{"id":4,"verb":"tool_call","id_ref":"tool-1","state":"end"}
{"id":5,"verb":"model_call","id_ref":"model-2"}
{"id":6,"verb":"turn","state":"settled","aborted":false}
{"id":7,"verb":"request","request":"native-q1","method":"confirm","title":"Continue?"}
{"id":8,"verb":"pause_before","phase":"preflight"}
{"id":9,"verb":"release","phase":"preflight"}
{"id":10,"verb":"drop_reply","command":"prompt"}
{"id":11,"verb":"exit","code":0}
```

Responses are `{id,result:{...}}` or `{id,error:{message}}`. A completed control
verb is an ordering barrier: its native output has been written and ledger entry
appended, not proof that the adapter already processed it. Tests wait on public
facts/streams after the barrier; sleeps never establish an insertion point.
`pause_before` / `pause_after` use semantic phases `preflight`, `acceptance`,
`insertion`, `tool`, `model`; `release` opens that barrier. `hang` is a held reply,
not a timer loop. `queue`, `retain`, `session_switch`, `crash`, `host_exit` extend
this same verb set; unsupported fixture verbs are a **harness gap**, not a declared
adapter omission. The full scripted `on/match/do` timeline is recorded separately.

Ledger records are owned by the fake runtime, not reconstructed from provider
replies. Each record has `seq,kind,ns,pid` and typed details: `command`,
`input_received`, `inserted`, `model_call`, `request`, `answer`, `native_reply`,
`turn`, `tool_call`, `exit`. Read-only metadata commands are distinguished from
native input/control effects. Raw native arguments are retained for assertion;
bootstrap secrets and unrelated inherited environment values are not logged.

## Run the current slice

Copy `pi.example.json` into your scratch directory and replace only the explicit
adapter executable. Whole argv tokens `{python}`, `{fixture}`, `{control}`,
`{ledger}`, `{script}`, `{root}`, `{work}`, `{codex_fixture}` and `{harness_root}`
are substituted; other arguments stay
literal (no shell expansion).

```
python3 tests/run_agent_conformance.py --run --implementation /absolute/target.json \
  --vqro /absolute/vqro --hc /absolute/herdr-core --cases C1,C2,D6,D13
```

The runner re-execs in an empty-HOME user/network/PID/mount namespace, launches a fresh
private core/backend per case, and writes `results.json`, `results.md`, per-case
`result.json`, native ledger and PID/birth/cleanup receipts. Hard parent timeout
kills the owned namespace, including detached children. Partial selections always
name all omitted cases. Leaving `--cases` out includes all 61 Part A groups;
currently unimplemented applicable handlers fail as `component:harness`.

The normative IDs are parsed independently from the captured immutable `main`
conformance document, saved as `normative-conformance.md` with commit/hash in the
report. Catalog drift refuses fixture execution. Unverified source exports may run
diagnostics but never assert current-main completeness. C11/C12/D16/O12/O13/O14
are visible harness gaps until their runners exist.

Handlers exist for C1–C10, D1–D15, O1–O11, R1–R7, M1–M4 and L1–L4.
Independent review identified insufficient acceptance predicates in several of
these handlers; their older diagnostic pass counts are not conformance proof.
Some optional declared branches still have explicit harness gaps (including native
preempt/keys, richer requests/configuration and a many-endpoint host rebind).
Core causality forwarding and the old producer unfinished-cut overflow remain
separate implementation failures until fixed target images are exercised.
Children/display-terminal applicability uses canonical module descriptors, not an
invented `agent.terminal` operation. This is an incremental harness release, **not complete Part A or Part B**.

Provider descriptors are supersets: enum values are sets, numeric limits maxima,
boolean true permits resource true. Resource descriptors choose current values.
An empty provider op object is not unrestricted. `agent.modules` is an object
map of canonical optional module names to descriptors containing `v`, never a
list. The Pi fixture accepts real native `--mode rpc` launch arguments. Content declares
`{types:[...],shape:ordered|text_then_media}`. C10 records the many-endpoint branch
as not-applicable for an explicitly one-endpoint host, without inventing an
unsupported feature or an extra top-level result status.

## Inherited shared-component checks

O3/O11 may inherit the shared Rust producer/core checks instead of relaunching
200 fake endpoints for every adapter. A target supplies `component_evidence`
entries keyed by case: executed `adapter_image_sha256`, reviewed `producer_source`,
`component`, a `receipt` and `unit_log` (`{path,sha256}`), and `required_tests`.
Every artifact is checksummed and bounded to4MiB; a passing full receipt is
required. O3 names snapshot-hold, unfinished-cut-overflow and reset-recovery
regressions. Known counterexamples override an older green receipt. O11 also
binds `core_image_sha256`, validates the200-stream scale `summary`, and requires
reserved-budget/open, writing-stream closure and idle-sibling survival proofs.
The rule is per stream within one shared connection budget, not forced closure
of idle sibling streams. Missing linkage/proofs are `fail/harness_gap`; these
checks are explicitly inherited component evidence, never another runtime.

## Results / honesty

Each case/subcase produces `pass`, `fail`, `unsupported-declared` or
`not-applicable`, alongside capabilities/versions, native/composed provenance,
limits and evidence refs. Only absence of an **optional declared capability**
permits unsupported-declared. Declaration-selected exclusions use not-applicable
and include their explicit excluding declaration; they never count as a pass.
Unimplemented applicable branches remain fail with reason `harness_gap`.
A missing runner implementation, unavailable fixture control, start failure,
mandatory-core gap or unsupported protocol plumbing is `fail` with component
`harness`, `fixture`, `core`, or `adapter`; none becomes a fake capability skip.
A partial case selection is visibly partial and cannot certify Part A/Part B.

Item records use only `op:put|append|remove|notice|snapshot`, snapshot
`phase:begin|end`. The old P3 `reset`/`snapshot_end`/string-value shapes are not
accepted. The independent reducer stages snapshots until end, validates byte
framing/sequence/revisions/append bases, and keeps count/byte-bounded diagnostics.

Every subprocess gets a private HOME/config/state/runtime/socket root and no
HERDR_* variables. Native fixtures never open a provider/model connection. Backend
execution is owned by the private backend, not a new vqro process host. Cleanup
kills/removes only resources created under the harness root and records leftovers.
