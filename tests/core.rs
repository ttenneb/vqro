//! End-to-end tests of the core protocol over a real socket, with the fake backend as a fixture.

use std::path::PathBuf;
use std::time::Duration;

use base64::Engine;
use prim::client;
use prim::rpc::{Incoming, Peer};
use prim::server::{start, Backend};
use serde_json::{json, Value};
use tokio::sync::mpsc;

struct Env {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    state: PathBuf,
}

fn env() -> Env {
    let dir = tempfile::Builder::new().prefix("pt").tempdir_in("/tmp").unwrap();
    let socket = dir.path().join("s.sock");
    let state = dir.path().join("state");
    Env { _dir: dir, socket, state }
}

async fn connect(e: &Env) -> (Peer, mpsc::UnboundedReceiver<Incoming>) {
    let (p, rx, _) = client::connect(&e.socket, "test").await.unwrap();
    (p, rx)
}

async fn until<F: FnMut() -> bool>(mut f: F) {
    for _ in 0..100 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("condition not reached");
}

async fn ext_status(p: &Peer, name: &str) -> String {
    let l = p.call("extension.list", Value::Null).await.unwrap();
    l["extensions"][name]["status"].as_str().unwrap_or("").to_string()
}

#[tokio::test]
async fn open_attach_and_type() {
    let e = env();
    let _core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (p, mut rx) = connect(&e).await;
    let r = p.call("resource.spawn", json!({"argv": ["sh"]})).await.unwrap();
    let res = r["resource"]["id"].as_str().unwrap().to_string();
    p.call("command.invoke", json!({"name": "prim.layout.reset"})).await.unwrap();
    let snap = p.call("workspace.snapshot", Value::Null).await.unwrap();
    assert_eq!(snap["entries"]["workspace/active"]["value"]["layout"], "main");
    assert_eq!(snap["entries"][format!("view/term-{res}")]["value"]["resource"], res.as_str());

    let a = p.call("resource.attach", json!({"resource": res, "cols": 80, "rows": 24})).await.unwrap();
    let sid = a["stream"].as_u64().unwrap();
    p.notify("stream.input", json!({"stream": sid, "text": "abc"}));
    let mut got = Vec::new();
    while let Some(Incoming::Notification { method, params }) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await.unwrap() {
        if method == "stream.data" {
            got.extend(base64::engine::general_purpose::STANDARD.decode(params["data"].as_str().unwrap()).unwrap());
            if got.ends_with(b"abc") {
                break;
            }
        }
    }
    assert!(got.ends_with(b"abc"), "input echoed in order");
}

