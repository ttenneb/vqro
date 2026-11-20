//! A fake agent runtime serving per-viewer credited **item streams** (agent-v0 §4), for the item-stream gate
//! (`tests/e2e_item_streams.py`). Evidence, not product.
//!
//! One provider hosts N fake agents. Each agent is an [`ItemJournal`] (the shared SDK producer): messages are
//! `put` (with multibyte text and `native.ts`, the producer's clock in ms), then `append`ed to `/content/0/text`,
//! and `remove`d when more than 24 exist. Each open item stream has its own [`ItemCursor`] and task: the generator
//! never waits for a viewer; a viewer that falls out of retention gets the gap reason `retention`.
//! `agent.session.switch` starts a new epoch (open streams get an in-stream snapshot); a restart of this process
//! starts new epochs too (same instance, so resource identities carry over, but the journals are gone). Writes are
//! split at odd byte offsets (mid-UTF-8, mid-line) on purpose.
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Notify;
use vqro::provider_client::items::{ItemCursor, ItemJournal, JournalConfig, Next};
use vqro::provider_client::{Config, Outcome, Provider, ProviderError, ProviderStream, Request, StreamCommand};

const TEXT: &[&str] = &["héllo ", "wörld ", "naïve ", "日本語 ", "漢字テスト ", "🙂 ", "🚀✨ ", "ok ", "Ω≈ç√ ", "tool output\t"];

fn now_ms() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64 }

fn arg(name: &str, default: &str) -> String {
    let a: Vec<String> = std::env::args().collect();
    a.iter().position(|x| x == name).and_then(|i| a.get(i + 1).cloned()).unwrap_or_else(|| default.into())
}

struct Agent {
    journal: ItemJournal,
    next_id: u64,
    position: u64,
    current: Option<String>,
    open: std::collections::VecDeque<String>,
    session: u64,
}

impl Agent {
    fn chunk(k: u64, bytes: usize) -> String {
        let (mut s, mut i) = (String::new(), k);
        while s.len() < bytes {
            s.push_str(TEXT[(i % TEXT.len() as u64) as usize]);
            i = i.wrapping_mul(2654435761).wrapping_add(7);
        }
        s
    }
    fn new_message(&mut self, text: String) {
        self.next_id += 1;
        self.position += 1;
        let id = format!("m{}", self.next_id);
        let value = json!({"position": self.position, "role": "assistant", "content": [{"type": "text", "text": text}], "final": false, "native": {"ts": now_ms()}});
        self.journal.put("message", &id, value).expect("put");
        self.open.push_back(id.clone());
        self.current = Some(id);
    }
    /// One generator step: a removal, a new message, or an append to the current one.
    fn step(&mut self, n: u64, text_bytes: usize) {
        if self.open.len() > 24 {
            let id = self.open.pop_front().unwrap();
            self.journal.remove("message", &id).expect("remove");
            if self.current.as_deref() == Some(&id) { self.current = None; }
        } else if self.current.is_none() || n % 8 == 0 {
            self.new_message(Self::chunk(n, text_bytes));
        } else {
            let id = self.current.clone().unwrap();
            let long = self.journal.entity("message", &id).and_then(|(_, v)| v.pointer("/content/0/text")).and_then(Value::as_str).map(|t| t.len() > 4096).unwrap_or(false);
            if long { self.current = None; self.new_message(Self::chunk(n, text_bytes)); }
            else { self.journal.append_text("message", &id, "/content/0/text", &Self::chunk(n, text_bytes)).expect("append"); }
        }
    }
}

struct Shared {
    agents: Vec<(Mutex<Agent>, Notify)>,
    paused: AtomicBool,
    boot: String,
    stats: Stats,
}

