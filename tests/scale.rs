//! Scale harness: a deterministic load test of a real prim core over its socket, with budgets.
//!
//! Ignored by default. Run (see docs/test-plan.md, "Scale harness"):
//!   cargo test --release --test scale -- --ignored --exact scale_load --nocapture
//!
//! The core runs in its own process (this test binary re-executed as `scale_core_child`), so its RSS
//! is measured alone, with the fake backend and its load profile (`src/adapter/fake.rs`). Two fake
//! adapter instances stand in for two hosts. Load: N terminals, some agent-like (status changes),
//! some high-output; one observer client that drains everything, one writer client issuing
//! workspace transactions and extension restarts, and stalled clients that subscribe and attach but
//! never read. The test writes a JSON receipt, then fails if any budget is exceeded.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use prim::client;
use prim::rpc::Incoming;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

// Budgets. PLACEHOLDERS: the human sets the real values. Exceeding any fails the test.
const BUDGET_CORE_RSS_PEAK_MB: f64 = 512.0;
const BUDGET_CORE_RSS_GROWTH_MB: f64 = 128.0; // peak minus RSS right after setup
const BUDGET_SPAWN_P99_MS: f64 = 50.0;
const BUDGET_TXN_P50_MS: f64 = 10.0;
const BUDGET_TXN_P99_MS: f64 = 50.0;
const BUDGET_ECHO_P99_MS: f64 = 50.0; // input -> echo on an idle stream during the output flood
const BUDGET_SLOW_ECHO_P99_MS: f64 = 250.0; // the same, on a slow-reading client's own idle stream
const BUDGET_EVENT_LAG_P99_MS: f64 = 250.0; // status change emitted by the backend -> seen by a client
const BUDGET_MISSED: u64 = 0; // `missed` notifications to the draining observer
const BUDGET_STATUS_DELIVERED_MIN: f64 = 0.95; // share of emitted status changes the observer saw
const BUDGET_EXT_RESTART_FAILURES: u64 = 0;
const BUDGET_FINAL_SNAPSHOT_MS: f64 = 1000.0; // core still answers promptly after the load

/// Load profile, from `PRIM_SCALE_*` environment variables (defaults: the brief's target floor).
#[derive(Debug, Clone)]
struct Profile {
    terminals: usize,
    agents: usize,
    floods: usize,
    flood_bytes_per_sec: u64,
    status_every_ms: u64,
    txn_per_sec: f64,
    stalled_clients: usize,
    slow_clients: usize,
    slow_read_bytes_per_sec: u64,
    slow_echo_timeout_s: f64,
    slow_attach_floods: bool,
    ext_restart_every_s: f64,
    adapters: usize,
    duration_s: f64,
    echo_every_ms: u64,
}

fn env_num<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(format!("PRIM_SCALE_{name}")).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl Profile {
    fn from_env() -> Profile {
        let p = Profile {
            terminals: env_num("TERMINALS", 600),
            agents: env_num("AGENTS", 200),
            floods: env_num("FLOODS", 8),
            flood_bytes_per_sec: env_num("FLOOD_BYTES_PER_SEC", 1_000_000),
            status_every_ms: env_num("STATUS_EVERY_MS", 2000),
            txn_per_sec: env_num("TXN_PER_SEC", 20.0),
            stalled_clients: env_num("STALLED_CLIENTS", 1),
            slow_clients: env_num("SLOW_CLIENTS", 1),
            slow_read_bytes_per_sec: env_num("SLOW_READ_BYTES_PER_SEC", 65_536),
            slow_echo_timeout_s: env_num("SLOW_ECHO_TIMEOUT_S", 10.0),
            slow_attach_floods: env_num("SLOW_ATTACH_FLOODS", 1u8) != 0,
            ext_restart_every_s: env_num("EXT_RESTART_EVERY_S", 5.0),
            adapters: env_num("ADAPTERS", 2),
            duration_s: env_num("DURATION_S", 30.0),
            echo_every_ms: env_num("ECHO_EVERY_MS", 200),
        };
        assert!(p.agents + p.floods < p.terminals, "need at least one idle terminal for the echo probe");
        assert!(p.adapters >= 1);
        p
    }
    fn to_json(&self) -> Value {
        json!({
            "terminals": self.terminals, "agents": self.agents, "floods": self.floods,
            "flood_bytes_per_sec": self.flood_bytes_per_sec, "status_every_ms": self.status_every_ms,
            "txn_per_sec": self.txn_per_sec, "clients": 2 + self.stalled_clients + self.slow_clients, "stalled_clients": self.stalled_clients,
            "slow_clients": self.slow_clients, "slow_read_bytes_per_sec": self.slow_read_bytes_per_sec, "slow_echo_timeout_s": self.slow_echo_timeout_s,
            "slow_attach_floods": self.slow_attach_floods,
            "ext_restart_every_s": self.ext_restart_every_s, "adapters": self.adapters,
            "duration_s": self.duration_s, "echo_every_ms": self.echo_every_ms,
        })
    }
}

