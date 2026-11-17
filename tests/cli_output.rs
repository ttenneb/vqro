//! A closed stdout is normal CLI plumbing, not a panic. Only a private fake endpoint.
use std::{io::BufRead,process::Stdio};
use prim::rpc::{Incoming,Peer};
use serde_json::json;
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn status_exits_quietly_when_stdout_closes_after_first_line() {
    let dir=tempfile::Builder::new().prefix("po").tempdir_in("/tmp").unwrap();
    let socket=dir.path().join("s"); let listener=tokio::net::UnixListener::bind(&socket).unwrap();
    let (closed,wait_closed)=tokio::sync::oneshot::channel();
    let mock=tokio::spawn(async move {
        let (stream,_)=listener.accept().await.unwrap(); let(r,w)=stream.into_split();let(peer,mut incoming)=Peer::spawn(r,w);
        let Some(Incoming::Request{id,method,..})=incoming.recv().await else{panic!("hello")};assert_eq!(method,"hello");peer.respond(id,Ok(json!({"server":"private"})));
        let Some(Incoming::Request{id,method,..})=incoming.recv().await else{panic!("list")};assert_eq!(method,"client.list");
        wait_closed.await.unwrap();
        peer.respond(id,Ok(json!({"clients":[{"client":"other","author":"fixture","version":"1"}]})));
        peer.closed().await;
    });
    let mut child=std::process::Command::new(env!("CARGO_BIN_EXE_prim")).arg("status").env_clear().env("HOME",dir.path()).env("PRIM_SOCKET",socket)
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut reader=std::io::BufReader::new(child.stdout.take().unwrap());let mut line=String::new();reader.read_line(&mut line).unwrap();assert!(line.starts_with("running:"));
    drop(reader);closed.send(()).unwrap();
    let output=child.wait_with_output().unwrap();
    assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    assert!(output.stderr.is_empty(),"broken stdout must be quiet"); mock.await.unwrap();
}
