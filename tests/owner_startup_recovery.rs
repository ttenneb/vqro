//! Invalid or removed legacy ownership is unresolved, not reset to a guessed default at restart.
use serde_json::json;
use vqro::{client,principal::Identity,server::{self,Backend}};
#[tokio::test]
async fn existing_missing_or_invalid_owner_is_preserved_until_explicit_repair() {
    for prior in [None,Some(json!(17)),Some(json!("")),Some(json!("system")),Some(json!("extension:managed"))] {
        let root=tempfile::Builder::new().prefix("own-st").tempdir().unwrap();let state=root.path().join("st");std::fs::create_dir(&state).unwrap();
        let mut entries=json!({"doc/existing":{"version":1,"value":true}});
        if let Some(value)=&prior {entries["doc/principals/owner"]=json!({"version":1,"value":value});}
        std::fs::write(state.join("workspace.json"),serde_json::to_vec(&json!({"rev":1,"next_txn":2,"entries":entries,"history":[]})).unwrap()).unwrap();
        let socket=root.path().join("s");let core=server::start(socket.clone(),state,false,Backend::Fake,None).unwrap();
        let(peer,_,hello)=client::connect_with_identity(&socket,"recovery",Some(Identity::Owner)).await.unwrap();
        assert_eq!(hello["actor"],"owner-unset","prior {prior:?} must not be guessed at startup");
        match prior {Some(value)=>assert_eq!(peer.call("workspace.get",json!({"key":"doc/principals/owner"})).await.unwrap()["value"],value),None=>assert!(peer.call("workspace.get",json!({"key":"doc/principals/owner"})).await.unwrap().is_null())};
        peer.call("workspace.owner.bootstrap",json!({"principal":"explicit-repair"})).await.unwrap();
        let(_,_,repaired)=client::connect_with_identity(&socket,"repaired",Some(Identity::Owner)).await.unwrap();assert_eq!(repaired["actor"],"explicit-repair");
        server::shutdown(&core).await;
    }
}
