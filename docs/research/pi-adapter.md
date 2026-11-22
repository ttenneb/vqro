# Rust Pi adapter declaration notes (implementation in progress)

This is not an adapter certification or install report. `agent-v0.md`/the program rulings are normative.
Fake/public-wire evidence and the final installed-Pi1 regression/smoke remain separately identified.

## Evidence boundaries
- Native `success:true` is `admitted`, not inserted/handled/completed. No delivery/run association is inferred
  from text, order, a queue shrinking or completion. Native input is never replayed.
- Starts and admit's possible starts share one preflight gate. Unknown/unresolved preflight is busy even if native
  state looks idle. Interrupt does not release it. Controls bypass that gate.
- Sender framing is a user-role attribution, never system/developer authority. Plain user requires a proven human
  owner identity; otherwise declared attributed_user is used or the requested rendering is rejected.
- Input is array-only text_then_media: at most one leading text block, then images. No concat/reorder/fallback.
  Pi loads its skills, extensions and prompt templates (human choice, customization slice 1); explicit -e /
  --extension / --prompt-template / -p argv is still refused. A send whose text Pi would expand or run (a leading
  /skill:, or /<name> of an advertised template or extension command; any leading /name while that inventory is
  unknown) is refused as non-literal: run it with agent.command. With extensions loaded, agent.send.literal is
  "none", input_transform is "may" (Pi names only command-bearing extensions) and every receipt carries
  may_transform "unknown"; a launch that itself passes --no-extensions and --no-prompt-templates stays
  "controlled_launch" with input_transform "none".
- Snapshots are tail coverage, not complete native history. Shared ItemJournal owns the sole retained entity
  document; native events become puts/appends, with explicit oversized-content omissions.

## Native dialogs (coordinator rulings, 2026-11-19)
Installed Pi1 `dist/modes/rpc/rpc-mode.js:40-88,175-196,632-645` is the source evidence:
- Dialog frames have a UUID but no session/run/tool/parent-operation binding. Each actual native dialog flow uses
  `scope:{operation:<opaque dialog operation>}`. Declare `background_requests:false`; never infer a session/run
  association from timing.
- Choice/confirm/text are native UI methods; a confirm title is not evidence of permission semantics. Choices
  retain stable IDs/full valid labels. Cancel, false and empty text are distinct wire answers.
- A positive native timeout has only a relative duration, starts before output and expires silently. Such a dialog
  is cancelled once and reported unrepresentable: no invented expiry and no approval of possibly expired UI.
  Untimed dialogs remain answerable. Unknown/unrepresentable methods also cancel once, never auto-approve.
- UI answers have no ACK and remain `dispatched`. A later get_state read on the same pipe can fence the native
  synchronous response-map update; that is never reported as handled versus already cancelled.
- notify/setStatus/setWidget/setTitle/set_editor_text are fire-and-forget UI, despite having IDs, not questions.
  `notify` (human-facing `message`, optional `notifyType` info/warning/error) becomes a standard `notice` entity
  `{position, text, severity?, native}` that viewers show; severity only from a known `notifyType` (`warning` → `warn`).
  The others, and a notify without text, stay private `_ui` entities.
- First valid answer claims the live request revision atomically. No-write failure can release a claim; an
  uncertain write cannot. Reconnect/epoch changes expire unverifiable pending dialogs and cancel once.
- Facts summaries carry params up to2KiB; larger params mark the summary incomplete/Cancel-only, while the full
  request stays in items. Confirm messages and editor prefill/multiline must be displayed before approval.
  K3's corresponding renderer fix is63da208 (not yet merged at this checkpoint).

## Native queue limits
Pi1 `agent-session.js:709-715,2046-2053` emits/clears text arrays. Previews are native observations with null IDs;
list/clear only, never withdraw/replace or a delivery-id reconstruction. Another input invalidates old membership
until a new native queue observation/count cut. An empty queue never proves insertion.

Native clear does not return image attachments. The declaration says removed content is text-only and media is
unknown; results preserve returned text exactly and mark `removed.media` incomplete. More than64 returned entries
or a result beyond48KiB is explicitly unknown/over-bound, not a fabricated empty clear. Late native replies are
kept as bounded `_control` observations keyed by their returned operation ID, without reissuing clear.

