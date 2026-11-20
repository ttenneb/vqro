//! A deliberately system-named workspace owner still cannot default-u core system work.
use serde_json::json;
use vqro::{client,principal::Identity,server::{self,Backend}};
#[tokio::test]
async fn system_named_owner_undo_selects_own_connection_work_not_system_bootstrap() {
    let root=tempfile::Builder::new().prefix("own-u").tempdir().unwrap();let socket=root.path().join("s");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();
    let(old,_,_)=client::connect_as(&socket,"old-owner","owner").await.unwrap();
    old.call("workspace.apply",json!({"ops":[{"op":"delete","key":"doc/principals/owner"}]})).await.unwrap();
    old.call("workspace.owner.bootstrap",json!({"principal":"system"})).await.unwrap();
    let(owner,_,hello)=client::connect_with_identity(&socket,"system-owner",Some(Identity::Owner)).await.unwrap();assert_eq!(hello["actor"],"system");
    assert_eq!(owner.call("command.invoke",json!({"name":"vqro.revert"})).await.unwrap_err().kind(),"not_found","bootstrap must never become a default-u target just because owner matches system");
    owner.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/own-user-work","value":1}]})).await.unwrap();
    owner.call("command.invoke",json!({"name":"vqro.revert"})).await.unwrap();
    // Existing u semantics may undo a revert (redo); that still targets only this user's work.
    owner.call("command.invoke",json!({"name":"vqro.revert"})).await.unwrap();
    assert_eq!(owner.call("workspace.get",json!({"key":"doc/principals/owner"})).await.unwrap()["value"],"system");
    server::shutdown(&core).await;
}
