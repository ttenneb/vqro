# Terminal client admission: private protocol 24 / endpoint generation 2

Status: accepted (owner-authorized 2026-11-01).

## Decision and scope

Vqro terminal connections require private protocol **24** for direct terminals
or endpoint generation **2** for client-owned shells. Protocols 22/23 and endpoint
generation 1 are retired for terminal admission. This supersedes the generation-1
floor in Collections authority transition and the previous endpoint availability
statements. It does not change the independent session format (currently v8).

The development-period no-compatibility-floor decision permits a coordinated
terminal cutover before ecosystem building. This explicit admission boundary
prevents silently mixing old and new terminal clients during selective upstream
ports. It is not a claim of universal old-client refusal: raw JSON API requests
remain `vqro.api.v1`, with no new envelope, version field or admission check.
Extension artifacts, adapter contracts and exact artifact pins are unchanged.

## Bootstrap and codecs

The outer bincode enum tags and recognizable hello prefixes remain unchanged.
The server reads the leading private version for direct/shell hello tags before
decoding geometry. An incompatible version gets a Vqro-branded Welcome error and
closes only that connection. Endpoint admission reads `generation` before core
hello fields; generation 1 gets `unsupported_generation`. No rejected hello
registers a client or changes session state. Malformed and oversized frames keep
the existing framing bounds.

No payload codec shape or meaning changes in this slice. The replacement list is
therefore **admission constants only**: private 23 → 24 and endpoint 1 → 2.
`endpoint.hello.v1`, `endpoint.welcome.v1`, `shell.snapshot.v1`,
`shell.surface.v1`, `shell.input.semantic.v1`, `shell.blob.v1`, optional controls
and advertised methods retain their names and frozen shapes. Future actual
shape changes require separately named codecs/methods, not reinterpretation.

Old hello/welcome/snapshot/method/handoff/session fixture files remain immutable
historical inputs. Current admission/refusal tests are separate expectations;
decoding an old fixture is not permission to attach an old client. A new client
also rejects an old server's error-free mismatched Welcome. Compatible clients
and other sessions are unaffected by another connection's refusal.

## Operation and verification

Client/server binaries must be updated together; restart the selected Vqro
session after backing up matching state under separate deployment authorization.
Do not live-handoff across this protocol cut. Rollback restores the matching
binary and state, not an older reader over newer state. No Herdr server changes
are authorized by this decision.

Focused tests cover direct and endpoint current/current success, old and future
refusal, minimal legacy bootstrap before body decode, old-server success replies,
frozen v1 input decoding and connection-local isolation. Raw API identity and
request behavior remain separately tested. Platform builds and full integrated
gates are independent evidence, not implied by these targeted tests.
