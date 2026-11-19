//! In-process provider conformance: no hc, sessions, or real backend effects.
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vqro::adapter::{AdapterCmd, AdapterEvent, AdapterHandle, ResourceFacts, Stream, StreamInput};
use vqro::core::{add_adapter, Core, Shared};
use vqro::provider::{Identity, IdentityMap, ProviderInfo};
use serde_json::{json, Value};
use tokio::sync::mpsc;

fn info(instance: &str) -> ProviderInfo {
    ProviderInfo { connection: 1, instance: instance.into(), role: "backend".into(), software: json!({"name": "fixture", "version": "1"}) }
}
fn facts(reference: &str, incarnation: &str) -> ResourceFacts {
    serde_json::from_value(json!({"backend_ref": reference, "incarnation": incarnation, "kind": "terminal", "name": null,
        "title": null, "state": "live", "status": null, "exit": null, "argv": ["sh"], "cwd": null})).unwrap()
}
fn core(state: &Path) -> Shared {
    std::fs::create_dir_all(state).unwrap();
    Arc::new(Mutex::new(Core::new(vqro::store::Store::in_memory(), Some(state.into()), state.join("unused"), false).unwrap()))
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() { tokio::time::sleep(Duration::from_millis(5)).await; }
    }).await.unwrap();
}
async fn register(core: &Shared, name: &str, instance: &str, resources: Vec<ResourceFacts>) -> (mpsc::UnboundedSender<(String, AdapterEvent)>, mpsc::UnboundedReceiver<AdapterCmd>) {
    let (events, receiver) = mpsc::unbounded_channel();
    let (commands, inbox) = mpsc::unbounded_channel();
    add_adapter(core, AdapterHandle { name: name.into(), session: None, context: None, capabilities: json!({"terminal.spawn": {}, "terminal.kill": {}, "terminal.output": {"raw": true}}), cmds: commands }, receiver, false);
    events.send((name.into(), AdapterEvent::Registered(info(instance)))).unwrap();
    events.send((name.into(), AdapterEvent::Snapshot(resources))).unwrap();
    until(|| core.lock().unwrap().providers[name].synced).await;
    (events, inbox)
}
async fn native(commands: &mut mpsc::UnboundedReceiver<AdapterCmd>) -> Option<AdapterCmd> {
    vqro::adapter::command_for(commands.recv().await?, 1)
}
fn id(core: &Shared, provider: &str, reference: &str) -> String {
    core.lock().unwrap().resources.values().find(|r| r.adapter == provider && r.facts.backend_ref == reference).unwrap().id.clone()
}

#[tokio::test]
async fn same_instance_survives_core_restart_but_replacement_retires_every_old_id() {
    let dir = tempfile::tempdir().unwrap();
    let first = core(dir.path());
    let (_events, _cmds) = register(&first, "fixture", "A", vec![facts("t1", "birth1")]).await;
    let old = id(&first, "fixture", "t1");
    let restarted = core(dir.path());
    let (events, _cmds) = register(&restarted, "fixture", "A", vec![facts("t1", "birth1")]).await;
    assert_eq!(old, id(&restarted, "fixture", "t1"));
    events.send(("fixture".into(), AdapterEvent::Registered(info("B")))).unwrap();
    events.send(("fixture".into(), AdapterEvent::Snapshot(vec![facts("t2", "birth2")]))).unwrap();
    until(|| restarted.lock().unwrap().resources.values().any(|r| r.facts.backend_ref == "t2")).await;
    assert!(!restarted.lock().unwrap().resources.contains_key(&old));
    let replacement = id(&restarted, "fixture", "t2");
    assert_ne!(replacement, old);
    events.send(("fixture".into(), AdapterEvent::Registered(info("A")))).unwrap();
    events.send(("fixture".into(), AdapterEvent::Snapshot(vec![facts("t1", "birth1")]))).unwrap();
    until(|| restarted.lock().unwrap().resources.values().any(|r| r.facts.backend_ref == "t1")).await;
    assert_ne!(id(&restarted, "fixture", "t1"), old, "a retired id cannot resurrect even if the old instance returns");
    let saved: Value = serde_json::from_slice(&std::fs::read(dir.path().join("resources.json")).unwrap()).unwrap();
    assert!(saved["ids"].get(&old).is_none());
    assert!(saved["ids"].get(&replacement).is_none());
}

