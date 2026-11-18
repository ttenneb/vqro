//! Scoped reads must stay small on the provider wire even when workspace snapshots do not.
use prim::client;
use prim::server::{start, Backend};
use serde_json::{json, Value};

#[tokio::test]
async fn scoped_reads_work_under_the_unchanged_provider_record_cap() {
    let dir = tempfile::Builder::new().prefix("pquery").tempdir_in("/tmp").unwrap();
    let socket = dir.path().join("s.sock");
    let core = start(socket.clone(), dir.path().join("state"), false, Backend::Fake, None).unwrap();
    let (control, _, _) = client::connect(&socket, "query-test").await.unwrap();
    control.call("workspace.apply", json!({"ops": [
        {"op": "set", "key": "doc/large/one", "value": "x".repeat(650_000)},
        {"op": "set", "key": "doc/large/two", "value": "y".repeat(650_000)},
        {"op": "set", "key": "view/owned", "value": {"kind": "panel", "bridge": {"instance": "inst", "ref": "agent"}, "items": []}}
    ]})).await.unwrap();
    let snapshot = control.call("workspace.snapshot", json!({})).await.unwrap();
    assert!(serde_json::to_vec(&snapshot).unwrap().len() > 1_048_576);
    let (provider, _, _) = client::connect(&socket, "query-provider").await.unwrap();
    let registered = provider.call("provider.register", json!({"name": "query-provider", "instance": "inst", "role": "backend",
        "software": {"name": "query-test", "version": "1"}, "provider_version": "0.1.0", "capabilities": {},
        "limits": {"max_streams": 1, "stream_window_bytes": 65536}})).await.unwrap();
    let generation = registered["generation"].clone();
    assert_eq!(registered["limits"]["max_record_bytes"], 1_048_576, "do not hide the bug by raising the wire cap");
    provider.call("provider.sync", json!({"generation": generation, "final": true, "resources": [
        {"ref": "agent", "incarnation": "one", "kind": "agent", "state": "live"},
        {"ref": "other", "incarnation": "one", "kind": "terminal", "state": "live"}
    ]})).await.unwrap();
    let oversized = provider.call("workspace.snapshot", json!({"generation": generation})).await.unwrap_err();
    assert_eq!(oversized.kind(), "too_large", "the provider still cannot read the aggregate snapshot");
    let resource = provider.call("resource.list", json!({"generation": generation, "provider": "query-provider", "backend_ref": "agent"})).await.unwrap();
    assert_eq!(resource["resources"].as_array().unwrap().len(), 1);
    assert_eq!(resource["resources"][0]["backend_ref"], "agent");
    assert!(serde_json::to_vec(&resource).unwrap().len() < 4096);
    let entry = provider.call("workspace.get", json!({"generation": generation, "key": "view/owned"})).await.unwrap();
    assert_eq!(entry, snapshot["entries"]["view/owned"]);
    assert!(serde_json::to_vec(&entry).unwrap().len() < 4096);
    let missing = provider.call("workspace.get", json!({"generation": generation, "key": "view/missing"})).await.unwrap();
    assert!(missing.is_null());
    assert_eq!(control.call("resource.list", json!({"provider": "query-provider"})).await.unwrap()["resources"].as_array().unwrap().len(), 2);
    assert!(control.call("resource.list", json!({"provider": "query-provider-prefix"})).await.unwrap()["resources"].as_array().unwrap().is_empty());
    assert_eq!(control.call("resource.list", json!({"backend_ref": "other"})).await.unwrap()["resources"].as_array().unwrap().len(), 1);
    assert_eq!(control.call("resource.list", Value::Null).await.unwrap()["resources"].as_array().unwrap().len(), 2, "unfiltered callers keep their existing behavior");
    prim::server::shutdown(&core).await;
}

#[tokio::test]
async fn malformed_scoped_read_params_are_rejected_without_effects() {
    let dir = tempfile::Builder::new().prefix("pquery").tempdir_in("/tmp").unwrap();
    let socket = dir.path().join("s.sock");
    let core = start(socket.clone(), dir.path().join("state"), false, Backend::Fake, None).unwrap();
    let (peer, _, _) = client::connect(&socket, "query-test").await.unwrap();
    for params in [json!({"provider": 7}), json!({"backend_ref": []}),json!({"id":false}),json!({"host":7})] {
        assert_eq!(peer.call("resource.list", params).await.unwrap_err().kind(), "invalid");
    }
    for params in [json!({}), json!({"key": true})] {
        assert_eq!(peer.call("workspace.get", params).await.unwrap_err().kind(), "invalid");
    }
    prim::server::shutdown(&core).await;
}
