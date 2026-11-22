//! Native fake notifications only: no credentials, profiles or installed runtime.
use serde_json::{Value, json};
use vqro::codex::Endpoint;

fn endpoint() -> Endpoint {
    let mut e = Endpoint::new("e".into(), "inc".into(), "host".into(), "epoch".into());
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
fn native_windows_preserve_scope_and_convert_only_units() {
    let mut e = endpoint();
    assert!(limits(&e).is_null());
    let changes = e.notification(
        "account/rateLimits/updated",
        &json!({
        "accountId":"native-a", "rateLimits":{"limitId":"codex", "primary":{
            "usedPercent":42,"windowDurationMins":300,"resetsAt":1800000000},
            "secondary":{"usedPercent":3}}, "secret":"never copy"}),
    );
    assert!(!changes.is_empty());
    let rows = limits(&e);
    assert_eq!(rows.as_array().unwrap().len(), 2);
    assert_eq!(
        rows[0]["scope"],
        json!({"kind":"account","native_account_id":"native-a","bucket":"codex"})
    );
    assert_eq!(
        rows[0]["window"],
        json!({"id":"primary","duration_ms":18000000})
    );
    assert_eq!(rows[0]["used_percent"], 42);
    assert_eq!(rows[0]["resets_at_ms"], 1800000000000_i64);
    assert!(rows[0]["observed_at_ms"].as_u64().is_some());
    assert!(rows[0].get("remaining").is_none());
    assert!(rows[0]["scope"].get("account").is_none());
    assert!(rows[1].get("resets_at_ms").is_none());
    assert!(!rows.to_string().contains("secret"));
}

#[test]
fn sparse_push_preserves_other_windows_and_account_change_drops_old_facts() {
    let mut e = endpoint();
    e.notification("account/rateLimits/updated", &json!({"accountId":"a","rateLimits":{
        "limitId":"codex","primary":{"usedPercent":50,"resetsAt":100},"secondary":{"usedPercent":60}}}));
    let before = limits(&e);
    e.notification(
        "account/rateLimits/updated",
        &json!({"ordinaryUsageAllowed":null,"rateLimits":null}),
    );
    assert_eq!(limits(&e), before);
    e.notification(
        "account/rateLimits/updated",
        &json!({"rateLimits":{"limitId":"codex","primary":{"usedPercent":51}}}),
    );
    let rows = limits(&e);
    assert_eq!(rows[0]["used_percent"], 51);
    assert_eq!(rows[0]["resets_at_ms"], 100000);
    assert_eq!(rows[1], before[1]);
    e.notification("account/updated", &json!({"authMode":"chatgpt"}));
    assert!(limits(&e).is_null());
}

#[test]
fn multibucket_deduplicates_legacy_and_rejects_bad_numbers() {
    let mut e = endpoint();
    e.notification("account/rateLimits/updated", &json!({
        "rateLimits":{"limitId":"codex","primary":{"usedPercent":99}},
        "rateLimitsByLimitId":{"codex":{"primary":{"usedPercent":7}},
          "other":{"primary":{"usedPercent":-1},"secondary":{"usedPercent":10,"resetsAt":9223372036854775807_i64}}}}));
    let rows = limits(&e);
    assert_eq!(rows.as_array().unwrap().len(), 2);
    assert_eq!(rows[0]["used_percent"], 7);
    assert!(rows[1].get("resets_at_ms").is_none());
    e.set_session(
        &json!({"id":"s2","status":{"type":"idle"}}),
        "epoch2".into(),
    )
    .unwrap();
    assert!(limits(&e).is_null());
}
