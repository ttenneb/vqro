//! Connection-owned commands cannot outlive their owner or exhaust snapshot metadata.
use std::time::Duration;
use vqro::{client,server::{self,Backend}};
use serde_json::json;

#[tokio::test]
async fn disconnected_command_names_are_removed_instead_of_accumulating(){
    let root=tempfile::Builder::new().prefix("pcmd").tempdir_in("/tmp").unwrap();let socket=root.path().join("s");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();let baseline=core.lock().unwrap().commands.len();
    for i in 0..100 {
        let(peer,_rx,_)=client::connect(&socket,"command-owner").await.unwrap();let name=format!("temporary-{i}");
        peer.call("command.register",json!({"name":name,"title":"temporary"})).await.unwrap();peer.close();
        tokio::time::timeout(Duration::from_secs(2),async{while core.lock().unwrap().commands.contains_key(&name){tokio::task::yield_now().await;}}).await.expect("disconnected command must leave the registry");
        assert_eq!(core.lock().unwrap().commands.len(),baseline);
    }
    server::shutdown(&core).await;
}

#[tokio::test]
async fn command_metadata_admission_counts_escaped_bytes_and_replacement_credit(){
    let root=tempfile::Builder::new().prefix("pcmd").tempdir_in("/tmp").unwrap();let socket=root.path().join("s");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();let(peer,_rx,_)=client::connect(&socket,"command-owner").await.unwrap();
    let title="\0".repeat(70*1024); // ~420KiB encoded, not70KiB
    for name in ["budget.a","budget.b"] {peer.call("command.register",json!({"name":name,"title":title})).await.unwrap();}
    let error=peer.call("command.register",json!({"name":"budget.c","title":title})).await.unwrap_err();
    assert_eq!(error.kind(),"overloaded");assert!(!core.lock().unwrap().commands.contains_key("budget.c"));
    peer.call("command.register",json!({"name":"budget.a","title":"short"})).await.unwrap();
    peer.call("command.register",json!({"name":"budget.c","title":title})).await.unwrap();
    let list=peer.call("command.list",json!({})).await.unwrap();assert!(vqro::limits::encoded_bytes(&list["commands"])<=1024*1024);
    let oversized=peer.call("command.register",json!({"name":"budget.too-large","title":"\0".repeat(200*1024)})).await.unwrap_err();
    assert_eq!(oversized.kind(),"too_large");
    let snapshot=peer.call("workspace.snapshot",json!({})).await.unwrap();
    assert!(vqro::limits::encoded_bytes(&snapshot["commands"])<=1024*1024,"command names/titles cannot consume the recovery envelope");
    server::shutdown(&core).await;
}
