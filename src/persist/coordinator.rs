//! Unwired two-file persistence coordinator and roll-forward recovery journal.
//!
//! This module deliberately has no production caller. It establishes the crash contract needed
//! before session and extension-state persistence can share authority: one process-local writer,
//! exact resolved-path and digest binding, and roll-forward recovery after publication starts.
//!
//! Every operation requires a path-bound cross-process ownership guard in addition to the
//! process-local mutex. The fixed lock pathname lives in the same canonical, trusted private
//! parent as both targets and the journal. Unix `flock` assumes cooperative same-user writers: an
//! actor with parent write access could replace the pathname, so every Vqro writer must use this
//! guard and must never unlink or rename its lock file. Target basenames that conservatively alias
//! the fixed lock basename under Windows case and trailing-dot/space rules are rejected on every
//! platform, including case-sensitive filesystems; this avoids filesystem probing before wiring.
//! Production remains unwired. Journal-bound paths must be UTF-8 so their exact text can be
//! verified cross-platform. On Windows, replacement uses the platform WRITE_THROUGH path, but
//! parent-directory sync is explicitly a no-op; it does not provide Unix directory-fsync
//! durability.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::snapshot::{SessionSnapshot, MAX_SESSION_SNAPSHOT_BYTES};
use crate::extension_state::{DurableExtensionStateV1, MAX_STATE_STORE_BYTES};

const JOURNAL_VERSION: u32 = 1;
const JOURNAL_KIND: &str = "herdr.session-extension-state.roll-forward";
const MAX_JOURNAL_BYTES: u64 = 16 * 1024;
const STAGE_MARKER: &str = ".roll-forward-stage.";
const JOURNAL_TEMP_MARKER: &str = ".roll-forward-journal-tmp.";
const OWNERSHIP_FILE_NAME: &str = "session-extension-state.lock";

static PERSISTENCE_WRITER: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PersistenceOutcome {
    NotPublished,
    RecoveryRequired,
}

#[derive(Debug)]
pub(crate) struct PersistenceError {
    pub(crate) source: std::io::Error,
    // Read by the future coordinator caller; the staged API has no production caller yet.
    #[allow(dead_code)]
    pub(crate) outcome: PersistenceOutcome,
}

impl std::fmt::Display for PersistenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartupRecovery {
    NoJournal,
    Recovered,
}

#[derive(Debug, Clone)]
pub(crate) struct PersistencePaths {
    pub(crate) session: PathBuf,
    pub(crate) extension_state: PathBuf,
    pub(crate) journal: PathBuf,
}

#[derive(Debug)]
pub(crate) struct PersistenceOwnership {
    paths: ResolvedPaths,
    _guard: crate::platform::PersistenceLockGuard,
}

