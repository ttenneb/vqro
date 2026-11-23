//! Powerless fake Rex/control + fake core. Never starts Rex or a real backend.
use super::*;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt, AsyncBufReadExt, BufReader};

static NEXT: AtomicU64 = AtomicU64::new(0);

async fn read_frame<R: tokio::io::AsyncRead + Unpin>(io: &mut R) -> Value {
    let mut h = [0; 2];
    io.read_exact(&mut h).await.unwrap();
    let n = match h[1] & 127 {
        126 => io.read_u16().await.unwrap() as usize,
        127 => io.read_u64().await.unwrap() as usize,
        n => n as usize,
    };
    assert!(n < 65536);
    let mut mask = [0; 4];
    if h[1] & 128 != 0 { io.read_exact(&mut mask).await.unwrap(); }
    let mut bytes = vec![0; n];
    io.read_exact(&mut bytes).await.unwrap();
    for (i, b) in bytes.iter_mut().enumerate() { *b ^= mask[i % 4]; }
    serde_json::from_slice(&bytes).unwrap()
}

async fn send_frame<W: tokio::io::AsyncWrite + Unpin>(io: &mut W, value: Value) {
    let bytes = value.to_string().into_bytes();
    let mut frame = vec![0x81];
    if bytes.len() < 126 { frame.push(bytes.len() as u8); }
    else { frame.push(126); frame.extend_from_slice(&(bytes.len() as u16).to_be_bytes()); }
    frame.extend(bytes);
    io.write_all(&frame).await.unwrap();
}

fn answer(method: &str, mode: u8) -> Value {
    match method {
        "session.list" if mode == 4 => json!({}),
        "session.list" if mode == 5 => json!({"sessions": []}),
        "session.list" if mode == 7 || mode == 25 => json!({"$error": true}),
        "session.list" => json!({"sessions": [{"session_id": "s", "label": "vqro"}]}),
        "session.attach" if mode == 1 => json!({"$error": true}),
        "session.list_blocks" if mode == 2 => json!({"$error": true}),
        "session.list_blocks" if mode == 3 => json!({}),
        "session.list_blocks" if mode == 17 || mode == 23 || mode == 24 || mode == 26 => json!({"blocks": [
            {"block_id":"b", "creator_name":TERMINAL,"label":"block"},
            {"block_id":"unobserved", "creator_name":TERMINAL,"label":"pending"}]}),
        "session.list_blocks" => json!({"blocks": [{"block_id":"b", "creator_name":TERMINAL,"label":"block"}]}),
        "com.superlogical.terminal.title" => json!({"title": if mode == 6 { "new title" } else { "old title" }}),
        "com.superlogical.terminal.pwd" => json!({"pwd": if mode == 6 { "/new" } else { "/old" }}),
        "com.superlogical.terminal.program_status" if mode == 19 => json!({"$error":true}),
        "com.superlogical.terminal.program_status" if mode == 16 => json!({"records":[7]}),
        "com.superlogical.terminal.program_status" if mode == 18 => json!({"records":[{"state":17,"progress":"bad"}]}),
        "com.superlogical.terminal.program_status" => json!({"records": [{"state": if mode == 6 || mode == 12 { "working" } else { "idle" }}]}),
        "com.superlogical.terminal.process" if mode == 12 || mode == 17 || mode == 19 => json!({"$error":true}),
        "com.superlogical.terminal.process" if mode == 13 || mode == 23 => json!({}),
        "com.superlogical.terminal.process" if mode == 21 => json!({"child":{"name":7},"foreground":{"name":false}}),
        "com.superlogical.terminal.process" if mode == 24 => json!({"last_exit":{}}),
        "com.superlogical.terminal.process" if mode == 14 => json!({"last_exit":{}}),
        "com.superlogical.terminal.process" if mode == 15 => json!({"last_exit":{"exit_code":"bad"}}),
        "com.superlogical.terminal.process" if mode == 6 => json!({"child":{"name":"new child"},"foreground":{"name":"new foreground"},"last_exit":{"exit_code":17}}),
        "com.superlogical.terminal.process" => json!({"child":{"name":"old child"}}),
        "session.new_window" => json!({"block_ids":["created"]}),
        "com.superlogical.terminal.resize" if mode == 8 => json!({"$error":true}),
        "session.own_block" if mode == 9 => json!({"$drop":true}),
        "session.own_block" if mode == 10 => json!({"$ignore":true}),
        _ => json!({}),
    }
}

