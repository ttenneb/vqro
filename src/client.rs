//! Connecting to a running core.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::net::UnixStream;

use crate::rpc::{IncomingReceiver, Peer};

pub async fn connect(socket: &Path, client: &str) -> Result<(Peer, IncomingReceiver, Value)> {
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
    // Fail clearly before spawning/retrying a core whose predecessor still owns state.
    // Release this probe before the child acquires its lifetime lock.
    drop(crate::store::Store::lock_state(&state.join("workspace.json"))?);
    let log = std::fs::OpenOptions::new().create(true).append(true).open(state.join("serve.log"))?;
    let mut cmd = std::process::Command::new(executable_path()?);
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


/// Installed path, not the running inode: Linux appends ` (deleted)` after atomic replacement.
/// Preserve argv[0]'s symlink path so a new install can also replace the symlink's target.
pub fn executable_path() -> Result<std::path::PathBuf> {
    static PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    if let Some(path) = PATH.get() { return Ok(path.clone()); }
    let invoked = std::env::args_os().next().and_then(|arg| {
        let path = std::path::PathBuf::from(arg);
        if path.components().count() > 1 || path.is_absolute() {
            Some(if path.is_absolute() { path } else { std::env::current_dir().ok()?.join(path) })
        } else {
            std::env::split_paths(&std::env::var_os("PATH")?).map(|dir| dir.join(&path)).find(|path| executable_file(path))
        }
    });
    let path = match invoked.filter(|path| executable_file(path)) { Some(path) => path, None => normalize_executable(std::env::current_exe()?) };
    let _ = PATH.set(path.clone());
    Ok(PATH.get().cloned().unwrap_or(path))
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata)=std::fs::metadata(path) else { return false; };
    if !metadata.is_file() { return false; }
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; if metadata.permissions().mode() & 0o111 == 0 { return false; } }
    true
}

fn normalize_executable(path: std::path::PathBuf) -> std::path::PathBuf {
    #[cfg(target_os = "linux")]
    if !path.exists() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        if let Some(bytes) = path.as_os_str().as_bytes().strip_suffix(b" (deleted)") {
            return std::path::PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec()));
        }
    }
    path
}

/// This binary's build identity: its modification time in seconds.
pub fn build_stamp() -> Option<u64> {
    static BUILD: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *BUILD.get_or_init(|| {
        let exe = executable_path().ok()?;
        let m = std::fs::metadata(exe).ok()?.modified().ok()?;
        Some(m.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs())
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn deleted_inode_suffix_resolves_replaced_install_but_not_a_real_suffix_name() {
        let dir=tempfile::tempdir().unwrap(); let installed=dir.path().join("prim");
        std::fs::write(&installed,b"replacement").unwrap();
        assert_eq!(normalize_executable(dir.path().join("prim (deleted)")),installed);
        let literal=dir.path().join("literal (deleted)"); std::fs::write(&literal,b"real filename").unwrap();
        assert_eq!(normalize_executable(literal.clone()),literal);
    }
}
