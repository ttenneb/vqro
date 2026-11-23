//! Cache tests use tiny fake native artifacts; no Zig, network, or live services.
#[path = "../build/ghostty_cache.rs"]
mod ghostty_cache;
#[path = "../build/native_config.rs"]
mod native_config;
// Include the actual build-script parser; its other entrypoints are not test entrypoints.
#[allow(dead_code)]
#[path = "../build.rs"]
mod build_script;

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
        native: [("boundary".into(), Some("fixture".into()))].into(),
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

// CLOEXEC closes descriptors at exec, not at fork. Keep unrelated process-spawn and lock-owning
// fixtures mutually exclusive, so immediate eviction assertions cannot see pre-exec FD copies.
static PROCESS_FD_FIXTURES: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn serial_fixture() -> std::sync::MutexGuard<'static, ()> {
    PROCESS_FD_FIXTURES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

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
fn gc_activation_child() {
    let _serial = serial_fixture();
    let Ok(expected) = std::env::var("GHOSTTY_GC_EXPECTED") else {
        return;
    };
    let outcome = std::panic::catch_unwind(|| {
        build_script::env_bool("VQRO_LIBGHOSTTY_CACHE_GC").unwrap_or(false)
    });
    match expected.as_str() {
        "on" | "off" => assert_eq!(outcome.unwrap(), expected == "on"),
        "refuse" => {
            let error = outcome.expect_err("unrecognised boolean must stop the build");
            let text = error
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| error.downcast_ref::<&str>().copied())
                .unwrap_or("non-string panic");
            assert!(
                text.starts_with("invalid boolean value for VQRO_LIBGHOSTTY_CACHE_GC:"),
                "{text}"
            );
        }
        _ => panic!("unknown expected activation outcome"),
    }
}

