use super::*;
use crate::persist::snapshot::{layout_fingerprint, parse_snapshot, SessionSnapshot};

struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "vqro-recovery-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn session(&self) -> PathBuf {
        self.0.join("session.json")
    }
    fn copies(&self) -> Vec<(u128, PathBuf)> {
        recovery_files(&self.0.join("session-snapshots")).unwrap_or_default()
    }
    fn write(&self, snapshot: &SessionSnapshot) {
        std::fs::write(self.session(), serde_json::to_vec(snapshot).unwrap()).unwrap();
    }
    fn age(&self) {
        for (_, path) in self.copies() {
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(UNIX_EPOCH))
                .unwrap();
        }
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn snapshot() -> SessionSnapshot {
    parse_snapshot(include_str!(
        "../../tests/fixtures/session/collection-layout-v4.json"
    ))
    .unwrap()
}

#[test]
fn rolling_images_survive_exit_bursts_clear_and_restart() {
    let root = Root::new();
    let original = snapshot();
    crate::persist::io::save_to_path(&root.session(), &original).unwrap();
    let copies = root.copies();
    assert_eq!(copies.len(), 1);
    let bytes = std::fs::read(&copies[0].1).unwrap();
    for i in 0..100 {
        let mut shrinking = original.clone();
        shrinking.workspaces[0].custom_name = Some(format!("exit {i}"));
        crate::persist::io::save_to_path(&root.session(), &shrinking).unwrap();
    }
    crate::persist::io::clear_session_path(&root.session()).unwrap();
    assert!(!root.session().exists());
    assert_eq!(root.copies(), copies);
    assert_eq!(std::fs::read(&copies[0].1).unwrap(), bytes);
    assert_eq!(
        layout_fingerprint(&parse_snapshot(std::str::from_utf8(&bytes).unwrap()).unwrap()),
        layout_fingerprint(&original)
    );
}

#[test]
fn cadence_and_identical_suppression_survive_restart_and_clock_rollback() {
    let root = Root::new();
    let mut image = snapshot();
    root.write(&image);
    preserve_at(&root.session(), SystemTime::now()).unwrap();
    let first = root.copies();
    assert_eq!(first.len(), 1);
    root.age();
    let reordered: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.session()).unwrap()).unwrap();
    std::fs::write(
        root.session(),
        serde_json::to_vec_pretty(&reordered).unwrap(),
    )
    .unwrap();
    preserve_at(&root.session(), SystemTime::now()).unwrap();
    assert_eq!(
        root.copies(),
        first,
        "equivalent layout does not rotate after cadence expires"
    );
    std::fs::File::options()
        .write(true)
        .open(&first[0].1)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new().set_modified(SystemTime::now() + Duration::from_secs(86400)),
        )
        .unwrap();
    image.workspaces[0].custom_name = Some("clock rolled back".into());
    root.write(&image);
    preserve_at(&root.session(), SystemTime::now()).unwrap();
    assert_eq!(root.copies().len(), 2);
    assert!(
        root.copies()[1].0 > first[0].0,
        "creation order cannot reverse"
    );
    image.workspaces[0].custom_name = Some("restart".into());
    root.write(&image);
    preserve_at(&root.session(), SystemTime::now()).unwrap();
    assert_eq!(
        root.copies().len(),
        2,
        "disk mtime maintains cadence without process state"
    );
}

