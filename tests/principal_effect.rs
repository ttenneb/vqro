#![cfg(unix)] // Unix-socket/signal fixtures; on Windows the lib's unit tests run (docs/windows.md).
//! Real Rust SDK and fake native ledger: attribution is core supplied, rejected delegation dispatches nothing.
use std::sync::{Arc,Mutex};
use serde_json::{json,Value};
use vqro::{client,provider_client::{Config,Provider,Request,Outcome},rpc::Peer,server::{self,Backend}};
#[tokio::test]
async fn native_ledger_separates_actual_actor_effective_initiator_and_labels() {
    let root=tempfile::Builder::new().prefix("c2fx").tempdir().unwrap();let socket=root.path().join("s");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();
    let config=Config::new("principal-probe","fixed",json!({"fixture.control":{}}).as_object().unwrap().clone());
    let(provider,mut requests)=Provider::connect(tokio::net::UnixStream::connect(&socket).await.unwrap(),config).await.unwrap();
    provider.sync(vec![json!({"ref":"term","incarnation":"one","kind":"terminal","state":"live","argv":[]})],true).await.unwrap();
    let ledger:Arc<Mutex<Vec<Value>>>=Arc::default();let captured=ledger.clone();
    let worker=tokio::spawn(async move {
        while let Some(request)=requests.recv().await {
            if let Request::Effect(effect)=request {
                captured.lock().unwrap().push(json!({"effect":effect.effect,"op":effect.op,"author":effect.author,"actor":effect.attribution.actor,"principal":effect.attribution.principal,"initiator":effect.attribution.initiator,"label":effect.attribution.label}));
                effect.respond(Ok(Outcome::Observed(Some(json!({"native":true})))));
            }
        }
    });
    let(alice,_,hello)=client::connect_as(&socket,"actor","alice").await.unwrap();
    let(owner,_,_)=client::connect_as(&socket,"operator","owner").await.unwrap();
    let resource=alice.call("resource.list",json!({"provider":"principal-probe"})).await.unwrap()["resources"][0]["id"].clone();
    let mut params=json!({"resource":resource,"op":"fixture.control","args":{},"label":"owner","actor":"owner","principal":"owner"});
    alice.call("resource.invoke",params.clone()).await.unwrap();
    assert_eq!(ledger.lock().unwrap()[0]["principal"],"alice");assert_eq!(ledger.lock().unwrap()[0]["actor"],"alice");assert_eq!(ledger.lock().unwrap()[0]["label"],"owner");assert_eq!(ledger.lock().unwrap()[0]["author"],hello["actual_author"]);
    params["on_behalf_of"]=json!("bob");
    assert_eq!(alice.call("resource.invoke",params.clone()).await.unwrap_err().kind(),"forbidden");assert_eq!(ledger.lock().unwrap().len(),1);
    owner.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/principals/delegations/b-a","value":{"from":"bob","to":"alice","until_ms":null}}]})).await.unwrap();
    alice.call("resource.invoke",params.clone()).await.unwrap();
    assert_eq!(ledger.lock().unwrap()[1]["actor"],"alice");assert_eq!(ledger.lock().unwrap()[1]["principal"],"bob");assert_eq!(ledger.lock().unwrap()[1]["initiator"],"alice");
    let(extension,mut callbacks,plugin_hello)=client::connect_as(&socket,"plugin","plugin-callback").await.unwrap();
    extension.call("command.register",json!({"name":"probe.effect"})).await.unwrap();
    let mut native=params.clone();native.as_object_mut().unwrap().remove("on_behalf_of");
    let callback=tokio::spawn(async move {
        let Some(vqro::rpc::Incoming::Request{id,params,..})=callbacks.recv().await else{panic!("callback")};
        assert_eq!(params["principal"],"bob");assert_eq!(params["initiator"],"alice");
        native["invocation"]=params["invocation"].clone();let result=extension.call("resource.invoke",native.clone()).await;
        extension.respond(id,result);
        native.as_object_mut().unwrap().remove("invocation");native["on_behalf_of"]=json!("bob");
        assert_eq!(extension.call("resource.invoke",native).await.unwrap_err().kind(),"forbidden","callback proof grants no standing delegation");
    });
    alice.call("command.invoke",json!({"name":"probe.effect","on_behalf_of":"bob"})).await.unwrap();callback.await.unwrap();
    assert_eq!(ledger.lock().unwrap()[2]["actor"],"plugin-callback");assert_eq!(ledger.lock().unwrap()[2]["principal"],"bob");assert_eq!(ledger.lock().unwrap()[2]["initiator"],"alice");assert_eq!(ledger.lock().unwrap()[2]["author"],plugin_hello["actual_author"]);
    owner.call("workspace.apply",json!({"ops":[{"op":"delete","key":"doc/principals/delegations/b-a"}]})).await.unwrap();
    assert_eq!(alice.call("resource.invoke",params.clone()).await.unwrap_err().kind(),"forbidden");assert_eq!(ledger.lock().unwrap().len(),3);
    let stream=tokio::net::UnixStream::connect(&socket).await.unwrap();let(r,w)=stream.into_split();let(unknown,_)=Peer::spawn(r,w);
    params.as_object_mut().unwrap().remove("on_behalf_of");unknown.call("resource.invoke",params).await.unwrap();
    assert_eq!(ledger.lock().unwrap()[3]["principal"],"undeclared","raw recovery remains ungated; no implicit owner");
    server::shutdown(&core).await;drop(provider);worker.abort();let _=worker.await;
}
