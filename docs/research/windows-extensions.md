# Native Windows: bundled extensions audit (2026-11-22)

Static audit of `extensions/` at Windows stream base `a982139e`, plus the small fixes recorded below.
No Windows machine was available. **Fine means no extension-local Unix dependency found, not a Windows runtime pass.**
All Python extensions additionally depend on interpreter selection (`defaults.json`) and SDK endpoint support
(slice C, win-pyext). Rust provider/bridge execution is audited by the Windows lead and transport helpers.

## Inventory and verdicts

Paths below are relative to `extensions/`. A manifest is paired with its implementation; all 20 manifests were
read. Manifests contain descriptive/capability JSON, not shell commands or filesystem operations.

| Extension / manifest | Evidence and Unix assumptions | Verdict |
| --- | --- | --- |
| `agents.py` / `manifests/agents.json` | `agents.py:1902` SDK registration; agent operations use declared public capabilities, not local signals. Transcript helpers have slash-only presentation below. | Fine for control logic; transcript presentation needs Windows run |
| `codex.py` / `manifests/codex.json` | `codex.py:71-72` optional literal `CODEX_HOME` / `HOME` forwarded to runtime; no credentials opened. Backend Rust bridge owns launch. A configured HOME is a runtime setting, not a Windows home-directory discovery mechanism. | Needs Windows run (Codex runtime/profile handling) |
| `coordination.py` / `manifests/coord.json` | `coordination.py:278` thread-pool dispatch; protected persistence is core RPC. Archive export is the local filesystem exception below. | Fine except archive export: fix or declare unsupported |
| `gestures.py` / `manifests/gestures.json` | `gestures.py:15` SDK plus binding data; no local execution. | Fine |
| `icons.py` / `manifests/icons.json` | `icons.py:24` SDK plus glyph data; terminal font/width is client-owned. | Fine; Windows font rendering needs run |
| `keys.py` / `manifests/keys.json` | `keys.py:23` SDK plus key data; tmux/screen key profiles do not invoke those programs. | Fine |
| `links.py` / `manifests/links.json` | `links.py:42` already chooses Windows `cmd /c start`; `:51` parses EDITOR/VISUAL with POSIX shlex; `:52` defaults to `vi`; `:59` passes `start_new_session=True` (POSIX-only effect). Paths use os.path. | Needs Windows run; configure explicit editor/opener argv for now |
| `logs.py` / `manifests/logs.json` | `logs.py:43` os.path.join; `:65` binary reads, decoded with replacement; readers close after bounded tail read, not held for entire follow session. | Fine; Windows rotation sharing needs run |
| `panes.py` / `manifests/panes.json` | `panes.py:18` SDK; spawn/kill go to backend, no shell executable selected locally. | Fine |
| `pi.py` / `manifests/pi.json` | `pi.py:23` hard-coded python3 isolated bridge launcher; literal argv, no shell interpolation. | Interpreter decision + native bridge run needed |
| `retention.py` / `manifests/retention.json` | `retention.py:39` SDK; resource.forget capability, not local process cleanup. | Fine |
| `settings.py` / `manifests/settings.json` | `settings.py:29` SDK; `:527` endpoint shown as opaque diagnostic string. | Fine |
| `sidebar.py` / `manifests/sidebar.json` | `sidebar.py:155,513` slash-only trimming before native basename. Changed to include os.sep (Unix behaviour unchanged). Kill is backend capability, not os.kill. | Fixed local path labels; Windows run needed |
| `tabs.py` / `manifests/tabs.json` | `tabs.py:14` SDK; shell selection belongs to backend. | Fine |
| `themes.py` / `manifests/themes.json` | `themes.py:22` SDK plus palette data. | Fine |
| `tier1.py`, `tier1_run.py` / `manifests/tier1.json` | `tier1.py:43` state default formerly XDG only; `:70` /proc parent-executable probe already catches OSError and falls back to PATH; `:80` plugin text, `:110` hook JSON and `:169` hook subprocess now explicitly UTF-8. `tier1_run.py:24` AF_UNIX and `:27` text-mode wire remain slice C follow-up. | Low-risk path/encoding fixes done; wrapper transport and interpreter still need fix |
| `tmux_server.py` / `manifests/tmux-server.json` | `tmux_server.py:28` owned Unix socket; `:41-54` POSIX group kill is already guarded and falls back to Popen.kill. No native tmux to control. | Declare unsupported on native Windows; reject before helper/polling |
| `workspaces.py` / `manifests/workspaces.json` | `workspaces.py:41,360` Git text output uses locale default; `:94,102,340,357` slash-only trimming fixed to include native separator. `:105` /nonexistent is a comparison sentinel, not an accessed path. | Local separator fixes done; Git Unicode paths/encoding need Windows run |
| Rust herdr provider / `manifests/herdr.json` | `defaults.json:96` literal vqro-herdr executable; provider adapter owns hc discovery and Windows shell behaviour (lead slice E). | Needs integrated Windows run |
| Rust tmux provider / `manifests/tmux.json` | `defaults.json:109` vqro-tmux; off by default. No native Windows tmux. | Declare unsupported; Rust entry rejection owned by lead |

