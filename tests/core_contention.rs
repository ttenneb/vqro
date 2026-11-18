//! In-process, isolated target-scale lifecycle regression/profile; no backend processes.
use prim::{core::Core,store::{Store,Op},adapter::{AdapterHandle,AdapterEvent,ResourceFacts},provider::ProviderInfo};
use serde_json::json;
use std::time::Instant;
fn install(c:&mut Core,name:&str,role:&str) {
 let(tx,_rx)=tokio::sync::mpsc::unbounded_channel();
 c.install_provider(AdapterHandle {name:name.into(),session:None,context:None,capabilities:json!({}),cmds:tx},false,role=="backend").unwrap();
 c.on_adapter_event(name,AdapterEvent::Registered(ProviderInfo {connection:1,instance:"instance".into(),role:role.into(),software:json!({"name":"storm","version":"1"})}));
}
fn facts(reference:&str,kind:&str)->ResourceFacts {serde_json::from_value(json!({"backend_ref":reference,"incarnation":"birth","kind":kind,"state":"live","data":{"fixture":"x".repeat(4096)}})).unwrap()}
#[test]
#[ignore="target-scale release profiling; private Store only"]
fn reconnect_storm_profile() {
 let root=tempfile::tempdir().unwrap();let mut store=Store::in_memory();
 store.apply("fixture",None,(0..1000).map(|i|Op::Set{key:format!("doc/load{i}"),value:json!("x".repeat(2048)),expect:None}).collect()).unwrap();
 let mut c=Core::new(store,Some(root.path().into()),root.path().join("unused"),false).unwrap();
 install(&mut c,"backend","backend");
 c.upsert_resources("backend",(0..600).map(|i|facts(&format!("t{i}"),"terminal")).collect(),true).unwrap();
 for i in 0..400 {let name=format!("runtime{i}");install(&mut c,&name,"runtime");c.upsert_resources(&name,vec![facts("agent","agent")],true).unwrap();}
 let mut times=[Vec::new(),Vec::new(),Vec::new(),Vec::new(),Vec::new()];
 for i in 0..100 {
  let name=format!("runtime{}",i%400);let t=Instant::now();install(&mut c,&name,"runtime");times[0].push(t.elapsed().as_secs_f64()*1000.);
  let t=Instant::now();c.upsert_resources(&name,vec![facts("agent","agent")],true).unwrap();times[1].push(t.elapsed().as_secs_f64()*1000.);
  let t=Instant::now();let snapshot=c.capture_snapshot();times[2].push(t.elapsed().as_secs_f64()*1000.);
  let t=Instant::now();std::hint::black_box(snapshot.into_value());times[4].push(t.elapsed().as_secs_f64()*1000.);
  let t=Instant::now();c.on_adapter_event(&name,AdapterEvent::Connected(false,None));times[3].push(t.elapsed().as_secs_f64()*1000.);
 }
 for (name,mut times) in ["register","sync","snapshot_capture","disconnect","snapshot_materialize"].into_iter().zip(times) {times.sort_by(f64::total_cmp);eprintln!("{name}: p50_ms={} p99_ms={} max_ms={}",times[50],times[98],times[99]);}
}
