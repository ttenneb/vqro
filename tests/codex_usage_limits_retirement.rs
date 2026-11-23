//! A sparse representation replacement must not turn restrictive native evidence into capacity.
use serde_json::json;
use vqro::codex::runtime::Runtime;
use vqro::rpc::{Incoming, Peer};
#[tokio::test]
async fn sparse_same_id_legacy_after_restrictive_map_marks_public_inventory_unknown() {
    let (io, _other) = tokio::io::duplex(1024);
    let (read, write) = tokio::io::split(io);
    let (peer, _) = Peer::spawn(read, write);
    let runtime = Runtime::with_host_metadata(
        peer,
        1,
        "inc".into(),
        "fixture".into(),
        "instance".into(),
        None,
        None,
        Some(
            json!({"id":"work","label":"Work","kind":"subscription","sources":{"id":"vqro:launcher","label":"vqro:launcher","kind":"vqro:launcher"}}),
        ),
    );
    runtime.incoming(Incoming::Notification {
        method: "account/rateLimits/updated".into(),
        params: json!({
        "accountId":"native-workspace","rateLimitsByLimitId":{"codex":{
            "primary":{"usedPercent":10},"secondary":{"usedPercent":100}}}}),
    });
    assert!(
        !runtime.fact("endpoint-1").unwrap()["data"]["agent"]["incomplete"]
            .as_array()
            .unwrap()
            .contains(&json!("usage_limits"))
    );
    runtime.incoming(Incoming::Notification {method:"account/rateLimits/updated".into(),params:json!({
        "accountId":"native-workspace","rateLimits":{"limitId":"codex","primary":{"usedPercent":11}}})});
    let mut facts = runtime.facts();
    facts.push(runtime.fact("endpoint-1").unwrap());
    facts.extend(runtime.take_dirty_facts());
    for fact in facts {
        let agent = &fact["data"]["agent"];
        assert!(
            agent["incomplete"]
                .as_array()
                .unwrap()
                .contains(&json!("usage_limits"))
        );
        assert_eq!(agent["usage_limits"][0]["scope"]["account"], "work");
    }
    runtime.native.close();
}
