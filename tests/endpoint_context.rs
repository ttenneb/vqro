#![cfg(unix)]
use std::{process::Command,time::Duration};
use vqro::rpc::{Peer,Incoming};use serde_json::json;
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn scratch_state_ignores_terminal_endpoint_but_honours_unmarked_explicit_endpoint(){
 let root=tempfile::Builder::new().prefix("pec").tempdir_in("/tmp").unwrap();let socket=root.path().join("ambient.sock");let listener=tokio::net::UnixListener::bind(&socket).unwrap();
 let calls=std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));let observed=calls.clone();
 let task=tokio::spawn(async move{loop{let(s,_)=listener.accept().await.unwrap();observed.fetch_add(1,std::sync::atomic::Ordering::SeqCst);tokio::spawn(async move{let(r,w)=s.into_split();let(p,mut rx)=Peer::spawn(r,w);while let Some(m)=rx.recv().await{if let Incoming::Request{id,method,..}=m {p.respond(id,Ok(if method=="hello" {json!({"epoch":"private-ambient-fixture"})}else{json!({"clients":[]})}));}}});}});
 for (marker,expected) in [("terminal",0),("",1)] {
  let output=Command::new(env!("CARGO_BIN_EXE_vqro")).arg("status").env_clear().env("HOME",root.path()).env("PATH","/usr/bin:/bin").env("VQRO_STATE_DIR",root.path().join("scratch")).env("VQRO_SOCKET",&socket).env("VQRO_SOCKET_SOURCE",marker).output().unwrap();
  assert!(output.status.success());tokio::time::sleep(Duration::from_millis(10)).await;assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),expected,"marker={marker}, stdout={}",String::from_utf8_lossy(&output.stdout));
 }
 task.abort();
}
