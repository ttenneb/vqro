# Windows Python test triage: W11 / W14

2026-11-23, source base `47088308`. Input: **windows-native-install.md**, W11 and W14 (native CPython 3.14.2:
1604 tests, 44 failures, 130 errors, 8 skips). The individual native tracebacks were not available for this
source pass. This is a classification of identifiable causes in the current tree, **not a claim to account for
all 174 original failures/errors**, nor a native rerun. Current discovery has more tests than that older report.

## Boundaries

- No Windows host here. Linux regressions and fake-platform controls are development evidence only.
- Only genuinely Unix fixture paths are skipped on Windows. Pure data/oracle cases, socketpair provider tests,
  mocked Windows pipe authentication/overlapped tests, and Windows unsupported-operation controls remain enabled.
- Unsupported product functionality is named separately below; a fixture skip is never evidence of native support.
- Windows symlink fixtures skip only actual missing-privilege error 1314, not arbitrary access/rename failures.
- No Rust changes, tools installed, native API calls simulated as certification, live backend activity or full gate.

## Fixture classification and actions

Paths are relative to `tests/`; names identify the source tests, not inferred entries in the unavailable native log.

| Test / suite | Cause found in source | Action and remaining Windows coverage |
| --- | --- | --- |
| `test_check.py` | Linux Bash gate, unconditional fcntl import, executable modes/flock | Module SkipTest before fcntl import on win32; native gate is separate `check.ps1` work. |
| `test_check_lifetime.py` | cgroups, pidfds, /proc, fork/session groups and Unix signals, including module-time signal constants | Module SkipTest before loading Linux lifetime implementation on win32. These are not Windows lifetime passes. |
| `test_windows_check.py` | Tests the Linux-to-Windows cross-check Bash function with POSIX executable/symlink stubs | Module SkipTest on win32; it is not the native Windows gate. |
| `test_scale_artifact.py::{ScaleArtifactTests,PrebuildRecipeTests}` | Linux timing-gate executable-bit, shebang and Bash recipe fixtures | Class skips on win32. |
| `test_coord_harness.py::{StartupFailureTests,SilentPeerTests}` | /proc process identity, executable shebang helpers, real AF_UNIX controller peers | Class skips on win32; no attempt to emulate that lifetime mechanism. |
| `test_agent_conformance.py::{PeerTests,NativeFixtureTests}` | asyncio Unix servers/connections for the conformance controller | Class skips on win32; other oracle/content/declaration classes remain enabled. |
| `test_agent_conformance_content_origin.py::CallbackWireChecks.test_exact_public_proof_forwarding_and_no_ambient_authority` | Actual asyncio Unix server for callback proof forwarding | Method skip on win32; other content-origin checks remain enabled. |
| `test_coord_conformance.py::NativeOracle.test_control_error_reply_keeps_the_request_id` | Actual asyncio Unix server for a control-error roundtrip | Method skip on win32; other native-oracle and checker tests stay enabled. |
| `test_host_context.py::HostHandshake.test_provider_opt_in_is_explicit_on_its_registration_only` | Actual asyncio Unix server; hard-coded /tmp fixture root | Method skip for Unix transport fixture; use tempfile's default root. Pure host-context helper tests remain enabled. |
| `test_vqro_ext.py::ListenerConflictsOverASocket` | Child extension connects to a scripted AF_UNIX core | Class skip on win32; tempfile default root replaces /tmp. In-memory/thread/cache unit tests remain enabled. |
| `test_vqro_ext_slow_core.py::SlowCore` | Scripted AF_UNIX server | Class skip on win32; tempfile default root replaces /tmp. |
| `test_pi_provider.py::BridgeTests.test_cli_explicit_argv_and_managed_stdin_eof_are_clean`, `test_cli_native_exit_with_open_stdin_is_clean` | CLI lifetime helper specifically creates a Unix-domain core server | Only these two methods skipped on win32; the other socketpair/subprocess bridge controls remain enabled. |
| `test_agent_conformance_codex_l3.py::WholeHandlerControls` | Replacement protocol explicitly freezes/resumes processes with SIGSTOP/SIGCONT constants | Class skip on win32; pure replacement grading/helper tests remain enabled. |
| `test_pi.py::PiTests.test_default_bridge_never_imports_project_or_pythonpath_shadow_modules` | Fake bridge binary is an executable POSIX shell script | Method skip on win32; literal argv/policy tests remain enabled. |
| `test_pi_launcher.py::PiLauncherTests.test_exec_preserves_literal_argv_and_never_imports_project_code` | Fake Rust bridge is a shebang executable, not a native exe | Method skip on win32; explicit missing-binary/legacy-entry tests remain enabled. |
| `test_vqro_transport.py::{UnixTransportTests,AsyncTransportTests.test_unix_calls_existing_asyncio_api}` | These are mocked Unix branches, but Windows lacks AF_UNIX/open_unix_connection names | Supply a fake AF_UNIX constant and create the mocked asyncio attribute. Keep them enabled; no native socket API is called. |
| `test_tmux_server.py` | Positive helper-policy controls encounter the real Windows unavailable guard; one fake killpg attribute was absent | Mock supported platform in fixture setup and create the fake killpg attribute. Explicit Windows command/poll no-effect tests still override win32 and remain enabled. Native POSIX-group test already has a capability skip. |
| `test_links.py` | Executable /bin/sh opener and shell quoting of the recording filename | Replace with a Python script launched by sys.executable; explicit UTF-8 record file. Real child process still runs. Added non-ASCII URL roundtrip. |
| `test_tier1.py::Tier1Tests` | Executable shell hook fixture and POSIX mode assertions | Use an instance-local Python-script hook adapter through the real UTF-8 subprocess helper. Retain launch/routing/settings assertions on all platforms; assert mode bits only on Unix. Instance-local binding avoids leaking a class patch into suites reusing this fixture manually. |
| `test_vqro_provider.py::CancelledWriteTests.test_close_aborts_bytes_left_below_the_high_water_mark` | Assumed a 30 KB record must leave a nonempty transport buffer; Windows kernel may accept all bytes | Deterministic fake writer/transport with 30 KB residue and no pending flush. Assert close aborts the residue. No Windows skip, enlarged sleeps, or weakened cleanup assertion. Other actual socketpair cancellation tests remain enabled. |
| `test_coord_native_framing.py::NativeFrames` | Symlink escape target hard-coded to a Unix Python path; symlink creation may lack Windows privilege | Separate portable dependency-byte digest test from symlink negative. Link to sys.executable; only actual WinError 1314 skips the symlink case. |
| `test_install_extension_tests.py` | Symlink fixtures may lack Windows privilege; captured offline CLI output can be UTF-8 | Skip only actual 1314 at symlink creation, with a control proving unrelated permission errors still fail. Offline CLI capture explicitly decodes UTF-8 and requests UTF-8 child diagnostics. Copy/publication/rollback tests are not broadly skipped. |
| `test_coordination_archive.py` positive export/verify/free methods | Product deliberately refuses durable archive export on Windows | Explicit skips for four Unix exporter paths; the Windows no-filesystem-effect refusal test remains enabled. This is an existing product gap, not a newly certified exporter. |
| `test_coordination_archive_recovery.py` four positive export scenarios | Depend on the Unix exporter/directory publication barrier | Explicit positive-path skips; the export-independent open-handover refusal remains enabled. |
| `test_testing_integration.py::{SandboxTests,IntegrationModesTests}` | Positive scenarios depend on a sandbox with Unix endpoint containment/select/process-lifetime assumptions | Product unsupported decision below; positive sandbox scenarios explicitly skipped on win32. SourceMark/Scenario pure controls remain enabled, and new constructor no-effect negatives remain enabled. |
| `test_testing_unit.py::{HealthChecks,PlainClientTests}` | Fixed product issues below; AF_UNIX server fixture for read-client framing | Use mocked shared connector and portable socketpair fixture, preserving notification/EOF/deadline/close assertions. Keep tests enabled; mode-bit assertions are Unix-only. |
| `test_testing_perf.py::CollectorTests.test_settings_override_receipt_and_busy_machine_warnings_do_not_fail` | POSIX receipt mode assertion on a Windows file | Keep receipt/content/verdict checks enabled; assert owner-only mode only on Unix. No Windows ACL certification. |

