//! Prototype P3 (agent standard v0): a fake agent runtime serving per-viewer credited **item streams**.
//!
//! Evidence, not product. One provider hosts N fake agents. Each agent produces JSONL records
//! (`put`/`append`/`remove`, with multibyte text) into a retained log bounded by `--retain` bytes
//! (128 KiB default). A viewer attaches with `resource.attach {items: {after?: {epoch, seq}}}`:
//! - the provider answers `continue` when the cursor's epoch is current and retention still covers
//!   `seq + 1`, then sends every record strictly after it;
//! - otherwise `reset`: a `reset {epoch, cut, coverage}` record, a `put` for every live entity and a
//!   `snapshot_end {cut}`, written into the same credited stream under the agent lock (the stream order
//!   is the fence), then live records strictly after `cut`.
//!
//! The generator never waits for a viewer. Each viewer has its own task and cursor; a viewer that falls
//! out of retention gets an explicit `stream.gap` (`retention_exceeded`) and must reattach. A session
//! switch (`agent.session.switch`) or a restart of this process starts a new epoch; open streams get an
//! in-stream `reset` + snapshot. Every record carries `h`, a hash of the entity value after it applies,
//! so a viewer can detect a stale, duplicated or fabricated reconstruction. Writes are split at odd byte
//! offsets (mid-UTF-8, mid-line) on purpose.
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Notify;
use vqro::provider_client::{Config, Outcome, Provider, ProviderError, ProviderStream, Request, StreamCommand};

const TEXT: &[&str] = &["héllo ", "wörld ", "naïve ", "日本語 ", "漢字テスト ", "🙂 ", "🚀✨ ", "ok ", "Ω≈ç√ ", "tool output\t"];

