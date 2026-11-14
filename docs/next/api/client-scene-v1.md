# Client scene v1 availability

Status: implementation in progress; not installed or an exact-input guarantee.

The accepted [atomic scene decision](../architecture/atomic-client-scene-v1.md)
introduces `shell.scene.v1` as a negotiated surface codec for endpoint generation
2 and private protocol 26. JSON API `vqro.api.v1`, input semantic v1 and session
persistence do not change. Existing snapshot/surface codecs remain immutable
decode inputs. Serving requires the scene codec; predecessor-only peers receive
a client-local greeting refusal before surface activation.

A full transaction is one binary server frame and one client-loop event. It
contains a route epoch, the existing named JSON snapshot/structure/agent-view/
host-document/collapse projections and a full surface-v2 value. All contained
projections name that surface's boot and projection revision. Optional projections
are present according to negotiated capabilities. Validation precedes installation;
unknown, duplicate, mismatched or malformed transaction members cannot partially
replace installed routing state.

Patches retain their existing named shape: boot ID, projection revision and exact
installed base-surface revision must match. They update cells, not route authority.
A separate `shell.scene.route-revoked.v1` control closes older route authority for
genuine routing changes while retaining old pixels. Only a full scene at or above
the revocation epoch can reopen it. Task/status/document-only churn advances scene
revision, not route epoch. Geometry conservatively revokes keyboard routes as well
as mouse routes in this first shipment.

This is presentation coherence only. Normal input is still the existing admission
lane: no exact runtime batch admission, queued/rejected disposition, losslessness,
automatic replay, or whole-server freeze fix is claimed.
