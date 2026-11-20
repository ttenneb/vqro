//! C9: native receipts already echo explicit links; the core audit must not drop them.
use serde_json::{json,Value};
use vqro::{client,provider_client::{Config,Provider,Request,Outcome},rpc::Incoming,server::{self,Backend}};
#[tokio::test]
async fn c9_explicit_qualified_links_reach_native_and_core_audit_unchanged() {
    let root=tempfile::Builder::new().prefix("cause").tempdir().unwrap();let socket=root.path().join("s");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();
    let config=Config::new("causality-native","fixed",json!({"agent.send":{}}).as_object().unwrap().clone());
    let(provider,mut requests)=Provider::connect(tokio::net::UnixStream::connect(&socket).await.unwrap(),config).await.unwrap();
    provider.sync(vec![json!({"ref":"agent","incarnation":"one","kind":"agent","state":"live","argv":[]})],true).await.unwrap();
    let worker=tokio::spawn(async move{while let Some(request)=requests.recv().await{if let Request::Effect(effect)=request {
        let receipt=json!({"cause":effect.args["cause"],"correlation":effect.args["correlation"]});
        effect.respond(Ok(Outcome::Observed(Some(receipt))));
    }}});
    let(peer,mut events,_)=client::connect_as(&socket,"C9","owner").await.unwrap();
    let resource=peer.call("resource.list",json!({"provider":"causality-native"})).await.unwrap()["resources"][0]["id"].clone();
    peer.call("events.subscribe",json!({"types":["effect"]})).await.unwrap();
    // Actual failing C9 trace: deliberately local id, explicitly qualified by boot and endpoint.
    let cause=json!({"kind":"effect","id":"same-local-effect","endpoint":"r2","epoch":"170cc6555b6f10b1a6ee59799a59c5a0.e1","boot":"d93997a46f759a9e960f842cab044660","provenance":"asserted"});
    let correlation=json!("C9-caller-thread");
    let reply=peer.call("resource.invoke",json!({"resource":resource,"op":"agent.send","args":{"v":0,"cause":cause,"correlation":correlation}})).await.unwrap();
    assert_eq!(reply["data"]["cause"],cause);assert_eq!(reply["data"]["correlation"],correlation);
    let audit:Value=tokio::time::timeout(std::time::Duration::from_secs(2),async{loop{if let Incoming::Notification{method,params}=events.recv().await.unwrap(){if method=="event" && params["type"]=="effect"{break params["data"].clone();}}}}).await.unwrap();
    assert_eq!(audit["cause"],cause,"generic core effect audit dropped caller causality (C9)");
    assert_eq!(audit["correlation"],correlation);
    server::shutdown(&core).await;drop(provider);worker.abort();let _=worker.await;
}
#[tokio::test]
async fn root_sdk_links_are_not_rewritten_and_ambiguous_or_unqualified_calls_never_dispatch() {
    use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
    let root=tempfile::Builder::new().prefix("cause-r").tempdir().unwrap();let socket=root.path().join("s");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();
    let config=Config::new("causal-root","fixed",json!({"future.op":{}}).as_object().unwrap().clone());
    let(provider,mut requests)=Provider::connect(tokio::net::UnixStream::connect(&socket).await.unwrap(),config).await.unwrap();
    provider.sync(vec![json!({"ref":"opaque","incarnation":"one","kind":"future","state":"live","argv":[]})],true).await.unwrap();
    let count=Arc::new(AtomicUsize::new(0));let ledger=count.clone();
    let worker=tokio::spawn(async move{while let Some(request)=requests.recv().await{if let Request::Effect(effect)=request {
        ledger.fetch_add(1,Ordering::SeqCst);
        let observed=json!({"cause":effect.cause(),"correlation":effect.correlation(),"args":effect.args});
        effect.respond(Ok(Outcome::Observed(Some(observed))));
    }}});
    let(peer,_,_)=client::connect_as(&socket,"root-cause","owner").await.unwrap();
    let resource=peer.call("resource.list",json!({"provider":"causal-root"})).await.unwrap()["resources"][0]["id"].clone();
    let args=json!({"opaque":{"cause":"not metadata"}});let mut effects=Vec::new();
    for boot in ["prior-boot-A","prior-boot-B"] {
        let cause=json!({"kind":"effect","id":"same-local","boot":boot,"endpoint":"old-endpoint","epoch":"old-epoch","provenance":"native","future":{"preserved":true}});
        let reply=peer.call("resource.invoke",json!({"resource":resource,"op":"future.op","args":args,"cause":cause,"correlation":"same-thread"})).await.unwrap();
        assert_eq!(reply["data"]["cause"],cause);assert_eq!(reply["data"]["correlation"],"same-thread");assert_eq!(reply["data"]["args"],args);effects.push(reply["effect"].clone());
    }
    assert_ne!(effects[0],effects[1],"caller links never become effect ids or deduplication keys");
    let absent=peer.call("resource.invoke",json!({"resource":resource,"op":"future.op","args":{}})).await.unwrap();assert!(absent["data"]["cause"].is_null());
    let cause=json!({"kind":"effect","id":"local","boot":"old","provenance":"asserted"});
    let both_args=json!({"cause":cause,"correlation":"matching"});
    let both=peer.call("resource.invoke",json!({"resource":resource,"op":"future.op","args":both_args,"cause":cause,"correlation":"matching"})).await.unwrap();
    assert_eq!(both["data"]["cause"],cause);assert_eq!(both["data"]["args"],both_args);
    for extra in [json!({"correlation":"root","args":{"correlation":"nested"}}),json!({"cause":cause,"args":{"cause":{"kind":"delivery","id":"other","provenance":"asserted"}}}),json!({"cause":{"kind":"request","id":"local","provenance":"asserted"}}),json!({"correlation":false})] {
        let mut p=json!({"resource":resource,"op":"future.op","args":{}});p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        assert_eq!(peer.call("resource.invoke",p).await.unwrap_err().kind(),"invalid");
    }
    assert_eq!(count.load(Ordering::SeqCst),4,"invalid causality must add no native ledger entry");
    let audits=core.lock().unwrap().events_since(0).unwrap().into_iter().filter(|event|event["type"]=="effect").collect::<Vec<_>>();
    assert_eq!(audits.len(),4);assert_eq!(audits[0]["data"]["cause"]["boot"],"prior-boot-A");assert_eq!(audits[1]["data"]["cause"]["boot"],"prior-boot-B");assert!(audits[2]["data"].get("cause").is_none());assert!(audits[2]["data"].get("correlation").is_none());
    server::shutdown(&core).await;drop(provider);worker.abort();let _=worker.await;
}