fn fnv(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

struct Entity {
    rev: u64,
    value: String,
}

struct Agent {
    epoch: String,
    seq: u64,
    log: VecDeque<(u64, Arc<Vec<u8>>)>,
    log_bytes: usize,
    entities: BTreeMap<String, Entity>,
    next_id: u64,
    current: Option<String>,
}

impl Agent {
    fn first_retained(&self) -> u64 {
        self.log.front().map(|(s, _)| *s).unwrap_or(self.seq + 1)
    }
    fn push(&mut self, mut record: Value, retain: usize) {
        self.seq += 1;
        record["seq"] = json!(self.seq);
        record["ts"] = json!(now_ms());
        let mut line = serde_json::to_vec(&record).unwrap();
        line.push(b'\n');
        self.log_bytes += line.len();
        self.log.push_back((self.seq, Arc::new(line)));
        while self.log_bytes > retain {
            let (_, l) = self.log.pop_front().unwrap();
            self.log_bytes -= l.len();
        }
    }
    /// One generator step: a new message, an append to the current one, or a removal.
    fn step(&mut self, n: u64, text_bytes: usize, retain: usize) {
        let chunk = |k: u64| {
            let mut s = String::new();
            let mut i = k;
            while s.len() < text_bytes {
                s.push_str(TEXT[(i % TEXT.len() as u64) as usize]);
                i = i.wrapping_mul(2654435761).wrapping_add(7);
            }
            s
        };
        if self.entities.len() > 24 {
            let id = self.entities.keys().next().unwrap().clone();
            let rev = self.entities.remove(&id).unwrap().rev + 1;
            if self.current.as_deref() == Some(&id) { self.current = None; }
            self.push(json!({"type": "remove", "kind": "message", "id": id, "rev": rev}), retain);
        } else if self.current.is_none() || n % 8 == 0 {
            self.next_id += 1;
            let id = format!("m{}", self.next_id);
            let value = chunk(n);
            let h = fnv(&value);
            self.entities.insert(id.clone(), Entity { rev: 1, value: value.clone() });
            self.current = Some(id.clone());
            self.push(json!({"type": "put", "kind": "message", "id": id, "rev": 1, "value": value, "h": h}), retain);
        } else {
            let id = self.current.clone().unwrap();
            let text = chunk(n);
            let e = self.entities.get_mut(&id).unwrap();
            let base = e.rev;
            e.rev += 1;
            e.value.push_str(&text);
            // Keep entity values bounded; a long message is rewritten as a fresh put.
            if e.value.len() > 4096 {
                e.value = text.clone();
                let (rev, h) = (e.rev, fnv(&e.value));
                self.push(json!({"type": "put", "kind": "message", "id": id, "rev": rev, "value": text, "h": h}), retain);
            } else {
                let (rev, h) = (e.rev, fnv(&e.value));
                self.push(json!({"type": "append", "kind": "message", "id": id, "base": base, "rev": rev, "field": "/value", "text": text, "h": h}), retain);
            }
        }
    }
    /// `reset` + a put per live entity + `snapshot_end`, all at one cut (caller holds the lock).
    fn snapshot(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut line = |v: Value| { out.extend(serde_json::to_vec(&v).unwrap()); out.push(b'\n'); };
        line(json!({"type": "snapshot", "phase": "begin", "epoch": self.epoch,
            "coverage": {"entities": self.entities.len(), "retained_from": self.first_retained()}}));
        for (id, e) in &self.entities {
            line(json!({"type": "put", "kind": "message", "id": id, "rev": e.rev, "value": e.value, "h": fnv(&e.value)}));
        }
        line(json!({"type": "snapshot", "phase": "end", "seq": self.seq}));
        out
    }
    fn digest(&self) -> String {
        let mut all = String::new();
        for (id, e) in &self.entities { all.push_str(&format!("{id}:{}:{};", e.rev, fnv(&e.value))); }
        fnv(&all)
    }
}

struct Shared {
    agents: Vec<(Mutex<Agent>, Notify)>,
    retain: usize,
    paused: AtomicBool,
    instance: String,
    stats: Stats,
}

#[derive(Default)]
struct Stats {
    records: AtomicU64,
    opens_continue: AtomicU64,
    opens_reset: AtomicU64,
    gaps: AtomicU64,
    switch_resets: AtomicU64,
    streams: AtomicU64,
    sent_bytes: AtomicU64,
}

fn arg(name: &str, default: &str) -> String {
    let a: Vec<String> = std::env::args().collect();
    a.iter().position(|x| x == name).and_then(|i| a.get(i + 1).cloned()).unwrap_or_else(|| default.into())
}

fn epoch_for(instance: &str, agent: usize, session: u64) -> String {
    format!("{instance}.{agent}.{session}")
}

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
    // A restart keeps the instance (resource identities carry over) but starts a new epoch: retention is gone.
    let instance = arg("--instance", "fake-items");
    let boot = format!("b{}", now_ms());
    let shared = Arc::new(Shared {
        agents: (0..n).map(|i| (Mutex::new(Agent { epoch: epoch_for(&boot, i, 0), seq: 0, log: VecDeque::new(), log_bytes: 0,
            entities: BTreeMap::new(), next_id: 0, current: None }), Notify::new())).collect(),
        retain, paused: AtomicBool::new(false), instance: boot.clone(), stats: Stats::default(),
    });
    let mut caps = Map::new();
    caps.insert("items.stream".into(), json!({"retain_bytes": retain, "records": ["put", "append", "remove", "snapshot"]}));
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
                a.lock().unwrap().step(n.wrapping_mul(31).wrapping_add(i as u64), text_bytes, g.retain);
                notify.notify_waiters();
            }
            g.stats.records.fetch_add(g.agents.len() as u64, Ordering::Relaxed);
        }
    });
    // Stats line every 10 s on stderr.
    let s = shared.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            let (log, ents): (usize, usize) = s.agents.iter().fold((0, 0), |(l, e), (a, _)| { let a = a.lock().unwrap(); (l + a.log_bytes, e + a.entities.values().map(|x| x.value.len()).sum::<usize>()) });
            let st = &s.stats;
            eprintln!("STATS {}", json!({"t": now_ms(), "records": st.records.load(Ordering::Relaxed), "streams_open": st.streams.load(Ordering::Relaxed),
                "opens_continue": st.opens_continue.load(Ordering::Relaxed), "opens_reset": st.opens_reset.load(Ordering::Relaxed),
                "gaps": st.gaps.load(Ordering::Relaxed), "switch_resets": st.switch_resets.load(Ordering::Relaxed),
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
                        let session = a.epoch.rsplit('.').next().unwrap().parse::<u64>().unwrap() + 1;
                        a.epoch = epoch_for(&shared.instance, i, session);
                        a.seq = 0; a.log.clear(); a.log_bytes = 0; a.entities.clear(); a.current = None;
                        let epoch = a.epoch.clone();
                        drop(a);
                        notify.notify_waiters();
                        Ok(Outcome::Observed(Some(json!({"epoch": epoch}))))
                    }
                    ("items.pause", _) => {
                        shared.paused.store(e.args["paused"].as_bool().unwrap_or(true), Ordering::Relaxed);
                        Ok(Outcome::Observed(None))
                    }
                    ("items.digest", Some(i)) => {
                        let a = shared.agents[i].0.lock().unwrap();
                        Ok(Outcome::Observed(Some(json!({"epoch": a.epoch, "seq": a.seq, "digest": a.digest(), "entities": a.entities.len()}))))
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
                let after = items.get("after").and_then(|a| Some((a["epoch"].as_str()?.to_string(), a["seq"].as_u64()?)));
                // Decide continue/reset and capture the opening bytes at one cut, under the lock.
                let (reset, epoch, cursor, opening) = {
                    let a = shared.agents[i].0.lock().unwrap();
                    match after {
                        Some((e, s)) if e == a.epoch && s <= a.seq && s + 1 >= a.first_retained() => (false, a.epoch.clone(), s, Vec::new()),
                        _ => (true, a.epoch.clone(), a.seq, a.snapshot()),
                    }
                };
                let stats = if reset { &shared.stats.opens_reset } else { &shared.stats.opens_continue };
                stats.fetch_add(1, Ordering::Relaxed);
                let Some(stream) = o.respond_items(reset, &epoch) else { continue };
                tokio::spawn(viewer(shared.clone(), i, stream, epoch, cursor, opening));
            }
        }
    }
}

