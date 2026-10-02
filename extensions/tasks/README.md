# Experimental Tasks core

Linux trusted protocol-2 executable, Python 3 stdlib SQLite. Task policy and
persistence live here; the host supplies neutral invocation/caller observations.
Not sandboxed, not secure human authentication, not Pi todo integration.

- [Tasks policy, CLI, receipts and limits](../../docs/next/api/tasks-core-v0.md)
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

Installing/linking this trusted provider on a real installation requires owner
approval. No install or automatic activation is performed by these tests.
