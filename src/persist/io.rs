use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use tracing::warn;

#[cfg(any(unix, test))]
use super::snapshot::MAX_COLLECTIONS_ACTION_SEQUENCE;
use super::snapshot::{
    parse_history_snapshot, snapshot_file_version, SessionHistorySnapshot, SessionSnapshot,
    MAX_HISTORY_SNAPSHOT_BYTES, MAX_SESSION_SNAPSHOT_BYTES, SNAPSHOT_VERSION,
};

fn session_path() -> PathBuf {
    crate::session::data_dir().join("session.json")
}

fn session_history_path() -> PathBuf {
    crate::session::data_dir().join("session-history.json")
}

const HANDOFF_STAGE_MARKER: &str = ".handoff-stage.";

fn cleanup_stale_handoff_stages(target: &Path) -> std::io::Result<()> {
    let target = resolve_write_target(target)?;
    let Some(parent) = target.parent() else {
        return Ok(());
    };
    let Some(file_name) = target.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let prefix = format!("{file_name}{HANDOFF_STAGE_MARKER}");
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut removed = false;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        if suffix.len() != 32
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_file() {
            std::fs::remove_file(entry.path())?;
            removed = true;
        }
    }
    if removed {
        crate::platform::sync_parent_directory(parent)?;
    }
    Ok(())
}

// Follow symlinks manually so a write through a (possibly dangling) symlink
// lands on the target. `fs::canonicalize` requires the target to exist, which
// excludes the dangling-symlink case stow users hit on the very first save.
fn resolve_write_target(path: &Path) -> std::io::Result<PathBuf> {
    let mut current = path.to_path_buf();
    for _ in 0..16 {
        let meta = match std::fs::symlink_metadata(&current) {
            Ok(meta) => meta,
            Err(_) => return Ok(current),
        };
        if !meta.file_type().is_symlink() {
            return Ok(current);
        }
        let link = std::fs::read_link(&current)?;
        current = if link.is_absolute() {
            link
        } else {
            current
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(link)
        };
    }
    Ok(current)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AtomicSessionSaveOutcome {
    NotPublished,
    PublicationUncertain,
}

#[derive(Debug)]
pub(crate) struct AtomicSessionSaveError {
    pub(crate) source: std::io::Error,
    // No production persistence coordinator inspects the publication outcome yet.
    #[allow(dead_code)]
    pub(crate) outcome: AtomicSessionSaveOutcome,
}

impl std::fmt::Display for AtomicSessionSaveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(formatter)
    }
}

/// Atomically publishes a session snapshot without changing current save behavior.
///
/// The resolved parent must already exist durably and be a trusted private directory governed by
/// an exclusive persistence writer. Production integration additionally requires deterministic
/// create, write, and cleanup fault injection plus coordinator fencing. This contract remains
/// staged without a production caller.
#[allow(dead_code)]
pub(crate) fn save_session_atomically(
    snapshot: &SessionSnapshot,
) -> Result<(), AtomicSessionSaveError> {
    prepare_session_write(&session_path(), snapshot.version).map_err(|source| {
        AtomicSessionSaveError {
            source,
            outcome: AtomicSessionSaveOutcome::NotPublished,
        }
    })?;
    save_session_atomically_to_path_with(
        &session_path(),
        snapshot,
        std::fs::File::sync_all,
        crate::platform::replace_file,
        crate::platform::sync_containing_directory,
    )
}

fn save_session_atomically_to_path_with<SyncTemporary, Replace, SyncDirectory>(
    path: &Path,
    snapshot: &SessionSnapshot,
    sync_temporary: SyncTemporary,
    replace: Replace,
    sync_directory: SyncDirectory,
) -> Result<(), AtomicSessionSaveError>
where
    SyncTemporary: FnOnce(&std::fs::File) -> std::io::Result<()>,
    Replace: FnOnce(&Path, &Path) -> std::io::Result<()>,
    SyncDirectory: FnOnce(&Path) -> std::io::Result<()>,
{
    let not_published = |source| AtomicSessionSaveError {
        source,
        outcome: AtomicSessionSaveOutcome::NotPublished,
    };
    let publication_uncertain = |source| AtomicSessionSaveError {
        source,
        outcome: AtomicSessionSaveOutcome::PublicationUncertain,
    };

    let target = crate::platform::resolve_write_target(path).map_err(not_published)?;
    let parent = target.parent().ok_or_else(|| {
        not_published(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session path has no parent directory",
        ))
    })?;
    let parent_metadata = std::fs::metadata(parent).map_err(not_published)?;
    if !parent_metadata.is_dir() {
        return Err(not_published(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "session parent path is not a directory",
        )));
    }

    let mut json =
        serde_json::to_vec_pretty(snapshot).map_err(|source| not_published(source.into()))?;
    json.push(b'\n');
    if json.len() as u64 > MAX_SESSION_SNAPSHOT_BYTES {
        return Err(not_published(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "session file exceeds its size limit",
        )));
    }

    let mut random = [0_u8; 16];
    crate::platform::fill_secure_random(&mut random).map_err(not_published)?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let temporary_path = target.with_extension(format!("json.atomic.tmp.{suffix}"));
    let mut temporary =
        crate::platform::create_private_state_file(&temporary_path).map_err(not_published)?;
    let preparation = temporary
        .write_all(&json)
        .and_then(|()| sync_temporary(&temporary));
    drop(temporary);
    if let Err(source) = preparation {
        // This pathname cleanup is safe only before replacement is invoked, under the documented
        // trusted-private-parent and exclusive-writer precondition. After invocation, even an
        // error may mean the source was moved and its old name was reused by another writer.
        let _ = std::fs::remove_file(&temporary_path);
        return Err(not_published(source));
    }
    replace(&temporary_path, &target).map_err(publication_uncertain)?;
    sync_directory(&target).map_err(publication_uncertain)
}

pub(super) fn save_to_path(path: &Path, snapshot: &SessionSnapshot) -> std::io::Result<()> {
    prepare_session_write(path, snapshot.version)?;
    let target = resolve_write_target(path)?;
    super::recovery::preserve(&target);
    save_json_to_path(path, snapshot, MAX_SESSION_SNAPSHOT_BYTES)?;
    super::recovery::preserve(&target);
    Ok(())
}

/// A conversion backup is immutable, byte-exact, private, and durable before
/// publication. Failed conversion/publication keeps both it and the old file.
pub(super) fn prepare_session_write(
    path: &Path,
    next_version: u32,
) -> std::io::Result<Option<PathBuf>> {
    let target = resolve_write_target(path)?;
    let content = match read_bounded_text(&target, MAX_SESSION_SNAPSHOT_BYTES) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let old_version = session_write_metadata(&content)?.version;
    if old_version > next_version {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!(
            "downgrade unsupported: session {} is version {old_version}, writer supports {next_version}",
            target.display(),
        )));
    }
    if old_version == next_version {
        return Ok(None);
    }
    backup_conversion(
        &target,
        content.as_bytes(),
        old_version,
        next_version,
        crate::platform::sync_containing_directory,
    )
    .map(Some)
}