#[test]
fn gc_activation_uses_the_main_accepted_env_bool_grammar() {
    let _serial = serial_fixture();
    for (value, expected) in [
        (Some("1"), "on"),
        (Some("true"), "on"),
        (Some("YES"), "on"),
        (Some("On"), "on"),
        (Some("0"), "off"),
        (Some("false"), "off"),
        (Some("no"), "off"),
        (Some("OFF"), "off"),
        (None, "off"),
        (Some(""), "refuse"),
        (Some("maybe"), "refuse"),
    ] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "gc_activation_child"])
            .env_clear()
            .env("GHOSTTY_GC_EXPECTED", expected);
        if let Some(value) = value {
            child.env("VQRO_LIBGHOSTTY_CACHE_GC", value);
        }
        assert!(child.status().unwrap().success(), "GC value {value:?}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn lock_descriptor_exec_child() {
    let _serial = serial_fixture();
    let Some(lock) = std::env::var_os("GHOSTTY_CACHE_EXPECT_NO_LOCK_FD") else {
        return;
    };
    let lock = std::path::PathBuf::from(lock);
    for entry in fs::read_dir("/proc/self/fd").unwrap() {
        assert_ne!(
            fs::read_link(entry.unwrap().path()).ok().as_ref(),
            Some(&lock),
            "cache lock must close at exec"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn actual_cache_lock_descriptors_are_cloexec_and_absent_after_exec() {
    let _serial = serial_fixture();
    let root = tempfile::tempdir().unwrap();
    let input = inputs();
    let lock = root
        .path()
        .canonicalize()
        .unwrap()
        .join(format!("{}.lock", input.key()));
    ensure(root.path(), &input, REQUIRED, |prefix, work| {
        let descriptors = fs::read_dir("/proc/self/fd")?
            .filter_map(Result::ok)
            .filter(|entry| fs::read_link(entry.path()).ok().as_ref() == Some(&lock))
            .map(|entry| entry.file_name().to_str().unwrap().parse::<i32>().unwrap())
            .collect::<Vec<_>>();
        assert!(
            !descriptors.is_empty(),
            "inspect the actual live production lock, not a separate fixture fd"
        );
        for fd in descriptors {
            // Read-only query; ensure's File guard owns and keeps this descriptor alive throughout.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert!(
                flags >= 0 && flags & libc::FD_CLOEXEC != 0,
                "cache fd {fd} lacks CLOEXEC"
            );
        }
        assert!(
            Command::new(std::env::current_exe()?)
                .args(["--exact", "lock_descriptor_exec_child"])
                .env_clear()
                .env("GHOSTTY_CACHE_EXPECT_NO_LOCK_FD", &lock)
                .status()?
                .success()
        );
        build(prefix, work)
    })
    .unwrap();
}

#[cfg(unix)]
fn query_transport(root: &Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join(name);
    fs::write(&path, format!("#!/usr/bin/env python3\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[cfg(unix)]
#[test]
fn production_windows_wrapper_does_not_mask_query_errors_as_absent() {
    let _serial = serial_fixture();
    let root = tempfile::tempdir().unwrap();
    for scenario in [
        "denied",
        "null",
        "start-denied",
        "failed-absent",
        "stderr-absent",
        "absent",
        "live",
    ] {
        // Exercise the actual generated command and production decoder through a subprocess, not
        // cleanup_stages' injected liveness seam. This transport models the original masking bug.
        let body = format!(
            r#"import sys
assert sys.argv[1:4] == ['-NoProfile', '-NonInteractive', '-Command']
script = sys.argv[4]
scenario = {scenario:?}
if scenario in ('denied', 'null'):
    if '-ErrorAction SilentlyContinue' in script:
        print('absent')
        sys.exit(0)
    assert '-ErrorAction Stop' in script
    assert 'NoProcessFoundForGivenId' in script
    sys.exit(1)
if scenario == 'start-denied': sys.exit(1)
if scenario == 'failed-absent':
    print('absent')
    sys.exit(1)
if scenario == 'stderr-absent':
    print('absent')
    print('access denied', file=sys.stderr)
    sys.exit(0)
print('absent' if scenario == 'absent' else '123456')
"#
        );
        let executable = query_transport(root.path(), scenario, &body);
        let result = ghostty_cache::windows_process_start(&executable, 123);
        match scenario {
            "absent" => assert_eq!(result.unwrap(), None),
            "live" => assert_eq!(result.unwrap().as_deref(), Some("123456")),
            _ => assert!(result.is_err(), "{scenario} must be UNKNOWN, not absent"),
        }
    }
    let script = ghostty_cache::windows_process_script(123);
    assert!(!script.contains("SilentlyContinue"));
    assert!(script.contains("-ErrorAction Stop"));
    assert!(script.contains("NoProcessFoundForGivenId"));
    assert!(script.contains("CategoryInfo.Category"));
    let normalized = script.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(normalized.contains("if (($_.FullyQualifiedErrorId -split ',')[0] -eq 'NoProcessFoundForGivenId' -and $_.CategoryInfo.Category -eq [System.Management.Automation.ErrorCategory]::ObjectNotFound) { [Console]::Out.WriteLine('absent'); exit 0 } exit 1"));
    assert!(normalized.contains("if ($null -eq $p) { exit 1 }"));
    assert_eq!(script.matches("WriteLine('absent')").count(), 1);
    assert!(script.find("NoProcessFoundForGivenId").unwrap() < script.find("StartTime").unwrap());
}

#[cfg(windows)]
#[test]
fn production_powershell_script_classifies_real_cmdlet_errors_before_start_time() {
    let _serial = serial_fixture();
    // Runs in the real PowerShell interpreter on Windows. Advanced-function failures have the same
    // ErrorAction/error-record behavior as cmdlets, including nonterminating permission errors.
    for (body, expected) in [
        (
            "Write-Error -Message denied -ErrorId AccessDenied -Category PermissionDenied",
            "unknown",
        ),
        (
            "Write-Error -Message missing -ErrorId NoProcessFoundForGivenId -Category ObjectNotFound",
            "absent",
        ),
        ("return $null", "unknown"),
        (
            "$p = New-Object PSObject; $p | Add-Member ScriptProperty StartTime { throw 'denied' }; $p",
            "unknown",
        ),
    ] {
        let script = format!(
            "function Get-Process {{ [CmdletBinding()] param([int]$Id) {body} }}; {}",
            ghostty_cache::windows_process_script(123)
        );
        let result = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            .unwrap();
        let result = ghostty_cache::windows_process_result(result);
        if expected == "absent" {
            assert_eq!(result.unwrap(), None);
        } else {
            assert!(result.is_err(), "{body}");
        }
    }
    assert!(
        ghostty_cache::windows_process_start(Path::new("powershell.exe"), std::process::id())
            .unwrap()
            .is_some()
    );
}

#[cfg(unix)]
#[test]
fn production_unix_wrapper_keeps_ambiguous_ps_failures_unknown() {
    let _serial = serial_fixture();
    let root = tempfile::tempdir().unwrap();
    for (name, body, known) in [
        ("empty-failure", "import sys; sys.exit(1)", false),
        ("empty-success", "", false),
        (
            "denied",
            "import sys; print('denied', file=sys.stderr); sys.exit(1)",
            false,
        ),
        ("live", "print('Mon Nov 23 17:00:00 2026')", true),
    ] {
        let ps = query_transport(root.path(), name, body);
        let result = ghostty_cache::unix_process_start(&ps, 123);
        if known {
            assert!(result.unwrap().is_some());
        } else {
            assert!(result.is_err(), "{name} must never be read as absent");
        }
    }
}

#[cfg(unix)]
#[test]
fn production_linux_wrapper_requires_positive_absence_in_a_working_proc_view() {
    let _serial = serial_fixture();
    let root = tempfile::tempdir().unwrap();
    assert!(ghostty_cache::linux_process_start(root.path(), 123).is_err());
    fs::create_dir(root.path().join("self")).unwrap();
    fs::write(root.path().join("self/stat"), b"working self query").unwrap();
    assert_eq!(
        ghostty_cache::linux_process_start(root.path(), 123).unwrap(),
        None
    );
    fs::create_dir(root.path().join("123")).unwrap();
    assert!(
        ghostty_cache::linux_process_start(root.path(), 123).is_err(),
        "missing identity for an existing process is UNKNOWN"
    );
    fs::create_dir(root.path().join("123/stat")).unwrap();
    assert!(
        ghostty_cache::linux_process_start(root.path(), 123).is_err(),
        "query errors are UNKNOWN"
    );
    assert!(ghostty_cache::linux_process_start(root.path(), 0).is_err());
}

#[test]
fn every_input_changes_key_and_field_boundaries_are_unambiguous() {
    let _serial = serial_fixture();
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
    let mut changed = original.clone();
    changed
        .native
        .insert("boundary".into(), Some("other".into()));
    variants.push(changed);
    let mut changed = original.clone();
    changed.native.insert("caller".into(), None);
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
fn native_reuse_boundary_is_explicit_for_every_supported_platform() {
    let _serial = serial_fixture();
    let empty = std::collections::BTreeMap::new();
    assert!(
        !native_config::ENV.contains(&"LD_LIBRARY_PATH"),
        "Cargo's runtime loader path is not a native archive setting"
    );
    for target in [
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-musl",
        "x86_64-pc-windows-gnu",
        "aarch64-pc-windows-gnullvm",
    ] {
        assert!(native_config::reason(target, &empty).is_none(), "{target}");
    }
    for target in ["aarch64-apple-darwin", "x86_64-pc-windows-msvc", "other"] {
        assert!(native_config::reason(target, &empty).is_some(), "{target}");
    }
    for variable in native_config::ENV {
        let settings = [(variable.to_string(), Some("external".into()))].into();
        let opted_out = native_config::reason("x86_64-unknown-linux-gnu", &settings).is_some();
        assert_eq!(opted_out, *variable != "PROGRAMDATA", "{variable}");
    }
}

#[test]
fn resolved_toolchain_host_and_every_caller_setting_affect_the_key() {
    let _serial = serial_fixture();
    let config = native_config::capture(
        "/nonexistent-ghostty-cache-zig",
        "host-triple",
        "x86_64-unknown-linux-gnu",
    );
    assert!(
        config.uncached.is_some(),
        "unknown resolution must fail closed to a local build"
    );
    assert_eq!(
        config.key["boundary"].as_deref(),
        Some(native_config::BOUNDARY)
    );
    let mut input = inputs();
    input.native = config.key;
    for name in input.native.keys() {
        let mut changed = input.clone();
        changed.native.insert(name.clone(), Some("changed".into()));
        assert_ne!(input.key(), changed.key(), "{name}");
    }
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("zig");
    let lib = dir.path().join("lib");
    fs::write(&exe, b"fixture").unwrap();
    fs::create_dir(&lib).unwrap();
    let output = format!(
        ".{{\n .zig_exe = {},\n .lib_dir = {},\n .target = \"host-resolved-target\",\n}}",
        serde_json::to_string(&exe.to_str().unwrap()).unwrap(),
        serde_json::to_string(&lib.to_str().unwrap()).unwrap()
    );
    let resolved = native_config::parse_zig_env(&output).unwrap();
    for (name, value) in resolved {
        let mut a = input.clone();
        a.native.insert(name.clone(), value);
        let mut b = a.clone();
        b.native.insert(name.clone(), Some("changed".into()));
        assert_ne!(a.key(), b.key(), "{name}");
    }
    assert!(native_config::parse_zig_env("unrecognized output").is_err());
}

#[test]
fn actual_source_contents_not_checkout_path_determine_identity() {
    let _serial = serial_fixture();
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
fn observed_source_edit_during_build_is_local_only_and_never_a_stale_hit() {
    let _serial = serial_fixture();
    let root = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let file = source.path().join("main.zig");
    fs::write(&file, b"S").unwrap();
    let mut input = inputs();
    input.source = source_digest(source.path()).unwrap();
    let changed = ghostty_cache::ensure_checked(
        root.path(),
        &input,
        REQUIRED,
        6,
        false,
        || source_digest(source.path()),
        |prefix, work| {
            fs::write(&file, b"T")?;
            build(prefix, work)
        },
        |entry| ghostty_cache::materialize(entry, REQUIRED, out.path()),
    )
    .unwrap();
    assert!(!changed.cached);
    assert!(!changed.reused);
    assert!(!root.path().join(input.key()).exists());
    assert_eq!(
        fs::read(out.path().join("lib/native.a")).unwrap(),
        b"fake archive"
    );
    fs::write(&file, b"S").unwrap();
    let next = ghostty_cache::ensure_checked(
        root.path(),
        &input,
        REQUIRED,
        6,
        false,
        || source_digest(source.path()),
        build,
        |_| Ok(()),
    )
    .unwrap();
    assert!(next.cached);
    assert!(
        !next.reused,
        "a source edit must never publish T under key(S)"
    );
}

#[test]
fn source_is_rechecked_immediately_before_accepting_a_reuse() {
    let _serial = serial_fixture();
    let root = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let file = source.path().join("main.zig");
    fs::write(&file, b"S").unwrap();
    let mut input = inputs();
    input.source = source_digest(source.path()).unwrap();
    ensure(root.path(), &input, REQUIRED, build).unwrap();
    let calls = std::cell::Cell::new(0);
    let result = ghostty_cache::ensure_checked(
        root.path(),
        &input,
        REQUIRED,
        6,
        false,
        || {
            calls.set(calls.get() + 1);
            if calls.get() == 2 {
                fs::write(&file, b"T")?;
            }
            source_digest(source.path())
        },
        build,
        |_| Ok(()),
    )
    .unwrap();
    assert!(!result.cached);
    assert!(!result.reused);
    verify(&root.path().join(input.key()), &input, REQUIRED).unwrap();
}

#[test]
fn source_edit_while_waiting_for_lock_cannot_accept_an_old_hit() {
    let _serial = serial_fixture();
    let root = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    fs::write(source.path().join("main.zig"), b"S").unwrap();
    let mut input = inputs();
    input.source = source_digest(source.path()).unwrap();
    ensure(root.path(), &input, REQUIRED, build).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join(format!("{}.lock", input.key())))
        .unwrap();
    lock.lock().unwrap();
    let (started, waiting) = std::sync::mpsc::channel();
    let root_path = root.path().to_owned();
    let source_path = source.path().to_owned();
    let candidate = input.clone();
    let builder = std::thread::spawn(move || {
        started.send(()).unwrap();
        ghostty_cache::ensure_checked(
            &root_path,
            &candidate,
            REQUIRED,
            6,
            false,
            || source_digest(&source_path),
            build,
            |_| Ok(()),
        )
        .unwrap()
    });
    waiting.recv().unwrap();
    fs::write(source.path().join("main.zig"), b"T").unwrap();
    drop(lock);
    let result = builder.join().unwrap();
    assert!(!result.cached);
    assert!(!result.reused);
    verify(&root.path().join(input.key()), &input, REQUIRED).unwrap();
}

#[test]
fn source_digest_matches_checked_in_vendor_manifest() {
    let _serial = serial_fixture();
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
    let _serial = serial_fixture();
    let root = tempfile::tempdir().unwrap();
    let result = ensure(root.path(), &inputs(), REQUIRED, build).unwrap();
    fs::write(result.path.join("lib\\native.a"), b"fake archive").unwrap();
    assert!(verify(&result.path, &inputs(), REQUIRED).is_err());
}

#[test]
fn manifest_and_every_artifact_are_required_and_verified() {
    let _serial = serial_fixture();
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
    let _serial = serial_fixture();
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
    let _serial = serial_fixture();
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
fn interrupted_stage_needs_dead_owner_and_one_hour_tree_quiescence() {
    let _serial = serial_fixture();
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
    let stage = Path::new(&stage);
    // Deterministic liveness seam: the recorded process is gone, independent of host query support.
    fs::write(stage.join("owner.json"), br#"{"pid":123,"start":"birth"}"#).unwrap();
    let now = std::time::SystemTime::now();
    let later = now + std::time::Duration::from_secs(7200);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join(format!("{}.lock", inputs().key())))
        .unwrap();
    lock.lock().unwrap();
    ghostty_cache::cleanup_stages(root.path(), &inputs().key(), now, |_| Ok(None)).unwrap();
    assert!(
        stage.exists(),
        "a dead owner alone does not prove orphan Zig stopped"
    );
    ghostty_cache::cleanup_stages(root.path(), &inputs().key(), later, |_| {
        Ok(Some("birth".into()))
    })
    .unwrap();
    assert!(stage.exists(), "a quiet tree with a live owner must remain");
    let object = stage.join("zig-cache/unfinished.o");
    fs::OpenOptions::new()
        .write(true)
        .open(&object)
        .unwrap()
        .set_modified(later)
        .unwrap();
    ghostty_cache::cleanup_stages(root.path(), &inputs().key(), later, |_| Ok(None)).unwrap();
    assert!(
        stage.exists(),
        "newest descendant activity must protect the whole stage"
    );
    fs::OpenOptions::new()
        .write(true)
        .open(object)
        .unwrap()
        .set_modified(now)
        .unwrap();
    ghostty_cache::cleanup_stages(root.path(), &inputs().key(), later, |_| {
        Err(std::io::Error::other("unknown"))
    })
    .unwrap();
    assert!(stage.exists(), "unknown process identity must be retained");
    ghostty_cache::cleanup_stages(root.path(), &inputs().key(), later, |_| {
        Ok(Some("new-process-birth".into()))
    })
    .unwrap();
    assert!(
        !stage.exists(),
        "PID reuse with a different start identity means the recorded owner is gone"
    );
}

#[test]
fn cache_hit_retains_unknown_stages_and_other_keys() {
    let _serial = serial_fixture();
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
    assert!(
        stale.exists(),
        "an ownerless stage must never be inferred abandoned"
    );
    assert!(active_other.is_dir());
    assert!(unrelated.is_file());
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.path().join(format!("{}.lock", input.key())))
        .unwrap();
    lock.lock().unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(7200);
    ghostty_cache::cleanup_stages(root.path(), &input.key(), later, |_| Ok(None)).unwrap();
    assert!(stale.exists(), "even an old ownerless stage is retained");
    fs::write(stale.join("owner.json"), br#"{"pid":123,"start":null}"#).unwrap();
    ghostty_cache::cleanup_stages(root.path(), &input.key(), later, |_| Ok(None)).unwrap();
    assert!(stale.exists(), "unknown start identity is retained");
    for record in [
        br#"{"pid":0,"start":"birth"}"#.as_slice(),
        br#"{"pid":123,"start":""}"#.as_slice(),
    ] {
        fs::write(stale.join("owner.json"), record).unwrap();
        ghostty_cache::cleanup_stages(root.path(), &input.key(), later, |_| {
            panic!("invalid identity must not be queried")
        })
        .unwrap();
        assert!(stale.exists(), "invalid owner identity must be retained");
    }
}

#[test]
fn second_checkout_reuses_but_key_mismatch_builds_again() {
    let _serial = serial_fixture();
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
    let _serial = serial_fixture();
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
    let _serial = serial_fixture();
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
fn gc_opt_out_leaves_old_keys_until_explicitly_enabled() {
    let _serial = serial_fixture();
    let root = tempfile::tempdir().unwrap();
    for index in 0..3 {
        let input = numbered_input(index);
        let entry = ghostty_cache::ensure_checked(
            root.path(),
            &input,
            REQUIRED,
            1,
            false,
            || Ok(input.source.clone()),
            build,
            |_| Ok(()),
        )
        .unwrap();
        age_entry(&entry.path, index as u64);
    }
    for index in 0..3 {
        assert!(root.path().join(numbered_input(index).key()).exists());
    }
    let input = numbered_input(2);
    ghostty_cache::ensure_checked(
        root.path(),
        &input,
        REQUIRED,
        1,
        true,
        || Ok(input.source.clone()),
        build,
        |_| Ok(()),
    )
    .unwrap();
    assert!(!root.path().join(numbered_input(0).key()).exists());
    assert!(!root.path().join(numbered_input(1).key()).exists());
}

#[cfg(target_os = "linux")]
#[test]
fn gc_opt_out_retains_an_otherwise_reclaimable_stage() {
    let _serial = serial_fixture();
    let root = tempfile::tempdir().unwrap();
    assert!(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "interrupted_child"])
            .env_clear()
            .env("GHOSTTY_CACHE_INTERRUPTED_ROOT", root.path())
            .status()
            .unwrap()
            .success()
    );
    let name = fs::read_to_string(root.path().join("abandoned-stage")).unwrap();
    let stage = Path::new(&name);
    for path in [
        stage.join("owner.json"),
        stage.join("zig-cache/unfinished.o"),
        stage.join("zig-cache"),
        stage.join("install"),
        stage.to_owned(),
    ] {
        fs::File::open(path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH)
            .unwrap();
    }
    let input = inputs();
    ghostty_cache::ensure_checked(
        root.path(),
        &input,
        REQUIRED,
        6,
        false,
        || Ok(input.source.clone()),
        build,
        |_| Ok(()),
    )
    .unwrap();
    assert!(
        stage.exists(),
        "disabled GC must not reclaim even a positively dead and quiet stage"
    );
    ghostty_cache::ensure_checked(
        root.path(),
        &input,
        REQUIRED,
        6,
        true,
        || Ok(input.source.clone()),
        build,
        |_| Ok(()),
    )
    .unwrap();
    assert!(!stage.exists());
}

#[test]
fn retention_is_configurable_and_zero_is_rejected() {
    let _serial = serial_fixture();
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
    let _serial = serial_fixture();
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
    let _serial = serial_fixture();
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
    let _serial = serial_fixture();
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
    let _serial = serial_fixture();
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
    let _serial = serial_fixture();
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
fn publication_rejects_absolute_internal_links_and_preserves_relative_links_after_relocation() {
    let _serial = serial_fixture();
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let input = inputs();
    assert!(
        ensure(root.path(), &input, REQUIRED, |prefix, work| {
            build(prefix, work)?;
            symlink(prefix.join("lib/native.a"), prefix.join("lib/alias.a"))
        })
        .is_err()
    );
    assert!(
        !root.path().join(input.key()).exists(),
        "absolute staging paths must never be published"
    );
    let entry = ensure(root.path(), &input, REQUIRED, |prefix, work| {
        build(prefix, work)?;
        symlink("native.a", prefix.join("lib/alias.a"))
    })
    .unwrap();
    assert_eq!(entry.path.file_name().unwrap(), input.key().as_str());
    verify(&entry.path, &input, REQUIRED).unwrap();
    assert_eq!(
        fs::read(entry.path.join("lib/alias.a")).unwrap(),
        b"fake archive"
    );
}

#[cfg(unix)]
#[test]
fn relocation_escape_return_relative_link_is_rejected_before_publication() {
    let _serial = serial_fixture();
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let input = inputs();
    let result = ensure(root.path(), &input, REQUIRED, |prefix, work| {
        build(prefix, work)?;
        symlink("../../install/lib/native.a", prefix.join("lib/alias.a"))
    });
    assert!(
        result.is_err(),
        "a link valid only under the install staging basename is nonrelocatable"
    );
    assert!(!root.path().join(input.key()).exists());
    let good = ensure(root.path(), &input, REQUIRED, |prefix, work| {
        build(prefix, work)?;
        fs::create_dir(prefix.join("lib/nested"))?;
        symlink("../native.a", prefix.join("lib/nested/alias.a"))
    })
    .unwrap();
    verify(&good.path, &input, REQUIRED).unwrap();
    assert_eq!(
        fs::read(good.path.join("lib/nested/alias.a")).unwrap(),
        b"fake archive"
    );
}

#[cfg(unix)]
#[test]
fn artifact_symlinks_must_stay_inside_entry() {
    let _serial = serial_fixture();
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
