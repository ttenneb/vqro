//! Adapter-side item streams (agent-v0 §4): one shared, pure, bounded producer and a matching reducer.
//!
//! - [`ItemJournal`]: an adapter's current entities (keyed `(kind, id)` within one epoch, each with a `rev`) and a
//!   retained journal of `op` records with contiguous `seq`, bounded by `retain_bytes`. Mutations emit exactly the
//!   §4 records (`put`, `append` text or bytes, `remove`, `notice`). The epoch changes on journal loss and session
//!   switch ([`ItemJournal::new_epoch`]).
//! - [`ItemCursor`]: one viewer's position. [`ItemJournal::open`] decides `continue` (the viewer's `after` is in the
//!   current epoch and still retained) or `reset` (one serialized cut: `snapshot begin`, a `put` per entity without
//!   `seq`, `snapshot end {seq}`). [`ItemCursor::next`] then yields the records strictly after the cursor, a new
//!   in-stream snapshot after an epoch change, or the gap reason `retention` when the viewer fell out of the journal.
//!   Changes after a cut are buffered in the journal itself, so the post-cut bound is `retain_bytes`.
//! - [`ItemView`]: a viewer-side reducer that applies the byte stream (lines may span frames), installs a snapshot
//!   only at `end`, and counts every protocol violation (duplicate, missing, stale base, unknown entity, …).
//!
//! Pure and synchronous: no I/O, no tasks, no clock. An adapter keeps one journal per agent under its own lock and,
//! per open stream, a cursor whose output it writes with [`super::ProviderStream::send`] (or `gap("retention")`).
use super::ProviderError;
use base64::Engine;
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// agent-v0 §4 and §8 bounds.
#[derive(Debug, Clone)]
pub struct JournalConfig {
    /// Retained journal bytes (encoded lines). Default 128 KiB per agent; `retain_bytes × agents` is the
    /// adapter's steady journal memory. Declare it in facts as `items.retain_bytes`.
    pub retain_bytes: usize,
    /// One whole encoded record, newline included. 64 KiB.
    pub max_record_bytes: usize,
    /// Retained entities per endpoint (agent-v0 §8: declared in facts as `items.entities`, default 512).
    pub max_entities: usize,
    /// All current entities together, encoded values (`items.entity_bytes`, default 512 KiB). Bounds every snapshot.
    pub max_entity_bytes: usize,
}

impl Default for JournalConfig {
    fn default() -> Self { JournalConfig { retain_bytes: 128 * 1024, max_record_bytes: 64 * 1024, max_entities: 512, max_entity_bytes: 512 * 1024 } }
}

impl JournalConfig {
    /// The `items` facts an adapter declares for these bounds (agent-v0 §8).
    pub fn facts(&self) -> serde_json::Value {
        serde_json::json!({"retain_bytes": self.retain_bytes, "entities": self.max_entities, "entity_bytes": self.max_entity_bytes})
    }
}

/// What a snapshot claims to contain (agent-v0 §4 `coverage`). `tail` and `state_only` are never shown as complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// Every entity the adapter retains (the default).
    AllRetained,
    /// Only the recent part: older entities were pruned or native history is missing.
    Tail,
    /// Current state only (state, pending requests), no timeline.
    StateOnly,
}

impl Coverage {
    pub fn as_str(self) -> &'static str {
        match self { Coverage::AllRetained => "all_retained", Coverage::Tail => "tail", Coverage::StateOnly => "state_only" }
    }
}

struct Entity {
    rev: u64,
    value: Value,
    bytes: usize,
}

/// One agent's entities and retained records. See the module docs.
pub struct ItemJournal {
    cfg: JournalConfig,
    epoch: String,
    seq: u64,
    log: VecDeque<(u64, Arc<[u8]>)>,
    log_bytes: usize,
    entities: BTreeMap<(String, String), Entity>,
    entity_bytes: usize,
    coverage: Coverage,
}

fn line(v: &Value) -> Vec<u8> {
    let mut out = serde_json::to_vec(v).expect("records serialize");
    out.push(b'\n');
    out
}

fn invalid(m: impl Into<String>) -> ProviderError { ProviderError::new("invalid", m) }

/// Resolve a JSON Pointer to an existing string (agent-v0 §4 `field`).
fn string_at<'a>(value: &'a mut Value, field: &str) -> Result<&'a mut String, ProviderError> {
    match value.pointer_mut(field) {
        Some(Value::String(s)) => Ok(s),
        Some(_) => Err(invalid(format!("field {field} is not a string"))),
        None => Err(invalid(format!("field {field} does not exist"))),
    }
}

