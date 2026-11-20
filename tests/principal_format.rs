//! Principal names use the pinned format-control guard too; an invalid legacy owner is unresolved, never guessed.
use serde_json::{json,Value};
use vqro::{client,principal::{self,Identity},server::{self,Backend},store::{Store,Op}};

#[test]
fn principal_names_refuse_every_pinned_format_character_without_normalizing_ordinary_names() {
    let vectors:Value=serde_json::from_str(include_str!("fixtures/text_controls.json")).unwrap();
    let mut count=0;
    for range in vectors["format_ranges"].as_array().unwrap() {
        for point in range[0].as_u64().unwrap()..=range[1].as_u64().unwrap() {
            let name=format!("own{}er",char::from_u32(point as u32).unwrap());count+=1;
            assert!(principal::validate(&name).is_err(),"format codepoint {point} in principal");
            assert!(principal::validate_assertable(&name).is_err());
        }
    }
    assert_eq!(count,170);
    for name in ["Alice","owner","systemd","hosting","é-owner","e\u{301}","name\u{fe0f}"] {assert!(principal::validate_assertable(name).is_ok(),"{name}");}
}

#[tokio::test]
async fn format_owner_is_unresolved_and_requires_explicit_human_repair() {
    let root=tempfile::Builder::new().prefix("owner-cf").tempdir().unwrap();
    let state=root.path().join("st");std::fs::create_dir(&state).unwrap();
    let legacy="own\u{200d}er";
    let mut store=Store::open(state.join("workspace.json")).unwrap();
    store.apply("legacy-fixture",None,vec![Op::Set{key:principal::OWNER_KEY.into(),value:json!(legacy),expect:None}]).unwrap();drop(store);
    let socket=root.path().join("s");let core=server::start(socket.clone(),state,false,Backend::Fake,None).unwrap();
    let(peer,_,hello)=client::connect_with_identity(&socket,"recovery",Some(Identity::Owner)).await.unwrap();
    let prior=peer.call("workspace.get",json!({"key":principal::OWNER_KEY})).await.unwrap()["value"].clone();
    let rejected=match client::connect_as(&socket,"bad-name",legacy).await {Err(_)=>true,Ok(_)=>false};
    let repaired=peer.call("workspace.owner.bootstrap",json!({"principal":"explicit-owner"})).await.is_ok();
    let(_,_,fresh)=client::connect_with_identity(&socket,"fresh-owner",Some(Identity::Owner)).await.unwrap();
    server::shutdown(&core).await;
    assert_eq!(prior,json!(legacy),"startup preserves invalid legacy data, no guessed reset");
    assert_eq!(hello["actor"],"owner-unset");assert_eq!(hello["owner_unset"],true);
    assert!(rejected,"raw principal assertions must refuse format characters too");
    assert!(repaired);assert_eq!(fresh["actor"],"explicit-owner");assert_eq!(fresh["owner_unset"],false);
}
