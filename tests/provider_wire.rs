//! Socket conformance against pinned provider-v0, using private state and no hc.
use std::path::PathBuf;
use std::time::Duration;
use base64::Engine;
use prim::{client, rpc::{Incoming, IncomingReceiver, Peer}, server::{self, Backend}};
use serde_json::{json, Value};

struct Env { _dir: tempfile::TempDir, socket: PathBuf, core: prim::core::Shared }
impl Drop for Env { fn drop(&mut self) { prim::ext::stop_all(&self.core); } }
async fn env() -> Env {
    let dir = tempfile::Builder::new().prefix("pw").tempdir_in("/tmp").unwrap();
    let socket = dir.path().join("s");
    let core = server::start(socket.clone(), dir.path().join("st"), false, Backend::Fake, None).unwrap();
    Env { _dir: dir, socket, core }
}
async fn peer(e: &Env) -> (Peer, IncomingReceiver) { let (p, rx, _) = client::connect(&e.socket, "wire-test").await.unwrap(); (p, rx) }
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async { while !condition() { tokio::time::sleep(Duration::from_millis(5)).await; } }).await.unwrap();
}
fn registration(name: &str) -> Value {
    json!({"name":name,"instance":"fixture-instance","role":"backend","software":{"name":"fixture","version":"1"},"provider_version":"1",
        "capabilities":{"terminal.spawn":{},"terminal.kill":{},"terminal.input":{},"terminal.resize":{},"terminal.output":{"raw":true},"fixture.dispatch":{},"fixture.reject":{},"fixture.error":{},"fixture.hang":{}},
        "limits":{"max_streams":1,"stream_window_bytes":16}})
}
fn facts(reference: &str) -> Value { json!({"ref":reference,"kind":"terminal","state":"live","argv":[],"data":{"opaque":[1,2]}}) }

#[tokio::test]
async fn spawn_completion_does_not_roll_back_newer_published_facts() {
    let e=env().await;let(provider,mut incoming)=peer(&e).await;let(viewer,_)=peer(&e).await;
    let generation=provider.call("provider.register",registration("wire")).await.unwrap()["generation"].clone();
    provider.call("provider.sync",json!({"generation":generation,"resources":[],"final":true})).await.unwrap();
    for (i,state) in ["finished","live"].into_iter().enumerate() {
        let caller=viewer.clone();let spawn=tokio::spawn(async move{caller.call("resource.spawn",json!({"provider":"wire"})).await});
        let request=loop{if let Some(Incoming::Request{id,method,..})=incoming.recv().await{assert_eq!(method,"effect.invoke");break id;}};
        let mut initial=facts(&format!("t{i}"));initial["name"]=json!("creation snapshot");
        let mut latest=initial.clone();latest["state"]=json!(state);latest["name"]=json!("newer observation");
        if i==0 {provider.notify("resource.upsert",json!({"generation":generation,"facts":latest}));}
        else {provider.call("provider.sync",json!({"generation":generation,"resources":[latest],"final":false})).await.unwrap();}
        until(||e.core.lock().unwrap().resources.values().any(|r|r.adapter=="wire"&&r.facts.backend_ref==format!("t{i}")&&r.facts.name.as_deref()==Some("newer observation"))).await;
        provider.respond(request,Ok(json!({"outcome":"observed","data":{"facts":initial}})));
        let result=spawn.await.unwrap().unwrap();assert_eq!(result["resource"]["state"],state);assert_eq!(result["resource"]["name"],"newer observation");
        if i==1 {
            provider.call("provider.sync",json!({"generation":generation,"resources":[],"final":true})).await.unwrap();
            let current=e.core.lock().unwrap().resource(result["resource"]["id"].as_str().unwrap()).unwrap().to_json();
            assert_eq!(current["name"],"newer observation","creation replies must not roll back the in-progress sync cache either");
        }
    }
}