fn adapter_name(i: usize) -> String {
    if i == 0 { "fake".into() } else { format!("fake{}", i + 1) }
}

/// The core process. Does nothing unless started by `scale_load`.
#[test]
#[ignore]
fn scale_core_child() {
    let Ok(socket) = std::env::var("PRIM_SCALE_CHILD_SOCKET") else { return };
    let state = PathBuf::from(std::env::var("PRIM_SCALE_CHILD_STATE").unwrap());
    let adapters: usize = std::env::var("PRIM_SCALE_CHILD_ADAPTERS").unwrap().parse().unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async move {
        let core = prim::server::start(socket.into(), state, false, prim::server::Backend::Fake, None).unwrap();
        // More fake instances, as more hosts. `server::start` only starts one backend; the core
        // already supports several adapters, so add the rest here.
        for i in 1..adapters {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            prim::core::add_adapter(&core, prim::adapter::fake::start(adapter_name(i), tx), rx, false);
        }
        // Stop like `prim serve` does: on SIGTERM, stop the extensions, then exit. Dropping the
        // runtime drops their child handles, which kill anything still running.
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        term.recv().await;
        prim::ext::stop_all(&core);
        tokio::time::sleep(Duration::from_millis(200)).await;
    });
}

/// The core child process. Stopped as `prim stop` does (SIGTERM, so the core stops its own
/// extensions); afterwards any process still carrying this run's `PRIM_SOCKET` has leaked: it is
/// killed and reported, and the test fails. Dropping (e.g. on a panic) does the same.
struct CoreProc {
    child: Option<std::process::Child>,
    socket: PathBuf,
}

#[derive(Default)]
struct Cleanup {
    core_exit: String,
    /// (pid, command line) of processes from this run still alive after the core stopped.
    leaked: Vec<(u32, String)>,
}

fn signal(pid: u32, sig: &str) {
    let _ = std::process::Command::new("kill").arg(format!("-{sig}")).arg(pid.to_string()).stderr(std::process::Stdio::null()).status();
}

/// Live processes whose environment has exactly `PRIM_SOCKET=<socket>`: this run's extensions.
fn run_processes(socket: &std::path::Path) -> Vec<(u32, String)> {
    let needle = format!("PRIM_SOCKET={}", socket.display());
    let me = std::process::id();
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        if pid == me {
            continue;
        }
        let Ok(env) = std::fs::read(entry.path().join("environ")) else { continue };
        if env.split(|b| *b == 0).any(|var| var == needle.as_bytes()) {
            let cmd = std::fs::read(entry.path().join("cmdline")).unwrap_or_default();
            let cmd = String::from_utf8_lossy(&cmd).replace('\0', " ").trim().to_string();
            found.push((pid, cmd));
        }
    }
    found
}

impl CoreProc {
    fn pid(&self) -> u32 {
        self.child.as_ref().map(|c| c.id()).unwrap_or(0)
    }