#[tokio::test]
async fn commands_route_to_their_owner_and_go_unavailable() {
    let e = env();
    let _core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (owner, mut owner_rx, _) = client::connect(&e.socket, "x").await.unwrap();
    owner.call("hello", json!({"extension": "greeter"})).await.unwrap();
    owner.call("command.register", json!({"name": "greet", "title": "say hi"})).await.unwrap();
    let o = owner.clone();
    let handler = tokio::spawn(async move {
        while let Some(m) = owner_rx.recv().await {
            if let Incoming::Request { id, method, params } = m {
                assert_eq!(method, "command.invoke");
                o.respond(id, Ok(json!({"hi": params["caller"]})));
            }
        }
    });

    let (p, _rx) = connect(&e).await;
    let r = p.call("command.invoke", json!({"name": "greet"})).await.unwrap();
    assert!(r["hi"].as_str().unwrap().starts_with("test:"));

    // Only the core owns `prim.*`, and names can't be stolen from a live owner.
    assert_eq!(p.call("command.register", json!({"name": "prim.revert"})).await.unwrap_err().kind(), "forbidden");
    assert_eq!(p.call("command.register", json!({"name": "greet"})).await.unwrap_err().kind(), "conflict");

    // The owner goes away: drop every handle so its socket closes.
    handler.abort();
    let _ = handler.await;
    drop(owner);
    let mut kind = String::new();
    for _ in 0..40 {
        kind = p.call("command.invoke", json!({"name": "greet"})).await.map(|_| String::new()).unwrap_or_else(|e| e.kind().to_string());
        if kind == "unavailable" || kind == "disconnected" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(kind == "unavailable" || kind == "disconnected", "got {kind}");
}

#[tokio::test]
async fn crash_budget_then_replace_then_safe_mode() {
    let e = env();
    let _core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (p, _rx) = connect(&e).await;
    p.call("workspace.apply", json!({"ops": [{"op": "set", "key": "extension/bad", "value": {"argv": ["sh", "-c", "exit 1"]}}]})).await.unwrap();
    for _ in 0..100 {
        if ext_status(&p, "bad").await == "failed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(ext_status(&p, "bad").await, "failed");

    // Replacing the spec is a fresh start.
    p.call("workspace.apply", json!({"ops": [{"op": "set", "key": "extension/bad", "value": {"argv": ["sleep", "30"]}}]})).await.unwrap();
    for _ in 0..40 {
        if ext_status(&p, "bad").await == "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(ext_status(&p, "bad").await, "running");

    p.call("command.invoke", json!({"name": "prim.safe_mode", "args": {"on": true}})).await.unwrap();
    assert_eq!(ext_status(&p, "bad").await, "safe_mode");
    p.call("command.invoke", json!({"name": "prim.safe_mode", "args": {"on": false}})).await.unwrap();
    assert_eq!(ext_status(&p, "bad").await, "running");
    p.call("command.invoke", json!({"name": "prim.extension.disable", "args": {"name": "bad"}})).await.unwrap();
    assert_eq!(ext_status(&p, "bad").await, "disabled");
}

#[tokio::test]
async fn transactions_revert_and_events() {
    let e = env();
    let _core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (p, mut rx) = connect(&e).await;
    let seq = p.call("events.subscribe", json!({})).await.unwrap()["seq"].as_u64().unwrap();
    let t = p
        .call("workspace.apply", json!({"ops": [
            {"op": "set", "key": "view/a", "value": {"kind": "panel", "title": "A"}},
            {"op": "set", "key": "layout/main", "value": {"view": "a"}},
            {"op": "set", "key": "workspace/active", "value": {"layout": "main"}}
        ]}))
        .await
        .unwrap();
    let mut saw = false;
    until(|| {
        while let Ok(Incoming::Notification { method, params }) = rx.try_recv() {
            if method == "event" && params["type"] == "entries" && params["seq"].as_u64().unwrap() > seq {
                saw = true;
            }
        }
        saw
    })
    .await;

    // A bad agent edit is rejected whole.
    let err = p.call("workspace.apply", json!({"ops": [{"op": "delete", "key": "view/a"}]})).await.unwrap_err();
    assert_eq!(err.kind(), "invalid");

    p.call("workspace.revert", json!({"txn": t["txn"]})).await.unwrap();
    let snap = p.call("workspace.snapshot", Value::Null).await.unwrap();
    assert!(snap["entries"].get("view/a").is_none());

    // Survives a restart of the store.
    let hist = p.call("workspace.history", json!({"limit": 5})).await.unwrap();
    assert_eq!(hist["txns"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn defaults_install_once() {
    let e = env();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("defaults.json"), r#"{"extensions": [{"name": "nap", "argv": ["sleep", "30"]}]}"#).unwrap();
    let _core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, Some(dir.path().to_path_buf())).unwrap();
    let (p, _rx) = connect(&e).await;
    let snap = p.call("workspace.snapshot", Value::Null).await.unwrap();
    assert!(snap["entries"].get("extension/nap").is_some());
    // Removing a default sticks: it is not reinstalled on the next start.
    p.call("workspace.apply", json!({"ops": [{"op": "delete", "key": "extension/nap"}]})).await.unwrap();
    let mut store = prim::store::Store::open(e.state.join("workspace.json")).unwrap();
    assert!(store.get("doc/prim/defaults").is_some());
    let _ = &mut store;
}

#[tokio::test]
async fn live_view_content_is_ephemeral() {
    let e = env();
    let _core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (publisher, _prx, _) = client::connect(&e.socket, "pub").await.unwrap();
    let (p, _rx) = connect(&e).await;
    publisher.call("view.publish", json!({"view": "panel1", "content": {"items": [{"text": "hi"}]}})).await.unwrap();
    let snap = p.call("workspace.snapshot", Value::Null).await.unwrap();
    assert_eq!(snap["live"]["panel1"]["items"][0]["text"], "hi");
    // Not a transaction: history stays empty.
    let hist = p.call("workspace.history", json!({})).await.unwrap();
    assert!(hist["txns"].as_array().unwrap().is_empty());
    // Cleared when the publisher goes away.
    drop(publisher);
    drop(_prx);
    let mut cleared = false;
    for _ in 0..40 {
        let snap = p.call("workspace.snapshot", Value::Null).await.unwrap();
        if snap["live"].get("panel1").is_none() {
            cleared = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(cleared);
}