#[derive(Default)]
struct Stats { records: AtomicU64, opens_continue: AtomicU64, opens_reset: AtomicU64, gaps: AtomicU64, epoch_snapshots: AtomicU64, streams: AtomicU64, sent_bytes: AtomicU64 }

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    if std::env::var("VQRO_EXTENSION_LIFETIME").as_deref() == Ok("stdin") {
        std::thread::spawn(|| {
            use std::io::Read;
            let mut b = [0u8; 4096];
            while matches!(std::io::stdin().read(&mut b), Ok(n) if n > 0) {}
            std::process::exit(0);
        });
    }
    let n: usize = arg("--agents", "200").parse().unwrap();
    let rate: f64 = arg("--rate", "2").parse().unwrap();
    let text_bytes: usize = arg("--text", "160").parse().unwrap();
    let retain: usize = arg("--retain", "131072").parse().unwrap();
    let name = arg("--name", "items");
    // A restart keeps the instance (resource identities carry over) but starts new epochs: the journals are gone.
    let instance = arg("--instance", "fake-items");
    let boot = format!("b{}", now_ms());
    let cfg = JournalConfig { retain_bytes: retain, ..Default::default() };
    let shared = Arc::new(Shared {
        agents: (0..n).map(|i| (Mutex::new(Agent { journal: ItemJournal::new(format!("{boot}.{i}.0"), cfg.clone()), next_id: 0, position: 0, current: None, open: Default::default(), session: 0 }), Notify::new())).collect(),
        paused: AtomicBool::new(false), boot, stats: Stats::default(),
    });
    let mut caps = Map::new();
    caps.insert("items.stream".into(), json!({"retain_bytes": retain}));
    for op in ["agent.session.switch", "items.pause", "items.digest"] { caps.insert(op.into(), json!({})); }
    let mut config = Config::new(name, instance.clone(), caps);
    config.max_item_streams = arg("--max-item-streams", "1024").parse().unwrap();
    let endpoint = std::env::var("VQRO_SOCKET").expect("VQRO_SOCKET");
    let sock = tokio::net::UnixStream::connect(&endpoint).await.expect("core");
    let (provider, mut requests) = Provider::connect(sock, config).await.expect("register");
    let facts: Vec<Value> = (0..n).map(|i| json!({"ref": format!("a{i}"), "incarnation": instance, "kind": "agent", "state": "live",
        "name": format!("fake-agent-{i}"), "data": {"items": {"retain_bytes": retain}}})).collect();
    provider.full_sync(facts).await.expect("sync");

    // Generator: every agent advances `rate` records per second, never waiting for viewers.
    let g = shared.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs_f64(1.0 / rate));
        let mut n = 0u64;
        loop {
            tick.tick().await;
            if g.paused.load(Ordering::Relaxed) { continue; }
            n += 1;
            for (i, (a, notify)) in g.agents.iter().enumerate() {
                a.lock().unwrap().step(n.wrapping_mul(31).wrapping_add(i as u64), text_bytes);
                notify.notify_waiters();
            }
            g.stats.records.fetch_add(g.agents.len() as u64, Ordering::Relaxed);
        }
    });
    let s = shared.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            let (log, ents) = s.agents.iter().fold((0, 0), |(l, e), (a, _)| { let a = a.lock().unwrap(); (l + a.journal.retained_bytes(), e + a.journal.entity_bytes()) });
            let st = &s.stats;
            eprintln!("STATS {}", json!({"t": now_ms(), "records": st.records.load(Ordering::Relaxed), "streams_open": st.streams.load(Ordering::Relaxed),
                "opens_continue": st.opens_continue.load(Ordering::Relaxed), "opens_reset": st.opens_reset.load(Ordering::Relaxed),
                "gaps": st.gaps.load(Ordering::Relaxed), "epoch_snapshots": st.epoch_snapshots.load(Ordering::Relaxed),
                "sent_bytes": st.sent_bytes.load(Ordering::Relaxed), "retained_log_bytes": log, "entity_bytes": ents}));
        }
    });

    while let Some(req) = requests.recv().await {
        match req {
            Request::Effect(e) => {
                let idx = e.r#ref.as_deref().and_then(|r| r.strip_prefix('a')).and_then(|x| x.parse::<usize>().ok()).filter(|i| *i < shared.agents.len());
                let result = match (e.op.as_str(), idx) {
                    ("agent.session.switch", Some(i)) => {
                        let (a, notify) = &shared.agents[i];
                        let mut a = a.lock().unwrap();
                        a.session += 1;
                        let epoch = format!("{}.{i}.{}", shared.boot, a.session);
                        a.journal.new_epoch(epoch.clone());
                        a.current = None; a.open.clear();
                        drop(a);
                        notify.notify_waiters();
                        Ok(Outcome::Observed(Some(json!({"epoch": epoch}))))
                    }
                    ("items.pause", _) => { shared.paused.store(e.args["paused"].as_bool().unwrap_or(true), Ordering::Relaxed); Ok(Outcome::Observed(None)) }
                    ("items.digest", Some(i)) => {
                        let a = shared.agents[i].0.lock().unwrap();
                        Ok(Outcome::Observed(Some(json!({"epoch": a.journal.epoch(), "seq": a.journal.seq(), "digest": a.journal.digest(), "entities": a.journal.len()}))))
                    }
                    _ => Err(ProviderError::new("unsupported", "unknown op or agent")),
                };
                e.respond(result);
            }
            Request::Open(o) => {
                let idx = o.stream.r#ref.strip_prefix('a').and_then(|x| x.parse::<usize>().ok()).filter(|i| *i < shared.agents.len());
                let (Some(i), Some(items)) = (idx, o.items.clone()) else {
                    o.respond(Err(ProviderError::new("unsupported", "item streams only")));
                    continue;
                };
                // One cut under the agent lock: continue from `after` or reset with a snapshot.
                let cursor = shared.agents[i].0.lock().unwrap().journal.open(items.get("after"));
                let stat = if cursor.reset() { &shared.stats.opens_reset } else { &shared.stats.opens_continue };
                stat.fetch_add(1, Ordering::Relaxed);
                let Some(stream) = o.respond_items(cursor.reset(), cursor.epoch()) else { continue };
                tokio::spawn(viewer(shared.clone(), i, stream, cursor));
            }
        }
    }
}

