use super::{registry::*, *};
use std::sync::atomic::{AtomicU64, Ordering};

struct Root(std::path::PathBuf);
impl Root {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vqro-ownership-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        crate::platform::create_private_package_directory(&path).expect("private injected root");
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn entry(pid: u32) -> RegistryEntry {
    RegistryEntry {
        endpoint: "/private/test-api.sock".into(),
        ownership: OwnershipRecord {
            version: 1,
            server: ServerKey {
                boot_id: "boot".into(),
                pid,
                birth: 100,
            },
            epoch: 2,
            complete: true,
            owned: vec![TtyIdentity {
                device: 1,
                inode: 2,
                rdev: 3,
            }],
        },
    }
}

#[test]
fn registry_round_trip_is_bounded_and_cleanup_removes_only_own_incarnation() {
    let root = Root::new();
    let directory = RegistryDirectory::prepare(&root.0).expect("directory");
    let first = entry(42);
    let second = entry(43);
    directory.publish(&first).expect("first atomic record");
    directory.publish(&second).expect("second record");
    assert_eq!(directory.entries().expect("entries").len(), 2);
    directory
        .remove(&first.ownership.server)
        .expect("only first record");
    assert_eq!(directory.entries().expect("remaining"), vec![second]);
}

#[test]
fn missing_runtime_root_is_an_observation_failure_not_an_ownership_failure() {
    let root = Root::new();
    assert!(RegistryDirectory::prepare(&root.0.join("missing")).is_err());
    let mut state = OwnershipState::new(entry(42).ownership.server);
    let change = state.begin_change();
    state.finish_add(
        change,
        Some(TtyIdentity {
            device: 1,
            inode: 2,
            rdev: 3,
        }),
    );
    assert_eq!(
        state.record().owned.len(),
        1,
        "physical ownership continues"
    );
    assert!(
        !state.status().complete,
        "no successful publication acknowledgement"
    );
}

#[test]
fn a_live_unreadable_malformed_or_symlinked_record_is_not_silently_skipped() {
    use std::os::unix::fs::symlink;
    let root = Root::new();
    let directory = RegistryDirectory::prepare(&root.0).expect("directory");
    let first = entry(42);
    directory.publish(&first).expect("record");
    let path = directory.record_path(&first.ownership.server);
    std::fs::write(&path, b"partial").expect("corrupt own private fixture");
    assert!(directory.entries().is_err());
    std::fs::remove_file(&path).expect("remove fixture");
    symlink(root.0.join("absent"), &path).expect("dangling symlink fixture");
    assert!(directory.entries().is_err());
    assert!(
        directory.publish(&first).is_err(),
        "do not overwrite an untrusted existing path"
    );
}

#[test]
fn canonical_record_name_must_match_its_incarnation_and_unknown_fields_reject() {
    let root = Root::new();
    let directory = RegistryDirectory::prepare(&root.0).expect("directory");
    let first = entry(42);
    directory.publish(&first).expect("record");
    let path = directory.record_path(&first.ownership.server);
    let other_path = directory.record_path(&entry(43).ownership.server);
    std::fs::rename(&path, &other_path).expect("mismatched fixture");
    assert!(directory.entries().is_err());
    assert!(parse_entry(br#"{"extra":true}"#).is_err());
    assert!(parse_entry(&vec![b'x'; MAX_RECORD_BYTES + 1]).is_err());
}

#[test]
fn publication_failure_leaves_old_file_but_live_status_detects_its_staleness() {
    let root = Root::new();
    let directory = RegistryDirectory::prepare(&root.0).expect("directory");
    let mut state = OwnershipState::new(entry(42).ownership.server);
    let first = state.begin_change();
    state.finish_add(
        first,
        Some(TtyIdentity {
            device: 1,
            inode: 2,
            rdev: 3,
        }),
    );
    directory
        .publish(&RegistryEntry {
            endpoint: "/private/api.sock".into(),
            ownership: state.record(),
        })
        .expect("R1");
    state.publication_succeeded(state.status().epoch);
    let change = state.begin_change();
    state.finish_add(
        change,
        Some(TtyIdentity {
            device: 1,
            inode: 3,
            rdev: 4,
        }),
    );
    // No second write: represent ENOSPC/permission/lock failure without relying
    // on a root- or filesystem-dependent chmod failure.
    let old = directory.entries().expect("old complete file").remove(0);
    assert!(old.ownership.complete);
    assert!(validate_record(&old.ownership, &state.status(), &state.status().server).is_err());
}

#[test]
fn positively_dead_files_do_not_consume_the_live_server_cap() {
    let root = Root::new();
    let directory = RegistryDirectory::prepare(&root.0).expect("directory");
    for pid in 1000..1065 {
        directory.publish(&entry(pid)).expect("old private fixture");
    }
    let live = entry(5000);
    directory.publish(&live).expect("live fixture");
    let records = directory
        .entries_with_liveness(|key| {
            if key.pid == 5000 {
                ServerLiveness::Live(key.clone())
            } else {
                ServerLiveness::Dead
            }
        })
        .expect("dead files are not live slots");
    assert_eq!(records, vec![live]);
}

#[test]
fn transient_lock_recovery_does_not_need_a_second_lifecycle_change() {
    use std::sync::atomic::AtomicUsize;
    let root = Root::new();
    let directory = RegistryDirectory::prepare(&root.0).expect("directory");
    let first = entry(42);
    directory.publish(&first).expect("R1");
    let lock_path = directory
        .record_path(&first.ownership.server)
        .parent()
        .expect("parent")
        .join(".lock");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .expect("existing private lock");
    lock.lock().expect("hold namespace briefly");
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(25));
        drop(lock);
    });
    let attempts = AtomicUsize::new(0);
    let mut second = first.clone();
    second.ownership.epoch += 1;
    let result = super::publisher::retry_publication(|| {
        attempts.fetch_add(1, Ordering::SeqCst);
        directory.publish(&second)
    });
    release.join().expect("release own lock");
    result.expect("bounded retry recovers without any further ownership signal");
    assert!(attempts.load(Ordering::SeqCst) > 1);
    assert_eq!(directory.entries().expect("R2"), vec![second]);
}

