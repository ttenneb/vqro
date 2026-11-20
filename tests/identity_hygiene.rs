//! Private persistence/RSS regressions; no real backend, credentials or live endpoint.
use vqro::provider::{Identity,IdentityMap};
use serde_json::{json,Value};
use std::time::{Duration,Instant};
#[path="support/timing.rs"] mod timing;
fn identity()->Identity{Identity{provider:"fixture".into(),instance:"instance".into(),reference:"terminal".into(),incarnation:"birth".into()}}
#[test]
fn ten_thousand_lifetimes_stay_bounded_and_never_reuse_an_id(){
 let root=tempfile::tempdir().unwrap();let path=root.path().join("resources.json");let mut map=IdentityMap::open(Some(path.clone())).unwrap();let mut max=Duration::ZERO;let started=Instant::now();
 for n in 1..=10_000 {
  let op=timing::WorkClock::start();let id=map.identify(identity()).unwrap().id;assert_eq!(id,format!("r{n}"));
  assert_eq!(map.retire(|_|true).unwrap().0,vec![id]);let work=op.finish();max=max.max(work.cpu);
  work.assert_cpu_budget(Duration::from_millis(100),&format!("identity lifetime {n}"));
  if n%100==0{let saved:Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();assert!(saved["ids"].as_object().unwrap().is_empty(),"retired records must not accumulate");assert!(std::fs::metadata(&path).unwrap().len()<128);}
 }
 drop(map);let mut restarted=IdentityMap::open(Some(path)).unwrap();assert_eq!(restarted.identify(identity()).unwrap().id,"r10001");
 println!("identity churn10000 total_wall_ms={:.3} max_lifetime_cpu_ms={:.3}",started.elapsed().as_secs_f64()*1000.0,max.as_secs_f64()*1000.0);
}
#[test]
fn old_tombstones_and_legacy_records_advance_high_water_without_being_retained(){
 let root=tempfile::tempdir().unwrap();let path=root.path().join("resources.json");
 std::fs::write(&path,json!({"next":1,"ids":{"r9000":{"identity":identity(),"retired":true},"r9999":["fixture","legacy"]}}).to_string()).unwrap();
 let mut map=IdentityMap::open(Some(path.clone())).unwrap();assert!(!map.active("r9000"));assert_eq!(map.identify(identity()).unwrap().id,"r10000");
 let saved:Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();assert_eq!(saved["ids"].as_object().unwrap().len(),1);assert_eq!(saved["next"],10001);
}

#[tokio::test]
async fn native_inventory_600_new_ids_has_a_bounded_lock_hold(){
 use vqro::{core::Core,adapter::{AdapterHandle,AdapterEvent},provider::ProviderInfo};
 let root=tempfile::tempdir().unwrap();let core=std::sync::Mutex::new(Core::new(vqro::store::Store::in_memory(),Some(root.path().into()),root.path().join("unused"),false).unwrap());let(tx,_rx)=tokio::sync::mpsc::unbounded_channel();
 let mut c=core.lock().unwrap();c.install_adapter(AdapterHandle{name:"fixture".into(),session:None,context:None,capabilities:json!({}),cmds:tx},true).unwrap();c.on_adapter_event("fixture",AdapterEvent::Registered(ProviderInfo{connection:1,instance:"A".into(),role:"backend".into(),software:json!({"name":"fixture","version":"1"})}));
 let facts=(0..600).map(|i|serde_json::from_value(json!({"backend_ref":format!("t{i}"),"kind":"terminal","state":"live"})).unwrap()).collect();let start=timing::WorkClock::start();c.on_adapter_event("fixture",AdapterEvent::Snapshot(facts));let held=start.finish();assert_eq!(c.resources.len(),600);
 println!("public inventory600 lock_work_cpu_ms={:.3} wall_lock_hold_ms={:.3}",held.cpu.as_secs_f64()*1000.0,held.wall.as_secs_f64()*1000.0);held.assert_cpu_budget(Duration::from_millis(150),"public inventory under core lock");
}
#[tokio::test]
async fn claimed_extension_names_do_not_grant_extension_authorship(){
 let root=tempfile::Builder::new().prefix("pih").tempdir_in("/tmp").unwrap();let socket=root.path().join("s");let core=vqro::server::start(socket.clone(),root.path().join("st"),false,vqro::server::Backend::Extensions,None).unwrap();
 let(a,_a,_)=vqro::client::connect(&socket,"a").await.unwrap();let(b,_b,_)=vqro::client::connect(&socket,"b").await.unwrap();
 let one=a.call("hello",json!({"extension":"claimed"})).await.unwrap();let two=b.call("hello",json!({"extension":"claimed"})).await.unwrap();assert!(!one["you"].as_str().unwrap().starts_with("ext:"));assert_ne!(one["you"],two["you"]);
 vqro::server::shutdown(&core).await;
}
#[tokio::test]
async fn different_live_authors_cannot_overwrite_or_clear_a_published_view(){
 let root=tempfile::Builder::new().prefix("pih").tempdir_in("/tmp").unwrap();let socket=root.path().join("s");let core=vqro::server::start(socket.clone(),root.path().join("st"),false,vqro::server::Backend::Extensions,None).unwrap();
 let(a,_a,_)=vqro::client::connect(&socket,"a").await.unwrap();let(b,_b,_)=vqro::client::connect(&socket,"b").await.unwrap();
 a.call("view.publish",json!({"view":"owned","content":{"label":"original"}})).await.unwrap();
 assert_eq!(b.call("view.publish",json!({"view":"owned","content":{"label":"replacement"}})).await.unwrap_err().kind(),"conflict");
 assert_eq!(b.call("view.publish",json!({"view":"owned","content":null})).await.unwrap_err().kind(),"conflict");vqro::server::shutdown(&core).await;
}