impl PersistenceOwnership {
    #[allow(dead_code)] // Removed when production persistence acquires the staged ownership fence.
    pub(crate) fn try_acquire(paths: &PersistencePaths) -> Result<Self, PersistenceError> {
        let paths = resolve_paths(paths).map_err(not_published)?;
        let guard = crate::platform::try_lock_persistence(&paths.ownership).map_err(|error| {
            let source = match error {
                crate::platform::PersistenceLockError::Busy => std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "persistence ownership is held by another process",
                ),
                crate::platform::PersistenceLockError::Io(error) => error,
            };
            not_published(source)
        })?;
        Ok(Self {
            paths,
            _guard: guard,
        })
    }

    fn verify(&self, paths: &PersistencePaths) -> Result<ResolvedPaths, PersistenceError> {
        let resolved = resolve_paths(paths).map_err(not_published)?;
        if resolved != self.paths {
            return Err(not_published(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "persistence ownership does not match the requested paths",
            )));
        }
        Ok(resolved)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JournalPhase {
    Prepared,
    ReplacingSession,
    ReplacingExtensionState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalFile {
    target_path: String,
    stage_path: String,
    sha256: String,
    length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RollForwardJournalV1 {
    version: u32,
    kind: String,
    phase: JournalPhase,
    session: JournalFile,
    extension_state: JournalFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedPaths {
    session: PathBuf,
    extension_state: PathBuf,
    journal: PathBuf,
    ownership: PathBuf,
    parent: PathBuf,
}

#[derive(Debug)]
struct JournalPublishError {
    source: std::io::Error,
    replacement_attempted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boundary {
    SessionStaged,
    ExtensionStateStaged,
    JournalPrepared,
    SessionIntent,
    SessionReplaced,
    ExtensionStateIntent,
    ExtensionStateReplaced,
    JournalRemoved,
}

// Process-local serialization complements the path-bound cross-process ownership guard.
fn exclusive_writer() -> Result<MutexGuard<'static, ()>, PersistenceError> {
    PERSISTENCE_WRITER.lock().map_err(|_| PersistenceError {
        source: std::io::Error::other("persistence coordinator lock is poisoned"),
        outcome: PersistenceOutcome::NotPublished,
    })
}

/// Staged API for a future single session + extension-state checkpoint caller.
///
/// It is intentionally not wired into ordinary saves. Once replacement is about to begin, every
/// failure leaves the journal for startup roll-forward and is reported as `RecoveryRequired`.
#[allow(dead_code)]
pub(crate) fn persist_checkpoint(
    ownership: &PersistenceOwnership,
    paths: &PersistencePaths,
    session: &SessionSnapshot,
    extension_state: &DurableExtensionStateV1,
) -> Result<(), PersistenceError> {
    persist_checkpoint_with(ownership, paths, session, extension_state, |_| Ok(()))
}

fn persist_checkpoint_with<F>(
    ownership: &PersistenceOwnership,
    paths: &PersistencePaths,
    session: &SessionSnapshot,
    extension_state: &DurableExtensionStateV1,
    mut boundary: F,
) -> Result<(), PersistenceError>
where
    F: FnMut(Boundary) -> std::io::Result<()>,
{
    let _writer = exclusive_writer()?;
    let paths = ownership.verify(paths)?;
    if path_entry_exists(&paths.journal).map_err(not_published)? {
        return Err(not_published(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "a persistence recovery journal already exists",
        )));
    }

    extension_state.validate().map_err(|error| {
        not_published(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid extension state: {error:?}"),
        ))
    })?;
    let session_bytes =
        encode_bounded(session, MAX_SESSION_SNAPSHOT_BYTES, "session").map_err(not_published)?;
    let session_text = std::str::from_utf8(&session_bytes).map_err(|error| {
        not_published(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    })?;
    super::snapshot::validate_snapshot(session_text).map_err(|error| {
        not_published(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    })?;
    let extension_bytes = encode_bounded(extension_state, MAX_STATE_STORE_BYTES, "extension state")
        .map_err(not_published)?;

    let session_stage = stage_path(&paths.session).map_err(not_published)?;
    let extension_stage = stage_path(&paths.extension_state).map_err(not_published)?;
    let mut publication_started = false;
    let result: Result<(), PersistenceError> = (|| {
        write_private_synced(&session_stage, &session_bytes).map_err(not_published)?;
        boundary(Boundary::SessionStaged).map_err(not_published)?;
        write_private_synced(&extension_stage, &extension_bytes).map_err(not_published)?;
        boundary(Boundary::ExtensionStateStaged).map_err(not_published)?;

        let mut journal = RollForwardJournalV1 {
            version: JOURNAL_VERSION,
            kind: JOURNAL_KIND.into(),
            phase: JournalPhase::Prepared,
            session: journal_file(&paths.session, &session_stage, &session_bytes)
                .map_err(not_published)?,
            extension_state: journal_file(
                &paths.extension_state,
                &extension_stage,
                &extension_bytes,
            )
            .map_err(not_published)?,
        };
        publish_journal(&paths, &journal).map_err(|error| {
            if error.replacement_attempted {
                recovery_required(error.source)
            } else {
                not_published(error.source)
            }
        })?;
        boundary(Boundary::JournalPrepared).map_err(not_published)?;

        journal.phase = JournalPhase::ReplacingSession;
        publish_journal(&paths, &journal).map_err(|error| recovery_required(error.source))?;
        publication_started = true;
        boundary(Boundary::SessionIntent).map_err(recovery_required)?;
        replace_and_sync(&session_stage, &paths.session).map_err(recovery_required)?;
        boundary(Boundary::SessionReplaced).map_err(recovery_required)?;

        journal.phase = JournalPhase::ReplacingExtensionState;
        publish_journal(&paths, &journal).map_err(|error| recovery_required(error.source))?;
        boundary(Boundary::ExtensionStateIntent).map_err(recovery_required)?;
        replace_and_sync(&extension_stage, &paths.extension_state).map_err(recovery_required)?;
        boundary(Boundary::ExtensionStateReplaced).map_err(recovery_required)?;

        verify_target(&journal.session).map_err(recovery_required)?;
        verify_target(&journal.extension_state).map_err(recovery_required)?;
        remove_journal(&paths.journal).map_err(recovery_required)?;
        boundary(Boundary::JournalRemoved).map_err(recovery_required)?;
        Ok(())
    })();

    if result
        .as_ref()
        .is_err_and(|error| error.outcome == PersistenceOutcome::NotPublished)
        && !publication_started
    {
        // A prepared journal commits the transaction to roll-forward, even before replacement.
        // Check this before cleanup: its stages are now recovery evidence, not scratch files. An
        // unreadable presence check is also publication-uncertain and must retain both stages.
        match path_entry_exists(&paths.journal) {
            Ok(true) => {
                return result.map_err(|error| PersistenceError {
                    source: error.source,
                    outcome: PersistenceOutcome::RecoveryRequired,
                });
            }
            Err(source) => return Err(recovery_required(source)),
            Ok(false) => {}
        }
        let _ = remove_owned_regular_file(&session_stage);
        let _ = remove_owned_regular_file(&extension_stage);
        let _ = crate::platform::sync_parent_directory(&paths.parent);
    }
    result
}

/// Inspects and completes a pending two-file checkpoint. Startup does not call this yet.
///
/// Recovery accepts only exact configured targets and exact stage/target digests. Any ambiguity is
/// returned without deleting or rolling back evidence.
#[allow(dead_code)]
pub(crate) fn recover_checkpoint_at_startup(
    ownership: &PersistenceOwnership,
    paths: &PersistencePaths,
) -> Result<StartupRecovery, PersistenceError> {
    let _writer = exclusive_writer()?;
    let paths = ownership.verify(paths)?;
    if !path_entry_exists(&paths.journal).map_err(recovery_required)? {
        return Ok(StartupRecovery::NoJournal);
    }
    let mut journal = read_journal(&paths).map_err(recovery_required)?;
    validate_journal(&paths, &journal).map_err(recovery_required)?;

    if journal.phase == JournalPhase::Prepared {
        journal.phase = JournalPhase::ReplacingSession;
        publish_journal(&paths, &journal).map_err(|error| recovery_required(error.source))?;
    }

    // Never trust phase progression as proof of either replacement. A stale, corrupted, or
    // manually advanced phase must still establish both exact target digests before deletion.
    ensure_replaced(&journal.session).map_err(recovery_required)?;
    if journal.phase != JournalPhase::ReplacingExtensionState {
        journal.phase = JournalPhase::ReplacingExtensionState;
        publish_journal(&paths, &journal).map_err(|error| recovery_required(error.source))?;
    }
    ensure_replaced(&journal.extension_state).map_err(recovery_required)?;
    verify_target(&journal.session).map_err(recovery_required)?;
    verify_target(&journal.extension_state).map_err(recovery_required)?;
    remove_journal(&paths.journal).map_err(recovery_required)?;
    Ok(StartupRecovery::Recovered)
}

fn not_published(source: std::io::Error) -> PersistenceError {
    PersistenceError {
        source,
        outcome: PersistenceOutcome::NotPublished,
    }
}

fn recovery_required(source: std::io::Error) -> PersistenceError {
    PersistenceError {
        source,
        outcome: PersistenceOutcome::RecoveryRequired,
    }
}

fn resolve_paths(paths: &PersistencePaths) -> std::io::Result<ResolvedPaths> {
    for target in [&paths.session, &paths.extension_state, &paths.journal] {
        validate_target_basename(target)?;
    }
    let session = resolve_exact_target(&paths.session)?;
    let extension_state = resolve_exact_target(&paths.extension_state)?;
    let journal = resolve_exact_target(&paths.journal)?;
    for target in [&session, &extension_state, &journal] {
        validate_target_basename(target)?;
    }
    if [
        session.as_path(),
        extension_state.as_path(),
        journal.as_path(),
    ]
    .into_iter()
    .collect::<std::collections::HashSet<_>>()
    .len()
        != 3
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "persistence paths must be distinct",
        ));
    }
    let parent = session
        .parent()
        .ok_or_else(|| std::io::Error::other("session target has no parent"))?
        .to_path_buf();
    if extension_state.parent() != Some(parent.as_path())
        || journal.parent() != Some(parent.as_path())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session, extension state, and journal must share one resolved directory",
        ));
    }
    let ownership = parent.join(OWNERSHIP_FILE_NAME);
    Ok(ResolvedPaths {
        session,
        extension_state,
        journal,
        ownership,
        parent,
    })
}

