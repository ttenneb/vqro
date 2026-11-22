# Agents extension UI (2026-11-17)

## Delivery

`extensions/agents.py` is stdlib Python over prim_ext and public workspace/resource/command primitives. No Rust changes or agent process hosting in core/extensions. Bundled defaults enable it; Settings → Extensions links its runtime configuration popup. User guide: `extensions/agents.md`.

New Pi agent (tabbar/prefix a) uses bridge:true and literal configured Pi argv, waits for the hosted agent and bridge-owned view, then invokes tabs.show. It never retries a spawn if the outcome/tab/registration is uncertain. Default runtime is pi1 --mode rpc. Multiple runtime entries produce a picker; invalid settings fail before execution. The bridge Python module comes from the installed extension directory.

Agent pane actions are prompt/steer/follow_up/abort/clear_queue/answer. Prompt-like actions use existing input. The pending-request popup covers confirm/select/input/editor and explicit cancellation, validates the latest request id/type, and reports dispatched answers. Editor uses the existing one-line input; no new editor/widget claim. Oversized popup details remain bounded with cancellation still available. Missing/incomplete facts are never called empty/answered.

Menus are declared as a complete activation set, including restored agents, for the current prim_ext reconciliation contract. Resource-status updates do not persist transactions. Pane templates deliberately do not pin a resource id: the palette reuses those templates and must resolve the focused agent, not the first agent's menu. Removed menu entries are owner-checked and compare-and-set; only workspace transactions retry, never native effects.

Bridge content now uses semantic status badges and observed active/settled/aborted fields, queue counts/previews, request details and transcript. The view stays <=32KiB, preserves fixed status/request summaries, trims old transcript rows and visibly reports clipping. No client/core Rust changes or custom colours.

## Verification

- `scripts/check.sh quick`: PASS build-zero-warnings, cargo-test, Python unittest and no-leftover-processes. 90 Python tests (15 agents-specific) and 154 Rust tests pass; zero warnings,17 opt-in Rust tests ignored.
- Private real prim+herdr-core extension-command E2E passes all six actions, native dispositions/queues, exact rejected/indeterminate effect audits, native crash ->gone/exit23 and core SIGKILL/restart with same agent/host/native processes/current run and no replay.
- Repeated physical TUI E2Es pass. Prefix a actually starts the fake agent/tab, the pane Prompt input actually sends a fake prompt, pane Abort settles, queue/status/transcript render, pending-request popup → Yes/No → Yes actually dispatches the native UI answer. Public effect audits verify observed spawn and dispatched answer, not guessed UI success.
- The latest run stages Python helpers and pi_bridge as an installed-directory layout in the private extension root. No source-path import shortcut. All model prompts are fake fixtures, never real Pi/provider calls.
- UI is closed before the harness's manual core restart to avoid racing the TUI's auto-start. The command/bridge restart path is tested, not an additional TUI-reconnect claim.
- Input transport probe now expects declared `{stream:true,effect:false}` and exact unsupported/rejected before dispatch. No hidden terminal.send/keys/attach compensation. Existing stream EOF remains dispatched.

Final PTY/package-layout receipt: `<private-notes>`.
Earlier PTY repeat: `ep-peiwf01u/receipt.json`; extension-command runs: `ep-ojr2t47j`, `ep-50ek8em3`.
Captures in each PTY root: `ui-new-binding.txt`, `ui-agent-ready.txt`, `ui-prompt-input.txt`, `ui-working.txt`, `ui-queues.txt`, `ui-requests.txt`, `ui-confirm-options.txt`, `ui-transcript.txt`.
Quick gate receipt: `<private-notes>`.

All services/terminals/config/state/runtime/socket roots are private, environment-allowlisted, network-isolated, and cleaned with PID/birth checks. Tmux uses a private socket and /dev/null config. No installed/live services, credentials or real sessions changed. Scratch evidence retained.

Full release gate now includes the fake Pi provider and agents UI opt-in flows. Its existing TUI fixture also copies pi_bridge and ignores real tmux config, so newly bundled agents can start there. Full gate/install remains the coordinator's release step; it was not run or claimed here. Windows, alternate backends, real provider-backed turns and a multiline editor UI are not validated.
