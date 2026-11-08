# Client-attributed view actions v1

Status: experimental implementation, pending independent principal/action admission review. Foundation acceptance alone does not enable or accept this path. Linux is the native validation target; macOS execution remains unverified and Windows new clicks remain read-only with existing Tasks CLI guidance. Attribution is cooperative same-user classification, not authentication or proof of a physical gesture.

## Pre-publication shape finalization

The v1 pair was finalized before first publication/admission. The unpushed,
unmerged, unadmitted8b92e80b draft omitted placed-consumer context; it is not a
published compatibility contract. Final v1 requires `consumer_pane_id` and
`terminal_scope`. Old draft requests are rejected for missing required fields;
there is no weaker fallback. This finalization changes no frozen codec or
published method fixture.

## Optional public methods

`host.view.action.prepare.v1` and `host.view.action.submit.v1` use the raw JSON API or its advertised native endpoint tunnel. No named binary codec changes. Missing support disables only these actions. Existing tab-only `host.document.action` is unchanged.

Prepare parameters:

```json
{"namespace":"provider.namespace","scope_id":"session","pane_id":"w1:p1","consumer_pane_id":"w1:p2","terminal_scope":["w1:p1"],"generation":1,"revision":2,"item_id":"row-id","option":"listed-key","operation_id":"one-original-operation-id"}
```

`option` is absent for an action, mandatory for a choice. No default is applied implicitly. IDs/generation/revision refer to the exact document accepted by this connection, not a fresh substitute. The item must have an enabled hierarchy rooted at the named existing pane in a current SESSION view. `consumer_pane_id` identifies an actually placed view in this connection's exact
current tab, with the matching namespace/view ID and a committed surface-v2
descriptor. Its live `layout.view_scope` must match that committed descriptor and
the complete `terminal_scope` expectation, including hidden/collapsed members.
The expectation is only a fence, never scope authority. Another global SESSION
root, an unplaced/background/moved consumer or a selected-only scope is rejected.
Prepare/submit/final admission bind and recheck these facts. Consumer visibility
is a separate fence: this exact connection needs an active accepted surface with
a nonempty consumer body, and the consumer must remain selected/visible with
nonempty current effective geometry. A stale committed descriptor does not keep
an unselected or zoomed-away consumer eligible. This consumer-body check never
trims the authoritative terminal scope.

Both document publisher and action service must remain current; the action service is the exact service recorded by the document catalog, with its independent generation and ActionInvoke grant. No task-specific state or ID interpretation occurs in the host.

Caller identity is not a parameter. Native requests have their exact connection. Raw socket requests require kernel PID/birth/account matching exactly one registered connection; neither a payload client ID nor the foreground client supplies authority. Fresh complete ancestry, corroborated real controlling TTY, owned-pane exclusion and live fixed-namespace ownership epochs are mandatory. See [client TTY attribution](../architecture/client-tty-action-attribution.md).

Prepare returns `host_view_action_prepared` with a random token, `requires_confirmation`, optional `confirmation_token`, `expires_in_ms` (60000), and original operation ID. At most one pending intent per connection, 64 globally; proof/result jobs use the existing bounded extension work slots. Tokens bind connection incarnation, caller/TTY, publisher/service generations, document revision, item, opaque subject, listed choice and operation ID. Expiry/replacement is pruned on explicit requests/document events; there is no heartbeat. Teardown drops pending state and cancels queued/pre-dispatch work.

Submit parameters:

```json
{"token":"exact-prepare-token","confirmation_token":"exact-irreversible-choice-token"}
```

The confirmation token is sent only after the client presents and explicitly confirms the exact irreversible question/choice. A Boolean cannot replace this binding. Reversible actions omit it. `{"token":"...","cancel":true}` consumes only this connection's matching pending token without executing anything. A successfully submitted token is single-use. Submission repeats the fresh peer/TTY/ownership proof, checked audit and final local/document/provider/connection fences. The three-second proof deadline starts before worker spawn, travels with its completion and is checked by the actor before token creation and immediately before queueing. Shutdown/stop intent rejects undispatched completions without actor peer/TTY I/O; already-dispatched known replies remain a separate lane. No execution retry is performed.

## Neutral view metadata

The optional JSON-only `view.affordance` is inert presentation data. Unsupported/malformed shapes stay read-only. Supported shapes:

```json
{"kind":"action","label":"Accept work","detail":"Full bounded question","subject":{"package_owned":"opaque"}}
{"kind":"choice","label":"Answer","detail":"Full bounded question","subject":{"package_owned":"opaque"},"options":[{"key":"go","label":"Go"},{"key":"wait","label":"Wait"}],"default":"wait","reversible":false}
```

There are 2–4 distinct listed options, one listed default and a decision-wide reversibility flag, not per-option undo policy. Detail is at most1024 UTF-8 bytes; an individual hint is at most8192 bytes, within the document's existing bound. Labels and details reject control characters. A token never makes top-level document `actions` executable or bypasses provider policy.

## Negotiated provider caller

Only `action.invoke.client.v1` receives the new source. A provider must advertise it and the host must welcome it with ActionInvoke granted. Legacy `action.invoke.v1` and its caller/payload envelope are unchanged; the new source never falls back to it. A missing optional method rejects before dispatch without retiring a compatible legacy runtime.

```json
{"caller":{"class":"human_unverified","uid":1000,"client":{"boot_id":"kernel-namespace","pid":123,"birth":456,"tty":{"kind":"unix","device":"device:inode:rdev"}}},"payload":{"action":"view_action","subject":{"package_owned":"opaque"},"option":"go","operation_id":"one-original-operation-id"}}
```

There is no invented `terminal_id`. The host derives and overwrites the complete caller envelope. The package owns the `view_action` adapter and maps its opaque subject/choice into its ordinary mutation/authorization/receipt path. Tasks maps to accept or structured answer using the displayed task revision and original operation ID; legacy callers/receipts are not rewritten. A stale provider result can publish its current document revision but must not replace and retry the original intent.

## Audit and outcomes

Checked writes/flushes use the existing rotating server-log writer independently of tracing filters. A missing/disabled/failed attempt writer rejects before dispatch and never blocks startup, PTYs or recovery. Host records correlate connection/boot/request, full caller, publisher/document/subject revision, option and original operation ID. Post-audit ownership observation and final local epoch admission fences are mandatory.

`host_view_action_submitted` retains the original operation ID, caller and complete `provider_response`, plus optional `audit_warning`. Dispatch-not-started, provider reply and outcome-unknown are distinct. A known provider reply is preserved even if later audit writing fails or its client-action provider retires after replying. In the negotiated client lane, a matching RPC error is preserved verbatim as `provider_error`, including after retirement, rather than relabeled as transport uncertainty. Legacy generic callers retain their existing error classification and retirement behavior. A known error reply does not itself prove no effect; original operation correlation remains mandatory. The queue pins the exact generation snapshot that was checked and never adopts a later instance via a second load. Unknown outcomes require inspection of the existing operation/receipt; no fresh-ID retry button or blind replay. Cancellation after provider dispatch cannot undo an effect.

New method/result shapes are additive JSON contracts. Frozen named codecs, historical method fixtures, terminal recovery and legacy invocation authority remain unchanged. No install, push or shipping approval is implied.