#[cfg(target_os="linux")]
#[test]
fn ten_thousand_idle_subscribers_disconnect_without_retained_rss(){
 use std::{io::{BufRead,BufReader,Write},os::unix::net::UnixStream,process::{Child,Command,Stdio}};
 struct Owned(Child);impl Drop for Owned{fn drop(&mut self){let _=self.0.kill();let _=self.0.wait();}}
 fn cycle(socket:&std::path::Path){
  let mut s=UnixStream::connect(socket).unwrap();s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
  s.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"hello\",\"params\":{\"client\":\"idle-test\"}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"events.subscribe\",\"params\":{}}\n").unwrap();
  let mut reader=BufReader::new(s);let mut line=String::new();loop{line.clear();assert!(reader.read_line(&mut line).unwrap()>0);let v:Value=serde_json::from_str(&line).unwrap();assert!(v.get("error").is_none(),"{v}");if v["id"]==2{break;}}
 }
 fn rss(pid:u32)->u64{std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap().lines().find_map(|l|l.strip_prefix("VmRSS:").map(|v|v.split_whitespace().next().unwrap().parse::<u64>().unwrap()*1024)).unwrap()}
 let root=tempfile::Builder::new().prefix("pih").tempdir_in("/tmp").unwrap();let socket=root.path().join("s");let ext=root.path().join("ext");std::fs::create_dir(&ext).unwrap();
 let mut child=Owned(Command::new(env!("CARGO_BIN_EXE_vqro")).args(["serve","--safe"]).env_clear().env("HOME",root.path()).env("PATH","/usr/bin:/bin").env("VQRO_STATE_DIR",root.path().join("st")).env("VQRO_SOCKET",&socket).env("VQRO_EXTENSIONS_DIR",ext).env("XDG_RUNTIME_DIR",root.path()).env("HC_SOCKET",root.path().join("private-hc.sock")).env("HC_SESSION","identity-private").env("VQRO_HERDR","[\"/bin/false\"]").stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
 let deadline=Instant::now()+Duration::from_secs(5);while !socket.exists(){assert!(child.0.try_wait().unwrap().is_none());assert!(Instant::now()<deadline);std::thread::sleep(Duration::from_millis(5));}
 for _ in 0..200{cycle(&socket);}std::thread::sleep(Duration::from_millis(100));let before=rss(child.0.id());let mut peak=before;
 for n in 0..10_000{cycle(&socket);if n%1000==0{peak=peak.max(rss(child.0.id()));}}
 std::thread::sleep(Duration::from_millis(200));let after=rss(child.0.id());println!("idle subscribers10000 rss_before={before} rss_peak={peak} rss_after={after}");
 assert!(after<=before+8*1024*1024,"idle subscription RSS did not return to its warm baseline: {before} -> {after}");
}