async fn fixture() -> (Arc<App>, Arc<AtomicU8>) {
    let (app, mode, _, _, _) = fixture_detailed().await;
    (app, mode)
}

struct Fixture {
    app: Arc<App>, mode: Arc<AtomicU8>, paused: Arc<Notify>, release: Arc<Notify>, published: Arc<Mutex<Vec<Value>>>,
    requests: tokio::sync::mpsc::UnboundedReceiver<Request>, core: tokio::sync::mpsc::UnboundedSender<Value>,
}

async fn fixture_detailed() -> (Arc<App>, Arc<AtomicU8>, Arc<Notify>, Arc<Notify>, Arc<Mutex<Vec<Value>>>) {
    let f = fixture_full().await;
    (f.app, f.mode, f.paused, f.release, f.published)
}

async fn fixture_full() -> Fixture {
    use std::os::unix::fs::PermissionsExt;
    let root = format!("/tmp/rxf-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = format!("{root}/s");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let mode = Arc::new(AtomicU8::new(0));
    let server_mode = mode.clone();
    let paused = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (server_paused, server_release) = (paused.clone(), release.clone());
    tokio::spawn(async move {
        let (mut io, _) = listener.accept().await.unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") { header.push(io.read_u8().await.unwrap()); }
        io.write_all(b"HTTP/1.1 101 Switching Protocols\r\nSec-WebSocket-Protocol: rex.control.v1\r\n\r\n").await.unwrap();
        let _ = read_frame(&mut io).await;
        send_frame(&mut io, json!({"type":"welcome"})).await;
        let (mut read, write) = io.into_split();
        let write = Arc::new(tokio::sync::Mutex::new(write));
        loop {
            let req = read_frame(&mut read).await;
            let mode = server_mode.load(Ordering::Relaxed);
            let payload = if mode == 25 && req["method"] == "session.new_window" {
                json!({"block_ids":[format!("created-{}", req["id"].as_str().unwrap())]})
            } else { answer(req["method"].as_str().unwrap(), mode) };
            if payload["$drop"] == true { break; }
            if payload["$ignore"] == true { continue; }
            let reply = if payload["$error"] == true { json!({"type":"response","id":req["id"],"error":{"code":"fixture","message":"observation failed"}}) }
                else { json!({"type":"response","id":req["id"],"payload":payload}) };
            if (mode == 11 && req["method"] == "com.superlogical.terminal.title")
                || (mode == 20 && req["method"] == "com.superlogical.terminal.program_status")
                || (mode == 26 && req["method"] == "com.superlogical.terminal.process" && req["payload"]["block_id"] == "unobserved") {
                let (write, paused, release) = (write.clone(), server_paused.clone(), server_release.clone());
                tokio::spawn(async move {
                    paused.notify_one();
                    release.notified().await;
                    send_frame(&mut *write.lock().await, reply).await;
                });
            } else {
                send_frame(&mut *write.lock().await, reply).await;
            }
        }
    });
    let (control, _events) = Control::connect(Path::new(&path), "test", Duration::from_secs(2)).await.unwrap();
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(root).unwrap();
    let (client, core) = tokio::io::duplex(65536);
    let published = Arc::new(Mutex::new(Vec::new()));
    let core_published = published.clone();
    let (core_tx, mut core_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        let (read, write) = tokio::io::split(core);
        let write = Arc::new(tokio::sync::Mutex::new(write));
        let injected_write = write.clone();
        tokio::spawn(async move {
            while let Some(value) = core_rx.recv().await {
                injected_write.lock().await.write_all((value.to_string() + "\n").as_bytes()).await.unwrap();
            }
        });
        let mut lines = BufReader::new(read).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            let req: Value = serde_json::from_str(&line).unwrap();
            core_published.lock().unwrap().push(req.clone());
            if req.get("id").is_some() && req.get("method").is_some() {
                let reply = json!({"jsonrpc":"2.0","id":req["id"],"result":{"generation":1}}).to_string() + "\n";
                write.lock().await.write_all(reply.as_bytes()).await.unwrap();
            }
        }
    });
    let (provider, requests) = Provider::connect(client, Config::new("fixture", "i", capabilities())).await.unwrap();
    let app = Arc::new(App { provider, control, socket: "unused".into(), owned_dir: None, spawn_label: "vqro".into(), run: "run".into(), history: 0,
        state: Mutex::new(State { next_revision:0, removed:HashSet::new(), facts_ready:false, ending:false, pending:HashMap::new(), blocks:BTreeMap::new(), sessions:HashMap::new(), killed:HashSet::new(), streams:HashMap::new() }),
        reconcile:Notify::new(), reconciling:tokio::sync::Mutex::new(()), publishing:tokio::sync::Mutex::new(()) });
    app.reconcile(true).await.unwrap();
    Fixture { app, mode, paused, release, published, requests, core:core_tx }
}

