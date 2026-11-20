//! AS6 connection defaults, diagnostic labels and exact cooperative delegation; no security claim.
use std::path::PathBuf;
use vqro::{client,core::Shared,rpc::{Incoming,Peer},server::{self,Backend}};
use serde_json::{json,Value};
struct Env {_root:tempfile::TempDir,socket:PathBuf,state:PathBuf,core:Shared}
impl Env {
    fn new()->Self {
        let root=tempfile::Builder::new().prefix("c2prin").tempdir().unwrap();
        let socket=root.path().join("s");let state=root.path().join("st");
        let core=server::start(socket.clone(),state.clone(),false,Backend::Fake,None).unwrap();
        Self{_root:root,socket,state,core}
    }
    async fn known(&self,id:&str)->Peer {client::connect_as(&self.socket,"principal-probe",id).await.unwrap().0}
    async fn raw(&self)->Peer {
        let stream=tokio::net::UnixStream::connect(&self.socket).await.unwrap();let(r,w)=stream.into_split();
        let(peer,_)=Peer::spawn(r,w);peer.call("hello",json!({"client":"owner"})).await.unwrap();peer
    }
}
async fn put(peer:&Peer,key:&str,value:Value)->Value {peer.call("workspace.apply",json!({"ops":[{"op":"set","key":key,"value":value}]})).await.unwrap()}
async fn history(peer:&Peer)->Vec<Value> {peer.call("workspace.history",json!({"limit":100})).await.unwrap()["txns"].as_array().unwrap().clone()}
async fn get(peer:&Peer,key:&str)->Value {peer.call("workspace.get",json!({"key":key})).await.unwrap()}
async fn undo(peer:&Peer)->vqro::rpc::RpcResult {peer.call("command.invoke",json!({"name":"vqro.revert"})).await}
async fn grant(owner:&Peer,id:&str,from:&str,to:&str,until:Option<u64>) {put(owner,&format!("doc/principals/delegations/{id}"),json!({"from":from,"to":to,"until_ms":until})).await;}