#[test]
fn bounded_pruning_preserves_user_files_symlinks_and_pending_copies() {
    let root = Root::new();
    let directory = root.0.join("session-snapshots");
    std::fs::create_dir(&directory).unwrap();
    let manual = directory.join("my-layout.json");
    let pending = directory.join(
        "session-000000000000000000000000000000000000001-00000000000000000000000000000001.pending",
    );
    std::fs::write(&manual, b"user").unwrap();
    std::fs::write(&pending, b"partial").unwrap();
    for i in 0..LIMIT {
        std::fs::write(
            directory.join(format!("session-{i:039}-{i:032x}.json")),
            b"old",
        )
        .unwrap();
    }
    root.age();
    root.write(&snapshot());
    preserve_at(&root.session(), SystemTime::now()).unwrap();
    assert_eq!(root.copies().len(), LIMIT);
    assert!(!directory
        .join(format!("session-{:039}-{:032x}.json", 0, 0))
        .exists());
    assert_eq!(std::fs::read(manual).unwrap(), b"user");
    assert_eq!(std::fs::read(pending).unwrap(), b"partial");
    #[cfg(unix)]
    {
        let link = directory.join(format!("session-{:039}-{:032x}.json", 0, 999));
        std::os::unix::fs::symlink(root.session(), &link).unwrap();
        assert_eq!(
            root.copies().len(),
            LIMIT,
            "symlinks must not be pruned as owned copies"
        );
        assert!(link.exists());
    }
}

#[test]
fn recovery_failure_never_blocks_primary_save_or_clear() {
    let root = Root::new();
    std::fs::write(root.0.join("session-snapshots"), b"blocked").unwrap();
    crate::persist::io::save_to_path(&root.session(), &snapshot()).unwrap();
    assert!(root.session().exists());
    crate::persist::io::clear_session_path(&root.session()).unwrap();
    assert!(!root.session().exists());
}

#[test]
fn empty_invalid_future_and_missing_images_do_not_rotate_or_become_fallback() {
    let root = Root::new();
    preserve_at(&root.session(), SystemTime::now()).unwrap();
    let mut empty = snapshot();
    empty.workspaces.clear();
    root.write(&empty);
    preserve_at(&root.session(), SystemTime::now()).unwrap();
    std::fs::write(root.session(), b"invalid").unwrap();
    preserve_at(&root.session(), SystemTime::now()).unwrap();
    let mut future = snapshot();
    future.version = 999;
    root.write(&future);
    preserve_at(&root.session(), SystemTime::now()).unwrap();
    assert!(root.copies().is_empty());
    assert!(crate::persist::io::save_to_path(&root.session(), &snapshot()).is_err());
    assert!(crate::persist::io::clear_session_path(&root.session()).is_err());
    assert_eq!(
        std::fs::read(root.session()).unwrap(),
        serde_json::to_vec(&future).unwrap()
    );
}

#[test]
fn oversized_recognized_previous_image_cannot_block_healthy_save_or_clear() {
    let root = Root::new();
    root.write(&snapshot());
    preserve_at(&root.session(), SystemTime::now()).unwrap();
    let previous = root.copies()[0].1.clone();
    let file = std::fs::File::options()
        .write(true)
        .open(&previous)
        .unwrap();
    // Sparse file proves the comparison does not allocate/read the full image.
    file.set_len(super::super::snapshot::MAX_SESSION_SNAPSHOT_BYTES + 1)
        .unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(UNIX_EPOCH))
        .unwrap();
    drop(file);
    assert!(super::super::io::read_bounded_text(
        &previous,
        super::super::snapshot::MAX_SESSION_SNAPSHOT_BYTES
    )
    .is_err());
    let mut next = snapshot();
    next.workspaces[0].custom_name = Some("healthy primary".into());
    super::super::io::save_to_path(&root.session(), &next).unwrap();
    assert_eq!(root.copies().len(), 2);
    assert_eq!(
        parse_snapshot(&std::fs::read_to_string(root.session()).unwrap())
            .unwrap()
            .workspaces[0]
            .custom_name
            .as_deref(),
        Some("healthy primary")
    );
    super::super::io::clear_session_path(&root.session()).unwrap();
    assert!(!root.session().exists());
    assert_eq!(root.copies().len(), 2);
}

#[test]
fn prune_continues_past_an_undeletable_entry() {
    let root = Root::new();
    let locked = root.0.join("directory");
    let removable = root.0.join("old");
    std::fs::create_dir(&locked).unwrap();
    std::fs::write(&removable, b"old").unwrap();
    prune(&[(0, locked.clone()), (1, removable.clone())], 2).unwrap();
    assert!(locked.exists());
    assert!(!removable.exists());
    assert!(prune(&[(0, locked)], 1).is_err());
}
