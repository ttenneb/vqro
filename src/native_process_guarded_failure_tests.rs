//! Failure classification at the actual memory writer commit. No runtime subprocesses.
use super::*;

async fn notice_failure(guarded_claude: bool, committed: bool) {
    let (state, mut receiver) = inner();
    state.dead.store(false, Ordering::SeqCst);
    state.exit.send_replace(None);
    let process = Process {
        inner: state.clone(),
    };
    let notice = tokio::spawn(async move {
        let deadline = Instant::now() + Duration::from_secs(3);
        if guarded_claude {
            process
                .notify_guarded(
                    "control_response",
                    serde_json::json!({"id":"q"}),
                    deadline,
                    WriteToken::new(),
                )
                .await
        } else {
            process
                .notify(
                    "extension_ui_response",
                    serde_json::json!({"id":"q"}),
                    deadline,
                )
                .await
        }
    });
    // The public caller has enqueued the real line, but the byte writer is not running yet.
    let write = receiver.recv().await.unwrap();
    let capacity = if committed { 1 } else { 1024 };
    let (a, mut b) = tokio::io::duplex(capacity);
    let mut prefix = Vec::new();
    let writer = if committed {
        state
            .writes
            .try_send(write)
            .unwrap_or_else(|_| panic!("fixture queue full"));
        let writer = tokio::spawn(write_native(a, receiver, state.clone()));
        let mut byte = [0];
        b.read_exact(&mut byte).await.unwrap();
        prefix.extend_from_slice(&byte);
        state.protocol_error("synthetic protocol break while async stop is delayed");
        let error = notice.await.unwrap().unwrap_err();
        assert!(
            error.may_have_acted,
            "actual writer won commit; notice outcome must remain unknown"
        );
        writer
    } else {
        state.protocol_error("synthetic protocol break before writer commit; async stop delayed");
        let error = notice.await.unwrap().unwrap_err();
        assert!(!error.may_have_acted);
        // AFTER that no-effect return, allow the still-alive byte writer to run the old queue.
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
    b.read_to_end(&mut prefix).await.unwrap();
    if committed {
        assert!(!prefix.is_empty());
    } else {
        assert!(
            prefix.is_empty(),
            "reported NO EFFECT but queued notice reached actual native bytes AFTER the return"
        );
    }
}
#[tokio::test]
async fn claude_guarded_no_effect_revokes_before_later_writer() {
    notice_failure(true, false).await;
}
#[tokio::test]
async fn claude_guarded_committed_failure_remains_unknown() {
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
    state.dead.store(false, Ordering::SeqCst);
    state.exit.send_replace(None);
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
            state.protocol_error("synthetic ticket failure after commit");
        }
        assert!(ticket.reply().await.unwrap_err().may_have_acted);
        writer
    } else {
        if drop_sender {
            state.pending.lock().unwrap().remove(&ticket.id);
        } else {
            state.protocol_error("synthetic ticket failure before commit");
        }
        assert!(!ticket.reply().await.unwrap_err().may_have_acted);
        // Keep the ticket alive, just as a caller retaining a late-receipt handle may do.
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
