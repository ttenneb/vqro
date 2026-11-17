//! prim-tmux against an in-test fake core (provider-v0 section 9) and a private tmux 3.7b server.
//! Opt-in: cargo test --test tmux_provider -- --ignored
//! Every tmux call uses this test's own -S socket and allowlisted environment; Drop kills only it.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixListener;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

struct Tmux {
    root: tempfile::TempDir,
    socket: PathBuf,
}

impl Tmux {
    fn start() -> Tmux {
        let out = Command::new("/usr/bin/tmux").arg("-V").env_clear().output().unwrap();
        assert_eq!(out.stdout, b"tmux 3.7b\n", "these tests pin tmux 3.7b");
        let root = tempfile::Builder::new().prefix("ptp").tempdir_in("/tmp").unwrap();
        let t = Tmux { socket: root.path().join("s"), root };
        // -N (used by cli) refuses to start a server; this one call starts it.
        let ok = Command::new("/usr/bin/tmux")
            .arg("-S")
            .arg(&t.socket)
            .args(["-f", "/dev/null", "new-session", "-d", "-s", "work", "-x", "100", "-y", "30", "/bin/sh"])
            .env_clear()
            .env("HOME", t.root.path())
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "xterm-256color")
            .output()
            .unwrap();
        assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
        t.cli(&["set-option", "-g", "status", "off"]);
        t
    }
    fn cli(&self, args: &[&str]) -> std::process::Output {
        Command::new("/usr/bin/tmux")
            .args(["-N", "-S"])
            .arg(&self.socket)
            .args(["-f", "/dev/null"])
            .args(args)
            .env_clear()
            .env("HOME", self.root.path())
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "xterm-256color")
            .output()
            .unwrap()
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        let _ = self.cli(&["kill-server"]);
    }
}

struct Core {
    r: BufReader<OwnedReadHalf>,
    w: OwnedWriteHalf,
    /// Messages read while waiting for something else, in order.
    backlog: Vec<Value>,
}

impl Core {
    async fn read(&mut self) -> Value {
        let mut line = String::new();
        let n = tokio::time::timeout(Duration::from_secs(10), self.r.read_line(&mut line)).await.expect("provider message in time").unwrap();
        assert!(n > 0, "provider disconnected");
        serde_json::from_str(&line).unwrap()
    }
    async fn send(&mut self, v: Value) {
        self.w.write_all(format!("{v}\n").as_bytes()).await.unwrap();
    }
    async fn reply(&mut self, req: &Value, result: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": req["id"], "result": result})).await;
    }
    /// The next message matching `pred` (earlier ones go to the backlog).
    async fn until(&mut self, mut pred: impl FnMut(&Value) -> bool) -> Value {
        if let Some(i) = self.backlog.iter().position(&mut pred) {
            return self.backlog.remove(i);
        }
        loop {
            let m = self.read().await;
            if pred(&m) {
                return m;
            }
            self.backlog.push(m);
        }
    }
}

struct Provider(std::process::Child);
impl Drop for Provider {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn start(tmux: &Tmux) -> (Core, Provider) {
    let endpoint = tmux.root.path().join("core.sock");
    let listener = UnixListener::bind(&endpoint).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_prim-tmux"))
        .args(["--tmux", "/usr/bin/tmux", "--tmux-socket"])
        .arg(&tmux.socket)
        .args(["--name", "tmux@test"])
        .env_clear()
        .env("HOME", tmux.root.path())
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "xterm-256color")
        .env("PRIM_SOCKET", &endpoint)
        .spawn()
        .unwrap();
    let provider = Provider(child);
    let (sock, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept()).await.unwrap().unwrap();
    let (r, w) = sock.into_split();
    let mut core = Core { r: BufReader::new(r), w, backlog: Vec::new() };
    let hello = core.read().await;
    assert_eq!(hello["method"], "hello");
    core.reply(&hello, json!({"server": "fake-core"})).await;
    (core, provider)
}

