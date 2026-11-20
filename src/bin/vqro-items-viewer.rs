//! A checking viewer for the item-stream gate (`tests/e2e_item_streams.py`): one client connection that attaches the
//! item stream (agent-v0 §4) of every agent of a provider and reconstructs each with the SDK's [`ItemView`]
//! (installs snapshots only at `end`; counts duplicate, missing, stale-base, unknown-entity, UTF-8/JSON and framing
//! errors across arbitrary byte splits). A `stream.gap`/`stream.closed` reattaches with the view's `after`;
//! `continue` keeps the state, `reset` replaces it from the in-stream snapshot. Attaches are windowed (at most 64 in
//! flight, under the core's 128 requests per connection). `--stall` attaches everything and then never reads its
//! socket. Prints a stats line every 10 s and, at the end (`--seconds`), per-agent digests to `--out`.
use base64::Engine;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use vqro::provider_client::items::{ItemView, ViewErrors};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

fn now_ms() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64 }
fn arg(name: &str, default: &str) -> String {
    let a: Vec<String> = std::env::args().collect();
    a.iter().position(|x| x == name).and_then(|i| a.get(i + 1).cloned()).unwrap_or_else(|| default.into())
}

#[derive(Default)]
struct View {
    view: ItemView,
    stream: Option<u64>,
    epoch_seen: Option<String>,
}

/// Connection-level counters; integrity errors are summed from every view at the end.
#[derive(Default, serde::Serialize)]
struct Counters {
    records: u64, bytes: u64, opens_reset: u64, opens_continue: u64, in_stream_resets: u64, gaps: u64, closed: u64,
    reattach: u64, attach_errors: u64, rpc_bad_json: u64,
    #[serde(flatten)]
    errors: ViewErrors,
}

impl View {
    fn feed(&mut self, data: &[u8], c: &mut Counters, lat: &mut Vec<u32>) {
        c.bytes += data.len() as u64;
        let before = self.view.records;
        self.view.feed_records(data, |r| {
            if let Some(ts) = r["value"]["native"]["ts"].as_u64() { lat.push(now_ms().saturating_sub(ts) as u32); }
        });
        c.records += self.view.records - before;
        // An installed snapshot with a different epoch than the one this viewer last saw: an in-stream reset.
        if let Some(e) = self.view.epoch() {
            if self.epoch_seen.as_deref().is_some_and(|old| old != e) { c.in_stream_resets += 1; }
            self.epoch_seen = Some(e.to_string());
        }
    }
    fn after(&self) -> Value {
        let a = self.view.after();
        if a.is_null() { json!({}) } else { json!({"after": a}) }
    }
}

fn sum_errors(views: &HashMap<String, View>) -> ViewErrors {
    views.values().fold(ViewErrors::default(), |mut a, v| {
        let e = &v.view.errors;
        a.duplicate += e.duplicate; a.missing += e.missing; a.stale_base += e.stale_base; a.unknown_entity += e.unknown_entity;
        a.bad_utf8 += e.bad_utf8; a.bad_json += e.bad_json; a.bad_record += e.bad_record; a.out_of_snapshot += e.out_of_snapshot;
        a.oversize += e.oversize; a.over_bound += e.over_bound;
        a
    })
}

