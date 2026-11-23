//! Integration of native limits with the separately configured launcher account seam.
use serde_json::json;
use vqro::rpc::Incoming;
#[path = "support/codex_account.rs"]
mod codex_account;

#[tokio::test]
async fn configured_account_links_every_public_fact_path_without_replacing_native_identity() {
    for configured in [false, true] {
        let account = configured.then(|| {
            json!({"id":"configured-work","label":"Work","kind":"subscription",
            "sources":{"id":"vqro:launcher","label":"vqro:launcher","kind":"vqro:launcher"}})
        });
        let runtime = codex_account::runtime(account).await;
        runtime.incoming(Incoming::Notification {
            method: "account/rateLimits/updated".into(),
            params: json!({"accountId":"native-workspace","rateLimits":{"limitId":"codex",
                "primary":{"usedPercent":12,"windowDurationMins":300}}}),
        });
        let mut facts = runtime.facts();
        facts.push(runtime.fact("endpoint-1").unwrap());
        facts.extend(runtime.take_dirty_facts());
        assert!(facts.len() >= 3);
        for fact in facts {
            let scope = &fact["data"]["agent"]["usage_limits"][0]["scope"];
            assert_eq!(scope["native_account_id"], "native-workspace");
            assert_eq!(scope["bucket"], "codex");
            if configured {
                assert_eq!(scope["account"], "configured-work");
                assert_eq!(scope["account"], fact["data"]["agent"]["account"]["id"]);
            } else {
                assert!(scope.get("account").is_none());
                assert!(fact["data"]["agent"].get("account").is_none());
            }
        }
        runtime.native.close();
    }
}