### Explicit UTF-8 / native tool paths (W14)

| Source / tests | Cause | Action |
| --- | --- | --- |
| `agent_conformance/inventory.py::main_contract` | Source-export fallback decoded normative Markdown using locale; Windows default tool PATH may not contain Git | Explicit UTF-8 fallback. On Windows preserve the caller's native tool PATH in the otherwise minimal Git environment; Unix tool PATH behaviour unchanged. |
| `agent_conformance/evidence.py::git`, `test_agent_conformance_evidence.py::run_git` | os.defpath is not the installed Git PATH on Windows | Same explicit native Windows PATH handling, with no inherited Herdr/config/credential variables. Fake command tests assert the complete environment. Missing Git still reports a harness gap, not conformance success. |
| `pi_provider_e2e.py::E2E.command` and recorded CLI capture | text=True implicitly decoded UTF-8 snapshots via locale | Explicit encoding='utf-8'; fake capture test asserts it. The enclosing tmux/procfs E2E harness remains Unix-only, not newly Windows-supported. |
| `test_accounts_secondary.py`, `test_account_capacity_pipeline.py`, `test_causal_vectors.py`, `test_causality.py`, `test_key_labels.py`, `test_vqro_items.py` | JSON/vector fixture reads used locale | Explicit UTF-8 reads. No platform skips. |
| `test_agents.py`, `test_settings.py`, `test_same_setup_stop.py`, `test_tui_keys_lifetime.py` | Reading shipped source/manifests through locale | Explicit UTF-8 source/manifest reads. Pure data/source tests stay enabled. |
| `test_testing_unit.py::install_test` | Generated Python test source must be UTF-8, not a locale-encoded script | Explicit UTF-8 fixture writes. Added actual non-ASCII diagnostic roundtrip through the copied-suite runner. |

