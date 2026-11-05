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
