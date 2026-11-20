# Controlled Codex app-server fixture (no model)

Legacy no-`--control` mode preserves the Rust native-wire tests. K1 mode uses the
same generic socket, bounded JSONL ledger, barriers and canonical timeline engine
as `tests/agent_conformance/runtime.py`; it does not add a production test verb.

```text
python3 tests/fixtures/fake_codex.py --control <private socket> --ledger <private JSONL> --script <timeline JSON> --harness-root <K1 checkout>/tests
```

`--harness-root` is fixture-only dependency wiring while K1 and A2 are separate;
it defaults to this checkout's `tests` once K1 merges. Paths must be absolute and
the control path must not exist. Use a private root and an empty inherited environment.

- `state` works before a thread exists. Optional `session` is a native thread id;
  the default is the first created thread, not the most recently used thread.
- Controlled input never auto-finishes. `turn`, `tool_call`, `model_call`,
  `request`, `retain`, pause/release, dropped replies and exit/crash are explicit.
  Neutral `turn:end` is an inner model-turn boundary, not Codex outer foreground
  settlement: it emits no invented native lifecycle event. `turn:settled` emits the
  real `turn/completed`; `failure` emits native failed completion, not host end.
- `drop_reply {operation:"send"}` drops the native ACK, not accepted input.
  Acceptance and insertion are independently gated. Steer guards are checked
  atomically after a paused acceptance is released. Interrupted uninserted input
  is cancelled, not moved to a new turn or replayed.
- The oracle records attempted native writes independently of adapter outcomes;
  accepted input retains actual ordered native blocks and `text_elements`.
  No attribution frame is stripped or constructed from standard caller arguments.
- `model_call` records the fixture's user-message role/content envelope from its
  inserted native inputs. Timeline matching checks each actual text block without
  concatenation, including separate sender frames; each matching rule fires once.
- `request` supports pinned native permission and tool-user-input forms. Background
  forms use `is_blocking:false` (or `scope:"session"`); foreground completion does
  not answer them. Native response writes have no invented acknowledgement.
  Answer rows retain the raw result/error and normalize known cancellation errors
  (-32800/-32601), permission decisions and form answers for the independent oracle;
  cancellation remains distinct from decline/empty values. No native input-hook
  consumption or actual tool-to-CLI launch route is claimed by this fixture.
- `session_switch` creates a new native thread and returns `requires_resume:true`.
  App-server threads do not spontaneously become another thread: use public
  `agent.session` new/resume to change a logical endpoint. Queue modes are not
  native capabilities of this adapter and are not emulated by the fixture.

Self-check (private paths; no network/runtime/authentication):

```text
python3 tests/fixtures/codex_control_selftest.py --harness-root <K1 checkout>/tests --root <private scratch>
```

The self-check verifies pre-thread readiness and environment booleans, two thread
selectors, ordered input, ACK-before-insertion, actual model-input content, native
permission/form responses, background lifetime, stale atomic guard rejection,
dropped ACKs, bounded retention controls and the canonical tool/model timeline.
It is fixture evidence, not a replacement for K1 through the public core.
