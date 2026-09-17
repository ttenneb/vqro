use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use tracing::warn;

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
    // Phase 1 has no production caller to inspect the publication outcome yet.
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
/// create, write, and cleanup fault injection plus coordinator fencing. Phase 1 stages this
/// contract without a production caller.
#[allow(dead_code)]
pub(crate) fn save_session_atomically(
    snapshot: &SessionSnapshot,
) -> Result<(), AtomicSessionSaveError> {
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
    save_json_to_path(path, snapshot, MAX_SESSION_SNAPSHOT_BYTES)
}

fn save_json_to_path<T: serde::Serialize>(
    path: &Path,
    snapshot: &T,
    maximum: u64,
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
        temporary.sync_all()?;
        drop(temporary);
        crate::platform::durable_replace_file(&tmp_path, &target)
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
        save_json_to_path(history_path, history, MAX_HISTORY_SNAPSHOT_BYTES)?;
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

#[cfg(test)]
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
        crate::platform::durable_replace_file(&self.path, &self.target)?;
        self.published = true;
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
    save_to_path(&path, snapshot)?;
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
    if let Err(err) = clear_path(&path) {
        crate::logging::session_clear_failed(&path, &err.to_string());
        return;
    }
    clear_history();
    crate::logging::session_cleared(&path);
}

pub fn clear_history() {
    let path = session_history_path();
    if let Err(err) = clear_path(&path) {
        crate::logging::session_clear_failed(&path, &err.to_string());
    }
}

pub(crate) enum StartupSessionLoad {
    Missing,
    Loaded(SessionSnapshot),
    Rejected(String),
}

pub(crate) fn load_for_startup() -> StartupSessionLoad {
    load_for_startup_from_path(&session_path())
}

fn read_bounded_text(path: &Path, maximum: u64) -> std::io::Result<String> {
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
    match super::snapshot::parse_snapshot_with_status(&content) {
        Ok((snapshot, normalized)) => {
            if let Err(error) = super::restore::validate_snapshot_admission(&snapshot) {
                let diagnostic = format!("failed to validate existing session file: {error}");
                warn!(%diagnostic);
                return StartupSessionLoad::Rejected(diagnostic);
            }
            if normalized {
                if let Err(error) = save_to_path(path, &snapshot) {
                    let diagnostic = format!(
                        "failed to durably publish migrated session snapshot before restore: {error}"
                    );
                    warn!(%diagnostic);
                    return StartupSessionLoad::Rejected(diagnostic);
                }
            }
            StartupSessionLoad::Loaded(snapshot)
        }
        Err(err) => {
            let diagnostic = if let Some(version) = snapshot_file_version(&content) {
                if version > SNAPSHOT_VERSION {
                    format!(
                        "session file version {version} is newer than supported {SNAPSHOT_VERSION}"
                    )
                } else {
                    format!("failed to parse existing session file: {err}")
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
        StartupSessionLoad::Loaded(snapshot) => Some(snapshot),
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
    fn startup_load_distinguishes_every_existing_unrestorable_snapshot() {
        let malformed = temp_session_path("malformed-load");
        std::fs::create_dir_all(malformed.parent().unwrap()).unwrap();
        std::fs::write(&malformed, b"not json").unwrap();
        assert!(matches!(
            load_for_startup_from_path(&malformed),
            StartupSessionLoad::Rejected(_)
        ));
        assert_eq!(std::fs::read(&malformed).unwrap(), b"not json");

        let newer = temp_session_path("newer-load");
        std::fs::create_dir_all(newer.parent().unwrap()).unwrap();
        let newer_bytes = format!(
            "{{\"version\":{},\"future_identity\":{{\"must\":\"survive\"}}}}",
            SNAPSHOT_VERSION + 1
        );
        std::fs::write(&newer, &newer_bytes).unwrap();
        assert!(matches!(
            load_for_startup_from_path(&newer),
            StartupSessionLoad::Rejected(_)
        ));
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
        let old = b"authoritative-old-bytes";
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
