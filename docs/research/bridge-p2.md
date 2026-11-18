# Pi bridge P2 hygiene and idle work (2026-11-18)

Python-only hardening on `bridge-p2`, based on `f9a537c`. No native port, new core primitive, runtime restart/input
replay or execution/lifetime guarantee. The backend still owns the terminal program; the bridge owns its Pi pipes.

## Import isolation

The old default `python3 -m pi_bridge` ran in the agent project with the extensions directory on `PYTHONPATH`.
Python put cwd first, so project `uuid.py`, `queue.py`, or a `pi_bridge` package executed before normal startup.
The black-box default-launcher regression fails all three shadow cases on the baseline.

The default is now literal `python3 -I /installed/extensions/pi_bridge/launch.py`. Isolated interpreter startup
excludes cwd, environment Python paths and user site packages before bridge imports. The script adds only its own
trusted installation to its interpreter path, without modifying `PYTHONPATH`. `bridge_argv`/`bridge_env` remain
literal host configuration; no shell or core-platform interpreter inference. Custom launchers own their import
isolation. This is platform-neutral launch configuration, not implemented Windows transport or remote placement.

The isolated CLI, native fake-Pi E2Es and three project-shadow regressions use the installed-directory layout.
The package installer already copies the whole package, including the new script; it was not run for this task.

## Child environment

The bridge no longer injects a Python module path at default launch. Legacy/custom launchers may still need one;
before starting Pi, remove only this loaded bridge installation from `PYTHONPATH`, preserving unrelated user
entries verbatim. Unresolvable unrelated paths are not used as grounds to discard the user's configuration.

Pi/tools receive no `HERDR_*` or `PRIM_*` internals except the opaque `PRIM_SOCKET` and
`PRIM_SOCKET_SOURCE=terminal`. The marker preserves the existing rule that an agent's explicit scratch state
selects its own endpoint rather than accidentally using the ambient terminal endpoint. Supervision tokens,
bootstrap credentials and core configuration are not inherited. This is not a same-user sandbox: the bridge
process itself and its bootstrap environ remain in that documented trust boundary.

An actual fake runtime captures its environment and asks Python to locate `settings`. The baseline retains the
bridge path; fixed behavior preserves the user path, cannot locate prim's settings extension, and retains exactly
the terminal endpoint/marker among `PRIM_*`. No duplicate extension is started to prove the leak.

## Event-driven idle behavior

Three independent wakeup sources accounted for the reported idle CPU:

- Owned-child returncode polling every 20 ms. The client now extends asyncio's existing stream protocol with its
  native `process_exited` callback. This is independent of inherited-pipe EOF, unlike `Process.wait()`. Pending
  readers still drain for the bounded configured interval; completed readers no longer spin that drain loop.
- The fallback thread's 50 ms queue timeout. It now blocks on the existing one-frame mailbox, wakes on data/close,
  and retains bounded nonblocking shutdown if the terminal stops reading.
- The unconditional 5 Hz snapshot/view rebuild. The publisher waits for client journal revisions or explicit
  control/connection changes. Only dirty ordinary output is sampled/coalesced; user controls and diagnostics stay
  immediate. Idle has no sampling timer. Revision waits cannot miss an event that occurred before subscription.

Deterministic tests cover no idle content rebuild (baseline called it 3 times in 350 ms), no sleep-based exit poll,
blocking fallback without a timeout, journal wakeup edges, bounded descendant-held pipes, EOF/crash cleanup,
deadlines, dialog cancellation, reconnect/flapping backoff and transcript coalescing. No effects are resent.

## Measurements and validation

Reused the bridge-language reviewer's fake core/launch measurement shape, with a private fake endpoint, fake Pi,
env-clear launch, nice 10 and an 8-CPU affinity. Each sample observes 200 live bridges for 30 s after 10 s warmup; all 200
close on stdin EOF with exit 0.

| Measurement | Before | After |
|---|---|---|
| Idle bridge CPU, sum over 200 | 114.3% of one core | 0.0% measured (no CPU ticks observed during 30 s) |
| Prompt/abort fixture, period 3 | Not rerun here | 8.99% of one core; 200/200 alive |
| Idle PSS/private per bridge | ≈13 MiB | ≈13 MiB |

Interpreter memory is **not** fixed, and fake-runtime numbers are not real-model CPU claims. The original Python
bridge process-per-runtime design is unchanged. Linux/Python 3.14 was exercised; macOS/Windows execution remains
unverified. No pidfd, Unix signal or other OS-specific exit mechanism was added to the client.

Local evidence under `~/.local/share/herdr/scratch/prim-bridge-p2/`: `import-before.log`, `env-before.log`,
`idle-before-test.log`, `idle-{before,after}.jsonl`, `load-after.jsonl`, `python-final.log`, `provider-final.log` and
`full-final.json`. Full release acceptance is the final `scripts/check.sh full` receipt, not these focused tests.
The reviewer harness logs may report a fake-core BrokenPipe while its clients close; all measured bridges and
owned fake children exited 0. No live prim/hc/tmux, user configuration, model calls, install or push were used.
