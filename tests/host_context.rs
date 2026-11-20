//! G11: a runtime opts in to a tool-only, registration-bound host identity, never an endpoint guess.
use serde_json::json;
use vqro::{client,rpc::Incoming,server::{self,Backend}};

#[tokio::test]
async fn runtime_opt_in_supplies_verified_host_context_not_a_free_host_assertion() {
    let root=tempfile::Builder::new().prefix("hctx").tempdir_in("/tmp").unwrap();let socket=root.path().join("s");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();
    let(probe,_,_)=client::connect(&socket,"backend-opt-in-negative").await.unwrap();
    let backend_opt_in=probe.call("provider.register",json!({"name":"not-a-runtime","instance":"one","role":"backend","host_context":true,"software":{"name":"fixture","version":"1"},"provider_version":"1","capabilities":{}})).await;
    let(backend,mut requests,_)=client::connect(&socket,"native-host-fixture").await.unwrap();
    let g=backend.call("provider.register",json!({"name":"host-backend","instance":"stable","role":"backend","software":{"name":"fixture","version":"1"},"provider_version":"1","capabilities":{"terminal.spawn":{"env":true}}})).await.unwrap()["generation"].clone();
    backend.call("provider.sync",json!({"generation":g,"resources":[],"final":true})).await.unwrap();
    let(control,_,_)=client::connect(&socket,"control").await.unwrap();
    let spawn=tokio::spawn({let control=control.clone();async move {control.call("resource.spawn",json!({"provider":"host-backend","bridge":true})).await}});
    let Some(Incoming::Request{id,params,..})=requests.recv().await else{panic!("native spawn")};
    let bootstrap=params["args"]["env"]["VQRO_BRIDGE_TOKEN"].clone();
    backend.respond(id,Ok(json!({"outcome":"observed","data":{"facts":{"ref":"native-host","incarnation":"one","kind":"terminal","state":"live","argv":[],"data":{}}}})));
    let host=spawn.await.unwrap().unwrap()["resource"]["id"].clone();
    let(runtime,_,_)=client::connect(&socket,"runtime-fixture").await.unwrap();
    let reply=runtime.call("provider.register",json!({"name":"many-endpoints","instance":"stable","role":"runtime","token":bootstrap,"host_context":true,"software":{"name":"fixture","version":"1"},"provider_version":"1","capabilities":{}})).await.unwrap();
    let secret=reply["host_context"].as_str().map(String::from);
    let mut actors=Vec::new();
    if let Some(token)=&secret {
        for endpoint in ["endpoint-a","endpoint-b"] {
            let(tool,_,_)=client::connect(&socket,endpoint).await.unwrap();
            let hello=tool.call("hello",json!({"client":endpoint,"host_context":token})).await.unwrap();
            actors.push(hello["actor"].clone());
            tool.call("workspace.apply",json!({"ops":[{"op":"set","key":format!("doc/tool/{endpoint}"),"value":1}]})).await.unwrap();
        }
    }
    let(ordinary,_,ordinary_hello)=client::connect(&socket,"human-shell").await.unwrap();
    let raw=ordinary.call("hello",json!({"identity":{"kind":"principal","principal":format!("host:{}",host.as_str().unwrap())}})).await;
    let history=control.call("workspace.history",json!({"limit":10})).await.unwrap();
    server::shutdown(&core).await;backend.close();runtime.close();
    assert_eq!(backend_opt_in.unwrap_err().kind(),"invalid");
    assert!(secret.is_some(),"explicit runtime registration must return a verifiable host-context credential");
    assert_eq!(actors,vec![json!(format!("host:{}",host.as_str().unwrap()));2]);
    assert_eq!(ordinary_hello["actor"],"undeclared");assert!(raw.is_err(),"a claimed host id is never a credential");
    for txn in history["txns"].as_array().unwrap().iter().take(2){assert_eq!(txn["actor"],actors[0]);assert_eq!(txn["principal"],actors[0]);}
}
