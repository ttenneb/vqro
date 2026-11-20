use serde_json::{json,Value};
use vqro::{causality::Causality,client,server::{self,Backend}};
fn link()->Value {json!({"kind":"future","id":"same-local-id","provenance":"future","extra":{"keep":[1,true]}})}
#[test]
fn qualified_shapes_open_values_and_exact_utf8_bounds() {
    for cause in [link(),json!({"kind":"entity","id":"x","endpoint":"old-endpoint","epoch":"old-epoch","provenance":"native"}),json!({"kind":"request","id":"x","endpoint":"old-endpoint","epoch":"old-epoch","provenance":"adapter"}),json!({"kind":"effect","id":"x","boot":"prior-boot","provenance":"asserted"}),json!({"kind":"delivery","id":"global","provenance":"asserted"})] {
        let p=json!({"cause":cause,"correlation":"é".repeat(128)});let parsed=Causality::parse(&p).unwrap();
        let mut echoed=json!({});parsed.annotate(&mut echoed);assert_eq!(echoed,p);
    }
    let mut cause=link();cause["id"]=json!("é".repeat(128));assert!(Causality::parse(&json!({"cause":cause})).is_ok());
    cause["id"]=json!("é".repeat(129));assert_eq!(Causality::parse(&json!({"cause":cause})).unwrap_err().kind(),"invalid");
    for p in [json!({"cause":null}),json!({"cause":"x"}),json!({"cause":{"kind":"future","id":"x"}}),json!({"cause":{"kind":"entity","id":"x","provenance":"asserted"}}),json!({"cause":{"kind":"request","id":"x","endpoint":"r","provenance":"asserted"}}),json!({"cause":{"kind":"effect","id":"x","provenance":"asserted"}}),json!({"correlation":""}),json!({"correlation":null}),json!({"correlation":{"id":"x"}}),json!({"correlation":"x".repeat(257)})] {
        assert_eq!(Causality::parse(&p).unwrap_err().kind(),"invalid","{p}");
    }
    let mut empty=json!({});Causality::default().annotate(&mut empty);assert_eq!(empty,json!({}),"unknown stays absent");
}
#[test]
fn cause_encoded_bytes_and_container_depth_are_exact() {
    let mut cause=link();cause["padding"]=json!("");let base=serde_json::to_vec(&cause).unwrap().len();
    cause["padding"]=json!("x".repeat(4096-base));assert_eq!(serde_json::to_vec(&cause).unwrap().len(),4096);
    assert!(Causality::parse(&json!({"cause":cause})).is_ok());
    cause["padding"]=json!("x".repeat(4097-base));assert_eq!(Causality::parse(&json!({"cause":cause})).unwrap_err().kind(),"invalid");
    let mut nested=Value::Null;for _ in 0..63 {nested=json!([nested]);}
    let mut cause=link();cause["nested"]=nested;assert!(Causality::parse(&json!({"cause":cause})).is_ok());
    cause["nested"]=json!([cause["nested"]]);assert_eq!(Causality::parse(&json!({"cause":cause})).unwrap_err().kind(),"invalid");
}
#[tokio::test]
async fn workspace_audit_event_and_restart_preserve_explicit_links_without_backfill() {
    let root=tempfile::Builder::new().prefix("cause-w").tempdir().unwrap();let socket=root.path().join("s");let state=root.path().join("st");
    let mut core=server::start(socket.clone(),state.clone(),false,Backend::Fake,None).unwrap();
    let(peer,_,_)=client::connect_as(&socket,"causal-write","owner").await.unwrap();let cause=link();
    let p=json!({"cause":cause,"correlation":"write-thread","ops":[{"op":"set","key":"doc/causal","value":1}]});
    peer.call("workspace.apply",p).await.unwrap();
    let rows=peer.call("workspace.history",json!({})).await.unwrap();assert_eq!(rows["txns"][0]["cause"],cause);assert_eq!(rows["txns"][0]["correlation"],"write-thread");
    let event=core.lock().unwrap().events_since(0).unwrap().into_iter().rev().find(|event|event["type"]=="entries").unwrap();
    assert_eq!(event["data"]["cause"],cause);assert_eq!(event["data"]["correlation"],"write-thread");
    for bad in [json!({"kind":"effect","id":"local","provenance":"asserted"}),json!(false)] {
        assert_eq!(peer.call("workspace.apply",json!({"cause":bad,"ops":[{"op":"set","key":"doc/rejected","value":1}]})).await.unwrap_err().kind(),"invalid");
    }
    assert!(peer.call("workspace.get",json!({"key":"doc/rejected"})).await.unwrap().is_null());
    server::shutdown(&core).await;drop(peer);core=server::start(socket.clone(),state,false,Backend::Fake,None).unwrap();
    let(peer,_,_)=client::connect_as(&socket,"after-restart","owner").await.unwrap();
    let rows=peer.call("workspace.history",json!({})).await.unwrap();assert_eq!(rows["txns"][0]["cause"],cause);
    peer.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/no-inference","value":2}]})).await.unwrap();
    let rows=peer.call("workspace.history",json!({})).await.unwrap();assert!(rows["txns"][0].get("cause").is_none());assert!(rows["txns"][0].get("correlation").is_none());
    server::shutdown(&core).await;
}