### Shared Python modules (including those without manifests)

| File | Evidence / verdict |
| --- | --- |
| `agent_launch.py` | `:1` shared launch policy through public primitives; no OS-specific process API. Fine (runtime launch still needs run). |
| `agent_request_routes.py` | `:1` route policy/data. Fine. |
| `agent_transcript.py` | `:429-430,458,706-707` slash-only shortening/basename/home presentation. Does not prevent transport or runtime operation, but Windows paths can display verbatim rather than shortened. Needs Windows presentation run; avoid reinterpreting remote-host paths with local os.path. |
| `coordination_archive.py` | `:26` bounded binary verify closes reader; `:51-65` exclusive file + fdopen(wb), flush/fsync; `:67-71` POSIX directory-open/fsync durability step. **Native Windows archive export explicitly unsupported**: `:37-39` rejects before any filesystem access. Lead approved this guard rather than weakening durability evidence needed for retirement. Unix unchanged. No replace-over-open-file here (exclusive creation, never replacement). |
| `coordination_engine.py` | `:2210,2246` bounded binary archive reads in with blocks; no persistent open destination. Otherwise pure state machine plus public RPC callbacks. Fine apart from archive export dependency. |
| `coordination_policy.py` | `:1` policy data/validation, no local OS interfaces. Fine. |
| `coordination_receipts.py` | `:72` slash splitting is JSON Pointer, not filesystem path. Fine. |
| `coordination_requests.py` | `:1` request data/validation. Fine. |
| `coordination_store.py` | `:43` immutable-row snapshots; writes through protected core operational RPC, not local files. Fine pending core Windows store work. |
| `coordination_streams.py` | `:1` stream data/reduction; no local OS API. Fine. |
| `vqro_display.py` | `:1` Unicode display/sanitizing, no OS API. Fine; fonts/width need Windows client run. |
| `vqro_items.py` | `:40` slash splitting is JSON Pointer. Fine. |
| `vqro_views.py` | `:17` argv basename uses split('/'), so Windows executable names can show full path. Nonfatal display limitation; needs Windows run. Resource paths may belong to remote hosts, so local os.path alone is not a correct general fix. |
| `vqro_ext.py` | `:32-44` managed stdin lifetime watcher uses thread + os.read (no select on pipe); `:96` AF_UNIX socket. Transport owned by slice C. Windows lifetime EOF needs native run. |
| `vqro_provider.py` | `:292` asyncio.open_unix_connection. Transport owned by slice C. |
| `vqro_provider_stream.py` | `:1` generic ordered stream transport helper, no Unix-only process API found. Slice C owns any changes. |

### `pi_bridge/`

