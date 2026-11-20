//! Caller-assertion reservations also apply to explicit owner bootstrap and protected configuration.
use serde_json::json;
use vqro::{client,server::{self,Backend}};
#[tokio::test]
async fn reserved_owner_targets_are_rejected_without_adopting_any_write() {
    let root=tempfile::Builder::new().prefix("own-r").tempdir().unwrap();let socket=root.path().join("s");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();
    let(owner,_,_)=client::connect_as(&socket,"owner","owner").await.unwrap();
    for principal in ["system","system:future","extension:managed","host:rid"] {
        assert_eq!(owner.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/principals/owner","value":principal}]})).await.unwrap_err().kind(),"invalid");
        assert_eq!(owner.call("workspace.get",json!({"key":"doc/principals/owner"})).await.unwrap()["value"],"owner");
    }
    owner.call("workspace.apply",json!({"ops":[{"op":"delete","key":"doc/principals/owner"}]})).await.unwrap();
    for principal in ["system","system:future","extension:managed","host:rid"] {
        assert_eq!(owner.call("workspace.owner.bootstrap",json!({"principal":principal})).await.unwrap_err().kind(),"invalid");
    }
    owner.call("workspace.owner.bootstrap",json!({"principal":"explicit-owner"})).await.unwrap();
    server::shutdown(&core).await;
}
