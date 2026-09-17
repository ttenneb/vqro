//! Unwired two-file persistence coordinator and roll-forward recovery journal.
//!
//! This module deliberately has no production caller. It establishes the crash contract needed
//! before session and extension-state persistence can share authority: one process-local writer,
//! exact resolved-path and digest binding, and roll-forward recovery after publication starts.
//!
//! The mutex does not fence another process. Cross-process ownership fencing is required before
//! this module may be wired into production. Journal-bound paths must be UTF-8 so their exact text
//! can be verified cross-platform. On Windows, replacement uses the platform WRITE_THROUGH path,
//! but parent-directory sync is explicitly a no-op; it does not provide Unix directory-fsync
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

#[derive(Debug, Clone)]
struct ResolvedPaths {
    session: PathBuf,
    extension_state: PathBuf,
    journal: PathBuf,
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

// Process-local serialization only. This is not a substitute for the server ownership fence that
// production wiring must hold across processes.
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
    paths: &PersistencePaths,
    session: &SessionSnapshot,
    extension_state: &DurableExtensionStateV1,
) -> Result<(), PersistenceError> {
    persist_checkpoint_with(paths, session, extension_state, |_| Ok(()))
}

fn persist_checkpoint_with<F>(
    paths: &PersistencePaths,
    session: &SessionSnapshot,
    extension_state: &DurableExtensionStateV1,
    mut boundary: F,
) -> Result<(), PersistenceError>
where
    F: FnMut(Boundary) -> std::io::Result<()>,
{
    let _writer = exclusive_writer()?;
    let paths = resolve_paths(paths).map_err(not_published)?;
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
    paths: &PersistencePaths,
) -> Result<StartupRecovery, PersistenceError> {
    let _writer = exclusive_writer()?;
    let paths = resolve_paths(paths).map_err(not_published)?;
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
    let session = resolve_exact_target(&paths.session)?;
    let extension_state = resolve_exact_target(&paths.extension_state)?;
    let journal = resolve_exact_target(&paths.journal)?;
    if session == extension_state || session == journal || extension_state == journal {
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
    Ok(ResolvedPaths {
        session,
        extension_state,
        journal,
        parent,
    })
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

    fn fixture() -> (
        PathBuf,
        PersistencePaths,
        SessionSnapshot,
        DurableExtensionStateV1,
    ) {
        let directory = std::env::temp_dir().join(format!(
            "herdr-two-file-journal-{}-{}",
            std::process::id(),
            secure_suffix().unwrap()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let paths = PersistencePaths {
            session: directory.join("session.json"),
            extension_state: directory.join("extension-state.json"),
            journal: directory.join("session-extension-state.journal.json"),
        };
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
        (directory, paths, session, state)
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
            let (directory, paths, session, state) = fixture();
            let error = persist_checkpoint_with(&paths, &session, &state, |boundary| {
                if boundary == crash_at {
                    Err(std::io::Error::other("simulated crash"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
            assert_eq!(error.outcome, PersistenceOutcome::RecoveryRequired);
            assert_eq!(
                recover_checkpoint_at_startup(&paths).unwrap(),
                StartupRecovery::Recovered
            );
            let session_text = std::fs::read_to_string(&paths.session).unwrap();
            assert!(super::super::snapshot::parse_snapshot(&session_text).is_ok());
            assert!(matches!(
                super::super::extension_state::load(&paths.extension_state),
                super::super::extension_state::ExtensionStateLoad::Loaded(_)
            ));
            assert!(!paths.journal.try_exists().unwrap());
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn fault_after_journal_removal_never_rolls_back_completed_targets() {
        let (directory, paths, session, state) = fixture();
        let error = persist_checkpoint_with(&paths, &session, &state, |boundary| {
            if boundary == Boundary::JournalRemoved {
                Err(std::io::Error::other("simulated post-removal fault"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(error.outcome, PersistenceOutcome::RecoveryRequired);
        assert_eq!(
            recover_checkpoint_at_startup(&paths).unwrap(),
            StartupRecovery::NoJournal
        );
        assert!(paths.session.try_exists().unwrap());
        assert!(paths.extension_state.try_exists().unwrap());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn pre_journal_faults_clean_stages_without_publishing() {
        for crash_at in [Boundary::SessionStaged, Boundary::ExtensionStateStaged] {
            let (directory, paths, session, state) = fixture();
            let error = persist_checkpoint_with(&paths, &session, &state, |boundary| {
                if boundary == crash_at {
                    Err(std::io::Error::other("simulated preparation fault"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
            assert_eq!(error.outcome, PersistenceOutcome::NotPublished);
            assert_eq!(
                recover_checkpoint_at_startup(&paths).unwrap(),
                StartupRecovery::NoJournal
            );
            assert!(std::fs::read_dir(&directory).unwrap().next().is_none());
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn uncertain_journal_replacement_never_cleans_a_reused_source_path() {
        let (directory, paths, session, state) = fixture();
        let _ = persist_checkpoint_with(&paths, &session, &state, |boundary| {
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
        let (directory, paths, session, state) = fixture();
        let _ = persist_checkpoint_with(&paths, &session, &state, |boundary| {
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
            let error = recover_checkpoint_at_startup(&paths).unwrap_err();
            assert_eq!(error.outcome, PersistenceOutcome::RecoveryRequired);
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn advanced_phase_still_repairs_and_verifies_both_targets() {
        let (directory, paths, session, state) = fixture();
        let _ = persist_checkpoint_with(&paths, &session, &state, |boundary| {
            (boundary != Boundary::JournalPrepared)
                .then_some(())
                .ok_or_else(|| std::io::Error::other("crash"))
        });
        let resolved = resolve_paths(&paths).unwrap();
        let mut journal = read_journal(&resolved).unwrap();
        journal.phase = JournalPhase::ReplacingExtensionState;
        publish_journal(&resolved, &journal).unwrap();

        assert_eq!(
            recover_checkpoint_at_startup(&paths).unwrap(),
            StartupRecovery::Recovered
        );
        verify_target(&journal.session).unwrap();
        verify_target(&journal.extension_state).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn late_phase_rejects_missing_or_corrupt_session_after_stage_was_consumed() {
        for replacement in [None, Some(b"corrupt-session".as_slice())] {
            let (directory, paths, session, state) = fixture();
            let _ = persist_checkpoint_with(&paths, &session, &state, |boundary| {
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

            let error = recover_checkpoint_at_startup(&paths).unwrap_err();
            assert_eq!(error.outcome, PersistenceOutcome::RecoveryRequired);
            assert!(paths.journal.try_exists().unwrap());
            assert!(!Path::new(&journal.session.stage_path).try_exists().unwrap());
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn identical_targets_remove_only_matching_bound_stages_before_completion() {
        let (directory, paths, session, state) = fixture();
        let _ = persist_checkpoint_with(&paths, &session, &state, |boundary| {
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
            recover_checkpoint_at_startup(&paths).unwrap(),
            StartupRecovery::Recovered
        );
        verify_target(&journal.session).unwrap();
        verify_target(&journal.extension_state).unwrap();
        assert!(!Path::new(&journal.session.stage_path).try_exists().unwrap());
        assert!(!Path::new(&journal.extension_state.stage_path)
            .try_exists()
            .unwrap());
        assert!(!paths.journal.try_exists().unwrap());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn recovery_rejects_oversized_journal_without_touching_targets() {
        let (directory, paths, _, _) = fixture();
        std::fs::write(&paths.session, b"old-session").unwrap();
        std::fs::write(&paths.extension_state, b"old-state").unwrap();
        std::fs::write(&paths.journal, vec![b'x'; MAX_JOURNAL_BYTES as usize + 1]).unwrap();

        let error = recover_checkpoint_at_startup(&paths).unwrap_err();
        assert_eq!(error.outcome, PersistenceOutcome::RecoveryRequired);
        assert_eq!(std::fs::read(&paths.session).unwrap(), b"old-session");
        assert_eq!(std::fs::read(&paths.extension_state).unwrap(), b"old-state");
        assert!(paths.journal.try_exists().unwrap());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn never_rolls_back_after_first_replacement() {
        let (directory, paths, session, state) = fixture();
        std::fs::write(&paths.session, b"old-session").unwrap();
        std::fs::write(&paths.extension_state, b"old-state").unwrap();
        let error = persist_checkpoint_with(&paths, &session, &state, |boundary| {
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
        recover_checkpoint_at_startup(&paths).unwrap();
        assert_ne!(std::fs::read(&paths.session).unwrap(), b"old-session");
        assert_ne!(std::fs::read(&paths.extension_state).unwrap(), b"old-state");
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
