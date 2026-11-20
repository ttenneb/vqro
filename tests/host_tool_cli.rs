//! Real tool CLI children of a two-endpoint runtime use only its verified host context.
use serde_json::json;
use vqro::{client,provider_client::{Config,Provider},rpc::{Incoming,Peer},server::{self,Backend}};
use std::{path::Path,time::Duration,sync::{Arc,Mutex,Condvar,atomic::{AtomicBool,Ordering}}};
struct Gate {armed:AtomicBool,entered:tokio::sync::Notify,released:Mutex<bool>,wake:Condvar}
impl Gate {
    fn release(&self){*self.released.lock().unwrap()=true;self.wake.notify_all();}
    fn writer(self:&Arc<Self>)->vqro::store::DurableWriter{let gate=self.clone();Arc::new(move|path,bytes|{
        if gate.armed.swap(false,Ordering::SeqCst){gate.entered.notify_one();let mut released=gate.released.lock().unwrap();while !*released{released=gate.wake.wait(released).unwrap();}}
        vqro::store::write_atomic(path,bytes)
    })}
}
struct Release(Arc<Gate>);impl Drop for Release{fn drop(&mut self){self.0.release();}}

async fn tool_cli(root:&Path,socket:&Path,env:&std::collections::BTreeMap<String,String>,key:&str)->std::process::Output {
    let binary=std::env::var("VQRO_TEST_BASELINE_CLI").ok().filter(|s|!s.is_empty()).unwrap_or_else(||env!("CARGO_BIN_EXE_vqro").into());
    tokio::process::Command::new(binary).env_clear().env("HOME",root).env("PATH",std::env::var_os("PATH").unwrap())
        .env("VQRO_STATE_DIR",root.join("st")).env("VQRO_EXTENSIONS_DIR",root.join("extensions"))
        .env("XDG_RUNTIME_DIR",root.join("run")).env("HC_SOCKET",root.join("hc.sock")).env("HC_SESSION","g11-private")
        .env("VQRO_HERDR","[\"/bin/false\"]").env("VQRO_SOCKET",socket).envs(env)
        .args(["call","workspace.apply",&json!({"ops":[{"op":"set","key":key,"value":1}]}).to_string()])
        .output().await.unwrap()
}
async fn hello(socket:&Path,token:&str)->Peer {
    let(peer,_,_)=client::connect(socket,"host-tool-sdk").await.unwrap();
    peer.call("hello",json!({"host_context":token})).await.unwrap();peer
}

