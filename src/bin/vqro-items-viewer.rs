//! Prototype P3 viewer: one client connection that attaches the item stream of every agent of a provider,
//! reconstructs each agent's entities and checks every record. Evidence, not product.
//!
//! Checks per stream: contiguous `seq` within an epoch (a lower one is a duplicate, a higher one a fabricated
//! gap), append `base` equals the entity's current `rev` (else stale), the hash `h` of the value after each
//! record matches (else fabricated/stale history), lines decode as UTF-8 JSON across arbitrary byte splits.
//! A `stream.gap`/`stream.closed` reattaches with `after: {epoch, seq}`; `continue` keeps the state, `reset`
//! replaces it from the in-stream snapshot. `--stall` attaches everything and then never reads its socket.
//! Prints a stats line every 10 s and, at the end (`--seconds`), per-agent digests to `--out`.
use base64::Engine;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

fn fnv(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() { h ^= *b as u64; h = h.wrapping_mul(0x100000001b3); }
    format!("{h:016x}")
}
fn now_ms() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64 }
fn arg(name: &str, default: &str) -> String {
    let a: Vec<String> = std::env::args().collect();
    a.iter().position(|x| x == name).and_then(|i| a.get(i + 1).cloned()).unwrap_or_else(|| default.into())
}

#[derive(Default)]
struct View {
    epoch: Option<String>,
    seq: u64,
    /// Puts of a snapshot being received; installed only at `snapshot end` (agent-v0 §4).
    pending: Option<(String, BTreeMap<String, (u64, String)>)>,
    entities: BTreeMap<String, (u64, String)>,
    buf: Vec<u8>,
    stream: Option<u64>,
}

#[derive(Default, serde::Serialize)]
struct Counters {
    records: u64, bytes: u64, opens_reset: u64, opens_continue: u64, in_stream_resets: u64, gaps: u64, closed: u64,
    reattach: u64, attach_errors: u64,
    duplicate: u64, missing: u64, stale_base: u64, hash_mismatch: u64, bad_utf8: u64, bad_json: u64, unknown_entity: u64, out_of_snapshot: u64,
}

