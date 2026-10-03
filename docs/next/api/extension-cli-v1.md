# Local extension CLI contributions v1

Available: manifest-v1 executable extensions on Linux/macOS. Windows foreground
command replacement is **not supported yet**; moved commands fail clearly, with
no caller-classification workaround. Remote execution is not provided here.

An enabled local extension may contribute aliases without adding a core product
command or API method:

```toml
[[cli_commands]]
name = "example"
description = "Describe this extension's records"
command = ["python3", "./cli.py"]
platforms = ["linux", "macos"]
```

`vqro example ARG ...` consults the existing local plugin registry and validates
the registered exact manifest identity/root. Built-in commands win by ordinary
core dispatch; two enabled valid contributions claiming the remaining alias
fail closed. Disabled, unavailable, invalid or unsupported contributions do not
execute. An unknown alias is a CLI error, not a request to launch the TUI.
Root help stays static; an extension owns its detailed `--help` and syntax.

Declarations are internal manifest data, not fields appended to frozen
`InstalledPluginInfo` or client codecs. No new server method or command registry
is introduced. Names start with a lowercase ASCII letter and contain only
lowercase letters, digits, `-` or `_`, at most64bytes. There are at most64commands,
128prefix argv tokens,4096bytes/token and64KiB prefix bytes. Descriptions are at
most256bytes without controls; empty/unknown platform lists are rejected.

Only **static declaration tokens** beginning `./` resolve relative to the
extension root, without parent/root escapes. Bare programs use PATH. User
arguments are forwarded literally with unchanged caller cwd, stdin, stdout,
stderr and exit status; they undergo no shell expansion. The host supplies
neutral extension-root, binary and selected socket context, never caller/UID/
execution overrides. Foreground process replacement retains the invoking CLI
PID and direct-shell-child or Pi ancestry. All API writes still receive fresh
host-attributed callers and existing service admission/capability checks.

This uses the existing cooperative same-user local executable-extension trust,
**not WASM sandboxing**. Installation/discovery grants no API authority. A
contribution runs only on an explicit CLI invocation; unrelated built-in session
and PTY recovery remains available when a contribution fails.

## Tasks consumer

Tasks supplies `task` and `message`; their grammar, help, decision syntax,
formatting, name-to-execution lookup and uncertainty recovery live in
`extensions/tasks/cli.py`. The provider remains authoritative for policy.
Output is readable on a TTY, JSON when redirected/piped; `--json` forces JSON.
Machine consumers should request it explicitly. The unchanged first-ten-minutes
guide's redirected plan/decision files remain JSON.

For uncertain writes preserve the original caller, operation ID, payload and
revision; inspect `task op ... --caller ...` first. A missing receipt is not
proof of noncommit, and a historical receipt is not current state. The command
never automatically retries. Caller fields are preconditions/selectors, not
identity overrides or authority.
