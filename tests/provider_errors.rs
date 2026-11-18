//! Wire provider errors reach callers with the right certainty (provider-v0 9.1, 9.4): protocol errors
//! mean no effect (outcome rejected, the provider's kind); a provider's `backend` error stays
//! indeterminate. A real in-process core, with vqro::provider_client as the provider.

use std::time::Duration;

use vqro::client;
use vqro::provider_client::{Config, Outcome, Provider, ProviderError, Request};
use vqro::rpc::Peer;
use vqro::server::{start, Backend};
use serde_json::{json, Value};

/// Core + a wire provider "wire" with one live resource. Requests are answered by `answer`.
async fn setup(answer: fn(&str, &Value) -> Result<Outcome, ProviderError>) -> (tempfile::TempDir, vqro::core::Shared, Peer, String) {
    let dir = tempfile::Builder::new().prefix("ppe").tempdir_in("/tmp").unwrap();
    let socket = dir.path().join("s.sock");
    let core = start(socket.clone(), dir.path().join("state"), false, Backend::Fake, None).unwrap();
    let caps = json!({"terminal.spawn": {}, "terminal.output": {"raw": true}, "terminal.input": {}, "terminal.kill": {}});
    let mut config = Config::new("wire", "inst-1", caps.as_object().unwrap().clone());
    config.extension = Some("wire".into());
    let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let (p, mut requests) = Provider::connect(stream, config).await.unwrap();
    p.full_sync(vec![json!({"ref": "t1", "incarnation": "", "kind": "terminal", "state": "live"})]).await.unwrap();
    tokio::spawn(async move {
        let _keep = p;
        while let Some(r) = requests.recv().await {
            match r {
                Request::Open(o) => drop(o.respond(Err(ProviderError::new("not_found", "no terminal t1 here")))),
                Request::Effect(e) => {
                    let result = answer(&e.op, &e.args);
                    e.respond(result);
                }
            }
        }
    });
    let (c, _, _) = client::connect(&socket, "test").await.unwrap();
    let rid = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let list = c.call("resource.list", Value::Null).await.unwrap();
            if let Some(r) = list["resources"].as_array().unwrap().iter().find(|r| r["provider"] == "wire") {
                return r["id"].as_str().unwrap().to_string();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    (dir, core, c, rid)
}

fn by_name(_op: &str, args: &Value) -> Result<Outcome, ProviderError> {
    // The spawn's `name` picks the provider's answer.
    match args["name"].as_str().unwrap_or("") {
        "backend" => Err(ProviderError::new("backend", "outcome unknown")),
        kind => Err(ProviderError::new(kind, format!("refused: {kind}"))),
    }
}

#[tokio::test]
async fn a_provider_not_found_on_attach_is_no_effect_not_indeterminate() {
    let (_dir, _core, c, rid) = setup(by_name).await;
    let err = c.call("resource.attach", json!({"resource": rid, "cols": 80, "rows": 24})).await.unwrap_err();
    let data = err.data.clone().unwrap_or_default();
    assert_eq!((err.kind(), data["outcome"].as_str()), ("not_found", Some("rejected")), "{err:?}");
}

#[tokio::test]
async fn provider_protocol_errors_on_effects_are_rejections_and_backend_stays_indeterminate() {
    let (_dir, _core, c, _) = setup(by_name).await;
    for kind in ["conflict", "invalid", "unsupported", "not_found", "stale_generation", "overloaded"] {
        let err = c.call("resource.spawn", json!({"provider": "wire", "argv": ["sh"], "name": kind})).await.unwrap_err();
        let data = err.data.clone().unwrap_or_default();
        assert_eq!((err.kind(), data["outcome"].as_str()), (kind, Some("rejected")), "{kind}: {err:?}");
    }
    let err = c.call("resource.spawn", json!({"provider": "wire", "argv": ["sh"], "name": "backend"})).await.unwrap_err();
    let data = err.data.clone().unwrap_or_default();
    assert_eq!((err.kind(), data["outcome"].as_str()), ("backend", Some("indeterminate")), "{err:?}");
}
