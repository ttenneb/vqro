# API-key keychains and host launch helper

Slice 4 implementation boundary: `extensions/accounts_keychain.py` and
`extensions/accounts_keychain_helper.py`. Stdlib-only; secrets stay on the execution host.
No credential, keychain, authenticated endpoint or native profile was accessed during
implementation or testing. Tests inject subprocess, Win32 DLL, input and exec boundaries.
**macOS and Windows native interoperability is unverified.** Linux interoperability is
also not exercised against a real keychain; fake tests prove plumbing, not OS behavior.

## Contract / use

`store(Secret) -> opaque handle`, `retrieve(handle) -> Secret` (private Python only),
`delete(handle) -> deleted | not_found`, failures `KeychainError.status` with fixed safe
text. All adapters use random 32-hex handles and a versioned service namespace. No
subscription tokens or provider refresh ownership. Secrets are nonempty UTF-8 text,
at most 2048 bytes, without NUL/CR/LF; no trimming. Secret repr/str is redacted.

The helper is invoked via an explicitly configured execution-host Python and absolute
host helper path, not the coordinator's default Python or an inferred remote path:

- `python3 /host/path/accounts_keychain_helper.py store`: hidden terminal input only;
  stdout is JSON containing only the resulting handle. It refuses non-terminal input
  and getpass's echo-on fallback. Do not type secrets into agent messages or commands.
- `... delete --handle HANDLE`: an explicit irreversible effect. Failed/unconfirmed
  deletion retains the registry reference unless a human explicitly chooses to forget.
- `... launch --handle HANDLE --provider openai -- RUNTIME ARG ...`: retrieve on host,
  remove competing inherited OpenAI/Anthropic API/bearer/OAuth variables, add only
  OPENAI_API_KEY or ANTHROPIC_API_KEY to a fresh child env, exec once without a shell.
  The helper does not print a key or expose a retrieve CLI. It never retries a spawn.

Accounts registry/launch integration seam (slice 1 owner): `kind: api_key`, opaque
`secret_ref` handle, known `provider`, runtime selection and public `VQRO_ACCOUNT`
metadata. Wrap the final runtime argv with the helper after explicit account choice;
never put secret values in `resource.spawn.env`, workspace state, RPC arguments,
views or diagnostics. The registry must associate the handle with its execution host;
no transparent local-to-remote secret export. Core remains unaware of keychains.

This module does not establish that every runtime uses env before native login or
refrains from persisting it. The launcher must select a documented API-key runtime
mode and audit precedence; it must not report a runtime-authenticated account solely
because the helper supplied env. Profile files and login state are never inspected by
the helper. API-billed overflow remains explicit account policy, never a fallback.

No keychain/CLI/session available: show unavailable, offer OS setup or another
explicitly selected account. No package installation, plaintext store, ambient key
substitution, or fake in-memory fallback. Keychain operations are not transactions:
store before committing its handle, and report an orphan handle if that commit fails.
A timeout/interruption during mutation is indeterminate, not a rollback or permission
to replay it. Environment access by same-user tooling/admins, runtime children or crash
dumps is outside this protection. Python cannot guarantee complete zeroization.

## Linux / Windows evidence and limitations

[secret-tool manual](https://manpages.debian.org/testing/libsecret-tools/secret-tool.1.en.html):
store reads exact stdin until EOF; pipe lookup does not append a newline; lookup may
unlock; clear removes **unlocked** matching items; exit status documents only zero vs
nonzero. Consequently CLI failures are conservatively backend_error, never invented
locked/not_found. Successful clear returns indeterminate because locked entries could
remain; the registry must not falsely claim confirmed removal. OS keychain UI can
finish removal explicitly. No broad search or dump. Operations have a 30-second bound.

Windows uses fixed-width DWORD/BOOL and declared CredWriteW/CredReadW/CredDeleteW/
CredFree signatures, generic credentials and local-machine persistence under the
current user. Blob encoding is UTF-8 with explicit length, not Windows password
UTF-16 (generic blobs are application-defined). Returned memory is freed even on
invalid data. Temporary writable store buffer is cleared; immutable copies cannot be
guaranteed erased. Native missing, access-denied and no-logon-session errors map
conservatively. Sources: [CREDENTIALW](https://learn.microsoft.com/en-us/windows/win32/api/wincred/ns-wincred-credentialw),
[CredWriteW](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credwritew),
[CredReadW](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-credreadw),
[CredDeleteW](https://learn.microsoft.com/en-us/windows/win32/api/wincred/nf-wincred-creddeletew).

## macOS stdin parser / echo proof

Inspected Apple open source, revision **db15acbe6a7f257a859ad9a3bb86097bfe0679d9**,
`SecurityTool/macOS/{security.c,readline.c,keychain_add.c,keychain_find.c}` in
[apple-oss-distributions/Security](https://github.com/apple-oss-distributions/Security/tree/db15acbe6a7f257a859ad9a3bb86097bfe0679d9/SecurityTool/macOS).
No Apple source is copied into the implementation.

- `security.c` main reads stdin lines in `-i`, prompts only on a TTY, calls split_line,
  executes each once, returns last result. Command argument echo requires `do_verbose`;
  helper supplies only `-i -q`, never `-v`, a TTY, shell, or user-controlled CLI flags.
- `readline.c` reads with getchar into a 4096-byte buffer; no history/echo. A newline
  terminates a command. We provide exactly one complete newline-terminated line then
  EOF, below 4095 bytes. No extra command can overwrite a failure result.
- `split_line` interprets whitespace, quotes and backslashes. Instead of trying to
  escape arbitrary API keys, store a versioned ASCII base64 envelope as the `-w`
  value. The envelope alphabet contains none of that parser syntax. Service is fixed,
  handle is validated hex. The largest allowed key produces a line below 2900 bytes.
- `keychain_add.c` `keychain_add_generic_password` uses the provided -w string and
  length directly; no password prompt, `-A`, search-list modification or update mode.
  Safe diagnostics never leave our captured private stdout/stderr.
- `keychain_find.c` `do_password_item_printing` with -w prints printable bytes and
  appends one newline (nonprintable data would instead be hex). Base64 envelope is
  printable, so lookup can remove exactly that newline, validate the prefix and decode
  with strict base64. Unicode/control characters in the original key cannot trigger
  ambiguous hex output. Deletion matches service+handle, not general keychain content.

This establishes parser/echo safety for the inspected implementation; **not a native
macOS test nor proof about every shipped security binary**. CLI error codes/search
behavior cannot distinguish all locked/absent cases, so errors remain backend_error.
If native validation discovers contrary behavior, disable that adapter and use the
approved Security.framework ctypes route rather than leaking secret argv.

## Verification

`env -i HOME="$HOME" PATH="$PATH" python3 -m unittest discover -s tests -p
 'test_accounts_keychain*.py'` tests exact stdin, secret-free argv, redacted errors,
ambiguous CLI states, no fallback, timeout without replay, macOS parser alphabet and
line bound/envelope, Win32 ABI/blob/free/error boundaries, hidden entry and env-only
single launch. No test invokes a real keychain or starts an authenticated runtime.
