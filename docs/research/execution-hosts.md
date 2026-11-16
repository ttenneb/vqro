# Execution hosts: prim-host vs herdr-core (2026-11-16)

Source: subagent, reading herdr-core at `bb8a2f7` (`~/Projects/herdr-core/crates/herdr-core/src`) and web sources.
[U] = unverified.

## Guarantees (Y yes, N no, ? undocumented)

| | client restart | host upgrade | dies with host | orphan reclaim | output while detached | exit retained |
|---|---|---|---|---|---|---|
| dtach | Y | ? | implicit SIGHUP¹ | N | N (redraw only) | ? |
| abduco | Y | ? | implicit¹ | N | ? | Y |
| shpool | Y | ? | ? | ? | rendered screen | ? |
| zmx ([arch](https://zmx.sh/architecture)) | Y, daemon per session | ? | implicit¹ | N | screen + scrollback (ghostty-vt) | ? |
| tmux | Y | N [U] | implicit¹ | N | pane history [U] | ? |
| zellij | Y | N (resurrection re-runs commands) | ? | N | ? | N |
| **herdr-core** | Y | **Y via draining old holders, no fd handoff** | Windows Y (Job, `windows.rs:968`); Unix only implicit SIGHUP (`unix.rs:720`) | after core crash Y (`holder.rs:196-217,441-464`); after holder crash N | screen + 1 MiB tail (`holder.rs:15`) | Y, capped at 500 (`terminals.rs:10`) |

¹ On last close of a PTY master, SIGHUP is sent to the controlling process
([POSIX close()](https://pubs.opengroup.org/onlinepubs/9799919799/functions/close.html)); it can be ignored.

## herdr-core's shape

A minimal **holder** owns every PTY, parents every program and always reads output (`holder.rs:1-4`); a restartable
**core** holds policy. The holder restarts a crashed core and hands it the terminal list and 1 MiB tails. Upgrades:
one holder per build; the old holder is told to `DRAIN`, keeps its programs, refuses new spawns, and exits when its
last terminal ends (`runtime.rs:401-423`, `holder.rs:277-327`). No SCM_RIGHTS or re-exec. This is the same split as D1.

**Gaps against `spec/execution-v0.md`** (holder-level; an adapter cannot add them):
1. No pipe children; PTY only, stderr merged (`holder.rs:331`, `unix.rs:687-725`).
2. No byte offsets; `terminal.read` returns text with `complete: bool`; `missed` is an event-stream snapshot (`events.rs:1-2`).
3. **Opposite backpressure:** when the core is 8 MiB behind, the holder **blocks the child** (`holder.rs:17-18,385-395`).
   Slow viewers get dropped backlog and a repaint (`wire.rs:238-250`).
4. No explicit dies-with-host on Unix; Windows Job allows breakaway.
5. Exit retention evicts at 500; no client-chosen IDs, `boot_id` or `guarantees`.
6. Policy inside the host: agent status, keep/restart, templates, machines.

## Prior art for the hard parts

- Bounded byte ring with absolute offsets is known: Eternal Terminal keeps the last N bytes plus sequence number and
  resends the gap on reconnect ([ET](https://eternalterminal.dev/howitworks/)). tmux discards and redraws; mosh syncs screen only.
- systemd FD store keeps fds across restarts, Linux-only, no parenthood ([systemd.io](https://systemd.io/FILE_DESCRIPTOR_STORE/)).
- `execve` keeps the PID and non-CLOEXEC fds ([execve(2)](https://man7.org/linux/man-pages/man2/execve.2.html)), so re-exec in place keeps
  the host as parent (inference).
- Linux PDEATHSIG fires on creating-*thread* exit; subreapers adopt orphans. macOS has neither [U].
  Windows `KILL_ON_JOB_CLOSE` ([MS](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_limit_information)).
- Crates: portable-pty 0.9.0 (MIT, last release 2025-02), pty-process 0.5.4 (MIT, Unix [U]), alacritty_terminal 0.26.0
  (Apache-2.0, ConPTY), zellij-server 0.46.0 (MIT).

## Hard, ranked

1. Exit status and reclaim across host death (only the parent can `waitpid`): re-exec in place, per-proc shim (zmx/dtach), or Linux subreaper.
2. `survives_host_upgrade` vs `child_dies_with_host` only coexist if upgrade is a handoff.
3. Dies-with-host on macOS and for pipes needs a watchdog or shim.
4. Last output ordered before the exit event, with offsets continuous across handover.
5. ConPTY and Job breakaway (deferred, D12).

## Tests proposed

T1 kill matrix on herdr-core (core vs holder, with and without `trap '' HUP`); T2 upgrade continuity (`hc update`);
T3 slow consumer (confirm the 8 MiB block); T4 prim-host spike with re-exec keeping fds; T5 output-then-exit ordering.
