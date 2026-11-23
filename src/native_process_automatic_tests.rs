//! Actual memory writer + automatic consent reducer fence: no native process or credentials.
use super::*;
use crate::claude_code::requests::Requests;

struct FlushFailure(Arc<Mutex<Vec<u8>>>);
impl AsyncWrite for FlushFailure {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        std::task::Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "synthetic post-write flush failure",
        )))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}
async fn automatic_reply_fence(fail_flush: bool) {
    let mut book = Requests::default();
    let mut frame = serde_json::json!({"type":"control_request","request_id":"automatic-1","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"tool-automatic","input":{"command":"first"},"requires_user_interaction":true}});
    let outgoing = book.observe(&frame, None).outgoing.remove(0);
    let token = outgoing.token;
    let mut line = serde_json::to_vec(&outgoing.frame).unwrap();
    line.push(b'\n');
    let (state, receiver) = inner();
    state.exit.send_replace(None);
    state.dead.store(false, Ordering::SeqCst);
    let permit = state.budget.clone().acquire_many_owned(64).await.unwrap();
    state
        .writes
        .try_send(Write {
            id: None,
            line: line.clone(),
            deadline: Instant::now() + Duration::from_secs(3),
            phase: token.phase.clone(),
            _permit: permit,
        })
        .unwrap_or_else(|_| panic!("fixture queue full"));
    if fail_flush {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        write_native(FlushFailure(bytes.clone()), receiver, state.clone()).await;
        assert_eq!(
            *bytes.lock().unwrap(),
            line,
            "the FULL automatic error was written before flush failure"
        );
        assert!(state.broken.load(Ordering::SeqCst));
        assert_eq!(token.phase.load(Ordering::SeqCst), WRITING);
    } else {
        let (a, mut b) = tokio::io::duplex(1024);
        let writer = tokio::spawn(write_native(a, receiver, state.clone()));
        let mut bytes = vec![0; line.len()];
        b.read_exact(&mut bytes).await.unwrap();
        let _released = state.budget.clone().acquire_many_owned(64).await.unwrap();
        assert_eq!(bytes, line);
        assert_eq!(token.phase.load(Ordering::SeqCst), WRITTEN);
        writer.abort();
        let _ = writer.await;
    }
    assert!(token.write_begun());
    assert!(
        !token.cancel_before_write(),
        "possible write is not revocable"
    );
    frame["request"]["requires_user_interaction"] = serde_json::json!(false);
    frame["request"]["input"]["command"] = serde_json::json!("changed supported question");
    let reused = book.observe(&frame, None);
    assert!(
        reused.changes.is_empty(),
        "possibly-written AUTOMATIC error cannot mint a supported same-ID request after changed payload"
    );
    assert!(reused.outgoing.is_empty());
    assert!(book.summaries().is_empty());
    assert!(book.has_unconfirmed());
    book.expire();
    assert!(
        book.observe(&frame, None).changes.is_empty(),
        "a new observation epoch is not native settlement"
    );
    let mut unsupported = frame.clone();
    unsupported["request"]["requires_user_interaction"] = serde_json::json!(true);
    assert!(
        book.observe(&unsupported, None).outgoing.is_empty(),
        "changed unsupported payload cannot replay another automatic response"
    );
    if fail_flush {
        book.tool_started("tool-automatic");
    } else {
        book.observe(
            &serde_json::json!({"type":"control_cancel_request","request_id":"automatic-1"}),
            None,
        );
    }
    assert!(!book.has_unconfirmed());
    assert_eq!(
        book.observe(&frame, None).changes.len(),
        1,
        "independent native settlement permits a new incarnation"
    );
    assert_eq!(book.summaries().len(), 1);
}
#[tokio::test]
async fn automatic_response_written_same_id_never_rearms() {
    automatic_reply_fence(false).await;
}
#[tokio::test]
async fn automatic_response_post_write_failure_same_id_never_rearms() {
    automatic_reply_fence(true).await;
}