#[tokio::test]
async fn wire_registration_chunks_and_generation_fences() {
    let e = env().await;
    let (p, _rx) = peer(&e).await;
    let r = p.call("provider.register", registration("wire")).await.unwrap();
    let g = r["generation"].as_u64().unwrap();
    assert_eq!(r["limits"]["stream_window_bytes"], 16);
    assert_eq!(p.call("provider.register", registration("other")).await.unwrap_err().kind(), "conflict");
    assert_eq!(p.call("provider.sync", json!({"generation":g+1,"resources":[],"final":true})).await.unwrap_err().kind(), "stale_generation");
    p.call("provider.sync", json!({"generation":g,"resources":[facts("a")],"final":false})).await.unwrap();
    p.notify("resource.upsert", json!({"generation":g,"facts":facts("b")}));
    p.call("provider.sync", json!({"generation":g,"resources":[],"final":true})).await.unwrap();
    let snap = e.core.lock().unwrap().snapshot();
    assert_eq!(snap["resources"].as_array().unwrap().len(), 2);
    assert_eq!(snap["providers"]["wire"]["synced"], true);
    let a = snap["resources"].as_array().unwrap().iter().find(|r| r["backend_ref"] == "a").unwrap()["id"].clone();
    p.call("provider.sync", json!({"generation":g,"resources":[facts("a")],"final":false})).await.unwrap();
    p.close();
    until(|| e.core.lock().unwrap().snapshot()["providers"]["wire"]["state"] == "disconnected").await;
    let snap = e.core.lock().unwrap().snapshot();
    assert_eq!(snap["resources"].as_array().unwrap().len(), 2, "incomplete inventory must not remove absent resources");
    assert!(snap["resources"].as_array().unwrap().iter().all(|r| r["available"] == false));
    let (replacement, _rx) = peer(&e).await;
    let next = replacement.call("provider.register", registration("wire")).await.unwrap()["generation"].as_u64().unwrap();
    assert!(next > g);
    replacement.call("provider.sync", json!({"generation":next,"resources":[facts("a")],"final":true})).await.unwrap();
    let snap = e.core.lock().unwrap().snapshot();
    assert_eq!(snap["resources"].as_array().unwrap().len(), 1);
    assert_eq!(snap["resources"][0]["id"], a);
    replacement.notify("resource.upsert", json!({"generation":g,"facts":facts("stale")}));
    tokio::time::timeout(Duration::from_secs(1), replacement.closed()).await.unwrap();
}

