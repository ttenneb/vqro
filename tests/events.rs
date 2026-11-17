//! Event subscription options: type filters and partial resource events.

use std::time::Duration;

use prim::client;
use prim::rpc::{Incoming, Peer};
use prim::server::{start, Backend};
use serde_json::{json, Value};

async fn next_event(rx: &mut prim::rpc::IncomingReceiver) -> (String, Value) {
    loop {
        match tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.expect("event in time").expect("connection open") {
            Incoming::Notification { method, params } if method == "event" || method == "missed" => return (method, params),
            _ => {}
        }
    }
}

async fn setup() -> (tempfile::TempDir, prim::core::Shared, Peer) {
    let dir = tempfile::Builder::new().prefix("pev").tempdir_in("/tmp").unwrap();
    let core = start(dir.path().join("s.sock"), dir.path().join("state"), false, Backend::Fake, None).unwrap();
    let (p, _, _) = client::connect(&dir.path().join("s.sock"), "test").await.unwrap();
    (dir, core, p)
}

#[tokio::test]
async fn partial_resource_events_send_the_full_record_first_then_only_changes() {
    let (dir, _core, p) = setup().await;
    let (sub, mut rx, _) = client::connect(&dir.path().join("s.sock"), "sub").await.unwrap();
    sub.call("events.subscribe", json!({"partial_resources": true, "types": ["resource"]})).await.unwrap();
    let r = p.call("resource.spawn", json!({"argv": ["agent", "--status-every-ms", "50"], "name": "a"})).await.unwrap();
    let id = r["resource"]["id"].as_str().unwrap().to_string();
    // Not a resource event: filtered out.
    p.call("workspace.apply", json!({"ops": [{"op": "set", "key": "doc/test/x", "value": 1}]})).await.unwrap();

    let (_, first) = next_event(&mut rx).await;
    assert_eq!(first["type"], "resource");
    assert_eq!(first["data"]["id"], id.as_str());
    assert!(first.get("partial").is_none(), "first event for a resource is the full record: {first}");
    assert_eq!(first["data"]["argv"][0], "agent");

    let (_, second) = next_event(&mut rx).await;
    assert_eq!(second["type"], "resource", "only subscribed types arrive: {second}");
    assert_eq!(second["partial"], true);
    assert_eq!(second["data"]["id"], id.as_str());
    assert!(second["data"]["status"]["n"].is_u64());
    assert!(second["data"].get("argv").is_none(), "unchanged fields are left out: {second}");
}

#[tokio::test]
async fn subscribers_without_options_get_full_events_of_every_type() {
    let (dir, _core, p) = setup().await;
    let (sub, mut rx, _) = client::connect(&dir.path().join("s.sock"), "sub").await.unwrap();
    sub.call("events.subscribe", json!({})).await.unwrap();
    p.call("resource.spawn", json!({"argv": ["agent", "--status-every-ms", "50"]})).await.unwrap();
    p.call("workspace.apply", json!({"ops": [{"op": "set", "key": "doc/test/x", "value": 1}]})).await.unwrap();
    let (mut resources, mut entries) = (0, 0);
    while resources < 3 || entries < 1 {
        let (_, ev) = next_event(&mut rx).await;
        match ev["type"].as_str() {
            Some("resource") => {
                assert!(ev.get("partial").is_none());
                assert_eq!(ev["data"]["argv"][0], "agent");
                resources += 1;
            }
            Some("entries") => entries += 1,
            _ => {}
        }
    }
}

#[tokio::test]
async fn live_view_allowlist_filters_both_replay_and_subsequent_publication() {
    let (dir, _core, p) = setup().await;
    let snapshot = p.call("workspace.snapshot", json!({})).await.unwrap();
    p.call("view.publish", json!({"view": "sidebar", "content": {"text": "x".repeat(500_000)}})).await.unwrap();
    p.call("view.publish", json!({"view": "themes", "content": {"text": "wanted"}})).await.unwrap();
    let (sub, mut rx, _) = client::connect(&dir.path().join("s.sock"), "filtered").await.unwrap();
    sub.call("events.subscribe", json!({"from": snapshot["seq"], "epoch": snapshot["epoch"], "types": ["view.content"],
        "view_content": {"names": ["themes"], "prefixes": ["agent-"]}})).await.unwrap();
    let (_, first) = next_event(&mut rx).await;
    assert_eq!(first["data"]["view"], "themes", "unrelated large replay must not reach the subscriber");
    p.call("view.publish", json!({"view": "sidebar", "content": null})).await.unwrap();
    p.call("view.publish", json!({"view": "agent-r1", "content": {"text": "wanted prefix"}})).await.unwrap();
    let (_, next) = next_event(&mut rx).await;
    assert_eq!(next["data"]["view"], "agent-r1");
    p.call("view.publish", json!({"view": "agent-r1", "content": null})).await.unwrap();
    let (_, removed) = next_event(&mut rx).await;
    assert_eq!(removed["data"]["view"], "agent-r1");
    assert!(removed["data"]["content"].is_null(), "filtered subscribers still observe matching view removal");
}

#[tokio::test]
async fn a_malformed_view_filter_is_rejected() {
    let (_dir, _core, p) = setup().await;
    let err = p.call("events.subscribe", json!({"view_content": {"names": "sidebar"}})).await.unwrap_err();
    assert_eq!(err.kind(), "invalid");
}

#[tokio::test]
async fn a_malformed_type_filter_is_rejected() {
    let (_dir, _core, p) = setup().await;
    let err = p.call("events.subscribe", json!({"types": "resource"})).await.unwrap_err();
    assert_eq!(err.kind(), "invalid");
}
