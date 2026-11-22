# Design: opt-in live-view deltas in core (2026-11-17, 23:40 EST)

> Historical proposal and prototype notes, preserved rather than accepted. **Stage 2 was deferred by the
> human on 2026-11-18 after Stage 1 measurements.** The per-subscriber design below is not the architecture
> to implement if this is reopened. See [deferred decision and revised requirements](live-deltas-deferred.md)
> and the [independent critique](live-deltas-design-critique.md). Dates/checkpoints below are historical.

## Outcome and scope

Keep the full readable 128-row transcript and immediate status/error/control feedback. Reduce event traffic
proportionally to changed data, rather than hiding history or slowing status. Native `view.publish` remains an
ordinary full replacement: no provider/runtime-specific rules or replay. Snapshots and legacy subscribers remain
full. Core does the generic shaping; both Python SDK and TUI consume it. No persisted state/history changes.

## Public contract

Add `events.subscribe {view_patches: true}` alongside existing `partial_resources`, `types` and `view_content`.
Absent/false preserves today's wire exactly. Name/prefix filters keep their existing meaning.

An opted-in subscriber receives its first `view.content` per view in full. Subsequent changes may be:

```
{epoch, seq, type:"view.content", data:{view:"agent-r7", base:123,
 patch:[{op:"replace",path:"/items/0/badges/0/text",value:"working"}]}}
```

`base` is the sequence of the preceding content event **for this view on this subscription**. `patch` is a
bounded RFC6902 subset: add/remove/replace only, standard JSON pointers and array indices. Apply atomically to
the previous full content; missing/mismatched base, malformed edits or unsupported edits require existing
snapshot/resubscribe recovery, never guess or partially apply. A full event (including content:null) supersedes
any base. Null clears the base/cache. `missed`, resubscribe and epoch changes clear all shaping bases; the next
publication is full. Snapshot live content itself is unchanged. A fenced snapshot at global sequence S is an
implicit base for a first subsequent patch whose base<=S: the snapshot already includes every content change
through S. Track S separately from the moving event cursor. Once a post-snapshot event for that view applies,
require its exact sequence. This handles fresh()/local-overflow snapshots without resubscription (today a second
events.subscribe on one connection is rejected), and avoids reapplying buffered edits already in the snapshot.

The SDK materializes full `data.content` before extension handlers run, so existing extensions and their
readonly sanitation remain unchanged. A raw Prim client must explicitly request this feature. The bundled
Extension and TUI opt in. Older cores may ignore the option and continue sending full events; both consumers
still accept full events at any time.

## Core mechanism and bounds

Extend existing per-subscriber EventFilter (the partial-resource mechanism), outside the shared core lock.
Maintain last-sent {encoded_content, seq, charged_size} per view, bounded to512 entries /8MiB charged bytes, with a
minimum per-entry charge for metadata. Bases are boxed encoded bytes, not cloned JSON graphs: the retained
buffer cap is real, without BTreeMap/Value object overhead multiplying it. Decode only the changed base for diff;
that bounded transient CPU/allocation is a trade-off. FIFO eviction is sufficient: an evicted view simply receives a full
publication next time. Filtered null/lifecycle events still evict. Oversized individual bases need not be cached.
This adds a bounded opt-in per-connection cache, not another global lifecycle system.

Compare objects recursively; emit only changed keys. For arrays, bounded lookahead detects small insertions/
removals, including scrolling transcript tails; recurse into changed aligned rows. Cap traversal, depth, edit
count and lookahead. On complexity exhaustion, use the original full event. Also use full when the encoded
patch is not materially smaller than the full event. No quadratic unrestricted LCS/diff, new dependency or
backend/panel-specific semantics. Identical content may produce an empty patch (or stay full if cheaper); clients
still observe correct sequence. Ownership, live-size budget and publication validation remain unchanged.

## Consumers and recovery

- Rust: shared pure atomic JSON patch applicator; TUI per-view base map and fixed snapshot-sequence threshold, cleared/reset with snapshot/live reset.
  A failed base/apply follows current missed-event resnapshot path; never renders partially patched content.
- Python: stdlib equivalent, per-view sequence map in Prim/Extension, snapshot clears exact bases and establishes its sequence threshold. Existing overflow
  recovery snapshots and resubscribes; callbacks continue to receive complete content.
- Both implementations test object deletion, pointer escaping, array insert/delete/append/shift, scalar/root
  replacement, malformed edits and base mismatch. No change to extension command/effect handling.

## Evidence and gates