#[tokio::test]
async fn effects_credit_offsets_gaps_and_dispatched_are_forwarded() {
    let e = env().await;
    let (provider, mut incoming) = peer(&e).await;
    let generation = provider.call("provider.register", registration("wire")).await.unwrap()["generation"].clone();
    provider.call("provider.sync", json!({"generation":generation,"resources":[facts("t1")],"final":true})).await.unwrap();
    let id = e.core.lock().unwrap().resources.values().find(|r| r.adapter == "wire").unwrap().id.clone();
    let actor = provider.clone();
    let gen_actor = generation.clone();
    let handler = tokio::spawn(async move {
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut offset = 0u64;
        while let Some(message) = incoming.recv().await {
            match message {
                Incoming::Request { id, method, params } => {
                    assert_eq!(params["generation"], gen_actor);
                    match method.as_str() {
                        "effect.invoke" => {
                            assert!((1..=300000).contains(&params["deadline_ms"].as_u64().unwrap()));
                            match params["op"].as_str().unwrap() {
                                "fixture.dispatch" => actor.respond(id, Ok(json!({"outcome":"dispatched","data":{"ok":true}}))),
                                "fixture.reject" => actor.respond(id, Ok(json!({"outcome":"rejected","error":{"kind":"forbidden","message":"no effect"}}))),
                                "fixture.error" => actor.respond(id, Err(prim::rpc::RpcError::new("backend", "native uncertainty"))),
                                "fixture.hang" => { let late = actor.clone(); tokio::spawn(async move { tokio::time::sleep(Duration::from_millis(40)).await; late.respond(id, Ok(json!({"outcome":"observed","data":{"late":true}}))); }); }
                                other => panic!("unexpected op {other}"),
                            }
                        }
                        "stream.open" => {
                            let stream = params["stream"].clone();
                            offset = 2;
                            actor.respond(id, Ok(json!({"initial":"repaint"})));
                            actor.notify("stream.data", json!({"generation":gen_actor,"stream":stream,"offset":0,"data":b64.encode(b"hi")}));
                        }
                        other => panic!("unexpected request {other}"),
                    }
                }
                Incoming::Notification { method, params } => {
                    assert_eq!(params["generation"], gen_actor);
                    if method == "stream.input" {
                        let bytes = b64.decode(params["data"].as_str().unwrap()).unwrap();
                        actor.notify("stream.data", json!({"generation":gen_actor,"stream":params["stream"],"offset":offset,"data":b64.encode(&bytes)}));
                        offset += bytes.len() as u64;
                        actor.notify("stream.gap", json!({"generation":gen_actor,"stream":params["stream"],"offset":offset,"reason":{"reason":"gap","gap":"paused"}}));
                    }
                }
            }
        }
    });
    let (viewer, mut output) = peer(&e).await;
    let malformed_spawn = viewer.call("resource.invoke", json!({"resource":id,"op":"terminal.spawn"})).await.unwrap_err();
    assert_eq!(malformed_spawn.kind(), "invalid"); assert_eq!(malformed_spawn.data.unwrap()["outcome"], "rejected");
    let dispatched = viewer.call("resource.invoke", json!({"resource":id,"op":"fixture.dispatch"})).await.unwrap();
    assert_eq!(dispatched["outcome"], "dispatched");
    let rejected = viewer.call("resource.invoke", json!({"resource":id,"op":"fixture.reject"})).await.unwrap_err();
    assert_eq!(rejected.kind(), "forbidden"); assert_eq!(rejected.data.unwrap()["outcome"], "rejected");
    let unknown = viewer.call("resource.invoke", json!({"resource":id,"op":"fixture.error"})).await.unwrap_err();
    assert_eq!(unknown.kind(), "backend"); assert_eq!(unknown.data.unwrap()["outcome"], "indeterminate");
    let expired = viewer.call("resource.invoke", json!({"resource":id,"op":"fixture.hang","deadline_ms":10})).await.unwrap_err();
    assert_eq!(expired.kind(), "timeout"); assert_eq!(expired.data.unwrap()["outcome"], "indeterminate");
    tokio::time::sleep(Duration::from_millis(60)).await; // Late original reply must be ignored, not poison the peer.
    assert_eq!(viewer.call("resource.invoke", json!({"resource":id,"op":"fixture.dispatch"})).await.unwrap()["outcome"], "dispatched");
    let stream = viewer.call("resource.attach", json!({"resource":id})).await.unwrap();
    assert_eq!(stream["initial"], "repaint");
    assert_eq!(viewer.call("resource.attach", json!({"resource":id})).await.unwrap_err().kind(), "overloaded");
    viewer.call("stream.input", json!({"stream":stream["stream"],"text":"yo"})).await.unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(Incoming::Notification { method, params }) = output.recv().await {
            if method == "stream.data" { bytes.extend(base64::engine::general_purpose::STANDARD.decode(params["data"].as_str().unwrap()).unwrap()); }
            if method == "stream.gap" { assert_eq!(params["reason"]["gap"], "paused"); break; }
        }
    }).await.unwrap();
    assert_eq!(bytes, b"hiyo", "gap must follow accepted bytes, never overtake them");
    provider.close(); handler.abort(); let _ = handler.await;
}

