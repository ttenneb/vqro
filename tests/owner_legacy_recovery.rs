//! The oversized invalid legacy owner that startup cannot repair must not lock the TUI out.
use serde_json::json;
use vqro::{client,limits,principal::{self,Identity},server::{self,Backend}};
#[tokio::test]
async fn malformed_oversized_owner_allows_unset_hello_raw_shrink_and_explicit_repair() {
    let root=tempfile::Builder::new().prefix("own-l").tempdir().unwrap();let state=root.path().join("st");std::fs::create_dir(&state).unwrap();
    let legacy=json!({"rev":1,"next_txn":2,"entries":{
        "doc/principals/owner":{"version":1,"value":17},
        "doc/huge":{"version":1,"value":"x".repeat(limits::WORKSPACE_BYTES+1024)}},"history":[]});
    std::fs::write(state.join("workspace.json"),serde_json::to_vec(&legacy).unwrap()).unwrap();drop(legacy);
    let socket=root.path().join("s");let core=server::start(socket.clone(),state,false,Backend::Fake,None).unwrap();
    let(peer,_,hello)=client::connect_with_identity(&socket,"owner-recovery-tui",Some(Identity::Owner)).await.unwrap();
    assert_eq!(hello["actor"],"owner-unset");assert_eq!(hello["owner_unset"],true);
    assert!(hello["owner_notice"].as_str().unwrap().contains("bootstrap"));
    assert_eq!(peer.call("workspace.get",json!({"key":principal::OWNER_KEY})).await.unwrap()["value"],17,"failed startup growth did not claim a persisted repair");
    peer.call("workspace.apply",json!({"ops":[{"op":"delete","key":"doc/huge"}]})).await.unwrap();
    peer.call("workspace.owner.bootstrap",json!({"principal":"explicit-owner"})).await.unwrap();
    assert_eq!(peer.call("workspace.get",json!({"key":principal::OWNER_KEY})).await.unwrap()["value"],"explicit-owner");
    let(_,_,new_hello)=client::connect_with_identity(&socket,"new-owner-tui",Some(Identity::Owner)).await.unwrap();
    assert_eq!(new_hello["actor"],"explicit-owner");assert_eq!(new_hello["owner_unset"],false);
    server::shutdown(&core).await;
}
