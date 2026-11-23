use serde_json::{Value, json};
use vqro::codex::Endpoint;
#[test]
fn malformed_legacy_containers_invalidate_retained_positive_subset_but_sparse_null_does_not() {
    for malformed in [json!("bad"), json!(false), json!([])] {
        let mut e = Endpoint::new("e".into(), "inc".into(), "h".into(), "epoch".into());
        e.notification(
            "account/rateLimits/updated",
            &json!({"rateLimits":{"primary":{"usedPercent":10}}}),
        );
        for sparse in [json!({}), json!({"rateLimits":Value::Null})] {
            e.notification("account/rateLimits/updated", &sparse);
            assert!(
                !e.facts()["data"]["agent"]["incomplete"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("usage_limits"))
            );
        }
        assert!(
            !e.notification(
                "account/rateLimits/updated",
                &json!({"rateLimits":malformed})
            )
            .is_empty()
        );
        let agent = e.facts()["data"]["agent"].clone();
        assert_eq!(agent["usage_limits"][0]["used_percent"], 10);
        assert!(
            agent["incomplete"]
                .as_array()
                .unwrap()
                .contains(&json!("usage_limits"))
        );
    }
}
