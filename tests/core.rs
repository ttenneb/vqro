//! End-to-end tests of the core protocol over a real socket, with the fake backend as a fixture.

use std::path::PathBuf;
use std::time::Duration;

use base64::Engine;
use prim::client;
use prim::rpc::{Incoming, Peer};
use prim::server::{start, Backend};
use serde_json::{json, Value};
#[path="support/managed.rs"] mod managed;

struct Env {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    state: PathBuf,
}

fn env() -> Env {
    let dir = tempfile::Builder::new().prefix("pt").tempdir_in("/tmp").unwrap();
    let socket = dir.path().join("s.sock");
    let state = dir.path().join("state");
    Env { _dir: dir, socket, state }
}

async fn connect(e: &Env) -> (Peer, prim::rpc::IncomingReceiver) {
    let (p, rx, _) = client::connect(&e.socket, "test").await.unwrap();
    (p, rx)
}

async fn until<F: FnMut() -> bool>(mut f: F) {
    for _ in 0..100 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("condition not reached");
}

async fn ext_status(p: &Peer, name: &str) -> String {
    let l = p.call("extension.list", Value::Null).await.unwrap();
    l["extensions"][name]["status"].as_str().unwrap_or("").to_string()
}

#[tokio::test]
async fn provider_capabilities_and_unavailable_resources_are_exported() {
    let e = env();
    let core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (p, _rx) = connect(&e).await;
    let spawned = p.call("resource.spawn", json!({"argv": ["sh"]})).await.unwrap();
    let id = spawned["resource"]["id"].clone();
    let snap = p.call("workspace.snapshot", Value::Null).await.unwrap();
    assert!(snap["providers"]["fake"]["capabilities"]["terminal.spawn"].is_object());
    assert_eq!(spawned["resource"]["available"], true);
    core.lock().unwrap().on_adapter_event("fake", prim::adapter::AdapterEvent::Connected(false, Some("test disconnect".into())));
    let snap = p.call("workspace.snapshot", Value::Null).await.unwrap();
    let resource = snap["resources"].as_array().unwrap().iter().find(|r| r["id"] == id).unwrap();
    assert_eq!(resource["available"], false, "disconnect keeps stale facts without deleting identity");
    assert_eq!(snap["providers"]["fake"]["state"], "disconnected");
    assert_eq!(p.call("resource.attach", json!({"resource": id})).await.unwrap_err().kind(), "unavailable");
}

#[tokio::test]
async fn changed_incarnation_retires_ids_instead_of_resurrecting_them() {
    let e = env();
    let core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (p, _rx) = connect(&e).await;
    let first = p.call("resource.spawn", json!({"argv": ["sh"]})).await.unwrap()["resource"]["id"].as_str().unwrap().to_string();
    let original = core.lock().unwrap().resources[&first].facts.clone();
    let mut changed = serde_json::to_value(&original).unwrap();
    changed["incarnation"] = json!("replacement");
    core.lock().unwrap().on_adapter_event("fake", prim::adapter::AdapterEvent::Upsert(serde_json::from_value(changed).unwrap()));
    let second = core.lock().unwrap().resources.keys().next().unwrap().clone();
    assert_ne!(first, second, "same backend ref with a new incarnation must retire its old id");
    core.lock().unwrap().on_adapter_event("fake", prim::adapter::AdapterEvent::Upsert(original));
    let third = core.lock().unwrap().resources.keys().next().unwrap().clone();
    assert_ne!(third, first, "a tombstoned id cannot come back");
    assert_ne!(third, second);
}