## Bounds currently implemented
Native JSONL/write budget1MiB,32 pending native calls,64KiB stderr tail. Input768KiB/32 blocks. Item journal128KiB,
512 entities/512KiB aggregate,64KiB wire records,1MiB extra snapshot buffer; four viewers. Receipts retain256
completed plus active native calls; no claimed persistent idempotency key. End evidence is fenced through gone and
a scoped acknowledged read before provider teardown. Requests: eight pending dialogs,16KiB each; at most16
unconfirmed automatic cancellations and24 UI control/fence tasks. Cancellation-control overflow fails closed by
stopping the owned child, not by approving or silently dropping a question.

## Native configuration
Ports the existing Python control declarations: up to64 model choices plus the current native model,64 reported
thinking levels, native queue delivery modes and auto-compaction. Model choice values are collision-free opaque
encodings of the native provider/ID pair (a provider containing `/` is not split). A cut/invalid catalog is marked
incomplete; it never becomes a known-empty inventory. Revisions change with values/descriptors; stale revisions,
wrong types and values outside advertised choices reject before write.

A setter is written once. Its unresolved workflow masks old model/image and queue-batch semantics, blocks sends
and another setter, but not native controls. A native ACK is only accepted; observed requires the later reported
value. Late ACKs continue readback without replay. Facts are published and followed by an acknowledged scoped read
before an observed reply. Config/command catalogs are omitted with an explicit incomplete marker if combined facts
would exceed60KiB. A failed/unknown readback stays visibly unresolved, never falsely ready.

Private native subprocess/SDK tests cover a late ACK past the caller deadline, no duplicate setter, concurrent
setter refusal, fresh facts/fence delivery, and successful ACK with mismatching readback remaining only accepted.
Pure tests cover actual metadata containers/arrays, native model IDs, revisions, allowed values and catalog cuts.

## Explicit commands
Preserves the Python bridge's new_session/cycle_model/compact controls and up to64 native advertised names.
Invocation is only agent.command; sends never enter this dispatcher. Unknown names, typed/non-string/over-bound
arguments and unexpected arguments to no-argument builtins reject before write. Explicit native prompt commands
share the existing start/admit preflight guard; their ACK stays accepted, not handled. Compact and cycle_model use
their native RPCs, read back state and fence facts; only a recognizable native result is observed. Compact tests
use the fake native process, never a paid model call. Late bounded results are canonical _control observations.
Native cycle_model results are projected to safe model identity/label/thinking fields; raw configured model headers
are never published in reply data or retained item records.

## Installed-runtime preflight regression
The opt-in pi_real_start component runs the actual installed runtimes with a public registerProvider/streamSimple
file-only fixture, private HOME/config/work/cache and a separate user/network namespace. On Pi1.5.2, one held
before_agent_start flow admits one native input/model call and rejects a second start before write. Its native
metadata already reports streaming:true there. On0.87.0, metadata is still idle; even an abort ACK cannot unlock the
unresolved first preflight, and both second attempts reject before write. Each final run has one input, one fake
model call,0paid calls and a confirmed owned child exit. Native abort itself waits on the held hook on1.5.2; no
claim of an idle preflight or immediate abort ACK is generalized from0.87. Runtime/source/fixture fingerprints
and exact component scope are retained with receipts; this is not a whole-adapter certificate.

## Sessions in the initial declaration
`agent.session:new` preserves the Python bridge's native new_session control. It neither prompts nor changes the
endpoint/owning host. The current session and one new epoch are read back and fenced into core facts before an
observed reply; prior receipt lookup survives. With no prior known session, the explicit new-session cut is still
advanced exactly once. A session change shares the prompt preflight guard (ordinary controls still bypass it), so
an unresolved prompt cannot accidentally land in the new conversation. While the switch is unverified, current
session facts are null and a session guard cannot be claimed enforceable.

Native CLI resume arguments remain available at launch. Optional resume/fork/list/close/delete actions and native
history paging are not declared here: the native RPC does not expose bounded all-session/history discovery or a
proven paginated cut. No filesystem discovery or guessed session/entry IDs is substituted.

## Temporary transcript and terminal fallback
Until the generic item renderer is available, data.views references one owned agent-rN panel. Declaration uses the
existing instance/ref bridge marker, scoped resource.list/workspace.get and a CAS transaction; a foreign marker or
lost CAS race is never overwritten or published. No full workspace snapshot or duplicate status view.

