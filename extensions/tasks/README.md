# Experimental Tasks core

Linux trusted protocol-2 executable, Python 3 stdlib SQLite. Task policy and
persistence live here; the host supplies neutral invocation/caller observations.
Not sandboxed, not secure human authentication, not Pi todo integration.

Durable addressed messages are a second record family in the same store
(`messages.py`, schema v4): `vqro message send/list/claim/edit` with the sender
taken from the host-supplied caller, exact-execution recipients, edit before
claim, and one human-controlled activation setting that a fresh store defaults
on. Delivery to Pi is the reply-routing adapter claiming at a turn boundary,
never pane typing. When a delivered message starts an agent turn, that turn's
final text is recorded as a durable reply message to the exact sender binding
(with `reply_to` when one message started it); a delivered reply is never
re-answered automatically. `vqro agent prompt` is human-only, so agents address
each other with messages rather than pane input.

- [Tasks policy, CLI, receipts and limits](../../docs/next/api/tasks-core-v0.md)
- [Read-only pane, filters and exact-owner navigation](../../docs/next/api/tasks-view-v0.md)
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
