//! Optional view metadata comes only from explicit native reports, never clocks or bridge defaults.
use serde_json::json;
use vqro::codex::Endpoint;

fn endpoint() -> Endpoint {
    Endpoint::new(
        "endpoint-1".into(),
        "inc".into(),
        "host".into(),
        "e0".into(),
    )
}

#[test]
fn native_session_settings_are_read_only_and_reset_with_the_session() {
    let mut e = endpoint();
    e.set_session_response(
        &json!({"thread":{"id":"t1","cwd":"/old/native","status":{"type":"idle"}},
        "cwd":"/native/current","model":"native-model","reasoningEffort":"high"}),
        "e1".into(),
    )
    .unwrap();
    let facts = e.facts();
    let session = &facts["data"]["agent"]["session"];
    assert_eq!(session["cwd"], "/native/current");
    assert_eq!(
        session["model"],
        json!({"id":"native-model","effort":"high"})
    );
    assert_eq!(e.session_info(), *session);
    assert!(
        facts["data"]["agent"]["config"]
            .as_array()
            .unwrap()
            .is_empty(),
        "read-only model is not a configure capability"
    );
    e.set_session_response(
        &json!({"thread":{"id":"t2","status":{"type":"idle"}},"reasoningEffort":"high"}),
        "e2".into(),
    )
    .unwrap();
    let facts = e.facts();
    assert!(facts["data"]["agent"]["session"].get("cwd").is_none());
    assert!(facts["data"]["agent"]["session"].get("model").is_none());
    assert!(facts["data"]["agent"]["usage"].is_null());
}

#[test]
fn native_context_window_is_not_occupancy_or_a_price_estimate() {
    let mut e = endpoint();
    e.set_session(&json!({"id":"t1","status":{"type":"idle"}}), "e1".into())
        .unwrap();
    let changes = e.notification(
        "thread/tokenUsage/updated",
        &json!({"threadId":"t1","turnId":"r1","tokenUsage":{
        "modelContextWindow":16000,"last":{"totalTokens":555},
        "total":{"inputTokens":11,"outputTokens":3,"cachedInputTokens":0,"totalTokens":99999}}}),
    );
    let facts = e.facts();
    let usage = &facts["data"]["agent"]["usage"];
    assert_eq!(
        usage,
        &json!({"context":{"window":16000},"tokens":{"input":11,"output":3,"cache_read":0}})
    );
    assert_eq!(changes[0].value["usage"], *usage);
    assert!(usage["context"].get("used").is_none());
    assert!(usage.get("cost").is_none());
    assert!(usage.get("at_ms").is_none());
    e.notification(
        "thread/tokenUsage/updated",
        &json!({"threadId":"t1","tokenUsage":{"modelContextWindow":8000}}),
    );
    assert_eq!(
        e.facts()["data"]["agent"]["usage"],
        json!({"context":{"window":8000}})
    );
    e.set_session(&json!({"id":"t2","status":{"type":"idle"}}), "e2".into())
        .unwrap();
    assert!(e.facts()["data"]["agent"]["usage"].is_null());
}

#[test]
fn native_turn_seconds_become_run_milliseconds_without_local_clock_inference() {
    let mut e = endpoint();
    e.set_session(&json!({"id":"t1","status":{"type":"idle"}}), "e1".into())
        .unwrap();
    let started = e.notification(
        "turn/started",
        &json!({"threadId":"t1","turn":{"id":"r1","status":"inProgress","startedAt":100}}),
    );
    let run = started.iter().find(|c| c.kind == "run").unwrap();
    assert_eq!(run.value["started_ms"], 100000);
    assert!(run.value.get("ended_ms").is_none());
    let ended = e.notification(
        "turn/completed",
        &json!({"threadId":"t1","turn":{"id":"r1","status":"completed","completedAt":102}}),
    );
    let run = ended.iter().find(|c| c.kind == "run").unwrap();
    assert_eq!(run.value["started_ms"], 100000);
    assert_eq!(run.value["ended_ms"], 102000);
    assert_eq!(run.value["status"], "completed");
    let unknown = e.notification(
        "turn/started",
        &json!({"threadId":"t1","turn":{"id":"r2","status":"inProgress","startedAt":u64::MAX}}),
    );
    assert!(
        !unknown.iter().any(|c| c.kind == "run"),
        "overflow is unknown, not a fabricated timestamp"
    );
}
