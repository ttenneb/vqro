//! Adapter-side semantic regressions; native wire/provider fixture coverage is separate.
use serde_json::{Value, json};
use vqro::codex::{Endpoint, RECEIPTS, capabilities};

fn endpoint() -> Endpoint {
    let mut e = Endpoint::new(
        "endpoint-1".into(),
        "inc-1".into(),
        "server-1".into(),
        "e1".into(),
    );
    e.set_session(
        &json!({"id":"thread-1","status":{"type":"idle"}}),
        "e2".into(),
    )
    .unwrap();
    e
}
fn send() -> Value {
    json!({"v":0,"mode":"admit","delivery":"d1","content":[{"type":"text","text":"/not-a-command\n\u{1b}[31m literal"}],"origin":{"principal":"p:worker","actor":"actor:worker","kind":"agent","agent":"r7"}})
}
fn active(e: &mut Endpoint, run: &str) {
    e.notification(
        "turn/started",
        &json!({"threadId":"thread-1","turn":{"id":run}}),
    );
}
fn approval(e: &mut Endpoint, native_id: Value) -> String {
    e.request(
        native_id,
        "item/commandExecution/requestApproval",
        &json!({"threadId":"thread-1","turnId":"t1","itemId":"tool-1","command":"printf ok"}),
    )
    .unwrap()
    .id
}
fn answer(key: &str) -> Value {
    json!({"v":0,"epoch":"e2","request":key,"revision":1,"response":{"choice":"accept"}})
}