| File | Evidence / verdict |
| --- | --- |
| `__init__.py` | Package marker. Fine. |
| `__main__.py` | `:19-42` thread reads binary stdin fd, calls loop thread-safely; no add_reader or signal handlers. Needs Windows stdin lifetime run. |
| `launch.py` | `:8` execvp('vqro-bridge-pi'); stdlib OS API exists on Windows, but Windows exec does not replace process like Unix. Executable resolution/terminal lifetime need native run. |
| `python_launch.py` | `:5` pathlib import root, legacy entry. Fine pending legacy transport. |
| `client.py` | `:74` asyncio subprocess_exec with binary pipes, `:430-435` Popen-style terminate/kill (no raw signal); on Windows terminate is forced termination, not graceful POSIX SIGTERM. Needs native subprocess/shutdown run; slice C owns file. |
| `control.py` | `:1` structured native RPC/control, no local Unix API. Fine. |
| `display.py` | `:136` daemon uses os.write (bytes), not text stdout lock. Windows console/lifetime behaviour needs run. |
| `program.py` | `:53-59` PYTHONPATH uses os.pathsep; `:495` asyncio lifecycle; no Unix signal registration. Fine pending transport/subprocess run. |

### `clients/`

- `text.py:63` uses shared Vqro SDK; `:757-764` stdin isatty/readline, no termios/pty. Unicode redirected
  stdout remains dependent on Python console/locale settings; native run required. `text.json:9` says python3.
- `vqro_client_model.py:857` slash-only argv basename (display only); model otherwise data-only. Slice C owns it.
- `opentui/src/rpc.ts:26-27` passes opaque endpoint via `Bun.connect({unix:path})`. Windows named-pipe support
  through that option is **unverified**, not assumed. Native OpenTUI dependencies/embedded terminal also need a run.
  Per `opentui/README.md:3-7`, this is an optional separately installed trial, not bundled by the installer.
  All `opentui/src/*.ts`, JSON metadata, package/tsconfig and lock were included in the path/API scan; the client
  model/style tests are data-only. No shell, /tmp, or signal assumption was found there.

## Interpreter inventory (decision 2; intentionally not changed)

Exact `rg -n '"python3"' src extensions` results, excluding this report:

- `extensions/defaults.json:7,14,21,28,35,42,49,56,64,71,76,83,89,119,127,136,145,154`.
- `extensions/pi.py:23` (backend-host bridge launch, **not** extension-supervisor argv).
- `extensions/tier1.py:118` (backend-host wrapper launch, **not** extension-supervisor argv).
- `extensions/clients/text.json:9` (UI launch path).
- `extensions/pi_bridge/PROGRAM.md:10` (documented legacy Python launcher).
- `src/ui_launch.rs:63,75` (test fixture/assertion, not runtime default).

Every executable Python file also has a `#!/usr/bin/env python3` shebang where present; a Windows argv launch
cannot rely on shebang handling. A core extension-supervisor-only fallback would not fix the backend-host Pi/tier1
or UI launcher call sites. Avoid substituting this machine's sys.executable into a remote backend-host argv.
Executable names lacking .exe are not automatically bugs: Python Windows lookup has native executable-extension
rules; verify vqro-bridge-pi, vqro-claude-hook, hc, pi, claude and codex on the real host (including .cmd launchers).

## Negative scan results / non-issues

Across shipped Python files: no os.fork, unguarded os.kill/os.killpg, setsid/preexec_fn, /bin/sh, SHELL fallback,
getuid, fcntl/termios/pty/resource/pwd/grp imports, select.select(pipe), loop.add_reader/add_signal_handler,
/tmp path, or os.replace caller was found. The only start_new_session sites are links and guarded tmux helper.
Workspace key strings (`doc/...`, `layout/...`) and JSON Pointer '/' are protocol identifiers, not path joining.
Python pathlib `/` is native path joining, not string slash concatenation.

