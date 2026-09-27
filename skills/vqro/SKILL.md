---
name: vqro
description: Control vqro terminal workspaces, panes, agents, repository Spaces, collections, and sessions through the CLI/API.
---

# vqro agent guide

vqro owns persistent terminal sessions. Terminals are canonical: use pane commands for ordinary processes and agent commands only after a supported coding agent is detected.

## Safety

- Read IDs from command output; never guess workspace, tab, pane, terminal, collection, or delegation IDs.
- Do not restart or stop the active session unless explicitly requested.
- Use one bounded subagent per task. Do not recursively spawn agents.
- Prefer waits and state reads over arbitrary sleeps.
- Keep Herdr and vqro routing separate. vqro uses `VQRO_*` environment variables and `vqro.sock`.

## Discover the installed interface

```bash
vqro --version
vqro --help
vqro workspace --help
vqro pane --help
vqro agent --help
vqro api schema
```

The installed binary is authoritative because alpha commands may change.

## Core workflow

```bash
vqro workspace list
vqro pane list
vqro pane split <pane-id> --direction right --cwd "$PWD" --no-focus
vqro pane run <pane-id> <command>...
vqro pane read <pane-id> --source detection --format text
```

For agents:

```bash
vqro agent start <name> --kind <agent> --pane <pane-id>
vqro agent prompt <name> "bounded task"
vqro agent wait <name> --until idle --until done --timeout 300000
vqro agent read <name> --source detection --format text
```

Close completed agent panes after incorporating their output.

## Spaces and collections

Repository identity and checkout grouping are shared server facts projected as Spaces. Non-Git workspaces remain valid. Use `vqro collection --help` and `vqro delegation --help` for orchestration. A collection is an ordered set of real panes with one selected preview. Its in-pane header exposes previous/next member controls, delegation provenance, and inspection. Splitting a selected member splits the collection leaf.

## Terminal inspector

Right-click a pane and choose **Inspect terminal**, or use a collection header’s inspect control. Telemetry is sampled only on request and excludes argv and environment data. Enabled `pane_inspector` extension services may return bounded declarative contributions; vqro validates and renders them as an embedded component inside the target pane. Services are trusted same-user subprocesses, not sandboxes; protocol grants only constrain host effects. Press `r` to refresh and `Esc` to return to the terminal.

## Named-session isolation

When controlling a named session from another shell, explicitly select it and clear stale overrides:

```bash
env -u VQRO_SOCKET_PATH -u VQRO_CLIENT_SOCKET_PATH VQRO_SESSION=<name> vqro pane list
```

Never substitute `HERDR_*` variables; those belong to a separately installed Herdr runtime.
