//! Owned-server lifecycle regressions. Every effect is under this test's private state root.
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use vqro::{client, rpc::Peer};
use serde_json::{json, Value};

pub struct ServerCleanup(pub PathBuf);
impl Drop for ServerCleanup {
    fn drop(&mut self) {
        // Panic cleanup is still an explicit stop, never a global process scan or default server.
        let _ = Command::new("/usr/bin/tmux").env_clear().args(["-N", "-S"])
            .arg(self.0.join("tmux.sock")).args(["kill-server"]).output();
    }
}

fn helper(state: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_vqro-tmux")).env_clear().env("HOME", state).env("PATH", "/usr/bin:/bin")
        .env("VQRO_STATE_DIR", state).args(["server"]).args(args).args(["--tmux", "/usr/bin/tmux"]).output().unwrap()
}
fn status(state: &Path) -> Value {
    let output = helper(state, &["status"]);
    assert!(output.status.success(), "status: {}", String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}
pub fn stop(state: &Path) {
    let before = status(state);
    assert_eq!(before["running"], true);
    let count = before["terminals"].to_string();
    let output = helper(state, &["stop", "--expect-instance", before["instance"].as_str().unwrap(), "--confirm-terminals", &count]);
    assert!(output.status.success(), "stop: {}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(serde_json::from_slice::<Value>(&output.stdout).unwrap()["outcome"], "accepted");
    wait_stopped(state);
}
fn wait_stopped(state: &Path) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        // A read may race native teardown ("server exited unexpectedly"). Retry observation,
        // never the destructive command, and require an affirmative not-running observation.
        let output = helper(state, &["status"]);
        if output.status.success() && serde_json::from_slice::<Value>(&output.stdout).unwrap()["running"] == false { return; }
        assert!(Instant::now() < deadline, "tmux still answers or cannot be observed after explicit stop");
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct Running {
    child: Option<Child>,
    _server: ServerCleanup,
    root: tempfile::TempDir,
}
impl Running {
    fn start(&mut self) {
        assert!(self.child.is_none());
        self.child = Some(Command::new(env!("CARGO_BIN_EXE_vqro")).env_clear().args(["serve", "--fake"])
            .env("HOME", self.root.path()).env("PATH", "/usr/bin:/bin")
            .env("XDG_STATE_HOME", self.root.path()) // provider must discover the actual root through hello
            .env("VQRO_SOCKET", self.root.path().join("p.sock"))
            .env("VQRO_EXTENSIONS_DIR", self.root.path().join("extensions"))
            .env("VQRO_TMUX_HELPER", env!("CARGO_BIN_EXE_vqro-tmux"))
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
    }
    async fn peer(&self) -> Peer {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok((peer, _, _)) = client::connect(&self.root.path().join("p.sock"), "tmux-lifetime-test").await { return peer; }
            assert!(Instant::now() < deadline, "private core did not start");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    fn reap(&mut self, kill: bool) {
        let mut child = self.child.take().unwrap();
        if kill { child.kill().unwrap(); }
        let deadline = Instant::now() + Duration::from_secs(6);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline { let _ = child.kill(); let _ = child.wait(); panic!("private core failed to exit"); }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Running {
    fn drop(&mut self) { if let Some(child) = &mut self.child { let _ = child.kill(); let _ = child.wait(); } }
}
async fn command(peer: &Peer, name: &str, args: Value) -> Value {
    peer.call("command.invoke", json!({"name": name, "args": args})).await.unwrap()
}
async fn resource(peer: &Peer) -> Value {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let reply = peer.call("resource.list", json!({})).await.unwrap();
        if let Some(resource) = reply["resources"].as_array().unwrap().iter().find(|r| r["provider"] == "tmux-lifetime" && r["available"] == true) {
            return resource.clone();
        }
        assert!(Instant::now() < deadline, "tmux resource did not become available: {reply}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}
async fn spawn_resource(peer: &Peer) -> Value {
    let deadline=Instant::now()+Duration::from_secs(5);
    loop {
        let snap=peer.call("workspace.snapshot",json!({})).await.unwrap();
        if snap["providers"]["tmux-lifetime"]["synced"]==true {break;}
        assert!(Instant::now()<deadline,"tmux provider did not sync");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    peer.call("resource.spawn",json!({"provider":"tmux-lifetime","argv":["/bin/sh"]})).await.unwrap();
    resource(peer).await
}
fn live(pid: u64) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).ok().and_then(|s| s.rsplit_once(')').map(|(_, tail)| !tail.trim_start().starts_with('Z'))).unwrap_or(false)
}

#[tokio::test]
#[ignore]
async fn owned_server_survives_disable_safe_mode_crash_and_reinstall_like_restart() {
    let root = tempfile::Builder::new().prefix("ptlife").tempdir_in("/tmp").unwrap();
    std::fs::create_dir(root.path().join("extensions")).unwrap();
    let state = root.path().join("vqro");
    let mut running = Running { child: None, _server: ServerCleanup(state.clone()), root };
    running.start();
    let peer = running.peer().await;
    peer.call("workspace.apply", json!({"ops":[{"op":"set", "key":"extension/tmux", "expect":0,
        "value":{"enabled":true,"kind":"provider","argv":[env!("CARGO_BIN_EXE_vqro-tmux"),"--tmux","/usr/bin/tmux","--name","tmux-lifetime"]}}]})).await.unwrap();
    let deadline = Instant::now()+Duration::from_secs(5);
    loop {
        let snap=peer.call("workspace.snapshot",json!({})).await.unwrap();
        if snap["providers"]["tmux-lifetime"]["synced"]==true {break;}
        assert!(Instant::now()<deadline,"tmux provider did not sync");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let inventory=peer.call("resource.list",json!({})).await.unwrap();
    assert!(!inventory["resources"].as_array().unwrap().iter().any(|r|r["provider"]=="tmux-lifetime"),"bootstrap must not become a shell/tab: {inventory}");
    assert_eq!(status(&state)["terminals"],0,"bootstrap is not a user terminal");
    let bootstrap=Command::new("/usr/bin/tmux").env_clear().args(["-N","-S"]).arg(state.join("tmux.sock"))
        .args(["list-panes","-a","-F","#{pane_dead}:#{@vqro_bootstrap}"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&bootstrap.stdout).trim(),"1:1","internal pane has no running shell");
    peer.call("resource.spawn",json!({"provider":"tmux-lifetime","argv":["/bin/sh"]})).await.unwrap();
    let first = resource(&peer).await;
    let pane_pid = first["data"]["pid"].as_u64().unwrap();
    let initial = status(&state);
    assert_eq!(initial["terminals"], 1);
    assert_eq!(first["instance"], initial["instance"], "helper and provider disagree on server identity");

    command(&peer, "vqro.extension.disable", json!({"name":"tmux"})).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(live(pane_pid), "disabling the extension killed its backend terminal");
    assert_eq!(status(&state)["instance"], initial["instance"]);
    command(&peer, "vqro.extension.enable", json!({"name":"tmux"})).await;
    assert_eq!(resource(&peer).await["id"], first["id"]);

    command(&peer, "vqro.safe_mode", json!({"on":true})).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(live(pane_pid), "safe mode killed the backend terminal");
    command(&peer, "vqro.safe_mode", json!({"on":false})).await;
    assert_eq!(resource(&peer).await["id"], first["id"]);

    // Harness-local identity only: public extension facts are platform-neutral, not process ids.
    let expected=std::fs::canonicalize(env!("CARGO_BIN_EXE_vqro-tmux")).unwrap();
    let mut provider_pid=None;
    for thread in std::fs::read_dir(format!("/proc/{}/task",running.child.as_ref().unwrap().id())).unwrap().flatten() {
        let Ok(children)=std::fs::read_to_string(thread.path().join("children")) else {continue};
        for child in children.split_whitespace() {if std::fs::read_link(format!("/proc/{child}/exe")).ok().as_ref()==Some(&expected){provider_pid=child.parse::<u64>().ok();}}
    }
    let provider_pid=provider_pid.expect("owned core's vqro-tmux child");
    assert!(Command::new("/bin/kill").env_clear().args(["-KILL", &provider_pid.to_string()]).status().unwrap().success());
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(live(pane_pid), "provider SIGKILL killed the terminal");
    assert_eq!(resource(&peer).await["id"], first["id"]);

    // Reinstall-like stop/new-process restart. Neither executable nor provider lifetime owns tmux.
    peer.call("core.shutdown", json!({})).await.unwrap();
    running.reap(false);
    assert!(live(pane_pid));
    running.start();
    let peer = running.peer().await;
    assert_eq!(resource(&peer).await["id"], first["id"]);
    assert_eq!(status(&state)["instance"], initial["instance"]);
    running.reap(true); // abrupt core death closes the provider's inherited lifetime pipe
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(live(pane_pid));
    running.start();
    let peer = running.peer().await;
    assert_eq!(resource(&peer).await["id"], first["id"]);

    let bad = helper(&state, &["stop", "--expect-instance", initial["instance"].as_str().unwrap(), "--confirm-terminals", "99"]);
    assert!(!bad.status.success());
    assert!(live(pane_pid), "wrong confirmation killed a terminal");
    stop(&state);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!live(pane_pid), "explicit stop left the terminal running");
    assert_eq!(status(&state)["running"], false, "provider silently recreated a stopped server");
    command(&peer, "vqro.extension.disable", json!({"name":"tmux"})).await;
    command(&peer, "vqro.extension.enable", json!({"name":"tmux"})).await;
    let fresh = spawn_resource(&peer).await;
    assert_ne!(fresh["id"], first["id"], "new server reused an old terminal identity");
    let replaced = helper(&state, &["stop", "--expect-instance", initial["instance"].as_str().unwrap(), "--confirm-terminals", "1"]);
    assert!(!replaced.status.success(), "old confirmation killed a replacement server");
    stop(&state);
    peer.call("core.shutdown", json!({})).await.unwrap();
    running.reap(false);
}

#[tokio::test]
#[ignore]
async fn independent_controls_stop_offline_provider_and_persist_enabled_provider_off() {
    let root = tempfile::Builder::new().prefix("ptctrl").tempdir_in("/tmp").unwrap();
    std::fs::create_dir(root.path().join("extensions")).unwrap();
    let state = root.path().join("vqro");
    let mut running = Running { child: None, _server: ServerCleanup(state.clone()), root };
    running.start();
    let peer = running.peer().await;
    peer.call("workspace.apply", json!({"ops":[
        {"op":"set","key":"extension/tmux","expect":0,"value":{"enabled":true,"kind":"provider",
            "argv":[env!("CARGO_BIN_EXE_vqro-tmux"),"--tmux","/usr/bin/tmux","--name","tmux-lifetime"]}},
        {"op":"set","key":"extension/tmux-server","expect":0,"value":{"enabled":true,
            "argv":["/usr/bin/python3",Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions/tmux_server.py")]}}
    ]})).await.unwrap();
    let initial = spawn_resource(&peer).await;
    command(&peer, "vqro.extension.disable",  json!({"name":"tmux"})).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let observed = command(&peer, "tmux.server.status", json!({})).await;
    assert_eq!(observed["running"], true, "status required a connected provider");
    assert!(command(&peer, "tmux.server.stop", json!({})).await["client"]["popup"].is_object());
    let rejected = peer.call("command.invoke", json!({"name":"tmux.server.stop", "args":{
        "instance":observed["instance"],"terminals":1,"text":"yes"}})).await;
    assert!(rejected.is_err());
    assert!(live(initial["data"]["pid"].as_u64().unwrap()));
    let stopped = command(&peer, "tmux.server.stop", json!({"instance":observed["instance"],"terminals":1,"text":"STOP 1"})).await;
    assert_eq!(stopped["outcome"], "accepted", "stop required a connected provider");
    wait_stopped(&state);
    assert_eq!(command(&peer,"tmux.server.status",json!({})).await["running"], false);
    command(&peer, "vqro.extension.enable", json!({"name":"tmux"})).await;
    spawn_resource(&peer).await;
    let fresh = command(&peer, "tmux.server.status", json!({})).await;
    assert_ne!(fresh["instance"], observed["instance"]);
    assert_eq!(command(&peer,"tmux.server.stop",json!({"instance":fresh["instance"],"terminals":1,"text":"STOP 1"})).await["outcome"],"accepted");
    let snapshot = peer.call("workspace.snapshot",json!({})).await.unwrap();
    assert_eq!(snapshot["entries"]["extension/tmux"]["value"]["enabled"],false,"stop failed to persist provider off");
    peer.call("core.shutdown",json!({})).await.unwrap();
    running.reap(false);
    running.start();
    let peer = running.peer().await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(status(&state)["running"],false,"core restart recreated explicitly stopped server");
    assert_eq!(command(&peer,"tmux.server.stop",json!({})).await["outcome"],"not_submitted");
    peer.call("core.shutdown",json!({})).await.unwrap();
    running.reap(false);
}
