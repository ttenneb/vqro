//! One bounded, cancellation-independent workspace committer. The queue owns admitted effects;
//! callers own only a receipt. CAS/validation sees the preceding adopted commit, never a stale
//! snapshot captured at admission. Only this module mutates a running Core's workspace store.
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};
use crate::{core::{Core, Shared}, queue, rpc::{RpcError, RpcResult}, store::Op};

const REQUESTS: usize = 128;
const BYTES: usize = 8 * 1024 * 1024;
type Prepare = Box<dyn FnOnce(&Core) -> Result<Plan, RpcError> + Send>;

pub(crate) enum Action {
    Apply { author:String, note:Option<String>, ops:Vec<Op> },
    Revert { author:String, txn:u64 },
}
pub(crate) struct Plan {
    pub action: Action,
    /// Policy stays with the caller; the worker owns adoption hooks so receipt cancellation
    /// cannot suppress necessary runtime reconciliation after a durable state change.
    pub after_adopt: Option<fn(&Shared)>,
}
struct Request { prepare:Prepare, receipt:oneshot::Sender<RpcResult>, _slot:OwnedSemaphorePermit }

pub(crate) struct Committer {
    sender: queue::Sender<Request>,
    start: Mutex<Option<queue::Receiver<Request>>>,
    slots: Arc<Semaphore>,
}
impl Committer {
    pub fn new() -> Self {
        let (sender, receiver) = queue::channel(BYTES);
        Self { sender, start:Mutex::new(Some(receiver)), slots:Arc::new(Semaphore::new(REQUESTS)) }
    }
    pub fn started(&self) -> bool { self.start.lock().unwrap().is_none() }
    pub fn pending(&self) -> usize { REQUESTS-self.slots.available_permits() }
    fn enqueue(&self, core:&Shared, bytes:usize, prepare:Prepare) -> Result<oneshot::Receiver<RpcResult>,RpcError> {
        let slot = self.slots.clone().try_acquire_owned().map_err(|_| rejected("overloaded", "workspace commit queue is full"))?;
        let (receipt, received) = oneshot::channel();
        // Core's owner and the idle worker have no strong-reference cycle. An active write
        // holds Core alive until adoption, even if its caller has disappeared.
        if let Some(receiver) = self.start.lock().unwrap().take() {
            tokio::spawn(run(Arc::downgrade(core), receiver));
        }
        self.sender.try_send(Request { prepare, receipt, _slot:slot }, bytes.max(256))
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
    let received = core.lock().unwrap().committer.enqueue(core, bytes, Box::new(prepare))?;
    received.await.map_err(|_| indeterminate("workspace committer stopped after admission; inspect state before retrying"))?
}
async fn run(core:Weak<Mutex<Core>>, mut queue:queue::Receiver<Request>) {
    while let Some(item) = queue.recv().await {
        // Keep both the byte charge and request slot through writing, adoption and hooks.
        let Request { prepare, receipt, _slot } = item.value;
        let Some(core) = core.upgrade() else { let _ = receipt.send(Err(rejected("unavailable", "core stopped before commit"))); continue; };
        let prepared = {
            let c = core.lock().unwrap();
            prepare(&c).map(|plan| (c.store.committed_copy(), plan))
        };
        let (mut store, plan) = match prepared {
            Ok(work) => work,
            Err(err) => { let _ = receipt.send(Err(err)); continue; }
        };
        let after_adopt = plan.after_adopt;
        // Candidate cloning, CAS/validation, size accounting, encoding and all filesystem
        // operations run on a blocking thread without the core mutex or an async worker.
        let written = tokio::task::spawn_blocking(move || {
            let result = match plan.action {
                Action::Apply { author, note, ops } => store.apply(&author, note, ops),
                Action::Revert { author, txn } => store.revert(&author, txn),
            };
            (store, result)
        }).await;
        let (store, result) = match written {
            Ok(written) => written,
            Err(err) => {
                // A panicking writer may already have renamed. Never roll back/replay or
                // continue from possibly stale memory; fail closed until core restart.
                let _ = receipt.send(Err(indeterminate(format!("workspace writer stopped: {err}; restart required"))));
                break;
            }
        };
        let adopted = result.is_ok() || result.as_ref().is_err_and(|e| e.kind() == "durability_unconfirmed");
        let (result, retired) = {
            let mut c = core.lock().unwrap();
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
        Box::new(|_| Ok(Plan { action:Action::Apply { author:"test".into(), note:None, ops:Vec::new() }, after_adopt:None }))
    }
    #[test]
    fn cancelled_receipts_and_dequeued_work_keep_the_byte_budget_until_completion() {
        let core = core(); let c = core.lock().unwrap();
        // Hold the receiver manually: test admission bookkeeping without performing effects.
        let mut receiver = c.committer.start.lock().unwrap().take().unwrap();
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
        drop(receiver);
        assert_eq!(c.committer.pending(), 0);
        assert_eq!(c.committer.enqueue(&core, 256, prepare()).unwrap_err().kind(), "unavailable");
        assert!(c.store.entries().is_empty());
    }
    #[test]
    fn metadata_admission_is_globally_count_bounded_too() {
        let core = core(); let c = core.lock().unwrap();
        let mut receiver = c.committer.start.lock().unwrap().take().unwrap();
        for _ in 0..REQUESTS { drop(c.committer.enqueue(&core, 0, prepare()).unwrap()); }
        assert_eq!(c.committer.pending(), REQUESTS);
        assert_eq!(c.committer.enqueue(&core, 0, prepare()).unwrap_err().kind(), "overloaded");
        let item = receiver.try_recv().unwrap();
        assert_eq!(c.committer.pending(), REQUESTS);
        drop(item);
        drop(c.committer.enqueue(&core, 0, prepare()).unwrap());
        assert_eq!(c.committer.pending(), REQUESTS);
        drop(receiver);
        assert_eq!(c.committer.pending(), 0);
    }
}
