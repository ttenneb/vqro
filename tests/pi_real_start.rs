//! Opt-in real installed Pi1 + public no-network fake provider. Not part of ordinary cargo test.
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/native.rs"] mod native;
#[allow(dead_code)] #[path="../src/bin/vqro-bridge-pi/admission.rs"] mod admission;
use admission::{Admission,Signals,Starts};
use native::Native;
use serde_json::{json,Value};
use std::{collections::HashMap,path::PathBuf,sync::{Arc,Mutex},time::Duration};
use tokio::time::Instant;
struct Stop(Native);impl Drop for Stop{fn drop(&mut self){self.0.stop();}}
fn rows(root:&std::path::Path)->Vec<Value>{std::fs::read_to_string(root.join("native.jsonl")).unwrap_or_default().lines().filter_map(|line|serde_json::from_str(line).ok()).collect()}
async fn witnessed(root:&std::path::Path,kind:&str){tokio::time::timeout(Duration::from_secs(10),async{loop{if rows(root).iter().any(|row|row["kind"]==kind){break;}tokio::time::sleep(Duration::from_millis(5)).await;}}).await.unwrap_or_else(|_|panic!("missing native {kind}: {:?}",rows(root)));}
#[tokio::test]
#[ignore="opt-in installed Pi1; run in env-i/private user+network namespace with A1_REAL_EVIDENCE"]
async fn installed_pi_serializes_preflight_without_replay(){
    assert!(std::env::vars().all(|(key,_)|!key.starts_with("HERDR_")));
    let root=PathBuf::from(std::env::var("A1_REAL_EVIDENCE").expect("explicit private evidence root"));std::fs::create_dir_all(&root).unwrap();
    assert!(!root.join("native.jsonl").exists(),"fresh private run required");
    for name in ["home","agent","tmp","work","sessions"]{std::fs::create_dir_all(root.join(name)).unwrap();}
    std::fs::write(root.join("agent/settings.json"),json!({"retry":{"enabled":false},"compaction":{"enabled":false}}).to_string()).unwrap();
    let package=PathBuf::from(std::env::var("A1_PI_PACKAGE").expect("explicit installed package path"));let cli=package.join("dist/bundle/cli.js");let fixture=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi_real_barrier.ts");
    let version:Value=serde_json::from_slice(&std::fs::read(package.join("package.json")).unwrap()).unwrap();assert!(["1.5.2","0.87.0"].iter().any(|v|version["version"]==*v));let old_pi=version["version"]=="0.87.0";
    let argv=vec!["/usr/bin/node".into(),cli.to_string_lossy().into_owned(),"--mode".into(),"rpc".into(),"--no-session".into(),"--no-extensions".into(),"--no-skills".into(),"--no-prompt-templates".into(),"--no-themes".into(),"--no-tools".into(),"--extension".into(),fixture.to_string_lossy().into_owned(),"--provider".into(),"a1-offline".into(),"--model".into(),"barrier".into(),"--thinking".into(),"off".into()];
    let env=HashMap::from([("HOME".into(),root.join("home").to_string_lossy().into_owned()),("PATH".into(),"/usr/bin:/bin".into()),("PI_CODING_AGENT_DIR".into(),root.join("agent").to_string_lossy().into_owned()),("A1_REAL_ROOT".into(),root.to_string_lossy().into_owned()),("TMPDIR".into(),root.join("tmp").to_string_lossy().into_owned()),("NODE_COMPILE_CACHE".into(),root.join("tmp/node-cache").to_string_lossy().into_owned()),("PI_SKIP_VERSION_CHECK".into(),"1".into())]);
    let signals=Signals::new();let observed=signals.clone();let native=Native::spawn(&argv,&root.join("work"),&env,Arc::new(move|event|observed.observe(&event))).await.unwrap();let _stop=Stop(native.clone());
    let initial=native.call("get_state",json!({}),Duration::from_secs(10)).await.unwrap();assert!(initial.success);witnessed(&root,"loaded").await;
    let starts=Starts::new(native.clone(),signals.clone());let receipts=Arc::new(Mutex::new(Vec::new()));let record=receipts.clone();let first=starts.clone();
    let first=tokio::spawn(async move{first.start(json!({"message":"A1_REAL_PREFLIGHT"}),Instant::now()+Duration::from_secs(8),Arc::new(move|id,reply|record.lock().unwrap().push((id,reply)))).await});
    witnessed(&root,"preflight").await;let idle=native.call("get_state",json!({}),Duration::from_secs(2)).await.unwrap();assert!(idle.data["isStreaming"].is_boolean());assert_eq!(idle.data["isCompacting"],false);
    if old_pi{assert_eq!(idle.data["isStreaming"],false);}
    for after_abort in [false,true].into_iter().take(if old_pi{2}else{1}){
        if after_abort{assert!(native.call("abort",json!({}),Duration::from_secs(2)).await.unwrap().success);}
        let second=starts.start(json!({"message":"A1_REAL_SECOND_MUST_NOT_WRITE"}),Instant::now()+Duration::from_secs(1),Arc::new(|_,_|{})).await;
        assert!(matches!(second,Admission::Refused(error) if error.kind=="busy"));
    }
    std::fs::write(root.join("release"),"").unwrap();assert!(matches!(first.await.unwrap(),Admission::Acknowledged{reply,..} if reply.success));witnessed(&root,"model").await;witnessed(&root,"agent_settled").await;
    let trace=rows(&root);let inputs:Vec<_>=trace.iter().filter(|row|row["kind"]=="input").collect();assert_eq!(inputs.len(),1);assert_eq!(inputs[0]["text"],"A1_REAL_PREFLIGHT");assert_eq!(trace.iter().filter(|row|row["kind"]=="model").count(),1);assert_eq!(receipts.lock().unwrap().len(),1);
    native.stop();tokio::time::timeout(Duration::from_secs(3),native.exited()).await.unwrap();
    let receipt=json!({"status":"pass","runtime_version":version["version"],"native_metadata_during_preflight":{"streaming":idle.data["isStreaming"],"compacting":idle.data["isCompacting"]},"native_inputs":1,"fake_model_calls":1,"paid_calls":0,"second_starts_rejected_before_write":if old_pi{2}else{1},"abort_ack_did_not_unlock":if old_pi{json!(true)}else{Value::Null},"network_namespace":std::fs::read_link("/proc/self/ns/net").unwrap().to_string_lossy(),"process_exited":true,"scope":"product Native/Starts component on exact installed Pi version; not a whole-adapter certificate"});
    std::fs::write(root.join("receipt.json"),serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
}
