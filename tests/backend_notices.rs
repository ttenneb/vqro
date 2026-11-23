//! Process supervision and provider readiness are different facts. No live backend.
#[path = "support/python.rs"] mod test_python;
use std::time::Duration;
use vqro::{client,server::{self,Backend}};
use serde_json::{json,Value};

#[tokio::test]
async fn running_provider_process_is_not_reported_ready_before_registration() {
    let root=tempfile::tempdir().unwrap();
    let socket=root.path().join("p");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();
    let (peer,_,_)=client::connect(&socket,"backend-notice-test").await.unwrap();
    peer.call("workspace.apply",json!({"ops":[{"op":"set","key":"extension/offline","value":{
        "argv":["/bin/cat"],"kind":"provider","default":true}}]})).await.unwrap();
    let state=peer.call("extension.list",Value::Null).await.unwrap()["extensions"]["offline"].clone();
    assert_eq!(state["status"],"running","the supervisor process really is running");
    assert_eq!(state["display_status"],"provider-not-connected");
    let snap=peer.call("workspace.snapshot",Value::Null).await.unwrap();
    assert!(snap["backend_notice"].as_str().unwrap().contains("offline"));
    let fixture=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_provider.py");
    peer.call("workspace.apply",json!({"ops":[{"op":"set","key":"extension/offline","value":{
        "argv":[test_python::python(),fixture,"--name","different-provider-name"],"kind":"provider","default":true}}]})).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            let snap=peer.call("workspace.snapshot",Value::Null).await.unwrap();
            if snap["providers"]["different-provider-name"]["synced"]==true {
                assert_eq!(snap["extensions"]["offline"]["display_status"],"running");
                assert!(snap["backend_notice"].is_null());
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    let events=core.lock().unwrap().events_since(0).unwrap();
    assert!(events.iter().any(|e|e["type"]=="entries" && e["data"]["backend_notice"].as_str().is_some_and(|n|n.contains("offline"))),"default changes must update the live notice");
    assert!(events.iter().any(|e|e["type"]=="provider" && e["data"]["name"]=="different-provider-name" &&
        e["data"]["state"]["synced"]==true && e["data"].get("backend_notice")==Some(&Value::Null)),"readiness must clear the live notice");
    peer.call("workspace.apply",json!({"ops":[{"op":"set","key":"extension/offline","value":{
        "argv":["/bin/cat"],"kind":"provider","default":true}}]})).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            let state=peer.call("extension.list",Value::Null).await.unwrap()["extensions"]["offline"].clone();
            if state["display_status"]=="provider-not-connected" {
                assert_eq!(state["status"],"running");
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    server::shutdown(&core).await;
}