impl View {
    fn digest(&self) -> String {
        let mut all = String::new();
        for (id, (rev, v)) in &self.entities { all.push_str(&format!("{id}:{rev}:{};", fnv(v))); }
        fnv(&all)
    }
    fn feed(&mut self, data: &[u8], c: &mut Counters, lat: &mut Vec<u32>) {
        self.buf.extend_from_slice(data);
        c.bytes += data.len() as u64;
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let Ok(text) = std::str::from_utf8(&line[..line.len() - 1]) else { c.bad_utf8 += 1; continue };
            let Ok(r) = serde_json::from_str::<Value>(text) else {
                c.bad_json += 1;
                if c.bad_json <= 5 { eprintln!("BADJSON stream {:?} seq {} len {}: {:?}", self.stream, self.seq, text.len(), &text[..text.len().min(300)]); }
                continue
            };
            self.apply(&r, c, lat);
        }
    }
    fn apply(&mut self, r: &Value, c: &mut Counters, lat: &mut Vec<u32>) {
        let op = r["type"].as_str().unwrap_or("");
        let id = r["id"].as_str().unwrap_or("").to_string();
        if op == "snapshot" {
            match r["phase"].as_str() {
                Some("begin") => self.pending = Some((r["epoch"].as_str().unwrap_or("").to_string(), BTreeMap::new())),
                Some("end") => match self.pending.take() {
                    // Install only now: the new epoch, every entity and the cut's `seq`.
                    Some((epoch, entities)) => {
                        if self.epoch.is_some() && self.epoch.as_deref() != Some(epoch.as_str()) { c.in_stream_resets += 1; }
                        self.epoch = Some(epoch);
                        self.entities = entities;
                        self.seq = r["seq"].as_u64().unwrap_or(0);
                    }
                    None => c.out_of_snapshot += 1,
                },
                _ => c.out_of_snapshot += 1,
            }
            return;
        }
        if let Some((_, entities)) = self.pending.as_mut() {
            // Snapshot puts carry no seq; anything else inside a snapshot is a protocol error.
            if op != "put" || r.get("seq").is_some() { c.out_of_snapshot += 1; return; }
            let v = r["value"].as_str().unwrap_or("").to_string();
            if r["h"].as_str() != Some(&fnv(&v)) { c.hash_mismatch += 1; }
            entities.insert(id, (r["rev"].as_u64().unwrap_or(0), v));
            return;
        }
        {
            let seq = r["seq"].as_u64().unwrap_or(0);
            if seq <= self.seq { c.duplicate += 1; return; }
            if seq != self.seq + 1 { c.missing += 1; if c.missing <= 5 { eprintln!("MISSING stream {:?} expected {} got {}", self.stream, self.seq + 1, seq); } }
            self.seq = seq;
            c.records += 1;
            if let Some(ts) = r["ts"].as_u64() { lat.push(now_ms().saturating_sub(ts) as u32); }
        }
        match op {
            "put" => {
                let v = r["value"].as_str().unwrap_or("").to_string();
                if r["h"].as_str() != Some(&fnv(&v)) { c.hash_mismatch += 1; }
                self.entities.insert(id, (r["rev"].as_u64().unwrap_or(0), v));
            }
            "append" => match self.entities.get_mut(&id) {
                None => c.unknown_entity += 1,
                Some((rev, v)) => {
                    if r["base"].as_u64() != Some(*rev) { c.stale_base += 1; return; }
                    v.push_str(r["text"].as_str().unwrap_or(""));
                    *rev = r["rev"].as_u64().unwrap_or(*rev + 1);
                    if r["h"].as_str() != Some(&fnv(v)) { c.hash_mismatch += 1; }
                }
            },
            "remove" => { if self.entities.remove(&id).is_none() { c.unknown_entity += 1; } }
            _ => {}
        }
    }
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
                for res in due {
                    let v = &views[&res];
                    let after = v.epoch.as_ref().map(|e| json!({"after": {"epoch": e, "seq": v.seq}})).unwrap_or(json!({}));
                    let id = send("resource.attach", json!({"resource": res, "items": after}), &mut wbuf);
                    pending.insert(id, res);
                }
                if !wbuf.is_empty() { wr.write_all(&wbuf).await.unwrap(); wbuf.clear(); }
                continue;
            }
        };
        if n == 0 { eprintln!("{name}: core closed"); break; }
        let m: Value = match serde_json::from_str(&line) { Ok(m) => m, Err(_) => { c.bad_json += 1; continue } };
        if let Some(id) = m["id"].as_u64() {
            if pending.contains_key(&id) {
                if let Some(a) = queue.pop_front() { let nid = send("resource.attach", json!({"resource": a, "items": {}}), &mut wbuf); pending.insert(nid, a); }
            }
            if let Some(res) = pending.remove(&id) {
                if let Some(sid) = m["result"]["stream"].as_u64() {
                    match m["result"]["initial"].as_str() { Some("reset") => c.opens_reset += 1, _ => c.opens_continue += 1 }
                    by_stream.insert(sid, res.clone());
                    let v = views.get_mut(&res).unwrap();
                    v.stream = Some(sid);
                    for d in early.remove(&sid).unwrap_or_default() { v.feed(&d, &mut c, &mut lat); }
                } else {
                    c.attach_errors += 1;
                    if c.attach_errors <= 5 || c.attach_errors % 500 == 0 { eprintln!("{name}: attach {res}: {}", m["error"]); }
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
                        // A partial line from the old stream is never stitched onto a new one.
                        v.buf.clear();
                        v.stream = None;
                        v.pending = None; // an unfinished snapshot is never installed; continue from the last installed cut
                        let after = v.epoch.as_ref().map(|e| json!({"after": {"epoch": e, "seq": v.seq}})).unwrap_or(json!({}));
                        c.reattach += 1;
                        let id = send("resource.attach", json!({"resource": res, "items": after}), &mut wbuf);
                        pending.insert(id, res);
                    }
                }
                _ => {}
            }
        }
        if !wbuf.is_empty() { wr.write_all(&wbuf).await.unwrap(); wbuf.clear(); }
        if stall && queue.is_empty() && pending.is_empty() {
            // Every stream is attached: from now on never read the socket (a stalled viewer).
            eprintln!("{name}: attached {} item streams; now never reading", by_stream.len());
            drop(lines);
            tokio::time::sleep_until(deadline.into()).await;
            return;
        }
        if report.elapsed() >= Duration::from_secs(10) {
            report = Instant::now();
            let (p50, p99, mx) = (pct(&mut lat, 0.5), pct(&mut lat, 0.99), lat.iter().copied().max().unwrap_or(0));
            println!("{}", json!({"viewer": name, "t_s": t0.elapsed().as_secs(), "lat_ms": {"n": lat.len(), "p50": p50, "p99": p99, "max": mx}, "counters": c}));
            lat.clear();
        }
    }
    let digests: BTreeMap<&String, Value> = views.iter().map(|(k, v)| (k, json!({"epoch": v.epoch, "seq": v.seq, "digest": v.digest(), "entities": v.entities.len(), "attached": v.stream.is_some()}))).collect();
    println!("{}", json!({"viewer": name, "final": true, "counters": c}));
    if !out.is_empty() { std::fs::write(&out, serde_json::to_vec_pretty(&json!({"viewer": name, "counters": c, "agents": digests})).unwrap()).unwrap(); }
}
