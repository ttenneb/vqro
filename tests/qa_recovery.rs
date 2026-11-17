//! Regressions from the independent foundation review. Private fake adapters/sockets only.
use std::time::Duration;
use prim::{adapter::{AdapterCmd, AdapterHandle, ResourceFacts, Stream, StreamInput}, client, core::Core, queue, server::{self, Backend}, store::Store};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn aggregate_workspace_snapshot_is_not_limited_by_control_backlog_budget() {
    let dir = tempfile::Builder::new().prefix("pq").tempdir_in("/tmp").unwrap();
    let socket = dir.path().join("s");
    let _core = server::start(socket.clone(), dir.path().join("state"), false, Backend::Fake, None).unwrap();
    let (peer, _, _) = client::connect(&socket, "qa").await.unwrap();
    for i in 0..9 {
        peer.call("workspace.apply", json!({"ops":[{"op":"set","key":format!("doc/large/{i}"),"value":"x".repeat(768*1024)}]})).await.unwrap();
    }
    let snapshot = peer.call("workspace.snapshot", Value::Null).await.expect("accepted state must remain recoverable");
    assert_eq!(snapshot["entries"].as_object().unwrap().len(), 9);
    assert!(!peer.is_closed());
}

#[tokio::test]
async fn idle_attach_returned_after_viewer_disconnect_is_closed_immediately() {
    let dir = tempfile::Builder::new().prefix("pq").tempdir_in("/tmp").unwrap();
    let socket = dir.path().join("s");
    let core = server::start(socket.clone(), dir.path().join("state"), false, Backend::Fake, None).unwrap();
    let (tx, mut commands) = tokio::sync::mpsc::unbounded_channel();
    let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
    prim::core::add_adapter(&core, AdapterHandle { name:"held".into(), session:None, capabilities:json!({"terminal.output":{"raw":true}}), cmds:tx }, receiver, false);
    events.send(("held".into(), prim::adapter::AdapterEvent::Registered(prim::provider::ProviderInfo {
        connection:1, instance:"fixture".into(), role:"backend".into(), software:json!({"name":"fixture","version":"1"})
    }))).unwrap();
    let facts: ResourceFacts = serde_json::from_value(json!({"backend_ref":"one","kind":"terminal","state":"live","name":null,"title":null})).unwrap();
    events.send(("held".into(), prim::adapter::AdapterEvent::Snapshot(vec![facts]))).unwrap();
    let resource = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Some(id) = core.lock().unwrap().resources.values().find(|r| r.adapter == "held").map(|r| r.id.clone()) { break id; }
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let mut viewer = tokio::net::UnixStream::connect(&socket).await.unwrap();
    viewer.write_all(format!("{}\n",json!({"jsonrpc":"2.0","id":1,"method":"resource.attach","params":{"resource":resource}})).as_bytes()).await.unwrap();
    let Some(AdapterCmd::Attach { reply, .. }) = prim::adapter::command_for(commands.recv().await.unwrap(), 1) else { panic!("expected attach") };
    viewer.shutdown().await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (input, mut input_rx) = queue::channel(256*1024);
    let (_output_tx, output) = queue::channel(256*1024);
    assert!(reply.send(Ok(Stream { initial:"repaint", input, output })).is_ok());
    let closed = tokio::time::timeout(Duration::from_millis(300), input_rx.recv()).await.expect("late silent stream must close without waiting for backend output");
    assert!(closed.is_none_or(|item| matches!(item.value, StreamInput::Close)));
}

#[tokio::test]
async fn retained_backlog_larger_than_control_queue_becomes_missed_not_disconnect() {
    let dir = tempfile::Builder::new().prefix("pq").tempdir_in("/tmp").unwrap();
    let socket = dir.path().join("s");
    let core = server::start(socket.clone(), dir.path().join("state"), false, Backend::Fake, None).unwrap();
    let epoch = {
        let mut core = core.lock().unwrap();
        for _ in 0..9 { core.emit("large", json!({"text":"x".repeat(768*1024)})); }
        core.epoch.clone()
    };
    let (peer, mut incoming, _) = client::connect(&socket, "qa").await.unwrap();
    peer.call("events.subscribe", json!({"epoch":epoch,"from":0})).await.unwrap();
    let Some(prim::rpc::Incoming::Notification { method, params }) = incoming.recv().await else { panic!("missing recovery notice") };
    assert_eq!(method, "missed"); assert_eq!(params["reason"], "backlog_budget");
    assert!(!peer.is_closed());
}

#[test]
fn variable_size_event_log_evicts_by_bytes_before_record_limit() {
    let mut core = Core::new(Store::in_memory(), None, "/tmp/unused".into(), false).unwrap();
    for _ in 0..1000 {
        core.emit("view.content", json!({"view":"same","content":{"label":"x".repeat(32768)}}));
    }
    assert!(core.events_since(0).is_none(), "32 MiB must not remain retained merely because count is below 10000");
    assert!(core.events_since(core.seq).unwrap().is_empty());
}
