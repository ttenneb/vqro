//! Actual memory writer failure boundaries; no subprocess, native runtime or credentials.
use super::*;
#[test]
fn only_revoked_or_already_cancelled_phases_prove_no_effect() {
    let phase = AtomicU8::new(QUEUED);
    assert!(revoke_unwritten(&phase));
    assert_eq!(phase.load(Ordering::SeqCst), CANCELLED);
    assert!(revoke_unwritten(&phase));
    for value in [WRITING, WRITTEN, REPLIED, u8::MAX] {
        let phase = AtomicU8::new(value);
        assert!(!revoke_unwritten(&phase));
        assert_eq!(phase.load(Ordering::SeqCst), value);
    }
}
struct CodecStub;
impl Codec for CodecStub {
    fn name(&self) -> &'static str {
        "synthetic"
    }
    fn request(&self, _: &str, _: &str, args: Value) -> Result<Value, String> {
        Ok(args)
    }
    fn reply_id(&self, _: &Value) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn reply(&self, _: &Value, _: &str) -> Result<Reply, String> {
        unreachable!()
    }
}
fn inner() -> (Arc<Inner>, mpsc::Receiver<Write>) {
    let (writes, receiver) = mpsc::channel(4);
    let (stop, _) = watch::channel(false);
    let (exit, _) = watch::channel(None);
    (
        Arc::new(Inner {
            pending: Mutex::new(HashMap::new()),
            writes,
            budget: Arc::new(Semaphore::new(64)),
            serial: AtomicU64::new(1),
            dead: AtomicBool::new(false),
            broken: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            stop,
            exit,
            stderr: Mutex::new(VecDeque::new()),
            stderr_total: AtomicU64::new(0),
            observe: Arc::new(|_| {}),
            codec: Arc::new(CodecStub),
        }),
        receiver,
    )
}
async fn notice_failure(claude_envelope: bool, committed: bool) {
    let (state, mut receiver) = inner();
    let process = Process {
        inner: state.clone(),
    };
    // Both callers use this shared typed-notification API. Claude's adapter/guarded token is
    // qualified separately when replayed; this main-based control adds no adapter dependency.
    let notice = tokio::spawn(async move {
        process
            .notify(
                if claude_envelope {
                    "control_response"
                } else {
                    "extension_ui_response"
                },
                serde_json::json!({"id":"q"}),
                Instant::now() + Duration::from_secs(3),
            )
            .await
    });
    let write = receiver.recv().await.unwrap(); // Queued, but byte writer not started.
    let (a, mut b) = tokio::io::duplex(if committed { 1 } else { 1024 });
    let mut bytes = Vec::new();
    let writer = if committed {
        state
            .writes
            .try_send(write)
            .unwrap_or_else(|_| panic!("fixture queue full"));
        let writer = tokio::spawn(write_native(a, receiver, state.clone()));
        let mut byte = [0];
        b.read_exact(&mut byte).await.unwrap();
        bytes.extend_from_slice(&byte);
        state.protocol_error("synthetic protocol break while asynchronous stop is delayed");
        assert!(
            notice.await.unwrap().unwrap_err().may_have_acted,
            "writer won commit; notice outcome must remain unknown"
        );
        writer
    } else {
        state.protocol_error("synthetic protocol break before writer commit; async stop delayed");
        assert!(!notice.await.unwrap().unwrap_err().may_have_acted);
        // Only AFTER the no-effect return do we allow the still-alive byte writer to run.
        state
            .writes
            .try_send(write)
            .unwrap_or_else(|_| panic!("fixture queue full"));
        let writer = tokio::spawn(write_native(a, receiver, state.clone()));
        let _released = state.budget.clone().acquire_many_owned(64).await.unwrap();
        writer
    };
    writer.abort();
    let _ = writer.await;
    b.read_to_end(&mut bytes).await.unwrap();
    if committed {
        assert!(!bytes.is_empty());
    } else {
        assert!(
            bytes.is_empty(),
            "reported NO EFFECT but queued notice reached actual native bytes AFTER the return"
        );
    }
}
#[tokio::test]
async fn claude_notice_no_effect_revokes_before_later_writer() {
    notice_failure(true, false).await;
}
#[tokio::test]
async fn claude_notice_committed_failure_remains_unknown() {
    notice_failure(true, true).await;
}
#[tokio::test]
async fn pi_notify_no_effect_revokes_before_later_writer() {
    notice_failure(false, false).await;
}
#[tokio::test]
async fn pi_notify_committed_failure_remains_unknown() {
    notice_failure(false, true).await;
}

async fn ticket_failure(committed: bool, drop_sender: bool) {
    let (state, mut receiver) = inner();
    let process = Process {
        inner: state.clone(),
    };
    let mut ticket = process
        .submit(
            "synthetic",
            serde_json::json!({}),
            Instant::now() + Duration::from_secs(3),
        )
        .unwrap();
    let write = receiver.recv().await.unwrap();
    let (a, mut b) = tokio::io::duplex(if committed { 1 } else { 1024 });
    let mut bytes = Vec::new();
    let writer = if committed {
        state
            .writes
            .try_send(write)
            .unwrap_or_else(|_| panic!("fixture queue full"));
        let writer = tokio::spawn(write_native(a, receiver, state.clone()));
        let mut byte = [0];
        b.read_exact(&mut byte).await.unwrap();
        bytes.extend_from_slice(&byte);
        if drop_sender {
            state.pending.lock().unwrap().remove(&ticket.id);
        } else {
            state.protocol_error("synthetic pending failure after commit");
        }
        assert!(ticket.reply().await.unwrap_err().may_have_acted);
        writer
    } else {
        if drop_sender {
            state.pending.lock().unwrap().remove(&ticket.id);
        } else {
            state.protocol_error("synthetic pending failure before commit");
        }
        assert!(!ticket.reply().await.unwrap_err().may_have_acted);
        // Keep the ticket alive as a caller retaining a late-receipt handle may do.
        state
            .writes
            .try_send(write)
            .unwrap_or_else(|_| panic!("fixture queue full"));
        let writer = tokio::spawn(write_native(a, receiver, state.clone()));
        let _released = state.budget.clone().acquire_many_owned(64).await.unwrap();
        writer
    };
    writer.abort();
    let _ = writer.await;
    b.read_to_end(&mut bytes).await.unwrap();
    if committed {
        assert!(!bytes.is_empty());
    } else {
        assert!(
            bytes.is_empty(),
            "reported NO EFFECT but queued ticket reached actual native bytes AFTER the failure return"
        );
    }
    drop(ticket);
}
#[tokio::test]
async fn pending_failure_no_effect_revokes_before_later_writer() {
    ticket_failure(false, false).await;
}
#[tokio::test]
async fn pending_failure_committed_remains_unknown() {
    ticket_failure(true, false).await;
}
#[tokio::test]
async fn closed_reply_no_effect_revokes_before_later_writer() {
    ticket_failure(false, true).await;
}
#[tokio::test]
async fn closed_reply_committed_remains_unknown() {
    ticket_failure(true, true).await;
}
