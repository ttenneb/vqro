# Extension-owned relationship context fixture

This trusted executable publisher consumes public, bounded `host.terminal-groups.v1` snapshots (`document_source = "terminal_groups"`) and projects its **own fixture graph**, not runtime delegation records. It publishes a section `context:task.parent:fixture-task` and terminal `context:task.owner:<terminal-id>` references with `Owner context` labels. It has no private sockets/files, network calls or host delegation dependency.

The public context convention supports typed task-parent/owner/thread links; the future Vqro work extension is the real publisher and owns durable policy. This demonstration does not create real tasks, assign agents, accept reports or launch anything. Context is extension-owned presentation, never server task authority. See `docs/next/api/trusted-semantic-documents-v1.md` for bounds and strict neutral pane/target joins. Agent sidebar context is visible independently of the activity provider or legacy delegation graph.

With approved runtime effects, `vqro plugin link <absolute-directory>` registers this manifest. Disable/unlink clears publication authority; late replies from revoked generations are rejected. No reserved namespace exception is used. The separate legacy `relationships` read source and strict delegation projection remain supported. Linux/macOS manifest; macOS is untested.
