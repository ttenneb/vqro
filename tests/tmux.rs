//! Opt-in real tmux 3.7b tests: cargo test --test tmux -- --ignored
//! Every child has an allowlisted environment and unique explicit -S socket. Drop kills ONLY it.
#![cfg(unix)]
#[path = "support/python.rs"] mod test_python;
use vqro::tmux::{Client, Config, Error, GapReason, Notification, PaneEvent, PaneId, PaneStream};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

struct Server {
    root: tempfile::TempDir,
    socket: PathBuf,
}
impl Server {
    fn new() -> Self {
        assert_eq!(Command::new("/usr/bin/tmux").arg("-V").env_clear().output().unwrap().stdout, b"tmux 3.7b\n");
        let root = tempfile::Builder::new().prefix("ptmx").tempdir_in("/tmp").unwrap();
        let server = Self { socket: root.path().join("s"), root };
        server.start();
        server
    }
    fn cli(&self, args: &[&str]) -> Output {
        Command::new("/usr/bin/tmux")
            .args(["-N", "-S"])
            .arg(&self.socket)
            .args(["-f", "/dev/null"])
            .args(args)
            .env_clear()
            .env("HOME", self.root.path())
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "xterm-256color")
            .output()
            .unwrap()
    }
    fn start(&self) {
        let out = Command::new("/usr/bin/tmux")
            .arg("-S")
            .arg(&self.socket)
            .args(["-f", "/dev/null", "new-session", "-d", "-s", "test", "-x", "100", "-y", "30", "/bin/sh", "-c", "exec sleep 600"])
            .env_clear()
            .env("HOME", self.root.path())
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "xterm-256color")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(self.cli(&["set-option", "-g", "status", "off"]).status.success());
    }
    fn stop(&self) {
        let _ = self.cli(&["kill-server"]);
        assert!(!self.cli(&["list-sessions"]).status.success());
    }
    async fn client(&self, capacity: usize) -> Client {
        let mut config = Config::new(self.socket.clone(), "test");
        config.binary = "/usr/bin/tmux".into();
        config.pane_capacity = capacity;
        Client::connect(config).await.unwrap()
    }
    async fn worker(&self, client: &Client, name: &str) -> PaneId {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tmux_worker.py");
        let done = self.root.path().join(format!("{name}.done"));
        let pane =
            client.spawn(&[test_python::python(), fixture.to_str().unwrap(), done.to_str().unwrap()], Some(self.root.path())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let screen = client.command(&["capture-pane", "-p", "-t", pane.as_str()]).await.unwrap();
                if screen.iter().any(|s| contains(s, b"WORKER-READY")) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        pane
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|s| s == needle)
}
async fn note(rx: &mut broadcast::Receiver<Notification>, predicate: impl Fn(&Notification) -> bool) -> Notification {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let n = rx.recv().await.unwrap();
            if predicate(&n) {
                return n;
            }
        }
    })
    .await
    .unwrap()
}
async fn until_data(stream: &mut PaneStream, needle: &[u8]) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(4), async {
        let mut data = Vec::new();
        loop {
            match stream.recv().await {
                PaneEvent::Data { bytes, .. } => data.extend_from_slice(&bytes),
                e => panic!("unexpected event: {e:?}"),
            }
            if contains(&data, needle) {
                return data;
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires /usr/bin/tmux 3.7b; private server"]
async fn native_effects_output_subscriptions_and_no_cross_pane_mix() {
    let server = Server::new();
    let client = server.client(128).await;
    let mut notes = client.notifications();
    let a = server.worker(&client, "a").await;
    let b = server.worker(&client, "b").await;
    note(&mut notes, |n| matches!(n, Notification::WindowAdded { .. })).await;
    let mut sa = client.watch(a.clone()).unwrap();
    let mut sb = client.watch(b.clone()).unwrap();
    client.input(&a, b"ECHO A\x00\\\xff\n").await.unwrap();
    client.input(&b, b"ECHO beta\n").await.unwrap();
    let da = until_data(&mut sa, b"ECHO:A\x00\\\xff\r\n").await;
    let db = until_data(&mut sb, b"ECHO:beta\r\n").await;
    assert!(!contains(&da, b"beta"));
    assert!(!contains(&db, b"ECHO:A"));
    client.input(&a, b"OSC alpha\n").await.unwrap();
    client.input(&b, b"OSC beta\n").await.unwrap();
    for (stream, tag) in [(&mut sa, "alpha"), (&mut sb, "beta")] {
        let mut status = None;
        let mut raw_bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), async {
            while status.is_none() || !contains(&raw_bytes, b"AFTER") {
                match stream.recv().await {
                    PaneEvent::Data { bytes, .. } => raw_bytes.extend_from_slice(&bytes),
                    PaneEvent::Osc7501(bytes) => status = Some(bytes),
                    e => panic!("{e:?}"),
                }
            }
        })
        .await
        .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&status.unwrap()).unwrap();
        assert_eq!(v["tag"], tag);
        assert!(contains(&raw_bytes, b"\x1b]7501;"));
    }
    let value = "literal ' quote; $HOME \\ # {hi}";
    client.command(&["set-option", "-p", "-t", a.as_str(), "@probe", value]).await.unwrap();
    assert_eq!(client.command(&["show-option", "-p", "-v", "-t", a.as_str(), "@probe"]).await.unwrap(), vec![value.as_bytes()]);
    client.subscribe_format("pane", Some(("%*", "#{pane_id}=#{@probe}"))).await.unwrap();
    note(
        &mut notes,
        |n| matches!(n, Notification::Subscription { pane, value: v, .. } if pane == a.as_str() && contains(v, value.as_bytes())),
    )
    .await;
    client.command(&["set-option", "-p", "-t", a.as_str(), "@probe", "changed"]).await.unwrap();
    note(&mut notes, |n| matches!(n, Notification::Subscription { pane, value, .. } if pane == a.as_str() && contains(value, b"changed")))
        .await;
    client.subscribe_format("pane", None).await.unwrap();
    let window =
        String::from_utf8(client.command(&["display-message", "-p", "-t", a.as_str(), "#{window_id}"]).await.unwrap().remove(0)).unwrap();
    client.resize_window(&window, 91, 27).await.unwrap();
    client.resize_pane(&a, 91, 27).await.unwrap();
    assert_eq!(
        client.command(&["display-message", "-p", "-t", a.as_str(), "#{pane_width}x#{pane_height}"]).await.unwrap(),
        vec![b"91x27".to_vec()]
    );
    note(&mut notes, |n| matches!(n, Notification::Layout { window: w, .. } if w == &window)).await;
    assert!(matches!(client.command(&["no-such-command"]).await, Err(Error::Rejected(_))));
    assert_eq!(client.command(&["display-message", "-p", "still-aligned"]).await.unwrap(), vec![b"still-aligned".to_vec()]);
    client.kill(&a).await.unwrap();
    note(&mut notes, |n| matches!(n, Notification::WindowClosed { window: w, .. } if w == &window)).await;
    client.close().await;
    assert!(server.cli(&["list-sessions"]).status.success(), "detaching must not kill the server");
}

