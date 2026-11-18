//! Linux transport lifetime matrix. Private fake core, owned provider children, explicit cleanup.
//! The long-soak driver must close its own sockets; these tests assert core fds after real EOF.
#![cfg(target_os = "linux")]
use prim::{
    client,
    rpc::{Incoming, IncomingReceiver, Peer},
};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const CYCLES: usize = 20;
struct Owned(Child);
impl Owned {
    fn kill(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        self.kill();
    }
}
struct Fixture {
    root: tempfile::TempDir,
    core: Owned,
    control: Peer,
    _incoming: IncomingReceiver,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fds {
    all: usize,
    sockets: usize,
}
fn fds(pid: u32) -> Fds {
    let targets = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .filter_map(|e| std::fs::read_link(e.ok()?.path()).ok())
        .collect::<Vec<_>>();
    Fds {
        all: targets.len(),
        sockets: targets
            .iter()
            .filter(|p| p.to_string_lossy().starts_with("socket:"))
            .count(),
    }
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("pfd")
            .tempdir_in("/tmp")
            .unwrap();
        for d in ["home", "state", "ext", "run", "hc-state", "cfg"] {
            std::fs::create_dir(root.path().join(d)).unwrap();
        }
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_prim"));
        Self::configure(root.path(), &mut cmd);
        let core = Owned(
            cmd.args(["serve", "--fake"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let until = Instant::now() + Duration::from_secs(5);
        let (control, incoming, _) = loop {
            match client::connect(&root.path().join("p"), "fd-control").await {
                Ok(p) => break p,
                Err(_) if Instant::now() < until => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                Err(e) => panic!("private core: {e}"),
            }
        };
        Self {
            root,
            core,
            control,
            _incoming: incoming,
        }
    }
    fn configure(root: &Path, cmd: &mut Command) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.join("run"), std::fs::Permissions::from_mode(0o700)).unwrap();
        cmd.env_clear()
            .env("HOME", root.join("home"))
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "xterm-256color")
            .env("PRIM_STATE_DIR", root.join("state"))
            .env("PRIM_SOCKET", root.join("p"))
            .env("PRIM_EXTENSIONS_DIR", root.join("ext"))
            .env("PRIM_HERDR", r#"["/bin/false"]"#)
            .env("XDG_RUNTIME_DIR", root.join("run"))
            .env("XDG_STATE_HOME", root.join("hc-state"))
            .env("XDG_CONFIG_HOME", root.join("cfg"))
            .env("HC_SOCKET", root.join("h"))
            .env("HC_SESSION", "fd-matrix");
    }
    fn command(&self, binary: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut c = Command::new(binary);
        Self::configure(self.root.path(), &mut c);
        c
    }
    async fn peer(&self) -> (Peer, IncomingReceiver) {
        let (p, r, _) = client::connect(&self.root.path().join("p"), "fd-viewer")
            .await
            .unwrap();
        (p, r)
    }
    async fn stable(&self, expected: Fds, label: &str) {
        let until = Instant::now() + Duration::from_secs(5);
        let mut matches = 0;
        loop {
            let actual = fds(self.core.0.id());
            if actual == expected {
                matches += 1;
                if matches >= 3 {
                    return;
                }
            } else {
                matches = 0;
            }
            assert!(
                Instant::now() < until,
                "{label}: fds did not return to {expected:?}: {actual:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    async fn snapshot(&self) -> Value {
        self.control
            .call("workspace.snapshot", Value::Null)
            .await
            .unwrap()
    }
    async fn provider_ready(&self, name: &str, connected: bool) -> Value {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let snapshot = self.snapshot().await;
            let p = &snapshot["providers"][name];
            if p["connected"] == connected && (!connected || p["synced"] == true) {
                return p.clone();
            }
            assert!(
                Instant::now() < until,
                "provider {name} connected={connected}: {p}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    async fn spawn(&self, provider: Option<&str>) -> Value {
        self.control
            .call(
                "resource.spawn",
                json!({"provider":provider,"argv":["/bin/cat"]}),
            )
            .await
            .unwrap()["resource"]
            .clone()
    }
    async fn attach(peer: &Peer, rid: &Value) -> Value {
        peer.call(
            "resource.attach",
            json!({"resource":rid,"cols":80,"rows":24}),
        )
        .await
        .unwrap()["stream"]
            .clone()
    }
    fn report(&self, label: &str, expected: Fds) {
        self.report_cycles(label, expected, CYCLES);
    }
    fn report_cycles(&self, label: &str, expected: Fds, cycles: usize) {
        eprintln!(
            "fd_matrix phase={label} cycles={cycles} baseline={expected:?} final={:?}",
            fds(self.core.0.id())
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.control.close();
        self.core.kill();
    }
}
fn registration(name: &str) -> Value {
    json!({"name":name,"instance":"fixture","role":"backend","software":{"name":"fd-fixture","version":"1"},"provider_version":"0","capabilities":{"terminal.output":{"raw":true}}})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "private process/socket fd matrix; full gate"]
async fn socket_fds_return_after_clients_streams_and_provider_cycles() {
    let f = Fixture::new().await;
    let baseline = fds(f.core.0.id());
    for _ in 0..CYCLES {
        let (p, _) = f.peer().await;
        p.close();
        f.stable(baseline, "viewer EOF").await;
    }
    f.report("viewer_eof", baseline);
    for _ in 0..CYCLES {
        let socket = tokio::net::UnixStream::connect(f.root.path().join("p"))
            .await
            .unwrap();
        let (reader, mut writer) = socket.into_split();
        let mut reader = BufReader::new(reader);
        writer.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"hello\",\"params\":{\"client\":\"half-close\"}}\n").await.unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(serde_json::from_str::<Value>(&line)
            .unwrap()
            .get("result")
            .is_some());
        writer.shutdown().await.unwrap();
        f.stable(baseline, "client write half-closed, read half retained")
            .await;
    }
    f.report("write_half_close", baseline);
    let rid = f.spawn(None).await["id"].clone();
    for _ in 0..CYCLES {
        let (p, _rx) = f.peer().await;
        let sid = Fixture::attach(&p, &rid).await;
        p.call("stream.close", json!({"stream":sid})).await.unwrap();
        p.close();
        f.stable(baseline, "stream close").await;
    }
    f.report("stream_close", baseline);
    for _ in 0..CYCLES {
        let (p, _rx) = f.peer().await;
        Fixture::attach(&p, &rid).await;
        p.close();
        f.stable(baseline, "attached viewer EOF").await;
    }
    f.report("attached_viewer_eof", baseline);
    for _ in 0..CYCLES {
        let (p, _) = f.peer().await;
        let g = p
            .call("provider.register", registration("fd-reconnect"))
            .await
            .unwrap()["generation"]
            .clone();
        p.call(
            "provider.sync",
            json!({"generation":g,"resources":[],"final":true}),
        )
        .await
        .unwrap();
        p.close();
        f.stable(baseline, "provider reconnect").await;
    }
    f.report("provider_reconnect", baseline);
    let (provider, mut requests) = f.peer().await;
    let generation = provider
        .call("provider.register", registration("fd-gap"))
        .await
        .unwrap()["generation"]
        .clone();
    provider.call("provider.sync",json!({"generation":generation,"final":true,"resources":[{"ref":"t","kind":"terminal","state":"live"}]})).await.unwrap();
    let rid = f
        .control
        .call("resource.list", json!({"provider":"fd-gap"}))
        .await
        .unwrap()["resources"][0]["id"]
        .clone();
    let actor = provider.clone();
    let handler = tokio::spawn(async move {
        while let Some(message) = requests.recv().await {
            if let Incoming::Request { id, method, params } = message {
                assert_eq!(method, "stream.open");
                actor.respond(id, Ok(json!({"initial":"none"})));
                actor.notify("stream.gap",json!({"generation":generation,"stream":params["stream"],"offset":0,"reason":{"reason":"gap","gap":"fixture"}}));
            }
        }
    });
    let with_provider = fds(f.core.0.id());
    for _ in 0..CYCLES {
        let (p, mut rx) = f.peer().await;
        Fixture::attach(&p, &rid).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(Incoming::Notification { method, .. }) = rx.recv().await {
                    if method == "stream.gap" {
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
        p.close();
        f.stable(with_provider, "gap and reattach").await;
    }
    f.report("gap_reattach", with_provider);
    provider.close();
    handler.await.unwrap();
    f.stable(baseline, "provider EOF after gaps").await;
    for _ in 0..CYCLES {
        let mut child = Owned(
            f.command("/usr/bin/python3")
                .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_provider.py"))
                .args(["--name", "fd-kill", "--instance", "fixed"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        f.provider_ready("fd-kill", true).await;
        child.kill();
        f.provider_ready("fd-kill", false).await;
        f.stable(baseline, "provider SIGKILL").await;
    }
    f.report("provider_sigkill", baseline);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "private native provider/HC reconnect fd matrix; full gate"]
async fn socket_fds_return_after_native_herdr_reconnect_and_sigkill() {
    let f = Fixture::new().await;
    let baseline = fds(f.core.0.id());
    let hc = PathBuf::from(std::env::var_os("HOME").unwrap()).join(".local/bin/herdr-core");
    assert!(hc.exists());
    struct HcStop<'a> {
        f: &'a Fixture,
        hc: PathBuf,
    }
    impl Drop for HcStop<'_> {
        fn drop(&mut self) {
            let _ = self.f.command(&self.hc).args(["server", "stop"]).output();
        }
    }
    let cleanup = HcStop {
        f: &f,
        hc: hc.clone(),
    };
    let helper = || {
        Owned(
            f.command(env!("CARGO_BIN_EXE_prim-herdr"))
                .arg("--hc")
                .arg(&hc)
                .args(["--name", "fd-herdr"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    };
    for _ in 0..CYCLES {
        let mut child = helper();
        f.provider_ready("fd-herdr", true).await;
        let resource = f.spawn(Some("fd-herdr")).await;
        let (p, mut rx) = f.peer().await;
        Fixture::attach(&p, &resource["id"]).await;
        child.kill();
        f.provider_ready("fd-herdr", false).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(Incoming::Notification { method, .. }) = rx.recv().await {
                    if method == "stream.closed" {
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
        f.stable(
            Fds {
                all: baseline.all + 1,
                sockets: baseline.sockets + 1,
            },
            "provider gone, healthy viewer intentionally retained",
        )
        .await;
        p.close();
        f.stable(baseline, "prim-herdr SIGKILL then viewer EOF")
            .await;
    }
    f.report("prim_herdr_sigkill", baseline);
    let mut child = helper();
    let mut current = f.provider_ready("fd-herdr", true).await;
    let connected = fds(f.core.0.id());
    for _ in 0..10 {
        let stopped = f.command(&hc).args(["server", "stop"]).output().unwrap();
        assert!(stopped.status.success());
        f.provider_ready("fd-herdr", false).await;
        f.stable(baseline, "native HC disconnect").await;
        let started = f
            .command(&hc)
            .args(["server", "status", "--json"])
            .output()
            .unwrap();
        assert!(started.status.success());
        let next = f.provider_ready("fd-herdr", true).await;
        assert!(next["generation"].as_u64().unwrap() > current["generation"].as_u64().unwrap());
        current = next;
        f.stable(connected, "native HC reconnect").await;
    }
    f.report_cycles("native_hc_reconnect", connected, 10);
    child.kill();
    f.stable(baseline, "final native provider EOF").await;
    drop(cleanup);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "private supervised extension and TUI fd matrix; full gate"]
async fn socket_fds_return_after_extension_kills_and_tui_quits() {
    let f = Fixture::new().await;
    let baseline = fds(f.core.0.id());
    let script = f.root.path().join("ext.py");
    let marker = f.root.path().join("pid");
    std::fs::write(&script,format!("import os,sys\nfrom pathlib import Path\nsys.path.insert(0,{:?})\nfrom prim_ext import Extension\ne=Extension('fd-fixture')\ne.command('fd-fixture.ready','FD fixture')(lambda args: {{}})\nPath({:?}).write_text(str(os.getpid()))\ne.run()\n",Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions"),marker)).unwrap();
    f.control.call("workspace.apply",json!({"ops":[{"op":"set","key":"extension/fd-fixture","value":{"argv":["/usr/bin/python3",script]}}]})).await.unwrap();
    async fn ready(f: &Fixture, marker: &Path, previous: u32) -> u32 {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(pid) = std::fs::read_to_string(marker)
                .unwrap_or_default()
                .parse::<u32>()
            {
                if pid != previous
                    && f.control
                        .call("command.invoke", json!({"name":"fd-fixture.ready"}))
                        .await
                        .is_ok()
                {
                    return pid;
                }
            }
            assert!(
                Instant::now() < until,
                "extension did not become ready after pid {previous}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    let mut pid = ready(&f, &marker, 0).await;
    let active = fds(f.core.0.id());
    for _ in 0..CYCLES {
        f.control
            .call(
                "command.invoke",
                json!({"name":"prim.extension.restart","args":{"name":"fd-fixture"}}),
            )
            .await
            .unwrap();
        pid = ready(&f, &marker, pid).await;
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        let parent = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse::<u32>()
            .unwrap();
        assert_eq!(
            parent,
            f.core.0.id(),
            "only signal this fixture core's own extension child"
        );
        assert!(Command::new("/bin/kill")
            .env_clear()
            .args(["-KILL", &pid.to_string()])
            .status()
            .unwrap()
            .success());
        pid = ready(&f, &marker, pid).await;
        f.stable(active, "supervised extension SIGKILL/restart")
            .await;
    }
    f.report("extension_sigkill", active);
    f.control
        .call(
            "command.invoke",
            json!({"name":"prim.extension.disable","args":{"name":"fd-fixture"}}),
        )
        .await
        .unwrap();
    f.stable(baseline, "extension disabled").await;
    struct Mux<'a>(&'a Fixture);
    impl Mux<'_> {
        fn call(&self, args: &[&str]) -> std::process::Output {
            let mut cmd = self.0.command("/usr/bin/tmux");
            if args.first() != Some(&"new-session") {
                cmd.arg("-N");
            }
            cmd.arg("-S")
                .arg(self.0.root.path().join("t"))
                .args(["-f", "/dev/null"])
                .args(args)
                .output()
                .unwrap()
        }
    }
    impl Drop for Mux<'_> {
        fn drop(&mut self) {
            let _ = self.call(&["kill-server"]);
        }
    }
    let mux = Mux(&f);
    for _ in 0..CYCLES {
        let started = mux.call(&[
            "new-session",
            "-d",
            "-s",
            "fd",
            env!("CARGO_BIN_EXE_prim"),
            "tui",
        ]);
        assert!(
            started.status.success(),
            "{}",
            String::from_utf8_lossy(&started.stderr)
        );
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let screen = mux.call(&["capture-pane", "-p", "-t", "fd"]);
            let clients = f.control.call("client.list", Value::Null).await.unwrap();
            let connected = clients["clients"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["client"] == "tui");
            if String::from_utf8_lossy(&screen.stdout).contains("prim")
                && connected
                && fds(f.core.0.id()).sockets > baseline.sockets
            {
                break;
            }
            assert!(Instant::now() < until, "TUI did not render");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(mux
            .call(&["send-keys", "-t", "fd", "C-g", "q"])
            .status
            .success());
        let until = Instant::now() + Duration::from_secs(5);
        while mux.call(&["has-session", "-t", "fd"]).status.success() {
            assert!(Instant::now() < until, "TUI did not quit");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        f.stable(baseline, "TUI quit").await;
    }
    f.report("tui_quit", baseline);
}
