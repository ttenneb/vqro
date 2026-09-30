# Trusted semantic document providers

Manifest-v1 executable services declaring `provides = ["host_document"]` and requesting `capabilities = ["host_document"]` may select `document_source = "agent_activity"` or `"relationships"`, in addition to `"terminal_groups"`. Linking/enabling trusted local code grants the bounded source snapshot and document publication path, not task policy, raw terminal reads, a package host-call lease, or an OS sandbox. Reserved host namespaces remain unavailable. Non-reserved dotted namespaces use the same admission path; there is no bundled-only source privilege.

The protocol-2 service pipe calls public `host.document.render`. The response is existing `host.document.v1`. Terminal-groups reads are tab-scoped; activity and legacy relationship reads are session-scoped. Source/target, generation, revision, namespace, size, capability and document validation apply before publication. Desired snapshots, queues, retries and caches are namespace-scoped. Disable, unlink, rebuild and runtime loss retire authority; late replies cannot restore a retired generation. Coalesced events drive reads, never synchronous per-frame I/O. Publication rejection participates in bounded semantic retry.

Activity labels include `agent`, `display`, and optional `state_idle`, `state_working`, `state_blocked`, `state_unknown`: bounded display-only values from public `pane.report_metadata`. They convey no lifecycle authority. A Pi hook may label exact requested role text as observed there; later hooks/providers can mutate it. Neither `requested` nor `loaded at Pi hook` is final outbound attestation. Neutral `agent.list/get` and agent/client snapshot facts remain available when bundled status presentation is disabled.

## Extension-owned relationship context convention

A trusted publisher may project its **own** task-parent, owner and thread context using existing navigation-document fields. This is presentation-only, not server task/delegation authority or a task state writer. The future Vqro work extension owns real tasks, threads and acceptance; the example publisher is only a fixture.

- Every item ID is `context:<kind>:<opaque-id>`. Kind is a dotted, lower-case ASCII identifier (letters, digits, `.`, `-`, `_`), at most 64 bytes with no empty segments. Examples: `task.parent`, `task.owner`, `thread.link`. Other dotted kinds are allowed; none authorizes an operation.
- Section items have no terminal. Terminal items carry the public terminal reference/placement. Parent IDs refer to items in this same document and form an acyclic hierarchy. Item IDs are unique.
- At most 256 items; labels at most 240 UTF-8 bytes. No actions, action references or required action capabilities. Disabled terminal context is not displayed.
- Session/workspace/tab/pane targets must agree with the current neutral pane placement. Unknown panes, conflicting terminal/placement bindings, missing parents, cycles and malformed types reject the winning document for that scope.
- For each scope the highest host-assigned priority wins, with lexical namespace tie-break. Invalid winners do not silently fall back to a different provider. The agent sidebar displays at most three context rows per pane, including when no activity document or legacy delegation exists. Cached projection/identity joins happen once per endpoint, not per pane; no filesystem or process queries occur during render.

The `extensions/public-relationships/` fixture consumes neutral `host.terminal-groups.v1` reads and publishes an extension-owned fixture graph per tab. It never reads runtime delegation records. Real policy, durable work storage and the human composer are later work-extension features, not inferred from fixture labels.

## Legacy compatibility

The `relationships` source still supplies mechanical runtime delegation records. Its `relationship:*` session document convention and strict complete delegation/parent/terminal joins are separate from `context:*`. Its items are terminal/section; activity session documents use terminal/group items with `activity:*` groups. Empty documents do not claim relationship navigation. Existing neutral delegation structure remains readable when bundled delegation presentation is disabled, without granting its mutation methods.

Navigation accepts admitted non-bundled publishers, then joins runtime identities and agent status against current endpoint facts. Source scope/shape is checked at host publication; client labels never create agents or change lifecycle evidence. Client-owned Done acknowledgement stays local. All generation-1 codec fields, enum variants and endpoint method shapes remain unchanged: the context convention uses existing string IDs, hierarchy, labels, targets and references.

Examples: `extensions/public-activity/`, `extensions/public-relationships/`. They read only public service input; optional `pi-role-status.ts` reports metadata via public CLI. Manifests target Linux/macOS. Only Linux is privately validated; macOS/Windows parity is not inferred from schema/unit tests.
