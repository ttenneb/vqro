# Atomic client scene v1

Status: accepted direction (owner redirect, 2026-11-14); implementation in progress.
Atomic publication ships independently of exact-target input admission.

## Comparison and boundary

The selected design is one immutable negotiated `shell.scene.v1` transaction,
using the existing render packet, framing, geometry computation and client event
loop. It contains the snapshot, routing projections, terminal/view descriptors,
collapse geometry and a complete surface. The client validates before installing
all of it in **one event**, without yielding to stdin. Cell patches continue to
name an already installed boot, projection and surface base; they cannot create
routing authority. No second layout tree or provider calls are introduced.

The following lane (not this release) puts connection incarnation, sequence,
exact runtime/attachment, host route epoch and semantic events on **every**
terminal input batch. Mouse also binds geometry. One admission transaction
reports queued, rejected or partial-unknown; admitted press identities own weak
exact-runtime release references. A rejection frontier closes a generation and
its suffix; fresh authority rearms only new input. No replay or deferral.

Compared with the **complete** parked deferred-only design, this removes the
ambient CompletedExact latch required by unmarked legacy successors, BEGIN /
result / RESUME / finite tail / END / result, subsequent cancellation and writer
purge/frontier proof, lease settlement, ABANDON/retirement and recovery-989 fresh
RESUME/fresh-ready. Payload bounds did not bound that lifetime protocol. Exact
runtime identity, non-owning release references, actual-prefix accounting and
transactional enqueue remain necessary in the later lane; atomic publication
alone supplies none of those guarantees. The parked implementation is not merged.

## State diagram

```text
NoScene -- valid full transaction --> Installed(scene, route, surface)
Installed -- harmless update prepared/queued --> Installed(old scene, same route)
Installed -- known route change --> Revoked(old raster retained, presses refused)
Installed/Revoked -- valid full transaction --> Installed(new scene, route, surface)
Installed -- matching installed-base patch --> Installed(same scene/route, next surface)
Any -- wrong boot/generation/base or malformed transaction --> no partial install
Any -- disconnect/deactivation --> no active routing authority
```

Revocation is an explicit host fact, not broad snapshot inequality. Receiving a
newer harmless snapshot is not permission to invalidate an otherwise valid old
scene. A revocation arriving before its replacement stays closed; an old queued
scene cannot reopen a newer revoked route. Unknown transport effects never replay.
This preserves the existing client-refusal notice and host admission guards.

## Closed route dependencies (first shipment)

These are the complete host publication dependencies; scene/raster revision is
separate. A changed dependency invalidates the old route before replacement:

1. Endpoint boot and live connection generation, activation/deactivation, handoff
   availability and selected workspace/tab/pane focus.
2. Exact terminal attachment and live runtime identity for eligible targets,
   including popup identity/ownership; creation, removal, respawn and relocation.
3. Canonical selected layout membership, selected stack members, collapsed versus
   expanded eligibility, zoom and view/terminal content identity and scope.
4. Collection selected/member/expanded-preview ownership and structural membership.
   Eligible target is not necessarily the focused leaf: an entered preview and
   the unfocused terminal under an inspector remain valid existing routes.
5. Surface dimensions, terminal/preview/popup rectangles, split/collapse hit
   geometry and local viewport/resize generation. **Geometry conservatively
   revokes keyboard authority too in this first shipment**; mouse binds geometry.
6. Target input interpretation modes (mouse reporting/pixel mode), popup blocking,
   and client-local overlay/inspector/list ownership or endpoint selection. Local
   state keeps its existing input leases and ownership checks; it is not host data.

Not dependencies: task/status/agent observation churn, titles/labels, cwd/Git,
notifications, tab status tokens, host document bodies, release/config notices,
terminal cell bytes/content sequence/cursor/scroll position, graphics payloads,
or provider refresh. A document changing actual structural target eligibility
is a change under items 3–4, not an exemption. No per-key/per-byte aggregate or
provider refresh computes this inventory. Host facts are captured once per
publication, using narrow existing runtime accessors.

## Counterexample schedules / required characterizations

- Legacy `S1/F1/C1 -> S2 -> key -> F2 -> C2`: harmless S2 must not be separately
  exposed while F1 is displayed. New scene2 is one event; key runs wholly before
  or after it. Genuine changes may explicitly revoke before scene2.
- Surface F2 and collapse C2 in one socket write is insufficient: the legacy
  reader emits separate events. The new full transaction decodes to one event.
- Patch before base, wrong base, stale generation or wrong boot: reject without
  installing snapshot, targets or collapse data; request/await a full base.
- Status/task/document-only S2 while render queue is full: old coherent scene1
  continues classifying keyboard input. Its route epoch does not change.
- Real focus/runtime/attachment/topology/geometry change: old route closes even
  with old pixels retained. A stale scene or patch never clears newer revocation.
- Release after revocation retains existing cleanup behavior; exact replacement-
  safe release and batch disposition are explicitly deferred to the next lane.

Protected characterization suites include collapse coherence/input refusal,
collection/inspector committed text, popup focus, surface patches, endpoint
activation, frozen wire tags/digests, hidden-member persistence/full-tree export,
and sliver geometry. Identity changes require adversarial invariant checks.
Native scheduling and paired fixed-geometry 1/15-pane measurements accompany the
finished implementation; they are not evidence of whole-server freeze causality.

## Codec / version decision

New named codec `shell.scene.v1`; do not reinterpret `shell.surface.v2` or any
frozen codec. Append one explicitly negotiated scene variant after existing
`ServerMessage` values; private protocol 25 becomes 26, endpoint generation stays
2, API JSON and session persistence are unchanged. Scene-capable peers only are
served this lane; predecessor-only greetings refuse client-locally before any
surface or geometry effect. Historical surface/snapshot values, tags and fixture
digests stay unchanged as decode inputs, not re-blessed new expectations. New
scene fixtures and tests cover the new shape and negotiation. This supersedes
the surface-v2 note's append-only removal condition for this named extension,
under the owner's 2026-11-14 authorization and development no-floor decision.

The initial/inactive endpoint metadata snapshot remains an observation, never
active terminal routing authority. Auxiliary structure/agent/document facts used
for active classification must be inside the scene transaction, not ahead of it
on the control lane. Graphics transfer lifetimes retain their existing ordering.
