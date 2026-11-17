//! Wire provider protocol errors reach callers as "no effect" (provider-v0 9.4), not "indeterminate".
//! A real in-process core, with prim::provider_client as an out-of-process-style provider.

use std::time::Duration;

use prim::client;
use prim::provider_client::{Config, Outcome, Provider, ProviderError, Request};
use prim::server::{start, Backend};
use serde_json::{json, Value};

async fn provider(socket: &std::path::Path) -> (Provider, tokio::sync::mpsc::UnboundedReceiver<Request>) {
    let caps = json!({"terminal.spawn": {}, "terminal.output": {"raw": true}, "terminal.input": {}, "terminal.kill": {}});
    let mut config = Config::new("wire", "inst-1", caps.as_object().unwrap().clone());
    config.extension = Some("wire".into());
    let stream = tokio::net::UnixStream::connect(socket).await.unwrap();
    let (p, rx) = Provider::connect(stream, config).await.unwrap();
    p.full_sync(vec![json!({"ref": "t1", "incarnation": "", "kind": "terminal", "state": "live"})]).await.unwrap();
    (p, rx)
}

#[tokio::test]
#[ignore = "core issue: src/provider_wire.rs provider_error() maps every provider protocol error to backend/indeterminate"]
async fn a_provider_not_found_on_attach_is_no_effect_not_indeterminate() {
    let dir = tempfile::Builder::new().prefix("ppe").tempdir_in("/tmp").unwrap();
    let socket = dir.path().join("s.sock");
    let _core = start(socket.clone(), dir.path().join("state"), false, Backend::Fake, None).unwrap();
    let (_p, mut requests) = provider(&socket).await;
    tokio::spawn(async move {
        while let Some(r) = requests.recv().await {
            match r {
                Request::Open(o) => drop(o.respond(Err(ProviderError::new("not_found", "no terminal t1 here")))),
                Request::Effect(e) => e.respond(Ok(Outcome::Rejected { kind: "backend".into(), message: "unused".into() })),
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
    let err = c.call("resource.attach", json!({"resource": rid, "cols": 80, "rows": 24})).await.unwrap_err();
    let data = err.data.clone().unwrap_or_default();
    assert_ne!(data["outcome"], "indeterminate", "a provider protocol error means no effect (9.4): {err:?}");
    assert_eq!(err.kind(), "not_found", "{err:?}");
}
