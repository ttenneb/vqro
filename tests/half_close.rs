#![cfg(unix)] // Unix-socket/signal fixtures; on Windows the lib's unit tests run (docs/windows.md).
//! Read EOF is a half-close, not permission to throw away replies to accepted work.
//! Private fake backend/state only; no native programs or real sessions.
use std::{sync::{Arc,Mutex,Condvar},time::Duration};
use vqro::{server::{self,Backend},store};
use serde_json::{json,Value};
use tokio::{io::{AsyncBufReadExt,AsyncWriteExt,BufReader},net::UnixStream};

#[tokio::test(flavor="multi_thread",worker_threads=4)]
async fn half_closed_pipeline_gets_every_reply_and_mutation_exactly_once(){
    let root=tempfile::Builder::new().prefix("phalf").tempdir_in("/tmp").unwrap();let socket=root.path().join("s");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();
    let initial=store::Store::open(root.path().join("st/workspace.json")).unwrap().history(500).len();
    for batch in 0..20 {
        let stream=UnixStream::connect(&socket).await.unwrap();let(r,mut w)=stream.into_split();let mut r=BufReader::new(r);
        let mut wire=String::new();
        for i in 0..5 {wire+=&format!("{}\n",json!({"jsonrpc":"2.0","id":i,"method":"workspace.apply","params":{"ops":[{"op":"set","key":format!("doc/half-{batch}-{i}"),"value":i,"expect":0}]}}));}
        w.write_all(wire.as_bytes()).await.unwrap();w.shutdown().await.unwrap();
        let mut replies=std::collections::BTreeSet::new();
        loop {
            let mut line=String::new();let n=tokio::time::timeout(Duration::from_secs(3),r.read_line(&mut line)).await.unwrap().unwrap();
            if n==0 {break;}
            let reply:Value=serde_json::from_str(&line).unwrap();assert!(reply.get("error").is_none(),"{reply}");assert!(replies.insert(reply["id"].as_u64().unwrap()),"duplicate reply");
        }
        assert_eq!(replies.len(),5,"a half-closed client still reads every accepted reply");
    }
    assert_eq!(store::Store::open(root.path().join("st/workspace.json")).unwrap().history(500).len(),initial+100);
    server::shutdown(&core).await;
}

struct Release(Arc<(Mutex<bool>,Condvar)>);
impl Drop for Release {fn drop(&mut self){*self.0.0.lock().unwrap()=true;self.0.1.notify_all();}}
#[tokio::test(flavor="multi_thread",worker_threads=4)]
async fn half_close_during_an_admitted_durable_write_preserves_its_reply(){
    let root=tempfile::Builder::new().prefix("phalf").tempdir_in("/tmp").unwrap();let socket=root.path().join("s");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();
    let initial=store::Store::open(root.path().join("st/workspace.json")).unwrap().history(1)[0].id;
    let gate=Arc::new((Mutex::new(false),Condvar::new()));let release=Release(gate.clone());let entered=Arc::new(tokio::sync::Notify::new());
    core.lock().unwrap().configure_workspace_writer(Arc::new({let gate=gate.clone();let entered=entered.clone();move|path,bytes|{
        entered.notify_one();let mut open=gate.0.lock().unwrap();while !*open{open=gate.1.wait(open).unwrap();}store::write_atomic(path,bytes)
    }})).unwrap();
    let stream=UnixStream::connect(&socket).await.unwrap();let(r,mut w)=stream.into_split();let mut r=BufReader::new(r);
    w.write_all(format!("{}\n",json!({"jsonrpc":"2.0","id":7,"method":"workspace.apply","params":{"ops":[{"op":"set","key":"doc/admitted","value":"once"}]}})).as_bytes()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2),entered.notified()).await.unwrap();w.shutdown().await.unwrap();
    // Let the reader observe EOF before the already-admitted candidate can finish.
    tokio::time::sleep(Duration::from_millis(30)).await;drop(release);
    let mut line=String::new();let n=tokio::time::timeout(Duration::from_secs(3),r.read_line(&mut line)).await.unwrap().unwrap();
    assert!(n>0,"durable adoption must not lose its reply merely because request input ended");
    let reply:Value=serde_json::from_str(&line).unwrap();assert_eq!(reply["id"],7);assert_eq!(reply["result"]["txn"],initial+1);
    assert_eq!(store::Store::open(root.path().join("st/workspace.json")).unwrap().history(10).len(),2);
    server::shutdown(&core).await;
}
