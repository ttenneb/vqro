//! Serial start admission. A native idle-looking preflight is still busy; controls bypass this gate.
use std::sync::{Arc,Mutex};
use std::time::Duration;
use serde_json::{json,Value};
use tokio::sync::{watch,OwnedMutexGuard};
use tokio::time::Instant;
use super::native::{Error,Event,Native,Reply,Ticket};

#[derive(Debug,Clone,Default)]
pub struct Current {
    pub metadata:Arc<Value>,
    pub streaming:Option<bool>,
    pub compacting:Option<bool>,
    pub run_active:bool,
    pub starts:u64,
    pub preflights:usize,
    pub ended:bool,
}
impl Current {
    pub fn idle(&self) -> bool {
        !self.ended && !self.run_active && self.preflights==0 && self.streaming==Some(false) && self.compacting==Some(false)
    }
}
pub struct Signals { current:Mutex<Current>, changed:watch::Sender<u64> }
impl Signals {
    pub fn new() -> Arc<Self> {
        let (changed,_) = watch::channel(0);
        Arc::new(Self { current:Mutex::new(Current::default()),changed })
    }
    pub fn current(&self) -> Current { self.current.lock().unwrap().clone() }
    pub fn changes(&self) -> watch::Receiver<u64> { self.changed.subscribe() }
    pub fn observe(&self,event:&Event) {
        let mut current = self.current.lock().unwrap();
        match event {
            Event::Exited(_) => { current.ended=true;current.run_active=false;current.streaming=Some(false); }
            Event::ProtocolError(_) => { current.streaming=None;current.compacting=None; }
            Event::Record(record) if !current.ended => match record["type"].as_str() {
                Some("agent_start") => { current.starts+=1;current.run_active=true;current.streaming=Some(true); }
                Some("agent_settled") => { current.run_active=false;current.streaming=Some(false); }
                Some("auto_compaction_start") => current.compacting=Some(true),
                Some("auto_compaction_end") => current.compacting=Some(false),
                Some("response") if record["command"]=="get_state" && record["success"]==true => {
                    if record["data"].is_object() {
                        current.metadata=Arc::new(record["data"].clone());
                        current.streaming=record["data"]["isStreaming"].as_bool();
                        current.compacting=record["data"]["isCompacting"].as_bool();
                    }
                }
                _=>{}
            },
            _=>{}
        }
        drop(current);
        self.changed.send_modify(|value|*value=value.wrapping_add(1));
    }
}

#[derive(Debug,Clone)]
pub struct Refusal { pub kind:&'static str,pub message:String }
#[derive(Debug,Clone)]
pub enum Admission {
    Acknowledged { native:String,reply:Reply },
    Dispatched { native:String },
    Refused(Refusal),
    Unknown(Error),
}
pub type Receipt = Arc<dyn Fn(String,Result<Reply,Error>)+Send+Sync>;

struct Flight { _guard:OwnedMutexGuard<()>,signals:Arc<Signals> }
impl Drop for Flight {
    fn drop(&mut self) {
        self.signals.current.lock().unwrap().preflights-=1;
        self.signals.changed.send_modify(|value|*value=value.wrapping_add(1));
    }
}
#[derive(Clone)]
pub struct Starts { native:Native,signals:Arc<Signals>,gate:Arc<tokio::sync::Mutex<()>> }
impl Starts {
    pub fn new(native:Native,signals:Arc<Signals>) -> Self { Self { native,signals,gate:Arc::new(tokio::sync::Mutex::new(())) } }
    pub async fn start(&self,args:Value,deadline:Instant,receipt:Receipt) -> Admission { self.prompt(args,deadline,receipt,false).await }
    pub async fn admit(&self,args:Value,deadline:Instant,receipt:Receipt) -> Admission { self.prompt(args,deadline,receipt,true).await }
    async fn prompt(&self,args:Value,deadline:Instant,receipt:Receipt,allow_busy:bool) -> Admission {
        let Ok(guard) = self.gate.clone().try_lock_owned() else { return refuse("busy","a native start preflight is unresolved"); };
        let baseline = {
            let mut current = self.signals.current.lock().unwrap();
            if current.ended || (!allow_busy && !current.idle()) { return refuse("busy","native idle is not established"); }
            current.preflights+=1;
            current.starts
        };
        let flight = Flight { _guard:guard,signals:self.signals.clone() };
        self.signals.changed.send_modify(|value|*value=value.wrapping_add(1));
        let mut ticket = match self.native.submit("prompt",args,deadline) {
            Ok(ticket)=>ticket,
            Err(error)=>return failure(error),
        };
        let response = tokio::select! { response=ticket.reply()=>Some(response), _=tokio::time::sleep_until(deadline)=>None };
        let native = ticket.id.clone();
        if let Some(response) = response {
            receipt(native.clone(),response.clone());
            // The ACK is not insertion. Keep serialization until a start or a fresh post-ACK idle cut.
            let this=self.clone();let acknowledged=response.is_ok();
            tokio::spawn(async move { this.fence(acknowledged,baseline,flight).await; });
            return match response { Ok(reply)=>Admission::Acknowledged { native,reply },Err(error)=>failure(error) };
        }
        if ticket.cancel_before_write() { return refuse("timeout","deadline before native dispatch"); }
        let written = ticket.written();
        let this=self.clone();
        tokio::spawn(async move { this.finish(ticket,baseline,flight,receipt).await; });
        if written { Admission::Dispatched { native } } else { Admission::Unknown(Error {kind:"timeout",message:"native write incomplete at deadline; outcome unknown".into(),may_have_acted:true}) }
    }
    async fn finish(&self,mut ticket:Ticket,baseline:u64,flight:Flight,receipt:Receipt) {
        let result = ticket.reply().await;
        receipt(ticket.id.clone(),result.clone());
        self.fence(result.is_ok(),baseline,flight).await;
    }
    async fn fence(&self,acknowledged:bool,baseline:u64,_flight:Flight) {
        if !acknowledged { return; }
        let current=self.signals.current();
        if current.ended || current.starts>baseline { return; }
        let mut changed=self.signals.changed.subscribe();
        // Only a request AFTER the authoritative ACK can witness a handled-but-idle prompt.
        let metadata = self.native.call("get_state",json!({}),Duration::from_secs(2)).await;
        if let Ok(reply) = metadata {
            if reply.success && (reply.data["isStreaming"]==true || (reply.data["isStreaming"]==false && reply.data["isCompacting"]==false)) { return; }
        }
        loop {
            let current=self.signals.current();
            if current.ended || current.starts>baseline { return; }
            if changed.changed().await.is_err() { return; }
        }
    }
}
fn refuse(kind:&'static str,message:&str) -> Admission { Admission::Refused(Refusal { kind,message:message.into() }) }
fn failure(error:Error) -> Admission {
    if error.may_have_acted { Admission::Unknown(error) } else { refuse(error.kind,&error.message) }
}