#[test]
fn permanent_lock_retry_has_a_deterministic_attempt_bound_and_fails_closed() {
    let mut attempts = 0;
    let result = super::publisher::retry_publication(|| {
        attempts += 1;
        Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
    });
    assert!(result.is_err());
    assert_eq!(attempts, super::publisher::MAX_PUBLICATION_ATTEMPTS);
}

#[test]
fn failed_exclusive_creation_never_unlinks_a_temporary_file_this_write_did_not_create() {
    let root = Root::new();
    let directory = RegistryDirectory::prepare(&root.0).expect("directory");
    let first = entry(42);
    let target = directory.record_path(&first.ownership.server);
    let collision = target
        .parent()
        .expect("namespace")
        .join(format!(".tmp-{}-41", std::process::id()));
    std::fs::write(&collision, b"belongs to a different publication").expect("private fixture");
    assert!(directory.publish_with_nonce(&first, 41).is_err());
    assert_eq!(
        std::fs::read(&collision).expect("unowned collision must survive"),
        b"belongs to a different publication"
    );
    assert!(!target.exists());
}

#[test]
fn positively_dead_records_are_ignored_but_unprovable_liveness_is_not() {
    let root = Root::new();
    let directory = RegistryDirectory::prepare(&root.0).expect("directory");
    let first = entry(42);
    directory.publish(&first).expect("record");
    std::fs::write(
        directory.record_path(&first.ownership.server),
        b"dead corrupt record",
    )
    .expect("private fixture");
    assert!(directory
        .entries_with_liveness(|_| ServerLiveness::Dead)
        .expect("proved dead")
        .is_empty());
    assert!(directory
        .entries_with_liveness(|_| ServerLiveness::Reused)
        .expect("proved reused")
        .is_empty());
    assert!(directory
        .entries_with_liveness(|_| ServerLiveness::Unknown)
        .is_err());
}
