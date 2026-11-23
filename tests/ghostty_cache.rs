//! Cache tests use tiny fake native artifacts; no Zig, network, or live services.
#[path = "../build/ghostty_cache.rs"]
mod ghostty_cache;

use ghostty_cache::{Inputs, source_digest, verify};
use std::fs;
use std::path::Path;
use std::process::Command;

fn inputs() -> Inputs {
    Inputs {
        schema: ghostty_cache::SCHEMA,
        source: "vendor tree sha256".into(),
        target: "x86_64-unknown-linux-gnu".into(),
        optimize: "ReleaseFast".into(),
        zig_version: "0.17.1".into(),
        flags: vec![
            "build".into(),
            "-Dsimd=true".into(),
            "-Dvt-features=-all,+color".into(),
        ],
    }
}

fn build(prefix: &Path, _work: &Path) -> std::io::Result<()> {
    fs::create_dir_all(prefix.join("lib"))?;
    fs::write(prefix.join("lib/native.a"), b"fake archive")
}

const REQUIRED: &[&str] = &["lib/native.a"];

fn ensure(
    root: &Path,
    input: &Inputs,
    required: &[&str],
    build: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<ghostty_cache::Entry> {
    ghostty_cache::ensure(
        root,
        input,
        required,
        ghostty_cache::DEFAULT_KEEP,
        build,
        |_| Ok(()),
    )
}

#[test]
fn every_input_changes_key_and_field_boundaries_are_unambiguous() {
    let original = inputs();
    let mut variants = Vec::new();
    let mut changed = original.clone();
    changed.schema += 1;
    variants.push(changed);
    let mut changed = original.clone();
    changed.source.push('x');
    variants.push(changed);
    let mut changed = original.clone();
    changed.target.push('x');
    variants.push(changed);
    let mut changed = original.clone();
    changed.optimize.push('x');
    variants.push(changed);
    let mut changed = original.clone();
    changed.zig_version.push('x');
    variants.push(changed);
    for index in 0..original.flags.len() {
        let mut changed = original.clone();
        changed.flags[index].push('x');
        variants.push(changed);
    }
    let mut changed = original.clone();
    changed.flags.push("new flag".into());
    variants.push(changed);
    for changed in variants {
        assert_ne!(original.key(), changed.key());
    }
    let mut a = original.clone();
    a.flags = vec!["ab".into(), "c".into()];
    let mut b = original;
    b.flags = vec!["a".into(), "bc".into()];
    assert_ne!(a.key(), b.key());
}

#[test]
fn actual_source_contents_not_checkout_path_determine_identity() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    for root in [a.path(), b.path()] {
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/main.zig"), b"source").unwrap();
    }
    let original = source_digest(a.path()).unwrap();
    assert_eq!(original, source_digest(b.path()).unwrap());
    for ignored in ["zig-out", ".zig-cache", "zig-cache", "zig-pkg"] {
        fs::create_dir(a.path().join(ignored)).unwrap();
        fs::write(a.path().join(ignored).join("noise"), b"ignored").unwrap();
    }
    assert_eq!(original, source_digest(a.path()).unwrap());
    fs::write(a.path().join("src/main.zig"), b"dirty source").unwrap();
    assert_ne!(original, source_digest(a.path()).unwrap());
    fs::write(b.path().join("added.zig"), b"new input").unwrap();
    assert_ne!(original, source_digest(b.path()).unwrap());
}

#[test]
fn source_digest_matches_checked_in_vendor_manifest() {
    use sha2::{Digest, Sha256};
    let vendored = Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/libghostty-vt");
    assert_eq!(
        source_digest(&vendored).unwrap(),
        format!(
            "{:x}",
            Sha256::digest(include_bytes!("../vendor/libghostty-vt.sha256"))
        )
    );
}

#[cfg(unix)]
#[test]
fn literal_backslashes_do_not_alias_path_separators() {
    let root = tempfile::tempdir().unwrap();
    let result = ensure(root.path(), &inputs(), REQUIRED, build).unwrap();
    fs::write(result.path.join("lib\\native.a"), b"fake archive").unwrap();
    assert!(verify(&result.path, &inputs(), REQUIRED).is_err());
}

