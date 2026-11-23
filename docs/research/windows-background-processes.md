# W01 consolidated Rust background launch audit (2026-11-23)

The foundation is Ttenneb's native-tested `c38d8ac7`, retained before follow-ups: one Tokio-returning
`program::background_command`, supervised extensions and herdr stdio connector, plus the original native test and
private detached-core smoke. Its reported native debug/release/desktop results do not automatically qualify the
wider consolidated candidate. Historical observations and remaining failures are in `windows-native-install.md`.

The follow-up covers nine additional Rust constructors and adds powerless flag/wiring controls plus a second
native all-stdio test. There is one public helper and one no-window policy, not competing std/Tokio helpers.
Synchronous callers use `into_std()`; Tokio 1.54.1 `src/process/mod.rs:338` returns the original std command, retaining
creation flags. These callers have no Tokio-specific lifetime options to preserve. Existing async callers retain
their stdio/environment/argv/cwd and explicit kill-on-drop settings.

## Every Rust command constructor

| Site | Verdict |
|---|---|
| `program::background_command_with` | Central Tokio constructor; Windows-only CREATE_NO_WINDOW via real/fake setter. |
| `ext::reconcile` | Human foundation: redirected supervised extension; helper for every generation. |
| `adapter::herdr::session` | Human foundation: framed stdio connector; helper. |
| `tmux::Client::connect` | Follow-up: piped control-mode client; helper. |
| `tmux::Client::capture_paused` | Follow-up: captured CLI query; helper. |
| `vqro-tmux::tmux_run` | Follow-up: backend CLI setup/query/server commands; helper + into_std. Native Windows unsupported guard remains. |
| `vqro-tmux` version probe | Follow-up: captured tmux -V; helper + into_std. |
| `vqro-rex::server::rex` | Follow-up: central Rex server/API CLI builder; helper + into_std. Rex still owns its service/terminals. |
| `vqro-bridge-codex` child | Follow-up: headless app-server over pipes, not interactive Codex UI; helper. |
| `vqro-bridge-pi::Native::spawn` | Follow-up: controlled RPC child; helper. |
| `vqro-bridge-pi::startup_version` | Follow-up: bounded captured version probe; helper. |
| `tui::ghostty` snapshot test child | Follow-up: isolated captured decoder child; helper + into_std. |
| `client::ensure_core` | Keep explicit DETACHED_PROCESS + CREATE_NEW_PROCESS_GROUP; long-lived core must not share UI console/group. |
| `ui_launch::command` | Interactive UI inherits terminal/stdio; unchanged. |
| `tui::run` replacement | Interactive replacement after console-reader handoff; unchanged. |
| `main::stop_pidfile` | Unix-only legacy kill fallback; unchanged. |
| `vqro-rex::status_tests::Fake::new` | Unix-only shell fixture writer; unchanged. |
| `vqro-rex::status_tests` stale endpoint child | Unix-only Python socket fixture; unchanged. |
| `vqro-bridge-pi::tests` env fixture | Command named not-executed is inspected but never spawned; unchanged. |

[Microsoft flags](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags) define
CREATE_NO_WINDOW=0x08000000. Redirection alone is insufficient. It is ignored with DETACHED_PROCESS/CREATE_NEW_CONSOLE
or for GUI applications, so the detached core does not redundantly combine it. Children/grandchildren must request
it independently: Python shares `vqro_process.background_kwargs`, and backends/runtimes own further descendants.
No workspace-terminal argv or backend spawn contract, restart budget, replay or process lifetime policy is changed.

## Controls and limits

- Preserve human native `program::tests::background_child_has_no_console_and_retains_stdio` unchanged.
- Add `background_child_round_trips_all_stdio_without_console`: bounded native PowerShell test through into_std,
  requiring zero GetConsoleWindow and working stdin/stdout/stderr. [API meaning](https://learn.microsoft.com/en-us/windows/console/getconsolewindow).
- Linux fake setter tests exact Windows flag/no Unix setter; wiring controls cover eleven helper calls plus explicit
  exceptions. Compile-time Windows SDK constant assertion guards the numeric value.
- Preserve the authored `tests/windows_background_smoke.py` foundation: two private declarations, detached core,
  extension/helper/native connector, restart and log draining; no real backend/agent/terminal. A Lead-approved safety
  follow-up changes failure cleanup only: no late PID report lookup or fixture termination access; captured handles
  are wait/close only, and only the Popen-owned core may be killed. Unobserved reports/timeouts fail visibly, and a
  passing result is emitted only after cleanup succeeds. Four powerless Windows-API/source controls cover this;
  reintroducing fixture TerminateProcess fails them. Positive restart/stdio assertions stay intact. Run it natively
  after building the consolidated candidate, not as a Linux/Wine desktop certificate.

The final candidate and extra runtime boundaries still need native Windows reruns and actual desktop/descendant
inspection. Human predecessor results are valuable evidence, not a passing full Windows gate or an install approval.