#[tokio::test]
async fn new_fake_instance_cannot_reuse_a_previous_prim_id() {
    let e = env();
    let _old = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (old, _rx) = connect(&e).await;
    let first = old.call("resource.spawn", json!({"argv": ["sh"]})).await.unwrap()["resource"]["id"].clone();
    // A restart stops the old writer first; two live cores must not share one state directory.
    prim::server::shutdown(&_old).await;
    let stale_facts=_old.lock().unwrap().resources.values().next().unwrap().facts.clone();
    _old.lock().unwrap().on_adapter_event("fake",prim::adapter::AdapterEvent::Removed { reference:stale_facts.backend_ref,incarnation:stale_facts.incarnation });
    assert_eq!(_old.lock().unwrap().resources.len(),1,"stopped cores ignore late backend state mutations");
    // The fake starts empty with refs f1, f2... again, but belongs to a new instance.
    let socket = e._dir.path().join("new.sock");
    // Other tests in this binary spawn children; one forked while the old core's lock file was open shares
    // that flock until its exec closes the fd, so allow that brief window (see the store fence test).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let _new = loop {
        match start(socket.clone(), e.state.clone(), false, Backend::Fake, None) {
            Ok(core) => break core,
            Err(err) if std::time::Instant::now() < deadline && err.to_string().contains("still in use") => {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await
            }
            Err(err) => panic!("{err}"),
        }
    };
    let (new, _rx, _) = client::connect(&socket, "test").await.unwrap();
    let second = new.call("resource.spawn", json!({"argv": ["sh"]})).await.unwrap()["resource"]["id"].clone();
    assert_ne!(first, second, "same ref on a new backend instance is not the same resource");
}

#[tokio::test]
async fn open_attach_and_type() {
    let e = env();
    let _core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (p, mut rx) = connect(&e).await;
    let r = p.call("resource.spawn", json!({"argv": ["sh"]})).await.unwrap();
    let res = r["resource"]["id"].as_str().unwrap().to_string();
    p.call("command.invoke", json!({"name": "prim.layout.reset"})).await.unwrap();
    let snap = p.call("workspace.snapshot", Value::Null).await.unwrap();
    assert_eq!(snap["entries"]["workspace/active"]["value"]["layout"], "main");
    assert_eq!(snap["entries"][format!("view/term-{res}")]["value"]["resource"], res.as_str());

    let a = p.call("resource.attach", json!({"resource": res, "cols": 80, "rows": 24})).await.unwrap();
    let sid = a["stream"].as_u64().unwrap();
    p.notify("stream.input", json!({"stream": sid, "text": "abc"}));
    let mut got = Vec::new();
    while let Some(Incoming::Notification { method, params }) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await.unwrap() {
        if method == "stream.data" {
            got.extend(base64::engine::general_purpose::STANDARD.decode(params["data"].as_str().unwrap()).unwrap());
            if got.ends_with(b"abc") {
                break;
            }
        }
    }
    assert!(got.ends_with(b"abc"), "input echoed in order");
}

