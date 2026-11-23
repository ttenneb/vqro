//! Fake CLI only: no Rex process, pipe or user server is contacted.
use super::*;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fake { root: PathBuf, dir: PathBuf, bin: PathBuf }
impl Fake {
    fn new(exit: i32, garbage: bool) -> Self {
        let root = PathBuf::from(format!("/tmp/rxstatus-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let dir = root.join("owned");
        std::fs::create_dir(&dir).unwrap();
        let bin = root.join("fake-rex");
        let ready = json!({"endpoint":r"\\.\pipe\rex-status-fixture"}).to_string();
        let initial = if garbage { "not-json" } else { "" };
        let script = format!("#!/bin/sh\ncase \"$*\" in\n*'server status --json'*)\n if test -f \"$REX_RUNTIME_DIR/started\"; then printf '%s' '{ready}'; exit 0; fi\n printf '%s' '{initial}'; printf '%s' 'fixture status failure' >&2; exit {exit};;\n*'server start --json'*) touch \"$REX_RUNTIME_DIR/started\"; printf '%s' '{{\"started\":true}}';;\n*'api call server.status'*) printf '%s' '{{\"instance_id\":\"fixture\",\"session_count\":0,\"version\":\"fake\"}}';;\n*) exit 2;;\nesac\n");
        // Written by a child process, never through a file descriptor of this test process: with tests running in
        // parallel, another test's fork (before its exec) can inherit an open write descriptor, and executing the
        // script then fails with ETXTBSY ("Text file busy"), which the status code rightly reports as unknown.
        let mut writer = std::process::Command::new("/bin/sh")
            .args(["-c", "cat > \"$1\" && chmod 700 \"$1\"", "sh"]).arg(&bin)
            .stdin(std::process::Stdio::piped()).spawn().unwrap();
        {
            use std::io::Write;
            let mut stdin = writer.stdin.take().unwrap();
            stdin.write_all(script.as_bytes()).unwrap();
        }
        assert!(writer.wait().unwrap().success());
        Self { root, dir, bin }
    }
}
impl Drop for Fake { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); } }

#[test]
fn hard_cli_failure_is_unknown_never_an_absence_witness() {
    let fake = Fake::new(1, false);
    assert!(reported_endpoint(&fake.bin, &fake.dir).is_err(), "a generic CLI failure is not server absence");
    assert!(!fake.dir.join("started").exists());
}

#[test]
fn unreachable_exit_is_absent_but_garbage_success_is_unknown() {
    let unreachable = Fake::new(4, false);
    assert!(reported_endpoint(&unreachable.bin, &unreachable.dir).unwrap().is_none());
    let garbage = Fake::new(0, true);
    assert!(reported_endpoint(&garbage.bin, &garbage.dir).is_err());
}

#[test]
fn unix_probe_other_error_is_unknown_not_running_false() {
    let fake = Fake::new(1, false);
    let regular = fake.root.join("not-a-directory");
    std::fs::write(&regular, b"not a directory").unwrap();
    let reported = status(&fake.bin, &regular.join("child")).unwrap();
    assert_eq!(reported["running"], "unknown");
    assert!(reported["reason"].as_str().is_some_and(|s| !s.is_empty()));
}

#[tokio::test]
async fn cli_absence_starts_but_unknown_status_never_starts_or_marks() {
    let absent = Fake::new(4, false);
    let _lock = lock(&absent.dir).await.unwrap();
    assert_eq!(status_with_probe(&absent.bin, &absent.dir, || reported_endpoint(&absent.bin, &absent.dir))["running"], false);
    let endpoint = ensure_with_probe(&absent.bin, &absent.dir, || reported_endpoint(&absent.bin, &absent.dir)).unwrap();
    assert_eq!(endpoint, Path::new(r"\\.\pipe\rex-status-fixture"));
    assert!(absent.dir.join("started").exists() && marked(&absent.dir));
    assert_eq!(status_with_probe(&absent.bin, &absent.dir, || reported_endpoint(&absent.bin, &absent.dir))["running"], true);

    for fake in [Fake::new(1, false), Fake::new(0, true)] {
        let _lock = lock(&fake.dir).await.unwrap();
        let error = ensure_with_probe(&fake.bin, &fake.dir, || reported_endpoint(&fake.bin, &fake.dir)).unwrap_err();
        assert!(error.contains("status unknown") && error.contains("refusing"));
        let reported = status_with_probe(&fake.bin, &fake.dir, || reported_endpoint(&fake.bin, &fake.dir));
        assert_eq!(reported["running"], "unknown");
        assert!(reported["reason"].as_str().is_some_and(|s| s.contains("status unknown")));
        assert!(!fake.dir.join("started").exists() && !marked(&fake.dir));
    }
}

#[tokio::test]
async fn unix_only_not_found_and_refused_are_absent_all_other_errors_refuse_start() {
    use std::io::{Error, ErrorKind};
    let fake = Fake::new(1, false);
    let endpoint = socket(&fake.dir);
    for kind in [ErrorKind::NotFound, ErrorKind::ConnectionRefused] {
        assert!(socket_probe::<()>(&endpoint, Err(Error::from(kind))).unwrap().is_none());
    }
    let _lock = lock(&fake.dir).await.unwrap();
    for kind in [ErrorKind::PermissionDenied, ErrorKind::Other, ErrorKind::TimedOut, ErrorKind::InvalidInput] {
        let probe = || socket_probe::<()>(&endpoint, Err(Error::new(kind, "fixture socket refusal")));
        assert!(ensure_with_probe(&fake.bin, &fake.dir, probe).unwrap_err().contains("status unknown"));
        let reported = status_with_probe(&fake.bin, &fake.dir, probe);
        assert_eq!(reported["running"], "unknown");
        assert!(!fake.dir.join("started").exists() && !marked(&fake.dir));
    }
    // Actual missing and stale Unix socket paths use those same two positive absence classifications.
    assert!(native_endpoint(&fake.bin, &fake.dir).unwrap().is_none());
    // The stale socket is made by a child process that binds and exits, never by a listener in this test process:
    // with tests running in parallel, another test's fork (before its exec) can briefly hold a copy of an in-process
    // listener, so it would still accept and the path would not be refused.
    let made = std::process::Command::new("python3")
        .args(["-c", "import socket,sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])"])
        .arg(&endpoint).status().unwrap();
    assert!(made.success() && endpoint.exists());
    assert!(native_endpoint(&fake.bin, &fake.dir).unwrap().is_none());
}