/// Write `bytes` split at odd offsets (mid-UTF-8 and mid-line on purpose).
async fn send_split(stream: &ProviderStream, bytes: &[u8], salt: u64, sent: &AtomicU64) -> Result<(), ProviderError> {
    let mut at = 0;
    let mut k = salt;
    while at < bytes.len() {
        k = k.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let n = ((k >> 33) as usize % 997 + 1).min(bytes.len() - at);
        stream.send(&bytes[at..at + n]).await?;
        at += n;
    }
    sent.fetch_add(bytes.len() as u64, Ordering::Relaxed);
    Ok(())
}

async fn viewer(shared: Arc<Shared>, i: usize, mut stream: ProviderStream, mut epoch: String, mut cursor: u64, opening: Vec<u8>) {
    shared.stats.streams.fetch_add(1, Ordering::Relaxed);
    let sent = &shared.stats.sent_bytes;
    let salt = stream.id;
    let ok = async {
        if !opening.is_empty() { send_split(&stream, &opening, salt, sent).await?; }
        loop {
            let notify = &shared.agents[i].1;
            let wake = notify.notified();
            tokio::pin!(wake);
            wake.as_mut().enable();
            // Collect what this viewer is missing, at most 64 KiB per round, without holding the lock while sending.
            let batch = {
                let a = shared.agents[i].0.lock().unwrap();
                if a.epoch != epoch {
                    // Session switch: new epoch, in-stream reset + snapshot (the fence), then live.
                    epoch = a.epoch.clone();
                    cursor = a.seq;
                    shared.stats.switch_resets.fetch_add(1, Ordering::Relaxed);
                    Some(a.snapshot())
                } else if cursor < a.seq && cursor + 1 < a.first_retained() {
                    None // fell out of retention: explicit gap for this viewer only
                } else {
                    let mut out = Vec::new();
                    let from = cursor;
                    for (s, line) in a.log.iter().filter(|(s, _)| *s > from) {
                        if out.len() + line.len() > 65536 && !out.is_empty() { break; }
                        out.extend_from_slice(line);
                        cursor = *s;
                    }
                    Some(out)
                }
            };
            match batch {
                None => {
                    shared.stats.gaps.fetch_add(1, Ordering::Relaxed);
                    stream.gap("retention").await?;
                    return Ok(());
                }
                Some(b) if !b.is_empty() => { send_split(&stream, &b, salt.wrapping_add(cursor), sent).await?; continue; }
                Some(_) => {}
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
    let _: Result<(), ProviderError> = ok.await;
    shared.stats.streams.fetch_sub(1, Ordering::Relaxed);
}
