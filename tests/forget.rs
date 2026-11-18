//! `resource.forget`: finished resources can be released and their ids retired (never reused).

use std::path::PathBuf;
use std::time::Duration;

use prim::client;
use prim::rpc::Peer;
use prim::server::{start, Backend};
use serde_json::{json, Value};

struct Env {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    state: PathBuf,
}

fn env() -> Env {
    let dir = tempfile::Builder::new().prefix("pf").tempdir_in("/tmp").unwrap();
    Env { socket: dir.path().join("s.sock"), state: dir.path().join("state"), _dir: dir }
}

async fn connect(socket: &std::path::Path) -> (Peer, prim::rpc::IncomingReceiver) {
    let (p, rx, _) = client::connect(socket, "test").await.unwrap();
    (p, rx)
}

fn number(id: &Value) -> u64 { id.as_str().unwrap().trim_start_matches('r').parse().unwrap() }

async fn finished(p: &Peer, id: &Value) {
    for _ in 0..200 {
        let list = p.call("resource.list", Value::Null).await.unwrap();
        if list["resources"].as_array().unwrap().iter().any(|r| &r["id"] == id && r["state"] == "finished") { return; }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("{id} never finished");
}

#[tokio::test]
async fn forget_refuses_live_and_retires_finished_ids() {
    let e = env();
    let core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (p, _rx) = connect(&e.socket).await;
    let snap = p.call("workspace.snapshot", Value::Null).await.unwrap();
    assert!(snap["providers"]["fake"]["capabilities"]["resource.forget"].is_object(), "capability is declared");

    let id = p.call("resource.spawn", json!({"argv": ["sh"]})).await.unwrap()["resource"]["id"].clone();
    let live = p.call("resource.forget", json!({"resource": id})).await.unwrap_err();
    assert_eq!(live.kind(), "invalid", "a live resource is refused: {live:?}");
    assert!(core.lock().unwrap().resources.contains_key(id.as_str().unwrap()), "nothing was removed");
    let via_invoke = p.call("resource.invoke", json!({"resource": id, "op": "resource.forget"})).await.unwrap_err();
    assert_eq!(via_invoke.kind(), "invalid", "the generic path cannot bypass the finished check");

    p.call("resource.kill", json!({"resource": id})).await.unwrap();
    finished(&p, &id).await;
    let r = p.call("resource.forget", json!({"resource": id})).await.unwrap();
    assert_eq!(r["outcome"], "observed");
    assert!(!core.lock().unwrap().resources.contains_key(id.as_str().unwrap()), "retired on the observed reply");
    assert!(!core.lock().unwrap().has_resource_identity(id.as_str().unwrap()));
    assert_eq!(p.call("resource.forget", json!({"resource": id})).await.unwrap_err().kind(), "not_found");

    // Restart: ids continue past the forgotten one, never reusing it.
    prim::server::shutdown(&core).await;
    let socket = e._dir.path().join("again.sock");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let _again = loop {
        match start(socket.clone(), e.state.clone(), false, Backend::Fake, None) {
            Ok(core) => break core,
            Err(err) if std::time::Instant::now() < deadline && err.to_string().contains("still in use") => tokio::time::sleep(Duration::from_millis(10)).await,
            Err(err) => panic!("{err}"),
        }
    };
    let (q, _rx2) = connect(&socket).await;
    let next = q.call("resource.spawn", json!({"argv": ["sh"]})).await.unwrap()["resource"]["id"].clone();
    assert!(number(&next) > number(&id), "{next} must not reuse or precede forgotten {id}");
}

async fn ids(p: &Peer) -> Vec<String> {
    let list = p.call("resource.list", Value::Null).await.unwrap();
    list["resources"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect()
}

async fn eventually(what: &str, mut ok: impl AsyncFnMut() -> bool) {
    for _ in 0..200 {
        if ok().await { return; }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("not reached: {what}");
}

/// The bundled retention policy: shown finished terminals stay until their last view closes;
/// never-shown ones keep the newest 64; live ones are never touched.
#[tokio::test]
async fn retention_extension_forgets_closed_and_old_never_shown_finished_terminals() {
    let e = env();
    let defaults = e._dir.path().join("defaults");
    std::fs::create_dir_all(&defaults).unwrap();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions/retention.py");
    std::fs::write(defaults.join("defaults.json"), json!({"version": 1, "extensions": [{"name": "retention", "argv": ["python3", script]}]}).to_string()).unwrap();
    let _core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, Some(defaults)).unwrap();
    let (p, _rx) = connect(&e.socket).await;
    eventually("retention running", async || p.call("extension.list", Value::Null).await.unwrap()["extensions"]["retention"]["status"] == "running").await;

    let spawn = async |p: &Peer| p.call("resource.spawn", json!({"argv": ["sh"]})).await.unwrap()["resource"]["id"].as_str().unwrap().to_string();
    let (shown, never, live) = (spawn(&p).await, spawn(&p).await, spawn(&p).await);
    p.call("workspace.apply", json!({"ops": [
        {"op": "set", "key": "view/a", "value": {"kind": "terminal", "resource": shown}},
        {"op": "set", "key": "view/x", "value": {"kind": "panel", "items": []}},
        {"op": "set", "key": "layout/main", "value": {"split": "h", "children": [{"view": "a"}, {"view": "x"}]}}]})).await.unwrap();
    for id in [&shown, &never] {
        p.call("resource.kill", json!({"resource": id})).await.unwrap();
        finished(&p, &json!(id)).await;
    }
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let now = ids(&p).await;
    assert!(now.contains(&shown) && now.contains(&never) && now.contains(&live), "shown and recent never-shown finished stay: {now:?}");

    // Closing its last view forgets the shown terminal.
    p.call("workspace.apply", json!({"ops": [{"op": "set", "key": "layout/main", "value": {"view": "x"}}]})).await.unwrap();
    eventually("closed view forgotten", async || !ids(&p).await.contains(&shown)).await;
    assert!(ids(&p).await.contains(&never), "never-shown finished within the budget stays");

    // Over the budget, the oldest never-shown finished ones go; the live one stays.
    for _ in 0..70 {
        let id = spawn(&p).await;
        p.call("resource.kill", json!({"resource": id})).await.unwrap();
    }
    eventually("budget of 64", async || {
        let list = p.call("resource.list", Value::Null).await.unwrap();
        list["resources"].as_array().unwrap().iter().filter(|r| r["state"] == "finished").count() == 64
    }).await;
    let now = ids(&p).await;
    assert!(!now.contains(&never), "the oldest never-shown finished terminal went first");
    assert!(now.contains(&live), "live resources are never forgotten");

    let r = p.call("command.invoke", json!({"name": "retention.forget_ended"})).await.unwrap();
    assert_eq!(r["forgotten"], 64, "{r}");
    assert_eq!(ids(&p).await, vec![live], "only the live terminal remains");
}

#[tokio::test]
async fn churn_of_forgotten_terminals_keeps_resources_bounded() {
    let e = env();
    let core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (p, _rx) = connect(&e.socket).await;
    let n: usize = std::env::var("PRIM_FORGET_CHURN").ok().and_then(|v| v.parse().ok()).unwrap_or(500);
    let started = std::time::Instant::now();
    for _ in 0..n {
        let id = p.call("resource.spawn", json!({"argv": ["sh"]})).await.unwrap()["resource"]["id"].clone();
        p.call("resource.kill", json!({"resource": id})).await.unwrap();
        finished(&p, &id).await;
        p.call("resource.forget", json!({"resource": id})).await.unwrap();
    }
    let count = core.lock().unwrap().resources.len();
    assert_eq!(count, 0, "{n} spawned+finished+forgotten terminals leave no resources");
    let snap = p.call("workspace.snapshot", Value::Null).await.unwrap();
    assert!(snap["resources"].as_array().unwrap().is_empty());
    eprintln!("churn {n} in {:?}", started.elapsed());
}