impl ItemJournal {
    pub fn new(epoch: impl Into<String>, cfg: JournalConfig) -> Self {
        ItemJournal { cfg, epoch: epoch.into(), seq: 0, log: VecDeque::new(), log_bytes: 0, entities: BTreeMap::new(), entity_bytes: 0, coverage: Coverage::AllRetained }
    }
    pub fn epoch(&self) -> &str { &self.epoch }
    /// The last emitted `seq` (0 before any record in this epoch).
    pub fn seq(&self) -> u64 { self.seq }
    /// The lowest retained `seq`; `seq() + 1` when nothing is retained.
    pub fn first_retained(&self) -> u64 { self.log.front().map(|(s, _)| *s).unwrap_or(self.seq + 1) }
    pub fn retained_bytes(&self) -> usize { self.log_bytes }
    pub fn entity_bytes(&self) -> usize { self.entity_bytes }
    pub fn retain_bytes(&self) -> usize { self.cfg.retain_bytes }
    /// The entity's `(rev, value)` as of the last record.
    pub fn entity(&self, kind: &str, id: &str) -> Option<(u64, &Value)> {
        self.entities.get(&(kind.to_string(), id.to_string())).map(|e| (e.rev, &e.value))
    }
    pub fn len(&self) -> usize { self.entities.len() }
    pub fn is_empty(&self) -> bool { self.entities.is_empty() }

    /// A new epoch: the journal is lost or the session switched. Entities are cleared (the adapter then puts the
    /// new session's state); open cursors send a new `snapshot begin` before any new-epoch record.
    pub fn new_epoch(&mut self, epoch: impl Into<String>) {
        let epoch = epoch.into();
        assert_ne!(epoch, self.epoch, "a new epoch must differ from the current one");
        self.epoch = epoch;
        self.seq = 0;
        self.log.clear();
        self.log_bytes = 0;
        self.entities.clear();
        self.entity_bytes = 0;
        self.coverage = Coverage::AllRetained;
    }

    /// What snapshots claim to contain from now on. Set `Tail` when native history is missing, `StateOnly` when only
    /// current state is known. Reset to `AllRetained` by [`ItemJournal::new_epoch`].
    pub fn set_coverage(&mut self, coverage: Coverage) { self.coverage = coverage; }
    pub fn coverage(&self) -> Coverage { self.coverage }

    /// Drop an old entity from what snapshots contain, without a `remove` record: viewers that already have it keep
    /// it, later snapshots omit it, and coverage becomes `tail` (unless already `state_only`). Use it to stay under
    /// the entity bounds; never for current state or pending requests (they must survive pruning).
    pub fn prune(&mut self, kind: &str, id: &str) -> bool {
        match self.entities.remove(&(kind.to_string(), id.to_string())) {
            Some(e) => {
                self.entity_bytes -= e.bytes;
                if self.coverage == Coverage::AllRetained { self.coverage = Coverage::Tail; }
                true
            }
            None => false,
        }
    }

    fn check_ids(kind: &str, id: &str) -> Result<(), ProviderError> {
        if kind.is_empty() || id.is_empty() || kind.len() > 128 || id.len() > 128 { return Err(invalid("kind and id must be 1-128 bytes")); }
        Ok(())
    }

    /// Emit one record with the next `seq` (already validated against the record bound).
    fn emit(&mut self, mut record: Value) -> u64 {
        self.seq += 1;
        record["seq"] = json!(self.seq);
        let l: Arc<[u8]> = line(&record).into();
        self.log_bytes += l.len();
        self.log.push_back((self.seq, l));
        while self.log_bytes > self.cfg.retain_bytes && self.log.len() > 1 {
            let (_, old) = self.log.pop_front().unwrap();
            self.log_bytes -= old.len();
        }
        self.seq
    }

    /// Size of `record` once it carries the next `seq`, against the 64 KiB record bound.
    fn fits(&self, record: &Value) -> Result<(), ProviderError> {
        let n = serde_json::to_vec(record).map(|v| v.len()).unwrap_or(usize::MAX).saturating_add(32);
        if n > self.cfg.max_record_bytes { return Err(ProviderError::new("too_large", format!("record of {n} bytes exceeds {}", self.cfg.max_record_bytes))); }
        Ok(())
    }

    fn charge(&self, old: usize, new: usize) -> Result<(), ProviderError> {
        if self.entity_bytes - old + new > self.cfg.max_entity_bytes {
            return Err(ProviderError::new("overloaded", format!("entities would exceed {} bytes", self.cfg.max_entity_bytes)));
        }
        Ok(())
    }

    /// `put {seq, kind, id, rev, value}`: the whole entity (created or replaced).
    pub fn put(&mut self, kind: &str, id: &str, value: Value) -> Result<u64, ProviderError> {
        Self::check_ids(kind, id)?;
        let key = (kind.to_string(), id.to_string());
        let (rev, old) = self.entities.get(&key).map(|e| (e.rev + 1, e.bytes)).unwrap_or((1, 0));
        if rev == 1 && self.entities.len() >= self.cfg.max_entities {
            return Err(ProviderError::new("overloaded", format!("more than {} entities", self.cfg.max_entities)));
        }
        let record = json!({"op": "put", "seq": 0, "kind": kind, "id": id, "rev": rev, "value": value.clone()});
        self.fits(&record)?;
        let bytes = serde_json::to_vec(&value).map(|v| v.len()).unwrap_or(0);
        self.charge(old, bytes)?;
        self.entity_bytes = self.entity_bytes - old + bytes;
        let seq = self.emit(record);
        self.entities.insert(key, Entity { rev, value, bytes });
        Ok(seq)
    }

