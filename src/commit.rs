//! One bounded, cancellation-independent workspace committer. The queue owns admitted effects;
//! callers own only a receipt. CAS/validation sees the preceding adopted commit, never a stale
//! snapshot captured at admission. Only this module mutates a running Core's workspace store.
use std::sync::{Arc, Mutex, Weak, atomic::{AtomicBool,Ordering}};
use tokio::sync::{oneshot, watch, OwnedSemaphorePermit, Semaphore};
use crate::{core::{Core, Shared}, queue, rpc::{RpcError, RpcResult}, store::Op};

const REQUESTS: usize = 128;
const BYTES: usize = 8 * 1024 * 1024;
pub(crate) const CONTROL_REQUESTS:usize=16;
const CONTROL_BYTES:usize=1024*1024;
const OPERATIONAL_REQUESTS:usize=80;
const OPERATIONAL_BYTES:usize=4*1024*1024;
type Prepare = Box<dyn FnOnce(&Core) -> Result<Plan, RpcError> + Send>;

pub(crate) enum Action {
    Apply { author:String, on_behalf_of:Option<Arc<str>>, note:Option<String>, ops:Vec<Op> },
    Revert { author:String, on_behalf_of:Option<Arc<str>>, txn:u64 },
    Operational {domain:Box<crate::operational::Domain>,author:String,on_behalf_of:Option<Arc<str>>,note:Option<String>,ops:Option<Vec<Op>>,control:bool},
}
pub(crate) struct Plan {
    pub action: Action,
    pub owner:Option<crate::ext::Owner>,
    /// Policy stays with the caller; the worker owns adoption hooks so receipt cancellation
    /// cannot suppress necessary runtime reconciliation after a durable state change.
    pub after_adopt: Option<fn(&Shared)>,
}
struct Request { prepare:Prepare, receipt:oneshot::Sender<RpcResult>, _slot:Completion,op_charge:Option<queue::Charge> }
struct Completion { slot:Option<OwnedSemaphorePermit>, _op_slot:Option<OwnedSemaphorePermit>, progress:watch::Sender<()> }
impl Drop for Completion {
    fn drop(&mut self) { drop(self.slot.take()); self.progress.send_replace(()); }
}
type Receiver=Arc<tokio::sync::Mutex<queue::Receiver<Request>>>;
pub(crate) struct Drain { pub completed:bool, pub rejected:usize, pub active:usize }

