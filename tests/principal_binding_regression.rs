//! A free per-call claim must not override the launcher's connection identity (AS6).
use serde_json::json;
use vqro::{rpc::Peer,server::{self,Backend}};
#[tokio::test]
async fn free_claim_does_not_replace_known_actor() {
    let root=tempfile::Builder::new().prefix("c2red").tempdir().unwrap();let socket=root.path().join("s");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();
    let stream=tokio::net::UnixStream::connect(socket).await.unwrap();let(r,w)=stream.into_split();let(peer,_)=Peer::spawn(r,w);
    peer.call("hello",json!({"identity":{"kind":"principal","principal":"alice"}})).await.unwrap();
    peer.call("workspace.apply",json!({"principal":"bob","label":"bob","ops":[{"op":"set","key":"doc/a","value":1}]})).await.unwrap();
    let history=peer.call("workspace.history",json!({})).await.unwrap();
    assert_eq!(history["txns"][0]["principal"],"alice","free declaration must not become effective principal");
    assert_eq!(history["txns"][0]["actor"],"alice");assert_eq!(history["txns"][0]["label"],"bob");
    server::shutdown(&core).await;
}