#[test]
fn manifest_and_every_artifact_are_required_and_verified() {
    let root = tempfile::tempdir().unwrap();
    let input = inputs();
    let result = ensure(root.path(), &input, REQUIRED, build).unwrap();
    assert!(!result.reused);
    let entry = result.path;
    assert!(verify(&entry, &input, REQUIRED).is_ok());
    assert!(verify(&entry, &input, &["lib/missing.a"]).is_err());
    let mut changed = input.clone();
    changed.zig_version.push('x');
    assert!(verify(&entry, &changed, REQUIRED).is_err());
    let archive = entry.join("lib/native.a");
    fs::write(&archive, b"corrupt").unwrap();
    assert!(verify(&entry, &input, REQUIRED).is_err());
    let rebuilt = ensure(root.path(), &input, REQUIRED, build).unwrap();
    assert!(!rebuilt.reused);
    fs::remove_file(archive).unwrap();
    assert!(verify(&entry, &input, REQUIRED).is_err());
    ensure(root.path(), &input, REQUIRED, build).unwrap();
    fs::write(entry.join("unrecorded"), b"extra artifact").unwrap();
    assert!(verify(&entry, &input, REQUIRED).is_err());
    ensure(root.path(), &input, REQUIRED, build).unwrap();
    fs::write(entry.join("manifest.json"), b"{}").unwrap();
    assert!(verify(&entry, &input, REQUIRED).is_err());
    ensure(root.path(), &input, REQUIRED, build).unwrap();
    fs::remove_file(entry.join("manifest.json")).unwrap();
    assert!(verify(&entry, &input, REQUIRED).is_err());
}

#[test]
fn partial_entry_and_failed_build_are_never_published() {
    let root = tempfile::tempdir().unwrap();
    let input = inputs();
    let entry = root.path().join(input.key());
    fs::create_dir(&entry).unwrap();
    fs::write(entry.join("partial"), b"interrupted build").unwrap();
    assert!(verify(&entry, &input, REQUIRED).is_err());
    assert!(
        ensure(root.path(), &input, REQUIRED, |_, _| {
            Err(std::io::Error::other("failed build"))
        })
        .is_err()
    );
    assert!(!entry.exists());
    assert!(ensure(root.path(), &input, REQUIRED, |_, _| Ok(())).is_err());
    assert!(!entry.exists());
    assert!(!ensure(root.path(), &input, REQUIRED, build).unwrap().reused);
}

#[test]
fn interrupted_child() {
    let Some(root) = std::env::var_os("GHOSTTY_CACHE_INTERRUPTED_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    ensure(root, &inputs(), REQUIRED, |_, work| {
        fs::write(work.join("unfinished.o"), b"interrupted Zig work")?;
        fs::write(
            root.join("abandoned-stage"),
            work.parent().unwrap().to_str().unwrap(),
        )?;
        // Like SIGKILL, process exit bypasses TempDir's destructor but releases the OS lock.
        std::process::exit(0);
    })
    .unwrap();
    panic!("child must exit while building");
}

#[test]
fn interrupted_build_stage_is_removed_before_rebuilding() {
    let root = tempfile::tempdir().unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "interrupted_child"])
        .env_clear()
        .env("GHOSTTY_CACHE_INTERRUPTED_ROOT", root.path())
        .status()
        .unwrap();
    assert!(child.success());
    let stage = fs::read_to_string(root.path().join("abandoned-stage")).unwrap();
    assert!(Path::new(&stage).join("zig-cache/unfinished.o").exists());
    ensure(root.path(), &inputs(), REQUIRED, |prefix, work| {
        assert!(
            !Path::new(&stage).exists(),
            "abandoned stage must be gone before a new build"
        );
        build(prefix, work)
    })
    .unwrap();
}