#[tokio::test]
async fn remote_ref_shape_migration_and_alias_reuse_tombstone_old_ids() {
    let dir = tempfile::tempdir().unwrap();
    let state = core(dir.path());
    let (events, _commands) = register(&state, "fixture", "relay-core", vec![facts("rohan/t7", "birth")]).await;
    let legacy = id(&state, "fixture", "rohan/t7");
    let stable_a = json!(["hc-machine-v1", "machine-a", null, "t7"]).to_string();
    let stable_b = json!(["hc-machine-v1", "machine-b", null, "t7"]).to_string();
    events.send(("fixture".into(), AdapterEvent::Snapshot(vec![facts(&stable_a, "birth")]))).unwrap();
    until(|| state.lock().unwrap().resources.values().any(|r| r.facts.backend_ref == stable_a)).await;
    let first = id(&state, "fixture", &stable_a);
    assert_ne!(first, legacy);
    events.send(("fixture".into(), AdapterEvent::Snapshot(vec![facts(&stable_b, "birth")]))).unwrap();
    until(|| state.lock().unwrap().resources.values().any(|r| r.facts.backend_ref == stable_b)).await;
    assert_ne!(id(&state, "fixture", &stable_b), first);
    let saved: Value = serde_json::from_slice(&std::fs::read(dir.path().join("resources.json")).unwrap()).unwrap();
    assert!(saved["ids"].get(&legacy).is_none());
    assert!(saved["ids"].get(&first).is_none());
}

#[tokio::test]
async fn stale_source_events_and_disconnect_cannot_disable_a_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let core = core(dir.path());
    let (old, _old_cmds) = register(&core, "fixture", "A", vec![facts("old", "")]).await;
    let old_generation = core.lock().unwrap().providers["fixture"].generation;
    let (_new, _new_cmds) = register(&core, "fixture", "B", vec![facts("new", "")]).await;
    old.send(("fixture".into(), AdapterEvent::Registered(info("A")))).unwrap();
    old.send(("fixture".into(), AdapterEvent::Upsert(facts("stale", "")))).unwrap();
    old.send(("fixture".into(), AdapterEvent::Connected(false, Some("old closed".into())))).unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let core = core.lock().unwrap();
    assert!(core.providers["fixture"].generation > old_generation);
    assert_eq!(core.providers["fixture"].info.as_ref().unwrap().instance, "B");
    assert!(core.providers["fixture"].health.borrow().connected);
    assert_eq!(core.resources.len(), 1);
    assert!(core.resources.values().all(|r| r.facts.backend_ref == "new" && r.available));
}

#[tokio::test]
async fn queued_effect_from_an_old_native_connection_is_rejected_before_execution() {
    let (commands, mut inbox) = mpsc::unbounded_channel();
    let handle = AdapterHandle { name: "fixture".into(), session: Some(11), context: None, capabilities: json!({"terminal.kill": {}}), cmds: commands };
    let effect = tokio::spawn(async move { handle.kill("t1".into()).await });
    let queued = inbox.recv().await.unwrap();
    assert!(vqro::adapter::command_for(queued, 12).is_none(), "an old queue entry must never reach the new native connection");
    let error = effect.await.unwrap().unwrap_err().rpc();
    assert_eq!(error.data.unwrap()["outcome"], "rejected");
    assert!(inbox.try_recv().is_err(), "there is no replay onto connection 12");
}

#[test]
fn identity_write_failure_does_not_consume_an_id_and_legacy_ids_are_tombstones() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("state");
    std::fs::create_dir(&parent).unwrap();
    let path = parent.join("resources.json");
    std::fs::write(&path, br#"{"next":1,"ids":{"r7":["fixture","old"]}}"#).unwrap();
    let mut map = IdentityMap::open(Some(path.clone())).unwrap();
    let identity = Identity { provider: "fixture".into(), instance: "A".into(), reference: "t1".into(), incarnation: "".into() };
    let saved = dir.path().join("saved");
    std::fs::rename(&parent, &saved).unwrap();
    std::fs::write(&parent, b"not a directory").unwrap();
    assert_eq!(map.identify(identity.clone()).err().unwrap().kind(), "io");
    std::fs::remove_file(&parent).unwrap();
    std::fs::rename(saved, &parent).unwrap();
    assert_eq!(map.identify(identity.clone()).unwrap().id, "r8");
    assert_eq!(IdentityMap::open(Some(path.clone())).unwrap().identify(identity).unwrap().id, "r8");
    let disk: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(disk["ids"].get("r7").is_none());
    assert_eq!(disk["next"], 9);
}

