# Tasks durable attention v0

Status: implemented working path; local trusted/cooperative-human assurance only.

An enabled Tasks document provider publishes an actionless navigation document
alongside the [full read-only view](tasks-view-v0.md). The native sidebar defaults
to a flat, owner-labelled attention list. Agents remains available through its
toggle, with neutral facts and terminal navigation unchanged.

Tasks owns the policy: decisions/acceptance owed to the local human, durable
blocked/missing-acceptor debt, and unavailable owner placement. Ordinary human
self-work is not answer/acceptance debt. Live execution/stall categories stay in
the full pane; native placement never proves execution or acceptance authority.

The host supplies optional `agent.activity.v1.terminals`: when present, complete
current placed terminal references, including non-agent terminals; absent means
unknown. Only the bound owner's terminal places a task. Missing placement is
global/unavailable, never inferred from a parent, acceptor or name. Space grouping
uses Spaces' admitted checkout membership; tabs use current host placement.

Native rendering reads admitted cached documents, not task stores, processes or
terminal bytes. A successful action may return optional opaque
`document_revision`; changes request a bounded sample for that exact declared
provider's document namespace. Sampling currently applies only to actionless
`agent_activity` documents, not interactive/action-bearing documents or other
sources. The hint is not part of persisted/replayed operation outcomes. Equal normalized output preserves publication/action revisions;
changed placement retires old observation immediately. Failed/disabled providers
show unavailable rather than healthy zero, and retirement fences late replies.

An enabled, healthy Tasks provider with no `tasks.sqlite` yet publishes an empty
observation: a fresh install works before its first core action. The document
path creates no directory/database and performs no migration or policy write;
a later sample opens the store once the core action runtime has initialized it.
Present unreadable, corrupt or unsupported-version stores still fail unavailable,
not healthy zero. This is package-owned absence policy, not host task knowledge.

Known follow-up limitation: a background decision deadline auto-apply has no action
response carrying the revision hint. Native badges refresh on the next action,
read or host-source change; the full Tasks pane's own polling observes it sooner.
There is no additional host timer or protocol mechanism for this case.

A publisher supplies `context:attention.view:<plugin>:<entrypoint>`, optional
`context:attention.measure:<0|1|2>` labels, count-bearing
`context:attention.badge:<workspace|tab|session>:<id>:<n>:<n>:<n>` items and flat
`context:attention.row:<id>` items. All are bounded actionless sections. Native
code treats rows/labels as opaque, and only sums counters over admitted Space
membership. This is a narrow presentation convention, not a host task ontology.

Badge/list clicks open the existing plugin pane with read-only
`VQRO_ATTENTION_SCOPE` JSON (`kind`, `ids`). Its durable filter shares Tasks policy
with the document producer. Sampling is asynchronous, so scopes observe current
placement and may supersede a badge before the pane's next read. Old hit maps are
fenced by document namespace/generation/revision. The existing `plugin.pane.open`
method is separately advertised as optional: absence disables only that action,
not the endpoint connection. No generation-1 codec or method shape changes.

There is no task mutation, message delivery, agent activation or automatic routing
in this presentation. No model comprehension, authenticated human presence,
reproducible-build or non-Linux assurance is claimed.
