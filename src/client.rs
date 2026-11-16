//! Connecting to a running core.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::rpc::{Incoming, Peer};

pub async fn connect(socket: &Path, client: &str) -> Result<(Peer, mpsc::UnboundedReceiver<Incoming>, Value)> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("cannot reach prim at {} (is `prim serve` running?)", socket.display()))?;
    let (r, w) = stream.into_split();
    let (peer, incoming) = Peer::spawn(r, w);
    let hello = peer.call("hello", json!({"client": client, "version": env!("CARGO_PKG_VERSION"), "build": build_stamp()})).await?;
    Ok((peer, incoming, hello))
}

/// Start `prim serve` in the background if nothing answers on the socket.
pub async fn ensure_core() -> Result<()> {
    let socket = crate::paths::socket_path()?;
    if tokio::net::UnixStream::connect(&socket).await.is_ok() {
        return Ok(());
    }
    let state = crate::paths::state_dir();
    std::fs::create_dir_all(&state)?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(state.join("serve.log"))?;
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.arg("serve").stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn()?;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if tokio::net::UnixStream::connect(&socket).await.is_ok() {
            return Ok(());
        }
    }
    anyhow::bail!("started `prim serve` but it isn't answering; see {}", state.join("serve.log").display())
}


/// This binary's build identity: its modification time in seconds.
pub fn build_stamp() -> Option<u64> {
    let exe = std::env::current_exe().ok()?;
    let m = std::fs::metadata(exe).ok()?.modified().ok()?;
    Some(m.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs())
}