#[tokio::test]
async fn labels_cannot_rebind_connection_or_select_somebody_elses_undo() {
    let e=Env::new();let alice=e.known("alice").await;let bob=e.known("bob").await;
    let a=put(&alice.clone().with_label("bob"),"doc/alice",json!(1)).await;
    put(&bob.clone().with_label("alice"),"doc/bob",json!(2)).await;
    let rows=history(&alice).await;
    assert_eq!(rows[0]["principal"],"bob");assert_eq!(rows[0]["actor"],"bob");assert_eq!(rows[0]["label"],"alice");
    assert_eq!(rows[1]["principal"],"alice");assert_eq!(rows[1]["label"],"bob");
    assert!(rows[1]["author"].as_str().unwrap().contains(e.core.lock().unwrap().epoch.as_str()));
    assert_ne!(rows[0]["author"],rows[1]["author"]);
    undo(&alice.clone().with_label("bob")).await.unwrap();
    assert_eq!(history(&alice).await[0]["reverts"],a["txn"]);
    assert!(get(&alice,"doc/alice").await.is_null());assert_eq!(get(&alice,"doc/bob").await["value"],2);
    assert_eq!(alice.call("hello",json!({"identity":{"kind":"principal","principal":"bob"}})).await.unwrap_err().kind(),"conflict");
    server::shutdown(&e.core).await;
}
#[tokio::test]
async fn core_derived_names_cannot_be_asserted_at_hello_and_rejections_are_recorded() {
    let e=Env::new();
    for id in ["extension:plugin","host:native","system","system:future"] {
        let error=client::connect_as(&e.socket,"reserved-principal-probe",id).await.err().expect("caller cannot assert a core-derived identity");
        assert_eq!(error.downcast_ref::<vqro::rpc::RpcError>().unwrap().kind(),"invalid");
    }
    let recorded=e.core.lock().unwrap().events_since(0).unwrap().into_iter().filter(|event|event["type"]=="identity.rejected").collect::<Vec<_>>();
    assert_eq!(recorded.len(),4);assert!(recorded.iter().all(|event|event["data"]["outcome"]=="invalid" && event["data"]["actor"]=="undeclared"));
    assert!(recorded.iter().all(|event|event["data"].get("identity").is_none()));
    assert!(client::connect_as(&e.socket,"ordinary","systematic").await.is_ok());
    server::shutdown(&e.core).await;
}
#[tokio::test]
async fn raw_labels_and_spoofed_record_fields_never_become_owner() {
    let e=Env::new();let unknown=e.raw().await;
    let hello=unknown.call("hello",json!({"client":"owner","label":"owner"})).await.unwrap();assert_eq!(hello["actor"],"undeclared");
    unknown.call("workspace.apply",json!({"principal":"owner","actor":"owner","initiator":"owner","author":"ext:trusted","label":"owner","ops":[{"op":"set","key":"doc/raw","value":1}]})).await.unwrap();
    let rows=history(&unknown).await;assert_eq!(rows[0]["principal"],"undeclared");assert_eq!(rows[0]["actor"],"undeclared");assert_eq!(rows[0]["label"],"owner");
    assert_ne!(rows[0]["author"],"ext:trusted");assert!(rows[0]["initiator"].is_null());assert_eq!(undo(&unknown).await.unwrap_err().kind(),"invalid");
    unknown.call("workspace.revert",json!({"txn":rows[0]["id"]})).await.unwrap();assert!(unknown.call("workspace.snapshot",json!({})).await.is_ok());
    assert_eq!(unknown.call("workspace.apply",json!({"label":"owner","ops":[{"op":"set","key":"doc/principals/owner","value":"mallory"}]})).await.unwrap_err().kind(),"forbidden");
    assert_eq!(unknown.call("grants.check",json!({"right":"anything","scope":"*"})).await.unwrap_err().kind(),"unknown_method");
    server::shutdown(&e.core).await;
}
#[tokio::test]
async fn system_bootstrap_is_audited_and_not_an_owner_u_target() {
    let e=Env::new();let owner=e.known("owner").await;
    assert_eq!(get(&owner,"doc/principals/owner").await["value"],"owner");
    let rows=history(&owner).await;assert_eq!(rows.len(),1);assert_eq!(rows[0]["principal"],"system");assert!(rows[0]["author"].as_str().unwrap().starts_with("system:"));
    assert_eq!(undo(&owner).await.unwrap_err().kind(),"not_found");
    let changed=put(&owner,"doc/principals/owner",json!("alice")).await;
    let(p,_,hello)=client::connect_with_identity(&e.socket,"tui",Some(vqro::principal::Identity::Owner)).await.unwrap();
    assert_eq!(hello["actor"],"alice");assert_eq!(owner.call("hello",json!({})).await.unwrap()["actor"],"owner");
    p.call("workspace.revert",json!({"txn":changed["txn"]})).await.unwrap();assert_eq!(get(&p,"doc/principals/owner").await["value"],"owner");
    server::shutdown(&e.core).await;
}
#[tokio::test]
async fn exact_delegation_records_actor_initiator_effective_and_scopes_u() {
    let e=Env::new();let owner=e.known("owner").await;let alice=e.known("alice").await;let bob=e.known("bob").await;
    grant(&owner,"b-a","bob","alice",None).await;
    let changed=alice.call("workspace.apply",json!({"on_behalf_of":"bob","label":"carol","ops":[{"op":"set","key":"doc/delegated","value":1}]})).await.unwrap();
    let rows=history(&alice).await;assert_eq!(rows[0]["actor"],"alice");assert_eq!(rows[0]["initiator"],"alice");assert_eq!(rows[0]["principal"],"bob");assert_eq!(rows[0]["label"],"carol");
    assert_eq!(undo(&alice).await.unwrap_err().kind(),"not_found");undo(&bob).await.unwrap();
    assert_eq!(history(&bob).await[0]["reverts"],changed["txn"]);assert!(get(&bob,"doc/delegated").await.is_null());server::shutdown(&e.core).await;
}
#[tokio::test]
async fn delegation_is_explicit_exact_unexpired_and_never_chained() {
    let e=Env::new();let owner=e.known("owner").await;let alice=e.known("alice").await;
    grant(&owner,"b-a","bob","alice",None).await;grant(&owner,"c-b","carol","bob",None).await;
    assert!(alice.call("workspace.get",json!({"key":"doc/x","on_behalf_of":"bob"})).await.is_ok());
    for target in ["carol","owner","unknown"] {assert_eq!(alice.call("workspace.get",json!({"key":"doc/x","on_behalf_of":target})).await.unwrap_err().kind(),"forbidden");}
    grant(&owner,"b-a","bob","alice",Some(vqro::store::now_ms().saturating_sub(1))).await;
    assert_eq!(alice.call("workspace.get",json!({"key":"doc/x","on_behalf_of":"bob"})).await.unwrap_err().kind(),"forbidden");
    assert!(alice.call("workspace.get",json!({"key":"doc/x"})).await.is_ok());assert_eq!(history(&alice).await[0]["principal"],"owner");server::shutdown(&e.core).await;
}
async fn delegate_cannot_change_principal_configuration(case:usize) {
    let e=Env::new();let owner=e.known("owner").await;let alice=e.known("alice").await;
    grant(&owner,"b-a","bob","alice",Some(vqro::store::now_ms()+60_000)).await;
    grant(&owner,"o-a","owner","alice",None).await;
    let(target,key,value)=match case {
        0=>("bob","doc/principals/delegations/b-c",json!({"from":"bob","to":"carol"})),
        1=>("bob","doc/principals/delegations/b-a",json!({"from":"bob","to":"alice"})),
        _=>("owner","doc/principals/owner",json!("alice")),
    };
    let before=get(&owner,key).await;let count=history(&owner).await.len();
    let result=alice.call("workspace.apply",json!({"on_behalf_of":target,"ops":[{"op":"set","key":key,"value":value}]})).await;
    assert_eq!(result.unwrap_err().kind(),"forbidden","principal configuration is governed by the connection actor, never its represented principal");
    assert_eq!(get(&owner,key).await,before);assert_eq!(history(&owner).await.len(),count);
    server::shutdown(&e.core).await;
}
#[tokio::test]
async fn delegate_cannot_mint_a_chain_as_its_grantor(){delegate_cannot_change_principal_configuration(0).await}
#[tokio::test]
async fn delegate_cannot_remove_its_expiry_as_its_grantor(){delegate_cannot_change_principal_configuration(1).await}
#[tokio::test]
async fn owner_delegate_cannot_take_over_workspace_owner(){delegate_cannot_change_principal_configuration(2).await}
#[tokio::test]
async fn delegation_replacement_requires_existing_and_new_grantor_and_guards_revert_delete() {
    let e=Env::new();let owner=e.known("owner").await;let alice=e.known("alice").await;let bob=e.known("bob").await;
    grant(&owner,"a-b","alice","bob",None).await;
    let original=put(&alice,"doc/principals/delegations/a-b",json!({"from":"alice","to":"carol"})).await;
    for op in [json!({"op":"delete","key":"doc/principals/delegations/a-b"}),json!({"op":"set","key":"doc/principals/delegations/a-b","value":{"from":"bob","to":"bob"}})] {
        assert_eq!(bob.call("workspace.apply",json!({"ops":[op]})).await.unwrap_err().kind(),"forbidden");
    }
    assert_eq!(alice.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/principals/delegations/a-b","value":{"from":"bob","to":"carol"}}]})).await.unwrap_err().kind(),"forbidden");
    assert_eq!(bob.call("workspace.revert",json!({"txn":original["txn"]})).await.unwrap_err().kind(),"forbidden");
    alice.call("workspace.apply",json!({"ops":[{"op":"delete","key":"doc/principals/delegations/a-b"}]})).await.unwrap();assert!(get(&alice,"doc/principals/delegations/a-b").await.is_null());server::shutdown(&e.core).await;
}
#[tokio::test]
async fn protected_config_mixed_transaction_is_all_or_nothing() {
    let e=Env::new();let alice=e.known("alice").await;
    assert_eq!(alice.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/ordinary","value":1},{"op":"set","key":"doc/principals/owner","value":"alice"}]})).await.unwrap_err().kind(),"forbidden");
    assert!(get(&alice,"doc/ordinary").await.is_null());server::shutdown(&e.core).await;
}
#[tokio::test]
async fn identifiers_labels_and_delegation_shapes_are_bounded_before_any_write() {
    let e=Env::new();let owner=e.known("owner").await;
    for label in [json!(null),json!(true),json!("x".repeat(257)),json!("bad\nlabel")] {
        assert_eq!(owner.call("workspace.apply",json!({"label":label,"ops":[{"op":"set","key":"doc/bad","value":1}]})).await.unwrap_err().kind(),"invalid");
    }
    for value in [json!({"from":"a","to":"b","until_ms":"tomorrow"}),json!({"from":"a","to":"b","until_ms":-1}),json!({"from":"a","to":"b","deny":true}),json!({"from":"a","to":"x".repeat(257)})] {
        assert_eq!(owner.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/principals/delegations/bad","value":value}]})).await.unwrap_err().kind(),"invalid");
    }
    assert!(get(&owner,"doc/bad").await.is_null());assert!(get(&owner,"doc/principals/delegations/bad").await.is_null());server::shutdown(&e.core).await;
}
#[tokio::test]
async fn delegation_table_bounds_charge_encoded_bytes_and_reject_atomically() {
    let e=Env::new();let owner=e.known("owner").await;
    for(count,note,kind)in[(1025,String::new(),"overloaded"),(24,"\0".repeat(1000),"too_large")] {
        let ops=(0..count).map(|i|json!({"op":"set","key":format!("doc/principals/delegations/g{i}"),"value":{"from":"alice","to":"bob","note":note}})).collect::<Vec<_>>();
        assert_eq!(owner.call("workspace.apply",json!({"ops":ops})).await.unwrap_err().kind(),kind);
        assert!(get(&owner,"doc/principals/delegations/g0").await.is_null());assert_eq!(history(&owner).await.len(),1);
    }
    put(&owner,"doc/raw-recovery",json!(true)).await;server::shutdown(&e.core).await;
}

#[tokio::test]
async fn explicit_other_principal_revert_and_scoped_u_keep_cas_protection() {
    let e=Env::new();let alice=e.known("alice").await;let bob=e.known("bob").await;
    let a=put(&alice,"doc/a",json!(1)).await;bob.call("workspace.revert",json!({"txn":a["txn"]})).await.unwrap();assert_eq!(history(&bob).await[0]["principal"],"bob");
    put(&alice,"doc/same",json!(1)).await;put(&bob,"doc/same",json!(2)).await;assert_eq!(undo(&alice).await.unwrap_err().kind(),"conflict");assert_eq!(get(&alice,"doc/same").await["value"],2);server::shutdown(&e.core).await;
}
#[tokio::test]
async fn legacy_free_declarations_are_not_backfilled_into_known_owner_history() {
    let root=tempfile::Builder::new().prefix("c2old").tempdir().unwrap();std::fs::create_dir(root.path().join("st")).unwrap();
    let mut store=vqro::store::Store::open(root.path().join("st/workspace.json")).unwrap();
    let old=store.apply_context("client:old",None,serde_json::from_value(json!({"principal":"owner","actual_author":"client:old:1"})).unwrap(),None,vec![vqro::store::Op::Set{key:"doc/legacy".into(),value:json!(1),expect:None}]).unwrap();
    let socket=root.path().join("s");let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();let(owner,_,_)=client::connect_as(&socket,"owner","owner").await.unwrap();
    assert_eq!(undo(&owner).await.unwrap_err().kind(),"not_found");assert_eq!(get(&owner,"doc/legacy").await["value"],1);
    owner.call("workspace.revert",json!({"txn":old.id})).await.unwrap();assert!(get(&owner,"doc/legacy").await.is_null());server::shutdown(&core).await;
}

#[tokio::test]
async fn bound_principal_survives_restart_but_actual_connection_does_not() {
    let mut e=Env::new();let alice=e.known("alice").await;let written=put(&alice,"doc/a",json!(1)).await;
    let old=history(&alice).await[0]["author"].clone();server::shutdown(&e.core).await;drop(alice);
    e.core=server::start(e.socket.clone(),e.state.clone(),false,Backend::Fake,None).unwrap();
    let alice=e.known("alice").await;undo(&alice).await.unwrap();let rows=history(&alice).await;
    assert_eq!(rows[0]["principal"],"alice");assert_ne!(rows[0]["author"],old);assert_eq!(rows[0]["reverts"],written["txn"]);assert_eq!(rows.iter().filter(|row|row["principal"]=="system").count(),1);server::shutdown(&e.core).await;
}
#[tokio::test]
async fn delegated_callback_carries_effective_and_original_initiator_separately() {
    let e=Env::new();let owner=e.known("owner").await;let alice=e.known("alice").await;let bob=e.known("bob").await;grant(&owner,"b-a","bob","alice",None).await;
    let stream=tokio::net::UnixStream::connect(&e.socket).await.unwrap();let(r,w)=stream.into_split();let(extension,mut incoming)=Peer::spawn(r,w);
    extension.call("hello",json!({"identity":{"kind":"principal","principal":"callback-worker"}})).await.unwrap();extension.call("command.register",json!({"name":"worker.run"})).await.unwrap();
    let caller=alice.clone();let invoked=tokio::spawn(async move{caller.call("command.invoke",json!({"name":"worker.run","on_behalf_of":"bob"})).await});
    let(id,p)=loop{match incoming.recv().await.unwrap(){Incoming::Request{id,method,params}if method=="command.invoke"=>break(id,params),_=>{}}};
    assert_eq!(p["principal"],"bob");assert_eq!(p["initiator"],"alice");assert_eq!(p["actor"],"alice");
    let changed=extension.call("workspace.apply",json!({"invocation":p["invocation"],"ops":[{"op":"set","key":"doc/callback","value":1}]})).await.unwrap();
    let rows=history(&alice).await;assert_eq!(rows[0]["principal"],"bob");assert_eq!(rows[0]["initiator"],"alice");assert_eq!(rows[0]["actor"],"callback-worker");
    assert!(!serde_json::to_string(&rows).unwrap().contains(p["invocation"].as_str().unwrap()),"opaque proof is never retained in history");
    extension.respond(id,Ok(json!({})));invoked.await.unwrap().unwrap();
    assert_eq!(extension.call("workspace.apply",json!({"invocation":p["invocation"],"ops":[{"op":"set","key":"doc/late","value":1}]})).await.unwrap_err().kind(),"forbidden");
    assert_eq!(undo(&alice).await.unwrap_err().kind(),"not_found");undo(&bob).await.unwrap();assert_eq!(history(&bob).await[0]["reverts"],changed["txn"]);server::shutdown(&e.core).await;
}
