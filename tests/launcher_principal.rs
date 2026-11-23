#![cfg(unix)] // Unix-socket/signal fixtures; on Windows the lib's unit tests run (docs/windows.md).
//! Launcher assignment is core-owned resource metadata, not a child/provider identity assertion.
use serde_json::{json,Value};
use std::sync::{Arc,Mutex};
use vqro::{client,provider_client::{Config,Provider,Request,Outcome},server::{self,Backend},store::{Store,Op}};
#[path="support/managed.rs"] mod managed;
#[tokio::test]
async fn spawn_assignment_survives_refresh_not_reincarnation_and_reserved_defaults_are_raw() {
    let root=tempfile::Builder::new().prefix("launch-p").tempdir().unwrap();let state=root.path().join("st");std::fs::create_dir(&state).unwrap();let credential=root.path().join("token");
    let mut store=Store::open(state.join("workspace.json")).unwrap();store.apply("fixture",None,vec![Op::Set{key:"doc/principals/owner".into(),value:json!("owner"),expect:None},Op::Set{key:"extension/writer".into(),value:managed::spec(&credential),expect:None}]).unwrap();drop(store);
    let socket=root.path().join("s");let core=server::start(socket.clone(),state,false,Backend::Fake,None).unwrap();
    let config=Config::new("launch-probe","stable",json!({"terminal.spawn":{"env":true}}).as_object().unwrap().clone());
    let(provider,mut requests)=Provider::connect(tokio::net::UnixStream::connect(&socket).await.unwrap(),config).await.unwrap();provider.sync(vec![],true).await.unwrap();
    let ledger:Arc<Mutex<Vec<Value>>>=Arc::default();let captured=ledger.clone();let synchronized=provider.clone();
    let worker=tokio::spawn(async move{let mut n=0;let mut inventory=Vec::new();while let Some(request)=requests.recv().await {if let Request::Effect(effect)=request {n+=1;captured.lock().unwrap().push(effect.args.clone());let facts=json!({"ref":format!("t{n}"),"incarnation":"one","kind":"terminal","state":"live","argv":[],"principal":"provider-spoof","data":{"principal":"child-spoof"}});inventory.push(facts.clone());synchronized.sync(inventory.clone(),true).await.unwrap();effect.respond(Ok(Outcome::Observed(Some(json!({"facts":facts})))));}}});
    let(alice,mut events,_)=client::connect_as(&socket,"alice","alice").await.unwrap();
    alice.call("events.subscribe",json!({"types":["resource"]})).await.unwrap();
    let one=alice.call("resource.spawn",json!({"provider":"launch-probe"})).await.unwrap()["resource"].clone();
    assert_eq!(one["principal"],"alice");
    tokio::time::timeout(std::time::Duration::from_secs(2),async{loop{if let Some(vqro::rpc::Incoming::Notification{method,params})=events.recv().await{if method=="event" && params["type"]=="resource" && params["data"]["id"]==one["id"] && params["data"]["principal"]=="alice"{break}}}}).await.unwrap();
    let snapshot=alice.call("workspace.snapshot",json!({})).await.unwrap();assert!(snapshot["resources"].as_array().unwrap().iter().any(|resource|resource["id"]==one["id"] && resource["principal"]=="alice"));
    assert_eq!(ledger.lock().unwrap()[0]["env"]["VQRO_PRINCIPAL"],"alice");
    let(writer,_,_)=client::connect(&socket,"writer").await.unwrap();writer.call("hello",json!({"extension":"writer","extension_token":managed::token(&credential).await})).await.unwrap();
    let two=writer.call("resource.spawn",json!({"provider":"launch-probe"})).await.unwrap()["resource"].clone();
    assert_eq!(ledger.lock().unwrap()[1]["env"]["VQRO_PRINCIPAL"],"undeclared");assert_eq!(two["principal"],"undeclared");
    let three=writer.call("resource.spawn",json!({"provider":"launch-probe","env":{"VQRO_PRINCIPAL":"p:explicit"}})).await.unwrap()["resource"].clone();
    assert_eq!(three["principal"],"p:explicit");assert_eq!(ledger.lock().unwrap()[2]["env"]["VQRO_PRINCIPAL"],"p:explicit");
    let invalid=writer.call("resource.spawn",json!({"provider":"launch-probe","env":{"VQRO_PRINCIPAL":"x".repeat(257)}})).await.unwrap()["resource"].clone();assert!(invalid["principal"].is_null(),"invalid identifier is not a known assigned principal");
    provider.sync(vec![json!({"ref":"t1","incarnation":"one","kind":"terminal","state":"live","argv":[],"principal":"forged","data":{"principal":"forged"}})],true).await.unwrap();
    let refreshed=alice.call("resource.list",json!({"provider":"launch-probe"})).await.unwrap()["resources"][0].clone();assert_eq!(refreshed["id"],one["id"]);assert_eq!(refreshed["principal"],"alice");
    provider.sync(vec![json!({"ref":"t1","incarnation":"two","kind":"terminal","state":"live","argv":[],"principal":"forged","data":{"principal":"forged"}})],true).await.unwrap();
    let replaced=alice.call("resource.list",json!({"provider":"launch-probe"})).await.unwrap()["resources"][0].clone();assert_ne!(replaced["id"],one["id"]);assert!(replaced["principal"].is_null(),"no backfill from provider/child reports");
    server::shutdown(&core).await;drop(provider);worker.abort();let _=worker.await;
}
#[tokio::test]
async fn unsupported_native_environment_never_invents_an_assignment() {
    let root=tempfile::Builder::new().prefix("launch-noenv").tempdir().unwrap();let socket=root.path().join("s");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();
    let config=Config::new("no-env","stable",json!({"terminal.spawn":{"env":false}}).as_object().unwrap().clone());
    let(provider,mut requests)=Provider::connect(tokio::net::UnixStream::connect(&socket).await.unwrap(),config).await.unwrap();provider.sync(vec![],true).await.unwrap();
    let worker=tokio::spawn(async move{if let Some(Request::Effect(effect))=requests.recv().await {assert!(effect.args["env"].get("VQRO_PRINCIPAL").is_none());effect.respond(Ok(Outcome::Observed(Some(json!({"facts":{"ref":"native","incarnation":"one","kind":"terminal","state":"live","argv":[],"principal":"forged","data":{}}})))));}});
    let(alice,_,_)=client::connect_as(&socket,"alice","alice").await.unwrap();
    let resource=alice.call("resource.spawn",json!({"provider":"no-env"})).await.unwrap()["resource"].clone();assert!(resource["principal"].is_null());
    worker.await.unwrap();server::shutdown(&core).await;drop(provider);
}
