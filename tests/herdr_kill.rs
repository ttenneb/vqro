//! Native kill reply reconciliation, no hc executable or real backend.
use std::time::Duration;
use prim::{client, server::{self,Backend}};
use serde_json::{json,Value};
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn kill_reply_finishes_resource_with_attach_open_without_finished_event() {
    let dir=tempfile::Builder::new().prefix("pk").tempdir_in("/tmp").unwrap();
    let fixture=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kill_reply_backend.py");
    let core=server::start(dir.path().join("s"),dir.path().join("st"),false,Backend::Herdr(vec!["python3".into(),fixture.to_string_lossy().into()]),None).unwrap();
    let (peer,_rx,_)=client::connect(&dir.path().join("s"),"kill-test").await.unwrap();
    let id=tokio::time::timeout(Duration::from_secs(2),async {
        loop { let list=peer.call("resource.list",Value::Null).await.unwrap(); if let Some(r)=list["resources"].as_array().unwrap().first() { break r["id"].clone(); } tokio::time::sleep(Duration::from_millis(5)).await; }
    }).await.unwrap();
    peer.call("resource.attach",json!({"resource":id})).await.unwrap();
    let result=peer.call("resource.kill",json!({"resource":id})).await.unwrap();
    assert_eq!(result["outcome"],"observed");
    let list=peer.call("resource.list",Value::Null).await.unwrap();
    let r=&list["resources"][0]; assert_eq!(r["id"],id); assert_eq!(r["state"],"finished");
    assert_eq!(r["exit"]["reason"],"killed"); assert!(r["exit"]["exit_code"].is_null(),"never invent an exit code");
    drop(peer); drop(core);
}