#[tokio::test]
async fn partial_session_observations_never_retire_known_blocks() {
    for failure in 1..=4 {
        let (app, mode) = fixture().await;
        let gone = Arc::new(Notify::new());
        app.state.lock().unwrap().streams.insert("b".into(), vec![gone.clone()]);
        if failure == 1 { app.state.lock().unwrap().sessions.clear(); }
        mode.store(failure, Ordering::Relaxed);
        assert!(app.reconcile(true).await.is_err(), "incomplete observation cannot full sync");
        let _ = app.reconcile(false).await;
        assert!(app.state.lock().unwrap().blocks.contains_key("b"), "failure {failure} retired live facts");
        assert!(tokio::time::timeout(Duration::from_millis(1), gone.notified()).await.is_err());
        mode.store(5, Ordering::Relaxed);
        app.reconcile(false).await.unwrap();
        assert!(!app.state.lock().unwrap().blocks.contains_key("b"), "successful session absence must retire its blocks");
    }
}

#[test]
fn malformed_unicode_percent_pwd_is_safe() {
    assert_eq!(cwd_from_pwd("file://host/%aé"), Some("/%aé".into()));
    assert_eq!(cwd_from_pwd("file://host/%é0"), Some("/%é0".into()));
    assert_eq!(cwd_from_pwd("file://host/%C3%A9"), Some("/é".into()));
}

#[tokio::test]
async fn lag_reconcile_refreshes_all_event_derived_facts() {
    let (app, mode) = fixture().await;
    mode.store(6, Ordering::Relaxed);
    // This is the reconcile called by Event::Lagged and the periodic recovery timer.
    app.reconcile(false).await.unwrap();
    let st = app.state.lock().unwrap();
    let f = &st.blocks["b"].facts;
    assert_eq!(f["title"], "new title");
    assert_eq!(f["cwd"], "/new");
    assert_eq!(f["status"]["state"], "working");
    assert_eq!(f["data"]["program"], "new child");
    assert_eq!(f["data"]["foreground"], "new foreground");
    assert_eq!(f["state"], "finished");
    assert_eq!(f["exit"]["exit_code"], 17);
}

#[tokio::test]
async fn created_but_unobserved_spawn_never_claims_exit() {
    let (app, mode) = fixture().await;
    mode.store(7, Ordering::Relaxed);
    match app.spawn(&json!({"argv":["fake"]})).await {
        Ok(Outcome::Observed(Some(value))) => {
            assert_ne!(value["facts"]["state"], "finished");
            assert!(value["facts"]["exit"].is_null());
        }
        Err(e) => { assert_eq!(e.kind, "backend"); assert!(e.message.contains("created")); }
        _ => panic!("creation evidence missing"),
    }
}