#[tokio::test]
async fn bridge_tokens_wait_rotate_recover_lost_reply_and_survive_core_restart() {
    let e = env().await;
    let (host, mut requests) = peer(&e).await;
    let hg = host.call("provider.register", registration("host-provider")).await.unwrap()["generation"].clone();
    host.call("provider.sync", json!({"generation":hg,"resources":[],"final":true})).await.unwrap();
    let (viewer, _rx) = peer(&e).await;
    let caller = viewer.clone();
    let spawn = tokio::spawn(async move { caller.call("resource.spawn", json!({"provider":"host-provider","bridge":true})).await });
    let Some(Incoming::Request { id, method, params }) = requests.recv().await else { panic!("missing host spawn") };
    assert_eq!(method, "effect.invoke");
    let bootstrap = params["args"]["env"]["PRIM_BRIDGE_TOKEN"].as_str().unwrap().to_string();
    assert_eq!(params["args"]["env"]["PRIM_SOCKET"], e.socket.to_string_lossy().as_ref());
    let mut reg = registration("runtime");
    reg["role"] = json!("runtime"); reg["capabilities"] = json!({"agent.steer":{}}); reg["token"] = json!(bootstrap);
    let (runtime, _rx) = peer(&e).await;
    let registrar = runtime.clone(); let pending_reg = reg.clone();
    let pending = tokio::spawn(async move { registrar.call("provider.register", pending_reg).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!pending.is_finished(), "runtime must wait for its durable host identity");
    host.respond(id, Ok(json!({"outcome":"observed","data":{"facts":facts("host")}})));
    let host_id = spawn.await.unwrap().unwrap()["resource"]["id"].clone();
    let first = pending.await.unwrap().unwrap();
    let first_resume = first["resume_token"].as_str().unwrap().to_string();
    let mut agent = facts("agent"); agent["kind"] = json!("agent");
    runtime.call("provider.sync", json!({"generation":first["generation"],"resources":[agent.clone()],"final":true})).await.unwrap();
    let agent_id = e.core.lock().unwrap().resources.values().find(|r| r.adapter == "runtime").unwrap().id.clone();
    assert_eq!(e.core.lock().unwrap().resources[&agent_id].to_json()["host"], host_id);
    let scoped=viewer.call("resource.list",json!({"host":host_id})).await.unwrap();
    assert_eq!(scoped["resources"].as_array().unwrap().len(),1);assert_eq!(scoped["resources"][0]["id"],agent_id);
    assert!(viewer.call("resource.list",json!({"host":"r-missing"})).await.unwrap()["resources"].as_array().unwrap().is_empty());
    assert_eq!(viewer.call("resource.list",json!({"id":host_id})).await.unwrap()["resources"].as_array().unwrap().len(),1);
    assert!(viewer.call("resource.list",json!({"id":host_id,"host":host_id})).await.unwrap()["resources"].as_array().unwrap().is_empty());
    // Pretend the first reply was lost: the bootstrap is now the previous, identity-bound credential.
    let (retry, _rx) = peer(&e).await;
    let second = retry.call("provider.register", reg.clone()).await.unwrap();
    assert_ne!(second["resume_token"], first_resume);
    tokio::time::timeout(Duration::from_secs(1), runtime.closed()).await.unwrap();
    let (ack, _rx) = peer(&e).await;
    reg["token"] = second["resume_token"].clone();
    let third = ack.call("provider.register", reg.clone()).await.unwrap();
    let (spent, _rx) = peer(&e).await;
    let mut old = reg.clone(); old["token"] = json!(bootstrap);
    assert_eq!(spent.call("provider.register", old).await.unwrap_err().kind(), "forbidden");
    let persisted: Value = serde_json::from_slice(&std::fs::read(e._dir.path().join("st/runtime-tokens.json")).unwrap()).unwrap();
    assert_eq!(persisted.as_object().unwrap().len(), 2, "credentials are bounded to current + previous");
    // A new core reads the identity map and credentials; stop the predecessor rather than
    // simulating a restart with two simultaneous writers to the same state directory.
    server::shutdown(&e.core).await;
    let socket = e._dir.path().join("new.sock");
    let restarted = server::start(socket.clone(), e._dir.path().join("st"), false, Backend::Fake, None).unwrap();
    let (resumed, _rx, _) = client::connect(&socket, "resume").await.unwrap();
    reg["token"] = third["resume_token"].clone();
    let fourth = resumed.call("provider.register", reg.clone()).await.unwrap();
    resumed.call("provider.sync", json!({"generation":fourth["generation"],"resources":[agent],"final":true})).await.unwrap();
    assert!(restarted.lock().unwrap().resources.contains_key(&agent_id), "same agent identity survives core restart");
    let (host_again, _rx, _) = client::connect(&socket, "host").await.unwrap();
    let h = host_again.call("provider.register", registration("host-provider")).await.unwrap();
    host_again.call("provider.sync", json!({"generation":h["generation"],"resources":[],"final":true})).await.unwrap();
    let (expired, _rx, _) = client::connect(&socket, "expired").await.unwrap();
    reg["token"] = fourth["resume_token"].clone();
    assert_eq!(expired.call("provider.register", reg).await.unwrap_err().kind(), "forbidden");
}

#[tokio::test]
async fn oversized_whole_facts_are_rejected_before_publication() {
    let e = env().await;
    let (p, _rx) = peer(&e).await;
    let generation = p.call("provider.register", registration("bounded")).await.unwrap()["generation"].clone();
    let mut large = facts("large"); large["status"] = json!({"payload":"x".repeat(128*1024)});
    let error = p.call("provider.sync", json!({"generation":generation,"resources":[large],"final":true})).await.unwrap_err();
    assert_eq!(error.kind(), "overloaded");
    assert!(!e.core.lock().unwrap().resources.values().any(|r| r.adapter == "bounded"));
}

#[tokio::test(flavor="multi_thread", worker_threads=2)]
#[ignore = "requires /usr/bin/tmux; test explicitly stops its owned native server"]
async fn prim_tmux_extension_interoperates_with_core_wire() {
    let e = env().await;
    struct StopOwnedServer(PathBuf);
    impl Drop for StopOwnedServer {
        fn drop(&mut self) {
            let _ = std::process::Command::new("/usr/bin/tmux").env_clear().args(["-N", "-S"])
                .arg(&self.0).arg("kill-server").output();
        }
    }
    let _server = StopOwnedServer(e._dir.path().join("st/tmux.sock"));
    let home = e._dir.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let (viewer, mut output) = peer(&e).await;
    viewer.call("workspace.apply", json!({"ops":[{"op":"set","key":"extension/tmux-wire-fixture","value":{"argv":["/usr/bin/env",format!("HOME={}",home.display()),env!("CARGO_BIN_EXE_prim-tmux"),"--name","native-tmux","--tmux","/usr/bin/tmux"]}}]})).await.unwrap();
    until(|| e.core.lock().unwrap().providers.get("native-tmux").is_some_and(|p| p.synced)).await;
    let spawned = viewer.call("resource.spawn", json!({"provider":"native-tmux","argv":["/bin/sh","-c","cat"]})).await.unwrap();
    let id = spawned["resource"]["id"].clone();
    let attached = viewer.call("resource.attach", json!({"resource":id,"cols":80,"rows":24})).await.unwrap();
    assert_eq!(attached["initial"], "repaint");
    viewer.call("stream.input", json!({"stream":attached["stream"],"text":"wire-tmux-echo\n"})).await.unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Incoming::Notification { method, params }) = output.recv().await {
            if method == "stream.data" { bytes.extend(base64::engine::general_purpose::STANDARD.decode(params["data"].as_str().unwrap()).unwrap()); }
            if String::from_utf8_lossy(&bytes).contains("wire-tmux-echo") { break; }
        }
    }).await.unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("wire-tmux-echo"));
    let killed = viewer.call("resource.kill", json!({"resource":id})).await.unwrap();
    assert!(matches!(killed["outcome"].as_str(), Some("accepted" | "observed")));
    viewer.call("workspace.apply", json!({"ops":[{"op":"delete","key":"extension/tmux-wire-fixture"}]})).await.unwrap();
    until(|| e.core.lock().unwrap().providers["native-tmux"].health.borrow().connected == false).await;
    until(|| e.core.lock().unwrap().exts.values().all(|e| e.pid.is_none())).await;
}