fn validate_target_basename(path: &Path) -> std::io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("persistence path has no file name"))?;
    let Some(name) = name.to_str() else {
        return Ok(());
    };
    let normalized = name.trim_end_matches(['.', ' ']);
    if normalized.eq_ignore_ascii_case(OWNERSHIP_FILE_NAME) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a persistence target basename aliases the fixed ownership basename",
        ));
    }
    Ok(())
}

fn resolve_exact_target(path: &Path) -> std::io::Result<PathBuf> {
    let resolved = crate::platform::resolve_write_target(path)?;
    let name = resolved
        .file_name()
        .ok_or_else(|| std::io::Error::other("persistence path has no file name"))?;
    let parent = resolved
        .parent()
        .ok_or_else(|| std::io::Error::other("persistence path has no parent"))?
        .canonicalize()?;
    let metadata = std::fs::metadata(&parent)?;
    if !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "persistence parent is not a directory",
        ));
    }
    Ok(parent.join(name))
}

struct CappedCounter<'a> {
    count: usize,
    maximum: usize,
    label: &'a str,
}

impl Write for CappedCounter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let next = self.count.checked_add(bytes.len()).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{} exceeds its size limit", self.label),
            )
        })?;
        if next > self.maximum {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{} exceeds its size limit", self.label),
            ));
        }
        self.count = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct ExactBuffer {
    bytes: Vec<u8>,
    expected: usize,
}

impl Write for ExactBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|next| next > self.expected)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "serialized size changed after bounded counting pass",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn encode_bounded<T: Serialize>(value: &T, maximum: u64, label: &str) -> std::io::Result<Vec<u8>> {
    let maximum = usize::try_from(maximum).unwrap_or(usize::MAX);
    let mut counter = CappedCounter {
        count: 0,
        maximum,
        label,
    };
    serde_json::to_writer_pretty(&mut counter, value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    counter.write_all(b"\n")?;

    // Allocate the payload buffer only after a capped counting pass proves the encoded size. The
    // second pass is itself capped at that exact count so even a stateful serializer cannot grow it.
    let mut output = ExactBuffer {
        bytes: Vec::with_capacity(counter.count),
        expected: counter.count,
    };
    serde_json::to_writer_pretty(&mut output, value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    output.write_all(b"\n")?;
    if output.bytes.len() != output.expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "serialized size changed after bounded counting pass",
        ));
    }
    Ok(output.bytes)
}

fn stage_path(target: &Path) -> std::io::Result<PathBuf> {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::other("persistence target has no UTF-8 file name"))?;
    Ok(target.with_file_name(format!("{name}{STAGE_MARKER}{}", secure_suffix()?)))
}

fn secure_suffix() -> std::io::Result<String> {
    let mut random = [0_u8; 16];
    crate::platform::fill_secure_random(&mut random)?;
    Ok(random.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn write_private_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = crate::platform::create_private_state_file(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn journal_file(target: &Path, stage: &Path, bytes: &[u8]) -> std::io::Result<JournalFile> {
    Ok(JournalFile {
        target_path: path_text(target)?,
        stage_path: path_text(stage)?,
        sha256: bytes_sha256(bytes),
        length: bytes.len() as u64,
    })
}

fn path_text(path: &Path) -> std::io::Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path is not UTF-8"))
}

fn bytes_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn publish_journal(
    paths: &ResolvedPaths,
    journal: &RollForwardJournalV1,
) -> Result<(), JournalPublishError> {
    publish_journal_with(paths, journal, crate::platform::replace_file)
}

fn publish_journal_with<Replace>(
    paths: &ResolvedPaths,
    journal: &RollForwardJournalV1,
    replace: Replace,
) -> Result<(), JournalPublishError>
where
    Replace: FnOnce(&Path, &Path) -> std::io::Result<()>,
{
    let before_replacement = |source| JournalPublishError {
        source,
        replacement_attempted: false,
    };
    let after_replacement = |source| JournalPublishError {
        source,
        replacement_attempted: true,
    };
    let bytes = encode_bounded(journal, MAX_JOURNAL_BYTES, "persistence journal")
        .map_err(before_replacement)?;
    let name = paths
        .journal
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            before_replacement(std::io::Error::other("journal path has no UTF-8 file name"))
        })?;
    let temporary = paths.journal.with_file_name(format!(
        "{name}{JOURNAL_TEMP_MARKER}{}",
        secure_suffix().map_err(before_replacement)?
    ));
    if let Err(source) = write_private_synced(&temporary, &bytes) {
        // Replacement was certainly not attempted, so this process still owns the temporary
        // pathname and may safely clean it. After the call below, pathname cleanup is forbidden:
        // even an error can mean replacement occurred and another writer reused the old name.
        let _ = remove_owned_regular_file(&temporary);
        let _ = crate::platform::sync_parent_directory(&paths.parent);
        return Err(before_replacement(source));
    }
    replace(&temporary, &paths.journal).map_err(after_replacement)?;
    crate::platform::sync_parent_directory(&paths.parent).map_err(after_replacement)
}

