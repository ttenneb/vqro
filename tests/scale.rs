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
            "txn_per_sec": self.txn_per_sec, "clients": 2 + self.stalled_clients, "stalled_clients": self.stalled_clients,
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
        loop {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    });
}

struct KillOnDrop(std::process::Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
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
    // Socket in /tmp (short path); state on the build disk, so durable writes cost what they do.
    let sock_dir = tempfile::Builder::new().prefix("pscale").tempdir_in("/tmp").unwrap();
    let socket = sock_dir.path().join("s.sock");
    let state_root = tempfile::Builder::new().prefix("scale-state").tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let state = state_root.path().join("state");

    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "scale_core_child", "--nocapture", "--test-threads=1"])
        .env("PRIM_SCALE_CHILD_SOCKET", &socket)
        .env("PRIM_SCALE_CHILD_STATE", &state)
        .env("PRIM_SCALE_CHILD_ADAPTERS", prof.adapters.to_string())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let child = KillOnDrop(child);
    let pid = child.0.id();
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
    drop(child);
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
        ("event_lag_p99_ms", (pct(&o.event_lag_ms, 0.99), BUDGET_EVENT_LAG_P99_MS, pct(&o.event_lag_ms, 0.99) <= BUDGET_EVENT_LAG_P99_MS)),
        ("missed", (o.missed as f64, BUDGET_MISSED as f64, o.missed <= BUDGET_MISSED)),
        ("status_delivered_ratio", (delivered, BUDGET_STATUS_DELIVERED_MIN, delivered >= BUDGET_STATUS_DELIVERED_MIN)),
        ("ext_restart_failures", (restarts_failed as f64, BUDGET_EXT_RESTART_FAILURES as f64, restarts_failed <= BUDGET_EXT_RESTART_FAILURES)),
        ("final_snapshot_ms", (final_snapshot_ms, BUDGET_FINAL_SNAPSHOT_MS, snap.is_ok() && final_snapshot_ms <= BUDGET_FINAL_SNAPSHOT_MS)),
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
        "events": {"received": o.events, "status_received": o.status_events, "status_expected_approx": expected_status, "lag": stats(&o.event_lag_ms), "missed": o.missed},
        "flood": {"observer_bytes": o.flood_bytes, "offered_bytes": flood_expected, "observer_share": o.flood_bytes as f64 / flood_expected.max(1.0)},
        "extension_restarts": {"ok": restarts_ok, "failed": restarts_failed, "latency": stats(&restart_ms)},
        "final_snapshot": {"ok": snap.is_ok(), "ms": final_snapshot_ms},
    });
    let text = serde_json::to_string_pretty(&receipt).unwrap();
    let path = std::env::var("PRIM_SCALE_RECEIPT").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("scale-receipt.json"));
    std::fs::write(&path, &text).unwrap();
    println!("{text}\nreceipt: {}", path.display());
    assert!(failed.is_empty(), "scale budgets exceeded: {failed:?} (receipt: {})", path.display());
}
