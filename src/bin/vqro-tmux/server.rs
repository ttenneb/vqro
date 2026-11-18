//! vqro-owned tmux server administration. No provider lifetime owns this server.
use std::path::{Path, PathBuf};
use serde_json::{json, Value};

pub async fn socket(explicit: Option<PathBuf>, standalone: bool) -> Result<PathBuf, String> {
    let state = if let Some(path) = explicit.or_else(|| std::env::var_os("VQRO_STATE_DIR").map(PathBuf::from)) {
        path
    } else if let Ok(endpoint) = std::env::var("VQRO_SOCKET") {
        // Public primitive: use the actual core state root, not the extension's HOME or cwd.
        let (peer, _, hello) = vqro::client::connect(Path::new(&endpoint), "tmux-server-location").await.map_err(|e| e.to_string())?;
        peer.close();
        hello["state_dir"].as_str().map(PathBuf::from).ok_or("tmux requires a persistent core state directory")?
    } else if standalone {
        vqro::paths::state_dir()
    } else {
        return Err("cannot locate vqro state: VQRO_SOCKET or VQRO_STATE_DIR is required".into());
    };
    if !state.is_absolute() {
        return Err("tmux requires an absolute vqro state directory".into());
    }
    let socket = state.join("tmux.sock");
    if socket.as_os_str().len() >= 100 {
        return Err("vqro state directory is too long for tmux; use a shorter VQRO_STATE_DIR (socket must be under 100 bytes)".into());
    }
    Ok(socket)
}

pub fn ensure(bin: &PathBuf, socket: &PathBuf) -> Result<(), String> {
    if super::tmux_cli(bin, socket, &["list-sessions"]).is_ok() {
        return Ok(());
    }
    std::fs::create_dir_all(socket.parent().ok_or("missing state directory")?).map_err(|e| e.to_string())?;
    // tmux owns creation and stale-socket handling. Never remove a live server's socket ourselves.
    // A control client needs a session. Keep a marked, exited native pane, never a login shell.
    // One tmux command queue installs remain-on-exit and the marker before child exit is handled.
    // Existing unmarked panes are user work and are never retroactively hidden or killed.
    match super::tmux_run(bin, socket, &[], &["new-session", "-d", "-s", "vqro", "-n", "vqro-bootstrap", "/usr/bin/true", "vqro-bootstrap",
        ";", "set-option", "-w", "-t", "vqro:", "remain-on-exit", "on",
        ";", "set-option", "-p", "-t", "vqro:", "@vqro_bootstrap", "1"]) {
        Ok(_) => Ok(()),
        Err(error) => {
            // Another provider may have won startup. Reuse its server, never kill/recreate it.
            super::tmux_cli(bin, socket, &["list-sessions"]).map(|_| ()).map_err(|_| error)
        }
    }
}

fn status(bin: &PathBuf, socket: &PathBuf) -> Result<Value, String> {
    if !socket.try_exists().map_err(|e| format!("cannot inspect owned server endpoint: {e}"))? {
        return Ok(json!({"running": false, "terminals": 0, "socket": socket}));
    }
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|e| e.to_string())?;
    let nonce: String = random.iter().map(|b| format!("{b:02x}")).collect();
    match super::tmux_cli(bin, socket, &["set-option", "-s", "-o", "@vqro_control_instance", &nonce]) {
        Ok(_) => {},
        Err(error) if error == "already set: @vqro_control_instance" => {},
        // tmux leaves the filesystem socket after kill-server; existence is not liveness.
        Err(error) if error == format!("no server running on {}", socket.display()) => {
            return Ok(json!({"running": false, "terminals": 0, "socket": socket}));
        }
        Err(error) => return Err(error),
    }
    // One native connection: identity and pane inventory cannot come from different servers.
    let output = super::tmux_cli(bin, socket, &["display-message", "-p", "#{pid}:#{start_time}:#{@vqro_control_instance}",
        ";", "list-panes", "-a", "-F", "#{?@vqro_bootstrap,,#{pane_id}}"])?;
    let mut lines = output.lines();
    let instance = lines.next().ok_or("missing server identity")?;
    if instance.rsplit(':').next().is_none_or(|n| n.len() != 32 || !n.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err("missing server-lifetime nonce".into());
    }
    let terminals = lines.filter(|line| !line.is_empty()).collect::<std::collections::BTreeSet<_>>().len();
    Ok(json!({"running": true, "terminals": terminals, "instance": instance, "socket": socket}))
}

pub async fn command(args: &[String]) -> Result<(), String> {
    let action = args.first().map(String::as_str).ok_or("usage: vqro-tmux server status|stop [--state-dir PATH] [--tmux BIN]")?;
    if !matches!(action, "status" | "stop") { return Err("expected server status or stop".into()); }
    let mut bin: PathBuf = "tmux".into();
    let mut state = None;
    let mut expected = None;
    let mut count = None;
    let mut it = args.iter().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--state-dir" => state = Some(PathBuf::from(value)),
            "--tmux" => bin = value.into(),
            "--expect-instance" => expected = Some(value.clone()),
            "--confirm-terminals" => count = Some(value.parse::<u64>().map_err(|_| "invalid terminal count")?),
            _ => return Err(format!("unknown server argument {flag}")),
        }
    }
    let socket = socket(state, true).await?;
    let before = status(&bin, &socket)?;
    if action == "status" { println!("{before}"); return Ok(()); }
    if before["running"] != true { return Err("no reachable vqro-owned server; no stop submitted".into()); }
    if expected.is_none() && count.is_none() {
        let n = before["terminals"].as_u64().ok_or("missing terminal count")?;
        eprintln!("Stop the vqro-owned tmux server and kill its {n} terminals? Type STOP {n}:");
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).map_err(|e| e.to_string())?;
        if input.trim() != format!("STOP {n}") { return Err("confirmation did not match; no stop submitted".into()); }
        expected = before["instance"].as_str().map(String::from);
        count = Some(n);
    }
    let expected = expected.ok_or("--expect-instance and --confirm-terminals must be supplied together")?;
    let count = count.ok_or("--expect-instance and --confirm-terminals must be supplied together")?;
    let current = status(&bin, &socket)?;
    if current["instance"] != expected || current["terminals"] != count {
        return Err("server instance or terminal count changed; confirm again (no stop submitted)".into());
    }
    let nonce = expected.rsplit(':').next().ok_or("missing nonce")?;
    // The final identity check and kill execute on the SAME tmux server queue. Never kill a
    // replacement server just because it acquired the same filesystem endpoint during confirmation.
    let reply = super::tmux_cli(&bin, &socket, &["if-shell", "-F",
        &format!("#{{==:#{{@vqro_control_instance}},{nonce}}}"), "kill-server", "display-message -p vqro_instance_changed"])
        .map_err(|error| format!("indeterminate stop: {error}; do not automatically retry"))?;
    if reply.trim() == "vqro_instance_changed" { return Err("server instance changed; no stop submitted".into()); }
    println!("{}", json!({"outcome": "accepted", "instance": expected, "terminals": count}));
    Ok(())
}
