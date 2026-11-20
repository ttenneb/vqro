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
  The literal route disables extension/skill/template discovery and rejects explicit interceptors/templates.
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
a scoped acknowledged read before provider teardown. Additional request/config bounds will be recorded with their
implementation and tests, not promised here.