fn backup_conversion<SyncDirectory>(
    target: &Path,
    original: &[u8],
    old_version: u32,
    next_version: u32,
    sync_directory: SyncDirectory,
) -> std::io::Result<PathBuf>
where
    SyncDirectory: FnOnce(&Path) -> std::io::Result<()>,
{
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::other("session path has no UTF-8 file name"))?;
    let mut random = [0_u8; 16];
    crate::platform::fill_secure_random(&mut random)?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let backup = target.with_file_name(format!(
        "{file_name}.pre-v{next_version}-v{old_version}.{suffix}.bak"
    ));
    let mut file = crate::platform::create_private_state_file(&backup)?;
    file.write_all(original)?;
    file.sync_all()?;
    drop(file);
    sync_directory(&backup)?;
    tracing::info!(session = %target.display(), backup = %backup.display(),
        old_version, next_version, "backed up session before format conversion");
    Ok(backup)
}

fn save_json_to_path<T: serde::Serialize>(
    path: &Path,
    snapshot: &T,
    maximum: u64,
) -> std::io::Result<()> {
    save_json_to_path_with(
        path,
        snapshot,
        maximum,
        std::fs::File::sync_all,
        crate::platform::durable_replace_file,
    )
}

fn save_json_to_path_with<T: serde::Serialize>(
    path: &Path,
    snapshot: &T,
    maximum: u64,
    sync: impl FnOnce(&std::fs::File) -> std::io::Result<()>,
    replace: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let target = resolve_write_target(path)?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut json = serde_json::to_vec_pretty(snapshot)?;
    json.push(b'\n');
    if json.len() as u64 > maximum {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "session file exceeds its size limit",
        ));
    }
    let mut random = [0_u8; 16];
    crate::platform::fill_secure_random(&mut random)?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let tmp_path = target.with_extension(format!("json.tmp.{suffix}"));
    let mut temporary = crate::platform::create_private_state_file(&tmp_path)?;
    let result = (|| {
        temporary.write_all(&json)?;
        sync(&temporary)?;
        drop(temporary);
        replace(&tmp_path, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

pub(super) fn save_to_paths(
    session_path: &Path,
    history_path: &Path,
    snapshot: &SessionSnapshot,
    history: Option<&SessionHistorySnapshot>,
) -> std::io::Result<()> {
    save_to_path(session_path, snapshot)?;
    if let Some(history) = history {
        let mut history = history.clone();
        history.layout_fingerprint = super::snapshot::layout_fingerprint(snapshot);
        save_json_to_path(history_path, &history, MAX_HISTORY_SNAPSHOT_BYTES)?;
    } else {
        clear_path(history_path)?;
    }
    Ok(())
}

pub(super) fn clear_path(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(all(test, unix))]
pub(crate) fn publish_handoff_snapshot_to_path(
    path: &Path,
    snapshot: &SessionSnapshot,
) -> std::io::Result<()> {
    save_to_path(path, snapshot)
}

/// A synced same-directory handoff snapshot that is not authoritative until
/// ownership commits. Dropping it before commit removes the staging artifact.
#[cfg(unix)]
pub(crate) struct StagedHandoffSnapshot {
    path: PathBuf,
    target: PathBuf,
    published: bool,
}

#[cfg(unix)]
impl StagedHandoffSnapshot {
    pub(crate) fn publish(mut self) -> std::io::Result<()> {
        prepare_session_write(&self.target, SNAPSHOT_VERSION)?;
        super::recovery::preserve(&self.target);
        crate::platform::durable_replace_file(&self.path, &self.target)?;
        self.published = true;
        super::recovery::preserve(&self.target);
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for StagedHandoffSnapshot {
    fn drop(&mut self) {
        if !self.published && std::fs::remove_file(&self.path).is_ok() {
            let _ = crate::platform::sync_containing_directory(&self.path);
        }
    }
}

#[cfg(unix)]
pub(crate) fn stage_handoff_snapshot(
    snapshot: &SessionSnapshot,
) -> std::io::Result<StagedHandoffSnapshot> {
    stage_handoff_snapshot_to_path(&session_path(), snapshot)
}

#[cfg(unix)]
fn stage_handoff_snapshot_to_path(
    target: &Path,
    snapshot: &SessionSnapshot,
) -> std::io::Result<StagedHandoffSnapshot> {
    cleanup_stale_handoff_stages(target)?;
    let target = resolve_write_target(target)?;
    let mut random = [0_u8; 16];
    crate::platform::fill_secure_random(&mut random)?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::other("handoff snapshot path has no UTF-8 file name"))?;
    let path = target.with_file_name(format!("{file_name}{HANDOFF_STAGE_MARKER}{suffix}"));
    save_json_to_path(&path, snapshot, MAX_SESSION_SNAPSHOT_BYTES)?;
    Ok(StagedHandoffSnapshot {
        path,
        target,
        published: false,
    })
}

pub fn save(snapshot: &SessionSnapshot, history: Option<&SessionHistorySnapshot>) {
    let path = session_path();
    let history_path = session_history_path();
    if let Err(err) = save_to_paths(&path, &history_path, snapshot, history) {
        crate::logging::session_save_failed(&path, &err.to_string());
        return;
    }
    crate::logging::session_saved(&path, snapshot.workspaces.len());
}

pub fn clear() {
    let path = session_path();
    if let Err(err) = clear_session_path(&path) {
        crate::logging::session_clear_failed(&path, &err.to_string());
        return;
    }
    clear_history();
    crate::logging::session_cleared(&path);
}

pub(super) fn clear_session_path(path: &Path) -> std::io::Result<()> {
    prepare_session_write(path, SNAPSHOT_VERSION)?;
    let target = resolve_write_target(path)?;
    super::recovery::preserve(&target);
    clear_path(path)
}

pub fn clear_history() {
    let path = session_history_path();
    if let Err(err) = clear_path(&path) {
        crate::logging::session_clear_failed(&path, &err.to_string());
    }
}

pub(crate) enum StartupSessionLoad {
    Missing,
    Loaded(Box<SessionSnapshot>),
    Rejected(String),
}

pub(crate) fn load_for_startup() -> StartupSessionLoad {
    load_for_startup_from_path(&session_path())
}

/// A previous live exporter can omit the action high-water. Consult only this
/// monotonic fact from the existing checkpoint, never its stale layout/identity.
/// Remove when no pre-v8 live sender can be a source.
#[cfg(unix)]
pub(crate) fn durable_collections_action_sequence() -> std::io::Result<u64> {
    checkpoint_collections_action_sequence(&session_path())
}

#[derive(serde::Deserialize)]
struct SessionWriteMetadata {
    version: u32,
    /// Only the live-handoff high-water reader consults this fact; Windows has
    /// no live handoff, so the field exists solely for that reader and its tests.
    #[cfg(any(unix, test))]
    #[serde(default)]
    collections_action_sequence: u64,
}

fn session_write_metadata(content: &str) -> std::io::Result<SessionWriteMetadata> {
    let invalid = |error| std::io::Error::new(std::io::ErrorKind::InvalidData, error);
    // Bound JSON syntax before extracting the two facts. Deserialize's usual
    // unknown-field skip does not enforce the tree reader's depth budget.
    // No layout DTO, legacy conversion or normalization is consulted here.
    let value: serde_json::Value = serde_json::from_str(content).map_err(invalid)?;
    serde_json::from_value(value).map_err(invalid)
}

#[cfg(any(unix, test))]
fn checkpoint_collections_action_sequence(path: &Path) -> std::io::Result<u64> {
    let content = match read_bounded_text(path, MAX_SESSION_SNAPSHOT_BYTES) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let metadata = session_write_metadata(&content)?;
    let diagnostic = if metadata.version > SNAPSHOT_VERSION {
        Some("checkpoint version is newer than this reader")
    } else if metadata.version < 7 && metadata.collections_action_sequence != 0 {
        Some("Collections action sequence cannot appear before snapshot version 7")
    } else if metadata.collections_action_sequence > MAX_COLLECTIONS_ACTION_SEQUENCE {
        Some("Collections action sequence exceeds the supported range")
    } else {
        None
    };
    if let Some(diagnostic) = diagnostic {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "cannot reconcile durable Collections high-water from {}: {diagnostic}",
                path.display()
            ),
        ));
    }
    Ok(metadata.collections_action_sequence)
}

pub(super) fn read_bounded_text(path: &Path, maximum: u64) -> std::io::Result<String> {
    let target = crate::platform::resolve_write_target(path)?;
    let mut file = std::fs::File::open(target)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > maximum {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "session file is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    Read::by_ref(&mut file)
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "session file exceeds its size limit",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

fn load_for_startup_from_path(path: &Path) -> StartupSessionLoad {
    load_for_startup_from_path_with(path, |snapshot| {
        save_json_to_path(path, snapshot, MAX_SESSION_SNAPSHOT_BYTES)
    })
}

fn load_for_startup_from_path_with<Publish>(path: &Path, publish: Publish) -> StartupSessionLoad
where
    Publish: FnOnce(&SessionSnapshot) -> std::io::Result<()>,
{
    if let Err(error) = cleanup_stale_handoff_stages(path) {
        warn!(%error, "failed to clean stale handoff snapshot stages");
    }
    if !path.exists() {
        return StartupSessionLoad::Missing;
    }
    let content = match read_bounded_text(path, MAX_SESSION_SNAPSHOT_BYTES) {
        Ok(content) => content,
        Err(err) => {
            let diagnostic = format!("failed to read existing session file: {err}");
            warn!(%diagnostic);
            return StartupSessionLoad::Rejected(diagnostic);
        }
    };
    if snapshot_file_version(&content).is_some_and(|version| version < SNAPSHOT_VERSION) {
        if let Err(error) = prepare_session_write(path, SNAPSHOT_VERSION) {
            return StartupSessionLoad::Rejected(format!(
                "failed to back up session {} before conversion: {error}",
                path.display()
            ));
        }
    }
    match super::snapshot::parse_snapshot_with_status(&content) {
        Ok((snapshot, normalized)) => {
            if let Err(error) = super::restore::validate_snapshot_admission(&snapshot) {
                let diagnostic = format!("failed to validate existing session file: {error}");
                warn!(%diagnostic);
                return StartupSessionLoad::Rejected(diagnostic);
            }
            if normalized {
                if let Err(error) = publish(&snapshot) {
                    let diagnostic = format!(
                        "failed to durably publish migrated session snapshot before restore: {error}"
                    );
                    warn!(%diagnostic);
                    return StartupSessionLoad::Rejected(diagnostic);
                }
            }
            StartupSessionLoad::Loaded(Box::new(snapshot))
        }
        Err(err) => {
            let diagnostic = if let Some(version) = snapshot_file_version(&content) {
                if version > SNAPSHOT_VERSION {
                    format!(
                        "session file version {version} is newer than supported {SNAPSHOT_VERSION}"
                    )
                } else {
                    format!("failed to convert session {}: {err}", path.display())
                }
            } else {
                format!("failed to parse existing session file: {err}")
            };
            warn!(%diagnostic);
            StartupSessionLoad::Rejected(diagnostic)
        }
    }
}

#[cfg(test)]
pub fn load() -> Option<SessionSnapshot> {
    match load_for_startup() {
        StartupSessionLoad::Loaded(snapshot) => Some(*snapshot),
        StartupSessionLoad::Missing | StartupSessionLoad::Rejected(_) => None,
    }
}

pub(crate) enum StartupHistoryLoad {
    Missing,
    Loaded(SessionHistorySnapshot),
    Rejected(String),
}

pub(crate) fn load_history_for_startup() -> StartupHistoryLoad {
    load_history_for_startup_from_path(&session_history_path())
}

fn load_history_for_startup_from_path(path: &Path) -> StartupHistoryLoad {
    if !path.exists() {
        return StartupHistoryLoad::Missing;
    }
    let content = match read_bounded_text(path, MAX_HISTORY_SNAPSHOT_BYTES) {
        Ok(content) => content,
        Err(err) => {
            return StartupHistoryLoad::Rejected(format!(
                "failed to read existing session history file: {err}"
            ));
        }
    };
    match parse_history_snapshot(&content) {
        Ok(snapshot) => StartupHistoryLoad::Loaded(snapshot),
        Err(err) => StartupHistoryLoad::Rejected(format!(
            "failed to parse existing session history file: {err}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::snapshot::{
        PaneHistorySnapshot, TabHistorySnapshot, WorkspaceHistorySnapshot,
    };

    fn temp_session_path(name: &str) -> PathBuf {
        let unique = format!(
            "herdr-session-tests-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        std::env::temp_dir().join(unique).join("session.json")
    }

    fn temp_session_paths(name: &str) -> (PathBuf, PathBuf) {
        let session = temp_session_path(name);
        let history = session.with_file_name("session-history.json");
        (session, history)
    }

    fn empty_snapshot() -> SessionSnapshot {
        SessionSnapshot {
            delegations: Vec::new(),
            collections_authority: Default::default(),
            collections_action_sequence: 0,
            version: SNAPSHOT_VERSION,
            repositories: Vec::new(),
            space_order: Vec::new(),
            workspaces: vec![],
            active: None,
            selected: 0,
            sidebar_width: Some(26),
            sidebar_section_split: Some(0.5),
            collapsed_space_keys: std::collections::HashSet::new(),
        }
    }

    #[test]
    fn session_v8_conversion_backs_up_exact_legacy_bytes_before_publication() {
        let path = temp_session_path("v8-backup");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut legacy: serde_json::Value = serde_json::from_slice(include_bytes!(
            "../../tests/fixtures/session/hidden-member-handoff-v7.json"
        ))
        .unwrap();
        legacy["workspaces"][0]["tabs"][0]["id"] =
            serde_json::json!("tab_44444444444444444444444444444444");
        let original = serde_json::to_vec_pretty(&legacy).unwrap();
        let old_version = legacy["version"].as_u64().unwrap();
        assert_eq!(old_version, 7, "fence must exercise a real legacy v7 input");
        assert!(
            legacy["workspaces"][0]["tabs"][0]["layout"]["Split"]["first"]
                .get("Collection")
                .is_some()
        );
        std::fs::write(&path, &original).unwrap();
        let StartupSessionLoad::Loaded(snapshot) = load_for_startup_from_path(&path) else {
            panic!("valid legacy session must convert before restore");
        };
        let prefix = format!("session.json.pre-v{SNAPSHOT_VERSION}-v{old_version}.");
        let backups: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|entry| {
                entry
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with(&prefix)
            })
            .collect();
        assert_eq!(
            backups.len(),
            1,
            "conversion must preserve the old session first"
        );
        assert_eq!(std::fs::read(&backups[0]).unwrap(), original);
        assert_eq!(snapshot.version, SNAPSHOT_VERSION);
        let converted = serde_json::to_value(&*snapshot).unwrap();
        let old_tab = &legacy["workspaces"][0]["tabs"][0];
        let new_tab = &converted["workspaces"][0]["tabs"][0];
        for field in ["id", "focused", "root_pane", "zoomed"] {
            assert_eq!(new_tab[field], old_tab[field], "changed {field}");
        }
        assert_eq!(
            new_tab["panes"].as_object().unwrap().len(),
            old_tab["panes"].as_object().unwrap().len()
        );
        for (id, pane) in old_tab["panes"].as_object().unwrap() {
            for (field, value) in pane.as_object().unwrap() {
                assert_eq!(
                    &new_tab["panes"][id][field], value,
                    "pane {id} changed {field}"
                );
            }
        }
        for field in [
            "id",
            "public_pane_numbers",
            "next_public_pane_number",
            "public_tab_numbers",
            "next_public_tab_number",
            "active_tab",
        ] {
            assert_eq!(
                converted["workspaces"][0][field],
                legacy["workspaces"][0][field]
            );
        }
        let old_split = &old_tab["layout"]["Split"];
        let new_split = &new_tab["layout"]["Split"];
        assert_eq!(new_split["ratio"], old_split["ratio"]);
        assert_eq!(new_split["direction"], old_split["direction"]);
        assert_eq!(new_split["second"], old_split["second"]);
        let record = &old_tab["collections"][0];
        let stack = &new_split["first"]["Stack"];
        assert_eq!(stack["id"], record["id"]);
        let expected_children: Vec<_> = record["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pane| serde_json::json!({"Pane": pane}))
            .collect();
        assert_eq!(stack["children"], serde_json::json!(expected_children));
        let expected_selection = record["members"]
            .as_array()
            .unwrap()
            .iter()
            .position(|pane| pane == &record["selected"])
            .unwrap();
        assert_eq!(stack["selected"], serde_json::json!(expected_selection));
        assert!(matches!(
            load_for_startup_from_path(&path),
            StartupSessionLoad::Loaded(_)
        ));
        assert_eq!(std::fs::read(&backups[0]).unwrap(), original);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn legacy_v6_writer_is_admitted_with_members_selection_policy_and_backup() {
        let path = temp_session_path("v6-writer");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut legacy: serde_json::Value = serde_json::from_slice(include_bytes!(
            "../../tests/fixtures/session/hidden-member-handoff-v7.json"
        ))
        .unwrap();
        legacy["version"] = serde_json::json!(super::super::LEGACY_SNAPSHOT_WRITER_VERSION);
        legacy
            .as_object_mut()
            .unwrap()
            .remove("collections_authority");
        legacy["workspaces"][0]["tabs"][0]["id"] =
            serde_json::json!("tab_44444444444444444444444444444444");
        legacy["workspaces"][0]["tabs"][0]["collections"][0]["selected"] =
            serde_json::json!(900002);
        legacy["workspaces"][0]["tabs"][0]["collections"][0]["label"] =
            serde_json::json!("helpers");
        legacy["workspaces"][0]["tabs"][0]["collections"][0]["archived"] =
            serde_json::json!([900001]);
        legacy["workspaces"][0]["tabs"][0]["layout"]["Split"]["ratio"] = serde_json::json!(0.37);
        let original = serde_json::to_vec(&legacy).unwrap();
        assert!(
            legacy.get("collections_authority").is_none(),
            "must exercise real legacy writer admission"
        );
        std::fs::write(&path, &original).unwrap();
        let StartupSessionLoad::Loaded(snapshot) = load_for_startup_from_path(&path) else {
            panic!("legacy v6 writer refused")
        };
        let saved = serde_json::to_value(&*snapshot).unwrap();
        assert_eq!(saved["version"], SNAPSHOT_VERSION);
        let tab = &saved["workspaces"][0]["tabs"][0];
        assert_eq!(tab["layout"]["Split"]["first"]["Stack"]["selected"], 1);
        assert_eq!(
            tab["layout"]["Split"]["first"]["Stack"]["children"],
            serde_json::json!([{"Pane":900001},{"Pane":900002}])
        );
        assert_eq!(
            (tab["layout"]["Split"]["ratio"].as_f64().unwrap() as f32).to_bits(),
            (legacy["workspaces"][0]["tabs"][0]["layout"]["Split"]["ratio"]
                .as_f64()
                .unwrap() as f32)
                .to_bits()
        );
        assert_eq!(
            tab["collections"][0]["archived"],
            serde_json::json!([900001])
        );
        assert_eq!(tab["collections"][0]["label"], "helpers");
        assert!(tab["collections"][0].get("members").is_none());
        assert!(tab["collections"][0].get("selected").is_none());
        let backups: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|entry| {
                entry
                    .extension()
                    .is_some_and(|extension| extension == "bak")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read(&backups[0]).unwrap(), original);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn failed_v8_conversion_and_publication_keep_original_and_readable_backup() {
        for fail_publication in [false, true] {
            let path = temp_session_path("v8-failed-conversion");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut legacy: serde_json::Value = serde_json::from_slice(include_bytes!(
                "../../tests/fixtures/session/hidden-member-handoff-v7.json"
            ))
            .unwrap();
            if !fail_publication {
                // Repairable membership/selection is admitted; an unknown shape
                // cannot be translated and must preserve the original +backup.
                legacy["workspaces"][0]["tabs"][0]["layout"] =
                    serde_json::json!({"UnsupportedLayout":999});
            }
            let original = serde_json::to_vec_pretty(&legacy).unwrap();
            std::fs::write(&path, &original).unwrap();
            let StartupSessionLoad::Rejected(error) =
                load_for_startup_from_path_with(&path, |_| {
                    assert!(fail_publication, "unsupported layout reached publication");
                    Err(std::io::Error::other("injected failure before replacement"))
                })
            else {
                panic!("conversion/publication must fail closed")
            };
            if !fail_publication {
                assert!(error.contains(path.to_str().unwrap()), "{error}");
                assert!(
                    error.contains("UnsupportedLayout") && error.contains("unknown variant"),
                    "{error}"
                );
            }
            assert_eq!(std::fs::read(&path).unwrap(), original);
            let backups: Vec<_> = std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|entry| {
                    entry
                        .extension()
                        .is_some_and(|extension| extension == "bak")
                })
                .collect();
            assert_eq!(backups.len(), 1);
            assert_eq!(std::fs::read(&backups[0]).unwrap(), original);
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn backup_sync_failure_and_downgrade_refuse_before_session_write() {
        let path = temp_session_path("v8-backup-sync");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = br#"{"version":6,"workspaces":[],"active":null,"selected":0}"#;
        std::fs::write(&path, original).unwrap();
        let error = backup_conversion(&path, original, 6, SNAPSHOT_VERSION, |_| {
            Err(std::io::Error::other("injected directory sync failure"))
        })
        .unwrap_err();
        assert!(error.to_string().contains("directory sync"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let mut snapshot = empty_snapshot();
        save_to_path(&path, &snapshot).unwrap();
        let current = std::fs::read(&path).unwrap();
        snapshot.version = SNAPSHOT_VERSION - 1;
        let error = save_to_path(&path, &snapshot).unwrap_err();
        assert!(error.to_string().contains("downgrade unsupported"));
        assert_eq!(std::fs::read(&path).unwrap(), current);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn write_guard_reads_metadata_without_admitting_stale_layout() {
        let path = temp_session_path("header-only-guard");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = br#"{"version":8,"collections_action_sequence":9,"workspaces":"stale"}"#;
        std::fs::write(&path, original).unwrap();
        assert!(prepare_session_write(&path, SNAPSHOT_VERSION)
            .unwrap()
            .is_none());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let future = format!(
            "{{\"version\":{},\"workspaces\":\"stale\"}}",
            SNAPSHOT_VERSION + 1
        );
        std::fs::write(&path, &future).unwrap();
        assert!(prepare_session_write(&path, SNAPSHOT_VERSION)
            .unwrap_err()
            .to_string()
            .contains("downgrade unsupported"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), future);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn checkpoint_high_water_reader_is_bounded_explicit_and_has_no_write_effect() {
        let path = temp_session_path("checkpoint-high-water");
        assert_eq!(checkpoint_collections_action_sequence(&path).unwrap(), 0);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut snapshot = empty_snapshot();
        snapshot.collections_action_sequence = 9;
        for version in [7, SNAPSHOT_VERSION] {
            snapshot.version = version;
            let original = serde_json::to_vec(&snapshot).unwrap();
            std::fs::write(&path, &original).unwrap();
            assert_eq!(checkpoint_collections_action_sequence(&path).unwrap(), 9);
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
        // Only the counter is authoritative here; a stale layout is not admitted.
        let original = br#"{"version":7,"collections_action_sequence":9,"workspaces":[{"tabs":[{"layout":{"Collection":"collection_1"},"collections":[]}]}]}"#;
        std::fs::write(&path, original).unwrap();
        assert_eq!(checkpoint_collections_action_sequence(&path).unwrap(), 9);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        for original in [
            b"not JSON".to_vec(),
            format!(
                "{{\"version\":{},\"collections_action_sequence\":10}}",
                SNAPSHOT_VERSION + 1
            )
            .into_bytes(),
        ] {
            std::fs::write(&path, &original).unwrap();
            assert!(checkpoint_collections_action_sequence(&path).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1,
            "metadata reconciliation must not create another store or rewrite the old file"
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn hostile_v8_tree_hits_json_depth_bound_before_admission_without_partial_restore() {
        let path = temp_session_path("hostile-v8-depth");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Build attacker input iteratively: neither the test nor the reader
        // constructs an unbounded recursive Rust tree before the depth guard.
        let prefix =
            r#"{"Split":{"direction":"Horizontal","ratio":0.5,"first":{"Pane":1},"second":"#;
        let layout = format!("{}{{\"Pane\":2}}{}", prefix.repeat(4096), "}}".repeat(4096));
        let original = format!(
            r#"{{"version":{},"workspaces":[{{"id":"w1","identity_cwd":"/","tabs":[{{"layout":{},"panes":{{}},"zoomed":false}}]}}]}}"#,
            SNAPSHOT_VERSION, layout
        );
        assert!(original.len() < MAX_SESSION_SNAPSHOT_BYTES as usize);
        std::fs::write(&path, &original).unwrap();
        let StartupSessionLoad::Rejected(error) = load_for_startup_from_path(&path) else {
            panic!("hostile v8 tree must be rejected, never partially loaded");
        };
        assert!(error.contains("recursion limit exceeded"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1,
            "a rejected current-format file must not be rewritten or converted"
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn startup_load_distinguishes_every_existing_unrestorable_snapshot() {
        let malformed = temp_session_path("malformed-load");
        std::fs::create_dir_all(malformed.parent().unwrap()).unwrap();
        std::fs::write(&malformed, b"not json").unwrap();
        assert!(matches!(
            load_for_startup_from_path(&malformed),
            StartupSessionLoad::Rejected(_)
        ));
        assert_eq!(std::fs::read(&malformed).unwrap(), b"not json");

        let rejection: serde_json::Value = serde_json::from_slice(include_bytes!(
            "../../docs/next/api/vqro-collections-v1.fixtures/old-reader-rejection-v7.json"
        ))
        .unwrap();
        assert_eq!(rejection["snapshot_version"], 7);
        assert_eq!(rejection["reader_snapshot_version"], 6);
        assert!(
            rejection["snapshot_version"].as_u64().unwrap()
                > rejection["reader_snapshot_version"].as_u64().unwrap()
        );
        assert_eq!(
            rejection["expected_error"],
            "session file version 7 is newer than supported 6"
        );
        let newer = temp_session_path("newer-load");
        std::fs::create_dir_all(newer.parent().unwrap()).unwrap();
        let newer_version = SNAPSHOT_VERSION + 1;
        let newer_bytes =
            format!("{{\"version\":{newer_version},\"future_identity\":{{\"must\":\"survive\"}}}}");
        std::fs::write(&newer, &newer_bytes).unwrap();
        let StartupSessionLoad::Rejected(error) = load_for_startup_from_path(&newer) else {
            panic!("current reader accepted a future snapshot version")
        };
        assert!(error.contains(&format!(
            "session file version {newer_version} is newer than supported {SNAPSHOT_VERSION}"
        )));
        assert_eq!(std::fs::read_to_string(&newer).unwrap(), newer_bytes);

        let unreadable = temp_session_path("unreadable-load");
        std::fs::create_dir_all(&unreadable).unwrap();
        assert!(matches!(
            load_for_startup_from_path(&unreadable),
            StartupSessionLoad::Rejected(_)
        ));

        let _ = std::fs::remove_dir_all(malformed.parent().unwrap());
        let _ = std::fs::remove_dir_all(newer.parent().unwrap());
        let history_malformed =
            temp_session_path("malformed-history").with_file_name("session-history.json");
        std::fs::create_dir_all(history_malformed.parent().unwrap()).unwrap();
        std::fs::write(&history_malformed, b"not history json").unwrap();
        assert!(matches!(
            load_history_for_startup_from_path(&history_malformed),
            StartupHistoryLoad::Rejected(_)
        ));
        assert_eq!(
            std::fs::read(&history_malformed).unwrap(),
            b"not history json"
        );

        let history_newer =
            temp_session_path("newer-history").with_file_name("session-history.json");
        std::fs::create_dir_all(history_newer.parent().unwrap()).unwrap();
        let newer_history_bytes = format!(
            "{{\"version\":{},\"workspaces\":[]}}",
            super::super::snapshot::HISTORY_SNAPSHOT_VERSION + 1
        );
        std::fs::write(&history_newer, &newer_history_bytes).unwrap();
        assert!(matches!(
            load_history_for_startup_from_path(&history_newer),
            StartupHistoryLoad::Rejected(_)
        ));
        assert_eq!(
            std::fs::read_to_string(&history_newer).unwrap(),
            newer_history_bytes
        );

        let history_unreadable =
            temp_session_path("unreadable-history").with_file_name("session-history.json");
        std::fs::create_dir_all(&history_unreadable).unwrap();
        assert!(matches!(
            load_history_for_startup_from_path(&history_unreadable),
            StartupHistoryLoad::Rejected(_)
        ));

        let _ = std::fs::remove_dir_all(malformed.parent().unwrap());
        let _ = std::fs::remove_dir_all(newer.parent().unwrap());
        let _ = std::fs::remove_dir_all(unreadable.parent().unwrap());
        let _ = std::fs::remove_dir_all(history_malformed.parent().unwrap());
        let _ = std::fs::remove_dir_all(history_newer.parent().unwrap());
        let _ = std::fs::remove_dir_all(history_unreadable.parent().unwrap());
    }

    fn history_snapshot(secret: &str) -> SessionHistorySnapshot {
        SessionHistorySnapshot {
            layout_fingerprint: None,
            version: super::super::snapshot::HISTORY_SNAPSHOT_VERSION,
            workspaces: vec![WorkspaceHistorySnapshot {
                tabs: vec![TabHistorySnapshot {
                    panes: std::collections::HashMap::from([(
                        0,
                        PaneHistorySnapshot {
                            ansi: secret.to_string(),
                            lines: 1,
                        },
                    )]),
                }],
            }],
        }
    }

    #[test]
    fn startup_durably_publishes_normalized_v5_before_returning_snapshot() {
        let path = temp_session_path("publish-v5-before-restore");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            br#"{"version":5,"workspaces":[{"id":"w1","identity_cwd":"/tmp","tabs":[{"layout":{"Pane":1},"panes":{"1":{"cwd":"/tmp"}},"zoomed":false}]}]}"#,
        )
        .unwrap();

        let StartupSessionLoad::Loaded(snapshot) = load_for_startup_from_path(&path) else {
            panic!("legacy snapshot should migrate");
        };
        assert_eq!(snapshot.version, SNAPSHOT_VERSION);
        let published: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(published["version"], SNAPSHOT_VERSION);
        let tab_id = published["workspaces"][0]["tabs"][0]["id"]
            .as_str()
            .expect("published tab identity");
        assert!(crate::workspace::TabId::parse(tab_id).is_ok());
        let terminal_id = published["workspaces"][0]["tabs"][0]["panes"]["1"]["terminal_id"]
            .as_str()
            .expect("published terminal identity");
        assert!(crate::terminal::TerminalId::parse(terminal_id).is_ok());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn handoff_staging_is_durable_but_not_authoritative_until_publish() {
        let path = temp_session_path("handoff-stage");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let old = br#"{"version":6,"workspaces":[],"active":null,"selected":0}"#;
        std::fs::write(&path, old).unwrap();
        let staged = stage_handoff_snapshot_to_path(&path, &empty_snapshot()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), old);

        staged.publish().unwrap();
        let published = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            super::super::snapshot::parse_snapshot(&published)
                .unwrap()
                .version,
            SNAPSHOT_VERSION
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dropped_handoff_stage_preserves_authoritative_bytes() {
        let path = temp_session_path("handoff-stage-drop");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let old = b"authoritative-old-bytes";
        std::fs::write(&path, old).unwrap();
        let staged = stage_handoff_snapshot_to_path(&path, &empty_snapshot()).unwrap();
        let staged_path = staged.path.clone();
        drop(staged);
        assert_eq!(std::fs::read(&path).unwrap(), old);
        assert!(!staged_path.exists());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn creating_a_stage_cleans_a_leaked_predecessor() {
        let path = temp_session_path("handoff-stage-replace-stale");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        save_to_path(&path, &empty_snapshot()).unwrap();
        let first = stage_handoff_snapshot_to_path(&path, &empty_snapshot()).unwrap();
        let leaked = first.path.clone();
        std::mem::forget(first);

        let replacement = stage_handoff_snapshot_to_path(&path, &empty_snapshot()).unwrap();
        assert!(!leaked.exists());
        assert!(replacement.path.exists());
        drop(replacement);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn startup_cleans_only_exact_stale_handoff_stage_files() {
        let path = temp_session_path("handoff-stage-crash-cleanup");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        save_to_path(&path, &empty_snapshot()).unwrap();
        let staged = stage_handoff_snapshot_to_path(&path, &empty_snapshot()).unwrap();
        let leaked_stage = staged.path.clone();
        std::mem::forget(staged); // simulate SIGKILL: Drop cannot clean the stage
        let unrelated = path.with_file_name("session.json.handoff-stage.not-ours");
        let authoritative_like = path.with_file_name("session.json.backup");
        let symlink_stage =
            path.with_file_name("session.json.handoff-stage.00000000000000000000000000000000");
        std::fs::write(&unrelated, b"keep").unwrap();
        std::fs::write(&authoritative_like, b"keep").unwrap();
        std::os::unix::fs::symlink(&path, &symlink_stage).unwrap();

        assert!(matches!(
            load_for_startup_from_path(&path),
            StartupSessionLoad::Loaded(_)
        ));
        assert!(!leaked_stage.exists());
        assert!(unrelated.exists());
        assert!(authoritative_like.exists());
        assert!(std::fs::symlink_metadata(&symlink_stage)
            .unwrap()
            .file_type()
            .is_symlink());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn startup_rejects_session_files_over_the_bounded_read_limit() {
        let path = temp_session_path("bounded-read");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_SESSION_SNAPSHOT_BYTES + 1).unwrap();
        assert!(matches!(
            load_for_startup_from_path(&path),
            StartupSessionLoad::Rejected(_)
        ));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    fn atomic_temporary_paths(path: &Path) -> Vec<PathBuf> {
        let prefix = format!(
            "{}.atomic.tmp.",
            path.file_name().unwrap().to_string_lossy()
        );
        std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|candidate| {
                candidate
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
            })
            .collect()
    }

    #[test]
    fn atomic_session_save_requires_an_existing_parent() {
        let path = temp_session_path("atomic-missing-parent");
        assert!(!path.parent().unwrap().exists());

        let error = save_session_atomically_to_path_with(
            &path,
            &empty_snapshot(),
            std::fs::File::sync_all,
            |_, _| panic!("replace must not run without an existing parent"),
            |_| panic!("directory sync must not run without an existing parent"),
        )
        .unwrap_err();

        assert_eq!(error.outcome, AtomicSessionSaveOutcome::NotPublished);
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn atomic_session_save_reports_pre_replace_failure_as_not_published() {
        let path = temp_session_path("atomic-pre-replace-failure");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"authoritative-old-bytes").unwrap();

        let error = save_session_atomically_to_path_with(
            &path,
            &empty_snapshot(),
            |_| Err(std::io::Error::other("injected temporary sync failure")),
            |_, _| panic!("replace must not run after a temporary sync failure"),
            |_| panic!("directory sync must not run before replacement"),
        )
        .unwrap_err();

        assert_eq!(error.outcome, AtomicSessionSaveOutcome::NotPublished);
        assert_eq!(std::fs::read(&path).unwrap(), b"authoritative-old-bytes");
        assert!(atomic_temporary_paths(&path).is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn atomic_session_save_reports_replace_error_as_publication_uncertain() {
        let path = temp_session_path("atomic-replace-failure");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"authoritative-old-bytes").unwrap();

        let error = save_session_atomically_to_path_with(
            &path,
            &empty_snapshot(),
            std::fs::File::sync_all,
            |_, _| Err(std::io::Error::other("injected replace failure")),
            |_| panic!("directory sync must not run after a replace error"),
        )
        .unwrap_err();

        assert_eq!(
            error.outcome,
            AtomicSessionSaveOutcome::PublicationUncertain
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"authoritative-old-bytes");
        assert_eq!(atomic_temporary_paths(&path).len(), 1);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn atomic_session_save_never_unlinks_the_old_temp_name_after_replace_starts() {
        let path = temp_session_path("atomic-replace-moved-then-failed");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"authoritative-old-bytes").unwrap();
        let mut snapshot = empty_snapshot();
        snapshot.selected = 6;

        let error = save_session_atomically_to_path_with(
            &path,
            &snapshot,
            std::fs::File::sync_all,
            |source, destination| {
                crate::platform::replace_file(source, destination)?;
                std::fs::write(source, b"replacement-path-sentinel")?;
                Err(std::io::Error::other("injected late replace error"))
            },
            |_| panic!("directory sync must not run after a replace error"),
        )
        .unwrap_err();

        assert_eq!(
            error.outcome,
            AtomicSessionSaveOutcome::PublicationUncertain
        );
        let published =
            super::super::snapshot::parse_snapshot(&std::fs::read_to_string(&path).unwrap())
                .unwrap();
        assert_eq!(published.selected, 6);
        let temporary_paths = atomic_temporary_paths(&path);
        assert_eq!(temporary_paths.len(), 1);
        assert_eq!(
            std::fs::read(&temporary_paths[0]).unwrap(),
            b"replacement-path-sentinel"
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn atomic_session_save_reports_post_replace_sync_failure_as_uncertain() {
        let path = temp_session_path("atomic-directory-sync-failure");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"authoritative-old-bytes").unwrap();
        let mut snapshot = empty_snapshot();
        snapshot.selected = 7;

        let error = save_session_atomically_to_path_with(
            &path,
            &snapshot,
            std::fs::File::sync_all,
            crate::platform::replace_file,
            |_| Err(std::io::Error::other("injected directory sync failure")),
        )
        .unwrap_err();

        assert_eq!(
            error.outcome,
            AtomicSessionSaveOutcome::PublicationUncertain
        );
        let published =
            super::super::snapshot::parse_snapshot(&std::fs::read_to_string(&path).unwrap())
                .unwrap();
        assert_eq!(published.selected, 7);
        assert!(atomic_temporary_paths(&path).is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn atomic_session_save_succeeds_and_cleans_its_temporary_file() {
        let path = temp_session_path("atomic-success");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"authoritative-old-bytes").unwrap();
        let mut snapshot = empty_snapshot();
        snapshot.selected = 9;

        save_session_atomically_to_path_with(
            &path,
            &snapshot,
            std::fs::File::sync_all,
            crate::platform::replace_file,
            crate::platform::sync_containing_directory,
        )
        .unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.ends_with(b"\n"));
        let published =
            super::super::snapshot::parse_snapshot(std::str::from_utf8(&bytes).unwrap()).unwrap();
        assert_eq!(published.selected, 9);
        assert!(atomic_temporary_paths(&path).is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_session_save_preserves_existing_symlink() {
        let target = temp_session_path("atomic-symlink-target");
        let link = target.with_file_name("atomic-link.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"authoritative-old-bytes").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let mut snapshot = empty_snapshot();
        snapshot.selected = 11;

        save_session_atomically_to_path_with(
            &link,
            &snapshot,
            std::fs::File::sync_all,
            crate::platform::replace_file,
            crate::platform::sync_containing_directory,
        )
        .unwrap();

        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        let published =
            super::super::snapshot::parse_snapshot(&std::fs::read_to_string(&target).unwrap())
                .unwrap();
        assert_eq!(published.selected, 11);
        assert!(atomic_temporary_paths(&target).is_empty());
        std::fs::remove_dir_all(target.parent().unwrap()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_session_save_preserves_relative_and_dangling_symlinks() {
        let directory = temp_session_path("atomic-relative-symlinks")
            .parent()
            .unwrap()
            .to_path_buf();
        std::fs::create_dir_all(&directory).unwrap();
        let relative_target = directory.join("relative-target.json");
        let relative_link = directory.join("relative-link.json");
        std::fs::write(&relative_target, b"old-relative-bytes").unwrap();
        std::os::unix::fs::symlink("relative-target.json", &relative_link).unwrap();
        let mut relative_snapshot = empty_snapshot();
        relative_snapshot.selected = 12;

        save_session_atomically_to_path_with(
            &relative_link,
            &relative_snapshot,
            std::fs::File::sync_all,
            crate::platform::replace_file,
            crate::platform::sync_containing_directory,
        )
        .unwrap();

        let dangling_target = directory.join("new-target.json");
        let dangling_link = directory.join("dangling-link.json");
        std::os::unix::fs::symlink("new-target.json", &dangling_link).unwrap();
        let mut dangling_snapshot = empty_snapshot();
        dangling_snapshot.selected = 13;
        save_session_atomically_to_path_with(
            &dangling_link,
            &dangling_snapshot,
            std::fs::File::sync_all,
            crate::platform::replace_file,
            crate::platform::sync_containing_directory,
        )
        .unwrap();

        assert!(std::fs::symlink_metadata(&relative_link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(std::fs::symlink_metadata(&dangling_link)
            .unwrap()
            .file_type()
            .is_symlink());
        let relative = super::super::snapshot::parse_snapshot(
            &std::fs::read_to_string(&relative_target).unwrap(),
        )
        .unwrap();
        let dangling = super::super::snapshot::parse_snapshot(
            &std::fs::read_to_string(&dangling_target).unwrap(),
        )
        .unwrap();
        assert_eq!(relative.selected, 12);
        assert_eq!(dangling.selected, 13);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn atomic_session_save_rejects_symlink_cycles_and_excessive_depth() {
        let directory = temp_session_path("atomic-symlink-depth")
            .parent()
            .unwrap()
            .to_path_buf();
        std::fs::create_dir_all(&directory).unwrap();
        let cycle_a = directory.join("cycle-a.json");
        let cycle_b = directory.join("cycle-b.json");
        std::os::unix::fs::symlink("cycle-b.json", &cycle_a).unwrap();
        std::os::unix::fs::symlink("cycle-a.json", &cycle_b).unwrap();

        let cycle_error = save_session_atomically_to_path_with(
            &cycle_a,
            &empty_snapshot(),
            std::fs::File::sync_all,
            |_, _| panic!("replace must not run for a symlink cycle"),
            |_| panic!("directory sync must not run for a symlink cycle"),
        )
        .unwrap_err();
        assert_eq!(cycle_error.outcome, AtomicSessionSaveOutcome::NotPublished);

        for index in 0..=40 {
            let link = directory.join(format!("depth-{index}.json"));
            let target = if index == 40 {
                "depth-target.json".to_string()
            } else {
                format!("depth-{}.json", index + 1)
            };
            std::os::unix::fs::symlink(target, link).unwrap();
        }
        let depth_error = save_session_atomically_to_path_with(
            &directory.join("depth-0.json"),
            &empty_snapshot(),
            std::fs::File::sync_all,
            |_, _| panic!("replace must not run for excessive symlink depth"),
            |_| panic!("directory sync must not run for excessive symlink depth"),
        )
        .unwrap_err();
        assert_eq!(depth_error.outcome, AtomicSessionSaveOutcome::NotPublished);
        assert!(!directory.join("depth-target.json").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn ordinary_windows_replacement_failure_preserves_primary_and_history_then_retries() {
        use std::os::windows::fs::OpenOptionsExt;
        let (session, history) = temp_session_paths("windows-replacement");
        save_to_paths(
            &session,
            &history,
            &empty_snapshot(),
            Some(&history_snapshot("old history")),
        )
        .unwrap();
        for path in [&session, &history] {
            let original = std::fs::read(path).unwrap();
            let locked = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ)
                .open(path)
                .unwrap();
            let save = || {
                if path == &session {
                    let mut next = empty_snapshot();
                    next.selected = 7;
                    save_to_path(path, &next)
                } else {
                    save_json_to_path(
                        path,
                        &history_snapshot("new history"),
                        MAX_HISTORY_SNAPSHOT_BYTES,
                    )
                }
            };
            assert!(save().is_err());
            assert_eq!(std::fs::read(path).unwrap(), original);
            drop(locked);
            save().unwrap();
        }
        assert_eq!(
            super::super::snapshot::parse_snapshot(&std::fs::read_to_string(&session).unwrap())
                .unwrap()
                .selected,
            7
        );
        assert!(std::fs::read_to_string(&history)
            .unwrap()
            .contains("new history"));
        assert!(std::fs::read_dir(session.parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp.")));
        std::fs::remove_dir_all(session.parent().unwrap()).unwrap();
    }

    #[test]
    fn ordinary_writer_syncs_before_replace_and_keeps_primary_on_faults() {
        let (path, _) = temp_session_paths("ordinary-faults");
        let original = empty_snapshot();
        save_to_path(&path, &original).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let mut next = original.clone();
        next.selected = 42;
        for fail_sync in [true, false] {
            let steps = std::cell::RefCell::new(Vec::new());
            let result = save_json_to_path_with(
                &path,
                &next,
                MAX_SESSION_SNAPSHOT_BYTES,
                |file| {
                    steps.borrow_mut().push("sync");
                    assert!(file.metadata()?.len() > 0);
                    if fail_sync {
                        Err(std::io::Error::other("sync fault"))
                    } else {
                        file.sync_all()
                    }
                },
                |_, _| {
                    steps.borrow_mut().push("replace");
                    Err(std::io::Error::other("replace fault before publication"))
                },
            );
            assert!(result.is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(
                *steps.borrow(),
                if fail_sync {
                    vec!["sync"]
                } else {
                    vec!["sync", "replace"]
                }
            );
            assert!(std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".tmp.")));
        }
        save_to_path(&path, &next).unwrap();
        assert_eq!(
            super::super::snapshot::parse_snapshot(&std::fs::read_to_string(&path).unwrap())
                .unwrap()
                .selected,
            42
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn save_to_paths_writes_pane_history_only_to_history_file() {
        let (session_path, history_path) = temp_session_paths("split-history");

        save_to_paths(
            &session_path,
            &history_path,
            &empty_snapshot(),
            Some(&history_snapshot("split-secret")),
        )
        .unwrap();

        let session = std::fs::read_to_string(&session_path).unwrap();
        let history = std::fs::read_to_string(&history_path).unwrap();
        assert!(!session.contains("split-secret"));
        assert!(!session.contains("history"));
        assert!(history.contains("split-secret"));
    }

    #[test]
    fn save_to_paths_removes_stale_history_when_history_is_disabled() {
        let (session_path, history_path) = temp_session_paths("clear-history");
        save_to_paths(
            &session_path,
            &history_path,
            &empty_snapshot(),
            Some(&history_snapshot("stale-secret")),
        )
        .unwrap();

        save_to_paths(&session_path, &history_path, &empty_snapshot(), None).unwrap();

        assert!(session_path.exists());
        assert!(!history_path.exists());
    }

    #[test]
    fn clear_path_removes_existing_session_file() {
        let path = temp_session_path("clear-existing");
        save_to_path(&path, &empty_snapshot()).unwrap();

        clear_path(&path).unwrap();

        assert!(!path.exists());
    }

    #[test]
    fn clear_path_ignores_missing_session_file() {
        let path = temp_session_path("clear-missing");

        clear_path(&path).unwrap();

        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn save_to_path_preserves_existing_symlink() {
        let target = temp_session_path("symlink-target");
        let link = target.with_file_name("link.json");
        save_to_path(&target, &empty_snapshot()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let mut snap = empty_snapshot();
        snap.selected = 7;
        save_to_path(&link, &snap).unwrap();

        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        let parsed =
            super::super::snapshot::parse_snapshot(&std::fs::read_to_string(&target).unwrap())
                .unwrap();
        assert_eq!(parsed.selected, 7);
    }

    #[cfg(unix)]
    #[test]
    fn save_to_path_writes_through_dangling_symlink() {
        let target = temp_session_path("dangling-target");
        let link = target.with_file_name("link.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        save_to_path(&link, &empty_snapshot()).unwrap();

        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn save_to_path_resolves_relative_symlink() {
        let session = temp_session_path("relative-symlink");
        let dir = session.parent().unwrap();
        std::fs::create_dir_all(dir).unwrap();
        let target = dir.join("real.json");
        let link = dir.join("link.json");
        std::os::unix::fs::symlink("real.json", &link).unwrap();

        save_to_path(&link, &empty_snapshot()).unwrap();

        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(target.exists());
    }
}
