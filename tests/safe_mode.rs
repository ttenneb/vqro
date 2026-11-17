//! Session recovery choices never persist enabled edits and safe mode starts no provider.
use prim::{client,rpc::RpcError,server::{self,Backend}};
use serde_json::{json,Value};
use std::time::Duration;
#[tokio::test]
async fn missing_provider_is_visible_and_safe_mode_preserves_failure_diagnostics(){
    let dir=tempfile::Builder::new().prefix("psd").tempdir_in("/tmp").unwrap();let socket=dir.path().join("s");
    let core=server::start(socket.clone(),dir.path().join("st"),false,Backend::Extensions,None).unwrap();let(p,_,_)=client::connect(&socket,"diagnostic-test").await.unwrap();
    p.call("workspace.apply",json!({"ops":[{"op":"set","key":"extension/missing","value":{"argv":["prim-qa3-nonexistent-program"],"kind":"provider","default":true}}]})).await.unwrap();
    let before=p.call("workspace.snapshot",Value::Null).await.unwrap();assert_eq!(before["extensions"]["missing"]["status"],"failed");assert!(before["backend_notice"].as_str().unwrap().contains("No default backend"));
    p.call("command.invoke",json!({"name":"prim.safe_mode","args":{"on":true}})).await.unwrap();
    let after=p.call("workspace.snapshot",Value::Null).await.unwrap();assert_eq!(after["extensions"]["missing"]["status"],"failed");assert_eq!(before["extensions"]["missing"]["last_exit"],after["extensions"]["missing"]["last_exit"]);
    server::shutdown(&core).await;
}

async fn until(mut ready:impl FnMut()->bool) { tokio::time::timeout(Duration::from_secs(3),async{while !ready(){tokio::time::sleep(Duration::from_millis(10)).await}}).await.unwrap(); }
#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn safe_session_is_empty_allowances_are_temporary_and_provider_links_are_fenced() {
    let dir=tempfile::Builder::new().prefix("psm").tempdir_in("/tmp").unwrap(); let socket=dir.path().join("s");let state=dir.path().join("st");
    let core=server::start(socket.clone(),state.clone(),true,Backend::Fake,None).unwrap();
    let (control,_rx,_)=client::connect(&socket,"safe-test").await.unwrap();
    assert!(control.call("workspace.snapshot",Value::Null).await.unwrap()["providers"].as_object().unwrap().is_empty());
    let fixture=std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_provider.py");
    control.call("workspace.apply",json!({"ops":[{"op":"set","key":"extension/recovery","value":{"kind":"provider","description":"Private fixture","enabled":false,"argv":["python3",fixture,"--socket",socket,"--name","wire"]}}]})).await.unwrap();
    control.call("command.invoke",json!({"name":"prim.safe_mode.allow","args":{"name":"recovery","on":true}})).await.unwrap();
    until(||core.lock().unwrap().exts["recovery"].running).await;
    let snap=control.call("workspace.snapshot",Value::Null).await.unwrap();
    assert_eq!(snap["safe_mode_allow"],json!(["recovery"])); assert_eq!(snap["entries"]["extension/recovery"]["value"]["enabled"],false);
    let pid=snap["extensions"]["recovery"]["pid"].clone();
    control.call("command.invoke",json!({"name":"prim.safe_mode.allow","args":{"name":"recovery","on":true}})).await.unwrap();
    assert_eq!(control.call("workspace.snapshot",Value::Null).await.unwrap()["extensions"]["recovery"]["pid"],pid,"allow is idempotent, not a provider restart");
    let (provider,_rx,_)=client::connect(&socket,"provider-test").await.unwrap();
    let registration=json!({"name":"forged","role":"backend","instance":"fixture","provider_version":"1","software":{"name":"fixture","version":"1"},"capabilities":{}});
    assert_eq!(provider.call("provider.register",registration.clone()).await.unwrap_err().kind(),"forbidden");
    provider.call("hello",json!({"extension":"recovery"})).await.unwrap();
    assert_eq!(provider.call("provider.register",registration).await.unwrap_err().kind(),"forbidden","self-declared hello identity is not supervision proof");
    until(||core.lock().unwrap().providers.get("wire").is_some_and(|p|p.health.borrow().connected)).await;
    assert!(control.call("workspace.snapshot",Value::Null).await.unwrap()["provider_extensions"].as_array().unwrap().contains(&json!("recovery")));
    control.call("command.invoke",json!({"name":"prim.safe_mode.allow","args":{"name":"recovery","on":false}})).await.unwrap();
    until(||!core.lock().unwrap().providers["wire"].health.borrow().connected).await;
    assert!(!core.lock().unwrap().exts["recovery"].running);
    server::shutdown(&core).await;
    let restarted=server::start(socket.clone(),state,true,Backend::Fake,None).unwrap();
    assert!(restarted.lock().unwrap().safe_mode_allow.is_empty()); assert!(restarted.lock().unwrap().providers.is_empty());
    server::shutdown(&restarted).await;
}
#[tokio::test]
async fn entering_safe_mode_fences_explicit_embedder_adapter_without_a_legacy_toggle() {
    let dir=tempfile::Builder::new().prefix("psn").tempdir_in("/tmp").unwrap();let socket=dir.path().join("s");
    let core=server::start(socket.clone(),dir.path().join("st"),false,Backend::Fake,None).unwrap();
    let (peer,_rx,_)=client::connect(&socket,"safe-test").await.unwrap();
    until(||core.lock().unwrap().providers.get("fake").is_some_and(|p|p.info.is_some())).await;
    let resource=peer.call("resource.spawn",json!({"argv":["sh"]})).await.unwrap()["resource"]["id"].as_str().unwrap().to_string();
    peer.call("command.invoke",json!({"name":"prim.safe_mode","args":{"on":true}})).await.unwrap();
    assert!(!core.lock().unwrap().resources[&resource].available);
    let error:RpcError=peer.call("resource.spawn",json!({"argv":["sh"]})).await.unwrap_err();assert_eq!(error.kind(),"unavailable");
    peer.call("command.invoke",json!({"name":"prim.safe_mode","args":{"on":false}})).await.unwrap();
    until(||core.lock().unwrap().providers["fake"].health.borrow().connected).await;
    peer.call("resource.spawn",json!({"argv":["sh"]})).await.unwrap();
    server::shutdown(&core).await;
}