#[tokio::test]
async fn resize_after_ownership_is_not_definite_rejection() {
    let (app, mode) = fixture().await;
    mode.store(8, Ordering::Relaxed);
    let error = app.size("s", "b", 80, 24).await.unwrap_err();
    assert!(!matches!(error, control::Error::Rejected {..}), "ownership already changed: {error}");
    mode.store(10, Ordering::Relaxed);
    assert!(matches!(app.size("s", "b", 80, 24).await, Err(control::Error::Timeout)));
    mode.store(9, Ordering::Relaxed);
    assert!(matches!(app.size("s", "b", 80, 24).await, Err(control::Error::Lost(_))));
}

fn block_event(name: &str, payload: Value) -> Value {
    json!({"type":"block_event","payload":{"block_id":"b","name":name,"payload":payload}})
}

#[tokio::test]
async fn refresh_cannot_overwrite_newer_event_facts_or_publish_them_stale() {
    let (app, mode, paused, release, published) = fixture_detailed().await;
    mode.store(11, Ordering::Relaxed);
    let refreshing = { let app = app.clone(); tokio::spawn(async move { app.reconcile(false).await }) };
    tokio::time::timeout(Duration::from_secs(1), paused.notified()).await.unwrap();
    app.event(block_event("child_exited", json!({"exit_code":9}))).await;
    app.event(block_event("title_changed", json!({"title":"new event title"}))).await;
    mode.store(12, Ordering::Relaxed);
    app.event(block_event("program_status_changed", json!({}))).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if app.state.lock().unwrap().blocks["b"].facts["status"]["state"] == "working" { break; }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }).await.unwrap();
    mode.store(19, Ordering::Relaxed);
    release.notify_one();
    refreshing.await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    let facts = app.state.lock().unwrap().blocks["b"].facts.clone();
    for actual in [facts, published.lock().unwrap().iter().rev().find(|v| v["method"] == "resource.upsert").unwrap()["params"]["facts"].clone()] {
        assert_eq!(actual["title"], "new event title");
        assert_eq!(actual["state"], "finished");
        assert_eq!(actual["exit"]["exit_code"], 9);
        assert_eq!(actual["status"]["state"], "working");
    }
}

#[tokio::test]
async fn malformed_status_entries_and_fields_keep_previous_status() {
    for invalid in [16, 18] {
        let (app, mode) = fixture().await;
        mode.store(invalid, Ordering::Relaxed);
        app.refresh_status("b").await;
        assert_eq!(app.state.lock().unwrap().blocks["b"].facts["status"]["state"], "idle");
        app.reconcile(false).await.unwrap();
        assert_eq!(app.state.lock().unwrap().blocks["b"].facts["status"]["state"], "idle");
    }
}

#[tokio::test]
async fn sparse_process_does_not_erase_known_names() {
    for invalid in [13, 21] {
    let (app, mode) = fixture().await;
    app.event(block_event("process_changed", json!({"foreground":{"name":"known foreground"}}))).await;
    mode.store(invalid, Ordering::Relaxed);
    app.reconcile(false).await.unwrap();
    let facts = app.state.lock().unwrap().blocks["b"].facts.clone();
    assert_eq!(facts["data"]["program"], "old child");
    assert_eq!(facts["data"]["foreground"], "known foreground");
    }
}

#[tokio::test]
async fn malformed_exit_is_not_a_lifecycle_witness() {
    for invalid in [14, 15] {
        let (app, mode) = fixture().await;
        mode.store(invalid, Ordering::Relaxed);
        app.reconcile(false).await.unwrap();
        let facts = app.state.lock().unwrap().blocks["b"].facts.clone();
        assert_eq!(facts["state"], "live");
        assert!(facts["exit"].is_null());
    }
}

