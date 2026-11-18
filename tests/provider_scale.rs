//! P0 target: one runtime provider per agent, not one provider per runtime product.
#![cfg(target_os="linux")]
use std::{path::PathBuf,process::{Child,Command,Stdio},time::{Duration,Instant}};
use prim::{client,rpc::{Peer,Incoming,IncomingReceiver},provider_client::{Provider,Config}};
use serde_json::{json,Value};
struct Private { child:Child,root:tempfile::TempDir,socket:PathBuf }
impl Drop for Private{fn drop(&mut self){let _=self.child.kill();let _=self.child.wait();}}
impl Private{
 async fn start()->Self{
  let root=tempfile::Builder::new().prefix("prgs").tempdir_in("/tmp").unwrap();let socket=root.path().join("s");let ext=root.path().join("ext");std::fs::create_dir(&ext).unwrap();
  let child=Command::new(env!("CARGO_BIN_EXE_prim")).arg("serve").env_clear().env("HOME",root.path()).env("PATH","/usr/bin:/bin").env("PRIM_STATE_DIR",root.path().join("st")).env("PRIM_SOCKET",&socket).env("PRIM_EXTENSIONS_DIR",ext).env("PRIM_HERDR","[\"/bin/false\"]").env("XDG_RUNTIME_DIR",root.path()).env("HC_SOCKET",root.path().join("never-live-hc")).env("HC_SESSION","registry-test").stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
  let p=Self{child,root,socket};let end=Instant::now()+Duration::from_secs(5);while !p.socket.exists(){assert!(Instant::now()<end);tokio::time::sleep(Duration::from_millis(5)).await;}p
 }
 fn rss(&self)->u64{std::fs::read_to_string(format!("/proc/{}/status",self.child.id())).unwrap().lines().find_map(|l|l.strip_prefix("VmRSS:").map(|v|v.split_whitespace().next().unwrap().parse::<u64>().unwrap()*1024)).unwrap()}
}
fn facts(reference:&str,kind:&str)->Value{json!({"ref":reference,"incarnation":"birth","kind":kind,"state":"live","argv":[],"data":{}})}
async fn host_spawn(viewer:&Peer,host:&Peer,requests:&mut IncomingReceiver,i:usize)->String{
 let caller=viewer.clone();let spawn=tokio::spawn(async move{caller.call("resource.spawn",json!({"provider":"hosts","bridge":true,"argv":["fake-pi"]})).await.unwrap()});
 let(id,params)=loop{if let Some(Incoming::Request{id,method,params})=requests.recv().await{assert_eq!(method,"effect.invoke");break(id,params);}};
 let token=params["args"]["env"]["PRIM_BRIDGE_TOKEN"].as_str().unwrap().to_string();host.respond(id,Ok(json!({"outcome":"observed","data":{"facts":facts(&format!("h{i}"),"terminal")}})));spawn.await.unwrap();token
}
async fn runtime(socket:&std::path::Path,i:usize,token:String)->(Provider,tokio::sync::mpsc::UnboundedReceiver<prim::provider_client::Request>){
 let mut config=Config::new(format!("pi-{i}"),format!("runtime-{i}"),serde_json::from_value(json!({"agent.prompt":{},"agent.abort":{}})).unwrap());config.role="runtime".into();config.token=Some(token);
 Provider::connect(tokio::net::UnixStream::connect(socket).await.unwrap(),config).await.unwrap()
}
fn percentile(values:&[f64],q:f64)->f64{let mut v=values.to_vec();v.sort_by(f64::total_cmp);v[((v.len()-1)as f64*q).round()as usize]}
#[tokio::test(flavor="multi_thread",worker_threads=4)]
async fn six_hundred_runtime_providers_and_two_thousand_completed_lifetimes(){
 let p=Private::start().await;let(viewer,_rx,_)=client::connect(&p.socket,"registry-viewer").await.unwrap();let(host,mut requests,_)=client::connect(&p.socket,"fake-host").await.unwrap();
 let reg=host.call("provider.register",json!({"name":"hosts","role":"backend","instance":"host-instance","provider_version":"1","software":{"name":"fixture","version":"1"},"capabilities":{"terminal.spawn":{"env":true}}})).await.unwrap();let generation=reg["generation"].clone();
 host.call("provider.sync",json!({"generation":generation,"resources":[],"final":true})).await.unwrap();
 let baseline=p.rss();let mut providers=Vec::new();let mut times=Vec::new();let setup=Instant::now();
 for i in 0..600{let token=host_spawn(&viewer,&host,&mut requests,i).await;let start=Instant::now();let(provider,inbox)=runtime(&p.socket,i,token).await;times.push(start.elapsed().as_secs_f64()*1000.0);provider.full_sync(vec![facts("agent","agent")]).await.unwrap();providers.push((provider,inbox));}
 let setup_ms=setup.elapsed().as_secs_f64()*1000.0;let active_rss=p.rss();let start=Instant::now();let snapshot=viewer.call("workspace.snapshot",Value::Null).await.unwrap();let snapshot_ms=start.elapsed().as_secs_f64()*1000.0;let snapshot_bytes=serde_json::to_vec(&snapshot).unwrap().len();
 assert_eq!(snapshot["providers"].as_object().unwrap().len(),601);assert_eq!(snapshot["resources"].as_array().unwrap().len(),1200);assert!(snapshot_bytes<8*1024*1024);assert!(snapshot_ms<500.0,"snapshot took {snapshot_ms}ms");assert!(active_rss<baseline+128*1024*1024,"600-provider RSS too large: {baseline} -> {active_rss}");
 let cleanup=Instant::now();for(provider,_)in &providers{provider.full_sync(Vec::new()).await.unwrap();provider.close();}drop(providers);let retire_agents_ms=cleanup.elapsed().as_secs_f64()*1000.0;
 let retire_hosts=Instant::now();host.call("provider.sync",json!({"generation":generation,"resources":[],"final":true})).await.unwrap();
 let end=Instant::now()+Duration::from_secs(10);loop{let s=viewer.call("workspace.snapshot",Value::Null).await.unwrap();if s["providers"].as_object().unwrap().len()==1&&s["resources"].as_array().unwrap().is_empty(){break;}assert!(Instant::now()<end,"empty disconnected runtime registry did not drain");tokio::time::sleep(Duration::from_millis(5)).await;}
 let retire_hosts_ms=retire_hosts.elapsed().as_secs_f64()*1000.0;let cleanup_ms=cleanup.elapsed().as_secs_f64()*1000.0;let churn=Instant::now();
 for i in 600..2600{let token=host_spawn(&viewer,&host,&mut requests,i).await;let(provider,_inbox)=runtime(&p.socket,i,token).await;provider.full_sync(vec![facts("agent","agent")]).await.unwrap();provider.full_sync(Vec::new()).await.unwrap();provider.close();host.call("provider.sync",json!({"generation":generation,"resources":[],"final":true})).await.unwrap();}
 let end=Instant::now()+Duration::from_secs(5);let final_state=loop{let s=viewer.call("workspace.snapshot",Value::Null).await.unwrap();if s["providers"].as_object().unwrap().len()==1{break s;}assert!(Instant::now()<end);tokio::time::sleep(Duration::from_millis(5)).await;};
 assert!(final_state["resources"].as_array().unwrap().is_empty());assert_eq!(final_state["adapters"].as_object().unwrap().len(),1);
 let end=Instant::now()+Duration::from_secs(5);loop{let clients=viewer.call("client.list",Value::Null).await.unwrap();if clients["clients"].as_array().unwrap().len()==2{break;}assert!(Instant::now()<end,"runtime connection buffers did not drain");tokio::time::sleep(Duration::from_millis(5)).await;}
 let map:Value=serde_json::from_slice(&std::fs::read(p.root.path().join("st/resources.json")).unwrap()).unwrap();assert!(map["ids"].as_object().unwrap().is_empty());let tokens:Value=serde_json::from_slice(&std::fs::read(p.root.path().join("st/runtime-tokens.json")).unwrap()).unwrap();assert!(tokens.as_object().unwrap().is_empty());
 let receipt=json!({"providers":600,"sequential_lifetimes":2000,"registration_p50_ms":percentile(&times,0.5),"registration_p99_ms":percentile(&times,0.99),"registration_max_ms":percentile(&times,1.0),"setup_ms":setup_ms,"snapshot_ms":snapshot_ms,"snapshot_bytes":snapshot_bytes,"rss_baseline":baseline,"rss_active":active_rss,"rss_after_churn":p.rss(),"teardown600_ms":cleanup_ms,"retire_agents600_ms":retire_agents_ms,"retire_hosts600_ms":retire_hosts_ms,"churn_ms":churn.elapsed().as_secs_f64()*1000.0});
 println!("provider scale {receipt}");if let Some(path)=std::env::var_os("REGISTRY_RECEIPT"){std::fs::write(path,receipt.to_string()).unwrap();}
 viewer.call("core.shutdown",Value::Null).await.unwrap();
}