#[test]
fn declares_native_admit_not_method_name_timing() {
    let c = capabilities();
    assert!(c["agent.send"]["modes"]["admit"].is_object());
    for mode in ["start", "boundary", "settle", "preempt"] {
        assert!(c["agent.send"]["modes"].get(mode).is_none());
    }
    assert_eq!(c["agent.send"]["keys"], Value::Null);
}
#[test]
fn preserves_literal_bytes_and_keeps_origin_in_user_content() {
    let e = endpoint();
    let a = send();
    let call = e.prepare_send(&a).unwrap();
    assert_eq!(call.method, "turn/start");
    assert_eq!(call.params["input"][1]["text"], a["content"][0]["text"]);
    assert!(
        call.params["input"][0]["text"]
            .as_str()
            .unwrap()
            .contains("p:worker")
    );
    assert!(call.params.get("developerInstructions").is_none());
}
#[test]
fn strict_blocks_cap_revision_epoch_mode_content_and_keys_before_call() {
    let e = endpoint();
    for (field, value, kind) in [
        ("content", json!("go"), "invalid"),
        ("content", json!({"type":"text","text":"go"}), "invalid"),
        (
            "content",
            json!([{"type":"image","mime":"image/png","data":"AA=="}]),
            "unsupported",
        ),
        ("mode", json!("boundary"), "unsupported"),
        ("caps_rev", json!(42), "stale"),
        ("epoch", json!("old"), "stale"),
        ("key", json!("key"), "unsupported"),
    ] {
        let mut a = send();
        a[field] = value;
        assert_eq!(e.prepare_send(&a).unwrap_err().kind(), kind);
    }
}
#[test]
fn run_guard_uses_atomic_native_expected_turn_not_a_new_turn() {
    let mut e = endpoint();
    active(&mut e, "t1");
    let mut a = send();
    a["guard"] = json!({"session":"thread-1","run":"t1"});
    let call = e.prepare_send(&a).unwrap();
    assert_eq!(call.method, "turn/steer");
    assert_eq!(call.params["expectedTurnId"], "t1");
    active(&mut e, "t2");
    assert_eq!(e.prepare_send(&a).unwrap_err().kind(), "stale");
}
#[test]
fn receipt_absence_and_eviction_mean_unknown_not_permission_to_replay() {
    let mut e = endpoint();
    assert_eq!(e.receipt("absent")["stage"], "unknown");
    for i in 0..RECEIPTS + 1 {
        e.record_receipt(
            &format!("d{i}"),
            "admitted",
            Some(json!({"run":"t1"})),
            &json!({}),
        );
    }
    assert_eq!(e.receipt("d0")["stage"], "unknown");
    assert_eq!(e.receipt("d1")["stage"], "admitted");
    assert_eq!(e.prepare_send(&send()).unwrap_err().kind(), "conflict");
}
#[test]
fn first_valid_answer_wins_invalid_answer_does_not_claim() {
    let mut e = endpoint();
    active(&mut e, "t1");
    let key = approval(&mut e, json!(7));
    let mut bad = answer(&key);
    bad["response"]["choice"] = json!("approveEverything");
    assert_eq!(e.prepare_answer(&bad).unwrap_err().kind(), "invalid");
    bad = answer(&key);
    bad["revision"] = json!(2);
    assert_eq!(e.prepare_answer(&bad).unwrap_err().kind(), "stale");
    let won = e.prepare_answer(&answer(&key)).unwrap();
    assert_eq!(won.native_id, 7);
    assert_eq!(won.result.unwrap()["decision"], "accept");
    assert_eq!(e.prepare_answer(&answer(&key)).unwrap_err().kind(), "stale");
}
#[test]
fn reused_native_ids_get_new_request_identity_and_cancel_is_not_decline() {
    let mut e = endpoint();
    active(&mut e, "t1");
    let old = approval(&mut e, json!(7));
    let mut a = answer(&old);
    a["response"] = json!({"action":"cancel"});
    assert_eq!(
        e.prepare_answer(&a).unwrap().result.unwrap()["decision"],
        "cancel"
    );
    let new = approval(&mut e, json!(7));
    assert_ne!(old, new);
    assert_eq!(e.prepare_answer(&answer(&old)).unwrap_err().kind(), "stale");
}
#[test]
fn form_validates_its_generated_schema_before_claim() {
    let mut e = endpoint();
    active(&mut e, "t1");
    let q=e.request(json!("q"),"item/tool/requestUserInput",&json!({"threadId":"thread-1","turnId":"t1","itemId":"x","questions":[{"id":"flavor","header":"Flavor","question":"Choose","options":[{"label":"A","description":"a"}]}]})).unwrap();
    let mut a = answer(&q.id);
    a["response"] = json!({"action":"accept","content":{"flavor":["B"]}});
    assert_eq!(e.prepare_answer(&a).unwrap_err().kind(), "invalid");
    a["response"]["content"]["flavor"] = json!(["A"]);
    assert_eq!(
        e.prepare_answer(&a).unwrap().result.unwrap(),
        json!({"answers":{"flavor":{"answers":["A"]}}})
    );
}
#[test]
fn completion_and_disconnect_are_not_endpoint_end_and_old_turn_cannot_idle_new() {
    let mut e = endpoint();
    active(&mut e, "t1");
    active(&mut e, "t2");
    e.notification(
        "turn/completed",
        &json!({"threadId":"thread-1","turn":{"id":"t1","status":"completed"}}),
    );
    assert_eq!(e.state, "running");
    e.notification(
        "turn/completed",
        &json!({"threadId":"thread-1","turn":{"id":"t2","status":"interrupted"}}),
    );
    assert_eq!(e.state, "idle");
    assert_eq!(e.lifecycle, "live");
    assert_eq!(
        e.facts()["data"]["agent"]["completion"]["reason"],
        "cancelled"
    );
    e.host_exited("process exited");
    assert_eq!(e.lifecycle, "ended");
    active(&mut e, "t3");
    assert_eq!(e.state, "unknown");
    assert_eq!(e.prepare_send(&send()).unwrap_err().kind(), "stale");
}
#[test]
fn many_endpoints_share_a_host_not_sessions_or_request_identity() {
    let mut a = endpoint();
    let b = Endpoint::new(
        "endpoint-2".into(),
        "inc-1".into(),
        a.host.clone(),
        "e3".into(),
    );
    let same = a.reference.clone();
    a.set_session(
        &json!({"id":"thread-2","status":{"type":"idle"}}),
        "e4".into(),
    )
    .unwrap();
    assert_eq!(a.reference, same);
    assert_ne!(a.session, b.session);
    assert_eq!(
        a.facts()["data"]["agent"]["host"],
        b.facts()["data"]["agent"]["host"]
    );
}
#[test]
fn unknown_and_oversized_requests_cannot_be_approved() {
    let mut e = endpoint();
    active(&mut e, "t1");
    assert_eq!(
        e.request(json!(1), "unknown/method", &json!({"threadId":"thread-1"}))
            .unwrap_err()
            .kind(),
        "unsupported"
    );
    assert_eq!(
        e.request(
            json!(2),
            "item/fileChange/requestApproval",
            &json!({"threadId":"thread-1","reason":"x".repeat(32768)})
        )
        .unwrap_err()
        .kind(),
        "unsupported"
    );
}
