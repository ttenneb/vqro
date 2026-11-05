# Client TTY ownership observation

Status: **experimental implementation**. Linux ownership publication and cached
status have private real-server verification. Native macOS execution remains
unverified. Windows has no proved peer-console path for new human clicks and
remains read-only; existing CLI actions are unchanged. This method alone does not
enable view actions or grant a human caller.

The authority design and accepted absent-publisher limitations are recorded in
[local TTY client action attribution](../architecture/client-tty-action-attribution.md).

## Optional read method

`server.pty_ownership.status.v1` is a bounded raw JSON API/native endpoint read:

```json
{"id":"ownership","method":"server.pty_ownership.status.v1","params":{}}
```

A response is either unavailable (`status: null`) or the server's cached facts:

```json
{"id":"ownership","result":{"type":"pty_ownership_status","status":{
  "server":{"boot_id":"kernel-boot-namespace","pid":123,"birth":456},
  "epoch":7,"complete":true
}}}
```

`complete` means the exact current, fully proved ownership epoch has been
successfully published. Ownership changes mark it dirty **before** creation,
import, transfer or removal. Missing runtime storage and publication failures
never prevent terminal/session operations. This response performs no filesystem,
process, provider or registry probes and never initializes a publisher; absence
of support/status is a read-only outcome for new client-human actions.

A reader independently verifies the endpoint's kernel PID/birth/account and
requires live complete status matching its ownership record before and after
proof/audit. JSON identity and a file's COMPLETE flag are not proof by themselves.
A late old publication acknowledgement cannot complete a newer epoch. A changed
epoch invalidates earlier admission evidence rather than being silently adopted.

The method is optional and additive; no frozen codec or previous named method
shape changes. Old servers may reject it and must not be assumed complete.

## Fixed volatile records

Participating servers use a private fixed OS-user namespace independent of
config/session/socket overrides. Records identify the exact server incarnation,
its existing endpoint locator, ownership epoch, publication completeness and
actual PTY slave devices. Record/aggregate counts and byte sizes are bounded.
Kernel-incarnation death/reuse may be ignored only when positively proved;
unreadable/unprovable live observations fail closed. Records from another server
are never cleaned by a reader.

Close removes ownership only after actual descriptor release; handoff attempts
overlapping publication without making transfer depend on it. Real private/test
servers may publish short-lived records in this real private namespace, and remove
only their own exact record after release/transfer. Totally absent pre-change or
first-publication-failed publishers remain the explicitly documented cooperative
coverage limits, not universal absence detection or authentication.
