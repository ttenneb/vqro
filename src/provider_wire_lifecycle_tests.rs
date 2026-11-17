//! Deterministic scheduling cases for accepted provider tail records racing EOF.
use super::*;
use std::path::PathBuf;
use tokio::io::{duplex, split, DuplexStream};

fn core() -> Shared {
    Arc::new(Mutex::new(crate::core::Core::new(crate::store::Store::in_memory(), None, PathBuf::from("unused"), false).unwrap()))
}

async fn registered(core: &Shared, id: u64) -> (Connection, DuplexStream, u64) {
    let (transport, remote) = duplex(65536);
    let (reader, writer) = split(transport);
    let (peer, _incoming) = Peer::spawn(reader, writer);
    let connection = Connection::new(id, peer);
    connection.hello().unwrap();
    let result = connection.register(core, json!({"name":"tail-provider", "instance":"same-native-instance", "role":"backend",
        "software":{"name":"fixture","version":"1"}, "provider_version":"1", "capabilities":{"agent.abort":{}},
        "limits":{"max_streams":1,"stream_window_bytes":1024}}), None).await.unwrap();
    let generation = result["generation"].as_u64().unwrap();
    connection.sync(core, json!({"generation":generation,"resources":[{"ref":"agent","incarnation":"birth",
        "kind":"agent","state":"live","data":{"before":"exit"}}],"final":true})).unwrap();
    (connection, remote, generation)
}

fn gone(generation: u64) -> Value {
    json!({"generation":generation,"ref":"agent","incarnation":"birth","reason":{"reason":"exited","exit_code":23}})
}

#[tokio::test]
async fn accepted_gone_after_eof_disconnect_retires_current_resource() {
    let core = core();
    let (connection, _remote, generation) = registered(&core, 1).await;
    let wire = connection.provider.lock().unwrap().as_ref().unwrap().clone();
    // Model Peer EOF waking WireProvider::run before the application drains
    // its already accepted notification. Routes must remain unavailable.
    wire.disconnect(&core);
    assert_eq!(core.lock().unwrap().resources.len(), 1);
    assert!(!core.lock().unwrap().resources.values().next().unwrap().available);
    connection.notification(&core, "resource.gone", gone(generation)).unwrap();
    assert!(core.lock().unwrap().resources.is_empty());
}

#[tokio::test]
async fn old_gone_cannot_retire_a_replacement_even_after_prior_validation() {
    let core = core();
    let (old, _old_remote, old_generation) = registered(&core, 1).await;
    let validated = old.validate(&core, &json!({"generation":old_generation})).unwrap();
    validated.disconnect(&core);
    let (_new, _new_remote, _new_generation) = registered(&core, 2).await;
    // Model replacement between ingress validation and retirement publication.
    assert!(validated.notification(&core, "resource.gone", gone(old_generation)).is_err());
    assert_eq!(core.lock().unwrap().resources.len(), 1);
    assert!(core.lock().unwrap().resources.values().next().unwrap().available);
    assert!(old.notification(&core, "resource.gone", gone(old_generation)).is_err());
}

#[tokio::test]
async fn eof_does_not_allow_upserts_to_revive_availability() {
    let core = core();
    let (connection, _remote, generation) = registered(&core, 1).await;
    connection.provider.lock().unwrap().as_ref().unwrap().disconnect(&core);
    assert!(connection.notification(&core, "resource.upsert", json!({"generation":generation,"facts":{
        "ref":"agent","incarnation":"birth","kind":"agent","state":"live","data":{"after":"EOF"}}})).is_err());
    assert!(!core.lock().unwrap().resources.values().next().unwrap().available);
}