## Product findings (not disguised as fixture-only failures)

The Windows lead explicitly approved these corrections/restrictions during triage:

| Finding | Correction / boundary | Focused control |
| --- | --- | --- |
| `extensions/vqro_testing.py::PlainClient` opened AF_UNIX directly | Connect through existing `vqro_transport.connect_endpoint(path, timeout=...)`; no SDK transport/authentication API changed. Stream creation failure closes the connected transport, connect failure propagates without fallback. | `PlainClientTests`; opaque pipe pass-through in `test_python_windows_fixtures.py`. |
| `extensions/testing_unit.py::tests_row` ran literal python3; diagnostics were decoded as UTF-8 without ensuring the child emitted UTF-8 | Use sys.executable (the running interpreter) and PYTHONIOENCODING=utf-8 in the explicit sanitized child environment. | Current-interpreter/environment assertion and actual café/λ diagnostic roundtrip; live-environment scrub control retained. |
| Fake sandbox backend command assumed /bin/false | Portable literal argv `[sys.executable, '-c', 'raise SystemExit(1)']`. | Inspect exact argv in both sandbox fixture and powerless control. |
| `write_receipt` used os.fchmod (absent on Windows) and locale text mode | Explicit UTF-8; fchmod only on Unix. Windows files inherit their directory ACLs; no owner-only Windows claim. | Fake win32 receipt writes exact UTF-8 bytes and never calls fchmod; Unix retention/mode tests retained. |
| Integration/performance Sandbox and SandboxClient depend on Unix endpoint paths, select-on-socket and process-group isolation/lifetime | **Not supported on native Windows yet.** Both constructors refuse clearly before filesystem/process effects. Manifests and Windows guide disclose the gap. A real endpoint/isolation/lifetime port is a separate slice, not a partial transport substitution. | Fake win32 constructors forbid Path and Popen access. |

### Remaining risks / native follow-up

- The unit-health diagnostic's standalone no-argument endpoint/state/extensions defaults are still Unix-shaped.
  On Windows select `--socket`, `--state-dir`, `--extensions-dir` explicitly (or use the configured in-app launch).
- Copy/publication/rollback, executable discovery, symlink-capability negatives and actual provider cancellation
  tests remain meaningful native rerun targets. Do not label further native failures fixture-only without traces.
- Conformance's real Unix controller/PTY launchers are not native Windows qualification. Their import no longer
  requires fcntl/pty/termios on Windows; using OwnedPty there raises an explicit HarnessGap. Other conformance
  oracles can still run, but no test skip or fake model substitutes for native ConPTY/controller coverage.
- Existing tests guarded by Linux gate-slot/systemd capabilities remain such; no native passing count is inferred.
- Windows API/console suppression, persistence/open-reader and log-rotation issues are other slices; this work
  neither skips nor fixes those Rust/runtime defects.

## Verification

- Linux Python discovery: **1778 tests, OK, 4 skips** (`python-tests-final.log`). The first run exposed a new
  class-level fixture patch leaking through manual fixture reuse; changed it to an instance-local adapter, then
  reran the complete Python suite successfully. No pre-existing/new failure was waived.
- After the final SandboxClient early-refusal addition: 8 new diagnostic/fixture controls, 104 diagnostic
  unit/integration/performance tests, 37 transport controls and 33 provider controls passed.
- `scripts/check_manifest_deps.py`: all 24 manifests pass. `compileall` and `git diff --check` pass.
- No Rust files changed, so no Windows GNU rebuild for this slice. No full gate was run; the parent owns the
  integrated gate. Native Windows rerun and the individual original traceback classification remain required.