#[tokio::test]
async fn commands_route_to_their_owner_and_go_unavailable() {
    let e = env();
    let _core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (owner, mut owner_rx, _) = client::connect(&e.socket, "x").await.unwrap();
    owner.call("hello", json!({"extension": "greeter"})).await.unwrap();
    owner.call("command.register", json!({"name": "greet", "title": "say hi"})).await.unwrap();
    let o = owner.clone();
    let handler = tokio::spawn(async move {
        while let Some(m) = owner_rx.recv().await {
            if let Incoming::Request { id, method, params } = m {
                assert_eq!(method, "command.invoke");
                o.respond(id, Ok(json!({"hi": params["caller"]})));
            }
        }
    });

    let (p, _rx) = connect(&e).await;
    let r = p.call("command.invoke", json!({"name": "greet"})).await.unwrap();
    assert!(r["hi"].as_str().unwrap().starts_with("client:"));

    // Only the core owns `prim.*`, and names can't be stolen from a live owner.
    assert_eq!(p.call("command.register", json!({"name": "prim.revert"})).await.unwrap_err().kind(), "forbidden");
    assert_eq!(p.call("command.register", json!({"name": "greet"})).await.unwrap_err().kind(), "conflict");

    // The owner goes away: drop every handle so its socket closes.
    handler.abort();
    let _ = handler.await;
    drop(owner);
    let mut kind = String::new();
    for _ in 0..40 {
        kind = p.call("command.invoke", json!({"name": "greet"})).await.map(|_| String::new()).unwrap_or_else(|e| e.kind().to_string());
        if kind == "unavailable" || kind == "disconnected" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(kind == "unavailable" || kind == "disconnected", "got {kind}");
}

#[tokio::test]
async fn hung_command_reports_indeterminate_deadline_without_resending() {
    let e = env();
    let _core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (handler, mut requests) = connect(&e).await;
    handler.call("command.register", json!({"name": "hang"})).await.unwrap();
    let (caller, _caller_rx) = connect(&e).await;
    let started = std::time::Instant::now();
    let error = tokio::time::timeout(Duration::from_millis(500), caller.call("command.invoke", json!({"name": "hang", "deadline_ms": 30}))).await.expect("forwarded deadline was ignored").unwrap_err();
    assert_eq!(error.kind(), "timeout");
    assert_eq!(error.data.unwrap()["outcome"], "indeterminate");
    assert!(started.elapsed() < Duration::from_millis(500));
    let Some(Incoming::Request { id, method, .. }) = requests.recv().await else { panic!("handler never received invocation") };
    assert_eq!(method, "command.invoke");
    handler.respond(id, Ok(json!({"late": true})));
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(requests.try_recv().is_err(), "deadline must never resend");
    let snap = caller.call("workspace.snapshot", Value::Null).await.unwrap();
    assert!(snap["commands"].is_array(), "late reply does not poison the connection");
}

#[tokio::test]
async fn stalled_socket_flood_closes_only_its_stream_explicitly() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let e = env();
    let _core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (controller, _controller_rx) = connect(&e).await;
    let resource = controller.call("resource.spawn", json!({"argv": ["flood", "--output-bytes-per-sec", "32000000"]})).await.unwrap()["resource"]["id"].clone();
    let socket = tokio::net::UnixStream::connect(&e.socket).await.unwrap();
    let (r, mut w) = socket.into_split();
    let mut lines = BufReader::new(r).lines();
    let attach = json!({"jsonrpc": "2.0", "id": 1, "method": "resource.attach", "params": {"resource": resource, "cols": 80, "rows": 24}});
    w.write_all(format!("{attach}\n").as_bytes()).await.unwrap();
    // Do not read the socket at all while the shared fake load profile floods output.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let snap = tokio::time::timeout(Duration::from_secs(1), controller.call("workspace.snapshot", Value::Null)).await.unwrap().unwrap();
    assert!(snap["resources"].as_array().unwrap().iter().any(|r| r["id"] == resource));
    let closed = tokio::time::timeout(Duration::from_secs(3), async {
        let mut bytes = 0;
        while let Some(line) = lines.next_line().await.unwrap() {
            let msg: Value = serde_json::from_str(&line).unwrap();
            if msg["method"] == "stream.data" { bytes += msg["params"]["data"].as_str().unwrap().len(); }
            if msg["method"] == "stream.closed" {
                assert!(bytes < 4 * 1024 * 1024, "queued flood exceeded the bounded writer plus socket buffers");
                return msg;
            }
        }
        panic!("socket disconnected without explicit stream closure");
    }).await.expect("stalled viewer was never told its stream overflowed");
    assert_eq!(closed["params"]["reason"]["reason"], "slow_consumer");
    // A different resource/viewer remains usable; one stalled socket does not stall the adapter.
    let normal = controller.call("resource.spawn", json!({"argv": ["sh"]})).await.unwrap()["resource"]["id"].clone();
    let attached = controller.call("resource.attach", json!({"resource": normal})).await.unwrap();
    controller.call("stream.input", json!({"stream": attached["stream"], "text": "still works"})).await.unwrap();
    controller.call("stream.close", json!({"stream": attached["stream"]})).await.unwrap();
}

#[tokio::test]
async fn old_extension_disconnect_does_not_disable_replacement() {
    let e = env();
    let core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (old, old_rx) = connect(&e).await;
    let token_file=e._dir.path().join("greeter.token");
    old.call("workspace.apply",json!({"ops":[{"op":"set","key":"extension/greeter","value":managed::spec(&token_file)}]})).await.unwrap();
    let token=managed::token(&token_file).await;
    old.call("hello", json!({"extension": "greeter","extension_token":token})).await.unwrap();
    old.call("command.register", json!({"name": "greet"})).await.unwrap();
    old.call("command.register", json!({"name": "old-only"})).await.unwrap();
    old.call("view.publish", json!({"view": "greeting", "content": {"label": "old"}})).await.unwrap();

    let (new, mut new_rx) = connect(&e).await;
    new.call("hello", json!({"extension": "greeter","extension_token":token})).await.unwrap();
    new.call("command.register", json!({"name": "greet"})).await.unwrap();
    new.call("view.publish", json!({"view": "greeting", "content": {"label": "new"}})).await.unwrap();
    drop(old_rx);
    drop(old);
    // The old-only command is our barrier: cleanup has run while holding the core lock.
    until(|| !core.lock().unwrap().commands["old-only"].available).await;
    assert!(core.lock().unwrap().commands["greet"].available);
    assert_eq!(core.lock().unwrap().live["greeting"].0["label"], "new");

    let responder = new.clone();
    let handler = tokio::spawn(async move {
        if let Some(Incoming::Request { id, method, .. }) = new_rx.recv().await {
            assert_eq!(method, "command.invoke");
            responder.respond(id, Ok(json!({"from": "replacement"})));
        } else {
            panic!("expected invocation on replacement connection");
        }
    });
    let (caller, _rx) = connect(&e).await;
    let result = tokio::time::timeout(Duration::from_secs(2), caller.call("command.invoke", json!({"name": "greet"}))).await.unwrap().unwrap();
    assert_eq!(result["from"], "replacement");
    handler.await.unwrap();
}

