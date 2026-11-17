use prim::rpc::Peer;
use serde_json::{json,Value};
use tokio::io::{AsyncBufReadExt,AsyncReadExt};
use std::time::Duration;

#[tokio::test]
async fn control_flush_waits_for_the_actual_reply_without_emitting_a_protocol_record() {
    let (socket,remote)=tokio::io::duplex(1);
    let (r,w)=tokio::io::split(socket);let (peer,_incoming)=Peer::spawn(r,w);
    let (r,_w)=tokio::io::split(remote);let mut reader=tokio::io::BufReader::new(r);
    peer.respond(json!(1),Ok(json!({"text":"x".repeat(80)})));
    let mut flushed=tokio::spawn({let peer=peer.clone();async move {peer.flush_control().await}});
    assert!(tokio::time::timeout(Duration::from_millis(10),&mut flushed).await.is_err());
    let mut line=String::new();
    tokio::time::timeout(Duration::from_secs(1),reader.read_line(&mut line)).await.unwrap().unwrap();
    assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["id"],1);
    assert!(flushed.await.unwrap());
    assert!(tokio::time::timeout(Duration::from_millis(10),reader.read_u8()).await.is_err(),"local barrier must never emit blank JSONL");
    peer.close();
}