    /// `append {seq, kind, id, base, rev, field, text}`: append to the existing string at JSON Pointer `field`.
    pub fn append_text(&mut self, kind: &str, id: &str, field: &str, text: &str) -> Result<u64, ProviderError> {
        self.append(kind, id, field, json!({"text": text}), |s| { s.push_str(text); Ok(()) })
    }

    /// `append {seq, kind, id, base, rev, field, bytes}`: `bytes` is one base64 chunk appended to the byte buffer the
    /// field holds as base64 of the whole buffer (never concatenated as text).
    pub fn append_bytes(&mut self, kind: &str, id: &str, field: &str, bytes: &[u8]) -> Result<u64, ProviderError> {
        let chunk = B64.encode(bytes);
        self.append(kind, id, field, json!({"bytes": chunk}), |s| {
            let mut buf = B64.decode(s.as_bytes()).map_err(|_| invalid("bytes field is not base64"))?;
            buf.extend_from_slice(bytes);
            *s = B64.encode(&buf);
            Ok(())
        })
    }

    fn append(&mut self, kind: &str, id: &str, field: &str, payload: Value, apply: impl FnOnce(&mut String) -> Result<(), ProviderError>) -> Result<u64, ProviderError> {
        Self::check_ids(kind, id)?;
        let key = (kind.to_string(), id.to_string());
        let e = self.entities.get(&key).ok_or_else(|| ProviderError::new("not_found", format!("no entity {kind}/{id}")))?;
        let (base, old) = (e.rev, e.bytes);
        let mut value = e.value.clone();
        apply(string_at(&mut value, field)?)?;
        let mut record = json!({"op": "append", "seq": 0, "kind": kind, "id": id, "base": base, "rev": base + 1, "field": field});
        for (k, v) in payload.as_object().unwrap() { record[k] = v.clone(); }
        self.fits(&record)?;
        // The resulting entity is sent whole as a snapshot put: it must fit the record bound too, or a snapshot
        // would carry an oversized line after many small appends.
        self.fits(&json!({"op": "put", "seq": 0, "kind": kind, "id": id, "rev": base + 1, "value": value}))?;
        let bytes = serde_json::to_vec(&value).map(|v| v.len()).unwrap_or(0);
        self.charge(old, bytes)?;
        self.entity_bytes = self.entity_bytes - old + bytes;
        let seq = self.emit(record);
        self.entities.insert(key, Entity { rev: base + 1, value, bytes });
        Ok(seq)
    }

    /// `remove {seq, kind, id, rev}`.
    pub fn remove(&mut self, kind: &str, id: &str) -> Result<u64, ProviderError> {
        let key = (kind.to_string(), id.to_string());
        let e = self.entities.remove(&key).ok_or_else(|| ProviderError::new("not_found", format!("no entity {kind}/{id}")))?;
        self.entity_bytes -= e.bytes;
        Ok(self.emit(json!({"op": "remove", "seq": 0, "kind": kind, "id": id, "rev": e.rev + 1})))
    }

    /// `notice {value}`: live only, no `seq`, not retained. Send the returned line to every open cursor's stream.
    pub fn notice(value: Value) -> Vec<u8> { line(&json!({"op": "notice", "value": value})) }

    /// One serialized cut: `snapshot begin`, a `put` per entity (no `seq`), `snapshot end {seq}`.
    pub fn snapshot(&self) -> Vec<u8> {
        let mut out = line(&json!({"op": "snapshot", "phase": "begin", "epoch": self.epoch, "coverage": self.coverage.as_str()}));
        for ((kind, id), e) in &self.entities {
            out.extend(line(&json!({"op": "put", "kind": kind, "id": id, "rev": e.rev, "value": e.value})));
        }
        out.extend(line(&json!({"op": "snapshot", "phase": "end", "seq": self.seq})));
        out
    }

    /// Open a viewer at its `after` (`{epoch, seq}` or null): `continue` if it is in this epoch and still retained,
    /// else `reset` with a snapshot at the current cut. Reply with `respond_items(cursor.reset(), cursor.epoch())`.
    pub fn open(&self, after: Option<&Value>) -> ItemCursor {
        let after = after.and_then(|a| Some((a["epoch"].as_str()?.to_string(), a["seq"].as_u64()?)));
        match after {
            Some((epoch, seq)) if epoch == self.epoch && seq <= self.seq && seq + 1 >= self.first_retained() => {
                ItemCursor { epoch, seq, reset: false, pending: None }
            }
            _ => ItemCursor { epoch: self.epoch.clone(), seq: self.seq, reset: true, pending: Some(self.snapshot()) },
        }
    }