Capture legacy versus opted-in observers side by side on the same600-terminal/200-agent/350-workspace workload,
with long stable transcript tails, status ticks, tail shifts and an error/retry change. Assert identical decoded
contents after a public observer fence, no crashes, strict idle-zero frames, substantial byte reduction and an
explicit bytes/min ceiling. Include reconnect/missed/full-fallback and bounded cache tests. Add to the existing
scale gate. Fake Pi memory gate remains separate and already red/green: old traced growth4,009,399 bytes, new334,133 bytes
across3328prompt/abort cycles, with512journal rows and2048recent effect IDs, preserving128transcript rows.

## Options and trade-offs

- **Append-only transcript stream:** best native token throughput, but needs stream lifetime, transcript truncation,
  reconnect snapshot/cursor/gap rules and a second renderer. Status/queue/dialog facts still need a generic view.
  A good future primitive if two runtimes need native streaming; larger change than solving duplicated stable data.
- **Keyed rows:** strong predictable row-level deltas and selection identity. Requires all producers to maintain
  stable row IDs and a new row-update/reorder contract; does not help arbitrary non-row live data. Useful later,
  not necessary to optimize existing panels without changing every producer.
- **Server-side generic diff (prototype here):** preserves producers and renderer model, works for any JSON live
  data, legacy/full fallback. Costs bounded per-subscriber bases/diff CPU; simple bounded lookahead is not an
  optimal diff, so large reorder/complex cases deliberately stay full. Measure CPU and bytes together.
- **Split status/transcript views:** separates frequently changing status from stable transcript, but a generic
  compositional panel/alias does not exist today. A visible split changes UX/focus; embedding needs another
  generic client contract. Still retransmits transcript windows on token/row shifts unless streaming/deltas exist.

### Should agents mirror the native panel?

Today mirroring is intentional: native output is optional and sanitized to read-only data; neutral agents adds
capability-gated controls, host-terminal access, unavailable/ended warnings and generic error/retry facts. Directly
opening the provider view would avoid the second publication, but lose that ownership/sanitation boundary or
require duplicating generic controls in every runtime. Do not quietly change those semantics in a traffic fix.
A generic composed/aliased view could avoid the mirror later, preserving extension-owned prefix/control rows and
read-only source rows; that needs design across renderer/layout/recovery, not a Pi-specific bypass. This prototype
reduces both native and mirrored status frames to small changes; test/report their bytes separately. Native
provider-to-core publications remain full (one copy per producer rather than one per subscriber), explicitly not
claimed fixed. The scale gate measures end-to-end subscriber events, including both names.

## Prototype checkpoint (00:05 EST)

Rust library/public-wire tests and Python SDK/atomic patch tests pass. New core and legacy observer remain
byte-for-byte semantic peers. Private600/200/350 red/green initially measured20.1x fewer subscriber bytes
(12,788,780 legacy versus635,632 delta bytes), with128 transcript rows retained, explicit error/retry banner and
scroll-tail shift, all materialized panels equal and zero idle publications. Record native and projection bytes
separately. The generator targets0.5updates/s/agent but synchronous calls were slower; report actual rate and
normalize the ceiling by update count to the requested rate, not slow-machine elapsed time. Current prototype
ceiling is12MiB/min at200agents×0.5updates/s; ratio must also exceed8x. Latest repeat includes server CPU time.

Full prototype gate completed26/26 at00:15EST on a553bdc (199Python tests, Rust/physicalTUI/scale/no leftovers).
Closeout report is docs/research/agent-panel-traffic.md atdd9e807; P3 remains separately ready in79a3134.
The critique's once/publication diff and compact log are not implemented; mirror/composition/status split still
need the human's choice. Do not substitute newer live content under an older replay sequence when compacting.
Final handoff text is <private-notes>; forced-mailbox
final send was definitively rejected agent_blocked at00:22EST, so no delivery is claimed. No protocol acceptance,
merge or install is implied. Design
review/human choice is still pending. Progress sends at23:28 and23:53 were rejected because coordinator pane
was interactive-blocked; this file is the retained checkpoint.

## Timing / delivery

P3 memory commit79a3134 already passes the full26/26 gate at23:39EST. Protocol approval arrived during that gate.
Estimate:25agent minutes implementation,10targeted scale/unit verification,10full gate waiting. Final forecast
00:25EST; next substantive checkpoint23:55EST. Main risk is resnapshot/base handling; do not weaken fences or
quietly drop recovery tests to meet the time. No live install/session effects.
