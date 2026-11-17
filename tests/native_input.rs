//! A terminal's raw stream input is not automatically an effect.invoke operation.
use std::{path::PathBuf,time::Duration};
use base64::Engine;
use prim::{client,rpc::Incoming,server::{self,Backend}};
use serde_json::{json,Value};

async fn stream_only_input(backend: Backend, name: &str, spawn: bool) {
    let dir=tempfile::Builder::new().prefix("ni").tempdir_in("/tmp").unwrap();
    let socket=dir.path().join("s");
    let core=server::start(socket.clone(),dir.path().join("st"),false,backend,None).unwrap();
    let (peer,mut output,_)=client::connect(&socket,"native-input-test").await.unwrap();
    let resource=if spawn {
        peer.call("resource.spawn",json!({"argv":["sh"]})).await.unwrap()["resource"]["id"].clone()
    } else {
        tokio::time::timeout(Duration::from_secs(2),async {
            loop { let snap=peer.call("resource.list",Value::Null).await.unwrap(); if let Some(r)=snap["resources"].as_array().unwrap().first() { break r["id"].clone(); } tokio::time::sleep(Duration::from_millis(5)).await; }
        }).await.unwrap()
    };
    let snap=peer.call("workspace.snapshot",Value::Null).await.unwrap();
    assert_eq!(snap["providers"][name]["capabilities"]["terminal.input"]["effect"],false);
    let error=peer.call("resource.invoke",json!({"resource":resource,"op":"terminal.input","args":{"text":"\u{4}"}})).await.unwrap_err();
    assert_eq!(error.kind(),"unsupported");assert_eq!(error.data.unwrap()["outcome"],"rejected");
    let attached=peer.call("resource.attach",json!({"resource":resource})).await.unwrap();
    let result=peer.call("stream.input",json!({"stream":attached["stream"],"text":"\u{4}"})).await.unwrap();
    assert_eq!(result["outcome"],"dispatched");
    let mut received = false;
    tokio::time::timeout(Duration::from_secs(2),async {
        while let Some(Incoming::Notification{method,params})=output.recv().await {
            if method=="stream.data" && base64::engine::general_purpose::STANDARD.decode(params["data"].as_str().unwrap()).unwrap().contains(&4) { received = true; break; }
        }
    }).await.unwrap();
    assert!(received, "stream must carry literal EOT without an effect invocation");
    server::shutdown(&core).await;
}
#[tokio::test]
async fn transport_flags_are_generic_and_do_not_invent_unsupported_effects() {
    let dir=tempfile::Builder::new().prefix("nt").tempdir_in("/tmp").unwrap(); let socket=dir.path().join("s");
    let core=server::start(socket.clone(),dir.path().join("st"),false,Backend::Fake,None).unwrap();
    let (provider,mut requests,_)=client::connect(&socket,"provider-test").await.unwrap();
    let generation=provider.call("provider.register",json!({"name":"transports","role":"backend","instance":"i","software":{"name":"fixture","version":"1"},"provider_version":"1",
        "capabilities":{"fixture.stream_only":{"effect":false},"terminal.input":{"effect":true,"stream":false},"terminal.output":{}}})).await.unwrap()["generation"].clone();
    provider.call("provider.sync",json!({"generation":generation,"resources":[{"ref":"t","kind":"terminal","state":"live"}],"final":true})).await.unwrap();
    let (viewer,_rx,_)=client::connect(&socket,"viewer").await.unwrap();
    let id=viewer.call("resource.list",Value::Null).await.unwrap()["resources"][0]["id"].clone();
    let error=viewer.call("resource.invoke",json!({"resource":id,"op":"fixture.stream_only","deadline_ms":20})).await.unwrap_err();
    assert_eq!(error.kind(),"unsupported");assert_eq!(error.data.unwrap()["outcome"],"rejected");
    assert!(requests.try_recv().is_err(),"effect:false must reject before reaching the provider");
    let (caller,target)=(viewer.clone(),id.clone());
    let invoke=tokio::spawn(async move { caller.call("resource.invoke",json!({"resource":target,"op":"terminal.input","args":{"text":"x"}})).await });
    let Some(Incoming::Request{id:call,method,params})=requests.recv().await else {panic!("missing native effect")};
    assert_eq!(method,"effect.invoke");assert_eq!(params["op"],"terminal.input");assert_eq!(params["args"]["text"],"x");
    provider.respond(call,Ok(json!({"outcome":"dispatched","data":{"bytes":1}})));
    assert_eq!(invoke.await.unwrap().unwrap()["outcome"],"dispatched");
    let (caller,target)=(viewer.clone(),id);
    let attach=tokio::spawn(async move {caller.call("resource.attach",json!({"resource":target})).await});
    let Some(Incoming::Request{id:call,method,..})=requests.recv().await else {panic!("missing native open")};assert_eq!(method,"stream.open");
    provider.respond(call,Ok(json!({"initial":"none"})));
    let stream=attach.await.unwrap().unwrap()["stream"].clone();
    assert_eq!(viewer.call("stream.input",json!({"stream":stream,"text":"x"})).await.unwrap_err().kind(),"unsupported");
    assert!(requests.try_recv().is_err(),"stream:false must reject before provider notification");
    server::shutdown(&core).await;
}

#[tokio::test]
async fn native_fake_declares_stream_only_input_and_preserves_literal_control_bytes() { stream_only_input(Backend::Fake,"fake",true).await; }
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn native_herdr_declares_stream_only_input_and_preserves_literal_control_bytes() {
    let fixture=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kill_reply_backend.py");
    stream_only_input(Backend::Herdr(vec!["python3".into(),fixture.to_string_lossy().into()]),"herdr",false).await;
}