Tests/fixtures were not treated as shipped extensions: e.g. `tests/test_tier1.py:19` intentionally creates a
/bin/sh hook, and private tmux/PTY tests require Unix. Their success on Linux is not Windows runtime evidence.

## Changes and verification

- Tier1 state now selects `%LOCALAPPDATA%/vqro/state` (HOME/AppData/Local fallback) on Windows, while
  `VQRO_STATE_DIR` still wins. Unix default and chmod behaviour stay unchanged; Windows does not pretend chmod
  0700 sets owner-only ACLs. Hook settings contain no credentials; inherited directory ACLs must be checked on Windows.
- Tier1 plugin source, settings JSON and hook subprocess output explicitly use UTF-8.
- Workspaces/sidebar trim native trailing separators without changing Unix behaviour.
- Focused test: `tests/test_extension_windows.py`; Linux tier1 regression: `tests/test_tier1.py`.
- Tmux Python extension stays connected for diagnostics; status/stop commands, direct helper calls and poll explicitly raise the native Windows unavailable error before filesystem inspection or subprocess creation. Both tmux manifests declare the limitation. Rust provider rejection is lead-owned.
- Archive export rejects native Windows before destination inspection or file creation; coord manifest declares the gap. Unix durable-export code is unchanged.
- Initial `scripts/check.sh quick`: Rust zero-warning build and cargo tests passed; Python ran 1408 tests with one unrelated failure, `test_agent_conformance_stream_bounds.ViewerPin.test_viewer_bound_to_trusted_revision_installs` (O14 viewer_revision not reachable from trusted harness main in this linked worktree). OpenTUI model and no-leftover-process checks passed.
- Windows-gnu all-targets check fails only in unintegrated `src/client.rs`, `src/server.rs` Unix transport and `tests/support/timing.rs` Unix clock calls. No Rust source changed in this slice.
- Slot-wrapped final quick rerun: zero-warning build passed; 1410 Python tests had the same one unrelated trusted-main pin failure. Cargo test stopped at four ext_lifetime tests (`private core did not start`); isolated serial `cargo test --test ext_lifetime -- --test-threads=1` then passed all four. No Rust source changed here; do not call the full gate green.
- Focused tests passed: 6 Windows-branch/path/UTF-8 tests, 12 tier1 tests, 36 sidebar tests, 16 tmux-server tests and 10 coordination archive/recovery tests. `python3 -m compileall -q extensions` and `git diff --check` passed.
- Evidence: `<private-notes>,quick-final.log,windows-check.log,ext-lifetime-rerun.log}` and gate receipts under `win-extaudit-target/`. All test processes ran without inherited HERDR variables; gates used private roots and the final run used the lead's gate-slot wrapper.

## API evidence and native follow-up

- CPython [`os.chmod`](https://docs.python.org/3/library/os.html#os.chmod): Windows chmod changes only read-only
  attribute; POSIX permission bits cannot establish an owner-only DACL.
- CPython [`os.open`](https://docs.python.org/3/library/os.html#os.open),
  [`os.fdopen`](https://docs.python.org/3/library/os.html#os.fdopen),
  [`os.fsync`](https://docs.python.org/3/library/os.html#os.fsync): binary/text mode must be explicit; Windows
  fsync delegates to CRT `_commit`, not a POSIX directory durability operation.
- CPython [`subprocess.Popen`](https://docs.python.org/3/library/subprocess.html#subprocess.Popen):
  start_new_session is POSIX-only; Windows terminate/kill use TerminateProcess. No Unix process-group guarantee claimed.
- CPython [`shlex`](https://docs.python.org/3/library/shlex.html): designed for Unix shells; explicit argv settings
  avoid treating a Windows EDITOR command string as POSIX quoting.
- Native acceptance must cover: non-ASCII installation/state/worktree paths, named-pipe extension connections,
  enable/disable/restart EOF, log tail + rotation, Pi/Claude/Codex launch/stop, link opening with metacharacters,
  optional OpenTUI, and explicit unsupported errors for tmux and any deferred archive operation.
