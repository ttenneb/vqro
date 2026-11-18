//! Real RPC storm against a private fake core. CORE_CONTENTION_BIN pins a baseline binary for red evidence.
use std::{process::{Command,Child,Stdio},sync::{Arc,atomic::{AtomicBool,Ordering}},time::{Duration,Instant}};
use prim::{client,rpc::Incoming};
use serde_json::json;
use base64::Engine;
struct Running(Child);
impl Drop for Running {fn drop(&mut self){let _=self.0.kill();let _=self.0.wait();}}
#[tokio::test(flavor="multi_thread",worker_threads=4)]
#[ignore="private 400-provider RPC storm; full gate"]
async fn rejected_provider_snapshots_do_not_starve_healthy_control_or_echo() {
 let root=tempfile::Builder::new().prefix("pcc").tempdir_in("/tmp").unwrap();
 for dir in ["state","run","ext","home"] {std::fs::create_dir(root.path().join(dir)).unwrap();}
 let socket=root.path().join("p");
 let binary=std::env::var_os("CORE_CONTENTION_BIN").unwrap_or_else(||env!("CARGO_BIN_EXE_prim").into());
 let _core=Running(Command::new(binary).args(["serve","--fake"]).env_clear().env("HOME",root.path().join("home")).env("PATH","/usr/bin:/bin")
  .env("PRIM_SOCKET",&socket).env("PRIM_STATE_DIR",root.path().join("state")).env("PRIM_EXTENSIONS_DIR",root.path().join("ext"))
  .env("XDG_RUNTIME_DIR",root.path().join("run")).env("HC_SOCKET",root.path().join("h")).env("HC_SESSION","core-storm").env("PRIM_HERDR",r#"["/bin/false"]"#)
  .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
 let until=Instant::now()+Duration::from_secs(5);
 let (probe,mut incoming,_)=loop {match client::connect(&socket,"healthy").await {Ok(p)=>break p,Err(_) if Instant::now()<until=>tokio::time::sleep(Duration::from_millis(10)).await,Err(e)=>panic!("private core: {e}")}};
 probe.call("workspace.apply",json!({"ops":(0..1000).map(|i|json!({"op":"set","key":format!("doc/load{i}"),"value":"x".repeat(2048)})).collect::<Vec<_>>()})).await.unwrap();
 let terminal=probe.call("resource.spawn",json!({"argv":["sh"]})).await.unwrap()["resource"]["id"].clone();
 let stream=probe.call("resource.attach",json!({"resource":terminal,"cols":80,"rows":24})).await.unwrap()["stream"].clone();
 let mut peers=Vec::new();
 for i in 0..400 {
  let (peer,_,_)=client::connect(&socket,"storm").await.unwrap();
  let generation=peer.call("provider.register",json!({"name":format!("storm{i}"),"role":"backend","instance":"instance","software":{"name":"storm","version":"1"},"provider_version":"0","capabilities":{}})).await.unwrap()["generation"].clone();
  if i==0 {for chunk in 0..6 {peer.call("provider.sync",json!({"generation":generation,"final":chunk==5,"resources":(0..100).map(|j|json!({"ref":format!("t{}",chunk*100+j),"incarnation":"birth","kind":"terminal","state":"live","data":{"fixture":"x".repeat(4096)}})).collect::<Vec<_>>()})).await.unwrap();}}
  peers.push((peer,generation));
 }
 let barrier=Arc::new(tokio::sync::Barrier::new(401));let stop=Arc::new(AtomicBool::new(false));let mut tasks=tokio::task::JoinSet::new();
 for (peer,generation) in peers {let barrier=barrier.clone();let stop=stop.clone();tasks.spawn(async move {
  barrier.wait().await;let mut rejected=0;
  while !stop.load(Ordering::Relaxed) {let error=peer.call("workspace.snapshot",json!({"generation":generation})).await.unwrap_err();assert!(matches!(error.kind(),"too_large"|"overloaded"),"{error}");rejected+=1;}
  peer.close();rejected
 });}
 barrier.wait().await;tokio::time::sleep(Duration::from_millis(100)).await;
 let mut control=Vec::new();let mut echoes=Vec::new();
 for i in 0..20 {
  let t=Instant::now();probe.call_with_timeout("hello",json!({"client":"healthy"}),Duration::from_secs(5)).await.unwrap();control.push(t.elapsed());
  let text=format!("healthy-echo-{i}");let t=Instant::now();probe.notify("stream.input",json!({"stream":stream,"text":text}));
  tokio::time::timeout(Duration::from_secs(5),async {loop {if let Some(Incoming::Notification{method,params})=incoming.recv().await {if method=="stream.data" {if let Some(data)=params["data"].as_str() {if String::from_utf8_lossy(&base64::engine::general_purpose::STANDARD.decode(data).unwrap()).contains(&text) {break;}}}}}}).await.unwrap();
  echoes.push(t.elapsed());tokio::time::sleep(Duration::from_millis(5)).await;
 }
 stop.store(true,Ordering::Relaxed);let mut rejected=0;
 tokio::time::timeout(Duration::from_secs(30),async {while let Some(result)=tasks.join_next().await {rejected+=result.unwrap();}}).await.unwrap();
 let control=*control.iter().max().unwrap();let echo=*echoes.iter().max().unwrap();
 eprintln!("storm rejected={rejected} healthy_control_max_ms={} echo_max_ms={}",control.as_secs_f64()*1000.,echo.as_secs_f64()*1000.);
 assert!(control<Duration::from_millis(250),"healthy control starved: {control:?}");
 assert!(echo<Duration::from_millis(250),"healthy echo starved: {echo:?}");
}
