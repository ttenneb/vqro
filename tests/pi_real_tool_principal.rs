//! Opt-in actual Pi1 runtime -> native bash tool -> CLI -> private C2 core identity proof.
//! File-only public fake model; not a whole controlled-launch adapter certificate.
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/native.rs"] mod native;
use native::{Event,Native};
use serde_json::{json,Value};
use std::{collections::HashMap,path::{Path,PathBuf},sync::{Arc,atomic::{AtomicBool,Ordering}},time::Duration};
struct Stop(Native);impl Drop for Stop{fn drop(&mut self){self.0.stop();}}
async fn call(binary:&Path,env:&HashMap<String,String>,method:&str,params:Value)->Value{
    let result=tokio::time::timeout(Duration::from_secs(5),tokio::process::Command::new(binary).env_clear().envs(env).args(["call",method,&params.to_string()]).output()).await.unwrap().unwrap();
    assert!(result.status.success(),"private CLI failed: {}",String::from_utf8_lossy(&result.stderr));serde_json::from_slice(&result.stdout).unwrap()
}
fn quote(value:&str)->String{format!("'{}'",value.replace('\'',"'\\''"))}
#[tokio::test]
#[ignore="opt-in installed Pi1; env-i/private user+network namespace, explicit A1_REAL_EVIDENCE/A1_TOOL_CORE_BINARY"]
async fn installed_pi_bash_cli_keeps_endpoint_principal_not_owner_or_label(){
    assert!(std::env::vars().all(|(key,_)|!key.starts_with("HERDR_")));
    assert_ne!(std::fs::read_link("/proc/self/ns/net").unwrap().to_string_lossy(),std::env::var("A1_PARENT_NET").unwrap());
    let root=PathBuf::from(std::env::var("A1_REAL_EVIDENCE").unwrap());std::fs::create_dir_all(&root).unwrap();assert!(!root.join("native.jsonl").exists(),"fresh private root required");
    for name in ["home","agent","tmp","work","core","empty","config","state"]{std::fs::create_dir_all(root.join(name)).unwrap();}
    std::fs::write(root.join("agent/settings.json"),json!({"retry":{"enabled":false,"provider":{"maxRetries":0}},"compaction":{"enabled":false},"cacheWarming":"off"}).to_string()).unwrap();
    let binary=PathBuf::from(std::env::var("A1_TOOL_CORE_BINARY").unwrap());let socket=root.join("c.sock");assert!(socket.as_os_str().len()<103);
    let core_env=HashMap::from([("HOME".into(),root.join("home").to_string_lossy().into_owned()),("PATH".into(),"/usr/bin:/bin".into()),("VQRO_STATE_DIR".into(),root.join("core").to_string_lossy().into_owned()),("VQRO_SOCKET".into(),socket.to_string_lossy().into_owned()),("VQRO_EXTENSIONS_DIR".into(),root.join("empty").to_string_lossy().into_owned()),("XDG_CONFIG_HOME".into(),root.join("config").to_string_lossy().into_owned()),("XDG_STATE_HOME".into(),root.join("state").to_string_lossy().into_owned())]);
    let log=std::fs::File::create(root.join("core.log")).unwrap();let mut core=tokio::process::Command::new(&binary).args(["serve","--fake"]).env_clear().envs(&core_env).current_dir(root.join("work")).stdout(log.try_clone().unwrap()).stderr(log).kill_on_drop(true).spawn().unwrap();
    tokio::time::timeout(Duration::from_secs(5),async{while !socket.exists(){assert!(core.try_wait().unwrap().is_none());tokio::time::sleep(Duration::from_millis(5)).await;}}).await.unwrap();
    let owner=call(&binary,&core_env,"workspace.get",json!({"key":"doc/principals/owner"})).await;assert_eq!(owner["value"],"owner");
    let principal="p:a1-native-tool";let note="A1 installed bash tool CLI attribution";
    let write=json!({"label":"owner","note":note,"ops":[{"op":"set","key":"doc/test/native-tool-principal","value":{"proof":"A1_G11"}}]});
    let command=format!("{} call workspace.apply {}",quote(binary.to_str().unwrap()),quote(&write.to_string()));
    let package=PathBuf::from(std::env::var("A1_PI_PACKAGE").unwrap());let version:Value=serde_json::from_slice(&std::fs::read(package.join("package.json")).unwrap()).unwrap();assert_eq!(version["version"],"1.5.2");
    let fixture=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi_real_tool_principal.ts");
    let argv=vec!["/usr/bin/node".into(),package.join("dist/bundle/cli.js").to_string_lossy().into_owned(),"--mode".into(),"rpc".into(),"--no-session".into(),"--no-extensions".into(),"--no-skills".into(),"--no-prompt-templates".into(),"--no-themes".into(),"--extension".into(),fixture.to_string_lossy().into_owned(),"--tools".into(),"bash".into(),"--provider".into(),"a1-tool-offline".into(),"--model".into(),"principal".into(),"--thinking".into(),"off".into()];
    let env=HashMap::from([("HOME".into(),root.join("home").to_string_lossy().into_owned()),("PATH".into(),"/usr/bin:/bin".into()),("PI_CODING_AGENT_DIR".into(),root.join("agent").to_string_lossy().into_owned()),("A1_REAL_ROOT".into(),root.to_string_lossy().into_owned()),("A1_TOOL_COMMAND".into(),command),("TMPDIR".into(),root.join("tmp").to_string_lossy().into_owned()),("NODE_COMPILE_CACHE".into(),root.join("tmp/node-cache").to_string_lossy().into_owned()),("PI_SKIP_VERSION_CHECK".into(),"1".into()),("VQRO_SOCKET".into(),socket.to_string_lossy().into_owned()),("VQRO_PRINCIPAL".into(),principal.into()),("VQRO_BRIDGE_TOKEN".into(),"must-not-inherit".into()),("HERDR_FORBIDDEN".into(),"must-not-inherit".into())]);
    let ended=Arc::new(AtomicBool::new(false));let observed=ended.clone();let native=Native::spawn(&argv,&root.join("work"),&env,Arc::new(move|event|{if matches!(event,Event::Record(row) if row["type"]=="agent_end"){observed.store(true,Ordering::Release);}})).await.unwrap();let _stop=Stop(native.clone());
    assert!(native.call("get_state",json!({}),Duration::from_secs(10)).await.unwrap().success);
    assert!(native.call("prompt",json!({"message":"Run the one explicit local principal probe."}),Duration::from_secs(15)).await.unwrap().success);
    tokio::time::timeout(Duration::from_secs(15),async{while !ended.load(Ordering::Acquire){tokio::time::sleep(Duration::from_millis(5)).await;}}).await.unwrap();
    let history=call(&binary,&core_env,"workspace.history",json!({"limit":20})).await;
    let writes:Vec<_>=history["txns"].as_array().unwrap().iter().filter(|txn|txn["note"]==note).collect();assert_eq!(writes.len(),1,"one explicit native tool command, never resent");
    let txn=writes[0];assert_eq!(txn["principal"],principal);assert_eq!(txn["actor"],principal);assert_eq!(txn["label"],"owner");assert_ne!(txn["principal"],owner["value"]);assert!(txn["changes"].as_array().unwrap().iter().any(|change|change["key"]=="doc/test/native-tool-principal"));
    let trace:Vec<Value>=std::fs::read_to_string(root.join("native.jsonl")).unwrap().lines().map(|line|serde_json::from_str(line).unwrap()).collect();assert_eq!(trace.iter().filter(|row|row["kind"]=="model").count(),2);
    native.stop();tokio::time::timeout(Duration::from_secs(3),native.exited()).await.unwrap();core.kill().await.unwrap();core.wait().await.unwrap();
    let receipt=json!({"status":"pass","runtime_version":"1.5.2","principal":principal,"core_principal":txn["principal"],"core_actor":txn["actor"],"asserted_label":txn["label"],"native_tool_cli_writes":1,"fake_model_calls":2,"paid_calls":0,"native_exited":true,"private_core_exited":true,"scope":"product Native filter + actual installed Pi RPC/bash tool + actual CLI + private C2 core; explicit fake-provider extension, not the controlled-launch whole-adapter profile"});
    std::fs::write(root.join("receipt.json"),serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
}
