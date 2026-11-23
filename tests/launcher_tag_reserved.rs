#![cfg(unix)] // Unix-socket/signal fixtures; on Windows the lib's unit tests run (docs/windows.md).
//! A raw explicit native environment is not evidence of a reserved core principal.
use serde_json::{json, Value};
use vqro::{client,provider_client::{Config,Provider,Request,Outcome},server::{self,Backend}};

#[tokio::test]
async fn explicit_reserved_tags_are_never_recorded_as_known_launcher_principals() {
    let root=tempfile::Builder::new().prefix("tag-reserved").tempdir().unwrap();
    let socket=root.path().join("s");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();
    let config=Config::new("tag-probe","stable",json!({"terminal.spawn":{"env":true}}).as_object().unwrap().clone());
    let(provider,mut requests)=Provider::connect(tokio::net::UnixStream::connect(&socket).await.unwrap(),config).await.unwrap();
    provider.sync(vec![],true).await.unwrap();
    let worker=tokio::spawn(async move {
        let mut n=0;
        while let Some(Request::Effect(effect))=requests.recv().await {
            n+=1;
            let tag=effect.args["env"]["VQRO_PRINCIPAL"].clone();
            // Returning the exact native assignment still cannot certify a reserved identity.
            effect.respond(Ok(Outcome::Observed(Some(json!({"facts":{"ref":format!("t{n}"),"incarnation":"one","kind":"terminal","state":"live","argv":[],"data":{"native_tag":tag}}})))));
        }
    });
    let(alice,_,_)=client::connect_as(&socket,"alice","alice").await.unwrap();
    let cases=[("system",false),("SyStEm",false),("system:startup",false),("SYSTEM:ROOT",false),("extension:worker",false),("ExTeNsIoN:worker",false),("host:r1",false),("HOST:r1",false),("owner-unset",false),("OWNER-UNSET",false),("Undeclared",false),("alice",true),("Alice",true),("systemd",true),("hosting",true),("é-owner",true),("undeclared",true)];
    let mut observed=Vec::new();
    for (tag,known) in cases {
        let resource=alice.call("resource.spawn",json!({"provider":"tag-probe","env":{"VQRO_PRINCIPAL":tag}})).await.unwrap()["resource"].clone();
        observed.push((tag,known,resource["principal"].clone(),resource["data"]["native_tag"].clone()));
    }
    server::shutdown(&core).await;drop(provider);worker.abort();let _=worker.await;
    for (tag,known,principal,native_tag) in observed {
        assert_eq!(native_tag,json!(tag),"explicit native input stays opaque, not rewritten");
        assert_eq!(principal,if known{json!(tag)}else{Value::Null},"{tag}: only assertable identifiers or exact undeclared sentinel are recorded");
    }
}
