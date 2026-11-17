//! User-shaped upgrade: native herdr state -> CLI serve/defaults -> prim-herdr's own hc lookup.
#![cfg(unix)]
use std::{path::{Path,PathBuf},process::{Child,Command,Stdio},time::Duration};
use prim::{client,server::{self,Backend}};
use serde_json::{json,Value};
struct Owned(Child);impl Drop for Owned{fn drop(&mut self){let _=self.0.kill();let _=self.0.wait();}}
async fn peer(socket:&Path)->prim::rpc::Peer{client::connect(socket,"upgrade-test").await.unwrap().0}
async fn resource(p:&prim::rpc::Peer)->Value{tokio::time::timeout(Duration::from_secs(5),async{loop{let s=p.call("resource.list",Value::Null).await.unwrap();if let Some(r)=s["resources"].as_array().unwrap().first(){break r.clone()}tokio::time::sleep(Duration::from_millis(10)).await}}).await.unwrap()}
#[tokio::test]
async fn only_unchanged_installed_default_program_paths_are_unpinned() {
 let root=tempfile::Builder::new().prefix("pup").tempdir_in("/tmp").unwrap();let socket=root.path().join("s");let state=root.path().join("st");
 let core=server::start(socket.clone(),state.clone(),true,Backend::Extensions,None).unwrap();let p=peer(&socket).await;
 let defaults:Value=serde_json::from_str(include_str!("../extensions/defaults.json")).unwrap();let mut ops=Vec::new();
 for name in ["herdr","tmux"] {let spec=defaults["extensions"].as_array().unwrap().iter().find(|s|s["name"]==name).unwrap();let mut value=json!({"argv":spec["argv"]});for field in ["description","kind","enabled","default"]{if !spec[field].is_null(){value[field]=spec[field].clone();}}value["argv"][0]=json!(format!("/old-install/prim-{name}"));if name=="tmux"{value["description"]=json!("user customized");}ops.push(json!({"op":"set","key":format!("extension/{name}"),"value":value}));}
 ops.push(json!({"op":"set","key":"doc/prim/defaults","value":{"version":2,"installed":["herdr","tmux"]}}));p.call("workspace.apply",json!({"ops":ops})).await.unwrap();server::shutdown(&core).await;
 let next=server::start(socket,state,true,Backend::Extensions,Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("extensions"))).unwrap();let snap=next.lock().unwrap().snapshot();
 assert_eq!(snap["entries"]["extension/herdr"]["value"]["argv"][0],"prim-herdr");assert_eq!(snap["entries"]["extension/tmux"]["value"]["argv"][0],"/old-install/prim-tmux");
 server::shutdown(&next).await;
}

#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn v1_native_state_upgrades_via_defaults_and_unconfigured_prim_herdr() {
    use std::os::unix::fs::{PermissionsExt,symlink};
    let root=tempfile::Builder::new().prefix("pu").tempdir_in("/tmp").unwrap();let state=root.path().join("st");let socket=root.path().join("p.sock");let bin=root.path().join("bin");let ext=root.path().join("ext");
    std::fs::create_dir(&bin).unwrap();std::fs::create_dir(&ext).unwrap();
    let hc=bin.join("hc");std::fs::copy(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/upgrade_hc.py"),&hc).unwrap();std::fs::set_permissions(&hc,std::fs::Permissions::from_mode(0o700)).unwrap();
    symlink("/usr/bin/python3",bin.join("python3")).unwrap();
    for entry in std::fs::read_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("extensions")).unwrap(){let entry=entry.unwrap();if entry.path().is_file(){std::fs::copy(entry.path(),ext.join(entry.file_name())).unwrap();}}
    let old=server::start(socket.clone(),state.clone(),false,Backend::Herdr(vec![hc.to_string_lossy().into(),"connect".into()]),None).unwrap();
    let p=peer(&socket).await;let before=resource(&p).await;let id=before["id"].clone();
    let layout=json!({"tabs":[{"title":"kept","node":{"view":"kept"}}],"active":0});
    p.call("workspace.apply",json!({"ops":[{"op":"set","key":"view/kept","value":{"kind":"terminal","resource":id}},
        {"op":"set","key":"layout/main","value":layout},{"op":"set","key":"workspace/active","value":{"layout":"main"}},
        {"op":"set","key":"doc/prim/defaults","value":{"installed":["tabs","agents"],"from":"v1"}}]})).await.unwrap();
    server::shutdown(&old).await;
    let mut child=Owned(Command::new(env!("CARGO_BIN_EXE_prim")).arg("serve").env_clear().env("HOME",root.path()).env("PATH",&bin)
        .env("PRIM_STATE_DIR",&state).env("PRIM_SOCKET",&socket).env("PRIM_EXTENSIONS_DIR",&ext).env("HC_SOCKET",root.path().join("never-live-hc.sock")).env("HC_SESSION","upgrade-fixture")
        .stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
    let after=tokio::time::timeout(Duration::from_secs(8),async{loop{if let Ok((p,_,_))=client::connect(&socket,"upgrade-test").await{break p}tokio::time::sleep(Duration::from_millis(10)).await}}).await.unwrap();
    let observed=resource(&after).await;assert_eq!(observed["id"],id);assert_eq!(observed["provider"],"herdr");
    let snap=after.call("workspace.snapshot",Value::Null).await.unwrap();assert_eq!(snap["entries"]["layout/main"]["value"],layout);
    assert_eq!(snap["entries"]["extension/herdr"]["value"]["argv"][0],"prim-herdr","logical program names must not pin the build directory");
    assert!(snap["entries"]["extension/pi"].is_object(),"v1 gains the new Pi launcher");
    tokio::time::timeout(Duration::from_secs(5),async{loop{let snapshot=after.call("workspace.snapshot",Value::Null).await.unwrap();if snapshot["entries"]["menu/pi.new"]["value"]["command"]=="pi.new"{break;}tokio::time::sleep(Duration::from_millis(10)).await;}}).await.expect("migrated Pi extension must restore the New Pi agent action");
    let spawned=after.call("resource.spawn",json!({"argv":["sh"]})).await.unwrap();assert_eq!(spawned["resource"]["provider"],"herdr");assert_eq!(spawned["resource"]["backend_ref"],"t2");
    after.call("workspace.apply",json!({"ops":[{"op":"delete","key":"extension/pi"}]})).await.unwrap();
    after.call("core.shutdown",Value::Null).await.unwrap();tokio::time::timeout(Duration::from_secs(5),async{while child.0.try_wait().unwrap().is_none(){tokio::time::sleep(Duration::from_millis(10)).await}}).await.unwrap();
    let restarted=server::start(socket.clone(),state,true,Backend::Extensions,Some(ext)).unwrap();
    assert!(restarted.lock().unwrap().snapshot()["entries"]["extension/pi"].is_null(),"a removed migrated Pi launcher stays removed");
    server::shutdown(&restarted).await;
}
