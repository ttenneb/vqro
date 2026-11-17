//! Command timeouts in prim::tmux against a deterministic fake control-mode tmux
//! (`tests/fixtures/fake_tmux_cc.py`): a read-only command that times out is not fatal and its late
//! reply never pairs with a later command; a mutating one is (outcome unknown); a late reply that
//! never comes is a desync.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use prim::tmux::{Client, Config, Error, Notification};
use tokio::sync::broadcast;

fn fake() -> Option<PathBuf> {
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_tmux_cc.py");
    Path::new("/usr/bin/python3").exists().then_some(fake)
}

async fn client(dir: &Path, conf: &str, timeout_ms: u64) -> Client {
    let socket = dir.join("s");
    std::fs::write(dir.join("s.conf"), conf).unwrap();
    let mut config = Config::new(socket, "test");
    config.binary = fake().unwrap();
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
    let Some(_) = fake() else { return eprintln!("skipped: no /usr/bin/python3") };
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
async fn mutating_timeout_is_indeterminate_and_drops_the_client() {
    let Some(_) = fake() else { return };
    let dir = tempfile::Builder::new().prefix("pft").tempdir_in("/tmp").unwrap();
    let c = client(dir.path(), r#"{"slow": {"kill-pane": 0.6}}"#, 200).await;
    let mut notes = c.notifications();
    assert!(matches!(c.command(&["kill-pane", "-t", "%1"]).await, Err(Error::Indeterminate(_))));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(disconnected(&mut notes));
    assert!(matches!(c.command(&["display-message", "-p", "x"]).await, Err(Error::Disconnected)));
}

#[tokio::test]
async fn a_late_reply_that_never_comes_is_a_desync_and_drops_the_client() {
    let Some(_) = fake() else { return };
    let dir = tempfile::Builder::new().prefix("pft").tempdir_in("/tmp").unwrap();
    // 50 ms timeout: the late reply may take 10 more timeouts (0.5 s); this one takes 3 s.
    let c = client(dir.path(), r#"{"slow": {"list-panes": 3}}"#, 50).await;
    let mut notes = c.notifications();
    assert!(matches!(c.command(&["list-panes"]).await, Err(Error::TimedOut(_))));
    let next = tokio::time::timeout(Duration::from_secs(2), c.command(&["display-message", "-p", "queued"])).await.unwrap();
    assert!(matches!(next, Err(Error::Disconnected | Error::Indeterminate(_))), "{next:?}");
    assert!(disconnected(&mut notes));
}