#[test]
fn cache_hit_removes_only_its_own_keys_stale_stage_directories() {
    let root = tempfile::tempdir().unwrap();
    let input = inputs();
    ensure(root.path(), &input, REQUIRED, build).unwrap();
    let stale = root.path().join(format!(".{}-abandoned", input.key()));
    fs::create_dir_all(stale.join("zig-cache")).unwrap();
    fs::write(stale.join("zig-cache/unfinished.o"), b"interrupted work").unwrap();
    let mut other = input.clone();
    other.target.push_str("-different");
    let active_other = root.path().join(format!(".{}-active", other.key()));
    fs::create_dir(&active_other).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        &active_other,
        root.path().join(format!(".{}-symlink", input.key())),
    )
    .unwrap();
    let unrelated = root.path().join(format!(".{}-file", input.key()));
    fs::write(&unrelated, b"not a stage directory").unwrap();
    assert!(
        ensure(root.path(), &input, REQUIRED, |_, _| panic!(
            "cache hit must not build"
        ))
        .unwrap()
        .reused
    );
    assert!(!stale.exists());
    assert!(active_other.is_dir());
    assert!(unrelated.is_file());
}

#[test]
fn second_checkout_reuses_but_key_mismatch_builds_again() {
    let root = tempfile::tempdir().unwrap();
    let input = inputs();
    let first = ensure(root.path(), &input, REQUIRED, build).unwrap();
    let second = ensure(root.path(), &input, REQUIRED, |_, _| {
        panic!("must skip build")
    })
    .unwrap();
    assert!(second.reused);
    assert_eq!(first.path, second.path);
    let mut changed = input;
    changed.target = "aarch64-unknown-linux-gnu".into();
    let other = ensure(root.path(), &changed, REQUIRED, build).unwrap();
    assert!(!other.reused);
    assert_ne!(first.path, other.path);
}

fn numbered_input(index: usize) -> Inputs {
    let mut input = inputs();
    input.zig_version = format!("test-{index}");
    input
}

fn age_entry(path: &Path, index: u64) {
    fs::OpenOptions::new()
        .write(true)
        .open(path.join("manifest.json"))
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(index + 1))
        .unwrap();
}

#[test]
fn seventh_key_evicts_least_recently_used_not_least_recently_built() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..6 {
        let entry = ensure(root.path(), &numbered_input(index), REQUIRED, build).unwrap();
        age_entry(&entry.path, index as u64);
    }
    assert!(
        ensure(root.path(), &numbered_input(0), REQUIRED, build)
            .unwrap()
            .reused
    );
    ensure(root.path(), &numbered_input(6), REQUIRED, build).unwrap();
    assert!(root.path().join(numbered_input(0).key()).exists());
    assert!(!root.path().join(numbered_input(1).key()).exists());
    for index in 2..7 {
        assert!(root.path().join(numbered_input(index).key()).exists());
    }
}

#[test]
fn locked_eviction_candidate_is_skipped_then_pruned_on_next_use() {
    let root = tempfile::tempdir().unwrap();
    for index in 0..6 {
        let entry = ensure(root.path(), &numbered_input(index), REQUIRED, build).unwrap();
        age_entry(&entry.path, index as u64);
    }
    let oldest = numbered_input(0).key();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join(format!("{oldest}.lock")))
        .unwrap();
    lock.lock().unwrap();
    ensure(root.path(), &numbered_input(6), REQUIRED, build).unwrap();
    assert!(root.path().join(&oldest).exists());
    drop(lock);
    ensure(root.path(), &numbered_input(6), REQUIRED, build).unwrap();
    assert!(!root.path().join(oldest).exists());
}

#[test]
fn retention_is_configurable_and_zero_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    assert!(ghostty_cache::ensure(root.path(), &inputs(), REQUIRED, 0, build, |_| Ok(())).is_err());
    for index in 0..4 {
        let entry = ghostty_cache::ensure(
            root.path(),
            &numbered_input(index),
            REQUIRED,
            2,
            build,
            |_| Ok(()),
        )
        .unwrap();
        age_entry(&entry.path, index as u64);
    }
    assert!(!root.path().join(numbered_input(0).key()).exists());
    assert!(!root.path().join(numbered_input(1).key()).exists());
    assert!(root.path().join(numbered_input(2).key()).exists());
    assert!(root.path().join(numbered_input(3).key()).exists());
}