fn data_of(m: &Value) -> Vec<u8> {
    B64.decode(m["params"]["data"].as_str().unwrap()).unwrap()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test]
#[ignore]
async fn registers_syncs_spawns_streams_status_and_kills_on_real_tmux() {
    let tmux = Tmux::start();
    let (mut core, _provider) = start(&tmux).await;

    let reg = core.read().await;
    assert_eq!(reg["method"], "provider.register");
    let p = &reg["params"];
    assert_eq!((p["name"].as_str(), p["role"].as_str()), (Some("tmux@test"), Some("backend")));
    assert_eq!(p["software"], json!({"name": "tmux", "version": "3.7b"}));
    let caps = &p["capabilities"];
    assert_eq!(caps["status.osc7501"]["lossy"], true);
    assert!(caps["layout.native"].is_object());
    assert!(caps.get("events.resync").is_none(), "absent means unsupported; never false (9.2)");
    assert!(caps.as_object().unwrap().values().all(Value::is_object));
    let instance = p["instance"].as_str().unwrap().to_string();
    assert_eq!(instance.rsplit(':').next().unwrap().len(), 32, "server-lifetime nonce in the instance");
    core.reply(&reg, json!({"provider": "tmux@test", "generation": 3, "limits": {"stream_window_bytes": 65536, "max_record_bytes": 1048576}})).await;

    let sync = core.read().await;
    assert_eq!((sync["method"].as_str(), sync["params"]["generation"].as_u64(), sync["params"]["final"].as_bool()), (Some("provider.sync"), Some(3), Some(true)));
    let panes = sync["params"]["resources"].as_array().unwrap().clone();
    assert_eq!(panes.len(), 1);
    assert_eq!((panes[0]["kind"].as_str(), panes[0]["state"].as_str()), (Some("terminal"), Some("live")));
    assert_eq!(panes[0]["data"]["session"], "work");
    core.reply(&sync, json!({})).await;

    // Spawn: observed, with facts the core can map; the pane is also upserted.
    core.send(json!({"jsonrpc": "2.0", "id": 10, "method": "effect.invoke", "params": {"generation": 3, "effect": "e1",
        "op": "terminal.spawn", "args": {"argv": ["/bin/sh"], "name": "spawned", "env": {"PRIM_TEST": "yes"}}, "deadline_ms": 10000}})).await;
    let r = core.until(|m| m["id"] == 10).await;
    assert_eq!(r["result"]["outcome"], "observed", "{r}");
    let facts = &r["result"]["data"]["facts"];
    let pane = facts["ref"].as_str().unwrap().to_string();
    assert_eq!((facts["name"].as_str(), facts["argv"][0].as_str()), (Some("spawned"), Some("/bin/sh")));
    core.until(|m| m["method"] == "resource.upsert" && m["params"]["facts"]["ref"] == pane.as_str()).await;
    // Same effect id again: conflict, not a second window.
    core.send(json!({"jsonrpc": "2.0", "id": 11, "method": "effect.invoke", "params": {"generation": 3, "effect": "e1",
        "op": "terminal.spawn", "args": {"argv": ["/bin/sh"]}, "deadline_ms": 10000}})).await;
    assert_eq!(core.until(|m| m["id"] == 11).await["error"]["data"]["kind"], "conflict");

    // Stream: repaint first, then live output; input reaches the pane; OSC 7501 becomes status.
    core.send(json!({"jsonrpc": "2.0", "id": 20, "method": "stream.open", "params": {"generation": 3, "stream": 1, "ref": pane,
        "incarnation": "", "cols": 100, "rows": 30, "history_lines": 0, "window_bytes": 65536}})).await;
    assert_eq!(core.until(|m| m["id"] == 20).await["result"]["initial"], "repaint");
    let first = core.until(|m| m["method"] == "stream.data").await;
    assert_eq!(first["params"]["offset"], 0);
    assert!(data_of(&first).starts_with(b"\x1b[H\x1b[2J"), "the repaint is labelled by a clear, not replayed output");
    let mut offset = data_of(&first).len() as u64;
    let input = "printf '\\033]7501;{\"state\":\"working\"}\\007'; echo prim-$((40+2))\r";
    core.send(json!({"jsonrpc": "2.0", "method": "stream.input", "params": {"generation": 3, "stream": 1, "data": B64.encode(input)}})).await;
    let mut seen = Vec::new();
    while !contains(&seen, b"prim-42") {
        let d = core.until(|m| m["method"] == "stream.data").await;
        assert_eq!(d["params"]["offset"].as_u64(), Some(offset), "contiguous offsets");
        offset += data_of(&d).len() as u64;
        seen.extend(data_of(&d));
        core.send(json!({"jsonrpc": "2.0", "method": "stream.credit", "params": {"generation": 3, "stream": 1, "bytes": data_of(&d).len()}})).await;
    }
    let status = core.until(|m| m["method"] == "resource.upsert" && m["params"]["facts"]["status"]["state"] == "working").await;
    assert_eq!(status["params"]["facts"]["ref"], pane.as_str());

    // Kill: observed finished, the open stream closes, the resource goes with reason killed.
    core.send(json!({"jsonrpc": "2.0", "id": 30, "method": "effect.invoke", "params": {"generation": 3, "effect": "e2",
        "op": "terminal.kill", "ref": pane, "incarnation": "", "args": {}, "deadline_ms": 10000}})).await;
    let r = core.until(|m| m["id"] == 30).await;
    assert_eq!((r["result"]["outcome"].as_str(), r["result"]["data"]["facts"]["state"].as_str()), (Some("observed"), Some("finished")));
    let closed = core.until(|m| m["method"] == "stream.closed" || m["method"] == "stream.gap").await;
    assert_eq!(closed["params"]["stream"], 1);
    let gone = core.until(|m| m["method"] == "resource.gone").await;
    assert_eq!((gone["params"]["ref"].as_str(), gone["params"]["reason"]["reason"].as_str()), (Some(pane.as_str()), Some("killed")));

    // Unknown pane: not_found, no effect.
    core.send(json!({"jsonrpc": "2.0", "id": 40, "method": "stream.open", "params": {"generation": 3, "stream": 2, "ref": pane,
        "incarnation": "", "cols": 80, "rows": 24, "history_lines": 0, "window_bytes": 65536}})).await;
    assert_eq!(core.until(|m| m["id"] == 40).await["error"]["data"]["kind"], "not_found");
}

