# Native Windows persistence follow-up (W02–W04, W07)

Source baseline: main 47088308. Native evidence: `windows-native-install.md`, especially W02–W04.
The reported native failures are not dismissed as fixture defects. Linux tests and a GNU-target check
are not native Windows or power-loss certification.

## W02: quarantine reservation

The reservation must become a `TempPath` before publishing the staged backup: this closes its handle
while retaining its pathname and cleanup guard. The pre-publication `sync_all` and write-through move
remain unchanged. The Windows publication test now exclusively opens the destination before calling
the mock publisher, so a retained reservation handle fails the control. Native recovery and barrier
tests must be rerun after integration.

## W03: bounded refusal under the retained durability barrier

Native evidence contradicts the earlier assumption that DELETE sharing alone makes MoveFileExW
replacement harmless. The old `windows_replace_with_open_delete_shared_reader` exposed a real unsupported assumption,
not a fixture defect. It is replaced by `windows_open_reader_blocks_replace_without_adopting_transaction`,
which tests the now-explicit refusal contract. A default share mode grants permission but does not establish
that this specific rename operation supports replacing an open target.

Microsoft documents `FILE_RENAME_POSIX_SEMANTICS` with `FILE_RENAME_REPLACE_IF_EXISTS` as allowing
replacement with existing handles; old handles retain the original file, new opens get the replacement.
`FILE_RENAME_INFO.Flags` selects this behavior for `FileRenameInfoEx`.

However, the documented flags have no equivalent to `MOVEFILE_WRITE_THROUGH`. The consulted
SetFileInformationByHandle and FlushFileBuffers documentation does not establish that a post-rename
file flush is the same namespace-publication barrier as the currently approved write-through move.
Renaming POSIX-first and then issuing a different write-through operation also lacks that justification.
Flushing the entire volume requires administrative privileges and is not an acceptable new requirement.

Lead's binding ruling retains MoveFileExW WRITE_THROUGH and allows bounded backoff only after
ACCESS_DENIED or SHARING_VIOLATION refuses the rename: eight attempts, seven scheduled waits totaling
one second. There is no POSIX rename fallback, no retry after success, and no transaction/input replay.
A persistent refusal leaves the original and committed state untouched. The helper is shared by ordinary
store writes and quarantine publication; failed backup publication still rejects the original replacement.

Linux fake-call controls require bounded refusal with unchanged bytes, success on a second attempt,
and immediate refusal of unrelated errors. The native-only store test retains an ordinary reader and
requires clean refusal with unchanged disk bytes, entries, history and transaction numbering; closing
the reader then permits a fresh explicit transaction. Core protocol and Windows usage docs record
this reader/write restriction. No stronger native durability guarantee is inferred.

References:
- [FILE_RENAME_INFORMATION flags](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information)
- [FILE_RENAME_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_rename_info)
- [SetFileInformationByHandle](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-setfileinformationbyhandle)
- [MoveFileExW WRITE_THROUGH](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw)
- [FlushFileBuffers scope and volume privilege](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers)

## W04: separate append-handle resize defect

The failing concurrent log test starts by seeding a file at twice LOG_BYTES. Before any rotation,
`append` calls `file.set_len(LOG_BYTES)` on its append-only handle. Rust 1.94's Windows
`OpenOptions::get_access_mode` deliberately omits FILE_WRITE_DATA whenever append is true, even if
write is also true. Its `File::truncate` uses FileEndOfFileInfo. Microsoft requires FILE_WRITE_DATA for
the corresponding FileEndOfFileInformation operation. Thus the first oversized-log trim has an
access mismatch independently of concurrent renames. This is a source-supported cause of ACCESS_DENIED;
the native report does not identify the exact failing syscall, so further rotation defects remain possible.

Fix: on Windows only, trim through a short-lived `.write(true)` (not append) handle under the existing
APPEND mutex. The normal writing handle remains append-only. That extra resize handle closes before
rotation. No new rename API, deletion policy, size limit, retry, persistence claim or log replay is added.
Unix retains its existing trim path. Diagnostic logs remain nondurable.

Tests: a portable isolated write-handle trim verifies the existing reader observes the bounded same
file; the native-only regression first demonstrates append-only resize refusal, then requires append
and rotation to succeed. The existing overlapping-generation/two-fixed-files test remains unchanged
and must pass natively; it is not waived by a Linux pass.

References:
- [Rust 1.94 Windows filesystem source: get_access_mode and truncate](https://github.com/rust-lang/rust/blob/1.94.0/library/std/src/sys/fs/windows.rs)
- [NtSetInformationFile: FileEndOfFileInformation requires FILE_WRITE_DATA](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-ntsetinformationfile)
- [File access rights](https://learn.microsoft.com/en-us/windows/win32/fileio/file-access-rights-constants)

## W07: exact vendor bytes

The root `.gitattributes` rule `vendor/** -text` prevents ambient autocrlf conversion. The upstream
nested attributes still explicitly pin LF for recognized source types; remaining vendored files
(such as extensionless legal text) inherit `-text`. No vendor bytes are changed. Vendor validation
checks the entire pinned inventory rather than assuming a clean Git status proves byte identity.
