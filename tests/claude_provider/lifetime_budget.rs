//! Actual SDK budget/channel seam, no core hooks or scheduling sleeps.
use super::*;
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_lookup_budget_stall_obeys_effect_deadline_and_releases_lifetime_stop() {
    use std::{
        future::{Future, poll_fn},
        task::Poll,
    };
    let shared = Shared::new(
        "budget-boot".into(),
        "claude-fixture".into(),
        "fixture-instance".into(),
    );
    {
        let mut state = shared.state.lock().unwrap();
        state.initialized(&json!({"claude_code_version":"2.1.329"}));
        state.observe(&vqro::native_process::Event::Record(
            json!({"type":"system","subtype":"init","session_id":"synthetic-session"}),
        ));
    }
    // One permit: the first complete upsert owns ALL budget until the capacity-one peer drains
    // its line. This peer deliberately NEVER reads it; the next enqueue is provably budget-blocked.
    // Reply timeout60s is irrelevant: the owner RPC cannot reach its reply timer at all.
    let (provider, mut requests, mut core) =
        registered_with_budget(&shared, 1, Duration::from_secs(60), 1).await;
    provider.upsert(shared.facts()).await.unwrap();
    let facts = shared.facts();
    let mut held = Box::pin(provider.upsert(facts));
    assert!(
        poll_fn(|cx| Poll::Ready(held.as_mut().poll(cx).is_pending())).await,
        "fixture must hold actual SDK outgoing budget BEFORE RPC timer"
    );
    let (sender, receiver) = tokio::sync::watch::channel(false);
    shared.set_lifetime(receiver.clone());
    let native = shared
        .spawn_native(
            &[
                "/usr/bin/python3".into(),
                "-c".into(),
                "import sys; sys.stdin.buffer.read()".into(),
            ],
            &std::env::current_dir().unwrap(),
            &HashMap::new(),
        )
        .await
        .unwrap();
    let incarnation = shared.state.lock().unwrap().incarnation().to_owned();
    core.send(json!({"jsonrpc":"2.0","id":777,"method":"effect.invoke","params":{"generation":7,"effect":"owner-budget","op":"agent.send","ref":"agent","incarnation":incarnation,"args":send_args(&shared,"budget-delivery",vec![json!({"type":"text","text":"must never dispatch"})]),"deadline_ms":100,"author":"fixture-client","actor":"client:fixture","principal":"owner"}})).await;
    let Request::Effect(effect) = requests.recv().await.unwrap() else {
        panic!("expected owned effect request")
    };
    let mut invoke = Box::pin(shared.invoke(&provider, &effect));
    let first = poll_fn(|cx| Poll::Ready(invoke.as_mut().poll(cx))).await;
    let was_blocked = first.is_pending();
    let mut ending = Box::pin(shared.end_lifetime(&sender));
    let end_pending = poll_fn(|cx| Poll::Ready(ending.as_mut().poll(cx).is_pending())).await;
    let result = match first {
        Poll::Ready(result) => Ok(result),
        Poll::Pending => tokio::time::timeout(Duration::from_secs(1), invoke.as_mut()).await,
    };
    drop(invoke); // baseline RED cleanup must release the guard even after the outer control expires
    if end_pending {
        let _ = tokio::time::timeout(Duration::from_secs(2), ending).await;
    }
    let publication_stopped = tokio::time::timeout(Duration::from_secs(2), native.exited()).await;
    native.stop();
    let _ = tokio::time::timeout(Duration::from_secs(2), native.exited()).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), native.drained()).await;
    let ended = *receiver.borrow();
    drop(held);
    provider.close(); // only AFTER proving stop: never unblock budget to manufacture green
    assert!(
        was_blocked && end_pending,
        "owner lookup must hold admission while the real budget is blocked"
    );
    let outcome=result.expect("WHOLE owner lookup must obey effect deadline, including SDK budget wait; otherwise lifetime stop is unbounded").unwrap();
    assert!(
        matches!(outcome,vqro::provider_client::Outcome::Rejected{ref kind,..} if kind=="timeout")
    );
    assert!(ended);
    assert!(matches!(
        publication_stopped,
        Ok(vqro::native_process::Exit { observed: true, .. })
    ));
    assert_eq!(
        native.pending(),
        0,
        "no native input/control call was admitted"
    );
}
