use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use vqro::codex::{
    Change, Endpoint, REQUEST_BYTES,
    runtime::{Observer, Runtime},
};
use vqro::rpc::{Incoming, Peer};

#[test]
fn cancellation_notice_is_bounded_system_observation_not_an_answerable_question() {
    let mut endpoint = Endpoint::new("slot".into(), "i".into(), "host".into(), "e1".into());
    endpoint
        .set_session(&json!({"id":"s","status":{"type":"idle"}}), "e2".into())
        .unwrap();
    assert!(
        endpoint
            .request(
                json!(7),
                "unknown/native",
                &json!({"threadId":"s","secret":"never-copy"})
            )
            .is_err()
    );
    let notice = endpoint.request_cancellation_notice();
    assert_eq!(notice.kind, "message");
    assert_eq!(notice.value["role"], "system");
    assert_eq!(notice.value["notice"]["cancellation"], "requested");
    assert_eq!(notice.value["notice"]["native_outcome"], "unknown");
    assert_eq!(notice.value["final"], true);
    let encoded = serde_json::to_string(&notice.value).unwrap();
    assert!(encoded.len() < 1024 && !encoded.contains("never-copy"));
    assert!(
        endpoint
            .prepare_answer(&json!({"v":0,"epoch":"e2","request":notice.id,
        "revision":1,"response":{"choice":"accept"}}))
            .is_err()
    );
    assert!(
        endpoint.facts()["data"]["agent"]["requests"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_reused_pending_id_is_a_protocol_fault_even_when_the_new_body_is_oversized() {
    let mut endpoint = Endpoint::new("slot".into(), "i".into(), "host".into(), "e1".into());
    endpoint
        .set_session(&json!({"id":"s","status":{"type":"idle"}}), "e2".into())
        .unwrap();
    let original = endpoint
        .request(
            json!(7),
            "item/commandExecution/requestApproval",
            &json!({"threadId":"s","turnId":"t","itemId":"i"}),
        )
        .unwrap();
    let error = endpoint
        .request(
            json!(7),
            "unknown/native",
            &json!({"threadId":"s","oversized":"x".repeat(REQUEST_BYTES)}),
        )
        .unwrap_err();
    assert_eq!(
        error.kind(),
        "stale",
        "never cancel the original pending native id as an unrepresentable new request"
    );
    assert!(
        endpoint
            .prepare_answer(&json!({"v":0,"epoch":"e2","request":original.id,
        "revision":1,"response":{"choice":"accept"}}))
            .is_ok()
    );
}

#[tokio::test]
async fn an_unbound_host_request_is_cancelled_once_and_reported_without_copying_auth_params() {
    let (local, remote) = tokio::io::duplex(1 << 16);
    let (read, write) = tokio::io::split(local);
    let (peer, _incoming) = Peer::spawn(read, write);
    let seen = Arc::new(Mutex::new(Vec::<(usize, Change)>::new()));
    let recorded = seen.clone();
    let observer: Observer = Arc::new(move |index, _epoch, changes, _reset| {
        recorded
            .lock()
            .unwrap()
            .extend(changes.into_iter().map(|c| (index, c)));
    });
    let runtime = Runtime::with_host(
        peer.clone(),
        2,
        "i".into(),
        "codex-test".into(),
        "test".into(),
        Some(observer),
    );
    seen.lock().unwrap().clear();
    runtime.incoming(Incoming::Request {
        id: json!(77),
        method: "unknown/authRequest".into(),
        params: json!({"credential":"never-copy-auth-material","nested":{"token":"nor-this"}}),
    });
    let (read, _write) = tokio::io::split(remote);
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    let cancel: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(cancel["id"], 77);
    assert_eq!(cancel["error"]["code"], -32800);
    assert!(
        cancel.get("result").is_none(),
        "cancellation is never approval"
    );
    line.clear();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), reader.read_line(&mut line))
            .await
            .is_err(),
        "one native response only"
    );
    let notices = seen.lock().unwrap();
    assert_eq!(
        notices.len(),
        2,
        "one shared-host notice per endpoint, not invented scoped questions"
    );
    for (index, notice) in notices.iter() {
        assert!(*index < 2);
        assert_eq!(notice.kind, "message");
        assert_eq!(notice.value["role"], "system");
        assert_eq!(notice.value["notice"]["scope"], "host");
        let value = serde_json::to_string(&notice.value).unwrap();
        assert!(!value.contains("never-copy") && !value.contains("nor-this"));
    }
    assert!(runtime.facts().iter().all(|f| {
        f["data"]["agent"]["requests"]
            .as_array()
            .unwrap()
            .is_empty()
    }));
    peer.close();
}
