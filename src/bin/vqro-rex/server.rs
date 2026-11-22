//! The vqro-owned Rex server: its place, start and administration. No provider lifetime owns this server: it
//! outlives the provider, safe mode and core restarts (D14), like vqro's owned tmux server, and stops only on an
//! explicit, confirmed `vqro-rex server stop`. The user's own Rex server (`--rex-socket`) is never started or stopped.
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

/// The runtime directory (sockets) of the owned server: `<vqro state>/rex`. Rex puts `server.sock` there when
/// `REX_RUNTIME_DIR` names it; its saved sessions go to `<vqro state>/rex/state`.
pub async fn dir(explicit: Option<PathBuf>, standalone: bool) -> Result<PathBuf, String> {
    let state = if let Some(path) = explicit.or_else(|| std::env::var_os("VQRO_STATE_DIR").map(PathBuf::from)) {
        path
    } else if let Ok(endpoint) = std::env::var("VQRO_SOCKET") {
        // Public primitive: the actual core state root, not the extension's HOME or cwd.
        let (peer, _, hello) = vqro::client::connect_with(Path::new(&endpoint), vqro::client::supervision_hello(json!({"client":"rex-server-location","version":env!("CARGO_PKG_VERSION"),"build":vqro::client::build_stamp()}))).await.map_err(|e| e.to_string())?;
        peer.close();
        hello["state_dir"].as_str().map(PathBuf::from).ok_or("rex requires a persistent core state directory")?
    } else if standalone {
        vqro::paths::state_dir()
    } else {
        return Err("cannot locate vqro state: VQRO_SOCKET or VQRO_STATE_DIR is required".into());
    };
    if !state.is_absolute() {
        return Err("rex requires an absolute vqro state directory".into());
    }
    let dir = state.join("rex");
    // Unix socket paths are limited (about 104 bytes on macOS); Rex adds `login-server.sock` too.
    if dir.join("login-server.sock").as_os_str().len() >= 100 {
        return Err("vqro state directory is too long for rex's sockets; use a shorter VQRO_STATE_DIR".into());
    }
    Ok(dir)
}

pub fn socket(dir: &Path) -> PathBuf {
    dir.join("server.sock")
}

/// A `rex` command for the owned server: environment cleared except what a login session needs, `REX_RUNTIME_DIR`
/// set, and never autostarting a server anywhere else.
pub fn rex(bin: &Path, dir: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(bin);
    cmd.env_clear();
    for k in ["PATH", "HOME", "LANG", "SHELL", "USER", "LOGNAME"] {
        if let Some(v) = std::env::var_os(k) {
            cmd.env(k, v);
        }
    }
    if std::env::var_os("PATH").is_none() {
        cmd.env("PATH", "/usr/bin:/bin");
    }
    cmd.env("REX_RUNTIME_DIR", dir).arg("--autostart=false");
    cmd
}

/// Start the owned server if nothing answers on its socket. `rex server start` is idempotent and keeps the server
/// running after its last session (exit-empty false). Its terminals inherit this environment (the same variables a
/// login session has), not vqro's own (`VQRO_*`, `HERDR_*`).
pub fn ensure(bin: &Path, dir: &Path) -> Result<(), String> {
    if std::os::unix::net::UnixStream::connect(socket(dir)).is_ok() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let out = rex(bin, dir).args(["server", "start", "--json", "--state-dir"]).arg(dir.join("state")).output()
        .map_err(|e| format!("cannot run {}: {e}", bin.display()))?;
    if !out.status.success() {
        return Err(format!("rex server start failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    for _ in 0..50 {
        if std::os::unix::net::UnixStream::connect(socket(dir)).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("rex server start reported success but its socket does not answer".into())
}

fn status(bin: &Path, dir: &Path) -> Result<Value, String> {
    if std::os::unix::net::UnixStream::connect(socket(dir)).is_err() {
        return Ok(json!({"running": false, "socket": socket(dir)}));
    }
    let out = rex(bin, dir).args(["api", "call", "server.status", "{}"]).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("rex server.status failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let s: Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    Ok(json!({"running": true, "socket": socket(dir), "instance": s["instance_id"], "sessions": s["session_count"],
              "version": s["version"]}))
}

/// `vqro-rex server status|stop [--state-dir PATH] [--rex BIN] [--expect-instance ID]`. Stop ends every program
/// in the owned server's terminals, so it asks for the instance shown by status (typed, or `--expect-instance`).
pub async fn command(args: &[String]) -> Result<(), String> {
    let action = args.first().map(String::as_str).ok_or("usage: vqro-rex server status|stop [--state-dir PATH] [--rex BIN]")?;
    if !matches!(action, "status" | "stop") {
        return Err("expected server status or stop".into());
    }
    let (mut bin, mut state, mut expected): (PathBuf, Option<PathBuf>, Option<String>) = ("rex".into(), None, None);
    let mut it = args.iter().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--state-dir" => state = Some(PathBuf::from(value)),
            "--rex" => bin = value.into(),
            "--expect-instance" => expected = Some(value.clone()),
            _ => return Err(format!("unknown server argument {flag}")),
        }
    }
    let dir = dir(state, true).await?;
    let before = status(&bin, &dir)?;
    if action == "status" {
        println!("{before}");
        return Ok(());
    }
    if before["running"] != true {
        return Err("no reachable vqro-owned rex server; nothing stopped".into());
    }
    let instance = before["instance"].as_str().unwrap_or_default().to_string();
    let expected = match expected {
        Some(e) => e,
        None => {
            eprintln!("Stop the vqro-owned rex server ({} sessions) and end every program in its terminals? Type its instance {instance}:", before["sessions"]);
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).map_err(|e| e.to_string())?;
            input.trim().to_string()
        }
    };
    if expected != instance || status(&bin, &dir)?["instance"] != instance {
        return Err("instance did not match (or the server changed); nothing stopped".into());
    }
    let out = rex(&bin, &dir).args(["server", "stop"]).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("rex server stop failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    println!("{}", json!({"stopped": true, "instance": instance}));
    Ok(())
}