/// Write `bytes` split at odd offsets (mid-UTF-8 and mid-line on purpose).
async fn send_split(stream: &ProviderStream, bytes: &[u8], salt: u64, sent: &AtomicU64) -> Result<(), ProviderError> {
    let (mut at, mut k) = (0, salt);
    while at < bytes.len() {
        k = k.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let n = ((k >> 33) as usize % 997 + 1).min(bytes.len() - at);
        stream.send(&bytes[at..at + n]).await?;
        at += n;
    }
    sent.fetch_add(bytes.len() as u64, Ordering::Relaxed);
    Ok(())
}

async fn viewer(shared: Arc<Shared>, i: usize, mut stream: ProviderStream, mut cursor: ItemCursor) {
    shared.stats.streams.fetch_add(1, Ordering::Relaxed);
    let salt = stream.id;
    let run = async {
        loop {
            let notify = &shared.agents[i].1;
            let wake = notify.notified();
            tokio::pin!(wake);
            wake.as_mut().enable();
            let (next, epoch_changed) = {
                let a = shared.agents[i].0.lock().unwrap();
                let before = cursor.epoch().to_string();
                let next = cursor.next(&a.journal, 65536);
                (next, before != cursor.epoch())
            };
            if epoch_changed { shared.stats.epoch_snapshots.fetch_add(1, Ordering::Relaxed); }
            match next {
                Next::Data(d) => { send_split(&stream, &d, salt.wrapping_add(cursor.seq()), &shared.stats.sent_bytes).await?; continue; }
                Next::Gap(reason) => { shared.stats.gaps.fetch_add(1, Ordering::Relaxed); stream.gap(reason).await?; return Ok(()); }
                Next::Idle => {}
            }
            tokio::select! {
                _ = &mut wake => {}
                cmd = stream.next() => match cmd {
                    Some(StreamCommand::Close) | None => { let _ = stream.close(json!({"reason": "closed"})).await; return Ok(()); }
                    Some(_) => {}
                },
            }
        }
    };
    let _: Result<(), ProviderError> = run.await;
    shared.stats.streams.fetch_sub(1, Ordering::Relaxed);
}
