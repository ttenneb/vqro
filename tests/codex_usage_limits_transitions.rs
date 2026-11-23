//! Schema-permitted legacy/map transitions must never retain two mirrors of one pool.
use serde_json::{Value, json};
use vqro::codex::Endpoint;
fn endpoint() -> Endpoint {
    Endpoint::new("e".into(), "inc".into(), "host".into(), "epoch".into())
}
fn rows(e: &Endpoint) -> Value {
    e.facts()["data"]["agent"]["usage_limits"].clone()
}
#[test]
fn legacy_map_and_back_replace_representation_without_guessing_pool_identity() {
    let mut e = endpoint();
    e.notification(
        "account/rateLimits/updated",
        &json!({"rateLimits":{"primary":{"usedPercent":80}}}),
    );
    assert_eq!(rows(&e).as_array().unwrap().len(), 1);
    e.notification("account/rateLimits/updated", &json!({
        "rateLimits":{"primary":{"usedPercent":80}},
        "rateLimitsByLimitId":{"codex":{"primary":{"usedPercent":20},"secondary":{"usedPercent":30}}}}));
    assert_eq!(rows(&e).as_array().unwrap().len(), 2);
    assert!(
        rows(&e)
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["scope"]["bucket"] == "codex")
    );
    let before = rows(&e);
    e.notification(
        "account/rateLimits/updated",
        &json!({"ordinaryUsageAllowed":null,"rateLimits":{}}),
    );
    assert_eq!(rows(&e), before);
    e.notification(
        "account/rateLimits/updated",
        &json!({"rateLimitsByLimitId":{"codex":{"primary":{"usedPercent":21}}}}),
    );
    assert_eq!(rows(&e).as_array().unwrap().len(), 2);
    assert_eq!(rows(&e)[1], before[1]);
    e.notification(
        "account/rateLimits/updated",
        &json!({"rateLimits":{"limitId":"named","primary":{"usedPercent":40}}}),
    );
    assert_eq!(rows(&e).as_array().unwrap().len(), 1);
    assert_eq!(rows(&e)[0]["scope"]["bucket"], "named");
    e.notification(
        "account/rateLimits/updated",
        &json!({"rateLimits":{"primary":{"usedPercent":50}}}),
    );
    assert_eq!(rows(&e).as_array().unwrap().len(), 1);
    assert!(rows(&e)[0]["scope"].get("bucket").is_none());
}
#[test]
fn named_legacy_supersedes_anonymous_and_empty_map_supersedes_legacy() {
    let mut e = endpoint();
    e.notification(
        "account/rateLimits/updated",
        &json!({"rateLimits":{"secondary":{"usedPercent":10}}}),
    );
    e.notification(
        "account/rateLimits/updated",
        &json!({"rateLimits":{"limitId":"codex","primary":{"usedPercent":11}}}),
    );
    assert_eq!(rows(&e).as_array().unwrap().len(), 1);
    assert_eq!(rows(&e)[0]["scope"]["bucket"], "codex");
    e.notification(
        "account/rateLimits/updated",
        &json!({"rateLimitsByLimitId":{},"rateLimits":{"primary":{"usedPercent":99}}}),
    );
    assert!(rows(&e).is_null());
}
