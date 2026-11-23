//! Command timeouts in vqro::tmux against a deterministic fake control-mode tmux
//! (`tests/fixtures/fake_tmux_cc.py`): a read-only command that times out is not fatal and its late
//! reply never pairs with a later command; a mutating one is (outcome unknown); a late reply that
//! never comes is a desync.
#![cfg(unix)]

#[path = "support/python.rs"] mod test_python;
use std::path::{Path, PathBuf};
use std::time::Duration;

use vqro::tmux::{Client, Config, Error, Notification};
use tokio::sync::broadcast;

fn fake() -> Option<PathBuf> {
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_tmux_cc.py");
    Path::new(test_python::python()).exists().then_some(fake)
}

async fn client(dir: &Path, conf: &str, timeout_ms: u64) -> Client {
    let socket = dir.join("s");
    std::fs::write(dir.join("s.conf"), conf).unwrap();
    let mut config = Config::new(socket, "test");
    // Run the fixture with the same resolved interpreter, even with an old python3 first on PATH.
    use std::os::unix::fs::PermissionsExt;
    let wrapper = dir.join("fake-tmux");
    std::fs::write(&wrapper, format!("#!/bin/sh\nexec '{}' '{}' \"$@\"\n", test_python::python().replace('\'', "'\\''"), fake().unwrap().display())).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    config.binary = wrapper;
    config.command_timeout = Duration::from_millis(timeout_ms);
    Client::connect(config).await.unwrap()
}

fn disconnected(rx: &mut broadcast::Receiver<Notification>) -> bool {
    while let Ok(n) = rx.try_recv() {
        if matches!(n, Notification::Disconnected { .. }) {
            return true;
        }
    }
    false
}

#[tokio::test]
async fn read_only_timeout_keeps_the_client_and_its_late_reply_never_pairs_with_the_next_command() {
    let Some(_) = fake() else { return eprintln!("skipped: no test Python") };
    let dir = tempfile::Builder::new().prefix("pft").tempdir_in("/tmp").unwrap();
    let c = client(dir.path(), r#"{"slow": {"list-panes": 0.6}}"#, 200).await;
    let mut notes = c.notifications();
    assert!(matches!(c.command(&["list-panes", "-a"]).await, Err(Error::TimedOut(_))));
    // Sent while the late reply is still due: it must get its own reply, not "%0 %1".
    let next = tokio::time::timeout(Duration::from_secs(3), c.command(&["display-message", "-p", "after"])).await.unwrap();
    assert_eq!(next.unwrap(), vec![b"after".to_vec()]);
    assert_eq!(c.command(&["list-panes"]).await.err().map(|e| matches!(e, Error::TimedOut(_))), Some(true), "still slow, still alive");
    assert_eq!(c.command(&["display-message", "-p", "still-paired"]).await.unwrap(), vec![b"still-paired".to_vec()]);
    assert!(!disconnected(&mut notes), "a read-only timeout does not drop the control client");
    // Each command was written once: no resend after the timeout.
    let log = std::fs::read_to_string(dir.path().join("s.log")).unwrap();
    assert_eq!(log.lines().filter(|l| l.starts_with("'list-panes'")).count(), 2);
    c.close().await;
}

#[tokio::test]
async fn short_read_deadline_does_not_destroy_the_late_reply_drain_window() {
    let Some(_) = fake() else { return };
    let dir = tempfile::Builder::new().prefix("pft").tempdir_in("/tmp").unwrap();
    let c = client(dir.path(), r#"{"slow": {"list-panes": 0.8}}"#, 50).await;
    let mut notes = c.notifications();
    assert!(matches!(c.command(&["list-panes"]).await, Err(Error::TimedOut(_))));
    // Healthy FIFO reply is later than the old 10*50ms drain allowance. The caller still
    // times out promptly, but a queued request must wait for the old barrier, not lose pairing.
    let next = tokio::time::timeout(Duration::from_secs(2), c.command(&["display-message", "-p", "own-reply"])).await.unwrap();
    assert_eq!(next.unwrap(), vec![b"own-reply".to_vec()]);
    assert!(!disconnected(&mut notes));
    let log = std::fs::read_to_string(dir.path().join("s.log")).unwrap();
    assert_eq!(log.lines().filter(|line| line.starts_with("'list-panes'")).count(), 1, "never resend a timed-out read");
    c.close().await;
}

#[tokio::test]
async fn mutating_timeout_is_indeterminate_and_drops_the_client() {
    let Some(_) = fake() else { return };
    let dir = tempfile::Builder::new().prefix("pft").tempdir_in("/tmp").unwrap();
    let c = client(dir.path(), r#"{"slow": {"kill-pane": 0.6}}"#, 200).await;
    let mut notes = c.notifications();
    assert!(matches!(c.command(&["kill-pane", "-t", "%1"]).await, Err(Error::Indeterminate(_))));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(disconnected(&mut notes));
    assert!(matches!(c.command(&["display-message", "-p", "x"]).await, Err(Error::Disconnected)));
    let log = std::fs::read_to_string(dir.path().join("s.log")).unwrap();
    assert_eq!(log.lines().filter(|line| line.starts_with("'kill-pane'")).count(), 1, "never resend an indeterminate mutation");
}

#[tokio::test]
async fn a_slow_attach_is_not_held_to_the_command_timeout() {
    // Flake on f05a2409: connect waited for tmux's attach block only `command_timeout` (50 ms in these tests), so a
    // slow start of the control client (python fake under load) failed connect with Disconnected.
    let Some(_) = fake() else { return };
    let dir = tempfile::Builder::new().prefix("pft").tempdir_in("/tmp").unwrap();
    let c = client(dir.path(), r#"{"attach_delay": 0.3}"#, 50).await;
    assert_eq!(c.command(&["display-message", "-p", "attached"]).await.unwrap(), vec![b"attached".to_vec()]);
    c.close().await;
}

#[tokio::test]
async fn a_late_reply_that_never_comes_is_a_desync_and_drops_the_client() {
    let Some(_) = fake() else { return };
    let dir = tempfile::Builder::new().prefix("pft").tempdir_in("/tmp").unwrap();
    // 50 ms caller timeout, bounded five-second drain floor. This peer never answers in time.
    let c = client(dir.path(), r#"{"slow": {"list-panes": 60}}"#, 50).await;
    let mut notes = c.notifications();
    assert!(matches!(c.command(&["list-panes"]).await, Err(Error::TimedOut(_))));
    let next = tokio::time::timeout(Duration::from_secs(6), c.command(&["display-message", "-p", "queued"])).await.unwrap();
    assert!(matches!(next, Err(Error::Disconnected | Error::Indeterminate(_))), "{next:?}");
    assert!(disconnected(&mut notes));
}
