//! Native-preflight regression. Private fixture controls are files, never product effects.
#[allow(dead_code)] // This test compiles only the admission slice of the private binary modules.
#[path="../src/bin/vqro-bridge-pi/native.rs"] mod native;
#[path="../src/bin/vqro-bridge-pi/admission.rs"] mod admission;
use std::collections::HashMap;
use std::sync::{Arc,Mutex};
use std::time::Duration;
use admission::{Admission,Signals,Starts};
use native::Native;
use serde_json::{json,Value};
use tokio::time::Instant;

const CHILD:&str=r#"
import json,os,pathlib,sys,threading,time
lock=threading.Lock()
streaming=False
pending=False
def emit(v):
    with lock: print(json.dumps(v),flush=True)
def response(row,data=None): emit({'type':'response','id':row['id'],'command':row['type'],'success':True,'data':data})
def released(row):
    global streaming,pending
    release=pathlib.Path(os.environ['PI_FIXTURE_RELEASE'])
    end=time.monotonic()+5
    while not release.exists() and release.parent.exists() and time.monotonic()<end: time.sleep(.01)
    if not release.exists(): return
    streaming=True
    emit({'type':'agent_start'})
    response(row)
    pending=False
for line in sys.stdin:
    row=json.loads(line)
    with open(os.environ['PI_FIXTURE_LEDGER'],'a') as f: f.write(json.dumps(row)+'\n')
    kind=row['type']
    if kind=='get_state': response(row,{'sessionId':'s1','isStreaming':streaming,'isCompacting':False})
    elif kind=='abort':
        emit({'type':'agent_settled','aborted':True})
        response(row)
    elif kind=='prompt':
        if row.get('message')=='gated':
            pending=True
            threading.Thread(target=released,args=(row,),daemon=True).start()
        elif row.get('message')=='broken': print('{broken',flush=True)
        else: response(row)
"#;
struct Fixture {
    root:tempfile::TempDir,native:Native,signals:Arc<Signals>,starts:Starts,
    receipts:Arc<Mutex<Vec<(String,Result<native::Reply,native::Error>)>>>,
}
impl Fixture {
    async fn new() -> Self {
        let root=tempfile::tempdir().unwrap();
        let env=HashMap::from([
            ("HOME".into(),root.path().to_string_lossy().into_owned()),("PATH".into(),"/usr/bin:/bin".into()),
            ("PI_FIXTURE_RELEASE".into(),root.path().join("release").to_string_lossy().into_owned()),
            ("PI_FIXTURE_LEDGER".into(),root.path().join("ledger").to_string_lossy().into_owned()),
        ]);
        let signals=Signals::new();let observed=signals.clone();
        let native=Native::spawn(&["/usr/bin/python3".into(),"-u".into(),"-c".into(),CHILD.into()],root.path(),&env,Arc::new(move|event|observed.observe(&event))).await.unwrap();
        let starts=Starts::new(native.clone(),signals.clone());
        Self {root,native,signals,starts,receipts:Arc::new(Mutex::new(Vec::new()))}
    }
    async fn idle(&self) {
        assert!(self.native.call("get_state",json!({}),Duration::from_secs(2)).await.unwrap().success);
        assert!(self.signals.current().idle(),"observation must be installed before RPC caller wakes");
    }
    async fn send(&self,message:&str,ms:u64) -> Admission {
        let receipts=self.receipts.clone();
        self.starts.start(json!({"message":message}),Instant::now()+Duration::from_millis(ms),Arc::new(move|id,result|receipts.lock().unwrap().push((id,result)))).await
    }
    async fn fence(&self) {
        let mut changed=self.signals.changes();
        tokio::time::timeout(Duration::from_secs(3),async {
            loop { if self.signals.current().preflights==0 {break;} changed.changed().await.unwrap(); }
        }).await.unwrap();
    }
    fn prompts(&self) -> usize {
        std::fs::read_to_string(self.root.path().join("ledger")).unwrap_or_default().lines().map(|line|serde_json::from_str::<Value>(line).unwrap()).filter(|row|row["type"]=="prompt").count()
    }
    async fn close(&self) {
        self.native.stop();tokio::time::timeout(Duration::from_secs(2),self.native.exited()).await.unwrap();
    }
}
fn busy(result:Admission) {
    let Admission::Refused(refusal)=result else {panic!("expected no-write busy refusal");};
    assert_eq!(refusal.kind,"busy");assert!(!refusal.message.is_empty());
}