#[tokio::test]
#[ignore]
async fn panes_created_and_closed_outside_prim_are_reconciled() {
    let tmux = Tmux::start();
    let (mut core, _provider) = start(&tmux).await;
    let reg = core.read().await;
    core.reply(&reg, json!({"provider": "tmux@test", "generation": 1, "limits": {"stream_window_bytes": 65536, "max_record_bytes": 1048576}})).await;
    let sync = core.read().await;
    core.reply(&sync, json!({})).await;
    // A window created by someone else, and a new session (a second control client is attached).
    let out = tmux.cli(&["new-window", "-d", "-P", "-F", "#{pane_id}", "-t", "work:", "/bin/sh"]);
    let outside = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let up = core.until(|m| m["method"] == "resource.upsert" && m["params"]["facts"]["ref"] == outside.as_str()).await;
    assert_eq!(up["params"]["facts"]["data"]["session"], "work");
    let out = tmux.cli(&["new-session", "-d", "-P", "-F", "#{pane_id}", "-s", "other", "/bin/sh"]);
    let other = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let up = core.until(|m| m["method"] == "resource.upsert" && m["params"]["facts"]["ref"] == other.as_str()).await;
    assert_eq!(up["params"]["facts"]["data"]["session"], "other");
    // Closed outside prim: gone with reason exited.
    assert!(tmux.cli(&["kill-pane", "-t", &outside]).status.success());
    let gone = core.until(|m| m["method"] == "resource.gone" && m["params"]["ref"] == outside.as_str()).await;
    assert_eq!(gone["params"]["reason"]["reason"], "exited");
    let _ = Path::new("/");
}