fn pct(v: &mut [u32], q: f64) -> u32 { if v.is_empty() { return 0 } v.sort_unstable(); v[((v.len() as f64 * q) as usize).min(v.len() - 1)] }

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let name = arg("--name", "viewer");
    let provider = arg("--provider", "items");
    let seconds: u64 = arg("--seconds", "60").parse().unwrap();
    let stall = std::env::args().any(|a| a == "--stall");
    let out = arg("--out", "");
    let sock = tokio::net::UnixStream::connect(std::env::var("VQRO_SOCKET").unwrap()).await.unwrap();
    let (rd, mut wr) = sock.into_split();
    let mut rd = BufReader::with_capacity(1 << 20, rd);
    let mut next_id = 1u64;
    let mut send = |method: &str, params: Value, wr_buf: &mut Vec<u8>| -> u64 {
        let id = next_id; next_id += 1;
        wr_buf.extend(serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap());
        wr_buf.push(b'\n');
        id
    };
    let mut wbuf = Vec::new();
    let mut line = String::new();
    // hello + list
    let hid = send("hello", json!({"client": name}), &mut wbuf);
    let lid = send("resource.list", json!({}), &mut wbuf);
    wr.write_all(&wbuf).await.unwrap(); wbuf.clear();
    let agents: Vec<String>;
    loop {
        line.clear();
        if rd.read_line(&mut line).await.unwrap() == 0 { panic!("core closed"); }
        let m: Value = serde_json::from_str(&line).unwrap();
        if m["id"] == lid {
            agents = m["result"]["resources"].as_array().unwrap().iter()
                .filter(|r| r["kind"] == "agent" && r["provider"] == provider.as_str() && r["available"] != false)
                .map(|r| r["id"].as_str().unwrap().to_string()).collect();
            break;
        }
        let _ = hid;
    }
    let mut views: HashMap<String, View> = agents.iter().map(|a| (a.clone(), View::default())).collect();
    let mut pending: HashMap<u64, String> = HashMap::new(); // request id -> resource
    let mut by_stream: HashMap<u64, String> = HashMap::new();
    let mut early: HashMap<u64, Vec<Vec<u8>>> = HashMap::new();
    // At most 64 attaches in flight: the core admits 128 requests per connection (server.rs).
    let mut queue: std::collections::VecDeque<String> = agents.iter().cloned().collect();
    while pending.len() < 64 { let Some(a) = queue.pop_front() else { break }; let id = send("resource.attach", json!({"resource": a, "items": {}}), &mut wbuf); pending.insert(id, a); }
    wr.write_all(&wbuf).await.unwrap(); wbuf.clear();
    let t0 = Instant::now();
    let mut c = Counters::default();
    let mut lat: Vec<u32> = Vec::new();
    let mut report = Instant::now();
    let deadline = t0 + Duration::from_secs(seconds);
    // Failed attaches retry from a timer, never by sleeping in the read loop (that would stall every stream).
    let mut retry: Vec<(Instant, String)> = Vec::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    // read_line is not cancel-safe: a select! that drops it mid-line loses bytes. Lines come from a reader task.
    let (ltx, mut lines) = tokio::sync::mpsc::channel::<String>(1024);
    tokio::spawn(async move {
        loop {
            let mut l = String::new();
            match rd.read_line(&mut l).await { Ok(0) | Err(_) => break, Ok(_) => { if ltx.send(l).await.is_err() { break } } }
        }
    });
    loop {
        let n = tokio::select! {
            l = lines.recv() => match l { Some(l) => { line = l; line.len() } None => 0 },
            _ = tokio::time::sleep_until(deadline.into()) => break,
            _ = tick.tick() => {
                let now = Instant::now();
                let due: Vec<String> = retry.iter().filter(|(t, _)| *t <= now).map(|(_, r)| r.clone()).collect();
                retry.retain(|(t, _)| *t > now);
                queue.extend(due);
                while pending.len() < 64 {
                    let Some(res) = queue.pop_front() else { break };
                    let after = views[&res].after();
                    let id = send("resource.attach", json!({"resource": res, "items": after}), &mut wbuf);
                    pending.insert(id, res);
                }
                if !wbuf.is_empty() { wr.write_all(&wbuf).await.unwrap(); wbuf.clear(); }
                continue;
            }
        };
        if n == 0 { eprintln!("{name}: core closed"); break; }
        let m: Value = match serde_json::from_str(&line) { Ok(m) => m, Err(_) => { c.rpc_bad_json += 1; continue } };
        if let Some(id) = m["id"].as_u64() {
            if let Some(res) = pending.remove(&id) {
                if let Some(sid) = m["result"]["stream"].as_u64() {
                    match m["result"]["initial"].as_str() { Some("reset") => c.opens_reset += 1, _ => c.opens_continue += 1 }
                    by_stream.insert(sid, res.clone());
                    let v = views.get_mut(&res).unwrap();
                    v.stream = Some(sid);
                    for d in early.remove(&sid).unwrap_or_default() { v.feed(&d, &mut c, &mut lat); }
                } else {
                    c.attach_errors += 1;
                    if c.attach_errors <= 5 || c.attach_errors % 500 == 0 { eprintln!("{name}: attach {res}: {} (in flight {}, attached {})", m["error"], pending.len(), by_stream.len()); }
                    retry.push((Instant::now() + Duration::from_millis(500), res));
                }
            }
        } else {
            let p = &m["params"];
            let sid = p["stream"].as_u64().unwrap_or(u64::MAX);
            match m["method"].as_str().unwrap_or("") {
                "stream.data" => {
                    let d = B64.decode(p["data"].as_str().unwrap_or("")).unwrap_or_default();
                    match by_stream.get(&sid) {
                        Some(res) => views.get_mut(res).unwrap().feed(&d, &mut c, &mut lat),
                        None => early.entry(sid).or_default().push(d),
                    }
                }
                m @ ("stream.gap" | "stream.closed") => {
                    if m == "stream.gap" { c.gaps += 1 } else { c.closed += 1 }
                    if let Some(res) = by_stream.remove(&sid) {
                        let v = views.get_mut(&res).unwrap();
                        // A partial line or an unfinished snapshot is never carried onto a new stream.
                        v.view.stream_ended();
                        v.stream = None;
                        c.reattach += 1;
                        queue.push_back(res); // windowed like every attach: never a burst past the request limit
                    }
                }
                _ => {}
            }
        }
        // At most 64 attaches in flight (the core admits 128 requests per connection); reattach from the
        // last installed cursor, or from nothing.
        while pending.len() < 64 {
            let Some(res) = queue.pop_front() else { break };
            let after = views[&res].after();
            let id = send("resource.attach", json!({"resource": res, "items": after}), &mut wbuf);
            pending.insert(id, res);
        }
        if !wbuf.is_empty() { wr.write_all(&wbuf).await.unwrap(); wbuf.clear(); }
        if stall && by_stream.len() == agents.len() {
            // Every stream is attached: from now on never read the socket (a stalled viewer).
            eprintln!("{name}: attached {} item streams; now never reading", by_stream.len());
            drop(lines);
            tokio::time::sleep_until(deadline.into()).await;
            return;
        }
        if report.elapsed() >= Duration::from_secs(10) {
            report = Instant::now();
            let (p50, p99, mx) = (pct(&mut lat, 0.5), pct(&mut lat, 0.99), lat.iter().copied().max().unwrap_or(0));
            c.errors = sum_errors(&views);
            println!("{}", json!({"viewer": name, "t_s": t0.elapsed().as_secs(), "lat_ms": {"n": lat.len(), "p50": p50, "p99": p99, "max": mx}, "counters": c}));
            lat.clear();
        }
    }
    c.errors = sum_errors(&views);
    let digests: BTreeMap<&String, Value> = views.iter().map(|(k, v)| (k, json!({"epoch": v.view.epoch(), "seq": v.view.seq(), "digest": v.view.digest(), "entities": v.view.len(), "attached": v.stream.is_some()}))).collect();
    println!("{}", json!({"viewer": name, "final": true, "counters": c}));
    if !out.is_empty() { std::fs::write(&out, serde_json::to_vec_pretty(&json!({"viewer": name, "counters": c, "agents": digests})).unwrap()).unwrap(); }
}
