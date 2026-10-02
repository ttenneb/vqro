# Tasks decision default policy

Status: **accepted direction, implementation under validation** (owner approval
relayed2026-10-02). Supersedes the human-only answer rule only for the explicit
structured, reversible timed-default path. Manual answers remain human-only.

The Tasks extension may record the filer's declared default without a model or a
per-decision human arm. The human chose observability over gating. Installation
and discovery still grant no authority: this runs only in the explicitly admitted
trusted Tasks provider. It grants no terminal execution, messaging or activation.

Cheap guards:
- Irreversible decisions never auto-apply. Every filed/rescheduled timer must give
  the configured human window: default15min, configured in the provider, not host
  state or caller payload. Original filer binding/human may reschedule; replacement
  executions inherit nothing. The recorded terms/revision cannot silently change.
- Policy is a distinct internal actor, never a synthetic human socket caller.
  Filer, default, deadline, actual apply time and lateness remain observable in
  immutable history/receipts. Digest counts auto-applied versus later-reverted
  decisions. Stricter human arming remains a later option, not current policy.
- Revert is human-only and policy-answer-only; it clears the timer. Reopening
  requires an active parent **if present** and no reported/final dependents, else rejects with
  blockers. Existing in-progress work is not undone; completion waits for re-answer.
  Human answers are final; changing one means a new decision.

Apply/revert changes decision **records only**, not arbitrary external effects.
The provider starts lazily. Its single-thread deadline loop runs while admitted
and alive; overdue eligible defaults catch up at the next admitted invocation
following downtime. It does not promise on-time execution while absent. Reads do
not impersonate human writes, start model turns or acknowledge the human's digest.

Persistence remains in the existing Tasks SQLite database. Historical task IDs,
caller-scoped operation outcomes and graph obligations remain protected. Same-user
human classification is cooperative/unverified, not human-presence authentication.
Availability and exact validated behavior belong in the API status documentation;
this accepted direction does not itself claim a passed product audit.