#[tokio::test]
async fn new_block_without_lifecycle_observation_is_not_published_live() {
    for invalid in [17, 23, 24] {
    let (app, mode, _, _, published) = fixture_detailed().await;
    mode.store(invalid, Ordering::Relaxed);
    let _ = app.reconcile(false).await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!app.state.lock().unwrap().blocks.contains_key("unobserved"));
    assert!(!published.lock().unwrap().iter().any(|v| v["params"]["facts"]["ref"] == "unobserved"));
    }
}

#[tokio::test]
async fn status_query_cannot_overwrite_a_later_status_event() {
    let (app, mode, paused, release, _) = fixture_detailed().await;
    mode.store(20, Ordering::Relaxed);
    let refreshing = { let app = app.clone(); tokio::spawn(async move { app.refresh_status("b").await }) };
    tokio::time::timeout(Duration::from_secs(1), paused.notified()).await.unwrap();
    mode.store(12, Ordering::Relaxed);
    app.event(block_event("program_status_changed", json!({}))).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if app.state.lock().unwrap().blocks["b"].facts["status"]["state"] == "working" { break; }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }).await.unwrap();
    release.notify_one();
    refreshing.await.unwrap();
    assert_eq!(app.state.lock().unwrap().blocks["b"].facts["status"]["state"], "working");
}

#[tokio::test]
async fn kill_invalidates_a_pre_kill_live_candidate() {
    let (app, mode, paused, release, published) = fixture_detailed().await;
    mode.store(11, Ordering::Relaxed);
    let refresh = { let app = app.clone(); tokio::spawn(async move { app.reconcile(false).await }) };
    tokio::time::timeout(Duration::from_secs(1), paused.notified()).await.unwrap();
    app.kill(Some("b"), "run").await.unwrap();
    mode.store(19, Ordering::Relaxed);
    release.notify_one();
    refresh.await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(app.state.lock().unwrap().blocks["b"].facts["state"], "finished");
    assert_eq!(published.lock().unwrap().iter().rev().find(|v| v["method"] == "resource.upsert").unwrap()["params"]["facts"]["state"], "finished");
}

#[tokio::test]
async fn removal_is_final_and_a_delayed_status_cannot_cross_local_lifetimes() {
    let (app, mode, paused, release, published) = fixture_detailed().await;
    mode.store(20, Ordering::Relaxed);
    let refresh = { let app = app.clone(); tokio::spawn(async move { app.refresh_status("b").await }) };
    tokio::time::timeout(Duration::from_secs(1), paused.notified()).await.unwrap();
    mode.store(5, Ordering::Relaxed);
    app.reconcile(false).await.unwrap();
    mode.store(6, Ordering::Relaxed);
    app.reconcile(false).await.unwrap();
    release.notify_one();
    refresh.await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!app.state.lock().unwrap().blocks.contains_key("b"), "removed refs cannot be rediscovered in this registration");
    let rows = published.lock().unwrap();
    let gone = rows.iter().position(|v| v["method"] == "resource.gone" && v["params"]["ref"] == "b").unwrap();
    assert!(!rows[gone+1..].iter().any(|v| v["method"] == "resource.upsert" && v["params"]["facts"]["ref"] == "b"));
}

#[tokio::test]
async fn unknown_id_events_allocate_no_bookkeeping() {
    let (app, _) = fixture().await;
    let before = app.state.lock().unwrap().next_revision;
    for n in 0..1000 {
        app.event(json!({"type":"block_event","payload":{"block_id":format!("unknown-{n}"),
            "name":"title_changed","payload":{"title":"ignored"}}})).await;
    }
    let state = app.state.lock().unwrap();
    assert_eq!(state.blocks.len(), 1);
    assert!(state.removed.is_empty());
    assert_eq!(state.next_revision, before, "unknown ids must not even consume revisions");
}

#[tokio::test]
async fn kill_publication_waits_for_the_fact_owner() {
    let (app, _) = fixture().await;
    let owner = app.publishing.lock().await;
    let kill = { let app = app.clone(); tokio::spawn(async move { app.kill(Some("b"), "run").await }) };
    tokio::time::sleep(Duration::from_millis(30)).await;
    let bypassed = kill.is_finished();
    drop(owner);
    kill.await.unwrap().unwrap();
    assert!(!bypassed, "kill result bypassed fact/publication owner");
}

