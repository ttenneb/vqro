//! Product RPC tests: real owner credentials, separate files, protected keys and fenced publication.
use std::{process::{Child,Command,Stdio},time::{Duration,Instant}};
use serde_json::{json,Value};
use vqro::{client,rpc::{Peer,IncomingReceiver}};
#[path="support/managed.rs"] mod managed;
struct Fixture {child:Child,root:tempfile::TempDir}
impl Drop for Fixture {fn drop(&mut self){let _=self.child.kill();let _=self.child.wait();}}
impl Fixture {
 async fn new()->Self {
  let root=tempfile::Builder::new().prefix("opdom").tempdir_in("/tmp").unwrap();for dir in ["state","extensions","run"]{std::fs::create_dir(root.path().join(dir)).unwrap();}
  let mut store=vqro::store::Store::open(root.path().join("state/workspace.json")).unwrap();
  store.apply("fixture",None,["owner","stranger"].into_iter().map(|name|vqro::store::Op::Set {key:format!("extension/{name}"),value:managed::spec(&root.path().join(format!("{name}.token"))),expect:None}).collect()).unwrap();drop(store);
  let child=Command::new(env!("CARGO_BIN_EXE_vqro")).env_clear().args(["serve","--fake"]).env("HOME",root.path()).env("PATH","/usr/bin:/bin").env("VQRO_STATE_DIR",root.path().join("state")).env("VQRO_SOCKET",root.path().join("p")).env("VQRO_EXTENSIONS_DIR",root.path().join("extensions")).env("XDG_RUNTIME_DIR",root.path().join("run")).env("HC_SOCKET",root.path().join("hc")).env("HC_SESSION","opdom-private").env("VQRO_HERDR",r#"["/bin/false"]"#).stdin(Stdio::null()).stdout(Stdio::null()).stderr(std::fs::File::create(root.path().join("core.log")).unwrap()).spawn().unwrap();
  Self {child,root}
 }
 async fn peer(&self)->(Peer,IncomingReceiver) {
  let until=Instant::now()+Duration::from_secs(5);loop {if let Ok((peer,rx,_))=client::connect(&self.root.path().join("p"),"operational-test").await{return(peer,rx);}assert!(Instant::now()<until);tokio::time::sleep(Duration::from_millis(10)).await;}
 }
 async fn owner(&self,name:&str)->(Peer,IncomingReceiver) {let(peer,rx)=self.peer().await;let token=managed::token(&self.root.path().join(format!("{name}.token"))).await;peer.call("hello",json!({"extension":name,"extension_token":token})).await.unwrap();(peer,rx)}
}
fn set(key:&str,value:Value,expect:u64)->Value {json!({"op":"set","key":key,"value":value,"expect":expect})}
#[tokio::test]
async fn only_current_owner_writes_separate_nonundo_records_and_mixed_actions_fail_whole() {
 let f=Fixture::new().await;let(control,_rx)=f.peer().await;let(owner,_orx)=f.owner("owner").await;let(other,_srx)=f.owner("stranger").await;
 assert_eq!(control.call("operational.register",json!({"name":"state"})).await.unwrap_err().kind(),"forbidden");
 let reg=owner.call("operational.register",json!({"name":"state","bytes":8388608})).await.unwrap();assert_eq!(reg["limits"]["records"],4096);
 assert_eq!(other.call("operational.register",json!({"name":"state"})).await.unwrap_err().kind(),"forbidden");
 assert_eq!(owner.call("operational.register",json!({"name":"second"})).await.unwrap_err().kind(),"conflict");
 let before=control.call("workspace.history",json!({"limit":500})).await.unwrap();
 let receipt=owner.call("operational.apply",json!({"domain":"state","ops":[set("operational/state/message",json!({"phase":"claimed"}),0)]})).await.unwrap();assert_eq!(receipt["undoable"],false);
 assert_eq!(control.call("workspace.history",json!({"limit":500})).await.unwrap(),before);
 let record=control.call("operational.get",json!({"domain":"state","key":"operational/state/message"})).await.unwrap();assert_eq!(record["entry"]["value"]["phase"],"claimed");
 let raw=control.call("workspace.apply",json!({"history":false,"ops":[set("doc/bypass",json!(1),0)]})).await.unwrap_err();assert_eq!(raw.kind(),"forbidden");
 for ops in [vec![set("doc/ordinary",json!(1),0),set("operational/state/message",json!("bad"),0)],vec![json!({"op":"delete","key":"operational/state/message"})]] {
  assert!(control.call("workspace.apply",json!({"ops":ops})).await.is_err());
 }
 assert!(owner.call("operational.apply",json!({"domain":"state","ops":[set("operational/state/next",json!(1),0),set("doc/ordinary",json!(2),0)]})).await.is_err());
 assert!(control.call("workspace.get",json!({"key":"doc/ordinary"})).await.unwrap().is_null());
 assert!(control.call("operational.get",json!({"domain":"state","key":"operational/state/next"})).await.unwrap()["entry"].is_null());
 let file:Value=serde_json::from_slice(&std::fs::read(f.root.path().join("state/operational/state.json")).unwrap()).unwrap();assert!(file.get("history").is_none());assert_eq!(file["owner"],"owner");
 let workspace:Value=serde_json::from_slice(&std::fs::read(f.root.path().join("state/workspace.json")).unwrap()).unwrap();assert!(workspace["entries"].get("operational/state/message").is_none());
 control.call("core.shutdown",json!({})).await.unwrap();
}
#[tokio::test]
async fn snapshot_pages_and_replayed_events_share_the_global_capture_fence() {
 let f=Fixture::new().await;let(control,_rx)=f.peer().await;let(owner,_orx)=f.owner("owner").await;
 owner.call("operational.register",json!({"name":"state"})).await.unwrap();
 for batch in 0..3 {owner.call("operational.apply",json!({"domain":"state","ops":(0..50).map(|n|set(&format!("operational/state/{:04}",batch*50+n),json!({"n":batch*50+n}),0)).collect::<Vec<_>>()})).await.unwrap();}
 let snapshot=control.call("operational.snapshot",json!({"domain":"state"})).await.unwrap();assert_eq!(snapshot["entries"].as_object().unwrap().len(),150);
 let page=control.call("operational.list",json!({"domain":"state","limit":64,"revision":snapshot["revision"]})).await.unwrap();assert_eq!(page["entries"].as_array().unwrap().len(),64);assert!(serde_json::to_vec(&page).unwrap().len()<=256*1024);
 let second=control.call("operational.list",json!({"domain":"state","after":page["next"],"revision":snapshot["revision"]})).await.unwrap();assert_eq!(second["entries"].as_array().unwrap().len(),64);
 let receipt=owner.call("operational.apply",json!({"domain":"state","control":true,"ops":[set("operational/state/new",json!(1),0)]})).await.unwrap();
 assert!(receipt["seq"].as_u64().unwrap()>snapshot["seq"].as_u64().unwrap());assert_eq!(receipt["epoch"],snapshot["epoch"]);
 assert_eq!(control.call("operational.list",json!({"domain":"state","after":second["next"],"revision":snapshot["revision"]})).await.unwrap_err().kind(),"conflict");
 let(observer,mut events)=f.peer().await;observer.call("events.subscribe",json!({"epoch":snapshot["epoch"],"from":snapshot["seq"],"types":["operational"]})).await.unwrap();
 let event=tokio::time::timeout(Duration::from_secs(2),events.recv()).await.unwrap().unwrap();
 let params=match event {vqro::rpc::Incoming::Notification{params,..}=>params,_=>panic!("notification")};assert_eq!(params["data"]["txn"],receipt["txn"]);assert_eq!(params["type"],"operational");assert_eq!(params["data"]["author"],"ext:owner");
 observer.close();control.call("core.shutdown",json!({})).await.unwrap();
}
#[tokio::test]
async fn retired_and_same_name_replacement_do_not_revive_old_operational_owners() {
 let f=Fixture::new().await;let(control,_rx)=f.peer().await;let(old,_orx)=f.owner("owner").await;
 old.call("operational.register",json!({"name":"state"})).await.unwrap();old.call("operational.apply",json!({"domain":"state","ops":[set("operational/state/claim",json!({"phase":"claimed"}),0)]})).await.unwrap();
 control.call("command.invoke",json!({"name":"vqro.extension.disable","args":{"name":"owner"}})).await.unwrap();
 assert_eq!(old.call("operational.apply",json!({"domain":"state","control":true,"ops":[set("operational/state/claim",json!("erased"),1)]})).await.unwrap_err().kind(),"stale_generation");
 std::fs::remove_file(f.root.path().join("owner.token")).unwrap();control.call("command.invoke",json!({"name":"vqro.extension.enable","args":{"name":"owner"}})).await.unwrap();let(new,_nrx)=f.owner("owner").await;
 new.call("operational.register",json!({"name":"state"})).await.unwrap();assert_eq!(new.call("operational.get",json!({"domain":"state","key":"operational/state/claim"})).await.unwrap()["entry"]["value"]["phase"],"claimed");
 assert_eq!(old.call("operational.register",json!({"name":"state"})).await.unwrap_err().kind(),"stale_generation");
 control.call("core.shutdown",json!({})).await.unwrap();
}
