# Review: live-view deltas design (report only)

Opus review pane, 2026-11-17 23:41–23:55 EST. Design: `hardening/design-live-deltas.md` (23:40). Code checked:
main `f9a537c` (`src/server.rs:553-583` `view.publish`, `src/events.rs` per-subscriber shaping), soak evidence
`review-soak-3-control.md` P2-2.

## Verdict

The direction is right: generic, opt-in, publishers unchanged, snapshots and old clients full, reusing the
`partial_resources` pattern, with no backend- or panel-specific rules. But as written it attacks **delivered**
bytes only, puts the diff work in the wrong place, and leaves two bigger, cheaper wins on the table. Approve it
with the four changes below. Without them it may not meet the 200-agent target.

## What it gets right

- Opt-in `events.subscribe {view_patches: true}`, so legacy wire output is unchanged. The first event per view is
  full; `missed`, resubscribe and epoch changes all reset to full. `base` is the sequence of the preceding content
  event, so ordering with `entries` and other events is preserved (everything stays in one seq stream).
- Patches apply atomically or not at all, never partially, and an unsupported or mismatched patch triggers resync.
  The Python SDK materializes full content before handlers run, so extensions are untouched.
- Diff costs are bounded (depth, edit count, lookahead), with a full-content fallback when the patch isn't smaller.
  No new dependency and no quadratic LCS. Platform-neutral (plain JSON).

## Problems and required changes

1. **Diff once per publication, not once per subscriber** (P1, scale and memory).
   - **Problem:** the design diffs inside each subscriber's EventFilter against that subscriber's cached copy:
     512 entries / 4 MiB per connection. With the TUI plus about 10 Python extensions subscribed, that is up to
     about 44 MiB of duplicate caches, and every publication is diffed N times.
   - **Change:** compute the patch **once in `view.publish`**, against the previous `live` value the core already
     holds, and attach it to the event with `base` = the previous content event's seq for that view. A subscriber
     whose last delivered seq for that view equals `base` gets the patch; anyone else (filtered, evicted, new) gets
     full content from the `live` map. Per-subscriber state shrinks to `view → last seq` (a few bytes), with no
     content copies. Same wire format.

2. **The shared event log still fills with full content** (P1). `emit("view.content", {content})` stores the full
   object in the log, which is bounded at 10 000 records and 8 MiB. At 200 agents with about 30 KB transcripts,
   8 MiB is only a few hundred publications. That is a second or two of history, so any subscriber falling slightly
   behind gets `missed` and a full snapshot of up to 64 MiB. Shrinking delivery doesn't fix that amplification.
   With change 1, the log stores the patch (plus the view name and `base`), and full content lives only in `live`
   for snapshots and full fallbacks. Log churn then drops by the same factor as delivery.

3. **Remove the agents-extension mirror regardless** (P1, cheap). The agents UI copies the bridge's panel, which
   doubles both publication and diff work and creates a second writer for the same data. The agents view should
   reference the bridge's view (layout or panel embedding by view id), or show its own small summary, not a copy.
   This halves traffic immediately and is independent of patches.

4. **Split status from transcript in the Pi bridge** (P2, cheap, worth doing regardless).
   - Status ticks are frequent and tiny; transcript rows change only on new output. Two views (`agent-rN-status`,
     `agent-rN`) make a status tick a full publication of a few hundred bytes even for clients without patches.
   - It also removes the risk that "N s ago" timestamps or row re-renders make every tick diff into a full
     replacement.
   - Publishers must render transcript rows stably: no relative times, and immutable rows once emitted. The design
     should state that requirement explicitly.

## Smaller points

- **Keyed rows:** with arrays, index lookahead works for tail scrolling but is fragile for inserts in the middle.
  Rows already carry `id` (list_detail requires it). When every element of an array has a unique string `id`,
  match by id (one pass, linear time), and emit `move` only if you allow it; otherwise remove and add. Fall back to
  index lookahead only without ids. It is still generic.
- **Cheaper recovery:** a base mismatch shouldn't cost a whole-workspace resnapshot. Add `view.get {view}` →
  `{seq, content}` (live content and the seq of its last event) so a client resyncs one view. It's small and
  generic, and also useful after filter changes.
- **Round-trip property test:** for random pairs (a, b) within the bounds, `apply(diff(a, b), a) == b`, in Rust,
  Python and across both (the Python applicator on Rust-produced patches). Include JSON-pointer escapes (`~0`,
  `~1`), unicode keys, nulls, type changes and arrays of mixed scalars and objects. Most delta bugs are here.
- **Numbers missing:** state the target, e.g. "200 agents, 1 status tick/s each, 1 transcript row per 2 s: under
  X KiB/s total". Rough expectation with changes 1, 3 and 4: about 200 B/s status + about 200 B/s transcript per
  agent, about 80 KiB/s at 200 agents, against about 1.7 MB/s extrapolated from 120–145 KiB/s at 30 agents. Gate on
  the measured number.
- **Patch semantics:** limit `op` to add/remove/replace (as stated). Reject `copy`/`move`/`test` unless the keyed-row
  move is adopted, and document `-` (append) handling. Cap patch ops and encoded bytes, and fall back to full above
  the cap.
- **`view_content` filters:** a filtered subscriber that skips some publications for a view must get full content
  next time. With change 1 that is automatic (its last seq won't match `base`).

## Options the design doesn't weigh

| Option | Verdict |
|---|---|
| (a)/(e) generic patches, computed in core | **Recommended** with changes 1–2; publishers unchanged. |
| (b) keyed-row ops as protocol messages | Not as a separate API. Use ids inside the generic diff (above). |
| (c) append-only transcript stream (seq, retention, resume, credit like terminal streams) | Right model if transcripts must exceed a window or need resume and scrollback, but a new primitive with its own bounds and recovery. **Defer** (D14 leanness); revisit if agent views need scrollback beyond the window. Don't build it for this traffic problem. |
| (d) split status/transcript | **Do now** (change 4); orthogonal to patches. |
| Reuse terminal stream credit for panels | No: panels are state, not a byte stream; credit/gap semantics don't fit. |

## Estimate

25 minutes of implementation is optimistic. A Rust diff with bounds, Rust and Python applicators, base tracking,
log representation, a recovery path, property tests and the scale gate is realistically **2–3 h of agent work** plus
about 20 min of gate. Changes 3 and 4 (no mirror, split views) are about 30–45 min together and deliver most of the
win alone; consider landing them first and measuring.

## Risk if adopted unchanged

Per-subscriber caches (memory), N× diff CPU on the core's event-delivery path, and log churn producing `missed` and
snapshot storms at 200 agents. The opt-in and fallback design keeps the correctness risk low; the risk is mainly
not meeting the scale target.
