//! Additional restart effect guards through the synthetic fixture.
use super::*;
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_background_refuses_by_default_explicit_loss_bypasses_only_it() {
    let (_root, shared, _guard, provider, mut requests, mut core, _log, helpers) =
        fixture("same", true).await;
    shared.state.lock().unwrap().background = None;
    let old = shared.process().unwrap();
    let mut request = args(&shared);
    let refused = core
        .invoke(
            &shared,
            &provider,
            &mut requests,
            "claude.restart",
            request.clone(),
        )
        .await;
    assert_eq!(refused["result"]["outcome"], "rejected");
    assert!(old.alive());
    assert!(
        lines(&helpers).is_empty(),
        "guard refuses before helper/preflight"
    );
    request["allow_background_loss"] = json!(true);
    let started = core
        .invoke(&shared, &provider, &mut requests, "claude.restart", request)
        .await;
    assert_eq!(started["result"]["data"]["started"], true, "{started}");
    assert!(!old.alive());
    stop(&shared).await;
    provider.close();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ended_bridge_lifetime_cannot_begin_replacement() {
    let (_root, shared, _guard, provider, mut requests, mut core, _log, helpers) =
        fixture("same", true).await;
    let (_sender, receiver) = tokio::sync::watch::channel(true);
    shared.set_lifetime(receiver);
    let old = shared.process().unwrap();
    let before = args(&shared);
    let reply = core
        .invoke(
            &shared,
            &provider,
            &mut requests,
            "claude.restart",
            before.clone(),
        )
        .await;
    assert_eq!(reply["result"]["outcome"], "rejected", "{reply}");
    assert!(old.alive());
    assert!(lines(&helpers).is_empty());
    assert_eq!(
        shared.state.lock().unwrap().epoch(),
        before["epoch"].as_str().unwrap()
    );
    stop(&shared).await;
    provider.close();
}
