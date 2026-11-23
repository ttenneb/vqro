//! Shared producer DTO -> normalized facts golden, also consumed by capacity policy tests.
use serde_json::{Value, json};
use vqro::codex::{Endpoint, runtime::Runtime};
use vqro::rpc::{Incoming, Peer};

#[tokio::test]
async fn shared_native_dto_marks_positive_subset_unknown_for_capacity() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/usage_limits_incomplete.json")).unwrap();
    for (dto, expected) in [("dto", "expected"), ("complete_dto", "complete_expected")] {
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
            Some(fixture[expected]["account"].clone()),
        );
        runtime.incoming(Incoming::Notification {
            method: "account/rateLimits/updated".into(),
            params: fixture[dto].clone(),
        });
        let facts = runtime.fact("endpoint-1").unwrap();
        let agent = &facts["data"]["agent"];
        let mut actual = json!({"account":agent["account"],"usage_limits":agent["usage_limits"],"incomplete":agent["incomplete"]});
        for row in actual["usage_limits"].as_array_mut().unwrap() {
            row["observed_at_ms"] = json!(0);
        }
        assert_eq!(actual, fixture[expected]);
        runtime.native.close();
    }
}

#[test]
fn row_and_bucket_bounds_and_malformed_windows_mark_inventory_incomplete() {
    for mode in ["rows", "buckets", "malformed"] {
        let mut e = Endpoint::new("e".into(), "inc".into(), "h".into(), "epoch".into());
        let mut buckets = serde_json::Map::new();
        let count = if mode == "buckets" {
            33
        } else if mode == "rows" {
            17
        } else {
            1
        };
        for n in 0..count {
            buckets.insert(
                format!("bucket-{n:02}"),
                json!({"primary":{"usedPercent":10},"secondary":{"usedPercent":20}}),
            );
        }
        if mode == "malformed" {
            buckets["bucket-00"]["secondary"]["usedPercent"] = json!("bad");
        }
        let changes = e.notification(
            "account/rateLimits/updated",
            &json!({"rateLimitsByLimitId":buckets}),
        );
        assert!(!changes.is_empty());
        let agent = e.facts()["data"]["agent"].clone();
        assert!(
            agent["incomplete"]
                .as_array()
                .unwrap()
                .contains(&json!("usage_limits")),
            "{mode}"
        );
        assert!(agent["usage_limits"].as_array().unwrap().len() <= 32);
        e.notification(
            "account/rateLimits/updated",
            &json!({"rateLimitsByLimitId":{"bucket-00":{"primary":{"usedPercent":0}}}}),
        );
        assert!(
            e.facts()["data"]["agent"]["incomplete"]
                .as_array()
                .unwrap()
                .contains(&json!("usage_limits"))
        );
    }
}
