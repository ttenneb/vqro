//! Usage facts remain bounded and never cross a native account or ended epoch.
use serde_json::{Value, json};
use vqro::codex::Endpoint;
fn endpoint() -> Endpoint {
    let mut e = Endpoint::new("e".into(), "inc".into(), "h".into(), "epoch".into());
    e.set_session(
        &json!({"id":"s1","status":{"type":"idle"}}),
        "epoch1".into(),
    )
    .unwrap();
    e
}
fn limits(e: &Endpoint) -> Value {
    e.facts()["data"]["agent"]["usage_limits"].clone()
}
#[test]
fn retention_is_bounded_and_native_account_changes_do_not_keep_old_windows() {
    let mut e = endpoint();
    for n in 0..100 {
        e.notification(
            "account/rateLimits/updated",
            &json!({"accountId":"a","rateLimitsByLimitId":{
            (format!("bucket-{n}")):{"primary":{"usedPercent":3}}}}),
        );
    }
    assert_eq!(limits(&e).as_array().unwrap().len(), 32);
    e.notification(
        "account/rateLimits/updated",
        &json!({"accountId":"b","rateLimits":{
        "limitId":"new","secondary":{"usedPercent":4}}}),
    );
    assert_eq!(limits(&e).as_array().unwrap().len(), 1);
    assert_eq!(limits(&e)[0]["scope"]["native_account_id"], "b");
    e.host_exited("EOF");
    let before = e.facts();
    e.notification(
        "account/rateLimits/updated",
        &json!({"rateLimits":{"primary":{"usedPercent":0}}}),
    );
    assert_eq!(e.facts(), before);
    assert!(limits(&e).is_null());
}
#[test]
fn malformed_windows_and_identifiers_never_publish_arbitrary_fields() {
    let mut e = endpoint();
    for value in [json!(-1), json!(101), json!("7"), Value::Null, json!(false)] {
        e.notification(
            "account/rateLimits/updated",
            &json!({"rateLimits":{"primary":{"usedPercent":value}}}),
        );
        assert!(limits(&e).is_null());
    }
    e.notification("account/rateLimits/updated", &json!({"accountId":"bad\nidentity","rateLimits":{
        "limitId":"x".repeat(1000),"primary":{"usedPercent":10,"resetsAt":-1,"windowDurationMins":-1}}}));
    let rows = limits(&e);
    assert!(rows[0]["scope"].get("native_account_id").is_none());
    assert!(rows[0]["scope"].get("bucket").is_none());
    assert!(rows[0].get("resets_at_ms").is_none());
    assert!(rows[0]["window"].get("duration_ms").is_none());
}