fn replace_and_sync(stage: &Path, target: &Path) -> std::io::Result<()> {
    crate::platform::replace_file(stage, target)?;
    crate::platform::sync_containing_directory(target)
}

fn remove_journal(journal: &Path) -> std::io::Result<()> {
    std::fs::remove_file(journal)?;
    crate::platform::sync_containing_directory(journal)
}

fn path_entry_exists(path: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn remove_owned_regular_file(path: &Path) -> std::io::Result<bool> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "owned persistence temporary is not a regular file",
        ));
    }
    std::fs::remove_file(path)?;
    Ok(true)
}

fn read_journal(paths: &ResolvedPaths) -> std::io::Result<RollForwardJournalV1> {
    let metadata = std::fs::symlink_metadata(&paths.journal)?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_JOURNAL_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "persistence journal is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&paths.journal)?
        .take(MAX_JOURNAL_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "persistence journal exceeds its size limit",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn validate_journal(paths: &ResolvedPaths, journal: &RollForwardJournalV1) -> std::io::Result<()> {
    if journal.version != JOURNAL_VERSION || journal.kind != JOURNAL_KIND {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "unsupported persistence journal contract",
        ));
    }
    validate_journal_file(
        paths,
        &journal.session,
        &paths.session,
        MAX_SESSION_SNAPSHOT_BYTES,
    )?;
    validate_journal_file(
        paths,
        &journal.extension_state,
        &paths.extension_state,
        MAX_STATE_STORE_BYTES,
    )
}

fn validate_journal_file(
    paths: &ResolvedPaths,
    file: &JournalFile,
    expected_target: &Path,
    maximum: u64,
) -> std::io::Result<()> {
    if file.target_path != path_text(expected_target)?
        || file.length > maximum
        || file.sha256.len() != 64
        || !file.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "journal target, digest, or bound is invalid",
        ));
    }
    let stage = Path::new(&file.stage_path);
    let expected_prefix = format!(
        "{}{STAGE_MARKER}",
        expected_target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| std::io::Error::other("target file name is not UTF-8"))?
    );
    let stage_name = stage
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let suffix = stage_name.strip_prefix(&expected_prefix).unwrap_or("");
    if stage.parent() != Some(paths.parent.as_path())
        || suffix.len() != 32
        || !suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "journal stage path is not an exact same-directory stage",
        ));
    }
    Ok(())
}

fn ensure_replaced(file: &JournalFile) -> std::io::Result<()> {
    let target = Path::new(&file.target_path);
    let stage = Path::new(&file.stage_path);
    if file_matches(target, file)? {
        cleanup_matching_owned_stage(stage, file)?;
        return Ok(());
    }
    if !file_matches(stage, file)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "neither target nor stage matches the journal digest",
        ));
    }
    replace_and_sync(stage, target)?;
    verify_target(file)
}

fn cleanup_matching_owned_stage(stage: &Path, expected: &JournalFile) -> std::io::Result<()> {
    let metadata = match std::fs::symlink_metadata(stage) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file()
        || metadata.len() != expected.length
        || crate::checksum::file_sha256(stage)? != expected.sha256
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "journal-bound stage does not match the already-published target",
        ));
    }
    std::fs::remove_file(stage)?;
    crate::platform::sync_containing_directory(stage)
}

fn verify_target(file: &JournalFile) -> std::io::Result<()> {
    if file_matches(Path::new(&file.target_path), file)? {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "replacement target does not match the journal digest",
        ))
    }
}