    /// A live `seq` → JSON digest of the current entities (tests and conformance compare producer and viewer).
    pub fn digest(&self) -> String {
        digest(self.entities.iter().map(|((k, i), e)| ((k.as_str(), i.as_str()), e.rev, &e.value)))
    }
}

/// FNV-1a over `kind/id:rev:value;` in key order: equal digests mean equal entity sets.
pub fn digest<'a>(entities: impl Iterator<Item = ((&'a str, &'a str), u64, &'a Value)>) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for ((kind, id), rev, value) in entities {
        for b in format!("{kind}/{id}:{rev}:{value};").bytes() { h ^= b as u64; h = h.wrapping_mul(0x100000001b3); }
    }
    format!("{h:016x}")
}

/// What a cursor has to send next.
#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    /// Bytes to send (whole lines).
    Data(Vec<u8>),
    /// The viewer fell out of retention: end the stream with this gap reason (`retention`).
    Gap(&'static str),
    /// Nothing new; wait for the journal to change.
    Idle,
}

/// One viewer's position in an [`ItemJournal`]. Hold it per open stream; call [`ItemCursor::next`] under the
/// journal's lock, then send outside it.
#[derive(Debug, Clone)]
pub struct ItemCursor {
    epoch: String,
    seq: u64,
    reset: bool,
    pending: Option<Vec<u8>>,
}

impl ItemCursor {
    /// Whether the open answered `reset` (else `continue`).
    pub fn reset(&self) -> bool { self.reset }
    /// The epoch to report in the open reply.
    pub fn epoch(&self) -> &str { &self.epoch }
    /// The last `seq` this cursor has emitted (or the snapshot cut).
    pub fn seq(&self) -> u64 { self.seq }

    /// The next bytes for this viewer, at most about `max_bytes` (whole lines; a snapshot is sent whole).
    pub fn next(&mut self, j: &ItemJournal, max_bytes: usize) -> Next {
        if let Some(snapshot) = self.pending.take() { return Next::Data(snapshot); }
        if self.epoch != j.epoch {
            // Session switch or journal loss: a new snapshot fences the new epoch, in-stream.
            self.epoch = j.epoch.clone();
            self.seq = j.seq;
            return Next::Data(j.snapshot());
        }
        if self.seq < j.seq && self.seq + 1 < j.first_retained() { return Next::Gap("retention"); }
        let mut out = Vec::new();
        let from = self.seq;
        for (s, l) in j.log.iter().filter(|(s, _)| *s > from) {
            if !out.is_empty() && out.len() + l.len() > max_bytes { break; }
            out.extend_from_slice(l);
            self.seq = *s;
        }
        if out.is_empty() { Next::Idle } else { Next::Data(out) }
    }
}

/// Protocol violations a viewer counts (a correct producer and transport produce none).
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ViewErrors {
    /// A record at or below the applied `seq` (ignored, as §4 says).
    pub duplicate: u64,
    /// A `seq` beyond `applied + 1`: records were lost.
    pub missing: u64,
    /// An `append` whose `base` is not the entity's `rev` (a gap: reattach).
    pub stale_base: u64,
    pub unknown_entity: u64,
    pub bad_utf8: u64,
    pub bad_json: u64,
    /// A record that is not one of the §4 shapes.
    pub bad_record: u64,
    /// Snapshot framing out of order (a put with `seq` inside a snapshot, an `end` without `begin`, …).
    pub out_of_snapshot: u64,
}

impl ViewErrors {
    pub fn total(&self) -> u64 { self.duplicate + self.missing + self.stale_base + self.unknown_entity + self.bad_utf8 + self.bad_json + self.bad_record + self.out_of_snapshot }
}

/// A viewer's reconstruction of one agent's item stream.
#[derive(Debug, Default)]
pub struct ItemView {
    epoch: Option<String>,
    seq: u64,
    entities: BTreeMap<(String, String), (u64, Value)>,
    pending: Option<(String, BTreeMap<(String, String), (u64, Value)>)>,
    buf: Vec<u8>,
    pub errors: ViewErrors,
    /// Live records applied (excludes snapshot puts and notices).
    pub records: u64,
    /// Snapshots installed (opening resets and epoch changes).
    pub snapshots: u64,
}

impl ItemView {
    pub fn new() -> Self { Self::default() }
    pub fn epoch(&self) -> Option<&str> { self.epoch.as_deref() }
    pub fn seq(&self) -> u64 { self.seq }
    pub fn len(&self) -> usize { self.entities.len() }
    pub fn is_empty(&self) -> bool { self.entities.is_empty() }
    pub fn entity(&self, kind: &str, id: &str) -> Option<&(u64, Value)> { self.entities.get(&(kind.to_string(), id.to_string())) }
    /// The `items.after` to reattach with, or null before any installed snapshot.
    pub fn after(&self) -> Value {
        self.epoch.as_ref().map(|e| json!({"epoch": e, "seq": self.seq})).unwrap_or(Value::Null)
    }
    pub fn digest(&self) -> String { digest(self.entities.iter().map(|((k, i), (r, v))| ((k.as_str(), i.as_str()), *r, v))) }

