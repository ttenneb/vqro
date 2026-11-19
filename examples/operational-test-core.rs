//! Test/measurement core with explicit durability gates. Not a production entrypoint.
use std::{path::PathBuf,sync::{Arc,atomic::{AtomicBool,Ordering}},time::Duration};
use serde_json::{json,Value};
#[tokio::main]
async fn main()->anyhow::Result<()> {
 let args=std::env::args().skip(1).collect::<Vec<_>>();anyhow::ensure!(args.len()==3,"usage: operational-test-core STATE SOCKET none|claim-before|claim-after|claim-unconfirmed|claim-error");
 let state=PathBuf::from(&args[0]);let socket=PathBuf::from(&args[1]);let fault=args[2].clone();anyhow::ensure!(["none","claim-before","claim-after","claim-unconfirmed","claim-error"].contains(&fault.as_str()),"unknown fault");
 if !state.join("workspace.json").exists() {
  let mut store=vqro::store::Store::open(state.join("workspace.json"))?;
  store.apply("fixture",None,vec![vqro::store::Op::Set {key:"extension/owner".into(),value:json!({"enabled":true,"argv":["/usr/bin/python3",PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/managed_identity.py"),state.join("owner.token")]}),expect:None}])?;
 }
 let core=vqro::server::start(socket,state.clone(),false,vqro::server::Backend::Fake,None)?;
 let ready=state.join("ready");let once=Arc::new(AtomicBool::new(false));
 core.lock().unwrap().configure_operational_writer(Arc::new(move|path,bytes| {
  if fault=="none" {return vqro::store::write_atomic(path,bytes);}
  let parsed:Value=serde_json::from_slice(bytes).unwrap();
  if parsed["last"]["note"]!="claim"||once.swap(true,Ordering::AcqRel) {return vqro::store::write_atomic(path,bytes);}
  let pause=|phase:&str| {std::fs::write(state.join("fault-boundary"),phase).unwrap();while !state.join("release-fault").exists(){std::thread::sleep(Duration::from_millis(2));}};
  if fault=="claim-before" {pause("before-claim-write");}
  if fault=="claim-error" {return Err(std::io::Error::other("injected before rename").into());}
  vqro::store::write_atomic(path,bytes)?;
  if fault=="claim-after" {pause("after-durable-before-receipt");}
  if fault=="claim-unconfirmed" {let mut error=vqro::store::AtomicWriteError::from(std::io::Error::other("injected missing directory confirmation"));error.renamed=true;return Err(error);}
  Ok(())
 })).unwrap();
 std::fs::write(ready,"ready")?;let shutdown=core.lock().unwrap().shutdown.clone();
 tokio::select! {_=shutdown.stopped()=>{},_=tokio::signal::ctrl_c()=>vqro::server::shutdown(&core).await}
 Ok(())
}