#[tokio::test(flavor="multi_thread", worker_threads=2)]
async fn external_sdk_fake_as_extension_spawns_echoes_kills_and_disconnects() {
    let e = env().await;
    let (viewer, mut output) = peer(&e).await;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake_provider.py");
    viewer.call("workspace.apply", json!({"ops":[{"op":"set","key":"extension/wire-fixture","value":{"argv":["python3",fixture,"--name","sdk-fake","--socket",e.socket,"--instance","sdk-instance"]}}]})).await.unwrap();
    until(|| e.core.lock().unwrap().providers.get("sdk-fake").is_some_and(|p| p.synced)).await;
    let spawn = viewer.call("resource.spawn", json!({"provider":"sdk-fake","argv":[],"name":"wire fixture"})).await.unwrap();
    assert_eq!(spawn["outcome"], "observed");
    let id = spawn["resource"]["id"].clone();
    let stream = viewer.call("resource.attach", json!({"resource":id,"history":2000})).await.unwrap();
    viewer.call("stream.input", json!({"stream":stream["stream"],"text":"external echo"})).await.unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(Incoming::Notification { method, params }) = output.recv().await {
            if method == "stream.data" { bytes.extend(base64::engine::general_purpose::STANDARD.decode(params["data"].as_str().unwrap()).unwrap()); if bytes.ends_with(b"external echo") { break; } }
        }
    }).await.unwrap();
    assert!(bytes.ends_with(b"external echo"));
    let killed = viewer.call("resource.kill", json!({"resource":id})).await.unwrap();
    assert_eq!(killed["outcome"], "observed");
    viewer.call("workspace.apply", json!({"ops":[{"op":"delete","key":"extension/wire-fixture"}]})).await.unwrap();
    until(|| e.core.lock().unwrap().providers["sdk-fake"].health.borrow().connected == false).await;
    until(|| e.core.lock().unwrap().exts.values().all(|e| e.pid.is_none())).await;
}