#[tokio::test]
#[ignore = "requires /usr/bin/tmux 3.7b; private server"]
async fn server_incarnation_survives_reconnect_but_not_restart_with_reused_ids() {
    let server = Server::new();
    let (a, b) = tokio::join!(server.client(32), server.client(32));
    assert_eq!(a.instance(), b.instance(), "concurrent attachments share one backend incarnation");
    let instance = a.instance().to_owned();
    let first = server.cli(&["list-panes", "-F", "#{pane_id}"]).stdout;
    a.close().await;
    b.close().await;
    let again = server.client(32).await;
    assert_eq!(instance, again.instance());
    let mut closed = again.notifications();
    let mut old_stream = again.watch(PaneId::parse("%0").unwrap()).unwrap();
    server.stop();
    note(&mut closed, |n| matches!(n, Notification::Disconnected { .. })).await;
    assert_eq!(old_stream.recv().await, PaneEvent::Closed);
    assert!(matches!(again.command(&["display-message", "-p", "never-replay"]).await, Err(Error::Disconnected)));
    server.start();
    let fresh = server.client(32).await;
    assert_eq!(first, server.cli(&["list-panes", "-F", "#{pane_id}"]).stdout);
    assert_ne!(instance, fresh.instance());
    // The independent capture path must also refuse a replaced backend identity atomically.
    fresh.command(&["set-option", "-s", "@vqro_control_instance", "00000000000000000000000000000000"]).await.unwrap();
    assert!(matches!(fresh.resume(&PaneId::parse("%0").unwrap()).await, Err(Error::Disconnected)));
    fresh.close().await;
}

