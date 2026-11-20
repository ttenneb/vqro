# Gate interruption owns a job, not just a launcher

The interrupted-gate incident exposed two independent lifetimes. The shared program slot lock's open file description had been inherited by descendants; coordination fixed that wrapper by closing the descriptor for the invoked gate. This change does not modify that separately-owned fix.

The second problem was `check.sh`'s `exec systemd-run --scope`. A scope remains alive while members survive; killing its launcher/main shell is not a request to stop the scope. The end-of-script leftover scan never runs after interruption. A shell EXIT trap alone cannot cover SIGKILL, and parent death does not normally signal an orphaned child.

## Mechanism

`check_lifetime.py` launches one transient user **service**, with native `ExitType=main`, `KillMode=control-group`, a two-second stop grace, and collection after completion/failure. It preserves streaming stdin/stdout/stderr through `--pipe`, the working directory, CPUWeight50, and an already-lower caller nice level. Native service teardown, rather than a process-name/root heuristic, owns all members including session-detached, reparented and SIGTERM-resistant fixtures.

The service MainPID waits on the actual gate and blocking Linux pidfds for its original launcher and launcher-parent identities. `check.sh` captures the parent PID/start token before bootstrap subprocesses, and the MainPID validates generation and liveness before starting the gate. Owner death during/after service launch exits MainPID; native systemd teardown follows. Default TERM/INT/HUP signal termination also ends ownership. SIGKILL of the launcher or actual gate is covered without depending on a shell trap. PID reuse is not treated as continuing ownership.

The owner watch is justified by an actual private negative: removing it leaves the native job alive after parent death. Changing the native exit lifetime back to `cgroup` also leaves members alive after controller death. Both controls fail their cleanup assertion and are explicitly stopped afterward.

Only nonsecret gate controls are forwarded into an `env -i` service command, with systemd environment expansion disabled so literal `${...}` in paths survives unchanged. Arbitrary credentials and HERDR variables are not serialized into ExecStart. A failed service launch is never replayed unscoped. When no user manager is available, quick/full retain the existing UNSCOPED/report-only path; it never kills a process inferred from another test root. A functioning scoped path requires Linux pidfds and modern systemd with the documented service/argument-expansion properties (validated here on systemd259.8). The service preserves cwd and standard I/O, not arbitrary caller descriptors or mount/network namespaces; suites still own their explicit private roots and isolation. An unsupported/failed scoped launch fails rather than silently running a possibly-started gate again.

The leftover inventory excludes the supervising ancestors as infrastructure, as well as the current script/live helpers. Detached fixtures remain visible. Service membership is accepted only for the exact internally-named unit, not a caller's old SCOPED marker.

## Tests and limits

`tests/test_check_lifetime.py` stages the **real check.sh bootstrap and collector**, with its ordinary suites replaced by a tiny private process forest. It checks normal completion; TERM/INT/HUP and SIGKILL of the controller; SIGKILL of the actual gate; running-parent death; and a paused service launch whose parent dies before admission. The fixture includes a double-forked, session-detached, SIGTERM-resistant member and an unrelated owned sentinel. Assertions use pidfds and cgroup population, not just process names. Output must be visible before job completion. Missing/stale owner, environment filtering, no-replay-on-launch-failure, and quick-mode unscoped fallback are separate small controls.

Real service tests skip with an explicit reason if the Linux APIs/user manager are unavailable. They start no backend, model, tmux, Herdr or build; every native unit and launcher process group belongs to the fixture. The regression is native lifecycle evidence, not a whole release-image gate, real-PTY behavior or retroactive cleanup of the incident's foreign processes.
