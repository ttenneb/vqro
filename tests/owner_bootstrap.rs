//! Owner-unset is recovery, never a guessed principal or an implicit coordination grant.
use serde_json::json;
use vqro::{client,principal::{self,Identity},server::{self,Backend},store::{Store,Op}};
#[test]
fn missing_or_invalid_owner_identity_is_unset_not_guessed() {
    let root=tempfile::tempdir().unwrap();let mut store=Store::open(root.path().join("workspace.json")).unwrap();
    for value in [None,Some(json!(17)),Some(json!(""))] {
        if let Some(value)=value {store.apply("fixture",None,vec![Op::Set{key:principal::OWNER_KEY.into(),value,expect:None}]).unwrap();}
        assert_eq!(principal::identity(&store,Some(&Identity::Owner),None).unwrap(),"owner-unset");
        let context=principal::Attribution::for_actor("owner-unset",&mut json!({})).unwrap();
        assert!(context.actor().is_none());assert!(context.effective().is_none(),"an unset-owner UI receives no effective known principal");
    }
}
#[tokio::test]
async fn explicit_bootstrap_is_guarded_serialized_audited_and_outside_owner_undo() {
    let root=tempfile::Builder::new().prefix("own-b").tempdir().unwrap();let socket=root.path().join("s");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();
    let(owner,mut callbacks,_)=client::connect_as(&socket,"owner","owner").await.unwrap();
    assert_eq!(owner.call("workspace.owner.bootstrap",json!({"principal":"mallory"})).await.unwrap_err().kind(),"conflict");
    owner.call("workspace.apply",json!({"ops":[{"op":"delete","key":principal::OWNER_KEY}]})).await.unwrap();
    let(unset,_,info)=client::connect_with_identity(&socket,"unset-tui",Some(Identity::Owner)).await.unwrap();
    assert_eq!(info["actor"],"owner-unset");assert_eq!(info["owner_unset"],true);assert!(info["owner_notice"].as_str().unwrap().contains("bootstrap"));
    assert_eq!(unset.call("command.invoke",json!({"name":"vqro.revert"})).await.unwrap_err().kind(),"invalid");
    owner.call("command.register",json!({"name":"fixture.check-unset"})).await.unwrap();
    let invoked=unset.clone();let invoked=tokio::spawn(async move{invoked.call("command.invoke",json!({"name":"fixture.check-unset"})).await});
    let(id,p)=loop{if let vqro::rpc::Incoming::Request{id,method,params}=callbacks.recv().await.unwrap(){if method=="command.invoke"{break(id,params)}}};
    assert_eq!(p["actor"],"owner-unset");assert_eq!(p["principal"],"undeclared","coordination callbacks receive no known effective principal");
    owner.respond(id,Ok(json!({})));invoked.await.unwrap().unwrap();
    // Raw recovery remains usable, including while the TUI has no coordination identity.
    unset.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/raw-recovery","value":1}]})).await.unwrap();
    assert_eq!(unset.call("workspace.apply",json!({"ops":[{"op":"set","key":principal::OWNER_KEY,"value":"guessed"}]})).await.unwrap_err().kind(),"forbidden");
    let(raw,_,raw_info)=client::connect(&socket,"bootstrap-requester").await.unwrap();
    for principal in [json!(""),json!(false),json!("owner-unset")] {
        assert_eq!(raw.call("workspace.owner.bootstrap",json!({"principal":principal})).await.unwrap_err().kind(),"invalid");
    }
    let before=raw.call("workspace.history",json!({"limit":100})).await.unwrap()["txns"].as_array().unwrap().len();
    let a=raw.clone();let b=raw.clone();let cause=json!({"kind":"delivery","id":"explicit-bootstrap","provenance":"asserted"});
    let(a,b)=tokio::join!(a.call("workspace.owner.bootstrap",json!({"principal":"alice","cause":cause,"correlation":"bootstrap"})),b.call("workspace.owner.bootstrap",json!({"principal":"bob","cause":cause,"correlation":"bootstrap"})));
    assert_ne!(a.is_ok(),b.is_ok(),"the serialized prepare guard permits exactly one initializer");
    let denied=if a.is_ok(){b.unwrap_err()}else{a.unwrap_err()};assert_eq!(denied.kind(),"conflict");
    let rows=raw.call("workspace.history",json!({"limit":100})).await.unwrap();let txns=rows["txns"].as_array().unwrap();assert_eq!(txns.len(),before+1);
    let audit=&txns[0];assert_eq!(audit["actor"],"system");assert_eq!(audit["principal"],"system");assert!(audit["author"].as_str().unwrap().starts_with("system:"));
    assert!(audit["note"].as_str().unwrap().contains(raw_info["actual_author"].as_str().unwrap()));
    assert_eq!(audit["cause"],cause);assert_eq!(audit["correlation"],"bootstrap");
    let(_,_,bound)=client::connect_with_identity(&socket,"new-owner",Some(Identity::Owner)).await.unwrap();assert!(bound["actor"]=="alice"||bound["actor"]=="bob");
    let(new_owner,_,_)=client::connect_with_identity(&socket,"new-owner-undo",Some(Identity::Owner)).await.unwrap();
    assert_eq!(new_owner.call("command.invoke",json!({"name":"vqro.revert"})).await.unwrap_err().kind(),"not_found");
    assert_eq!(unset.call("hello",json!({"identity":{"kind":"owner"}})).await.unwrap_err().kind(),"conflict","a repair never retags an existing connection");
    assert_eq!(raw.call("workspace.owner.bootstrap",json!({"principal":"carol"})).await.unwrap_err().kind(),"conflict");
    server::shutdown(&core).await;
}
