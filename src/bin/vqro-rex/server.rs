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
        vqro::paths::state_dir().map_err(|e| e.to_string())?
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

const OWNED_MARKER: &str = "vqro-owned-rex-v1\n";

fn marked(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("vqro-owned")).is_ok_and(|s| s == OWNED_MARKER)
}

/// Coordinate vqro's owned-server attach/create/stop calls. File locking uses flock on Unix and
/// LockFileEx on Windows; only the Windows compile path is verified here. Never used for --rex-socket.
pub async fn lock(dir: &Path) -> Result<std::fs::File, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    let file = options.open(dir.join("vqro-rex.lock")).map_err(|e| e.to_string())?;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => tokio::time::sleep(Duration::from_millis(10)).await,
            Err(std::fs::TryLockError::Error(e)) => return Err(format!("owned Rex lock: {e}")),
        }
    }
}

/// A `rex` command for the owned server: environment cleared except selected inherited variables, `REX_RUNTIME_DIR`
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
/// running after its last session (exit-empty false). Its terminals inherit selected PATH/HOME/locale/shell/user
/// variables from this process, not a freshly acquired login environment or vqro's own (`VQRO_*`, `HERDR_*`).
pub fn ensure(bin: &Path, dir: &Path) -> Result<(), String> {
    if std::os::unix::net::UnixStream::connect(socket(dir)).is_ok() {
        return if marked(dir) { Ok(()) } else {
            Err("existing Rex endpoint is not marked vqro-owned; use --rex-socket for an external server".into())
        };
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let out = rex(bin, dir).args(["server", "start", "--json", "--state-dir"]).arg(dir.join("state")).output()
        .map_err(|e| format!("cannot run {}: {e}", bin.display()))?;
    if !out.status.success() {
        return Err(format!("rex server start failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let should_mark = !marked(dir);
    if should_mark {
        let reply: Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
        if reply["started"] != true {
            return Err("Rex did not report starting a new server; refusing to mark an existing server as owned".into());
        }
    }
    for _ in 0..50 {
        if std::os::unix::net::UnixStream::connect(socket(dir)).is_ok() {
            if should_mark { std::fs::write(dir.join("vqro-owned"), OWNED_MARKER).map_err(|e| e.to_string())?; }
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

/// Count a complete control snapshot before stop, not an atomic promise about clients arriving afterwards.
async fn inventory(dir: &Path) -> Result<(usize, usize), String> {
    let (control, _events) = vqro::rex::Control::connect(&socket(dir), "vqro-rex-stop-inventory", Duration::from_secs(10)).await.map_err(|e| e.to_string())?;
    let result = async {
        let list = control.call("session.list", json!({})).await.map_err(|e| e.to_string())?;
        let sessions = list["sessions"].as_array().ok_or("invalid session.list before stop")?;
        let mut terminals = 0;
        for session in sessions {
            let id = session["session_id"].as_str().filter(|s| !s.is_empty()).ok_or("invalid session id before stop")?;
            control.call("session.attach", json!({"session_id":id})).await.map_err(|e| e.to_string())?;
            let blocks = control.call("session.list_blocks", json!({"session_id":id})).await.map_err(|e| e.to_string())?;
            let blocks = blocks["blocks"].as_array().filter(|blocks| blocks.iter().all(|b|
                b["creator_name"].is_string() && b["block_id"].as_str().is_some_and(|s| !s.is_empty())))
                .ok_or("invalid block list before stop")?;
            terminals += blocks.iter().filter(|b| b["creator_name"] == "com.superlogical.terminal").count();
        }
        Ok((sessions.len(), terminals))
    }.await;
    control.close().await;
    result
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
    if !marked(&dir) {
        return Err("not a marked vqro-owned Rex directory; external --rex-socket servers cannot be administered here".into());
    }
    let before = status(&bin, &dir)?;
    if action == "status" {
        println!("{before}");
        return Ok(());
    }
    if before["running"] != true {
        return Err("no reachable vqro-owned rex server; nothing stopped".into());
    }
    let instance = before["instance"].as_str().filter(|s| !s.is_empty())
        .ok_or("rex returned no non-empty instance id; nothing stopped")?.to_string();
    let expected = match expected {
        Some(e) => e,
        None => {
            eprintln!("Stop the vqro-owned rex server ({} sessions) and end every program in its terminals? Type its instance {instance}:", before["sessions"]);
            let mut input = String::new();
            std::io::stdin().read_line(&mut input).map_err(|e| e.to_string())?;
            input.trim().to_string()
        }
    };
    // Do not hold the coordination lock while waiting for human confirmation.
    let _lock = lock(&dir).await?;
    let current = status(&bin, &dir)?;
    let current = current["instance"].as_str().filter(|s| !s.is_empty())
        .ok_or("rex returned no non-empty instance id on recheck; nothing stopped")?;
    if expected != instance || current != instance {
        return Err("instance did not match (or the server changed); nothing stopped".into());
    }
    let (sessions, terminals) = inventory(&dir).await?;
    let warning = "Clients outside vqro may attach/create after this check and would be stopped too; native stop is unconditional.";
    eprintln!("Checked {sessions} sessions, {terminals} terminals in the owned server. {warning}");
    // The file lock excludes vqro attach/create calls only. External clients and same-user replacement
    // remain a check-then-stop race: no conditional-stop API is evidenced.
    let out = rex(&bin, &dir).args(["server", "stop"]).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("rex server stop failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    println!("{}", json!({"stopped": true, "instance": instance, "checked":{"sessions":sessions,"terminals":terminals},"warning":warning}));
    Ok(())
}

#[cfg(all(test, unix))]
mod regression_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn stop_refuses_empty_instance_on_either_read() {
        for empty_first in [true, false] {
            let root = PathBuf::from(format!("/tmp/rxstop-{}-{empty_first}", std::process::id()));
            std::fs::create_dir_all(root.join("rex")).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::write(root.join("rex/vqro-owned"), OWNED_MARKER).unwrap();
            let listener = std::os::unix::net::UnixListener::bind(socket(&root.join("rex"))).unwrap();
            let bin = root.join("rex-cli");
            let first = if empty_first { "" } else { "instance" };
            // Fake CLI only; the socket never speaks to Rex. Second read always returns an empty id.
            std::fs::write(&bin, format!("#!/bin/sh\ncd '{}' || exit 1\ncase \"$*\" in\n*server\\ stop*) touch stopped;;\n*) if test -e read; then id=''; else id='{first}'; touch read; fi; printf '{{\"instance_id\":\"%s\",\"session_count\":0}}' \"$id\";;\nesac\n", root.display())).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
            let result = command(&["stop".into(), "--state-dir".into(), root.to_str().unwrap().into(),
                "--rex".into(), bin.to_str().unwrap().into(), "--expect-instance".into(), first.into()]).await;
            let stopped = root.join("stopped").exists();
            drop(listener);
            std::fs::remove_dir_all(&root).unwrap();
            assert!(result.is_err() && !stopped, "empty_first={empty_first}: must refuse before native stop");
        }
    }

    #[tokio::test]
    async fn owned_stop_waits_for_lock_before_rechecking() {
        let root = PathBuf::from(format!("/tmp/rxlock-{}", std::process::id()));
        std::fs::create_dir_all(root.join("rex")).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(root.join("rex/vqro-owned"), OWNED_MARKER).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(socket(&root.join("rex"))).unwrap();
        let bin = root.join("rex-cli");
        std::fs::write(&bin, format!("#!/bin/sh\ncd '{}' || exit 1\nif test -e first; then touch second; id=''; else touch first; id=instance; fi\nprintf '{{\"instance_id\":\"%s\",\"session_count\":0}}' \"$id\"\n", root.display())).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        let guard = lock(&root.join("rex")).await.unwrap();
        let args = vec!["stop".into(), "--state-dir".into(), root.to_str().unwrap().into(),
            "--rex".into(), bin.to_str().unwrap().into(), "--expect-instance".into(), "instance".into()];
        let stopping = tokio::spawn(async move { command(&args).await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !root.join("first").exists() { tokio::time::sleep(Duration::from_millis(1)).await; }
        }).await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!root.join("second").exists(), "instance recheck passed a contended lock");
        drop(guard);
        assert!(stopping.await.unwrap().is_err()); // second status deliberately has an empty id
        assert!(root.join("second").exists());
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn admin_refuses_an_unmarked_external_directory_and_endpoint_flag() {
        let root = PathBuf::from(format!("/tmp/rxexternal-{}", std::process::id()));
        std::fs::create_dir_all(root.join("rex")).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(socket(&root.join("rex"))).unwrap();
        let bin = root.join("fake-rex");
        std::fs::write(&bin, format!("#!/bin/sh\ntouch '{}/invoked'\n", root.display())).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = command(&["stop".into(), "--state-dir".into(), root.to_str().unwrap().into(),
            "--rex".into(), bin.to_str().unwrap().into(), "--expect-instance".into(), "instance".into()]).await;
        assert!(result.unwrap_err().contains("not a marked"));
        assert!(command(&["stop".into(), "--rex-socket".into(), "external".into()]).await.unwrap_err().contains("unknown"));
        assert!(!root.join("invoked").exists(), "unmarked fake server must never be administered");
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }
}
