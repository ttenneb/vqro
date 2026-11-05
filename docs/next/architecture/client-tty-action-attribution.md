# Local TTY client action attribution

Status: **accepted design, implementation in progress** (2026-11-05).
This does not advertise method availability. The first consumer is Tasks view
acceptance and listed decision answers; other task/message mutations are outside
this slice. Existing local CLI attribution and frozen endpoint codecs are unchanged.

## Authority

An attached local client may use a new `human_unverified` caller source only after
the host observes its kernel peer account, PID and process birth, requires the same
account, and runs the existing ancestry classifier. Complete, current ancestry is
required: a bounded capture that stopped before its real platform boundary is not
proof of nonownership. Any Vqro server in the ancestry, any Vqro-owned pane, an
agent/unknown classification, uncertain evidence, or an absent controlling TTY
prevents an external-human upgrade. Known server ancestors are matched by
kernel-observed boot/PID/birth from the live ownership proof, regardless of their
executable name or argv. As a conservative fallback for absent publishers, any
ancestor with a literal `server` command argument also denies authority, regardless
of executable name or argument position; this can deny unrelated server launchers.
The CLI parser is not invoked during attribution. Neither a gesture nor a pane/client
identifier is human authority.

A real controlling TTY must be corroborated by platform-local kernel/device
observations and must not match an observed owned PTY. Compare actual slave or
controlling-device identity, not generic ptmx master device identity or server
stdin. Linux and macOS use their existing peer/process provenance plus native TTY
observations. The macOS legacy foreign-euid boundary is not complete negative
ancestry proof: clients stopped there remain read-only unless a genuine complete
root chain is available. Legacy positive CLI attribution is unchanged. Windows
does not yet have an equivalent proved peer-console path:
these clicks remain read-only with a CLI pointer. Unsupported checks fail closed.
This is cooperative same-user process classification, **not authentication** or
proof of a physical human gesture.

## Fixed ownership observations

Every participating server publishes a bounded, atomic, private ownership record
in one fixed OS-user namespace, regardless of config/session/socket overrides:
owner-validated `/run/user/<euid>/vqro/pty-owners-v1` on Linux and the native
per-user runtime/temp root with the same Vqro suffix on macOS. Records identify
server boot/PID/birth, the existing endpoint locator, ownership epoch,
publication-completeness and owned PTY slave identities. Endpoint locators and
JSON PIDs are not credentials; readers bind the live endpoint to its kernel peer.

A missing or unusable namespace/publication must never prevent server startup,
PTY creation, import, restore or handoff. A failed publisher logs prominently,
disables new client-human authority locally and retries on later ownership
changes. No heartbeat or process/file-descriptor census is introduced.

A file's `complete` flag is not freshness proof. Before each ownership change,
the server marks its authoritative cached ownership epoch dirty. The proposed
optional `server.pty_ownership.status.v1` read exposes this narrow cached epoch
and publication state without file/process/provider work. Readers require a
kernel-bound, live, complete epoch matching the record before and after their
snapshot. Thus a failed update leaving an older COMPLETE record cannot grant
permission. Missing, unreadable, unsupported, dirty, mismatched or unprovable live
observations keep clicks read-only. Ignore dead/reused server incarnations only
when positively proved; observation failure is not death.

Recheck observed epochs after asynchronous peer proof and checked audit writing.
Carry proof epochs into final admission and recheck the local authoritative epoch,
connection, caller, document, provider and confirmation identity immediately
before queueing. A preparation token never bypasses fresh ownership proof.
Publication/observation is bounded and off rendering; only lifecycle changes
publish. Old ownership is removed only after actual release/transfer; handoff
attempts overlapping publication without making terminal recovery depend on it.

### Explicit coverage limits

Pre-change servers that publish no record are invisible to record enumeration.
A current server whose **first** publication fails is also invisible to peers:
it logs prominently, disables its own new authority and retries on later ownership
changes. Peers cannot infer a missing file that never existed. These residuals are
explicitly accepted limits, not a universal absence/completeness guarantee.
Complete ancestry exclusion remains mandatory. No adversarial same-user or
cross-version authentication guarantee is made.

Private/test servers may write short-lived records in this real private per-user
namespace, including real fixed-namespace demonstrations, but may remove **only
their own exact incarnation's record** after release/transfer. Other test/config/
session/capture files remain private. Do not redirect a real fixed-namespace test
and then claim cross-root verification.

## Caller, dispatch and audit

Existing terminal-bound human and agent caller forms stay unchanged. The new
human source carries a bounded host-derived `client` object with boot/PID/birth
and corroborated TTY identity, plus UID or SID, and no fabricated `terminal_id`.
Providers explicitly advertise `action.invoke.client.v1`; services supporting only
`action.invoke.v1` never silently receive the new caller form. Both use the
existing bounded generic action worker and provider authorization.

The proposed optional public JSON/native `host.view.action.prepare.v1` and
`host.view.action.submit.v1` methods resolve exact accepted SESSION documents,
items, publisher generation and ActionInvoke grant. They do not reinterpret the
existing tab-only host-document action method or its interrupt tokens. Raw API
context requires a unique registered connection matching the kernel peer
incarnation, not a supplied client identifier.

A bounded single-use preparation binds connection/boot/caller, document/subject
revision, provider generation, option and original operation ID. Irreversible
choices require confirmation of that exact choice. Staleness invalidates selection
and refreshes rather than substituting a newer task revision or retrying a write.
Unknown outcomes preserve original receipt references and are never replayed with
a fresh operation ID. Tasks maps the opaque subject through ordinary Store.invoke;
its acceptor, human-only answer and state/revision checks remain authoritative.

Every use of the new caller has checked attempt logging containing host-derived
PID/birth/TTY and correlation, including rejected/unknown outcomes. Filterable or
silently uninitialized tracing alone is insufficient; failed attempt write disables
authority before effect admission. Existing durable provider receipts retain the
full original caller. Preserve a **known provider reply** if later audit writing
fails: surface an audit-health failure separately, never rewrite a known success
or rejection as unknown. No new audit database, task authority in core or package
host calls are introduced.

## Compatibility and presentation

No frozen codec or existing method shape is changed. New methods/caller forms are
explicitly optional; missing support disables only the action. Cached view metadata
remains package-described and host-drawn; unsupported/malformed metadata is
read-only. UI state remains client-local; render performs no provenance/provider
queries. The host draws all listed choices, default and decision-wide reversibility
and confirms irreversible answers. No additional task workflow actions are implied.