#[tokio::test]
async fn effect_replies_cannot_publish_a_snapshot_older_than_an_event_or_removal() {
    let mut f = fixture_full().await;
    for (n, remove) in [(1, false), (2, true)] {
        f.core.send(json!({"jsonrpc":"2.0","id":format!("effect-{n}"),"method":"effect.invoke",
            "params":{"generation":1,"effect":format!("kill-{n}"),"op":"terminal.kill","ref":"b","incarnation":"run","args":{},"deadline_ms":1000}})).unwrap();
        let Request::Effect(request) = tokio::time::timeout(Duration::from_secs(1), f.requests.recv()).await.unwrap().unwrap() else { panic!("effect request"); };
        let stale = f.app.state.lock().unwrap().blocks["b"].facts.clone();
        if remove {
            f.app.write_facts(Change::Remove { bid:"b".into(), incarnation:"run".into(), reason:json!({"reason":"exited"}) }).await.unwrap();
        } else {
            f.app.event(block_event("child_exited", json!({"exit_code":31}))).await;
        }
        f.app.write_facts(Change::Reply { request, result:Ok(Outcome::Observed(Some(json!({"facts":stale})))) }).await.unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Some(row) = f.published.lock().unwrap().iter().find(|v| v["id"] == format!("effect-{n}")).cloned() { break row; }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }).await.unwrap();
        if remove { assert!(reply["result"]["data"]["facts"].is_null()); }
        else {
            assert_eq!(reply["result"]["data"]["facts"]["state"], "finished");
            assert_eq!(reply["result"]["data"]["facts"]["exit"]["exit_code"], 31);
        }
    }
}

#[tokio::test]
async fn revisions_never_reset_and_retirement_budget_fails_closed() {
    let (app, _) = fixture().await;
    let (old, mut facts) = { let st = app.state.lock().unwrap(); (st.blocks["b"].revision, st.blocks["b"].facts.clone()) };
    app.write_facts(Change::Remove { bid:"b".into(), incarnation:"run".into(), reason:json!({"reason":"exited"}) }).await.unwrap();
    facts["ref"] = json!("c");
    let observation = app.write_facts(Change::Begin { bid:"c".into() }).await.unwrap().unwrap();
    app.write_facts(Change::Refresh { bid:"c".into(), session:"s".into(), base:observation["revision"].as_u64(), token:observation["token"].as_u64(), facts:facts.clone() }).await.unwrap();
    assert!(app.state.lock().unwrap().blocks["c"].revision > old);
    {
        let mut state = app.state.lock().unwrap();
        for n in 0..facts::MAX_REGISTRATION_REFS-2 { state.removed.insert(format!("retired-{n}")); }
        assert_eq!(state.blocks.len() + state.removed.len(), facts::MAX_REGISTRATION_REFS);
    }
    assert!(app.write_facts(Change::Begin { bid:"overflow".into() }).await.is_err());
    assert!(app.provider.is_closed());
    let state = app.state.lock().unwrap();
    assert!(state.removed.contains("b"));
    assert!(!state.blocks.contains_key("overflow"));
}

#[tokio::test]
async fn pending_discovery_exit_invalidates_its_captured_live_process() {
    let (app, mode, paused, release, published) = fixture_detailed().await;
    mode.store(26, Ordering::Relaxed);
    let refresh = { let app = app.clone(); tokio::spawn(async move { app.reconcile(false).await }) };
    tokio::time::timeout(Duration::from_secs(1), paused.notified()).await.unwrap();
    assert!(!app.state.lock().unwrap().blocks.contains_key("unobserved"));
    app.event(json!({"type":"block_event","payload":{"block_id":"unobserved","name":"child_exited","payload":{"exit_code":42}}})).await;
    release.notify_one();
    refresh.await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!app.state.lock().unwrap().blocks.contains_key("unobserved"), "an older live discovery survived the exit");
    assert!(!published.lock().unwrap().iter().any(|v| v["method"] == "resource.upsert"
        && v["params"]["facts"]["ref"] == "unobserved" && v["params"]["facts"]["state"] == "live"));
}

