//! A healthy wire-provider connection must not exhaust a lifetime effect-id ledger.
use std::time::Duration;
use vqro::{client, provider_client::{Config, Outcome, Provider, Request}, server::{self, Backend}};
use serde_json::{json, Value};

#[tokio::test(flavor="multi_thread",worker_threads=4)]
async fn more_than_65536_effects_on_one_healthy_connection_keep_working() {
    let dir=tempfile::Builder::new().prefix("pq2").tempdir_in("/tmp").unwrap();
    let socket=dir.path().join("s");
    let core=server::start(socket.clone(),dir.path().join("state"),false,Backend::Fake,None).unwrap();
    let caps=json!({"terminal.output":{"raw":true},"agent.steer":{}});
    let mut config=Config::new("rt","inst",caps.as_object().unwrap().clone());
    config.extension=Some("rt".into());
    let (provider,mut requests)=Provider::connect(tokio::net::UnixStream::connect(&socket).await.unwrap(),config).await.unwrap();
    provider.full_sync(vec![json!({"ref":"a1","kind":"agent","state":"live"})]).await.unwrap();
    tokio::spawn(async move {
        let _keep=provider;
        while let Some(request)=requests.recv().await {
            if let Request::Effect(effect)=request { effect.respond(Ok(Outcome::Accepted(Some(json!({"disposition":"queued"}))))); }
        }
    });
    let (viewer,_,_)=client::connect(&socket,"qa2").await.unwrap();
    let resource=tokio::time::timeout(Duration::from_secs(2),async {
        loop {
            let list=viewer.call("resource.list",Value::Null).await.unwrap();
            if let Some(r)=list["resources"].as_array().unwrap().iter().find(|r|r["provider"]=="rt") { break r["id"].clone(); }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }).await.unwrap();
    let mut failure=None;
    for i in 0..65_570u32 {
        match viewer.call("resource.invoke",json!({"resource":resource,"op":"agent.steer","args":{"message":"x"}})).await {
            Ok(r)=>assert_eq!(r["outcome"],"accepted"),
            Err(error)=>{ failure=Some((i,error.kind().to_string(),error.message)); break; }
        }
    }
    server::shutdown(&core).await;
    assert!(failure.is_none(),"healthy connection exhausted its ledger: {failure:?}");
}
