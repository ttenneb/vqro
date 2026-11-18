//! Malformed explicit undo targets must never silently become default/latest undo.
use prim::{client,server::{self,Backend}};
use serde_json::{json,Value};
#[tokio::test]
async fn explicit_noninteger_revert_targets_are_invalid_without_any_mutation(){
    let root=tempfile::Builder::new().prefix("prev").tempdir_in("/tmp").unwrap();let socket=root.path().join("s");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();let(peer,_rx,_)=client::connect(&socket,"undo-targets").await.unwrap();
    for(key,value)in[("doc/first",1),("doc/latest",2)] {peer.call("workspace.apply",json!({"ops":[{"op":"set","key":key,"value":value}]})).await.unwrap();}
    for txn in [json!("1"),json!(-1),json!(1.0),json!(true),Value::Null,json!({"id":1})] {
        let result=peer.call("command.invoke",json!({"name":"prim.revert","args":{"txn":txn}})).await;
        assert_eq!(result.unwrap_err().kind(),"invalid","target {txn}");
        let history=peer.call("workspace.history",json!({"limit":10})).await.unwrap();assert_eq!(history["txns"].as_array().unwrap().len(),2);
    }
    peer.call("command.invoke",json!({"name":"prim.revert","args":{"txn":1}})).await.unwrap();
    assert!(peer.call("workspace.get",json!({"key":"doc/first"})).await.unwrap().is_null());
    assert_eq!(peer.call("workspace.get",json!({"key":"doc/latest"})).await.unwrap()["value"],2);
    server::shutdown(&core).await;
}
