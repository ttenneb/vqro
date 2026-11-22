//! Powerless fake Rex/control + fake core. Never starts Rex or a real backend.
use super::*;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt, AsyncBufReadExt, BufReader};

static NEXT: AtomicU64 = AtomicU64::new(0);

async fn read_frame(io: &mut tokio::net::UnixStream) -> Value {
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

async fn send_frame(io: &mut tokio::net::UnixStream, value: Value) {
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
        "session.list" if mode == 7 => json!({"$error": true}),
        "session.list" => json!({"sessions": [{"session_id": "s", "label": "vqro"}]}),
        "session.attach" if mode == 1 => json!({"$error": true}),
        "session.list_blocks" if mode == 2 => json!({"$error": true}),
        "session.list_blocks" if mode == 3 => json!({}),
        "session.list_blocks" => json!({"blocks": [{"block_id":"b", "creator_name":TERMINAL,"label":"block"}]}),
        "com.superlogical.terminal.title" => json!({"title": if mode == 6 { "new title" } else { "old title" }}),
        "com.superlogical.terminal.pwd" => json!({"pwd": if mode == 6 { "/new" } else { "/old" }}),
        "com.superlogical.terminal.program_status" => json!({"records": [{"state": if mode == 6 { "working" } else { "idle" }}]}),
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
    use std::os::unix::fs::PermissionsExt;
    let root = format!("/tmp/rxf-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = format!("{root}/s");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let mode = Arc::new(AtomicU8::new(0));
    let server_mode = mode.clone();
    tokio::spawn(async move {
        let (mut io, _) = listener.accept().await.unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") { header.push(io.read_u8().await.unwrap()); }
        io.write_all(b"HTTP/1.1 101 Switching Protocols\r\nSec-WebSocket-Protocol: rex.control.v1\r\n\r\n").await.unwrap();
        let _ = read_frame(&mut io).await;
        send_frame(&mut io, json!({"type":"welcome"})).await;
        loop {
            let req = read_frame(&mut io).await;
            let payload = answer(req["method"].as_str().unwrap(), server_mode.load(Ordering::Relaxed));
            if payload["$drop"] == true { break; }
            if payload["$ignore"] == true { continue; }
            let reply = if payload["$error"] == true { json!({"type":"response","id":req["id"],"error":{"code":"fixture","message":"observation failed"}}) }
                else { json!({"type":"response","id":req["id"],"payload":payload}) };
            send_frame(&mut io, reply).await;
        }
    });
    let (control, _events) = Control::connect(Path::new(&path), "test", Duration::from_millis(200)).await.unwrap();
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(root).unwrap();
    let (client, core) = tokio::io::duplex(65536);
    tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(core);
        let mut lines = BufReader::new(read).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            let req: Value = serde_json::from_str(&line).unwrap();
            if req.get("id").is_some() {
                let reply = json!({"jsonrpc":"2.0","id":req["id"],"result":{"generation":1}}).to_string() + "\n";
                write.write_all(reply.as_bytes()).await.unwrap();
            }
        }
    });
    let (provider, _) = Provider::connect(client, Config::new("fixture", "i", capabilities())).await.unwrap();
    let app = Arc::new(App { provider, control, socket: "unused".into(), spawn_label: "vqro".into(), run: "run".into(), history: 0,
        state: Mutex::new(State { blocks:BTreeMap::new(), sessions:HashMap::new(), killed:HashSet::new(), argv:HashMap::new(), streams:HashMap::new() }),
        reconcile:Notify::new(), reconciling:tokio::sync::Mutex::new(()) });
    app.reconcile(true).await.unwrap();
    (app, mode)
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
