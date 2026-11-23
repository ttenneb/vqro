# W01 consolidated Python background child audit

Keep the native-tested human `vqro_process.background_kwargs()` as the sole implementation: a small, side-effect-free
stdlib process module, not a second helper in transport or an import of the SDK's managed-stdin lifetime watcher.
It returns CREATE_NO_WINDOW only on Windows and a fresh empty dict on Unix. Caller stdio, argv, cwd, environment,
sessions, deadlines and exit handling stay unchanged. No compatibility alias or duplicate transport helper remains.

## Thirteen background boundaries

| Module / function | Coverage |
|---|---|
| `workspaces.py:git` | Human: captured metadata query. |
| `workspaces.py:new_worktree` | Human: captured git operation. |
| `tier1.py:Tier1.run` | Human: captured declaration/settings queries, not interactive runtime. |
| `testing_unit.py:tests_row` | Human: private unit-test subprocess. |
| `testing_integration.py:extension_main.start` | Human: private integration harness. |
| `testing_perf.py:extension_main.start` | Human: private performance harness. |
| `vqro_testing.py:process_group_members` | Human: POSIX-only process facts query. |
| `vqro_testing.py:Sandbox.canary` | Human: private environment canary. |
| `vqro_testing.py:Sandbox.start` | Human: private test core with logs. |
| `vqro_testing.py:Sandbox.stop` | Human: private backend-stop CLI. |
| `accounts_keychain.py:_CommandStore._call` | Follow-up: captured CLI adapter; Windows currently uses its ctypes backend. |
| `tmux_server.py:TmuxServer.call` | Follow-up: captured helper; native Windows unavailable guard stays first. |
| `pi_bridge/client.py:PiRpcClient.start` | Follow-up: actual asyncio subprocess_exec RPC child, never the interactive Pi UI. |

Keep `links.py:launch` interactive. `tier1_run`, account launcher and Pi bridge launcher exec the backend-owned
terminal process and are unchanged; the separate Pi RPC child is covered above.

## Tests and native evidence boundary

Preserve the human `test_vqro_process.py` tests and detached-core Windows smoke. They include a portable real stdio
child and reported native console/stdio/restart/cleanup observations. Add broad `test_background_processes.py`
controls: complete source inventory, actual expanded kwargs through inert mocks, unchanged desktop opener, fresh
options, and real PiRpcClient.start with mocked asyncio spawn/reader tasks. Replace only the helper module's os
binding in these tests, avoiding global os.name changes that affect pathlib on Linux.

The existing tmux non-POSIX timeout fake now supplies CREATE_NO_WINDOW and asserts it: Linux subprocess lacks this
Windows constant, while a real NT interpreter has it. No product fallback or native-only test skip is substituted.

[Python flag](https://docs.python.org/3/library/subprocess.html#subprocess.CREATE_NO_WINDOW) and
[Microsoft semantics](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags): flags are
per launch, not inherited into all descendants, and do not hide arbitrary GUI applications. Historical native
results apply to the human predecessor. The additional Pi/runtime boundaries and consolidated candidate require
native Windows reruns and visible desktop/descendant/restart acceptance, not just these Linux mocks.
