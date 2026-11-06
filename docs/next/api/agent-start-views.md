# Agent-start views box

Status: implemented. This is a host-owned layout rule, not package authority or a
new combined view kind. See [generic collapse](../architecture/generic-pane-collapse.md).

## Configuration

The default is an ordinary terminal-free box below the terminal, with Tasks above
Messages. Both are views supplied by the existing Tasks extension:

```toml
[ui]
agent_start_views = ["vqro.tasks:tasks", "vqro.tasks:messages"]
```

Set `agent_start_views = []` to disable the rule, or supply an ordered list of
other `<extension>:<view>` identities. The earlier optional `agent_start_view`
setting remains an explicit single-view input override; an empty string disables
the rule. Only the resolved list is retained in runtime state. Every identity
uses the existing ASCII alphanumeric/`._-` validator, with a 1–120-byte namespace
and 1–64-byte view ID. An invalid entry rejects the complete group and produces
a startup/live-config diagnostic; it cannot create a valid prefix or set the
once marker.

The rule creates layout nodes only. It does not install an interpreter, grant
package capabilities, alter artifact/service bindings, or infer product counts.
Unavailable providers retain the ordinary unavailable-view behavior.

## Admission and intent

The existing once-per-terminal marker is preserved in session state. A terminal
already marked or covered by existing views is left alone: existing views are
not moved, retrofitted or reopened. Closing either child uses ordinary view close
normalization; one remaining child replaces its container, and closing both
removes that container. Later agent starts do not refill closed children.

The complete group must fit session/workspace/tab pane limits and the existing
per-identity box-depth budget. Failure leaves the tree, public numbering and
marker unchanged; no partial addition or fallback composition is committed.
View members create no terminal state or PTY. Scope is rederived from the nearest
enclosing nonempty full terminal membership, including hidden/collapsed members.

Placement is deterministically below, independent of terminal aspect ratio. The
outer frame belongs to the terminal-bearing agent group; its terminal body is
unframed and view children keep their inner frames. A single configured view
naturally reduces to a leaf. Existing layout operations can put the whole group
beside the terminal; this change chooses no new expanded side-width cap.