#[test]
fn verified_private_link_input_survives_shared_eviction_and_repairs_corruption() {
    let root = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let input = inputs();
    let snapshot = |path: &Path| ghostty_cache::materialize(path, REQUIRED, out.path());
    let entry = ghostty_cache::ensure(root.path(), &input, REQUIRED, 1, build, snapshot).unwrap();
    let archive = out.path().join("lib/native.a");
    let unchanged = fs::metadata(&archive).unwrap().modified().unwrap();
    ghostty_cache::ensure(root.path(), &input, REQUIRED, 1, build, snapshot).unwrap();
    assert_eq!(
        fs::metadata(&archive).unwrap().modified().unwrap(),
        unchanged
    );
    fs::write(&archive, b"corrupt private copy").unwrap();
    ghostty_cache::ensure(root.path(), &input, REQUIRED, 1, build, snapshot).unwrap();
    assert_eq!(fs::read(&archive).unwrap(), b"fake archive");
    age_entry(&entry.path, 0);
    ghostty_cache::ensure(root.path(), &numbered_input(1), REQUIRED, 1, build, |_| {
        Ok(())
    })
    .unwrap();
    assert!(!entry.path.exists());
    assert_eq!(
        fs::read(&archive).unwrap(),
        b"fake archive",
        "link input must not depend on shared lifetime"
    );
}

#[test]
fn snapshot_bytes_must_match_manifest_before_publication() {
    let root = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let result = ghostty_cache::ensure(root.path(), &inputs(), REQUIRED, 6, build, |entry| {
        fs::write(entry.join("lib/native.a"), b"changed during copying")?;
        ghostty_cache::materialize(entry, REQUIRED, out.path())
    });
    assert!(result.is_err());
    assert!(!out.path().join("lib/native.a").exists());
}

#[test]
fn concurrent_child() {
    let Some(root) = std::env::var_os("GHOSTTY_CACHE_TEST_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    ensure(root, &inputs(), REQUIRED, |prefix, work| {
        use std::io::Write;
        let mut count = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("builds"))?;
        writeln!(count, "build")?;
        std::thread::sleep(std::time::Duration::from_millis(100));
        build(prefix, work)
    })
    .unwrap();
}

#[test]
fn concurrent_processes_build_once_under_exclusive_lock() {
    let root = tempfile::tempdir().unwrap();
    let executable = std::env::current_exe().unwrap();
    let mut children = (0..6)
        .map(|_| {
            Command::new(&executable)
                .args(["--exact", "concurrent_child"])
                .env_clear()
                .env("GHOSTTY_CACHE_TEST_ROOT", root.path())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(
        fs::read_to_string(root.path().join("builds")).unwrap(),
        "build\n"
    );
    verify(&root.path().join(inputs().key()), &inputs(), REQUIRED).unwrap();
}

#[cfg(unix)]
#[test]
fn entry_and_manifest_cannot_be_symlinks() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let input = inputs();
    let result = ensure(root.path(), &input, REQUIRED, build).unwrap();
    let alias = root.path().join("alias");
    symlink(&result.path, &alias).unwrap();
    assert!(verify(&alias, &input, REQUIRED).is_err());
    let manifest = result.path.join("manifest.json");
    let external = root.path().join("external-manifest");
    fs::rename(&manifest, &external).unwrap();
    symlink(external, manifest).unwrap();
    assert!(verify(&result.path, &input, REQUIRED).is_err());
}

#[cfg(unix)]
#[test]
fn artifact_symlinks_must_stay_inside_entry() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let input = inputs();
    let result = ensure(root.path(), &input, REQUIRED, |prefix, work| {
        build(prefix, work)?;
        symlink("native.a", prefix.join("lib/alias.a"))
    })
    .unwrap();
    verify(&result.path, &input, REQUIRED).unwrap();
    fs::remove_file(result.path.join("lib/alias.a")).unwrap();
    let external = root.path().join("external");
    fs::write(&external, b"fake archive").unwrap();
    symlink(external, result.path.join("lib/alias.a")).unwrap();
    assert!(verify(&result.path, &input, REQUIRED).is_err());
}