#[tokio::test]
async fn unobserved_created_argv_is_not_retained_after_failure() {
    let (app, mode) = fixture().await;
    mode.store(25, Ordering::Relaxed);
    for _ in 0..20 {
        let error = app.spawn(&json!({"argv":["fake","argument"]})).await.unwrap_err();
        assert!(error.message.contains("created"));
    }
    assert!(app.state.lock().unwrap().pending.is_empty(), "unobserved created argv entries leaked");
}

#[tokio::test]
async fn discovery_cancellation_clears_its_pending_stamp() {
    let (app, mode, paused, release, _) = fixture_detailed().await;
    mode.store(26, Ordering::Relaxed);
    let refresh = { let app = app.clone(); tokio::spawn(async move { app.reconcile(false).await }) };
    tokio::time::timeout(Duration::from_secs(1), paused.notified()).await.unwrap();
    assert_eq!(app.state.lock().unwrap().pending.len(), 1);
    refresh.abort();
    assert!(refresh.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(1), async {
        while !app.state.lock().unwrap().pending.is_empty() { tokio::time::sleep(Duration::from_millis(1)).await; }
    }).await.unwrap();
    release.notify_one();
}

#[tokio::test]
async fn pending_tokens_protect_newer_argv_and_registration_end_clears_them() {
    let (app, _) = fixture().await;
    let old = app.write_facts(Change::Begin { bid:"new".into() }).await.unwrap().unwrap();
    let created = app.write_facts(Change::Created { bid:"new".into(), argv:vec!["command".into()] }).await.unwrap().unwrap();
    app.write_facts(Change::Cancel { bid:"new".into(), token:old["token"].as_u64().unwrap() }).await.unwrap();
    let observation = app.write_facts(Change::Begin { bid:"new".into() }).await.unwrap().unwrap();
    assert_eq!(observation["argv"], json!(["command"]));
    app.write_facts(Change::Cancel { bid:"new".into(), token:created["token"].as_u64().unwrap() }).await.unwrap();
    assert_eq!(app.state.lock().unwrap().pending.len(), 1);
    app.write_facts(Change::Remove { bid:"new".into(), incarnation:"run".into(), reason:json!({"reason":"exited"}) }).await.unwrap();
    assert!(app.state.lock().unwrap().pending.is_empty());
    app.write_facts(Change::Created { bid:"another".into(), argv:vec!["arg".into()] }).await.unwrap();
    app.write_facts(Change::End).await.unwrap();
    assert!(app.state.lock().unwrap().pending.is_empty());
    assert!(app.write_facts(Change::Created { bid:"late".into(), argv:vec!["arg".into()] }).await.unwrap().is_none());
    assert!(app.state.lock().unwrap().pending.is_empty());
}

#[tokio::test]
async fn pending_identity_and_argv_storage_budgets_are_enforced() {
    let (app, _) = fixture().await;
    let error = app.spawn(&json!({"argv":["x".repeat(facts::MAX_ARGV_BYTES)]})).await.unwrap_err();
    assert_eq!(error.kind, "invalid");
    assert!(error.message.contains("nothing was created"));
    for n in 0..facts::MAX_PENDING_REFS {
        app.write_facts(Change::Created { bid:format!("pending-{n}"), argv:vec!["arg".into()] }).await.unwrap();
    }
    assert!(app.write_facts(Change::Begin { bid:"overflow".into() }).await.is_err());
    assert_eq!(app.state.lock().unwrap().pending.len(), facts::MAX_PENDING_REFS);
    assert!(app.provider.is_closed());
    app.write_facts(Change::End).await.unwrap();
    assert!(app.state.lock().unwrap().pending.is_empty());
}
