# Native Windows Rust fixture follow-up (W10, W13)

2026-11-23. Input: the native Windows installation report, W10/W13. These are fixture corrections and a
source investigation, **not a passing native Windows suite or a backend fix**. Store replacement (W03) and
concurrent extension-log rotation (W04) are separate product investigations; they must not be skipped as fixtures.

## W10: the nine reported library failures

| Test | Cause/action | Verification still needed on Windows |
|---|---|---|
| `commit::tests::shutdown_deadline_rejects_queued_work_but_never_claims_active_write_failed` | Use the platform tempfile root instead of a rooted `/tmp` path. Assert that the root respects the platform temp directory. The writer/shutdown assertions are unchanged. | Run the existing test; do not relax its fence/durability checks. |
| `ext::qa2_tests::cooperative_provider_gets_time_for_its_one_second_cleanup` | Platform tempfile root; literal `python3` goes through the existing interpreter resolver instead of requiring a Unix executable path. Cleanup semantics remain unchanged. | Run with the actual Python discovery configuration; verify the child is reaped. |
| `server::actual_system_authors_are_never_default_undo_targets` | Shared test endpoint helper: Unix socket on Unix, unique local named pipe on Windows. No undo attribution logic changed. | Run with the real named-pipe transport. |
| `tui::tests::owner_unset_hello_reaches_the_real_snapshot_and_renderer` | Same shared native endpoint helper. Owner-unset assertions unchanged. | Named-pipe hello/snapshot plus renderer assertions. |
| `tui::tests::reconnect_draw_reattaches_terminal_and_repaints_backend_output` | Platform tempfile root and shared native endpoint; explicitly shut down its private fake core after the assertions. | Named-pipe reconnect/attach/repaint, then cleanup. |
| `ui_launch::tests::manifests_are_checked_listed_and_resolved_against_the_clients_directory` | Build the expected script path with separate `clients` and `text.py` components, matching production resolution rather than comparing mixed slash strings. | Run the existing manifest/command test; no optional client is launched. |
| `commit::tests::operational_owner_retires_at_the_existing_writer_dispatch_linearization` | **Read-only diagnosis below; unchanged.** | Confirm the exact panic site and fixture launch failure before altering the test. |
| `store::tests::windows_replace_with_open_delete_shared_reader` | **Not a fixture exemption.** W03 owner investigates the native replacement API/durability semantics. | Native regression remains required. |
| `ext_log::tests::one_generation_and_overlapping_writers_stay_within_two_fixed_files` | **Not a fixture exemption.** W04 owner investigates concurrent rotation. | Native overlapping-writer regression remains required. |

`transport::test_endpoint` is test-only and reuses the previous transport-test naming mechanism: PID plus random
nonce in the local pipe namespace; Unix remains a socket under the supplied private directory. Its powerless
Linux control constructs Windows names and validates them with the production Windows endpoint guard; its
Windows cfg assertion also checks the actual platform helper. Existing bind/connect tests still use that helper.
This does not emulate Windows I/O or prove native pipe behavior.

Focused tests run with a private, non-default `TMPDIR` on Linux. Restoring the three rooted tempdir calls makes
the three temp-root assertions fail. Replacing the Windows name builder with a filesystem name fails the local
namespace control. The original native W10 report supplies RED evidence for endpoint/path-separator failures;
Linux cannot reproduce Windows path-component formatting merely by cross-compiling. The named tests remain
native regression tests, not skipped or weakened on Windows.

### Operational-owner timeout: best source diagnosis, not a production timing conclusion

`src/commit.rs`, `operational_owner_retires_at_the_existing_writer_dispatch_linearization`, stores an extension argv
whose executable is a hard-coded Unix Python path. The actual fixture (`tests/fixtures/managed_identity.py`)
writes the supervisor token to `owner.token`, then waits on stdin. The test polls that token for five seconds
before it reaches its writer-dispatch barriers. `ext::reconcile` resolves only the portable literal `python3`
through Python discovery; an absolute Unix executable path is passed through literally. On native Windows the
fixture therefore normally cannot start, so the token poll is the first plausible timeout. This is stronger
evidence of a missing fixture prerequisite than of a commit linearization defect.

Proposal (not implemented in this read-only item): use literal `python3` for this fixture too; retain the
five-second bound and both dispatched/queued assertions, and include supervisor failure details if the token is
not produced. The report does not give the panic line, so do not claim this explains a later two-second writer
barrier timeout. If the token is present and a writer barrier times out, investigate that separately. Do not fix
this by increasing timeouts, skipping Windows, changing owner retirement, or relaxing write-through durability.

## W13: forward-slash cmd.exe launch

### What vqro actually does

- `src/server.rs::spawn` takes the supplied argv vector and hands it to the provider as `SpawnRequest`.
- `src/bin/vqro-herdr.rs::spawn` validates/copies strings; only an **empty** argv gets a Unix default shell.
  Explicit `C:/Windows/System32/cmd.exe` is not split, quoted, or normalized there.
- `src/adapter/herdr.rs`, `AdapterCmd::Spawn`, puts that vector directly in JSON `argv` for `terminal.run`.
  Its `Command::new` starts the **hc stdio connector**, not the requested terminal executable. The terminal
  executable does not pass through vqro's `std::process::Command` argument serializer.

There is no supported evidence of a vqro quoting defect in that path, so this change does **not** rewrite program
paths or argv. Such rewriting could also corrupt a backend-host path on another operating system.

### Best explanation to test, not a proven native root cause

Microsoft documents cmd's `/c` (execute and exit), `/k` (execute and stay), `/s` quote handling, and `/d`
(disable AutoRun); see [cmd](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/cmd).
The forward-slash executable spelling contains **`/cmd.exe`**, whose first two characters are `/c`.
A command interpreter that scans its raw command line for switches without excluding the executable token can
mistake that suffix for `/c` and try to run `md.exe`, then exit. That is consistent with the reported quick exit
code 1 and the backslash spelling remaining interactive. The ReactOS compatibility implementation's
[`Initialize`](https://github.com/reactos/reactos/blob/master/base/shell/cmd/cmd.c) demonstrates exactly that
raw-command-line scan; it is **not Microsoft's Windows implementation**, so this remains a hypothesis until
native output/raw command-line evidence confirms it.

Windows process creation has an executable name and a separate raw command-line string, and a child interprets
that string itself ([CreateProcessW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessw)).
Rust also warns that cmd.exe uses nonstandard argument decoding
([Command::arg, Windows](https://doc.rust-lang.org/std/process/struct.Command.html#method.arg)). Neither reference
proves how the separately installed herdr-core 0.1.22 constructs its terminal command line. That backend's
version-matched Windows process-creation source was not available in this checkout. The separate Herdr application
is not substituted as evidence for the core binary's implementation.

### Native follow-up

Keep the working backslash spelling for explicit cmd launches; the backend's default PowerShell also worked in
the report. On a private backend only, compare forward/backslash argv with `/d` (to exclude AutoRun), capture the
terminal output before forget, and inspect whether it reports `md.exe` as an invalid command. Compare direct
process creation with the same program/argv against `terminal.run`; retain each raw command line and exit code.
If the error reproduces directly, it is cmd parsing/launcher compatibility rather than vqro transport quoting;
if only the backend fails, route the raw command-line evidence to the backend owner. Quoting argv0 is not an
assumed fix: a parser that scans inside quotes may still see `/c`. No live backend or native cmd process was
launched for this source investigation.