#[tokio::test]
#[ignore = "requires /usr/bin/tmux 3.7b; private server"]
async fn cancellation_extra_blocks_and_timeouts_never_replay_or_shift_replies() {
    let server = Server::new();
    let client = server.client(32).await;
    // if-shell produces an additional synchronous command block. Random barriers must contain it.
    client.command(&["if-shell", "-F", "1", "display-message -p nested"]).await.unwrap();
    assert_eq!(
        client.command(&["display-message", "-p", "-l", "%pause %1"]).await.unwrap(),
        vec![b"%pause %1".to_vec()],
        "arbitrary reply bodies stay opaque"
    );
    let started = server.root.path().join("started");
    let once = server.root.path().join("once");
    let command = format!("printf start > {}; sleep .15; printf x >> {}", started.display(), once.display());
    let caller = {
        let client = client.clone();
        tokio::spawn(async move { client.command(&["run-shell", &command]).await })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while !started.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    caller.abort();
    assert_eq!(client.command(&["display-message", "-p", "after-cancel"]).await.unwrap(), vec![b"after-cancel".to_vec()]);
    assert_eq!(std::fs::read(&once).unwrap(), b"x", "cancelling a caller never replays its effect");
    client.close().await;

    let mut config = Config::new(server.socket.clone(), "test");
    config.binary = "/usr/bin/tmux".into();
    config.command_timeout = Duration::from_millis(200);
    let short = Client::connect(config).await.unwrap();
    let mut notes = short.notifications();
    let effect = server.root.path().join("before-timeout");
    let result = short.command(&["run-shell", &format!("printf x > {}; sleep .5", effect.display())]).await;
    assert!(matches!(result, Err(Error::Indeterminate(_))));
    note(&mut notes, |n| matches!(n, Notification::Disconnected { .. })).await;
    assert_eq!(std::fs::read(effect).unwrap(), b"x");
    assert!(matches!(short.command(&["display-message", "-p", "no-reconnect"]).await, Err(Error::Disconnected)));
    assert!(short.watch(PaneId::parse("%0").unwrap()).is_err());
    assert!(server.cli(&["list-sessions"]).status.success());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires /usr/bin/tmux 3.7b; private server and deliberate 3s reader stall"]
async fn native_pause_after_is_gap_then_labelled_capture_repaint_not_raw_history() {
    let server = Server::new();
    let client = server.client(1024).await;
    let pane = server.worker(&client, "flood").await;
    let mut native_notes = client.notifications();
    let window = String::from_utf8(server.cli(&["display-message", "-p", "-t", pane.as_str(), "#{window_id}"]).stdout).unwrap();
    assert!(server.cli(&["select-window", "-t", window.trim()]).status.success());
    let mut stream = client.watch(pane.clone()).unwrap();
    let collection = tokio::spawn(async move {
        let mut events = Vec::new();
        let mut tail = Vec::new();
        let mut continued = false;
        let mut pong = false;
        loop {
            let event = stream.recv().await;
            continued |= event == PaneEvent::Continued;
            if let PaneEvent::Data { bytes, .. } = &event {
                tail.extend_from_slice(bytes);
                pong |= contains(&tail, b"PONG");
                if tail.len() > 3 {
                    tail.drain(..tail.len() - 3);
                }
            }
            assert_ne!(event, PaneEvent::Closed);
            events.push(event);
            if continued && pong {
                return events;
            }
        }
    });
    // Independent CLI triggers output without first letting this client's command barrier drain it.
    assert!(server.cli(&["send-keys", "-t", pane.as_str(), "-l", "FLOOD 80000"]).status.success());
    assert!(server.cli(&["send-keys", "-t", pane.as_str(), "-H", "0a"]).status.success());
    // Deliberate stimulus, not a readiness assumption: stop draining the pipe on this runtime (blocking
    // calls only, so the reader cannot run) while the independent tmux server and producer continue.
    // tmux pauses a pane when it ages that pane's pending output, which it does as new output arrives.
    // So, still stalled: wait for the whole flood, let its pending output grow older than pause-after,
    // then make the pane produce more through the independent CLI. An async wait here let the reader
    // drain under load (no pause: "continuation not observed", 5/12 runs under CPU load).
    // tmux stops reading a pane while every attached client is a control client that cannot take more
    // (server-client.c server_client_check_pane_buffer); then nothing ages the stalled client's output
    // and whether it pauses is timing. A no-output control client always counts as able to take output
    // (control.c control_pane_offset), so the pane keeps being read and each read ages our client.
    let mut keep_reading = Command::new("/usr/bin/tmux")
        .args(["-N", "-S"])
        .arg(&server.socket)
        .args(["-f", "/dev/null", "-C", "attach-session", "-t", "test", "-f", "no-output"])
        .env_clear()
        .env("HOME", server.root.path())
        .env("PATH", "/usr/bin:/bin")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let done = server.root.path().join("flood.done");
    let started = Instant::now();
    while !done.exists() {
        assert!(started.elapsed() < Duration::from_secs(20), "flood did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(1000 * u64::from(Config::new(PathBuf::new(), "").pause_after) + 500));
    for _ in 0..3 {
        assert!(server.cli(&["send-keys", "-t", pane.as_str(), "-l", "ECHO tick"]).status.success());
        assert!(server.cli(&["send-keys", "-t", pane.as_str(), "-H", "0a"]).status.success());
        std::thread::sleep(Duration::from_millis(200));
    }
    drop(keep_reading.stdin.take());
    let _ = keep_reading.kill();
    let _ = keep_reading.wait();
    // Pause notes may be delayed until control activity. Capture/continue flushes those notes too.
    client.resume(&pane).await.unwrap();
    note(&mut native_notes, |n| matches!(n, Notification::Pause(p) if p == &pane)).await;
    client.input(&pane, b"PING\n").await.unwrap();
    let mut gap = false;
    let mut repaint = false;
    let mut continued = false;
    let mut raw_bytes = Vec::new();
    let mut statuses = Vec::new();
    let events = tokio::time::timeout(Duration::from_secs(5), collection).await.unwrap().unwrap();
    for event in events {
        match event {
            PaneEvent::Gap(GapReason::Paused | GapReason::RecoveryRequested) => gap = true,
            PaneEvent::Gap(other) => panic!("unexpected subscriber lag: {other:?}"),
            PaneEvent::Repaint { screen } => {
                assert!(gap, "gap precedes repaint");
                assert!(contains(&screen, b"FLOOD-END"));
                assert!(!contains(&screen, b"7501"));
                repaint = true;
            }
            PaneEvent::Continued => {
                assert!(repaint);
                continued = true;
            }
            PaneEvent::Data { bytes, .. } => raw_bytes.extend_from_slice(&bytes),
            PaneEvent::Osc7501(s) => statuses.push(s),
            PaneEvent::Closed => panic!("closed"),
        }
    }
    assert!(gap && repaint && continued);
    assert!(!contains(&raw_bytes, b"FLOOD-END"), "lost output was not replayed");
    assert!(statuses.is_empty(), "capture must not fabricate lost OSC status");
    client.close().await;
}

#[tokio::test]
#[ignore = "requires /usr/bin/tmux 3.7b; private server"]
async fn read_only_timeouts_under_a_flood_keep_the_client_and_replies_paired() {
    let server = Server::new();
    let mut config = Config::new(server.socket.clone(), "test");
    config.binary = "/usr/bin/tmux".into();
    config.command_timeout = Duration::from_millis(30);
    let client = Client::connect(config).await.unwrap();
    let mut notes = client.notifications();
    // A long flood in a second pane of this session: its %output precedes our replies on the pipe.
    let mut long = Config::new(server.socket.clone(), "test");
    long.binary = "/usr/bin/tmux".into();
    let spawner = Client::connect(long).await.unwrap();
    // Optional load-harness rendezvous: children are already attached on their normal CPUs;
    // the harness may now constrain only the reader/test threads, not throttle the producer.
    if let Some(path) = std::env::var_os("TMUX_FLOOD_LOAD_READY") {
        let path = PathBuf::from(path);
        std::fs::write(&path, server.socket.as_os_str().as_encoded_bytes()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !path.with_extension("go").exists() { tokio::time::sleep(Duration::from_millis(1)).await; }
        }).await.unwrap();
    }
    spawner.spawn(&["/bin/sh", "-c", "yes vqro-flood-line | head -n 3000000; sleep 600"], None).await.unwrap();
    let (mut ok, mut timed_out) = (0, 0);
    for i in 0..300 {
        let probe = format!("probe-{i}");
        match client.command(&["display-message", "-p", &probe]).await {
            Ok(lines) => {
                assert_eq!(lines, vec![probe.into_bytes()], "a late reply never pairs with a later command");
                ok += 1;
            }
            Err(Error::TimedOut(_)) => timed_out += 1,
            Err(e) => {
                let mut diagnostic = Vec::new();
                loop {
                    match notes.try_recv() {
                        Ok(note) => diagnostic.push(format!("{note:?}")),
                        Err(broadcast::error::TryRecvError::Lagged(n)) => diagnostic.push(format!("lagged {n}")),
                        Err(_) => break,
                    }
                }
                panic!("read-only timeouts are not fatal: {e:?} after {ok} ok, {timed_out} timed out; notifications={diagnostic:?}");
            },
        }
    }
    while let Ok(n) = notes.try_recv() {
        assert!(!matches!(n, Notification::Disconnected { .. }), "the control client stayed attached");
    }
    eprintln!("under flood: {ok} replies, {timed_out} timeouts (30 ms)");
    // Still paired and alive afterwards.
    let mut answered = false;
    for _ in 0..200 {
        match client.command(&["display-message", "-p", "final"]).await {
            Ok(lines) => {
                assert_eq!(lines, vec![b"final".to_vec()]);
                answered = true;
                break;
            }
            Err(Error::TimedOut(_)) => tokio::time::sleep(Duration::from_millis(20)).await,
            Err(e) => panic!("{e:?}"),
        }
    }
    assert!(answered);
    client.close().await;
    spawner.close().await;
}

#[tokio::test]
#[ignore = "requires /usr/bin/tmux 3.7b; private server"]
async fn sixteen_kib_of_arbitrary_bytes_arrive_intact_in_one_input_call() {
    // One `send-keys -H` per input once failed above ~8 000 bytes ("parse error: yacc stack overflow"),
    // so pastes into tmux panes were rejected whole. The client now splits input into several commands.
    let server = Server::new();
    let client = server.client(128).await;
    let out = server.root.path().join("input.bin");
    let script = format!("stty raw -echo; head -c 16384 > {}; echo DONE", out.display());
    let pane = client.spawn(&["/bin/sh", "-c", &script], Some(server.root.path())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let bytes: Vec<u8> = (0..16384u32).map(|i| (i * 7 % 256) as u8).collect();
    client.input(&pane, &bytes).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0) < bytes.len() as u64 {
        assert!(Instant::now() < deadline, "input did not arrive: {:?} bytes", std::fs::metadata(&out).map(|m| m.len()));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(std::fs::read(&out).unwrap(), bytes, "every byte value arrives once, in order");
}