    fn shutdown(&mut self) -> Cleanup {
        let Some(mut child) = self.child.take() else { return Cleanup::default() };
        signal(child.id(), "TERM");
        let t0 = Instant::now();
        let mut exit = None;
        while exit.is_none() && t0.elapsed() < Duration::from_secs(10) {
            exit = child.try_wait().ok().flatten();
            std::thread::sleep(Duration::from_millis(20));
        }
        let core_exit = match exit {
            Some(s) => format!("exited after SIGTERM in {:.0} ms: {s}", ms(t0.elapsed())),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                "killed: no exit within 10 s of SIGTERM".into()
            }
        };
        // Killed extensions exit asynchronously; give them a moment before calling them leaked.
        let t1 = Instant::now();
        let mut leaked = run_processes(&self.socket);
        while !leaked.is_empty() && t1.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(50));
            leaked = run_processes(&self.socket);
        }
        for (pid, _) in &leaked {
            signal(*pid, "KILL");
        }
        Cleanup { core_exit, leaked }
    }
}

impl Drop for CoreProc {
    fn drop(&mut self) {
        let c = self.shutdown();
        if !c.leaked.is_empty() {
            eprintln!("scale: killed leaked processes after an aborted run: {:?}", c.leaked);
        }
    }
}

fn rss_mb(pid: u32) -> Option<f64> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let kb: f64 = s.lines().find(|l| l.starts_with("VmRSS:"))?.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024.0)
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn pct(v: &[f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[((s.len() - 1) as f64 * q).round() as usize]
}

fn stats(v: &[f64]) -> Value {
    json!({"n": v.len(), "p50_ms": pct(v, 0.5), "p99_ms": pct(v, 0.99), "max_ms": pct(v, 1.0)})
}

/// What the observer saw.
#[derive(Default)]
struct Observed {
    event_lag_ms: Vec<f64>,
    status_events: u64,
    events: u64,
    missed: u64,
    flood_bytes: u64,
    idle_buf: String,
    /// The echo probe waiting for its token.
    probe: Option<(String, Instant, oneshot::Sender<Duration>)>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn scale_load() {
    let prof = Profile::from_env();
    eprintln!("scale profile: {prof:?}");
    // One run directory for socket and state, on a real disk so durable writes cost what they do:
    // $PRIM_SCALE_RUN_DIR, default ~/.local/share/herdr/scratch/prim-scale. Removed after the run.
    let base = std::env::var("PRIM_SCALE_RUN_DIR").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap()).join(".local/share/herdr/scratch/prim-scale"));
    std::fs::create_dir_all(&base).unwrap();
    let run_dir = tempfile::Builder::new().prefix("run").tempdir_in(&base).unwrap();
    let socket = run_dir.path().join("s.sock");
    assert!(socket.as_os_str().len() < 100, "socket path too long for a Unix socket: {}", socket.display());
    let state = run_dir.path().join("state");

    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "scale_core_child", "--nocapture", "--test-threads=1"])
        .env("PRIM_SCALE_CHILD_SOCKET", &socket)
        .env("PRIM_SCALE_CHILD_STATE", &state)
        .env("PRIM_SCALE_CHILD_ADAPTERS", prof.adapters.to_string())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut core = CoreProc { child: Some(child), socket: socket.clone() };
    let pid = core.pid();
    for _ in 0..200 {
        if tokio::net::UnixStream::connect(&socket).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let rss_start = rss_mb(pid).unwrap();

    let (writer, _wrx, _) = client::connect(&socket, "scale-writer").await.unwrap();
    let (observer, mut orx, _) = client::connect(&socket, "scale-observer").await.unwrap();

    // A trivial extension to restart under load (process supervision churn, no protocol traffic).
    writer.call("workspace.apply", json!({"ops": [{"op": "set", "key": "extension/scale-noop", "value": {"argv": ["sleep", "3600"]}}]})).await.unwrap();

    // Resources: agents first, then floods, then idle terminals; round-robin over adapters.
    let setup_t0 = Instant::now();
    let mut spawn_ms = Vec::new();
    let (mut floods, mut idle) = (Vec::new(), Vec::new());
    for i in 0..prof.terminals {
        let argv: Vec<String> = if i < prof.agents {
            vec!["agent".into(), "--status-every-ms".into(), prof.status_every_ms.to_string()]
        } else if i < prof.agents + prof.floods {
            vec!["flood".into(), "--output-bytes-per-sec".into(), prof.flood_bytes_per_sec.to_string()]
        } else {
            vec!["sh".into()]
        };
        let t = Instant::now();
        let r = writer.call("resource.spawn", json!({"argv": argv, "adapter": adapter_name(i % prof.adapters), "name": format!("t{i}")})).await.unwrap();
        spawn_ms.push(ms(t.elapsed()));
        let id = r["resource"]["id"].as_str().unwrap().to_string();
        if (prof.agents..prof.agents + prof.floods).contains(&i) {
            floods.push(id);
        } else if i >= prof.agents + prof.floods {
            idle.push(id);
        }
    }
    let setup_s = setup_t0.elapsed().as_secs_f64();
    let rss_after_setup = rss_mb(pid).unwrap();

    // Observer: events plus every flood stream plus one idle stream for the echo probe.
    let obs = Arc::new(Mutex::new(Observed::default()));
    let mut flood_sids = Vec::new();
    for f in &floods {
        let a = observer.call("resource.attach", json!({"resource": f})).await.unwrap();
        flood_sids.push(a["stream"].as_u64().unwrap());
    }
    let idle_sid = observer.call("resource.attach", json!({"resource": idle[0]})).await.unwrap()["stream"].as_u64().unwrap();
    observer.call("events.subscribe", json!({})).await.unwrap();
    // Delivery ratios compare what the observer saw against what was offered since here.
    let t_observe = Instant::now();
    let o = obs.clone();
    let pump = tokio::spawn(async move {
        let b64 = base64::engine::general_purpose::STANDARD;
        while let Some(msg) = orx.recv().await {
            let Incoming::Notification { method, params } = msg else { continue };
            let mut o = o.lock().unwrap();
            match method.as_str() {
                "event" => {
                    o.events += 1;
                    if params["type"] == "resource" {
                        if let Some(at) = params["data"]["status"]["at_ms"].as_u64() {
                            o.status_events += 1;
                            o.event_lag_ms.push(unix_ms().saturating_sub(at) as f64);
                        }
                    }
                }
                "missed" => o.missed += 1,
                "stream.data" => {
                    let data = params["data"].as_str().unwrap_or("");
                    if params["stream"].as_u64() == Some(idle_sid) {
                        let bytes = b64.decode(data).unwrap_or_default();
                        o.idle_buf.push_str(&String::from_utf8_lossy(&bytes));
                        let hit = o.probe.as_ref().is_some_and(|(tok, _, _)| o.idle_buf.contains(tok.as_str()));
                        if hit {
                            let (_, t0, done) = o.probe.take().unwrap();
                            o.idle_buf.clear();
                            let _ = done.send(t0.elapsed());
                        }
                    } else {
                        o.flood_bytes += (data.len() / 4 * 3) as u64;
                    }
                }
                _ => {}
            }
        }
    });

    // Stalled clients: subscribe and attach to every flood, then never read.
    let mut stalled = Vec::new();
    for _ in 0..prof.stalled_clients {
        let mut s = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let mut lines = vec![
            json!({"jsonrpc": "2.0", "id": 1, "method": "hello", "params": {"client": "scale-stalled"}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "events.subscribe", "params": {}}),
        ];
        for (k, f) in floods.iter().chain(idle.iter().take(1)).enumerate() {
            lines.push(json!({"jsonrpc": "2.0", "id": 10 + k, "method": "resource.attach", "params": {"resource": f}}));
        }
        for l in lines {
            s.write_all(format!("{l}\n").as_bytes()).await.unwrap();
        }
        stalled.push(s);
    }

    // Load phase.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let t_load = Instant::now();
    let deadline = t_load + Duration::from_secs_f64(prof.duration_s);

    // Slow clients: like the stalled ones, but they read at a fixed low rate and probe echo on
    // their own idle stream.
    let slow_tasks: Vec<_> = (0..prof.slow_clients)
        .map(|_| {
            let floods = if prof.slow_attach_floods { floods.as_slice() } else { &[] };
            let attach: Vec<String> = floods.iter().chain(idle.iter().take(1)).cloned().collect();
            slow_client(socket.clone(), attach, prof.clone(), stop.clone(), deadline)
        })
        .collect();

    let rss_abort_mb: f64 = env_num("RSS_ABORT_MB", 4096.0);
    let rss_task = {
        let stop = stop.clone();
        tokio::spawn(async move {
            let mut series = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                if let Some(r) = rss_mb(pid) {
                    series.push((t_load.elapsed().as_secs_f64(), r));
                    if r > rss_abort_mb {
                        // Safety stop for the machine running the test; the receipt shows it.
                        stop.store(true, Ordering::Relaxed);
                    }
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            series
        })
    };

    let txn_task = {
        let (w, stop, rate) = (writer.clone(), stop.clone(), prof.txn_per_sec);
        tokio::spawn(async move {
            let (mut lat, mut errors) = (Vec::new(), 0u64);
            if rate <= 0.0 {
                return (lat, errors);
            }
            let mut every = tokio::time::interval(Duration::from_secs_f64(1.0 / rate));
            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut k = 0u64;
            while !stop.load(Ordering::Relaxed) {
                every.tick().await;
                k += 1;
                let ops = json!([{"op": "set", "key": format!("doc/scale/{}", k % 50), "value": {"k": k, "pad": "x".repeat(200)}}]);
                let t = Instant::now();
                match w.call("workspace.apply", json!({"ops": ops, "note": "scale"})).await {
                    Ok(_) => lat.push(ms(t.elapsed())),
                    Err(_) => errors += 1,
                }
            }
            (lat, errors)
        })
    };

    let restart_task = {
        let (w, stop, every_s) = (writer.clone(), stop.clone(), prof.ext_restart_every_s);
        tokio::spawn(async move {
            let (mut ok, mut failed, mut lat) = (0u64, 0u64, Vec::new());
            if every_s <= 0.0 {
                return (ok, failed, lat);
            }
            let mut every = tokio::time::interval(Duration::from_secs_f64(every_s));
            every.tick().await;
            while !stop.load(Ordering::Relaxed) {
                every.tick().await;
                let t = Instant::now();
                match w.call("command.invoke", json!({"name": "prim.extension.restart", "args": {"name": "scale-noop"}})).await {
                    Ok(_) => {
                        ok += 1;
                        lat.push(ms(t.elapsed()));
                    }
                    Err(_) => failed += 1,
                }
            }
            (ok, failed, lat)
        })
    };

    // Echo probe on the observer's idle stream, sharing its connection with the floods.
    let mut echo_ms = Vec::new();
    let mut echo_lost = 0u64;
    let probe_seq = AtomicU64::new(0);
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        let token = format!("<p{}>", probe_seq.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        obs.lock().unwrap().probe = Some((token.clone(), Instant::now(), tx));
        observer.notify("stream.input", json!({"stream": idle_sid, "text": token}));
        match tokio::time::timeout(Duration::from_secs(5), rx).await {
            Ok(Ok(d)) => echo_ms.push(ms(d)),
            _ => {
                echo_lost += 1;
                echo_ms.push(5000.0);
                obs.lock().unwrap().probe = None;
            }
        }
        tokio::time::sleep(Duration::from_millis(prof.echo_every_ms)).await;
    }
    let load_s = t_load.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    let (txn_ms, txn_errors) = txn_task.await.unwrap();
    let (restarts_ok, restarts_failed, restart_ms) = restart_task.await.unwrap();
    let rss_series = rss_task.await.unwrap();

    let t = Instant::now();
    let snap = writer.call("workspace.snapshot", Value::Null).await;
    let final_snapshot_ms = ms(t.elapsed());
    let resources_seen = snap.as_ref().map(|s| s["resources"].as_array().map_or(0, |a| a.len())).unwrap_or(0);
    let rss_end = rss_mb(pid).unwrap_or(f64::NAN);
    drop(stalled);
    let mut slow = Vec::new();
    for t in slow_tasks {
        slow.push(t.await.unwrap());
    }
    let slow_echo: Vec<f64> = slow.iter().flat_map(|s| s.echo_ms.iter().copied()).collect();
    let cleanup = core.shutdown();
    pump.abort();
    let observed_s = t_observe.elapsed().as_secs_f64();

    let o = obs.lock().unwrap();
    let rss_peak = rss_series.iter().map(|(_, r)| *r).fold(rss_after_setup, f64::max);
    let expected_status = (prof.agents as f64 * observed_s * 1000.0 / prof.status_every_ms as f64).floor();
    let delivered = if expected_status > 0.0 { o.status_events as f64 / expected_status } else { 1.0 };
    let flood_expected = prof.floods as f64 * prof.flood_bytes_per_sec as f64 * observed_s;
    // Every ~2 s, to keep the receipt readable.
    let rss_samples: Vec<Value> = rss_series.iter().step_by(8).map(|(t, r)| json!([(*t * 10.0).round() / 10.0, (*r * 10.0).round() / 10.0])).collect();

    let measured: BTreeMap<&str, (f64, f64, bool)> = BTreeMap::from([
        ("core_rss_peak_mb", (rss_peak, BUDGET_CORE_RSS_PEAK_MB, rss_peak <= BUDGET_CORE_RSS_PEAK_MB)),
        ("core_rss_growth_mb", (rss_peak - rss_after_setup, BUDGET_CORE_RSS_GROWTH_MB, rss_peak - rss_after_setup <= BUDGET_CORE_RSS_GROWTH_MB)),
        ("spawn_p99_ms", (pct(&spawn_ms, 0.99), BUDGET_SPAWN_P99_MS, pct(&spawn_ms, 0.99) <= BUDGET_SPAWN_P99_MS)),
        ("txn_p50_ms", (pct(&txn_ms, 0.5), BUDGET_TXN_P50_MS, pct(&txn_ms, 0.5) <= BUDGET_TXN_P50_MS)),
        ("txn_p99_ms", (pct(&txn_ms, 0.99), BUDGET_TXN_P99_MS, pct(&txn_ms, 0.99) <= BUDGET_TXN_P99_MS)),
        ("echo_p99_ms", (pct(&echo_ms, 0.99), BUDGET_ECHO_P99_MS, pct(&echo_ms, 0.99) <= BUDGET_ECHO_P99_MS)),
        // NaN (no slow clients) passes.
        ("slow_echo_p99_ms", (pct(&slow_echo, 0.99), BUDGET_SLOW_ECHO_P99_MS, !(pct(&slow_echo, 0.99) > BUDGET_SLOW_ECHO_P99_MS))),
        ("event_lag_p99_ms", (pct(&o.event_lag_ms, 0.99), BUDGET_EVENT_LAG_P99_MS, pct(&o.event_lag_ms, 0.99) <= BUDGET_EVENT_LAG_P99_MS)),
        ("missed", (o.missed as f64, BUDGET_MISSED as f64, o.missed <= BUDGET_MISSED)),
        ("status_delivered_ratio", (delivered, BUDGET_STATUS_DELIVERED_MIN, delivered >= BUDGET_STATUS_DELIVERED_MIN)),
        ("ext_restart_failures", (restarts_failed as f64, BUDGET_EXT_RESTART_FAILURES as f64, restarts_failed <= BUDGET_EXT_RESTART_FAILURES)),
        ("final_snapshot_ms", (final_snapshot_ms, BUDGET_FINAL_SNAPSHOT_MS, snap.is_ok() && final_snapshot_ms <= BUDGET_FINAL_SNAPSHOT_MS)),
        // Not a placeholder: a run must leave nothing behind.
        ("leaked_processes", (cleanup.leaked.len() as f64, 0.0, cleanup.leaked.is_empty())),
    ]);
    let failed: Vec<&str> = measured.iter().filter(|(_, (_, _, ok))| !ok).map(|(k, _)| *k).collect();
    let commit = std::process::Command::new("git").args(["describe", "--always", "--dirty"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    let receipt = json!({
        "kind": "prim.scale-receipt", "version": 1,
        "commit": commit, "at_unix_ms": unix_ms(), "release_build": !cfg!(debug_assertions),
        "profile": prof.to_json(),
        "budgets_are_placeholders": true,
        "result": if failed.is_empty() { "pass" } else { "fail" },
        "exceeded": failed,
        "budgets": measured.iter().map(|(k, (v, b, ok))| (k.to_string(), json!({"measured": v, "budget": b, "ok": ok}))).collect::<serde_json::Map<_, _>>(),
        "setup": {"seconds": setup_s, "spawn": stats(&spawn_ms), "resources_in_final_snapshot": resources_seen},
        "load_seconds": load_s,
        "stopped_early_rss_over_mb": if load_s + 0.5 < prof.duration_s { json!(rss_abort_mb) } else { Value::Null },
        "core_rss_mb": {"start": rss_start, "after_setup": rss_after_setup, "peak": rss_peak, "end": rss_end, "samples_t_s_mb": rss_samples},
        "transactions": {"latency": stats(&txn_ms), "errors": txn_errors, "achieved_per_sec": txn_ms.len() as f64 / load_s},
        "echo": {"latency": stats(&echo_ms), "lost_after_5s": echo_lost},
        "slow_clients": slow.iter().map(|s| json!({
            "echo": stats(&s.echo_ms), "echo_timed_out": s.timed_out, "bytes_read": s.bytes_read,
            "idle_stream_attached": s.attached, "missed": s.missed,
        })).collect::<Vec<_>>(),
        "events": {"received": o.events, "status_received": o.status_events, "status_expected_approx": expected_status, "lag": stats(&o.event_lag_ms), "missed": o.missed},
        "flood": {"observer_bytes": o.flood_bytes, "offered_bytes": flood_expected, "observer_share": o.flood_bytes as f64 / flood_expected.max(1.0)},
        "extension_restarts": {"ok": restarts_ok, "failed": restarts_failed, "latency": stats(&restart_ms)},
        "final_snapshot": {"ok": snap.is_ok(), "ms": final_snapshot_ms},
        "cleanup": {"core_exit": cleanup.core_exit, "leaked_and_killed": cleanup.leaked.iter().map(|(p, c)| json!({"pid": p, "cmd": c})).collect::<Vec<_>>()},
    });
    let text = serde_json::to_string_pretty(&receipt).unwrap();
    let path = std::env::var("PRIM_SCALE_RECEIPT").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("scale-receipt.json"));
    std::fs::write(&path, &text).unwrap();
    println!("{text}\nreceipt: {}", path.display());
    assert!(failed.is_empty(), "scale budgets exceeded: {failed:?} (receipt: {})", path.display());
}

/// What one slow client saw.
#[derive(Default)]
struct Slow {
    echo_ms: Vec<f64>,
    timed_out: u64,
    bytes_read: u64,
    missed: u64,
    attached: bool,
}

/// A client that subscribes and attaches like the stalled ones, but reads at most
/// `slow_read_bytes_per_sec`, and probes input -> echo on its own idle stream (the last attach).
/// A probe that times out counts as the timeout.
fn slow_client(socket: PathBuf, attach: Vec<String>, prof: Profile, stop: Arc<std::sync::atomic::AtomicBool>, deadline: Instant) -> tokio::task::JoinHandle<Slow> {
    use tokio::io::AsyncReadExt;
    tokio::spawn(async move {
        let s = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (mut r, mut w) = s.into_split();
        let mut lines = vec![
            json!({"jsonrpc": "2.0", "id": 1, "method": "hello", "params": {"client": "scale-slow"}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "events.subscribe", "params": {}}),
        ];
        for (k, res) in attach.iter().enumerate() {
            lines.push(json!({"jsonrpc": "2.0", "id": 10 + k, "method": "resource.attach", "params": {"resource": res}}));
        }
        for l in lines {
            w.write_all(format!("{l}\n").as_bytes()).await.unwrap();
        }
        let idle_id = 10 + attach.len() as u64 - 1;

        // Reader: a token bucket of `slow_read_bytes_per_sec`, in 10 ms ticks.
        #[derive(Default)]
        struct Seen {
            sid: Option<u64>,
            idle_text: String,
            bytes: u64,
            missed: u64,
        }
        let seen = Arc::new(Mutex::new(Seen::default()));
        let per_tick = (prof.slow_read_bytes_per_sec / 100).max(1) as usize;
        let reader = {
            let (seen, stop) = (seen.clone(), stop.clone());
            tokio::spawn(async move {
                let b64 = base64::engine::general_purpose::STANDARD;
                let mut buf = vec![0u8; per_tick];
                let mut pending: Vec<u8> = Vec::new();
                let mut every = tokio::time::interval(Duration::from_millis(10));
                while !stop.load(Ordering::Relaxed) {
                    every.tick().await;
                    let Ok(Ok(n)) = tokio::time::timeout(Duration::from_millis(5), r.read(&mut buf)).await else { continue };
                    if n == 0 {
                        break;
                    }
                    pending.extend_from_slice(&buf[..n]);
                    let mut s = seen.lock().unwrap();
                    s.bytes += n as u64;
                    while let Some(nl) = pending.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = pending.drain(..=nl).collect();
                        let Ok(msg) = serde_json::from_slice::<Value>(&line) else { continue };
                        if msg["id"].as_u64() == Some(idle_id) {
                            s.sid = msg["result"]["stream"].as_u64();
                        } else if msg["method"] == "missed" {
                            s.missed += 1;
                        } else if msg["method"] == "stream.data" && s.sid.is_some() && msg["params"]["stream"].as_u64() == s.sid {
                            let bytes = b64.decode(msg["params"]["data"].as_str().unwrap_or("")).unwrap_or_default();
                            s.idle_text.push_str(&String::from_utf8_lossy(&bytes));
                        }
                    }
                }
            })
        };

        // Prober: one token at a time on the idle stream, once its attach reply has been read.
        let mut out = Slow::default();
        let timeout = Duration::from_secs_f64(prof.slow_echo_timeout_s);
        let mut k = 0u64;
        while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
            let sid = seen.lock().unwrap().sid;
            let Some(sid) = sid else {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            };
            out.attached = true;
            k += 1;
            let token = format!("<s{k}>");
            let note = json!({"jsonrpc": "2.0", "method": "stream.input", "params": {"stream": sid, "text": token}});
            w.write_all(format!("{note}\n").as_bytes()).await.unwrap();
            let t0 = Instant::now();
            loop {
                let hit = {
                    let mut s = seen.lock().unwrap();
                    let hit = s.idle_text.contains(&token);
                    if hit {
                        s.idle_text.clear();
                    }
                    hit
                };
                if hit {
                    out.echo_ms.push(ms(t0.elapsed()));
                    break;
                }
                if t0.elapsed() >= timeout {
                    out.timed_out += 1;
                    out.echo_ms.push(ms(timeout));
                    break;
                }
                if stop.load(Ordering::Relaxed) {
                    break; // load over before this probe could finish: not counted
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            tokio::time::sleep(Duration::from_millis(prof.echo_every_ms)).await;
        }
        // Wait for the load to end (the reader stops then), so this client stays connected throughout.
        let _ = reader.await;
        let s = seen.lock().unwrap();
        out.bytes_read = s.bytes;
        out.missed = s.missed;
        out
    })
}
