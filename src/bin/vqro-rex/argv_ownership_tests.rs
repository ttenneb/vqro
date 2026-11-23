//! Creator and discovery lifetimes share an identity budget, not ownership of creation input.
use super::*;

#[tokio::test]
async fn unrelated_observation_cancel_cannot_take_the_active_creators_argv() {
    let (app, mode, _, release, published) = fixture_detailed().await;
    let created = app.write_facts(Change::Created { bid:"unobserved".into(), argv:vec!["original-command".into(), "argument".into()] }).await.unwrap().unwrap();
    let mut creator = facts::PendingGuard::creator(app.clone(), "unobserved".into(), created["creator_token"].as_u64());
    let discovery = app.write_facts(Change::Begin { bid:"unobserved".into() }).await.unwrap().unwrap();
    app.write_facts(Change::Cancel { bid:"unobserved".into(), token:discovery["token"].as_u64().unwrap() }).await.unwrap();
    mode.store(26, Ordering::Relaxed);
    release.notify_one();
    app.reconcile(false).await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    let facts = app.state.lock().unwrap().blocks["unobserved"].facts.clone();
    assert_eq!(facts["argv"], json!(["original-command", "argument"]));
    assert!(facts["data"].get("argv_known").is_none());
    let rows = published.lock().unwrap();
    let wire = &rows.iter().rev().find(|v| v["method"] == "resource.upsert" && v["params"]["facts"]["ref"] == "unobserved").unwrap()["params"]["facts"];
    assert_eq!(wire["argv"], facts["argv"]);
    assert!(wire["data"].get("argv_known").is_none());
    drop(rows);
    assert!(app.state.lock().unwrap().pending.contains_key("unobserved"), "observation commit must not finish the creator");
    creator.clear().await;
    assert!(!app.state.lock().unwrap().pending.contains_key("unobserved"));
}

#[tokio::test]
async fn lost_creation_input_publishes_unknown_argv() {
    let (app, mode, _, release, _) = fixture_detailed().await;
    let created = app.write_facts(Change::Created { bid:"unobserved".into(), argv:vec!["lost-input".into()] }).await.unwrap().unwrap();
    let mut creator = facts::PendingGuard::creator(app.clone(), "unobserved".into(), created["creator_token"].as_u64());
    creator.clear().await; // creator completed without an observation; later discovery cannot recover input
    mode.store(26, Ordering::Relaxed);
    release.notify_one();
    app.reconcile(false).await.unwrap();
    let facts = app.state.lock().unwrap().blocks["unobserved"].facts.clone();
    assert_eq!(facts["argv"], Value::Null);
    assert!(facts["data"].get("argv_known").is_none());
    // Omitting argv carries the same unknown meaning as explicit null.
    let mut absent = facts.clone();
    absent.as_object_mut().unwrap().remove("argv");
    let parsed = vqro::provider_wire::parse_facts(absent).unwrap();
    assert!(parsed.argv.is_none());
}
