//! Pi-native transport tests: real private child, no installed runtime, credentials or model.
#[path = "../src/bin/vqro-bridge-pi/native.rs"]
mod native;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use native::{Event, Native, MAX_RECORD};
use serde_json::{json,Value};
use tokio::time::Instant;

const CHILD: &str = r#"
import json,os,subprocess,sys,time
sys.stderr.write('e'*100000)
sys.stderr.flush()
for line in sys.stdin:
    row=json.loads(line)
    with open(os.environ['PI_FIXTURE_LEDGER'],'a') as f: f.write(json.dumps(row)+'\n')
    kind=row['type']
    if kind=='hang': continue
    if kind=='extension_ui_response': continue
    if kind=='bad': print('{bad',flush=True); continue
    if kind=='big': print('x'*1100000,flush=True); continue
    if kind=='closed': os.close(1); time.sleep(60); continue
    if kind=='die': sys.exit(0)
    if kind=='split_exit':
        data=json.dumps({'type':'response','id':row['id'],'command':kind,'success':True,'data':{'complete':True}})+'\n'
        middle=len(data)//2
        sys.stdout.write(data[:middle]);sys.stdout.flush()
        code="import pathlib,sys,time;r=pathlib.Path(sys.argv[1]);end=time.monotonic()+3\nwhile not r.exists() and r.parent.exists() and time.monotonic()<end: time.sleep(.01)\nif r.exists(): sys.stdout.write(sys.argv[2]);sys.stdout.flush()"
        subprocess.Popen([sys.executable,'-u','-c',code,os.environ['PI_FIXTURE_RELEASE'],data[middle:]],stdin=subprocess.DEVNULL)
        sys.exit(0)
    if kind=='delayed': time.sleep(.2)
    if kind=='get_state': data={'sessionId':'s1','isStreaming':False}
    elif kind=='get_commands': data={'commands':[{'name':'fixture','description':'native'}]}
    elif kind=='array_commands': data=[{'name':'fixture'}]
    elif kind=='get_available_models': data={'models':[{'id':'fake','provider':'offline'}]}
    elif kind=='probe_env': data={'home':os.environ['HOME'],'herdr':any(k.startswith('HERDR_') for k in os.environ),'supervision':any(k.startswith('VQRO_') and k not in ('VQRO_SOCKET','VQRO_PRINCIPAL') for k in os.environ),'principal':os.environ.get('VQRO_PRINCIPAL'),'socket':os.environ.get('VQRO_SOCKET')}
    else: data={}
    command='wrong' if kind=='mismatch' else kind
    print(json.dumps({'type':'response','id':row['id'],'command':command,'success':True,'data':data}),flush=True)
    if kind=='pause_read': time.sleep(10)
"#;

struct Fixture {
    root: tempfile::TempDir,
    native: Native,
    events: Arc<Mutex<Vec<Event>>>,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        let env = HashMap::from([
            ("HOME".into(),root.path().join("home").to_string_lossy().into_owned()),
            ("PATH".into(),"/usr/bin:/bin".into()),
            ("PI_FIXTURE_LEDGER".into(),root.path().join("native.jsonl").to_string_lossy().into_owned()),
            ("PI_FIXTURE_RELEASE".into(),root.path().join("release").to_string_lossy().into_owned()),
            ("HERDR_FAKE".into(),"must-not-reach-child".into()),
            ("VQRO_BRIDGE_TOKEN".into(),"must-not-reach-child".into()),
            ("VQRO_EXTENSION_TOKEN".into(),"must-not-reach-child".into()),
            ("VQRO_PRINCIPAL".into(),"p:fixture-agent".into()),
            ("VQRO_SOCKET".into(),root.path().join("v.sock").to_string_lossy().into_owned()),
        ]);
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = events.clone();
        let native = Native::spawn(&["/usr/bin/python3".into(),"-u".into(),"-c".into(),CHILD.into()],root.path(),&env,
            Arc::new(move |event|observed.lock().unwrap().push(event))).await.unwrap();
        Self { root,native,events }
    }
    fn ledger(&self) -> Vec<Value> {
        std::fs::read_to_string(self.root.path().join("native.jsonl")).unwrap_or_default().lines().map(|line|serde_json::from_str(line).unwrap()).collect()
    }
    async fn close(&self) {
        self.native.stop();
        let exit = tokio::time::timeout(Duration::from_secs(2),self.native.exited()).await.unwrap();
        assert!(!exit.success);
        assert!(!self.native.alive());
    }
}

