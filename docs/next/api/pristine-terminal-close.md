# Conditional pristine-terminal close

Status: available when the server advertises `pane.close_if_pristine_terminal`.
This is a new optional native JSON API method, not an extension of `pane.close`.

```json
{"id":"cleanup","method":"pane.close_if_pristine_terminal","params":{"pane_id":"w1:p2","expected_terminal_id":"term_..."}}
```

Both parameters are required. Capture the terminal id from the original pane
creation response, never from a later lookup. The host compares the current
public pane identity and exact attachment and closes only a positively fresh,
unused shell runtime. It never follows a moved terminal or a historical pane
alias. Success uses the existing `ok` result; an ineligible or unknown target
returns `pristine_close_rejected`. Existing close confirmation and restored-group
checks still apply.

Eligibility is transient host-owned lifecycle state. All generic runtime
constructors, restore/import, handoff and respawn paths are ineligible. Only a
positively fresh API-split shell runtime can be armed, once, before publication.
Every user-input admission attempt invalidates eligibility before queueing,
including delayed, failed and uncertain attempts. Moves invalidate it too. PTY
protocol replies, resize and rendering do not constitute user-input admission.
Known foreground shell and no active/pending launch or resume are additional
required guards, not substitutes for pristine-input admission ownership.

The pristine check and cleanup claim share the existing PTY user-admission
mutex. Competing user input either invalidates eligibility first or is rejected
after cleanup claims admission. The mutex is released before ordinary close and
shutdown, but the owned claim stays alive until close returns. Failure reopens
input only if that unchanged claim still owns admission.
A stale cleanup or handoff rollback cannot reopen a newer owner or shutdown.
Pristine means no host-admitted user input, not absence of autonomous shell
startup activity; unknown foreground state is retained.

The CLI uses this method only for its own created pane after a definite
pre-execution rejection. Explicit existing panes and execution/readiness/input/
transport uncertainty retain recovery work. Unsupported method, missing creation
identity, busy/unknown state or failed admission retains the pane with an explicit
notice; there is no unconditional-close fallback and uncertain effects are not
retried. Safe retention is not a claim that cleanup succeeded.

The method uses the current native JSON transport and optional method discovery.
It does not change persistence, published private wire enums, `pane.close`, or the
frozen generation-1 endpoint bridge/method-shape fixtures.