fn file_matches(path: &Path, expected: &JournalFile) -> std::io::Result<bool> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() || metadata.len() != expected.length {
        return Ok(false);
    }
    Ok(crate::checksum::file_sha256(path)? == expected.sha256)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::ser::SerializeSeq;
    use std::collections::BTreeSet;
    use std::sync::Barrier;

    struct RepeatedStrings(usize);

    impl Serialize for RepeatedStrings {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            let mut sequence = serializer.serialize_seq(Some(self.0))?;
            for _ in 0..self.0 {
                sequence.serialize_element("0123456789abcdef")?;
            }
            sequence.end()
        }
    }

    fn paths_for_directory(directory: &Path) -> PersistencePaths {
        PersistencePaths {
            session: directory.join("session.json"),
            extension_state: directory.join("extension-state.json"),
            journal: directory.join("session-extension-state.journal.json"),
        }
    }

    fn fixture() -> (
        PathBuf,
        PersistencePaths,
        PersistenceOwnership,
        SessionSnapshot,
        DurableExtensionStateV1,
    ) {
        let directory = std::env::temp_dir().join(format!(
            "herdr-two-file-journal-{}-{}",
            std::process::id(),
            secure_suffix().unwrap()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let paths = paths_for_directory(&directory);
        let session = SessionSnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            workspaces: Vec::new(),
            repositories: Vec::new(),
            space_order: Vec::new(),
            active: None,
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
            delegations: Vec::new(),
        };
        let state = DurableExtensionStateV1::new("session:test".into(), 1).unwrap();
        let ownership = PersistenceOwnership::try_acquire(&paths).unwrap();
        (directory, paths, ownership, session, state)
    }

    #[test]
    fn crash_recovery_rolls_forward_at_every_durable_phase() {
        let boundaries = [
            Boundary::JournalPrepared,
            Boundary::SessionIntent,
            Boundary::SessionReplaced,
            Boundary::ExtensionStateIntent,
            Boundary::ExtensionStateReplaced,
        ];
        for crash_at in boundaries {
            let (directory, paths, ownership, session, state) = fixture();
            let error = persist_checkpoint_with(&ownership, &paths, &session, &state, |boundary| {
                if boundary == crash_at {
                    Err(std::io::Error::other("simulated crash"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
            assert_eq!(error.outcome, PersistenceOutcome::RecoveryRequired);
            assert_eq!(
                recover_checkpoint_at_startup(&ownership, &paths).unwrap(),
                StartupRecovery::Recovered
            );
            let session_text = std::fs::read_to_string(&paths.session).unwrap();
            assert!(super::super::snapshot::parse_snapshot(&session_text).is_ok());
            assert!(matches!(
                super::super::extension_state::load(&paths.extension_state),
                super::super::extension_state::ExtensionStateLoad::Loaded(_)
            ));
            assert!(!paths.journal.try_exists().unwrap());
            drop(ownership);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn fault_after_journal_removal_never_rolls_back_completed_targets() {
        let (directory, paths, ownership, session, state) = fixture();
        let error = persist_checkpoint_with(&ownership, &paths, &session, &state, |boundary| {
            if boundary == Boundary::JournalRemoved {
                Err(std::io::Error::other("simulated post-removal fault"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(error.outcome, PersistenceOutcome::RecoveryRequired);
        assert_eq!(
            recover_checkpoint_at_startup(&ownership, &paths).unwrap(),
            StartupRecovery::NoJournal
        );
        assert!(paths.session.try_exists().unwrap());
        assert!(paths.extension_state.try_exists().unwrap());
        drop(ownership);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn pre_journal_faults_clean_stages_without_publishing() {
        for crash_at in [Boundary::SessionStaged, Boundary::ExtensionStateStaged] {
            let (directory, paths, ownership, session, state) = fixture();
            let error = persist_checkpoint_with(&ownership, &paths, &session, &state, |boundary| {
                if boundary == crash_at {
                    Err(std::io::Error::other("simulated preparation fault"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
            assert_eq!(error.outcome, PersistenceOutcome::NotPublished);
            assert_eq!(
                recover_checkpoint_at_startup(&ownership, &paths).unwrap(),
                StartupRecovery::NoJournal
            );
            let entries = std::fs::read_dir(&directory)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            let lock_path = resolve_paths(&paths).unwrap().ownership;
            assert_eq!(entries.as_slice(), std::slice::from_ref(&lock_path));
            drop(ownership);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn uncertain_journal_replacement_never_cleans_a_reused_source_path() {
        let (directory, paths, ownership, session, state) = fixture();
        let _ = persist_checkpoint_with(&ownership, &paths, &session, &state, |boundary| {
            (boundary != Boundary::JournalPrepared)
                .then_some(())
                .ok_or_else(|| std::io::Error::other("crash"))
        });
        let resolved = resolve_paths(&paths).unwrap();
        let mut journal = read_journal(&resolved).unwrap();
        journal.phase = JournalPhase::ReplacingSession;
        let mut reused = None;
        let error = publish_journal_with(&resolved, &journal, |source, destination| {
            crate::platform::replace_file(source, destination)?;
            std::fs::write(source, b"reused-after-replacement")?;
            reused = Some(source.to_path_buf());
            Err(std::io::Error::other("uncertain replacement result"))
        })
        .unwrap_err();

        assert!(error.replacement_attempted);
        let reused = reused.unwrap();
        assert_eq!(std::fs::read(&reused).unwrap(), b"reused-after-replacement");
        assert_eq!(
            read_journal(&resolved).unwrap().phase,
            JournalPhase::ReplacingSession
        );
        drop(ownership);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn bounded_serialization_rejects_oversized_session_and_state_before_payload_allocation() {
        for label in ["session", "extension state"] {
            let error = encode_bounded(&RepeatedStrings(16), 64, label).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
            assert!(error.to_string().contains("exceeds its size limit"));
        }
    }

    #[test]
    fn recovery_rejects_tampering_unknown_versions_and_wrong_paths() {
        let (directory, paths, ownership, session, state) = fixture();
        let _ = persist_checkpoint_with(&ownership, &paths, &session, &state, |boundary| {
            (boundary != Boundary::JournalPrepared)
                .then_some(())
                .ok_or_else(|| std::io::Error::other("crash"))
        });
        let resolved = resolve_paths(&paths).unwrap();
        let original = read_journal(&resolved).unwrap();
        let cases = [
            {
                let mut value = original.clone();
                value.version += 1;
                value
            },
            {
                let mut value = original.clone();
                value.session.target_path =
                    resolved.parent.join("other.json").display().to_string();
                value
            },
            {
                let mut value = original.clone();
                value.extension_state.sha256 = "0".repeat(64);
                value
            },
        ];
        for value in cases {
            publish_journal(&resolved, &value).unwrap();
            let error = recover_checkpoint_at_startup(&ownership, &paths).unwrap_err();
            assert_eq!(error.outcome, PersistenceOutcome::RecoveryRequired);
        }
        drop(ownership);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn advanced_phase_still_repairs_and_verifies_both_targets() {
        let (directory, paths, ownership, session, state) = fixture();
        let _ = persist_checkpoint_with(&ownership, &paths, &session, &state, |boundary| {
            (boundary != Boundary::JournalPrepared)
                .then_some(())
                .ok_or_else(|| std::io::Error::other("crash"))
        });
        let resolved = resolve_paths(&paths).unwrap();
        let mut journal = read_journal(&resolved).unwrap();
        journal.phase = JournalPhase::ReplacingExtensionState;
        publish_journal(&resolved, &journal).unwrap();

        assert_eq!(
            recover_checkpoint_at_startup(&ownership, &paths).unwrap(),
            StartupRecovery::Recovered
        );
        verify_target(&journal.session).unwrap();
        verify_target(&journal.extension_state).unwrap();
        drop(ownership);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn late_phase_rejects_missing_or_corrupt_session_after_stage_was_consumed() {
        for replacement in [None, Some(b"corrupt-session".as_slice())] {
            let (directory, paths, ownership, session, state) = fixture();
            let _ = persist_checkpoint_with(&ownership, &paths, &session, &state, |boundary| {
                (boundary != Boundary::ExtensionStateIntent)
                    .then_some(())
                    .ok_or_else(|| std::io::Error::other("crash"))
            });
            let resolved = resolve_paths(&paths).unwrap();
            let journal = read_journal(&resolved).unwrap();
            assert_eq!(journal.phase, JournalPhase::ReplacingExtensionState);
            match replacement {
                Some(bytes) => std::fs::write(&paths.session, bytes).unwrap(),
                None => std::fs::remove_file(&paths.session).unwrap(),
            }

            let error = recover_checkpoint_at_startup(&ownership, &paths).unwrap_err();
            assert_eq!(error.outcome, PersistenceOutcome::RecoveryRequired);
            assert!(paths.journal.try_exists().unwrap());
            assert!(!Path::new(&journal.session.stage_path).try_exists().unwrap());
            drop(ownership);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn identical_targets_remove_only_matching_bound_stages_before_completion() {
        let (directory, paths, ownership, session, state) = fixture();
        let _ = persist_checkpoint_with(&ownership, &paths, &session, &state, |boundary| {
            (boundary != Boundary::JournalPrepared)
                .then_some(())
                .ok_or_else(|| std::io::Error::other("crash"))
        });
        let resolved = resolve_paths(&paths).unwrap();
        let journal = read_journal(&resolved).unwrap();
        std::fs::copy(&journal.session.stage_path, &journal.session.target_path).unwrap();
        std::fs::copy(
            &journal.extension_state.stage_path,
            &journal.extension_state.target_path,
        )
        .unwrap();

        assert_eq!(
            recover_checkpoint_at_startup(&ownership, &paths).unwrap(),
            StartupRecovery::Recovered
        );
        verify_target(&journal.session).unwrap();
        verify_target(&journal.extension_state).unwrap();
        assert!(!Path::new(&journal.session.stage_path).try_exists().unwrap());
        assert!(!Path::new(&journal.extension_state.stage_path)
            .try_exists()
            .unwrap());
        assert!(!paths.journal.try_exists().unwrap());
        drop(ownership);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn recovery_rejects_oversized_journal_without_touching_targets() {
        let (directory, paths, ownership, _, _) = fixture();
        std::fs::write(&paths.session, b"old-session").unwrap();
        std::fs::write(&paths.extension_state, b"old-state").unwrap();
        std::fs::write(&paths.journal, vec![b'x'; MAX_JOURNAL_BYTES as usize + 1]).unwrap();

        let error = recover_checkpoint_at_startup(&ownership, &paths).unwrap_err();
        assert_eq!(error.outcome, PersistenceOutcome::RecoveryRequired);
        assert_eq!(std::fs::read(&paths.session).unwrap(), b"old-session");
        assert_eq!(std::fs::read(&paths.extension_state).unwrap(), b"old-state");
        assert!(paths.journal.try_exists().unwrap());
        drop(ownership);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn never_rolls_back_after_first_replacement() {
        let (directory, paths, ownership, session, state) = fixture();
        std::fs::write(&paths.session, b"old-session").unwrap();
        std::fs::write(&paths.extension_state, b"old-state").unwrap();
        let error = persist_checkpoint_with(&ownership, &paths, &session, &state, |boundary| {
            if boundary == Boundary::SessionReplaced {
                Err(std::io::Error::other("crash after first replacement"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(error.outcome, PersistenceOutcome::RecoveryRequired);
        assert_ne!(std::fs::read(&paths.session).unwrap(), b"old-session");
        assert_eq!(std::fs::read(&paths.extension_state).unwrap(), b"old-state");
        recover_checkpoint_at_startup(&ownership, &paths).unwrap();
        assert_ne!(std::fs::read(&paths.session).unwrap(), b"old-session");
        assert_ne!(std::fs::read(&paths.extension_state).unwrap(), b"old-state");
        drop(ownership);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ownership_file_is_private_nontruncating_and_rejects_symlinks() {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};

        let directory = std::env::temp_dir().join(format!(
            "herdr-ownership-file-{}-{}",
            std::process::id(),
            secure_suffix().unwrap()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let paths = paths_for_directory(&directory);
        let lock_path = resolve_paths(&paths).unwrap().ownership;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true).mode(0o666);
        let mut file = options.open(&lock_path).unwrap();
        file.write_all(b"do-not-truncate").unwrap();
        drop(file);

        let ownership = PersistenceOwnership::try_acquire(&paths).unwrap();
        assert_eq!(std::fs::read(&lock_path).unwrap(), b"do-not-truncate");
        assert_eq!(std::fs::metadata(&lock_path).unwrap().mode() & 0o777, 0o600);

        std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o666)).unwrap();
        let contender = PersistenceOwnership::try_acquire(&paths).unwrap_err();
        assert_eq!(contender.source.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(
            std::fs::metadata(&lock_path).unwrap().mode() & 0o777,
            0o666,
            "a contender must not chmod a lock it did not acquire"
        );
        drop(ownership);
        drop(PersistenceOwnership::try_acquire(&paths).unwrap());
        assert_eq!(std::fs::metadata(&lock_path).unwrap().mode() & 0o777, 0o600);

        std::fs::remove_file(&lock_path).unwrap();
        let target = directory.join("lock-target");
        std::fs::write(&target, b"target").unwrap();
        std::os::unix::fs::symlink(&target, &lock_path).unwrap();
        let error = PersistenceOwnership::try_acquire(&paths).unwrap_err();
        assert_ne!(error.source.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(std::fs::read(&target).unwrap(), b"target");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn ownership_is_exclusive_in_one_process_and_bound_to_exact_paths() {
        let (directory, paths, ownership, session, state) = fixture();
        let contender = PersistenceOwnership::try_acquire(&paths).unwrap_err();
        assert_eq!(contender.source.kind(), std::io::ErrorKind::WouldBlock);

        let equivalent_parent = directory.join(".");
        let equivalent = PersistencePaths {
            session: equivalent_parent.join("session.json"),
            extension_state: equivalent_parent.join("extension-state.json"),
            journal: equivalent_parent.join("session-extension-state.journal.json"),
        };
        assert_eq!(
            resolve_paths(&paths).unwrap(),
            resolve_paths(&equivalent).unwrap()
        );
        assert_eq!(
            PersistenceOwnership::try_acquire(&equivalent)
                .unwrap_err()
                .source
                .kind(),
            std::io::ErrorKind::WouldBlock
        );

        let mut mismatched = paths.clone();
        mismatched.session = directory.join("other-session.json");
        let error = persist_checkpoint(&ownership, &mismatched, &session, &state).unwrap_err();
        assert_eq!(error.outcome, PersistenceOutcome::NotPublished);
        assert_eq!(error.source.kind(), std::io::ErrorKind::InvalidInput);

        drop(ownership);
        let reacquired = PersistenceOwnership::try_acquire(&paths).unwrap();
        drop(reacquired);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn every_target_rejects_conservative_ownership_basename_aliases() {
        let (directory, paths, ownership, _, _) = fixture();
        drop(ownership);
        let aliases = [
            "session-extension-state.lock",
            "SESSION-EXTENSION-STATE.LOCK",
            "SeSsIoN-ExTeNsIoN-StAtE.LoCk",
            "session-extension-state.lock.",
            "session-extension-state.lock...",
            "session-extension-state.lock ",
            "session-extension-state.lock. .  ",
        ];
        for alias in aliases {
            for target in 0..3 {
                let mut candidate = paths.clone();
                let path = directory.join(alias);
                match target {
                    0 => candidate.session = path,
                    1 => candidate.extension_state = path,
                    2 => candidate.journal = path,
                    _ => unreachable!(),
                }
                let error = resolve_paths(&candidate).unwrap_err();
                assert_eq!(
                    error.kind(),
                    std::io::ErrorKind::InvalidInput,
                    "target {target} accepted alias {alias:?}"
                );
            }
        }
        let mut no_probe = paths.clone();
        no_probe.session = directory
            .join("missing-parent")
            .join("SESSION-EXTENSION-STATE.LOCK. ");
        assert_eq!(
            resolve_paths(&no_probe).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );

        for ordinary in [
            "session.json",
            "session-extension-state.locked",
            "session-extension-state.lock.tmp",
            "other-session-extension-state.lock.",
        ] {
            validate_target_basename(&directory.join(ordinary)).unwrap();
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn coordinator_never_removes_ownership_file_and_reuses_it_across_acquisitions() {
        let (directory, paths, ownership, session, state) = fixture();
        let lock_path = resolve_paths(&paths).unwrap().ownership;
        drop(ownership);
        std::fs::write(&lock_path, b"persistent-lock-marker").unwrap();

        let ownership = PersistenceOwnership::try_acquire(&paths).unwrap();
        persist_checkpoint(&ownership, &paths, &session, &state).unwrap();
        assert_eq!(
            recover_checkpoint_at_startup(&ownership, &paths).unwrap(),
            StartupRecovery::NoJournal
        );
        assert_eq!(
            std::fs::read(&lock_path).unwrap(),
            b"persistent-lock-marker"
        );
        drop(ownership);
        assert!(lock_path.is_file());

        let ownership = PersistenceOwnership::try_acquire(&paths).unwrap();
        assert_eq!(
            std::fs::read(&lock_path).unwrap(),
            b"persistent-lock-marker"
        );
        drop(ownership);
        assert!(lock_path.is_file());

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_ownership_rejects_reparse_and_interior_nul_paths() {
        use std::os::windows::ffi::OsStringExt as _;

        let directory = std::env::temp_dir().join(format!(
            "herdr-windows-ownership-reparse-{}-{}",
            std::process::id(),
            secure_suffix().unwrap()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let paths = paths_for_directory(&directory);
        let lock_path = resolve_paths(&paths).unwrap().ownership;
        let target = directory.join("lock-target");
        std::fs::write(&target, b"target").unwrap();
        std::os::windows::fs::symlink_file(&target, &lock_path).unwrap();
        let error = PersistenceOwnership::try_acquire(&paths).unwrap_err();
        assert_ne!(error.source.kind(), std::io::ErrorKind::WouldBlock);

        let nul_path = PathBuf::from(std::ffi::OsString::from_wide(&[
            b'l' as u16,
            0,
            b'k' as u16,
        ]));
        let error = crate::platform::try_lock_persistence(&nul_path).unwrap_err();
        assert!(
            matches!(error, crate::platform::PersistenceLockError::Io(error) if error.kind() == std::io::ErrorKind::InvalidInput)
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_ownership_denies_path_mutation_until_drop() {
        let (directory, paths, ownership, _, _) = fixture();
        let lock_path = resolve_paths(&paths).unwrap().ownership;
        let renamed = directory.join("renamed.lock");
        let replacement = directory.join("replacement.lock");
        std::fs::write(&replacement, b"replacement").unwrap();

        assert!(std::fs::remove_file(&lock_path).is_err());
        assert!(std::fs::rename(&lock_path, &renamed).is_err());
        assert!(crate::platform::replace_file(&replacement, &lock_path).is_err());
        assert!(lock_path.is_file());
        assert!(replacement.is_file());

        drop(ownership);
        std::fs::rename(&lock_path, &renamed).unwrap();
        std::fs::rename(&renamed, &lock_path).unwrap();
        crate::platform::replace_file(&replacement, &lock_path).unwrap();
        assert_eq!(std::fs::read(&lock_path).unwrap(), b"replacement");
        std::fs::remove_file(&lock_path).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    const OWNERSHIP_CHILD_MODE: &str = "HERDR_TEST_PERSISTENCE_OWNERSHIP_MODE";
    const OWNERSHIP_CHILD_DIRECTORY: &str = "HERDR_TEST_PERSISTENCE_OWNERSHIP_DIRECTORY";
    const OWNERSHIP_CHILD_READY: &str = "HERDR_TEST_PERSISTENCE_OWNERSHIP_READY";
    const OWNERSHIP_CHILD_STOP: &str = "HERDR_TEST_PERSISTENCE_OWNERSHIP_STOP";
    const OWNERSHIP_CHILD_DONE: &str = "HERDR_TEST_PERSISTENCE_OWNERSHIP_DONE";

    #[test]
    #[allow(clippy::zombie_processes)] // The holder must exit before its exec child to test CLOEXEC/noninheritance.
    fn persistence_ownership_child_harness() {
        let Ok(mode) = std::env::var(OWNERSHIP_CHILD_MODE) else {
            return;
        };
        let directory = PathBuf::from(std::env::var_os(OWNERSHIP_CHILD_DIRECTORY).unwrap());
        if mode == "grandchild" {
            let ready = PathBuf::from(std::env::var_os(OWNERSHIP_CHILD_READY).unwrap());
            let stop = PathBuf::from(std::env::var_os(OWNERSHIP_CHILD_STOP).unwrap());
            let done = PathBuf::from(std::env::var_os(OWNERSHIP_CHILD_DONE).unwrap());
            std::fs::write(ready, b"ready").unwrap();
            while !stop.exists() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            std::fs::write(done, b"done").unwrap();
            return;
        }

        let paths = paths_for_directory(&directory);
        let ownership = PersistenceOwnership::try_acquire(&paths).unwrap();
        if mode == "hold" {
            let ready = PathBuf::from(std::env::var_os(OWNERSHIP_CHILD_READY).unwrap());
            std::fs::write(ready, b"ready").unwrap();
            let mut byte = [0_u8; 1];
            let _ = std::io::stdin().read(&mut byte);
            drop(ownership);
            return;
        }
        assert_eq!(mode, "spawn-grandchild");
        let ready = PathBuf::from(std::env::var_os(OWNERSHIP_CHILD_READY).unwrap());
        let stop = PathBuf::from(std::env::var_os(OWNERSHIP_CHILD_STOP).unwrap());
        let done = PathBuf::from(std::env::var_os(OWNERSHIP_CHILD_DONE).unwrap());
        let mut grandchild = std::process::Command::new(std::env::current_exe().unwrap());
        grandchild
            .args([
                "--exact",
                "persist::coordinator::tests::persistence_ownership_child_harness",
                "--nocapture",
            ])
            .env(OWNERSHIP_CHILD_MODE, "grandchild")
            .env(OWNERSHIP_CHILD_DIRECTORY, &directory)
            .env(OWNERSHIP_CHILD_READY, &ready)
            .env(OWNERSHIP_CHILD_STOP, &stop)
            .env(OWNERSHIP_CHILD_DONE, &done)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let _grandchild = grandchild.spawn().unwrap();
        for _ in 0..500 {
            if ready.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(ready.exists());
        drop(ownership);
    }

    #[allow(clippy::zombie_processes)] // Ownership of the live child is returned to the caller, which always waits.
    fn spawn_ownership_holder(directory: &Path, marker: &str) -> std::process::Child {
        let ready = directory.join(marker);
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "persist::coordinator::tests::persistence_ownership_child_harness",
                "--nocapture",
            ])
            .env(OWNERSHIP_CHILD_MODE, "hold")
            .env(OWNERSHIP_CHILD_DIRECTORY, directory)
            .env(OWNERSHIP_CHILD_READY, &ready)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        for _ in 0..500 {
            if ready.exists() {
                return child;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("persistence ownership child did not become ready");
    }

    #[test]
    fn ownership_fences_processes_and_releases_on_drop_kill_and_exec() {
        let (directory, paths, ownership, _, _) = fixture();
        drop(ownership);

        let mut holder = spawn_ownership_holder(&directory, "holder.ready");
        assert_eq!(
            PersistenceOwnership::try_acquire(&paths)
                .unwrap_err()
                .source
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        holder.stdin.take().unwrap().write_all(b"x").unwrap();
        assert!(holder.wait().unwrap().success());
        drop(PersistenceOwnership::try_acquire(&paths).unwrap());

        let mut killed = spawn_ownership_holder(&directory, "killed.ready");
        killed.kill().unwrap();
        let _ = killed.wait();
        drop(PersistenceOwnership::try_acquire(&paths).unwrap());

        let ready_path = directory.join("grandchild.ready");
        let stop_path = directory.join("grandchild.stop");
        let done_path = directory.join("grandchild.done");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "persist::coordinator::tests::persistence_ownership_child_harness",
                "--nocapture",
            ])
            .env(OWNERSHIP_CHILD_MODE, "spawn-grandchild")
            .env(OWNERSHIP_CHILD_DIRECTORY, &directory)
            .env(OWNERSHIP_CHILD_READY, &ready_path)
            .env(OWNERSHIP_CHILD_STOP, &stop_path)
            .env(OWNERSHIP_CHILD_DONE, &done_path)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(ready_path.exists());
        drop(PersistenceOwnership::try_acquire(&paths).unwrap());
        std::fs::write(&stop_path, b"stop").unwrap();
        for _ in 0..500 {
            if done_path.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(done_path.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn coordinator_is_exclusive() {
        let barrier = std::sync::Arc::new(Barrier::new(2));
        let entered = std::sync::Arc::new(Barrier::new(2));
        let seen = std::sync::Arc::new(Mutex::new(BTreeSet::new()));
        std::thread::scope(|scope| {
            for id in 0..2 {
                let barrier = barrier.clone();
                let entered = entered.clone();
                let seen = seen.clone();
                scope.spawn(move || {
                    barrier.wait();
                    let _guard = exclusive_writer().unwrap();
                    let mut active = seen.lock().unwrap();
                    assert!(active.insert(id));
                    assert_eq!(active.len(), 1);
                    active.remove(&id);
                    drop(active);
                    entered.wait();
                });
            }
            // Let either holder leave before requiring both threads to finish; the set assertion
            // proves the mutex, while this barrier keeps the test deterministic.
            entered.wait();
            entered.wait();
        });
    }
}