#[tokio::test]
async fn native_metadata_shapes_missing_fields_and_private_environment() {
    let f = Fixture::new().await;
    let reply = f.native.call("get_state",json!({}),Duration::from_secs(2)).await.unwrap();
    assert!(reply.success);
    assert!(reply.error.is_null());
    assert_eq!(reply.data["sessionId"],"s1");
    assert!(reply.data.get("isCompacting").is_none(),"missing optional metadata is not a transport failure");
    assert!(f.native.call("get_commands",json!({}),Duration::from_secs(2)).await.unwrap().data["commands"].is_array());
    assert!(f.native.call("array_commands",json!({}),Duration::from_secs(2)).await.unwrap().data.is_array());
    assert!(f.native.call("get_available_models",json!({}),Duration::from_secs(2)).await.unwrap().data["models"].is_array());
    let env = f.native.call("probe_env",json!({}),Duration::from_secs(2)).await.unwrap().data;
    assert_eq!(env["herdr"],false); assert_eq!(env["supervision"],false);
    assert_eq!(env["principal"],"p:fixture-agent");assert_eq!(env["socket"],f.root.path().join("v.sock").to_string_lossy().as_ref());
    assert_eq!(env["home"],f.root.path().join("home").to_string_lossy().as_ref());
    let start = Instant::now();
    while f.native.stderr_tail().1 < 100_000 && start.elapsed() < Duration::from_secs(2) { tokio::time::sleep(Duration::from_millis(5)).await; }
    let (tail,total) = f.native.stderr_tail();
    assert_eq!(total,100_000); assert_eq!(tail.len(),65_536);
    assert!(f.events.lock().unwrap().iter().any(|event|matches!(event,Event::Record(record) if record["type"]=="response")));
    f.close().await;
}

#[tokio::test]
async fn no_ack_does_not_block_control_queries_or_replay_native_input() {
    let f = Fixture::new().await;
    let error = f.native.call("hang",json!({"message":"once"}),Duration::from_millis(80)).await.unwrap_err();
    assert_eq!(error.kind,"timeout");
    assert!(error.message.contains("never resent"));
    assert!(error.may_have_acted);
    assert_eq!(f.native.pending(),0);
    let mut ticket = f.native.submit("get_state",json!({}),Instant::now()+Duration::from_secs(2)).unwrap();
    assert!(ticket.id.starts_with('n'));
    assert!(ticket.reply().await.unwrap().success);
    assert!(ticket.write_begun()); assert!(ticket.written());
    assert_eq!(f.ledger().iter().filter(|record|record["type"]=="hang").count(),1);
    f.native.notify("extension_ui_response",json!({"id":"widget","value":true}),Instant::now()+Duration::from_secs(2)).await.unwrap();
    // A following acknowledged query fences receipt of the unacknowledged native notice.
    f.native.call("get_state",json!({}),Duration::from_secs(2)).await.unwrap();
    assert_eq!(f.ledger().iter().filter(|record|record["type"]=="extension_ui_response").count(),1);
    f.close().await;
}

#[tokio::test]
async fn late_reply_is_not_a_retry_or_a_second_native_write() {
    let f = Fixture::new().await;
    assert_eq!(f.native.call("delayed",json!({}),Duration::from_millis(40)).await.unwrap_err().kind,"timeout");
    f.native.call("get_state",json!({}),Duration::from_secs(2)).await.unwrap();
    assert_eq!(f.native.pending(),0);
    assert_eq!(f.ledger().iter().filter(|record|record["type"]=="delayed").count(),1);
    f.close().await;
}

