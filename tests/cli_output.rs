//! CLI plumbing and bounded diagnostics. Only private fake endpoints.
use std::{io::BufRead,process::Stdio};
use prim::rpc::{Incoming,Peer,RpcError};
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

#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn cli_reports_busy_once_without_retrying_requests() {
    for status in [true,false] {
        let dir=tempfile::Builder::new().prefix("pc").tempdir_in("/tmp").unwrap();
        let socket=dir.path().join("s");let listener=tokio::net::UnixListener::bind(&socket).unwrap();
        let mock=tokio::spawn(async move {
            let(stream,_)=listener.accept().await.unwrap();let(r,w)=stream.into_split();let(peer,mut incoming)=Peer::spawn(r,w);
            let Some(Incoming::Request{id,..})=incoming.recv().await else{panic!("hello")};peer.respond(id,Ok(json!({"server":"private"})));
            let Some(Incoming::Request{id,method,..})=incoming.recv().await else{panic!("request")};
            assert_eq!(method,if status {"client.list"}else{"workspace.snapshot"});
            peer.respond(id,Err(RpcError::new("overloaded","core busy")));
            assert!(incoming.recv().await.is_none(),"CLI must not silently repeat a refused request");
        });
        let mut command=tokio::process::Command::new(env!("CARGO_BIN_EXE_prim"));
        command.args(if status {vec!["status"]}else{vec!["call","workspace.snapshot"]})
            .env_clear().env("HOME",dir.path()).env("PRIM_SOCKET",socket).kill_on_drop(true);
        let output=tokio::time::timeout(std::time::Duration::from_secs(4),command.output()).await.expect("busy CLI must promptly report").unwrap();
        assert_eq!(output.status.success(),status);
        assert!(String::from_utf8_lossy(&output.stderr).contains("overloaded"),"a refused diagnostic must not disappear");
        mock.await.unwrap();
    }
}

#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn diagnostic_cli_requests_have_short_deadlines() {
    for (status,hello_only) in [(true,false),(false,false),(true,true),(false,true)] {
    let dir=tempfile::Builder::new().prefix("pc").tempdir_in("/tmp").unwrap();
    let socket=dir.path().join("s");let listener=tokio::net::UnixListener::bind(&socket).unwrap();
    let mock=tokio::spawn(async move {
        let(stream,_)=listener.accept().await.unwrap();let(r,w)=stream.into_split();let(peer,mut incoming)=Peer::spawn(r,w);
        let Some(Incoming::Request{id,..})=incoming.recv().await else{panic!("hello")};
        if !hello_only {
            peer.respond(id,Ok(json!({"server":"private"})));
            let Some(Incoming::Request{method,..})=incoming.recv().await else{panic!("request")};assert_eq!(method,if status {"client.list"}else{"workspace.snapshot"});
        }
        peer.closed().await;
    });
    let mut command=tokio::process::Command::new(env!("CARGO_BIN_EXE_prim"));
    command.args(if status {vec!["status"]}else{vec!["call","workspace.snapshot"]}).env_clear().env("HOME",dir.path()).env("PRIM_SOCKET",socket).kill_on_drop(true);
    let output=tokio::time::timeout(std::time::Duration::from_secs(8),command.output()).await.expect("CLI must have a short diagnostic deadline").unwrap();
    assert_eq!(output.status.success(),status);
    assert!(format!("{}{}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr)).contains("timeout"));
    if status&&hello_only {assert!(String::from_utf8_lossy(&output.stdout).contains("core status unavailable"));}
    mock.await.unwrap();
    }
}