#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn unresolved_preflight_is_busy_despite_idle_and_abort_ack() {
    let f=Fixture::new().await;f.idle().await;
    let Admission::Dispatched {native}=f.send("gated",80).await else {panic!("long admission must dispatch, not fabricate acceptance");};
    assert!(!native.is_empty());
    assert_eq!(f.signals.current().preflights,1);
    assert_eq!(f.signals.current().streaming,Some(false));
    busy(f.send("must-not-write",500).await);
    busy(f.starts.admit(json!({"message":"also-must-not-write","streamingBehavior":"followUp"}),Instant::now()+Duration::from_millis(500),Arc::new(|_,_|{})).await);
    assert!(f.native.call("abort",json!({}),Duration::from_secs(2)).await.unwrap().success);
    busy(f.send("still-must-not-write",500).await);
    assert_eq!(f.prompts(),1,"both later starts rejected before native writes");
    std::fs::write(f.root.path().join("release"),b"release").unwrap();
    f.fence().await;
    assert_eq!(f.signals.current().starts,1);
    assert!(!f.signals.current().idle());
    assert_eq!(f.receipts.lock().unwrap().len(),1);
    assert!(f.receipts.lock().unwrap()[0].1.as_ref().unwrap().success);
    assert_eq!(f.prompts(),1,"late ACK never causes replay");
    let admitted=f.starts.admit(json!({"message":"native-admission","streamingBehavior":"followUp"}),Instant::now()+Duration::from_millis(500),Arc::new(|_,_|{})).await;
    assert!(matches!(admitted,Admission::Acknowledged {..}));f.fence().await;
    busy(f.send("busy-run-start",500).await);
    assert_eq!(f.prompts(),2,"admit uses the same preflight gate without holding it for the native run");
    f.close().await;
}

#[tokio::test(flavor="multi_thread",worker_threads=2)]
async fn ack_without_a_run_uses_fresh_idle_cut_and_never_infers_insertion() {
    let f=Fixture::new().await;f.idle().await;
    for _ in 0..2 {
        let Admission::Acknowledged {native,reply}=f.send("handled-or-queued-unknown",500).await else {panic!("ACK");};
        assert!(!native.is_empty());assert!(reply.success);assert!(reply.data.is_null());
        f.fence().await;
        assert_eq!(f.signals.current().starts,0);
        assert!(f.signals.current().idle());
    }
    assert_eq!(f.prompts(),2);assert_eq!(f.receipts.lock().unwrap().len(),2);
    f.close().await;
}

#[tokio::test]
async fn unknown_idle_and_expired_start_refuse_before_native_write() {
    let f=Fixture::new().await;
    busy(f.send("unknown-state",200).await);
    f.idle().await;
    let Admission::Refused(refusal)=f.starts.start(json!({"message":"expired"}),Instant::now(),Arc::new(|_,_|{})).await else {panic!("expired");};
    assert_eq!(refusal.kind,"timeout");assert_eq!(f.signals.current().preflights,0);
    assert_eq!(f.prompts(),0);
    f.close().await;
}

#[tokio::test]
async fn post_write_protocol_failure_is_unknown_not_rejected() {
    let f=Fixture::new().await;f.idle().await;
    let Admission::Unknown(error)=f.send("broken",500).await else {panic!("unknown after native write");};
    assert!(error.may_have_acted);
    tokio::time::timeout(Duration::from_secs(2),f.native.exited()).await.unwrap();
    assert!(f.signals.current().ended);
    assert_eq!(f.prompts(),1);
}
