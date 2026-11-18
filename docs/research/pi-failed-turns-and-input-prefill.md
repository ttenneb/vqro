# Pi failed turns and explicit-only input prefill (2026-11-17)

Follow-ups on `soak-ext-fixes`, separately committed as `ad44fc9` (failed turns) and `4723829` (input prefill).
No live bridge, session, credentials, provider/model or install was touched.

## Failed turns

The prior bridge looked only at assistant text blocks, so an empty-content assistant message with
`stopReason: error` disappeared. Native settlement then became `done`, hiding a failed model request.

Pi mapping now stays in `pi_bridge`:

- An assistant error records bounded, control-safe `failure.message`, adds an error-styled transcript row even
  for empty content, and remains `status: error` after `agent_settled`.
- Native `auto_retry_start/end` events supply optional attempt/max/delay/success facts and readable transcript
  rows. Active retries remain working; final failure remains error. Nothing in prim performs these retries or
  sends the prompt again.
- A successful retry or a new turn clears the current failure; historical error/retry transcript rows remain.
  Settlement clears active retry state but cannot manufacture success for an unresolved assistant error.
- The neutral Agents extension displays optional `data.failure`/`data.retry` facts without Pi-specific imports.
  Native provider output retains semantic styles and remains read-only/bounded.

Native contract checked against installed Pi 1.5: `docs/message-types.md:134-136` (`stopReason`, `errorMessage`),
`docs/json.md:166-170` and `dist/core/agent-session.d.ts:82-93` (retry events). No model call was used to infer it.

The fake Pi reproduces the reported ResourceNotFoundException, an empty assistant body, four observed retries
and final settlement. Tests also cover recovery/new-turn clearing, active retry observations, oversized/control
text, and a non-Pi provider's neutral failure facts. Red evidence showed `done != error`, missing styled rows,
and an invisible failure; green tests cover those exact failures.

**Deployment limitation:** an already-running backend-owned bridge runs its original Python code. Installing
new extensions does not mutate its process. The human's bridge started at 18:00 therefore remains old until a
new agent/bridge is started; no running agent was restarted or replayed here.

## Explicit-only input prefill

The generic TUI input path incorrectly inserted the focused pane title for every input. A literal fake-Pi
prompt exposed `Agent r2fixture-model-error`, matching the human's `Agent r8 Hello?` symptom.

Inputs now start empty unless their author explicitly supplies `prefill`: a string, or `{"context":"field"}`.
Unknown/missing sources are empty. Panel rows, chips, menu items, bindings and custom options use the same
mechanism. No command names or agent/resource kinds are recognized by the client. Option `value` is no longer
an implicit text default. Pane/tab rename and Settings' custom-prefix option explicitly request their initial
values; `tab_title` context is available even from a pane inside a tab. Protocol documentation records it.

The TUI regression covers empty/literal/empty-string/context defaults, rename context, panel-row forwarding,
and custom-option defaults. The physical TUI test now types an agent prompt without clearing any prefill and
asserts the fake runtime received **exactly** `ui-typed-prompt`; the model-error fixture is likewise typed
literally without a workaround.

## Verification

Final combined `scripts/check.sh full` at `4723829`: **20/20 PASS**, zero-warning build, cargo tests (including
TUI prefill), **158 Python tests**, fake Pi CLI/TUI, 600-terminal/200-agent sidebar load, provider opt-ins,
scale/TUI budgets and scoped no leftover processes. Environments were empty/private, builds used the existing
per-agent target at nice 10 with eight-core affinity.

- Receipt: `~/.local/share/herdr/scratch/prim-agent-ux-target/check-receipt.json`
- Final logs: `~/.local/share/herdr/scratch/prim-agent-ux-target/check-logs/20261117-194847/`
- Final physical UI receipt/screens: `~/.local/share/herdr/scratch/ep-2ykmqep5/`
  (`ui-failed-turn.txt` shows the error, Failed status and retry 4/4).
- Red/green evidence: `~/.local/share/herdr/scratch/prim-agent-ux/soak/model-error-{red,green}.log`,
  `prefill-{red,green}.log`, and `prefill-ui.log`.

Failed-turn work was estimated at 25 agent minutes and completed in about 17 (11 work, 6 tests). The separately
approved generic prefill fix added about 13 minutes (7 work, 6 tests). All owned test processes were stopped;
small receipts/screens remain for review.