#[tokio::test]
async fn oversized_and_expired_native_submissions_do_not_write() {
    let f = Fixture::new().await;
    let huge = f.native.submit("prompt",json!({"message":"x".repeat(MAX_RECORD)}),Instant::now()+Duration::from_secs(2)).err().unwrap();
    assert_eq!(huge.kind,"too_large"); assert!(!huge.may_have_acted);
    let expired = f.native.submit("prompt",json!({"message":"expired"}),Instant::now()).err().unwrap();
    assert_eq!(expired.kind,"timeout"); assert!(!expired.may_have_acted);
    f.native.call("get_state",json!({}),Duration::from_secs(2)).await.unwrap();
    assert!(f.ledger().iter().all(|record|record["type"]!="prompt"));
    f.close().await;
}

#[tokio::test]
async fn queued_ticket_cancel_is_definite_but_started_write_is_not() {
    let f = Fixture::new().await;
    f.native.call("pause_read",json!({}),Duration::from_secs(2)).await.unwrap();
    let big = f.native.submit("prompt",json!({"message":"x".repeat(900_000)}),Instant::now()+Duration::from_secs(3)).unwrap();
    let start = Instant::now();
    while !big.write_begun() && start.elapsed()<Duration::from_secs(1) { tokio::time::sleep(Duration::from_millis(1)).await; }
    assert!(big.write_begun()); assert!(!big.cancel_before_write());
    let small = f.native.submit("prompt",json!({"message":"cancel-before-write"}),Instant::now()+Duration::from_secs(2)).unwrap();
    assert!(small.cancel_before_write());
    assert!(!small.write_begun());
    f.close().await;
    assert_eq!(f.ledger().len(),1,"neither buffered prompt reached the paused reader");
}

#[tokio::test]
async fn malformed_oversized_or_mismatched_native_records_end_owned_runtime() {
    for command in ["bad","big","mismatch","closed"] {
        let f = Fixture::new().await;
        assert!(f.native.call(command,json!({}),Duration::from_secs(2)).await.is_err());
        let _ = tokio::time::timeout(Duration::from_secs(2),f.native.exited()).await.unwrap();
        assert!(!f.native.alive());
        assert!(f.events.lock().unwrap().iter().any(|event|matches!(event,Event::ProtocolError(message) if !message.is_empty())));
    }
}

#[tokio::test]
async fn inherited_pipe_does_not_hide_exit_or_lose_partial_framing() {
    let f = Fixture::new().await;
    let mut ticket = f.native.submit("split_exit",json!({}),Instant::now()+Duration::from_secs(2)).unwrap();
    // The descendant holds stdout and the second half until AFTER the process-exit observation.
    let exit = tokio::time::timeout(Duration::from_secs(2),f.native.exited()).await.unwrap();
    assert!(exit.success);
    assert!(!f.root.path().join("release").exists());
    std::fs::write(f.root.path().join("release"),b"release").unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(2),ticket.reply()).await.unwrap().unwrap();
    assert!(reply.success); assert_eq!(reply.data["complete"],true);
    assert_eq!(f.native.pending(),0);
}

#[tokio::test]
async fn all_active_pending_bound_refuses_without_native_write() {
    let f = Fixture::new().await;
    let tickets: Vec<_> = (0..32).map(|_|f.native.submit("hang",json!({}),Instant::now()+Duration::from_secs(3)).unwrap()).collect();
    let error = f.native.submit("extra",json!({}),Instant::now()+Duration::from_secs(3)).err().unwrap();
    assert_eq!(error.kind,"overloaded");
    f.close().await;
    assert!(f.ledger().iter().all(|record|record["type"]!="extra"));
    drop(tickets);
}

#[tokio::test]
async fn process_exit_is_observed_independently_of_pending_rpc_ack() {
    let f = Fixture::new().await;
    assert!(f.native.call("die",json!({}),Duration::from_secs(2)).await.is_err());
    let exit = tokio::time::timeout(Duration::from_secs(2),f.native.exited()).await.unwrap();
    assert!(exit.success); assert_eq!(exit.code,Some(0));
    assert!(f.events.lock().unwrap().iter().any(|event|matches!(event,Event::Exited(exit) if exit.success)));
    assert_eq!(f.native.pending(),0);
}
