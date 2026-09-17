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

pub(crate) fn publish_handoff_snapshot_to_path(
    path: &Path,
    snapshot: &SessionSnapshot,
) -> std::io::Result<()> {
    save_to_path(path, snapshot)
}

pub(crate) fn publish_handoff_snapshot(snapshot: &SessionSnapshot) -> std::io::Result<()> {
    publish_handoff_snapshot_to_path(&session_path(), snapshot)
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
        let newer_bytes = format!("{{\"version\":{}}}", SNAPSHOT_VERSION + 1);
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
            version: SNAPSHOT_VERSION,
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
            br#"{"version":4,"workspaces":[{"id":"w1","identity_cwd":"/tmp","tabs":[{"layout":{"Pane":1},"panes":{"1":{"cwd":"/tmp"}},"zoomed":false}]}]}"#,
        )
        .unwrap();

        let StartupSessionLoad::Loaded(snapshot) = load_for_startup_from_path(&path) else {
            panic!("legacy snapshot should migrate");
        };
        assert_eq!(snapshot.version, SNAPSHOT_VERSION);
        let published: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(published["version"], SNAPSHOT_VERSION);
        let terminal_id = published["workspaces"][0]["tabs"][0]["panes"]["1"]["terminal_id"]
            .as_str()
            .expect("published terminal identity");
        assert!(crate::terminal::TerminalId::parse(terminal_id).is_ok());
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
