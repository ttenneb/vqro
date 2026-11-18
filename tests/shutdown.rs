//! Disposable core processes only; these tests never use the user's endpoint, pidfile or hc.
use std::{path::{Path, PathBuf}, process::{Child, Command, Stdio}, time::Duration};
use vqro::{client, rpc::{Incoming, Peer, RpcError}};
use serde_json::{json, Value};

struct Owned(Child);
impl Drop for Owned { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); } }
struct Root { dir: tempfile::TempDir, socket: PathBuf, state: PathBuf }
impl Root {
    fn new() -> Self { let dir = tempfile::Builder::new().prefix("ps").tempdir_in("/tmp").unwrap(); let socket = dir.path().join("s"); let state = dir.path().join("st"); std::fs::create_dir(&state).unwrap(); Self { dir, socket, state } }
    fn cli(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_vqro"));
        cmd.env_clear().env("HOME", self.dir.path()).env("PATH", std::env::var("PATH").unwrap()).env("VQRO_SOCKET", &self.socket).env("VQRO_STATE_DIR", &self.state).env("VQRO_EXTENSIONS_DIR", self.dir.path().join("no-extensions")); cmd
    }
    async fn start(&self) -> (Owned, Peer) {
        let child = Owned(self.cli().args(["serve", "--fake"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
        let peer = tokio::time::timeout(Duration::from_secs(3), async { loop { if let Ok((peer, _, _)) = client::connect(&self.socket, "shutdown-test").await { break peer; } tokio::time::sleep(Duration::from_millis(10)).await; } }).await.unwrap();
        (child, peer)
    }
}
async fn exit(child: &mut Owned) { tokio::time::timeout(Duration::from_secs(4), async { loop { if let Some(status) = child.0.try_wait().unwrap() { assert!(status.success(), "{status}"); break; } tokio::time::sleep(Duration::from_millis(10)).await; } }).await.unwrap(); }
async fn marker(path: &Path) { tokio::time::timeout(Duration::from_secs(2), async { while !path.exists() { tokio::time::sleep(Duration::from_millis(5)).await; } }).await.unwrap(); }
async fn extension(peer: &Peer, root: &Root) {
    let script = "import sys,time;from pathlib import Path;p=Path(sys.argv[1]);(p/'started').write_text('yes');sys.stdin.buffer.read();time.sleep(.05);(p/'cleaned').write_text('EOF')";
    peer.call("workspace.apply", json!({"ops":[{"op":"set","key":"extension/cleanup","value":{"argv":["python3","-c",script,root.dir.path()]}}]})).await.unwrap();
    marker(&root.dir.path().join("started")).await;
}

#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn shutdown_by_protocol_and_recovery_command_acknowledges_and_exits() {
    for recovery in [false, true] {
        let root = Root::new(); let (mut child, peer) = root.start().await;
        extension(&peer, &root).await;
        if recovery { peer.call("command.invoke", json!({"name":"vqro.safe_mode","args":{"on":true}})).await.unwrap(); }
        let reply = if recovery { peer.call("command.invoke", json!({"name":"vqro.shutdown"})).await } else { peer.call("core.shutdown", Value::Null).await }.unwrap();
        assert_eq!(reply["outcome"], "accepted", "reply must arrive before core disconnect");
        tokio::time::timeout(Duration::from_secs(4), peer.closed()).await.unwrap();
        assert_eq!(std::fs::read_to_string(root.dir.path().join("cleaned")).unwrap(), "EOF", "extension cleanup must precede closing clients");
        exit(&mut child).await;
        assert!(!root.socket.exists()); assert!(!root.state.join("serve.pid").exists());
    }
}

#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn cli_stop_uses_protocol_even_without_pidfile() {
    let root = Root::new(); let (mut child, _peer) = root.start().await;
    std::fs::remove_file(root.state.join("serve.pid")).unwrap();
    let result = root.cli().arg("stop").output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert!(String::from_utf8_lossy(&result.stdout).contains("accepted"));
    exit(&mut child).await;
}

#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn protocol_refusal_does_not_signal_pidfile_target() {
    let root = Root::new();
    let mut target = Owned(Command::new("/bin/sleep").arg("30").env_clear().spawn().unwrap());
    std::fs::write(root.state.join("serve.pid"), target.0.id().to_string()).unwrap();
    let listener = tokio::net::UnixListener::bind(&root.socket).unwrap();
    let mock = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap(); let (r,w)=stream.into_split(); let (peer,mut rx)=Peer::spawn(r,w);
        while let Some(Incoming::Request {id,method,..})=rx.recv().await {
            if method=="hello" { peer.respond(id,Ok(json!({}))); }
            else { assert_eq!(method,"core.shutdown"); peer.respond(id,Err(RpcError::new("forbidden","refused"))); }
        }
    });
    let result = tokio::process::Command::from(root.cli()).arg("stop").output().await.unwrap();
    assert!(!result.status.success()); assert!(target.0.try_wait().unwrap().is_none(), "a real protocol answer must never fall back to a signal");
    mock.await.unwrap();
}

#[test]
fn no_answer_uses_only_the_private_pidfile_target_and_rejects_invalid_pids() {
    let root = Root::new();
    let mut target = Owned(Command::new("/bin/sleep").arg("30").env_clear().spawn().unwrap());
    std::fs::write(root.state.join("serve.pid"),target.0.id().to_string()).unwrap();
    assert!(root.cli().arg("stop").output().unwrap().status.success());
    target.0.wait().unwrap();
    std::fs::write(root.state.join("serve.pid"),"-1").unwrap();
    assert!(!root.cli().arg("stop").output().unwrap().status.success());
}

#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn sigterm_uses_the_same_ordered_cleanup() {
    let root=Root::new(); let (mut child, _peer)=root.start().await; extension(&_peer,&root).await;
    assert!(Command::new("kill").args(["-TERM",&child.0.id().to_string()]).env_clear().env("PATH",std::env::var("PATH").unwrap()).status().unwrap().success());
    exit(&mut child).await;
    assert_eq!(std::fs::read_to_string(root.dir.path().join("cleaned")).unwrap(),"EOF");
}