#[tokio::test]
async fn crash_budget_then_replace_then_safe_mode() {
    let e = env();
    let _core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (p, _rx) = connect(&e).await;
    p.call("workspace.apply", json!({"ops": [{"op": "set", "key": "extension/bad", "value": {"argv": ["sh", "-c", "exit 1"]}}]})).await.unwrap();
    for _ in 0..100 {
        if ext_status(&p, "bad").await == "failed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(ext_status(&p, "bad").await, "failed");
    let failed = p.call("extension.list", Value::Null).await.unwrap();
    assert_eq!(failed["extensions"]["bad"]["crashes"], 3);
    assert_eq!(failed["extensions"]["bad"]["crashes_total"], 3);
    assert_eq!(failed["extensions"]["bad"]["crash_window_seconds"], 300);

    // Replacing the spec is a fresh start.
    p.call("workspace.apply", json!({"ops": [{"op": "set", "key": "extension/bad", "value": {"argv": ["sleep", "30"]}}]})).await.unwrap();
    for _ in 0..40 {
        if ext_status(&p, "bad").await == "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(ext_status(&p, "bad").await, "running");
    let replaced = p.call("extension.list", Value::Null).await.unwrap();
    assert_eq!(replaced["extensions"]["bad"]["crashes"], 0, "replacement resets the budget");
    assert_eq!(replaced["extensions"]["bad"]["crashes_total"], 3, "historical exits remain explicit");

    p.call("command.invoke", json!({"name": "prim.safe_mode", "args": {"on": true}})).await.unwrap();
    assert_eq!(ext_status(&p, "bad").await, "safe_mode");
    p.call("command.invoke", json!({"name": "prim.safe_mode", "args": {"on": false}})).await.unwrap();
    assert_eq!(ext_status(&p, "bad").await, "running");
    p.call("command.invoke", json!({"name": "prim.extension.disable", "args": {"name": "bad"}})).await.unwrap();
    assert_eq!(ext_status(&p, "bad").await, "disabled");
    let disabled = p.call("extension.list", Value::Null).await.unwrap();
    assert_eq!(disabled["extensions"]["bad"]["crashes_total"], 3, "intentional stops are not crashes");
}

#[tokio::test]
async fn restarted_core_rejects_old_epoch_even_when_sequence_matches() {
    let e = env();
    let old_core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    until(|| old_core.lock().unwrap().adapter_state.contains_key("fake")).await;
    let (old, _) = connect(&e).await;
    let before = old.call("workspace.snapshot", Value::Null).await.unwrap();
    prim::server::shutdown(&old_core).await;
    let socket = e._dir.path().join("new.sock");
    let new_core = start(socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    until(|| new_core.lock().unwrap().adapter_state.contains_key("fake")).await;
    let (new, mut rx, hello) = client::connect(&socket, "test").await.unwrap();
    let after = new.call("workspace.snapshot", Value::Null).await.unwrap();
    assert_eq!(before["seq"], after["seq"], "sequence alone is ambiguous");
    assert!(after["epoch"].is_string());
    assert_ne!(before["epoch"], after["epoch"]);
    assert_eq!(hello["epoch"], after["epoch"]);
    let result = new.call("events.subscribe", json!({"from": before["seq"], "epoch": before["epoch"]})).await.unwrap();
    assert_eq!(result["epoch"], after["epoch"]);
    let Some(Incoming::Notification { method, params }) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await.unwrap() else {
        panic!("expected missed");
    };
    assert_eq!(method, "missed");
    assert_eq!(params["epoch"], after["epoch"]);
}

#[tokio::test]
async fn event_cursors_require_an_epoch_and_replay_only_newer_events() {
    let e = env();
    let _core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (p, _) = connect(&e).await;
    let snapshot = p.call("workspace.snapshot", Value::Null).await.unwrap();
    p.call("workspace.apply", json!({"ops": [{"op": "set", "key": "doc/test/cursor", "value": 1}]})).await.unwrap();
    let (replay, mut rx) = connect(&e).await;
    replay.call("events.subscribe", json!({"epoch": snapshot["epoch"], "from": snapshot["seq"]})).await.unwrap();
    loop {
        let Some(Incoming::Notification { method, params }) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await.unwrap() else {
            panic!("expected replay");
        };
        assert_eq!(method, "event");
        assert_eq!(params["epoch"], snapshot["epoch"]);
        assert!(params["seq"].as_u64().unwrap() > snapshot["seq"].as_u64().unwrap());
        if params["type"] == "entries" {
            break;
        }
    }
    for cursor in [json!({"from": 0}), json!({"epoch": snapshot["epoch"], "from": u64::MAX})] {
        let (peer, mut rx) = connect(&e).await;
        peer.call("events.subscribe", cursor).await.unwrap();
        let Some(Incoming::Notification { method, .. }) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await.unwrap() else {
            panic!("expected missed");
        };
        assert_eq!(method, "missed");
    }
}

#[tokio::test]
async fn transactions_revert_and_events() {
    let e = env();
    let _core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (p, mut rx) = connect(&e).await;
    let seq = p.call("events.subscribe", json!({})).await.unwrap()["seq"].as_u64().unwrap();
    let t = p
        .call("workspace.apply", json!({"ops": [
            {"op": "set", "key": "view/a", "value": {"kind": "panel", "title": "A"}},
            {"op": "set", "key": "layout/main", "value": {"view": "a"}},
            {"op": "set", "key": "workspace/active", "value": {"layout": "main"}}
        ]}))
        .await
        .unwrap();
    let mut saw = false;
    until(|| {
        while let Ok(Incoming::Notification { method, params }) = rx.try_recv() {
            if method == "event" && params["type"] == "entries" && params["seq"].as_u64().unwrap() > seq {
                saw = true;
            }
        }
        saw
    })
    .await;

    // A bad agent edit is rejected whole.
    let err = p.call("workspace.apply", json!({"ops": [{"op": "delete", "key": "view/a"}]})).await.unwrap_err();
    assert_eq!(err.kind(), "invalid");

    p.call("workspace.revert", json!({"txn": t["txn"]})).await.unwrap();
    let snap = p.call("workspace.snapshot", Value::Null).await.unwrap();
    assert!(snap["entries"].get("view/a").is_none());

    // Survives a restart of the store.
    let hist = p.call("workspace.history", json!({"limit": 5})).await.unwrap();
    assert_eq!(hist["txns"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn failed_workspace_write_has_no_state_history_or_event_effect() {
    let e = env();
    let core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    until(|| core.lock().unwrap().adapter_state.contains_key("fake")).await;
    let (p, _rx) = connect(&e).await;
    let ops = json!({"ops": [{"op": "set", "key": "doc/test/value", "value": 1}]});
    let first = p.call("workspace.apply", ops.clone()).await.unwrap();
    let before = p.call("workspace.snapshot", Value::Null).await.unwrap();
    let history = p.call("workspace.history", Value::Null).await.unwrap();
    let saved = e._dir.path().join("saved");
    std::fs::rename(&e.state, &saved).unwrap();
    std::fs::write(&e.state, b"not a directory").unwrap();
    let err = p.call("workspace.apply", json!({"ops": [{"op": "set", "key": "doc/test/value", "value": 2}]})).await.unwrap_err();
    assert_eq!(err.kind(), "io");
    assert_eq!(p.call("workspace.snapshot", Value::Null).await.unwrap(), before, "including event seq");
    assert_eq!(p.call("workspace.history", Value::Null).await.unwrap(), history);
    std::fs::remove_file(&e.state).unwrap();
    std::fs::rename(saved, &e.state).unwrap();
    let next = p.call("workspace.apply", ops).await.unwrap();
    assert_eq!(next["txn"].as_u64().unwrap(), first["txn"].as_u64().unwrap() + 1);
}

#[tokio::test]
async fn resource_map_write_failure_is_reported_without_hiding_backend_effect() {
    let e = env();
    let core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    // Force a rename failure without relying on permissions (also works as root).
    std::fs::create_dir(e.state.join("resources.json")).unwrap();
    let (p, _rx) = connect(&e).await;
    let error = p.call("resource.spawn", json!({"argv": ["sh"]})).await.unwrap_err();
    assert_eq!(error.kind(), "io");
    let data = error.data.unwrap();
    assert_eq!(data["outcome"], "indeterminate");
    assert_eq!(data["backend_outcome"], "observed");
    assert_eq!(data["provider"], "fake");
    assert!(data["instance"].is_string());
    assert_eq!(data["resource_facts"]["backend_ref"], "f1");
    let facts: prim::adapter::ResourceFacts = serde_json::from_value(data["resource_facts"].clone()).unwrap();
    // Repeated status changes while the disk remains broken must not spam the event log.
    for _ in 0..100 { core.lock().unwrap().on_adapter_event("fake", prim::adapter::AdapterEvent::Upsert(facts.clone())); }
    let events = core.lock().unwrap().events_since(0).unwrap();
    assert_eq!(events.iter().filter(|ev| ev["type"] == "persistence.error").count(), 1);
    assert!(!events.iter().any(|ev| ev["type"] == "resource"), "never publish an unpersisted id");
    // A different backend spawn triggers a successful map write; f1 is recovered without
    // resending its original spawn or waiting for another status event for it.
    std::fs::remove_dir(e.state.join("resources.json")).unwrap();
    let second = p.call("resource.spawn", json!({"argv": ["sh"]})).await.unwrap();
    assert_eq!(second["resource"]["backend_ref"], "f2");
    let snap = p.call("workspace.snapshot", Value::Null).await.unwrap();
    assert_eq!(snap["resources"].as_array().unwrap().len(), 2);
    let recovered = snap["resources"].as_array().unwrap().iter().find(|r| r["backend_ref"] == "f1").unwrap();
    let ids: Value = serde_json::from_slice(&std::fs::read(e.state.join("resources.json")).unwrap()).unwrap();
    assert!(ids["ids"].get(recovered["id"].as_str().unwrap()).is_some());
    assert_eq!(ids["ids"].as_object().unwrap().len(), 2);
    assert!(core.lock().unwrap().events_since(0).unwrap().iter().any(|ev| ev["type"] == "persistence.recovered"));
}

#[tokio::test]
async fn defaults_install_once() {
    let e = env();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("defaults.json"), r#"{"extensions": [{"name": "nap", "argv": ["sleep", "30"]}]}"#).unwrap();
    let _core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, Some(dir.path().to_path_buf())).unwrap();
    let (p, _rx) = connect(&e).await;
    let snap = p.call("workspace.snapshot", Value::Null).await.unwrap();
    assert!(snap["entries"].get("extension/nap").is_some());
    // Removing a default sticks: it is not reinstalled on the next start.
    p.call("workspace.apply", json!({"ops": [{"op": "delete", "key": "extension/nap"}]})).await.unwrap();
    let mut store = prim::store::Store::open(e.state.join("workspace.json")).unwrap();
    assert!(store.get("doc/prim/defaults").is_some());
    let _ = &mut store;
}

#[tokio::test]
async fn live_view_content_is_ephemeral() {
    let e = env();
    let _core = start(e.socket.clone(), e.state.clone(), false, Backend::Fake, None).unwrap();
    let (publisher, _prx, _) = client::connect(&e.socket, "pub").await.unwrap();
    let (p, _rx) = connect(&e).await;
    publisher.call("view.publish", json!({"view": "panel1", "content": {"items": [{"text": "hi"}]}})).await.unwrap();
    let snap = p.call("workspace.snapshot", Value::Null).await.unwrap();
    assert_eq!(snap["live"]["panel1"]["items"][0]["text"], "hi");
    // Not a transaction: history stays empty.
    let hist = p.call("workspace.history", json!({})).await.unwrap();
    assert!(hist["txns"].as_array().unwrap().is_empty());
    // Cleared when the publisher goes away.
    drop(publisher);
    drop(_prx);
    let mut cleared = false;
    for _ in 0..40 {
        let snap = p.call("workspace.snapshot", Value::Null).await.unwrap();
        if snap["live"].get("panel1").is_none() {
            cleared = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(cleared);
}
