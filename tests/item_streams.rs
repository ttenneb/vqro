//! Core item-stream channel against a raw wire provider (agent-v0 §4, provider-v0 9.5): private state, no hc.
use std::path::PathBuf;
use std::time::Duration;
use base64::Engine;
use serde_json::{json, Value};
use vqro::{client, rpc::{Incoming, IncomingReceiver, Peer}, server::{self, Backend}};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

struct Env { _dir: tempfile::TempDir, socket: PathBuf, core: vqro::core::Shared }
impl Drop for Env { fn drop(&mut self) { vqro::ext::stop_all(&self.core); } }
async fn env() -> Env {
    let dir = tempfile::Builder::new().prefix("pis").tempdir_in("/tmp").unwrap();
    let socket = dir.path().join("s");
    let core = server::start(socket.clone(), dir.path().join("st"), false, Backend::Fake, None).unwrap();
    Env { _dir: dir, socket, core }
}
async fn peer(e: &Env) -> (Peer, IncomingReceiver) { let (p, rx, _) = client::connect(&e.socket, "items-test").await.unwrap(); (p, rx) }
fn registration(limits: Value) -> Value {
    json!({"name": "items", "instance": "i", "role": "backend", "software": {"name": "fixture", "version": "1"}, "provider_version": "1",
        "capabilities": {"items.stream": {"retain_bytes": 1024}}, "limits": limits})
}

/// A raw provider with agents a1, a2 that answers every stream.open with `answer(open params)`.
async fn provider(e: &Env, limits: Value, answer: fn(&Value) -> Value) -> (Peer, Value, tokio::task::JoinHandle<()>, tokio::sync::mpsc::UnboundedReceiver<Value>) {
    let (p, mut incoming) = peer(e).await;
    let r = p.call("provider.register", registration(limits)).await.unwrap();
    let generation = r["generation"].clone();
    let facts = |r: &str| json!({"ref": r, "kind": "agent", "state": "live"});
    p.call("provider.sync", json!({"generation": generation, "resources": [facts("a1"), facts("a2")], "final": true})).await.unwrap();
    let (opens_tx, opens) = tokio::sync::mpsc::unbounded_channel();
    let actor = p.clone();
    let task = tokio::spawn(async move {
        while let Some(m) = incoming.recv().await {
            if let Incoming::Request { id, method, params } = m {
                assert_eq!(method, "stream.open");
                let _ = opens_tx.send(params.clone());
                actor.respond(id, Ok(answer(&params)));
            }
        }
    });
    (p, r, task, opens)
}
fn rid(e: &Env, r: &str) -> String {
    e.core.lock().unwrap().resources.values().find(|x| x.adapter == "items" && x.facts.backend_ref == r).unwrap().id.clone()
}
async fn next_stream_message(rx: &mut IncomingReceiver, sid: u64) -> (String, Value) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(Incoming::Notification { method, params }) = rx.recv().await {
                if params["stream"] == sid { return (method, params); }
            }
        }
    }).await.expect("stream message")
}

#[tokio::test]
async fn an_unknown_gap_reason_ends_only_that_stream_never_the_provider() {
    let e = env().await;
    let (p, r, _task, mut opens) = provider(&e, json!({}), |_| json!({"initial": "reset", "epoch": "e1"})).await;
    let generation = r["generation"].clone();
    let (viewer, mut vrx) = peer(&e).await;
    let s1 = viewer.call("resource.attach", json!({"resource": rid(&e, "a1"), "items": {}})).await.unwrap();
    let s2 = viewer.call("resource.attach", json!({"resource": rid(&e, "a2"), "items": {}})).await.unwrap();
    assert_eq!((s1["initial"].clone(), s1["epoch"].clone()), (json!("reset"), json!("e1")));
    let (o1, o2) = (opens.recv().await.unwrap(), opens.recv().await.unwrap());
    assert_eq!((o1["channel"].clone(), o1.get("cols").is_none()), (json!("items"), true));
    let (w1, w2) = (o1["stream"].clone(), o2["stream"].clone());
    // A future gap kind on stream 1: a protocol error for that stream only.
    p.notify("stream.gap", json!({"generation": generation, "stream": w1, "offset": 0, "reason": {"reason": "gap", "gap": "future_kind"}}));
    let (m, params) = next_stream_message(&mut vrx, s1["stream"].as_u64().unwrap()).await;
    assert_eq!((m.as_str(), params["reason"]["reason"].as_str()), ("stream.closed", Some("invalid")));
    // Stream 2, the provider connection and its resources are untouched.
    p.notify("stream.data", json!({"generation": generation, "stream": w2, "offset": 0, "data": B64.encode(b"{\"op\":\"notice\",\"value\":1}\n")}));
    let (m, _) = next_stream_message(&mut vrx, s2["stream"].as_u64().unwrap()).await;
    assert_eq!(m, "stream.data");
    assert!(!p.is_closed());
    assert!(e.core.lock().unwrap().resources.values().filter(|x| x.adapter == "items").all(|x| x.available));
    // A malformed closed reason is the same: that stream only.
    p.notify("stream.closed", json!({"generation": generation, "stream": w2, "reason": "not-an-object"}));
    let (m, params) = next_stream_message(&mut vrx, s2["stream"].as_u64().unwrap()).await;
    assert_eq!((m.as_str(), params["reason"]["reason"].as_str()), ("stream.closed", Some("invalid")));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!p.is_closed());
}

#[tokio::test]
async fn continue_is_accepted_only_for_an_open_with_after_in_that_epoch() {
    let e = env().await;
    // The provider (wrongly) always answers continue in epoch "A".
    let (_p, _r, _task, _opens) = provider(&e, json!({}), |_| json!({"initial": "continue", "epoch": "A"})).await;
    let (viewer, _vrx) = peer(&e).await;
    let a1 = rid(&e, "a1");
    let no_after = viewer.call("resource.attach", json!({"resource": a1, "items": {}})).await;
    assert!(no_after.is_err(), "continue without after must not reach the viewer: {no_after:?}");
    let other_epoch = viewer.call("resource.attach", json!({"resource": a1, "items": {"after": {"epoch": "B", "seq": 5}}})).await;
    assert!(other_epoch.is_err(), "continue in another epoch must not reach the viewer");
    let same = viewer.call("resource.attach", json!({"resource": a1, "items": {"after": {"epoch": "A", "seq": 5}}})).await.unwrap();
    assert_eq!((same["initial"].clone(), same["epoch"].clone()), (json!("continue"), json!("A")));
    // Mixing items with terminal parameters is refused before any provider call.
    let mixed = viewer.call("resource.attach", json!({"resource": a1, "items": {}, "cols": 80})).await.unwrap_err();
    assert_eq!(mixed.kind(), "invalid");
}

#[tokio::test]
async fn the_register_result_reports_the_granted_stream_limits() {
    let e = env().await;
    let (_p, r, _task, _opens) = provider(&e, json!({"max_streams": 100000, "max_item_streams": 100000}), |_| json!({})).await;
    assert_eq!(r["limits"]["max_streams"], 512);
    assert_eq!(r["limits"]["max_item_streams"], vqro::provider_wire::MAX_ITEM_STREAMS);
    let (_p2, r2, _t2, _o2) = {
        let e2 = env().await;
        let x = provider(&e2, json!({"max_item_streams": 7}), |_| json!({})).await;
        assert_eq!(x.1["limits"]["max_item_streams"], 7);
        x
    };
    let _ = r2;
}
