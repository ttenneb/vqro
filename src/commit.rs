//! One bounded, cancellation-independent workspace committer. The queue owns admitted effects;
//! callers own only a receipt. CAS/validation sees the preceding adopted commit, never a stale
//! snapshot captured at admission. Only this module mutates a running Core's workspace store.
use std::sync::{Arc, Mutex, Weak, atomic::{AtomicBool,Ordering}};
use tokio::sync::{oneshot, watch, OwnedSemaphorePermit, Semaphore};
use crate::{core::{Core, Shared}, queue, rpc::{RpcError, RpcResult}, store::Op};

const REQUESTS: usize = 128;
const BYTES: usize = 8 * 1024 * 1024;
type Prepare = Box<dyn FnOnce(&Core) -> Result<Plan, RpcError> + Send>;

pub(crate) enum Action {
    Apply { author:String, on_behalf_of:Option<Arc<str>>, note:Option<String>, ops:Vec<Op> },
    Revert { author:String, on_behalf_of:Option<Arc<str>>, txn:u64 },
}
pub(crate) struct Plan {
    pub action: Action,
    /// Policy stays with the caller; the worker owns adoption hooks so receipt cancellation
    /// cannot suppress necessary runtime reconciliation after a durable state change.
    pub after_adopt: Option<fn(&Shared)>,
}
struct Request { prepare:Prepare, receipt:oneshot::Sender<RpcResult>, _slot:Completion }
struct Completion { slot:Option<OwnedSemaphorePermit>, progress:watch::Sender<()> }
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
    progress:watch::Sender<()>,
    cancel_queued:Arc<AtomicBool>,
}
impl Committer {
    pub fn new() -> Self {
        let (sender, receiver) = queue::channel(BYTES);
        let receiver=Arc::new(tokio::sync::Mutex::new(receiver));
        let (progress,_)=watch::channel(());
        Self { sender,start:Mutex::new(Some(receiver.clone())),receiver,slots:Arc::new(Semaphore::new(REQUESTS)),progress,cancel_queued:Arc::new(AtomicBool::new(false)) }
    }
    pub fn started(&self) -> bool { self.start.lock().unwrap().is_none() }
    pub fn pending(&self) -> usize { REQUESTS-self.slots.available_permits() }
    fn enqueue(&self, core:&Shared, bytes:usize, prepare:Prepare) -> Result<oneshot::Receiver<RpcResult>,RpcError> {
        let slot=self.slots.clone().try_acquire_owned().map_err(|e| match e {
            tokio::sync::TryAcquireError::Closed=>rejected("unavailable","workspace commits are stopping; not admitted"),
            tokio::sync::TryAcquireError::NoPermits=>rejected("overloaded","workspace commit queue is full"),
        })?;
        let (receipt, received) = oneshot::channel();
        // Core's owner and the idle worker have no strong-reference cycle. An active write
        // holds Core alive until adoption, even if its caller has disappeared.
        if let Some(receiver) = self.start.lock().unwrap().take() {
            tokio::spawn(run(Arc::downgrade(core),receiver,self.cancel_queued.clone()));
        }
        self.sender.try_send(Request { prepare,receipt,_slot:Completion { slot:Some(slot),progress:self.progress.clone() } }, bytes.max(256))
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
/// Stop admission before waiting. At expiry, queued work is definitely not attempted;
/// a write already dispatched to the blocking pool remains indeterminate and is never replayed.
pub(crate) async fn shutdown(core:&Shared,grace:std::time::Duration) -> Drain {
    let (slots,mut progress,receiver,cancel)={
        let c=core.lock().unwrap(); let committer=&c.committer;
        committer.slots.close();
        (committer.slots.clone(),committer.progress.subscribe(),committer.receiver.clone(),committer.cancel_queued.clone())
    };
    let drained=tokio::time::timeout(grace,async {
        while REQUESTS-slots.available_permits()!=0 { if progress.changed().await.is_err() { break; } }
    }).await.is_ok() && REQUESTS-slots.available_permits()==0;
    if drained { return Drain { completed:true,rejected:0,active:0 }; }
    cancel.store(true,Ordering::Release);
    let mut rejected_count=0;
    if let Ok(mut receiver)=tokio::time::timeout(std::time::Duration::from_millis(100),receiver.lock()).await {
        receiver.close();
        while let Ok(item)=receiver.try_recv() {
            let _=item.value.receipt.send(Err(rejected("unavailable","shutdown deadline before commit started; no effect")));
            rejected_count+=1;
        }
    }
    Drain { completed:false,rejected:rejected_count,active:REQUESTS-slots.available_permits() }
}
async fn run(core:Weak<Mutex<Core>>, queue:Receiver,cancel:Arc<AtomicBool>) {
    loop {
        let item={ queue.lock().await.recv().await };
        let Some(item)=item else { break; };
        // Keep both the byte charge and request slot through writing, adoption and hooks.
        let queue::Item {value:Request {prepare,receipt,_slot},mut charge}=item;
        if cancel.load(Ordering::Acquire) { let _=receipt.send(Err(rejected("unavailable","shutdown before commit started; no effect"))); continue; }
        let Some(core) = core.upgrade() else { let _ = receipt.send(Err(rejected("unavailable", "core stopped before commit"))); continue; };
        let prepared = {
            let c = core.lock().unwrap();
            prepare(&c).map(|plan| (c.store.committed_copy(), plan))
        };
        let (mut store, plan) = match prepared {
            Ok(work) => work,
            Err(err) => { let _ = receipt.send(Err(err)); continue; }
        };
        let expanded_bytes=match &plan.action {
            Action::Apply {author,on_behalf_of,note,ops}=>crate::limits::encoded_bytes(ops).saturating_add(crate::limits::encoded_bytes(&(author,on_behalf_of.as_deref(),note))),
            Action::Revert {author,on_behalf_of,txn}=>store.revert_bytes(*txn).unwrap_or(0).saturating_add(crate::limits::encoded_bytes(&(author,on_behalf_of.as_deref(),format!("revert {txn}")))),
        };
        if let Err(error)=charge.try_resize(expanded_bytes.max(256)) {
            let _=receipt.send(Err(rejected(if error==queue::QueueError::Closed {"unavailable"} else {"overloaded"},format!("expanded workspace mutation ({expanded_bytes} bytes) was not dispatched: {error}"))));
            continue;
        }
        let after_adopt = plan.after_adopt;
        // Candidate cloning, CAS/validation, size accounting, encoding and all filesystem
        // operations run on a blocking thread without the core mutex or an async worker.
        let written = tokio::task::spawn_blocking(move || {
            let result = match plan.action {
                Action::Apply { author, on_behalf_of, note, ops } => store.apply_attributed(&author, on_behalf_of.as_deref(), note, ops),
                Action::Revert { author, on_behalf_of, txn } => store.revert_attributed(&author, on_behalf_of.as_deref(), txn),
            };
            (store, result)
        }).await;
        let (mut store, result) = match written {
            Ok(written) => written,
            Err(err) => {
                // A panicking writer may already have renamed. Never roll back/replay or
                // continue from possibly stale memory; fail closed until core restart.
                let _ = receipt.send(Err(indeterminate(format!("workspace writer stopped: {err}; restart required"))));
                let mut receiver=queue.lock().await;
                receiver.close();
                while let Ok(item)=receiver.try_recv() { drop(item); }
                break;
            }
        };
        let adopted = result.is_ok() || result.as_ref().is_err_and(|e| e.kind() == "durability_unconfirmed");
        let (result, retired) = {
            let mut c = core.lock().unwrap();
            // A stopped embedding may retain Core for inspection; only the dispatched writer
            // must hold the lock past stop, not its late in-memory adoption afterward.
            if c.shutdown.is_stopped() { store.release_state(); }
            let retired = if adopted { Some(std::mem::replace(&mut c.store, store)) } else { None };
            (c.finish_txn(result), retired)
        };
        // The last old history/entries Arc may have substantial destruction work too.
        drop(retired);
        if adopted { if let Some(hook) = after_adopt { hook(&core); } }
        let _ = receipt.send(result.map(|txn| Core::txn_summary(&txn)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn core() -> Shared {
        Arc::new(Mutex::new(Core::new(crate::store::Store::in_memory(), None, "unused".into(), false).unwrap()))
    }
    fn prepare() -> Prepare {
        Box::new(|_| Ok(Plan { action:Action::Apply { author:"test".into(), on_behalf_of:None, note:None, ops:Vec::new() }, after_adopt:None }))
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
        let plan=||Box::new(|_:&Core|Ok(Plan { action:Action::Apply { author:"test".into(),on_behalf_of:None,note:None,ops:vec![Op::Set {key:"doc/queued".into(),value:serde_json::json!(1),expect:None}] },after_adopt:Some(hook) })) as Prepare;
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
