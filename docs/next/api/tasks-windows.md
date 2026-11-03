# Tasks on Windows

Status: experimental Windows implementation. Cross-compilation does not certify
native named-pipe identity, console interruption, SQLite/NTFS locking, or Pi
message/attention behavior; those require a native trial.

Tasks is a trusted local Python extension, not a sandbox. Installation does not
grant API or package-to-package authority. Generic terminal recovery remains
available if Tasks cannot start.

## Interpreter discovery and confirmation

Install CPython **3.9 or newer** from [python.org](https://www.python.org/),
including its standard library and SQLite. Bundled Tasks needs no extension
copying, manifest editing, disabling, or manual linking. Open **Extensions →
Python setup** (mouse button or `p`), inspect the complete absolute `python.exe`
path, and confirm it once. Cancel leaves Tasks blocked without affecting other
extensions or terminal recovery. If the path does not fit, enlarge the terminal
before approval; a clipped path cannot be approved.

Discovery reads only PEP 514 `Software\Python\PythonCore\<tag>\InstallPath`
registrations: HKCU before HKLM, both registry views, numeric newest supported
version within the first usable hive, native-width view preferred on a version
tie. It uses `ExecutablePath`, or the default InstallPath plus `python.exe` only
when ExecutablePath is absent. Enumeration and string reads are bounded;
enumeration errors or a capped/incomplete tag enumeration fail closed rather than
electing a possibly non-newest prefix.
Candidates must be existing regular absolute `.exe` files outside WindowsApps.
Registrations are candidate data, not publisher attestations. Discovery does not
execute candidates, use PATH, invoke `py`, or probe `python`/`python3`/Store aliases.

An explicit candidate override wins, but still requires exact-path confirmation:

```toml
[extensions]
tasks_python = 'C:\Users\you\AppData\Local\Programs\Python\Python313\python.exe'
```

The host records the approved path in `confirmed_tasks_python` in the same
section. A valid confirmed path is pinned across registry upgrades. Changing the
override never transfers consent; an invalid override fails without fallback.
Removing the confirmation revokes approval. After manually changing configuration,
reload configuration (or restart Vqro) to retire existing provider generations;
new launches always revalidate the current binding. Confirmation rechecks the
exact displayed path against the current candidate, preserves unrelated config
edits, and rebuilds provider generations. Install Python before confirming;
Vqro does not install it. Missing/unconfirmed CLI and Extensions status name the
python.org/configuration remedy. Path approval grants no package capabilities
and is not binary publisher/hash attestation; executable/image identity remains
subject to the loaded-image proof below.

The manifest's literal `vqro:python3` executable token is a **narrow managed-Tasks
bootstrap**, accepted only for the exact current compiled bundle generation and
asset bytes, not for an extension merely claiming the Tasks name. It is not a
general legacy-extension interpreter contract. Its removal condition is an
explicitly approved public interpreter contract replacing this bootstrap; other
extensions cannot use it to inherit Tasks approval. Only argv[0] is substituted;
static arguments and user tails stay literal. CLI execution, fresh launcher
proof, and actual provider startup/restart use one resolver. Manifest hashes,
ordinary exact-package consent, and immutable generations remain unchanged in
meaning. Linux/macOS resolve the token to `python3` as before.

### Host API

On Windows, the `extension.list` result includes an optional `interpreter` fact:
`{path: string|null, confirmed: boolean, message: string}`. This is outside curated
inventory entries; absence on other hosts/older servers does not break inventory.
The separately advertised optional `extension.interpreter.confirm` method takes
`{expected_path: string, confirmed: true}`. The server re-resolves the current
candidate, rejects invalid/stale paths without storing approval, atomically saves
only the confirmation key, and retires provider generations. This JSON API adds
no frozen endpoint-codec fields or legacy bridge methods. Confirmation is an
explicit operator action, not extension package capability consent.

Windows foreground CLI programs must be `.exe` executables, not batch files or
implicit shell commands. Transparent human attribution additionally requires the
confirmed absolute declared executable.

## Identity and foreground execution

The host obtains the named-pipe client's kernel PID, nonzero process-creation
FILETIME, token user SID and bounded fresh parent/argv observations. It retains
its pre-request connection-incarnation check. Windows execution keys use a
validated kernel boot GUID, PID and creation FILETIME; a new process lifetime
never inherits another execution merely by reusing its PID.

Human observations are `{class:"human_unverified",terminal_id:...,sid:...}`;
Unix observations retain `uid` instead. Mixed accounts and malformed SIDs reject.
SID/UID accounts never compare equal across kinds. Neither shape authenticates
a human gesture. Agent classification still requires the original owned
terminal root birth, actual known-agent ancestor, and foreground execution.
Imported roots without their original birth proof remain Unknown.

Windows cannot replace a running process as Unix `exec` does. The CLI therefore
keeps a foreground launcher alive while its child inherits cwd, literal argv,
stdin/stdout/stderr and console. The launcher returns the child's Windows exit
code. Its scoped Ctrl-C/Break handler keeps it alive to reap the child without
setting an inherited CTRL_C-ignore flag. Native close/logoff and interrupt
behavior must be tested; it is not guaranteed by cross-compilation.

Only one extra launcher hop can be transparent to human classification. The
host must prove live parent/incarnations/SIDs, direct loaded-image bindings,
an enabled unambiguous exact declaration/generation, and **all** resolved static
arguments plus the literally forwarded user tail. A name, argv prefix,
environment variable, arbitrary grandchild or failed query is never sufficient.
An original owned non-agent shell root must still be proved. The Windows walk
can stop before a positively matched current-server PID/birth or foreign SID;
metadata errors are failures, not boundaries. A root beyond the retained
64-process prefix is Unknown.

Loaded-image proof holds two executable files: the trusted host file and the
resolved declared program. Native `NtQueryInformationProcess` class 44
(`ProcessImageFileMapping`) must return **exact STATUS_SUCCESS** for host → host
file, launcher → that same host file, and child → declared file. A pathname or
file ID alone does not establish a previously loaded image after rename/rebind.
Stronger process-query and file execute/synchronize rights apply only to this
optional launcher proof. Unsupported, denied, mismatched or other statuses mean
no transparent human hop; there is no path-only fallback. Native API availability
and rename behavior require Windows validation. See the
[native class contract](https://ntdoc.m417z.com/processinfoclass#processimagefilemapping-44).

Selected-job capture compares the entire relevant root/descendant PID union and
parent topology against its final complete bounded snapshot, then rechecks all
retained SID/birth/argv metadata. Added, removed, moved, partial or capped trees
provide no agent proof. These are bounded observations, not an atomic freeze of
process creation.

## Data and uncertainty

The Windows default store is
`%LOCALAPPDATA%/vqro/extensions/vqro.tasks/tasks.sqlite`. `LOCALAPPDATA` must be an
absolute local app-data directory; a missing/invalid value fails clearly.
Explicit provider `--data-dir` wins. Unix XDG defaults remain unchanged. No old
accidental Unix-style Windows store is migrated automatically.

SQLite uses WAL, FULL synchronization and a one-second busy timeout. Use a local
filesystem supporting SQLite WAL locking; NTFS runtime behavior needs native
validation. SID decision acknowledgements use a separate `decision_seen_sid`
keyspace, leaving existing UID acknowledgement rows unchanged.

The CLI uses bounded local named-pipe byte I/O with a single deadline, partial
I/O handling and completion-safe cancellation. It never reconnects/resends a
submitted request. Timeout after submission can mean the action committed:
retain the original caller, operation ID, payload and revision; inspect its
receipt before deciding what to do. Provider stdin uses one bounded reader and
one queued frame, with a 256 KiB per-frame limit, explicit EOF/failure and timer
wakeups; it does not use Unix `select` on Windows pipes.

## Native trial

With a fresh, explicitly admitted exact Windows artifact and two Pi panes:

1. Run `vqro task whoami` in the shell and each pane; inspect SID human and exact
   agent/execution bindings rather than trusting pane labels.
2. Create/list a task, send/claim a message between the panes, and inspect Tasks
   attention. It must not report provider/identity unavailable.
3. Interrupt a foreground command; verify launcher/child cleanup and exit status.
4. Exercise provider restart and concurrent SQLite writers on the local volume.
   A busy precommit rejection has no receipt; uncertain writes must not retry.

Never run these steps against a real user's tasks/session without explicit
operator approval. Linux tests and compile-only Windows tests are not this trial.
