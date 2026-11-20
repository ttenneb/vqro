//! Combined C1+C2: generic protected writes retain scoped identity without entering workspace undo.
use serde_json::{json,Value};
use vqro::{client,rpc::Incoming,server::{self,Backend},store::{Store,Op}};
#[path="support/managed.rs"] mod managed;
#[tokio::test]
async fn delegated_operational_callback_keeps_both_principals_without_global_history() {
    let root=tempfile::Builder::new().prefix("c2op").tempdir().unwrap();let state=root.path().join("st");std::fs::create_dir(&state).unwrap();
    let token_file=root.path().join("writer.token");
    let mut store=Store::open(state.join("workspace.json")).unwrap();
    store.apply("fixture",None,vec![Op::Set{key:"extension/writer".into(),value:managed::spec(&token_file),expect:None},Op::Set{key:"doc/principals/owner".into(),value:json!("owner"),expect:None}]).unwrap();drop(store);
    let socket=root.path().join("s");let core=server::start(socket.clone(),state.clone(),false,Backend::Fake,None).unwrap();
    let(owner,mut events,_)=client::connect_as(&socket,"owner","owner").await.unwrap();
    let(alice,_,_)=client::connect_as(&socket,"initiator","alice").await.unwrap();let(bob,_,_)=client::connect_as(&socket,"effective","bob").await.unwrap();
    owner.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/principals/delegations/b-a","value":{"from":"bob","to":"alice"}}]})).await.unwrap();
    let(writer,mut commands,_)=client::connect(&socket,"writer").await.unwrap();
    let hello=writer.call("hello",json!({"extension":"writer","extension_token":managed::token(&token_file).await})).await.unwrap();
    assert_eq!(hello["actor"],"extension:writer");
    assert!(hello["provides"].as_array().unwrap().iter().any(|value|value=="ability/vqro.operational-domains@1"));
    let before=owner.call("workspace.history",json!({"limit":100})).await.unwrap();
    let registration_cause=json!({"kind":"delivery","id":"global-registration","provenance":"asserted"});
    writer.call("operational.register",json!({"name":"state","cause":registration_cause,"correlation":"registration-thread"})).await.unwrap();
    let registration:Value=serde_json::from_slice(&std::fs::read(state.join("operational/state.json")).unwrap()).unwrap();
    assert_eq!(registration["last"]["attribution"]["cause"],registration_cause);assert_eq!(registration["last"]["attribution"]["correlation"],"registration-thread");
    writer.call("command.register",json!({"name":"writer.op"})).await.unwrap();
    owner.call("events.subscribe",json!({"types":["operational"]})).await.unwrap();
    let caller=alice.clone();let invocation=tokio::spawn(async move{caller.call("command.invoke",json!({"name":"writer.op","on_behalf_of":"bob"})).await});
    let(id,params)=loop{match commands.recv().await.unwrap(){Incoming::Request{id,method,params}if method=="command.invoke"=>break(id,params),_=>{}}};
    assert_eq!(params["principal"],"bob");assert_eq!(params["initiator"],"alice");
    let cause=json!({"kind":"request","id":"caller-local","endpoint":"prior-endpoint","epoch":"prior-epoch","provenance":"adapter","extra":{"keep":true}});
    writer.call("operational.apply",json!({"domain":"state","invocation":params["invocation"],"label":"diagnostic only","cause":cause,"correlation":"op-thread","ops":[{"op":"set","key":"operational/state/a","value":1}]})).await.unwrap();
    let event=tokio::time::timeout(std::time::Duration::from_secs(2),async {loop {if let Incoming::Notification{method,params}=events.recv().await.unwrap(){if method=="event" && params["type"]=="operational"{break params["data"].clone();}}}}).await.unwrap();
    assert_eq!(event["cause"],cause);assert_eq!(event["correlation"],"op-thread");
    assert_eq!(event["actor"],"extension:writer");assert_eq!(event["principal"],"bob");assert_eq!(event["initiator"],"alice");assert_eq!(event["author"],hello["actual_author"]);
    let bytes=std::fs::read(state.join("operational/state.json")).unwrap();let persisted:Value=serde_json::from_slice(&bytes).unwrap();
    let author=&persisted["last"]["attribution"];
    assert_eq!(author["cause"],cause);assert_eq!(author["correlation"],"op-thread");
    assert_eq!(author["author"],hello["actual_author"]);assert_eq!(author["actor"],"extension:writer");assert_eq!(author["principal"],"bob");assert_eq!(author["initiator"],"alice");assert_eq!(author["label"],"diagnostic only");
    assert!(!String::from_utf8(bytes).unwrap().contains(params["invocation"].as_str().unwrap()),"proof never persists in operational authority");
    writer.respond(id,Ok(json!({})));invocation.await.unwrap().unwrap();
    assert_eq!(writer.call("operational.apply",json!({"domain":"state","invocation":params["invocation"],"ops":[{"op":"set","key":"operational/state/late","value":1}]})).await.unwrap_err().kind(),"forbidden");
    assert_eq!(owner.call("workspace.history",json!({"limit":100})).await.unwrap(),before,"neither registration nor attributed operational traffic enters global undo");
    assert_eq!(bob.call("command.invoke",json!({"name":"vqro.revert"})).await.unwrap_err().kind(),"not_found");
    assert_eq!(writer.call("workspace.apply",json!({"history":false,"ops":[{"op":"set","key":"doc/bypass","value":1}]})).await.unwrap_err().kind(),"forbidden");
    let snapshot=owner.call("operational.snapshot",json!({"domain":"state"})).await.unwrap();
    assert_eq!(snapshot["entries"]["operational/state/a"]["value"],1);
    server::shutdown(&core).await;
}
