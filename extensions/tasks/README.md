# Experimental Tasks core

Trusted protocol-2 executable, Python 3 stdlib SQLite. Task policy and
persistence live here; the host supplies neutral invocation/caller observations.
Not sandboxed, not secure human authentication, not Pi todo integration.

Linux/macOS use `$XDG_STATE_HOME/vqro/extensions/vqro.tasks` when set, otherwise
`~/.local/state/vqro/extensions/vqro.tasks`. Windows uses the absolute local
`%LOCALAPPDATA%/vqro/extensions/vqro.tasks` directory. The provider creates it;
`provider.py --data-dir` overrides it. Windows needs an explicitly configured
absolute CPython executable in the exact admitted manifest (see below). Native
macOS/Windows verification is required; process-based demos remain Linux-only.

Durable addressed messages are a second record family in the same store
(`messages.py`, schema v4): `vqro message send/list/claim/edit` with the sender
taken from the host-supplied caller, exact-execution recipients, edit before
claim, and one human-controlled activation setting that a fresh store defaults
on. Delivery to Pi is the reply-routing adapter claiming at a turn boundary,
never pane typing. When a delivered message owns an agent turn, that turn's
final text is recorded as a durable reply message to the exact sender binding,
always with `reply_to` (a same-sender batch links its newest message). Ownership
is positive: the run's own observed input carries the delivery's unique message
id and that observation is untruncated, so a different,
transformed-without-it or truncated prompt is not proven, is
not auto-replied, and keeps its "no reply" label so the recipient answers
explicitly. A handoff queued behind
in-flight work, or a human turn that wins the race, is therefore not auto-replied,
and a delivered reply is never re-answered automatically. `vqro agent prompt` is human-only, so agents address
each other with messages rather than pane input.

Presentation: this extension publishes **views** - a Tasks panel and a
Messages panel per pane - which Vqro draws in the pane's top slot
(`extensions/tasks/document.py`). The full-tab Tasks terminal view is retired:
the tab pane entry is gone from the manifest, and a badge click opens the pane
panels instead of launching another terminal app. `view.py` stays as the
extension's formatting library and as the shape reference for the retired tab.
Human-owned tasks remain available when their recorded terminal closes; missing
placement is Global, not a human liveness warning. Agent-owned tasks retain the
unavailable/not-running indication when their execution has no placement.

- [Windows interpreter setup, identity and native trial](../../docs/next/api/tasks-windows.md)
- [Tasks policy, CLI, receipts and limits](../../docs/next/api/tasks-core-v0.md)
- [The retired full-tab view, kept as the shape reference](../../docs/next/api/tasks-view-v0.md)
- [Public generic invocation and authority boundary](../../docs/next/api/extension-action-invoke-v1.md)

```sh
just task-core-test
# Already-built Vqro + already-installed Pi; creates a NEW private scratch root.
python3 -B extensions/tasks/demo.py --vqro /absolute/path/to/vqro --pi /absolute/path/to/pi
# CLI failure characterization only: fixture replies are NOT task acceptance.
python3 -B extensions/tasks/cli_fault_demo.py --vqro /absolute/path/to/vqro
```

The real graph demo uses two model-free Pi processes and retains receipts and its
SQLite database. It exercises atomic human-approved import, readiness, quoted
criteria/evidence, independent acceptance, own steps, restart and exact execution
replacement, then measures a separate 100-node/400-edge import. `IMAGES.json`
records the source head and hashes of retained native binary/provider snapshots;
the run never follows a subsequently rebuilt native target or edited provider. It verifies a private configuration canary and birth-checks
owned processes for cleanup. Do not run it with a real user's HOME/socket/config.
It never installs tools or changes the human's Pi/session files.

Once explicitly admitted, open the read-only terminal tree with:

```sh
vqro plugin pane open --plugin vqro.tasks --entrypoint tree --placement tab
```

Click filters, expansion markers, age controls and owner links. Owner navigation
requires a fresh exact execution observation; replacement/unknown owners never
silently inherit a link. `q` exits the terminal UI. No tasks are mutated by it.

Installing/linking this trusted provider on a real installation requires owner
approval. No install or automatic activation is performed by these tests.
