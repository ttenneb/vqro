# Tasks on Windows

Status: experimental Windows implementation. Cross-compilation does not certify
native named-pipe identity, console interruption, SQLite/NTFS locking, or Pi
message/attention behavior; those require a native trial.

Tasks is a trusted local Python extension, not a sandbox. Installation does not
grant API or package-to-package authority. Generic terminal recovery remains
available if Tasks cannot start.

## Interpreter and exact artifact setup

Windows requires a trusted CPython installation with the standard library and
SQLite. Do not assume `python3`, `python`, the `py` launcher, or a Microsoft Store
application-execution alias names that installation. Vqro does not probe these
names or fall back to another interpreter.

For Windows, prepare a separate operator-owned copy of the Tasks extension,
outside Vqro's managed bundled generations. In that copy's `vqro-extension.toml`,
replace **all three** `python3` program tokens (both CLI declarations and the
provider service command) with the absolute path to the trusted `python.exe`.
Use TOML literal strings for Windows paths or forward slashes. Preserve the
remaining static argv tokens. Register/enable that exact local manifest using
the ordinary extension mechanism and explicitly approve its requested
capabilities. Do not edit an immutable bundled generation, reuse old artifact
consent for changed bytes, or enable two competing `task`/`message` aliases.
Interpreter/package installation and enabling on a real installation are
operator actions, not performed by validation scripts.

Missing setup fails explicitly. Windows foreground CLI programs must be `.exe`
executables, not batch files or implicit shell commands. Transparent human
attribution additionally requires an absolute declared executable. Linux/macOS
retain the shipped `python3` argv and their existing interpreter requirements.

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