#[tokio::test]
async fn two_endpoint_tools_use_public_sdk_environment_and_real_cli_and_retire_truthfully() {
    let root=tempfile::Builder::new().prefix("g11").tempdir_in("/tmp").unwrap();let socket=root.path().join("s");
    let core=server::start(socket.clone(),root.path().join("st"),false,Backend::Fake,None).unwrap();
    let gate=Arc::new(Gate{armed:AtomicBool::new(false),entered:tokio::sync::Notify::new(),released:Mutex::new(false),wake:Condvar::new()});let _release=Release(gate.clone());
    core.lock().unwrap().configure_workspace_writer(gate.writer()).unwrap();
    let(backend,mut requests,_)=client::connect(&socket,"backend").await.unwrap();
    let g=backend.call("provider.register",json!({"name":"native-backend","instance":"one","role":"backend","software":{"name":"fixture","version":"1"},"provider_version":"1","capabilities":{"terminal.spawn":{"env":true}}})).await.unwrap()["generation"].clone();
    backend.call("provider.sync",json!({"generation":g,"resources":[],"final":true})).await.unwrap();
    let(control,_,_)=client::connect(&socket,"control").await.unwrap();
    let pending=tokio::spawn({let control=control.clone();async move{control.call("resource.spawn",json!({"provider":"native-backend","bridge":true})).await}});
    let Some(Incoming::Request{id,params,..})=requests.recv().await else{panic!("native spawn")};
    let bootstrap=params["args"]["env"]["VQRO_BRIDGE_TOKEN"].as_str().unwrap().to_string();
    backend.respond(id,Ok(json!({"outcome":"observed","data":{"facts":{"ref":"host","incarnation":"one","kind":"terminal","state":"live","argv":[],"data":{}}}})));
    let host=pending.await.unwrap().unwrap()["resource"]["id"].as_str().unwrap().to_string();
    let mut config=Config::new("two-endpoint-runtime","stable",serde_json::Map::new());
    config.role="runtime".into();config.token=Some(bootstrap);config.host_context=true;
    let(runtime,_requests)=Provider::connect(tokio::net::UnixStream::connect(&socket).await.unwrap(),config).await.unwrap();
    runtime.sync(["endpoint-a","endpoint-b"].map(|reference|json!({"ref":reference,"incarnation":"one","kind":"agent","state":"live","argv":[],"data":{}})).to_vec(),true).await.unwrap();
    let inventory=control.call("resource.list",json!({"provider":"two-endpoint-runtime"})).await.unwrap();
    let env=runtime.tool_context_env(socket.to_str().unwrap()).unwrap();let token=env["VQRO_HOST_CONTEXT"].clone();
    let bound=hello(&socket,&token).await;
    let wrong=control.call("hello",json!({"host_context":"wrong-context"})).await;
    let mut outputs=Vec::new();
    for endpoint in ["endpoint-a","endpoint-b"] {outputs.push(tool_cli(root.path(),&socket,&env,&format!("doc/g11/{endpoint}")).await);}
    let history=control.call("workspace.history",json!({"limit":10})).await.unwrap();
    // A shell spawned by the host tool is not itself a tool and never inherits the credential.
    let shell=tokio::spawn({let bound=bound.clone();async move{bound.call("resource.spawn",json!({"provider":"native-backend"})).await}});
    let Some(Incoming::Request{id,params,..})=requests.recv().await else{panic!("human shell")};
    let shell_env=params["args"]["env"].clone();
    backend.respond(id,Ok(json!({"outcome":"observed","data":{"facts":{"ref":"human-shell","incarnation":"one","kind":"terminal","state":"live","argv":[],"data":{}}}})));
    shell.await.unwrap().unwrap();
    let(ordinary,_,ordinary_hello)=client::connect(&socket,"ordinary-sdk-no-opt-in").await.unwrap();
    let rights=bound.call("workspace.apply",json!({"on_behalf_of":"endpoint-a","ops":[{"op":"set","key":"doc/no-grant","value":1}]})).await;
    let public=control.call("workspace.snapshot",json!({})).await.unwrap();
    let resume=runtime.resume_token().unwrap().to_string();
    gate.armed.store(true,Ordering::SeqCst);
    let blocker=tokio::spawn({let control=control.clone();async move{control.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/blocker","value":1}]})).await}});
    tokio::time::timeout(Duration::from_secs(2),gate.entered.notified()).await.unwrap();
    let queued=tokio::spawn({let bound=bound.clone();async move{bound.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/queued-expired","value":1}]})).await}});
    tokio::time::timeout(Duration::from_secs(2),async{while core.lock().unwrap().workspace_pending_commits()<2{tokio::task::yield_now().await;}}).await.unwrap();
    runtime.close();
    tokio::time::timeout(Duration::from_secs(2),async{while core.lock().unwrap().providers.get("two-endpoint-runtime").is_some_and(|provider|provider.health.borrow().connected) {tokio::task::yield_now().await;}}).await.unwrap();
    gate.release();blocker.await.unwrap().unwrap();let queued_result=queued.await.unwrap();
    let expired=bound.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/expired","value":1}]})).await;
    let stale_hello=ordinary.call("hello",json!({"host_context":token})).await;
    let fresh=control.call("workspace.get",json!({"key":"doc/expired"})).await.unwrap();
    let queued_entry=control.call("workspace.get",json!({"key":"doc/queued-expired"})).await.unwrap();
    let mut next_config=Config::new("two-endpoint-runtime","stable",serde_json::Map::new());next_config.role="runtime".into();next_config.token=Some(resume);next_config.host_context=true;
    let(next,_next_requests)=Provider::connect(tokio::net::UnixStream::connect(&socket).await.unwrap(),next_config).await.unwrap();
    let next_env=next.tool_context_env(socket.to_str().unwrap()).unwrap();
    let replacement_old=bound.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/old-generation","value":1}]})).await;
    let new_bound=hello(&socket,&next_env["VQRO_HOST_CONTEXT"]).await;
    let new_hello=new_bound.call("workspace.get",json!({"key":"doc/blocker"})).await;
    let next_resume=next.resume_token().unwrap().to_string();
    *gate.released.lock().unwrap()=false;gate.armed.store(true,Ordering::SeqCst);
    let admitted=tokio::spawn({let new_bound=new_bound.clone();async move{new_bound.call("workspace.apply",json!({"ops":[{"op":"set","key":"doc/admitted-before-retirement","value":1}]})).await}});
    tokio::time::timeout(Duration::from_secs(2),gate.entered.notified()).await.unwrap();next.close();
    tokio::time::timeout(Duration::from_secs(2),async{while core.lock().unwrap().providers.get("two-endpoint-runtime").is_some_and(|provider|provider.health.borrow().connected){tokio::task::yield_now().await;}}).await.unwrap();
    gate.release();let admitted_result=admitted.await.unwrap();
    let mut disabled_config=Config::new("two-endpoint-runtime","stable",serde_json::Map::new());disabled_config.role="runtime".into();disabled_config.token=Some(next_resume);
    let(disabled,_disabled_requests)=Provider::connect(tokio::net::UnixStream::connect(&socket).await.unwrap(),disabled_config).await.unwrap();
    let no_implicit_export=disabled.tool_context_env(socket.to_str().unwrap());disabled.close();
    let persisted=std::fs::read(root.path().join("st/workspace.json")).unwrap();
    server::shutdown(&core).await;backend.close();
    assert_eq!(inventory["resources"].as_array().unwrap().len(),2);
    for resource in inventory["resources"].as_array().unwrap(){assert_eq!(resource["host"],host);assert!(resource["data"]["agent"]["caps"]["agent"]["tool_principal"].is_null(),"shared host does NOT claim endpoint env");}
    for output in outputs{assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));}
    for txn in history["txns"].as_array().unwrap().iter().take(2){assert_eq!(txn["actor"],format!("host:{host}"));assert_eq!(txn["principal"],format!("host:{host}"));}
    assert!(shell_env.get("VQRO_HOST_CONTEXT").is_none());assert_eq!(shell_env["VQRO_PRINCIPAL"],"undeclared");
    assert_eq!(ordinary_hello["actor"],"undeclared");assert!(wrong.is_err());assert!(rights.is_err());
    assert_eq!(expired.unwrap_err().kind(),"stale_generation");assert!(stale_hello.is_err());assert!(fresh["value"].is_null());
    assert_eq!(queued_result.unwrap_err().kind(),"stale_generation");assert!(queued_entry.is_null());
    assert_ne!(next_env["VQRO_HOST_CONTEXT"],token);assert_eq!(replacement_old.unwrap_err().kind(),"stale_generation");assert!(new_hello.is_ok());assert!(no_implicit_export.is_none());assert!(admitted_result.is_ok(),"irreversible writer admission is adopted truthfully, never replayed/relabelled after retirement");
    assert!(!public.to_string().contains(&token));assert!(!String::from_utf8_lossy(&persisted).contains(&token));
}