pub(crate) struct Committer {
    sender: queue::Sender<Request>,
    start: Mutex<Option<Receiver>>,
    receiver:Receiver,
    slots: Arc<Semaphore>,
    control_sender:queue::Sender<Request>,control_receiver:Receiver,control_slots:Arc<Semaphore>,
    operational_slots:Arc<Semaphore>,operational_bytes:Arc<Semaphore>,
    progress:watch::Sender<()>,
    cancel_queued:Arc<AtomicBool>,
}
impl Committer {
    pub fn new() -> Self {
        let (sender, receiver) = queue::channel(BYTES);
        let receiver=Arc::new(tokio::sync::Mutex::new(receiver));
        let (progress,_)=watch::channel(());
        let(control_sender,control_receiver)=queue::channel(CONTROL_BYTES);
        Self { sender,start:Mutex::new(Some(receiver.clone())),receiver,slots:Arc::new(Semaphore::new(REQUESTS)),control_sender,control_receiver:Arc::new(tokio::sync::Mutex::new(control_receiver)),control_slots:Arc::new(Semaphore::new(CONTROL_REQUESTS)),operational_slots:Arc::new(Semaphore::new(OPERATIONAL_REQUESTS)),operational_bytes:Arc::new(Semaphore::new(OPERATIONAL_BYTES)),progress,cancel_queued:Arc::new(AtomicBool::new(false)) }
    }
    pub fn started(&self) -> bool { self.start.lock().unwrap().is_none() }
    pub fn pending(&self) -> usize { REQUESTS-self.slots.available_permits()+CONTROL_REQUESTS-self.control_slots.available_permits() }
    fn enqueue(&self,core:&Shared,bytes:usize,prepare:Prepare)->Result<oneshot::Receiver<RpcResult>,RpcError> {self.enqueue_kind(core,bytes,prepare,false,false)}
    fn enqueue_kind(&self, core:&Shared, bytes:usize, prepare:Prepare,control:bool,operational:bool) -> Result<oneshot::Receiver<RpcResult>,RpcError> {
        let slot=(if control {self.control_slots.clone()}else{self.slots.clone()}).try_acquire_owned().map_err(|e| match e {
            tokio::sync::TryAcquireError::Closed=>rejected("unavailable","commits are stopping; not admitted"),
            tokio::sync::TryAcquireError::NoPermits=>rejected("overloaded","commit lane request limit; not admitted"),
        })?;
        let(op_slot,op_charge)=if operational&&!control {
            let slot=self.operational_slots.clone().try_acquire_owned().map_err(|_|rejected("overloaded","operational normal request limit; workspace headroom retained"))?;
            let charge=queue::Charge::reserve(self.operational_bytes.clone(),OPERATIONAL_BYTES,bytes.max(256)).map_err(|e|rejected("overloaded",format!("operational normal byte limit; workspace headroom retained: {e}")))?;
            (Some(slot),Some(charge))
        }else{(None,None)};
        let (receipt, received) = oneshot::channel();
        // Core's owner and the idle worker have no strong-reference cycle. An active write
        // holds Core alive until adoption, even if its caller has disappeared.
        if let Some(receiver) = self.start.lock().unwrap().take() {
            tokio::spawn(run(Arc::downgrade(core),receiver,self.control_receiver.clone(),self.cancel_queued.clone()));
        }
        let sender=if control {&self.control_sender}else{&self.sender};
        sender.try_send(Request { prepare,receipt,op_charge,_slot:Completion { slot:Some(slot),_op_slot:op_slot,progress:self.progress.clone() } }, bytes.max(256))
            .map_err(|e| rejected(if e == queue::QueueError::Closed { "unavailable" } else { "overloaded" }, format!("workspace commit was not admitted: {e}")))?;
        Ok(received)
    }
}
fn rejected(kind:&str, message:impl Into<String>) -> RpcError {
    let mut err = RpcError::new(kind, message);
    err.data.as_mut().unwrap()["outcome"] = "rejected".into();
    err
}
fn indeterminate(message:impl Into<String>) -> RpcError {
    let mut err = RpcError::new("indeterminate", message);
    err.data.as_mut().unwrap()["outcome"] = "indeterminate".into();
    err
}
pub(crate) async fn submit(core:&Shared, bytes:usize, prepare:impl FnOnce(&Core)->Result<Plan,RpcError> + Send + 'static) -> RpcResult {
    let received={
        let c=core.lock().unwrap();
        if c.shutdown.is_requested() { return Err(rejected("unavailable","core is shutting down; commit not admitted")); }
        c.committer.enqueue(core,bytes,Box::new(prepare))?
    };
    received.await.map_err(|_| indeterminate("workspace committer stopped after admission; inspect state before retrying"))?
}
pub(crate) async fn submit_operational(core:&Shared,bytes:usize,control:bool,prepare:impl FnOnce(&Core)->Result<Plan,RpcError>+Send+'static)->RpcResult {
    let received={let c=core.lock().unwrap();if c.shutdown.is_requested(){return Err(rejected("unavailable","core stopping; operational commit not admitted"));}c.committer.enqueue_kind(core,bytes,Box::new(prepare),control,true)?};
    received.await.map_err(|_|indeterminate("operational committer stopped after admission; inspect, never replay"))?
}
/// Stop admission before waiting. At expiry, queued work is definitely not attempted;
/// a write already dispatched to the blocking pool remains indeterminate and is never replayed.
pub(crate) async fn shutdown(core:&Shared,grace:std::time::Duration) -> Drain {
    let (slots,control_slots,mut progress,receivers,cancel)={
        let c=core.lock().unwrap(); let committer=&c.committer;
        committer.slots.close();committer.control_slots.close();committer.operational_slots.close();
        (committer.slots.clone(),committer.control_slots.clone(),committer.progress.subscribe(),[committer.receiver.clone(),committer.control_receiver.clone()],committer.cancel_queued.clone())
    };
    let pending=||REQUESTS-slots.available_permits()+CONTROL_REQUESTS-control_slots.available_permits();
    let drained=tokio::time::timeout(grace,async {
        while pending()!=0 { if progress.changed().await.is_err() { break; } }
    }).await.is_ok() && pending()==0;
    if drained { return Drain { completed:true,rejected:0,active:0 }; }
    cancel.store(true,Ordering::Release);
    let mut rejected_count=0;
    for receiver in receivers {
        if let Ok(mut receiver)=tokio::time::timeout(std::time::Duration::from_millis(100),receiver.lock()).await {
            receiver.close();
            while let Ok(item)=receiver.try_recv() {
                let _=item.value.receipt.send(Err(rejected("unavailable","shutdown deadline before commit started; no effect")));
                rejected_count+=1;
            }
        }
    }
    Drain { completed:false,rejected:rejected_count,active:pending() }
}
enum Written {
    Workspace(crate::store::Store,Result<Arc<crate::store::Txn>,RpcError>),
    Operational(Box<crate::operational::Domain>,Result<Arc<crate::operational::Transaction>,RpcError>,bool),
}
async fn run(core:Weak<Mutex<Core>>, queue:Receiver,control_queue:Receiver,cancel:Arc<AtomicBool>) {
    let(mut normal_open,mut control_open,mut control_burst)=(true,true,0usize);
    loop {
        let(item,control)=if control_burst>=4 {
            tokio::select! {biased;
                item=async {queue.lock().await.recv().await},if normal_open=>(item,false),
                item=async {control_queue.lock().await.recv().await},if control_open=>(item,true),
                else=>break,
            }
        }else{
            tokio::select! {biased;
                item=async {control_queue.lock().await.recv().await},if control_open=>(item,true),
                item=async {queue.lock().await.recv().await},if normal_open=>(item,false),
                else=>break,
            }
        };
        let Some(item)=item else {if control {control_open=false;}else{normal_open=false;}continue;};
        if control {control_burst=control_burst.saturating_add(1);}else{control_burst=0;}
        // Keep both the byte charge and request slot through writing, adoption and hooks.
        let queue::Item {value:Request {prepare,receipt,_slot,mut op_charge},mut charge}=item;
        if cancel.load(Ordering::Acquire) { let _=receipt.send(Err(rejected("unavailable","shutdown before commit started; no effect"))); continue; }
        let Some(core) = core.upgrade() else { let _ = receipt.send(Err(rejected("unavailable", "core stopped before commit"))); continue; };
        let prepared = {
            let c = core.lock().unwrap();
            prepare(&c).map(|plan| {let store=if matches!(&plan.action,Action::Operational{..}) {None}else{Some(c.store.committed_copy())};(store,plan)})
        };
        let (mut store, plan) = match prepared {
            Ok(work) => work,
            Err(err) => { let _ = receipt.send(Err(err)); continue; }
        };
        let expanded_bytes=match &plan.action {
            Action::Apply {author,on_behalf_of,note,ops}=>crate::limits::encoded_bytes(ops).saturating_add(crate::limits::encoded_bytes(&(author,on_behalf_of.as_deref(),note))),
            Action::Revert {author,on_behalf_of,txn}=>store.as_ref().unwrap().revert_bytes(*txn).unwrap_or(0).saturating_add(crate::limits::encoded_bytes(&(author,on_behalf_of.as_deref(),format!("revert {txn}")))),
            Action::Operational {author,on_behalf_of,note,ops,..}=>crate::limits::encoded_bytes(ops).saturating_add(crate::limits::encoded_bytes(&(author,on_behalf_of.as_deref(),note))),
        };
        let resized=charge.try_resize(expanded_bytes.max(256)).and_then(|_|match &mut op_charge {Some(charge)=>charge.try_resize(expanded_bytes.max(256)),None=>Ok(())});
        if let Err(error)=resized {
            let _=receipt.send(Err(rejected(if error==queue::QueueError::Closed {"unavailable"} else {"overloaded"},format!("expanded workspace mutation ({expanded_bytes} bytes) was not dispatched: {error}"))));
            continue;
        }
        let after_adopt = plan.after_adopt;
        // Candidate cloning, CAS/validation, size accounting, encoding and all filesystem
        // operations run on a blocking thread without the core mutex or an async worker.
        let writer = {
            let c=core.lock().unwrap();
            if let Action::Operational {domain,..}=&plan.action {
                if plan.owner.as_ref().is_none_or(|owner|owner.name!=domain.owner) {let _=receipt.send(Err(rejected("forbidden","operational commit requires its verified extension owner")));continue;}
            }
            if let Err(error)=crate::ext::check_owner(&c,plan.owner.as_ref()) { let _=receipt.send(Err(error)); continue; }
            let epoch=c.epoch.clone();let generation=plan.owner.as_ref().map(|owner|owner.generation).unwrap_or(0);
            // Irreversible writer admission and lifetime retirement share this mutex. After
            // this point the candidate MUST be adopted/reported normally, even if its owner exits.
            tokio::task::spawn_blocking(move || {
                match plan.action {
                    Action::Apply { author, on_behalf_of, note, ops } => {let mut store=store.take().unwrap();let result=store.apply_attributed(&author,on_behalf_of.as_deref(),note,ops);Written::Workspace(store,result)},
                    Action::Revert { author, on_behalf_of, txn } => {let mut store=store.take().unwrap();let result=store.revert_attributed(&author,on_behalf_of.as_deref(),txn);Written::Workspace(store,result)},
                    Action::Operational {mut domain,author,on_behalf_of,note,ops,control}=>{
                        let registration=ops.is_none();
                        let attribution=crate::operational::Attribution {author,on_behalf_of:on_behalf_of.as_deref().map(str::to_owned),epoch,generation};
                        let result=match ops {Some(ops)=>domain.apply(attribution,note,ops,control),None=>domain.register(attribution)};
                        Written::Operational(domain,result,registration)
                    },
                }
            })
        };
        let written=writer.await;
        let written = match written {
            Ok(written) => written,
            Err(err) => {
                // A panicking writer may already have renamed. Never roll back/replay or
                // continue from possibly stale memory; fail closed until core restart.
                let _ = receipt.send(Err(indeterminate(format!("workspace writer stopped: {err}; restart required"))));
                for receiver in [&queue,&control_queue] {let mut receiver=receiver.lock().await;receiver.close();while let Ok(item)=receiver.try_recv(){drop(item);}}
                break;
            }
        };
        let (result,adopted)=match written {
            Written::Workspace(mut store,result)=>{
                let adopted=result.is_ok()||result.as_ref().is_err_and(|e|e.kind()=="durability_unconfirmed");
                let(result,retired)={let mut c=core.lock().unwrap();if c.shutdown.is_stopped(){store.release_state();}
                    let retired=if adopted {Some(std::mem::replace(&mut c.store,store))}else{None};(c.finish_txn(result),retired)};
                drop(retired);(result.map(|txn|Core::txn_summary(&txn)),adopted)
            },
            Written::Operational(mut domain,result,registration)=>{
                let adopted=result.is_ok()||result.as_ref().is_err_and(|e|e.kind()=="durability_unconfirmed");
                let(result,retired)={let mut c=core.lock().unwrap();if c.shutdown.is_stopped(){domain.release_state();}
                    c.finish_operational(*domain,result,registration)};
                drop(retired);(result,adopted)
            },
        };
        if adopted { if let Some(hook) = after_adopt { hook(&core); } }
        let _ = receipt.send(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn core() -> Shared {
        Arc::new(Mutex::new(Core::new(crate::store::Store::in_memory(), None, "unused".into(), false).unwrap()))
    }
    fn prepare() -> Prepare {
        Box::new(|_| Ok(Plan { owner:None, action:Action::Apply { author:"test".into(), on_behalf_of:None, note:None, ops:Vec::new() }, after_adopt:None }))
    }
    #[test]
    fn operational_normal_leaves_workspace_and_control_count_and_byte_headroom() {
        let core=core();let c=core.lock().unwrap();let receiver=c.committer.start.lock().unwrap().take().unwrap();let mut receiver=receiver.try_lock().unwrap();let mut controls=c.committer.control_receiver.try_lock().unwrap();
        for _ in 0..OPERATIONAL_REQUESTS {drop(c.committer.enqueue_kind(&core,256,prepare(),false,true).unwrap());}
        assert_eq!(c.committer.enqueue_kind(&core,256,prepare(),false,true).unwrap_err().kind(),"overloaded");
        for _ in OPERATIONAL_REQUESTS..REQUESTS {drop(c.committer.enqueue(&core,256,prepare()).unwrap());}
        for _ in 0..CONTROL_REQUESTS {drop(c.committer.enqueue_kind(&core,256,prepare(),true,true).unwrap());}
        assert_eq!(c.committer.pending(),REQUESTS+CONTROL_REQUESTS);
        receiver.close();controls.close();while let Ok(item)=receiver.try_recv(){drop(item);}while let Ok(item)=controls.try_recv(){drop(item);}assert_eq!(c.committer.pending(),0);
        let core=super::tests::core();let c=core.lock().unwrap();let receiver=c.committer.start.lock().unwrap().take().unwrap();let mut receiver=receiver.try_lock().unwrap();let mut controls=c.committer.control_receiver.try_lock().unwrap();
        drop(c.committer.enqueue_kind(&core,OPERATIONAL_BYTES,prepare(),false,true).unwrap());
        assert!(c.committer.enqueue_kind(&core,256,prepare(),false,true).is_err());
        drop(c.committer.enqueue(&core,BYTES-OPERATIONAL_BYTES,prepare()).unwrap());
        drop(c.committer.enqueue_kind(&core,CONTROL_BYTES,prepare(),true,true).unwrap());
        assert_eq!(c.committer.sender.used_bytes(),BYTES);assert_eq!(c.committer.control_sender.used_bytes(),CONTROL_BYTES);
        receiver.close();controls.close();while let Ok(item)=receiver.try_recv(){drop(item);}while let Ok(item)=controls.try_recv(){drop(item);}
    }
    #[tokio::test]
    async fn ready_control_priority_is_bounded_and_normal_fifo_still_progresses() {
        let core=core();let mut receipts=Vec::new();
        {let c=core.lock().unwrap();
            for (control,count,label) in [(false,2,"n"),(true,8,"c")] {for n in 0..count {
                let author=format!("{label}{n}");let key=format!("doc/{author}");
                let prepare=Box::new(move|_:&Core|Ok(Plan {owner:None,action:Action::Apply {author,on_behalf_of:None,note:None,ops:vec![Op::Set {key,value:serde_json::json!(1),expect:None}]},after_adopt:None}));
                receipts.push(c.committer.enqueue_kind(&core,512,prepare,control,false).unwrap());
            }}
        }
        for receipt in receipts {receipt.await.unwrap().unwrap();}
        let c=core.lock().unwrap();let mut authors=c.store.history(20).iter().map(|txn|txn.author.clone()).collect::<Vec<_>>();authors.reverse();
        assert_eq!(authors,["c0","c1","c2","c3","n0","c4","c5","c6","c7","n1"]);
    }
    #[tokio::test]
    async fn operational_owner_retires_at_the_existing_writer_dispatch_linearization() {
        for dispatched in [false,true] {
            let root=Arc::new(tempfile::tempdir().unwrap());let token_path=root.path().join("owner.token");
            let mut store=crate::store::Store::open_locked(root.path().join("workspace.json")).unwrap();
            store.apply("fixture",None,vec![Op::Set {key:"extension/owner".into(),value:serde_json::json!({"enabled":true,"argv":["/usr/bin/python3",std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/managed_identity.py"),token_path]}),expect:None}]).unwrap();
            let core=Arc::new(Mutex::new(Core::new(store,Some(root.path().into()),root.path().join("p"),false).unwrap()));
            let gate=Arc::new((Mutex::new(false),std::sync::Condvar::new()));
            struct Release(Arc<(Mutex<bool>,std::sync::Condvar)>);impl Drop for Release{fn drop(&mut self){*self.0.0.lock().unwrap()=true;self.0.1.notify_all();}}
            let _release=Release(gate.clone());let entered=Arc::new(tokio::sync::Notify::new());
            let writer:crate::store::DurableWriter=Arc::new({let gate=gate.clone();let entered=entered.clone();let root=root.clone();move|path,bytes| {
                let _keep=&root;let value:serde_json::Value=serde_json::from_slice(bytes).unwrap();
                let block=if dispatched {value["last"]["note"]=="claim"}else{value["entries"].get("doc/block").is_some()};
                if block {entered.notify_one();let mut open=gate.0.lock().unwrap();while !*open {open=gate.1.wait(open).unwrap();}}
                crate::store::write_atomic(path,bytes)
            }});
            if dispatched {core.lock().unwrap().configure_operational_writer(writer).unwrap();}else{core.lock().unwrap().configure_workspace_writer(writer).unwrap();}
            core.lock().unwrap().manifest_catalog.root=root.path().join("empty");crate::ext::reconcile(&core);
            let until=std::time::Instant::now()+std::time::Duration::from_secs(5);
            let owner=loop {if let Ok(token)=std::fs::read_to_string(&token_path) {if let Some(owner)=crate::ext::authenticate(&core.lock().unwrap(),"owner",&token){break owner;}}assert!(std::time::Instant::now()<until);tokio::time::sleep(std::time::Duration::from_millis(5)).await;};
            let registration_owner=owner.clone();submit_operational(&core,256,true,move|c|Ok(Plan {owner:Some(registration_owner.clone()),action:Action::Operational {domain:Box::new(c.operational.prepare("state",&registration_owner.name,None)?),author:"ext:owner".into(),on_behalf_of:None,note:None,ops:None,control:true},after_adopt:None})).await.unwrap();
            let blocker=if !dispatched {let c=core.lock().unwrap();Some(c.committer.enqueue(&core,512,Box::new(|_|Ok(Plan {owner:None,action:Action::Apply {author:"fixture".into(),on_behalf_of:None,note:None,ops:vec![Op::Set {key:"doc/block".into(),value:serde_json::json!(1),expect:None}]},after_adopt:None}))).unwrap())}else{None};
            if !dispatched {tokio::time::timeout(std::time::Duration::from_secs(2),entered.notified()).await.unwrap();}
            let c=core.clone();let owner2=owner.clone();let request=tokio::spawn(async move {submit_operational(&c,512,true,move|c|Ok(Plan {owner:Some(owner2.clone()),action:Action::Operational {domain:Box::new(c.operational.prepare("state",&owner2.name,None)?),author:"ext:owner".into(),on_behalf_of:None,note:Some("claim".into()),ops:Some(vec![Op::Set {key:"operational/state/a".into(),value:serde_json::json!({"phase":"claimed"}),expect:Some(0)}]),control:true},after_adopt:None})).await});
            if dispatched {tokio::time::timeout(std::time::Duration::from_secs(2),entered.notified()).await.unwrap();}else{let until=std::time::Instant::now()+std::time::Duration::from_secs(2);while core.lock().unwrap().committer.pending()<2 {assert!(std::time::Instant::now()<until,"operational request not queued");tokio::task::yield_now().await;}}
            crate::ext::stop_all(&core);assert!(!owner.is_live());*gate.0.lock().unwrap()=true;gate.1.notify_all();
            if let Some(blocker)=blocker {blocker.await.unwrap().unwrap();}
            let result=request.await.unwrap();if dispatched {result.unwrap();}else{assert_eq!(result.unwrap_err().kind(),"stale_generation");}
            assert_eq!(core.lock().unwrap().operational.get("state").unwrap().entries.contains_key("operational/state/a"),dispatched);
            crate::ext::shutdown(&core).await;
        }
    }
    #[test]
    fn cancelled_receipts_and_dequeued_work_keep_the_byte_budget_until_completion() {
        let core = core(); let c = core.lock().unwrap();
        // Hold the receiver manually: test admission bookkeeping without performing effects.
        let receiver=c.committer.start.lock().unwrap().take().unwrap();
        let mut receiver=receiver.try_lock().unwrap();
        drop(c.committer.enqueue(&core, BYTES, prepare()).unwrap());
        assert_eq!(c.committer.sender.used_bytes(), BYTES);
        assert_eq!(c.committer.pending(), 1);
        let item = receiver.try_recv().unwrap();
        let err = c.committer.enqueue(&core, 256, prepare()).unwrap_err();
        assert_eq!(err.kind(), "overloaded");
        assert_eq!(err.data.unwrap()["outcome"], "rejected");
        assert_eq!(c.committer.pending(), 1);
        drop(item);
        assert_eq!(c.committer.pending(), 0);
        assert_eq!(c.committer.sender.used_bytes(), 0);
        drop(c.committer.enqueue(&core, BYTES, prepare()).unwrap());
        receiver.close();
        while let Ok(item)=receiver.try_recv() { drop(item); }
        assert_eq!(c.committer.pending(), 0);
        assert_eq!(c.committer.enqueue(&core, 256, prepare()).unwrap_err().kind(), "unavailable");
        assert!(c.store.entries().is_empty());
    }
    #[tokio::test]
    async fn shutdown_deadline_rejects_queued_work_but_never_claims_active_write_failed() {
        let root=Arc::new(tempfile::Builder::new().prefix("pcq").tempdir_in("/tmp").unwrap());
        let store=crate::store::Store::open_locked(root.path().join("workspace.json")).unwrap();
        let core=Arc::new(Mutex::new(Core::new(store,None,root.path().join("s"),false).unwrap()));
        let gate=Arc::new((Mutex::new(false),std::sync::Condvar::new()));
        struct Release(Arc<(Mutex<bool>,std::sync::Condvar)>);
        impl Drop for Release { fn drop(&mut self) { *self.0.0.lock().unwrap()=true; self.0.1.notify_all(); } }
        let _release=Release(gate.clone());
        let entered=Arc::new(tokio::sync::Notify::new());
        core.lock().unwrap().configure_workspace_writer(Arc::new({let gate=gate.clone();let entered=entered.clone();let root=root.clone();move|path,bytes| {
            let _keep=&root;entered.notify_one();let mut open=gate.0.lock().unwrap();while !*open {open=gate.1.wait(open).unwrap();}crate::store::write_atomic(path,bytes)
        }})).unwrap();
        fn hook(core:&Shared) { core.lock().unwrap().safe_mode=true; }
        let plan=||Box::new(|_:&Core|Ok(Plan { owner:None, action:Action::Apply { author:"test".into(),on_behalf_of:None,note:None,ops:vec![Op::Set {key:"doc/queued".into(),value:serde_json::json!(1),expect:None}] },after_adopt:Some(hook) })) as Prepare;
        let first=core.lock().unwrap().committer.enqueue(&core,256,plan()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2),entered.notified()).await.unwrap();
        let second=core.lock().unwrap().committer.enqueue(&core,256,plan()).unwrap();
        let drained=shutdown(&core,std::time::Duration::from_millis(20)).await;
        assert!(!drained.completed);assert_eq!(drained.rejected,1);assert_eq!(drained.active,1);
        let error=second.await.unwrap().unwrap_err();assert_eq!(error.data.unwrap()["outcome"],"rejected");
        assert_eq!(core.lock().unwrap().committer.enqueue(&core,256,plan()).unwrap_err().kind(),"unavailable");
        { let mut c=core.lock().unwrap();c.store.release_state();c.shutdown.finish(); }
        assert!(crate::store::Store::open_locked(root.path().join("workspace.json")).is_err(),"late writer still owns the fence after endpoint shutdown");
        *gate.0.lock().unwrap()=true;gate.1.notify_all();
        first.await.unwrap().unwrap();
        assert!(core.lock().unwrap().safe_mode,"late active completion must still run adoption hook");
        assert!(crate::store::Store::open_locked(root.path().join("workspace.json")).is_ok(),"late adoption must not re-pin the stopped core's fence");
        assert_eq!(crate::store::Store::open(root.path().join("workspace.json")).unwrap().history(10).len(),1,"queued work must never be replayed");
    }
    #[test]
    fn metadata_admission_is_globally_count_bounded_too() {
        let core = core(); let c = core.lock().unwrap();
        let receiver=c.committer.start.lock().unwrap().take().unwrap();
        let mut receiver=receiver.try_lock().unwrap();
        for _ in 0..REQUESTS { drop(c.committer.enqueue(&core, 0, prepare()).unwrap()); }
        assert_eq!(c.committer.pending(), REQUESTS);
        assert_eq!(c.committer.enqueue(&core, 0, prepare()).unwrap_err().kind(), "overloaded");
        let item = receiver.try_recv().unwrap();
        assert_eq!(c.committer.pending(), REQUESTS);
        drop(item);
        drop(c.committer.enqueue(&core, 0, prepare()).unwrap());
        assert_eq!(c.committer.pending(), REQUESTS);
        receiver.close();
        while let Ok(item)=receiver.try_recv() { drop(item); }
        assert_eq!(c.committer.pending(), 0);
    }
}