#[tokio::test]
async fn timeout_ignores_late_reply_without_resend_and_disconnect_ends_inflight_kill() {
    let dir = tempfile::Builder::new().prefix("pv").tempdir_in("/tmp").unwrap();
    let socket = dir.path().join("s");
    let core = vqro::server::start(socket.clone(), dir.path().join("state"), false, vqro::adapter::Backend::Fake, None).unwrap();
    let (events, mut commands) = register(&core, "fixture", "A", vec![facts("existing", "")]).await;
    let (peer, _incoming, _) = vqro::client::connect(&socket, "test").await.unwrap();
    let caller = peer.clone();
    let request = tokio::spawn(async move { caller.call("resource.spawn", json!({"provider": "fixture", "argv": ["sh"], "deadline_ms": 20})).await });
    let Some(AdapterCmd::Spawn(_, reply)) = native(&mut commands).await else { panic!("expected one native spawn") };
    let error = request.await.unwrap().unwrap_err();
    assert_eq!(error.kind(), "timeout");
    assert_eq!(error.data.as_ref().unwrap()["outcome"], "indeterminate");
    let effect = error.data.unwrap()["effect"].clone();
    assert!(reply.send(Ok(facts("late", ""))).is_err(), "late replies are ignored after timeout");
    assert!(!core.lock().unwrap().resources.values().any(|r| r.facts.backend_ref == "late"));
    // Independent native facts, not a late reply or replay, reconcile the observed resource.
    events.send(("fixture".into(), AdapterEvent::Upsert(facts("late", "")))).unwrap();
    until(|| core.lock().unwrap().resources.values().any(|r| r.facts.backend_ref == "late")).await;
    assert!(commands.try_recv().is_err(), "deadline must not resend spawn");
    let audit = core.lock().unwrap().events_since(0).unwrap();
    assert!(audit.iter().any(|e| e["type"] == "effect" && e["data"]["effect"] == effect && e["data"]["outcome"] == "indeterminate" && e["data"]["author"].is_string()));
    assert!(!audit.iter().any(|e| e["type"] == "effect.outcome"), "late outcome publication remains OPEN in provider v0");
    let resource = id(&core, "fixture", "existing");
    let caller = peer.clone();
    let target = resource.clone();
    let kill = tokio::spawn(async move { caller.call("resource.kill", json!({"resource": target, "deadline_ms": 2000})).await });
    let Some(AdapterCmd::Kill(_, reply)) = native(&mut commands).await else { panic!("expected native kill") };
    events.send(("fixture".into(), AdapterEvent::Connected(false, Some("transport lost".into())))).unwrap();
    let error = tokio::time::timeout(Duration::from_millis(500), kill).await.unwrap().unwrap().unwrap_err();
    assert_eq!(error.kind(), "unavailable");
    assert_eq!(error.data.as_ref().unwrap()["outcome"], "indeterminate");
    assert!(!core.lock().unwrap().resources[&resource].available);
    assert!(reply.send(Ok(vqro::adapter::EffectResult::accepted(json!({"late": true})))).is_err());
    events.send(("fixture".into(), AdapterEvent::Registered(info("A")))).unwrap();
    events.send(("fixture".into(), AdapterEvent::Snapshot(vec![facts("existing", ""), facts("late", "")]))).unwrap();
    until(|| core.lock().unwrap().resources[&resource].available).await;
    assert_eq!(id(&core, "fixture", "existing"), resource);
}

#[tokio::test]
async fn provider_loss_closes_an_idle_stream_and_keeps_its_facts() {
    let dir = tempfile::Builder::new().prefix("pv").tempdir_in("/tmp").unwrap();
    let socket = dir.path().join("s");
    let core = vqro::server::start(socket.clone(), dir.path().join("state"), false, vqro::adapter::Backend::Fake, None).unwrap();
    let (events, mut commands) = register(&core, "fixture", "A", vec![facts("t1", "")]).await;
    let resource = id(&core, "fixture", "t1");
    let (peer, mut incoming, _) = vqro::client::connect(&socket, "test").await.unwrap();
    let caller = peer.clone();
    let target = resource.clone();
    let attach = tokio::spawn(async move { caller.call("resource.attach", json!({"resource": target})).await });
    let Some(AdapterCmd::Attach { reply, .. }) = native(&mut commands).await else { panic!("expected attach") };
    let (input, mut native_input) = vqro::queue::channel(256 * 1024);
    let (_output, output) = vqro::queue::channel(256 * 1024);
    assert!(reply.send(Ok(Stream { initial: "none", epoch: None, input, output, terminal: None })).is_ok());
    assert_eq!(attach.await.unwrap().unwrap()["outcome"], "accepted");
    events.send(("fixture".into(), AdapterEvent::Connected(false, None))).unwrap();
    let reason = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Some(vqro::rpc::Incoming::Notification { method, params }) = incoming.recv().await {
                if method == "stream.closed" { break params["reason"]["reason"].clone(); }
            }
        }
    }).await.unwrap();
    assert_eq!(reason, "provider_disconnected");
    assert!(!core.lock().unwrap().resources[&resource].available);
    assert!(matches!(native_input.recv().await.unwrap().value, StreamInput::Close));
}