    /// The stream ended (closed, gap): drop a partial line and any unfinished snapshot; keep the installed state.
    pub fn stream_ended(&mut self) {
        self.buf.clear();
        self.pending = None;
    }

    /// Feed stream bytes (any split); returns the notices received, in order.
    pub fn feed(&mut self, data: &[u8]) -> Vec<Value> { self.feed_records(data, |_| {}) }

    /// As [`ItemView::feed`], calling `on_record` with every live record applied (not snapshot puts or notices).
    pub fn feed_records(&mut self, data: &[u8], mut on_record: impl FnMut(&Value)) -> Vec<Value> {
        self.buf.extend_from_slice(data);
        let mut notices = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let l: Vec<u8> = self.buf.drain(..=pos).collect();
            let Ok(text) = std::str::from_utf8(&l[..l.len() - 1]) else { self.errors.bad_utf8 += 1; continue };
            let Ok(r) = serde_json::from_str::<Value>(text) else { self.errors.bad_json += 1; continue };
            let before = self.records;
            if let Some(n) = self.apply(&r) { notices.push(n); }
            if self.records != before { on_record(&r); }
        }
        notices
    }

    fn apply(&mut self, r: &Value) -> Option<Value> {
        let op = r["op"].as_str().unwrap_or("");
        let key = || Some((r["kind"].as_str()?.to_string(), r["id"].as_str()?.to_string()));
        match op {
            "notice" => return Some(r["value"].clone()),
            "snapshot" => {
                match (r["phase"].as_str(), self.pending.take()) {
                    (Some("begin"), _) => match r["epoch"].as_str() {
                        Some(e) if r["coverage"].is_string() => self.pending = Some((e.to_string(), BTreeMap::new())),
                        _ => self.errors.bad_record += 1,
                    },
                    // Installed only now (agent-v0 §4): epoch, every entity and the cut's seq.
                    (Some("end"), Some((epoch, entities))) => match r["seq"].as_u64() {
                        Some(seq) => { self.epoch = Some(epoch); self.entities = entities; self.seq = seq; self.snapshots += 1; }
                        None => self.errors.bad_record += 1,
                    },
                    _ => self.errors.out_of_snapshot += 1,
                }
                return None;
            }
            _ => {}
        }
        if let Some((_, entities)) = self.pending.as_mut() {
            // Inside a snapshot: only puts without seq.
            match (op, key(), r["rev"].as_u64(), r.get("seq")) {
                ("put", Some(k), Some(rev), None) if r.get("value").is_some() => { entities.insert(k, (rev, r["value"].clone())); }
                _ => self.errors.out_of_snapshot += 1,
            }
            return None;
        }
        let (Some(seq), Some(k), Some(rev)) = (r["seq"].as_u64(), key(), r["rev"].as_u64()) else { self.errors.bad_record += 1; return None };
        if self.epoch.is_none() { self.errors.out_of_snapshot += 1; return None; }
        if seq <= self.seq { self.errors.duplicate += 1; return None; }
        if seq != self.seq + 1 { self.errors.missing += 1; }
        self.seq = seq;
        self.records += 1;
        match op {
            "put" if r.get("value").is_some() => { self.entities.insert(k, (rev, r["value"].clone())); }
            "remove" => { if self.entities.remove(&k).is_none() { self.errors.unknown_entity += 1; } }
            "append" => {
                let Some((cur, value)) = self.entities.get_mut(&k) else { self.errors.unknown_entity += 1; return None };
                if r["base"].as_u64() != Some(*cur) { self.errors.stale_base += 1; return None; }
                let Some(field) = r["field"].as_str() else { self.errors.bad_record += 1; return None };
                let Some(Value::String(s)) = value.pointer_mut(field) else { self.errors.bad_record += 1; return None };
                match (r["text"].as_str(), r["bytes"].as_str()) {
                    (Some(t), None) => s.push_str(t),
                    (None, Some(b)) => match (B64.decode(s.as_bytes()), B64.decode(b)) {
                        (Ok(mut buf), Ok(chunk)) => { buf.extend(chunk); *s = B64.encode(buf); }
                        _ => { self.errors.bad_record += 1; return None; }
                    },
                    _ => { self.errors.bad_record += 1; return None; }
                }
                *cur = rev;
            }
            _ => self.errors.bad_record += 1,
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &[&str] = &["héllo ", "日本語 ", "🙂🚀 ", "naïve ", "Ω≈ç√ ", "ok "];

    /// Deterministic generator: puts, text/bytes appends and removes over a few entities.
    struct Gen(u64);
    impl Gen {
        fn next(&mut self) -> u64 { self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); self.0 >> 33 }
        fn step(&mut self, j: &mut ItemJournal) {
            let n = self.next();
            let id = format!("m{}", n % 6);
            match (n % 10, j.entity("message", &id).is_some()) {
                (0, true) => { j.remove("message", &id).unwrap(); }
                (1..=5, true) => { j.append_text("message", &id, "/content/0/text", TEXT[(n % 6) as usize]).unwrap(); }
                (6, true) => { j.append_bytes("message", &id, "/native/raw", &[(n % 256) as u8, 0]).unwrap(); }
                _ => { j.put("message", &id, json!({"position": n % 1000, "role": "assistant", "content": [{"type": "text", "text": TEXT[(n % 6) as usize]}], "final": false, "native": {"raw": ""}})).unwrap(); }
            }
        }
    }

    /// Feed `bytes` in pseudo-random splits (mid-UTF-8, mid-line).
    fn feed_split(v: &mut ItemView, bytes: &[u8], salt: &mut u64) {
        let mut at = 0;
        while at < bytes.len() {
            *salt = salt.wrapping_mul(6364136223846793005).wrapping_add(1);
            let n = ((*salt >> 33) as usize % 97 + 1).min(bytes.len() - at);
            v.feed(&bytes[at..at + n]);
            at += n;
        }
    }

    fn drain(c: &mut ItemCursor, j: &ItemJournal, v: &mut ItemView, salt: &mut u64) -> Option<&'static str> {
        loop {
            match c.next(j, 4096) {
                Next::Data(d) => feed_split(v, &d, salt),
                Next::Gap(g) => return Some(g),
                Next::Idle => return None,
            }
        }
    }

    #[test]
    fn a_viewer_reconstructs_the_producer_exactly_across_arbitrary_splits() {
        let mut j = ItemJournal::new("e1", JournalConfig::default());
        let mut g = Gen(7);
        for _ in 0..50 { g.step(&mut j); }
        let (mut v, mut salt) = (ItemView::new(), 1u64);
        let mut c = j.open(None);
        assert!(c.reset());
        for _ in 0..40 {
            for _ in 0..25 { g.step(&mut j); }
            assert_eq!(drain(&mut c, &j, &mut v, &mut salt), None);
            assert_eq!(v.digest(), j.digest());
            assert_eq!(v.seq(), j.seq());
        }
        assert_eq!(v.errors, ViewErrors::default());
        assert_eq!(v.snapshots, 1);
        assert!(v.records > 900);
    }

    #[test]
    fn reattach_continues_within_retention_and_resets_beyond_it() {
        let cfg = JournalConfig { retain_bytes: 8 * 1024, ..Default::default() };
        let mut j = ItemJournal::new("e1", cfg);
        let mut g = Gen(3);
        for _ in 0..20 { g.step(&mut j); }
        let (mut v, mut salt) = (ItemView::new(), 5u64);
        let mut c = j.open(None);
        drain(&mut c, &j, &mut v, &mut salt);
        // Stream ends; a few records later the viewer is still covered: continue from exactly its cursor.
        v.stream_ended();
        for _ in 0..5 { g.step(&mut j); }
        let mut c = j.open(Some(&v.after()));
        assert!(!c.reset());
        drain(&mut c, &j, &mut v, &mut salt);
        assert_eq!(v.digest(), j.digest());
        // Far beyond retention: reset with a fresh snapshot, never a partial history.
        v.stream_ended();
        for _ in 0..500 { g.step(&mut j); }
        assert!(v.seq() + 1 < j.first_retained());
        let mut c = j.open(Some(&v.after()));
        assert!(c.reset());
        drain(&mut c, &j, &mut v, &mut salt);
        assert_eq!((v.digest(), v.seq()), (j.digest(), j.seq()));
        assert_eq!(v.errors, ViewErrors::default());
        // A wrong epoch is a reset too.
        assert!(j.open(Some(&json!({"epoch": "other", "seq": 1}))).reset());
    }

    #[test]
    fn a_slow_viewer_gets_a_retention_gap_deterministically_and_recovers_by_reset() {
        // Producer-side overflow: the viewer reads one small batch per 40 records; retention holds far fewer.
        let cfg = JournalConfig { retain_bytes: 4 * 1024, ..Default::default() };
        let mut j = ItemJournal::new("e1", cfg);
        let mut g = Gen(11);
        let (mut v, mut salt) = (ItemView::new(), 9u64);
        let mut c = j.open(None);
        let mut gap = None;
        for _ in 0..50 {
            for _ in 0..40 { g.step(&mut j); }
            match c.next(&j, 256) {
                Next::Data(d) => feed_split(&mut v, &d, &mut salt),
                Next::Gap(r) => { gap = Some(r); break; }
                Next::Idle => {}
            }
        }
        assert_eq!(gap, Some("retention"), "falling out of the journal must be an explicit gap");
        assert_eq!(v.errors, ViewErrors::default(), "nothing stale or fabricated before the gap");
        v.stream_ended();
        let mut c = j.open(Some(&v.after()));
        assert!(c.reset());
        assert_eq!(drain(&mut c, &j, &mut v, &mut salt), None);
        assert_eq!((v.digest(), v.seq()), (j.digest(), j.seq()));
    }

    #[test]
    fn an_epoch_change_is_fenced_in_stream_and_an_unfinished_snapshot_is_never_installed() {
        let mut j = ItemJournal::new("e1", JournalConfig::default());
        let mut g = Gen(5);
        for _ in 0..30 { g.step(&mut j); }
        let (mut v, mut salt) = (ItemView::new(), 2u64);
        let mut c = j.open(None);
        drain(&mut c, &j, &mut v, &mut salt);
        let before = (v.digest(), v.seq());
        // Session switch: the open cursor sends a new snapshot begin before any new-epoch record.
        j.new_epoch("e2");
        j.put("state", "current", json!({"state": "idle"})).unwrap();
        let Next::Data(snap) = c.next(&j, 4096) else { panic!("expected a snapshot") };
        assert!(std::str::from_utf8(&snap).unwrap().starts_with(r#"{"coverage":"all_retained","epoch":"e2","op":"snapshot","phase":"begin"}"#));
        // The stream ends halfway through that snapshot: the old state stays installed, untouched.
        feed_split(&mut v, &snap[..snap.len() - 10], &mut salt);
        assert_eq!((v.digest(), v.seq()), before);
        v.stream_ended();
        assert_eq!(v.epoch(), Some("e1"));
        let mut c = j.open(Some(&v.after()));
        assert!(c.reset());
        drain(&mut c, &j, &mut v, &mut salt);
        assert_eq!((v.epoch(), v.digest(), v.seq()), (Some("e2"), j.digest(), j.seq()));
        assert_eq!(v.errors, ViewErrors::default());
    }

    #[test]
    fn records_carry_exactly_the_agent_v0_fields() {
        let mut j = ItemJournal::new("e1", JournalConfig::default());
        j.put("message", "m1", json!({"content": [{"type": "text", "text": "Runn"}], "raw": ""})).unwrap();
        j.append_text("message", "m1", "/content/0/text", "ing…").unwrap();
        j.append_bytes("message", "m1", "/raw", &[0xff]).unwrap();
        j.append_bytes("message", "m1", "/raw", &[0x00]).unwrap();
        j.remove("message", "m1").unwrap();
        let keys: Vec<Vec<String>> = j.log.iter().map(|(_, l)| {
            let r: Value = serde_json::from_slice(&l[..l.len() - 1]).unwrap();
            let mut k: Vec<String> = r.as_object().unwrap().keys().cloned().collect(); k.sort(); k
        }).collect();
        let want = |s: &[&str]| { let mut v: Vec<String> = s.iter().map(|x| x.to_string()).collect(); v.sort(); v };
        assert_eq!(keys[0], want(&["op", "seq", "kind", "id", "rev", "value"]));
        assert_eq!(keys[1], want(&["op", "seq", "kind", "id", "base", "rev", "field", "text"]));
        assert_eq!(keys[2], want(&["op", "seq", "kind", "id", "base", "rev", "field", "bytes"]));
        assert_eq!(keys[4], want(&["op", "seq", "kind", "id", "rev"]));
        let n: Value = serde_json::from_slice(&ItemJournal::notice(json!({"text": "rate limited"}))).unwrap();
        assert_eq!(n, json!({"op": "notice", "value": {"text": "rate limited"}}));
        // Byte chunks are bytes, never concatenated base64 text: "/w==" + "AA==" is ff 00.
        let mut j2 = ItemJournal::new("e1", JournalConfig::default());
        j2.put("t", "x", json!({"raw": ""})).unwrap();
        j2.append_bytes("t", "x", "/raw", &[0xff]).unwrap();
        j2.append_bytes("t", "x", "/raw", &[0x00]).unwrap();
        assert_eq!(j2.entity("t", "x").unwrap().1["raw"], "/wA=");
        let mut v2 = ItemView::new();
        let mut c2 = j2.open(Some(&json!({"epoch": "e1", "seq": 0})));
        assert!(!c2.reset());
        v2.feed(&ItemJournal::new("e1", JournalConfig::default()).snapshot());
        v2.feed(&c2.next(&j2, 1 << 20).into_data());
        assert_eq!(v2.entity("t", "x").unwrap().1["raw"], "/wA=");
        assert_eq!(v2.errors, ViewErrors::default());
    }

    #[test]
    fn bounds_reject_oversized_records_and_entity_overflow_without_emitting() {
        let mut j = ItemJournal::new("e1", JournalConfig { max_entities: 2, ..Default::default() });
        j.put("m", "a", json!({})).unwrap();
        j.put("m", "b", json!({})).unwrap();
        assert_eq!(j.put("m", "c", json!({})).unwrap_err().kind, "overloaded");
        j.put("m", "a", json!({"replaced": true})).unwrap(); // replacing an entity is not a new one
        let cfg = JournalConfig { max_entity_bytes: 1024, ..Default::default() };
        let mut j = ItemJournal::new("e1", cfg);
        assert_eq!(j.put("m", "big", json!({"text": "x".repeat(70 * 1024)})).unwrap_err().kind, "too_large");
        j.put("m", "a", json!({"text": "x".repeat(600)})).unwrap();
        assert_eq!(j.put("m", "b", json!({"text": "x".repeat(600)})).unwrap_err().kind, "overloaded");
        assert_eq!(j.append_text("m", "a", "/text", &"y".repeat(600)).unwrap_err().kind, "overloaded");
        assert_eq!(j.append_text("m", "a", "/missing", "y").unwrap_err().kind, "invalid");
        assert_eq!(j.append_text("m", "zz", "/text", "y").unwrap_err().kind, "not_found");
        assert_eq!(j.seq(), 1, "rejected mutations emit nothing");
        // Retention keeps at least the newest record and bounds the rest by bytes.
        let mut j = ItemJournal::new("e1", JournalConfig { retain_bytes: 2048, ..Default::default() });
        for i in 0..200 { j.put("m", &format!("i{}", i % 5), json!({"text": "z".repeat(100)})).unwrap(); }
        assert!(j.retained_bytes() <= 2048);
        assert!(j.first_retained() > 1);
    }

    #[test]
    fn appends_never_grow_an_entity_past_one_snapshot_record() {
        let mut j = ItemJournal::new("e1", JournalConfig::default());
        j.put("message", "m", json!({"text": ""})).unwrap();
        let chunk = "x".repeat(1000);
        let mut refused = None;
        for n in 0..100 {
            if let Err(e) = j.append_text("message", "m", "/text", &chunk) { refused = Some((n, e.kind)); break; }
        }
        let (n, kind) = refused.expect("small appends must not grow an entity without bound");
        assert_eq!(kind, "too_large");
        assert!(n > 50 && n < 66, "refused at {n} KiB");
        // Every line of a snapshot, the entity's whole put included, stays within 64 KiB.
        assert!(j.snapshot().split(|b| *b == b'\n').all(|l| l.len() + 1 <= 64 * 1024));
    }

    #[test]
    fn coverage_is_declared_and_pruning_marks_the_snapshot_tail() {
        let mut j = ItemJournal::new("e1", JournalConfig::default());
        j.put("state", "current", json!({"state": "idle"})).unwrap();
        j.put("message", "old", json!({"text": "a"})).unwrap();
        let begin = |j: &ItemJournal| -> Value { serde_json::from_slice(j.snapshot().split(|b| *b == b'\n').next().unwrap()).unwrap() };
        assert_eq!(begin(&j)["coverage"], "all_retained");
        let (mut live, mut salt) = (ItemView::new(), 3u64);
        let mut c = j.open(None);
        drain(&mut c, &j, &mut live, &mut salt);
        assert!(j.prune("message", "old"));
        assert_eq!(begin(&j)["coverage"], "tail");
        assert!(j.entity("message", "old").is_none());
        // A live viewer keeps what it has (no remove was emitted); a new viewer gets the tail without it.
        assert!(live.entity("message", "old").is_some());
        let mut fresh = ItemView::new();
        fresh.feed(&j.snapshot());
        assert!(fresh.entity("message", "old").is_none() && fresh.entity("state", "current").is_some());
        j.set_coverage(Coverage::StateOnly);
        assert_eq!(begin(&j)["coverage"], "state_only");
        j.new_epoch("e2");
        assert_eq!(j.coverage(), Coverage::AllRetained);
    }

    #[test]
    fn a_viewer_counts_duplicates_gaps_and_stale_appends() {
        let mut j = ItemJournal::new("e1", JournalConfig::default());
        j.put("m", "a", json!({"text": ""})).unwrap();
        j.append_text("m", "a", "/text", "1").unwrap();
        j.append_text("m", "a", "/text", "2").unwrap();
        let lines: Vec<Vec<u8>> = j.log.iter().map(|(_, l)| l.to_vec()).collect();
        let mut v = ItemView::new();
        v.feed(&ItemJournal::new("e1", JournalConfig::default()).snapshot());
        v.feed(&lines[0]);
        v.feed(&lines[0]);
        assert_eq!(v.errors.duplicate, 1);
        v.feed(&lines[2]);
        assert_eq!((v.errors.missing, v.errors.stale_base), (1, 1));
    }

    impl Next {
        fn into_data(self) -> Vec<u8> { match self { Next::Data(d) => d, other => panic!("expected data, got {other:?}") } }
    }
}