The panel is derived from canonical message/tool/notice entities, not a second retained transcript document.
Text is a128-row/16KiB tail, under32KiB encoded; omissions are visible and terminal controls are removed. Only
changed transcript content is published, so queue-only updates do not resend it. Every optional view lookup/storage/
publication error is view-local (including io/overloaded/unavailable), never a native lifetime decision. Up to3
retries per provider connection run outside the native reader/control loop; exhaustion leaves a visible warning.
Actual connection loss reconnects normally. Final publication is bounded before fenced native end evidence.
Native retry/compaction notices are system-role entities, not replay.

An independent TTY monitor redraws native status, queue/dialog summaries and the same derived tail even when core
is unavailable. Its raw-write thread has one latest-frame slot and never holds Rust's global stdout lock. A stalled
terminal cannot block the native reader or process shutdown; it is never joined at exit. No colour escapes.

## Effective input origin (C2)
Before admission or any native write, `agent.send` binds `origin.principal` to the core's effective principal
(including exact delegation) and `origin.actor` to its recorded transport actor. Omitted caller identity fields
are filled from that core record; missing core identities mean `undeclared`. A supplied mismatch is
`forbidden`, not a relabelled/replayed input. Core `initiator` is retained
separately as `origin.initiator`; a supplied conflicting initiator also refuses before write. Identity fields
accept C2's256-byte bound. Caller-classified `kind`/`agent` remain unchanged and are explicitly listed in
`origin.claimed`; native framing calls these asserted claims, never authority inferred from names/prefixes.
On dispatched inbox mail, `origin.sender` retains coordination's original `from` with `provenance:"inbox-record"`
and is also explicitly claimed. It is not equated with transport actor/initiator and is not end-to-end core
proof of that original sender; this asynchronous provenance gap remains explicit. Native prose renders
`from <sender> via <actor>` and labels that inbox sender coordination-asserted, not core-proven. Quoted provenance remains
asserted and unchanged. Plain user rendering remains unsupported without verified
owner evidence. Receipts retain the same bound origin; neither ACK nor identity binding proves insertion or
handling. C2's canonical SDK causal metadata is annotated into validation/receipt arguments with the shared
`Causality::annotate` mechanism, so root-only cause/correlation are retained exactly, with no provenance upgrade
or link inferred from text/timing. Private SDK/native tests prove spoofed principal/actor/initiator write nothing,
and one delegated send yields exactly one native prompt with distinct effective sender/transport actor/initiator
plus unchanged root-only causal metadata.

## Tool principal environment (G11)
The owned child inherits only VQRO_SOCKET and VQRO_PRINCIPAL from the VQRO namespace, unchanged; supervision
credentials/configuration and every HERDR variable stay excluded. The launcher assigns the principal, not rN.
After registration a public workspace.get(doc/principals/owner) distinguishes an agent launch from the workspace
owner. Only a provided non-owner principal with known owner evidence declares caps.agent.tool_principal:"env";
unknown/owner launches omit it. The upper bound is agent.tool_principal:["env"], not a scalar top-level descriptor.
No identity is invented or asserted as a reserved core name. Opt-in `tests/pi_real_tool_principal.rs` now proves
the product Native filter -> actual installed Pi1.5.2 RPC -> native bash tool -> actual vqro CLI -> private C2
core path: one explicit tool write records principal/actor `p:a1-native-tool`, not workspace owner or the supplied
`label:"owner"`. Two file-only fake model turns, zero paid calls, private user/network namespace and empty HOME/
agent configuration; native and private core exits verified. This uses an explicit public fake-provider extension
and certifies that component path, not the controlled-launch whole-adapter profile or other runtimes.

## Partial verification —1874677
Native dialog normalizer/state and stop-coverage regressions pass; warning-free bridge build. Public
C1/C2/D6/D13/R1/R3/R5/M1/L1 pass. L2 coverage/end checks passed, but its final effect-event count failed once;
an unchanged-source repeat passes. K1 is diagnosing the observer-count assertion. R2 concurrent/stale first-valid
checks and R4 unknown-method cancel-once execute, then report honest harness gaps for their unfinished branches.
No full adapter certificate, installed-runtime regression, paid smoke, full gate or install is claimed.
