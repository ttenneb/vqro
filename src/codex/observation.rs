//! Codex projection policy around the shared SDK producer, not another item reducer.
//! The SDK alone owns wire records, revisions, snapshot cuts, cursors and retention gaps.
use super::{runtime::Observer, Change, ENTITY_BYTES};
use crate::provider_client::items::{Coverage, ItemCursor, ItemJournal, JournalConfig, Next};
use crate::provider_client::{OpenRequest, ProviderError, ProviderStream, StreamCommand};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

pub const RETAIN_BYTES: usize = 128 * 1024;
pub const ENTITY_TOTAL: usize = 512 * 1024;
pub const ENTITIES: usize = 512;
pub const VIEWERS: usize = 4;
pub const SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
pub const POST_CUT_BYTES: usize = 1024 * 1024;
const CONTROL_RESERVE: usize = 64 * 1024;
const CONTROL_ENTITIES: usize = 64;
const CHUNK: usize = 8 * 1024;
type Key = (String, String);

struct Entry {
    bytes: usize,
    control: bool,
    protected: bool,
}
struct Projection {
    journal: ItemJournal,
    entries: BTreeMap<Key, Entry>,
    order: VecDeque<Key>,
    normal_bytes: usize,
    normal_count: usize,
    position: u64,
    incomplete: bool,
}

fn control(kind: &str, value: &Value) -> bool {
    matches!(kind, "state" | "session") || (kind == "request" && value["status"] == "pending")
}
fn protected(kind: &str, value: &Value) -> bool {
    control(kind, value)
        || (kind == "run" && value["status"] == "inProgress")
        || (kind == "message" && value["final"] == false)
        || (kind == "tool" && matches!(value["status"].as_str(), Some("pending" | "running")))
        || (kind == "receipt" && value["stage"] == "dispatched")
}
fn timeline(kind: &str) -> bool {
    matches!(
        kind,
        "message" | "tool" | "plan" | "compaction" | "_codex_item"
    )
}
fn bytes(value: &Value) -> usize {
    crate::limits::encoded_bytes(value)
}

impl Projection {
    fn new(epoch: String) -> Self {
        let mut journal = ItemJournal::new(
            epoch,
            JournalConfig {
                retain_bytes: RETAIN_BYTES,
                max_record_bytes: ENTITY_BYTES,
                max_entities: ENTITIES,
                max_entity_bytes: ENTITY_TOTAL,
                snapshot_buffer_bytes: POST_CUT_BYTES,
            },
        );
        journal.set_coverage(Coverage::Tail);
        Self {
            journal,
            entries: BTreeMap::new(),
            order: VecDeque::new(),
            normal_bytes: 0,
            normal_count: 0,
            position: 0,
            incomplete: false,
        }
    }
    fn reset(&mut self, epoch: &str) {
        if self.journal.epoch() == epoch {
            return;
        }
        self.journal.new_epoch(epoch);
        self.journal.set_coverage(Coverage::Tail);
        self.entries.clear();
        self.order.clear();
        self.normal_bytes = 0;
        self.normal_count = 0;
        self.position = 0;
        self.incomplete = false;
    }
    fn remove(&mut self, key: &Key) {
        if let Some(entry) = self.entries.remove(key) {
            if !entry.control {
                self.normal_bytes -= entry.bytes;
                self.normal_count -= 1;
            }
            if let Some(at) = self.order.iter().position(|k| k == key) {
                self.order.remove(at);
            }
            if self.journal.remove(&key.0, &key.1).is_err() {
                self.incomplete = true;
            }
        }
    }
    fn room(&mut self, key: &Key, new_bytes: usize, is_control: bool) -> bool {
        for _ in 0..=ENTITIES {
            let old = self.entries.get(key);
            let old_bytes = old.map(|e| e.bytes).unwrap_or(0);
            let normal_bytes = self.normal_bytes
                - old.filter(|e| !e.control).map(|e| e.bytes).unwrap_or(0)
                + if is_control { 0 } else { new_bytes };
            let normal_count = self.normal_count - usize::from(old.is_some_and(|e| !e.control))
                + usize::from(!is_control);
            if self.journal.entity_bytes() - old_bytes + new_bytes <= ENTITY_TOTAL
                && self.entries.len() + usize::from(old.is_none()) <= ENTITIES
                && (is_control
                    || (normal_bytes <= ENTITY_TOTAL - CONTROL_RESERVE
                        && normal_count <= ENTITIES - CONTROL_ENTITIES))
            {
                return true;
            }
            let evict = self
                .order
                .iter()
                .find(|candidate| {
                    *candidate != key && self.entries.get(*candidate).is_some_and(|e| !e.protected)
                })
                .cloned();
            if let Some(evict) = evict {
                self.remove(&evict);
            } else {
                return false;
            }
        }
        false
    }
    fn account(&mut self, key: Key, value: &Value) {
        if let Some(old) = self.entries.remove(&key) {
            if !old.control {
                self.normal_bytes -= old.bytes;
                self.normal_count -= 1;
            }
        } else {
            self.order.push_back(key.clone());
        }
        let entry = Entry {
            bytes: bytes(value),
            control: control(&key.0, value),
            protected: protected(&key.0, value),
        };
        if !entry.control {
            self.normal_bytes += entry.bytes;
            self.normal_count += 1;
        }
        self.entries.insert(key, entry);
    }
    fn incomplete(&mut self) {
        self.incomplete = true;
        if let Some((_, value)) = self.journal.entity("state", "current") {
            let mut value = value.clone();
            value["items_incomplete"] = json!(true);
            if self.journal.put("state", "current", value.clone()).is_ok() {
                self.account(("state".into(), "current".into()), &value);
            }
        }
    }
    fn put(&mut self, kind: &str, id: &str, mut value: Value, create: bool) -> bool {
        let old = self.journal.entity(kind, id).map(|(_, v)| v.clone());
        if old.is_none() && !create {
            self.incomplete();
            return false;
        }
        if value["incomplete"] == true {
            if let Some(old) = &old {
                let mut retained = old.clone();
                retained["incomplete"] = json!(true);
                if value["final"].is_boolean() {
                    retained["final"] = value["final"].clone();
                }
                value = retained;
            }
            self.incomplete();
        }
        if timeline(kind) {
            if let Some(position) = old.as_ref().and_then(|v| v.get("position")) {
                value["position"] = position.clone();
            } else {
                self.position += 1;
                value["position"] = json!(self.position);
            }
        }
        if kind == "state" && self.incomplete {
            value["items_incomplete"] = json!(true);
        }
        if bytes(&value) > ENTITY_BYTES - 1024 {
            let mut short = json!({"incomplete":true,"position":value["position"]});
            if matches!(kind, "message" | "tool") {
                short["content"] = json!([]);
            }
            if kind == "tool" {
                short["title"] = json!("Codex tool (details truncated)");
            }
            for field in ["role", "status", "final"] {
                if let Some(v) = value.get(field) {
                    short[field] = v.clone();
                }
            }
            value = short;
            self.incomplete();
        }
        let key = (kind.into(), id.into());
        let size = bytes(&value);
        if !self.room(&key, size, control(kind, &value)) {
            // A settled request need not displace active transcript/state merely to retain its
            // history; removing the old pending entity is an explicit tail eviction.
            if kind == "request" && value["status"] != "pending" {
                self.remove(&key);
                return false;
            }
            self.incomplete();
            return false;
        }
        match self.journal.put(kind, id, value.clone()) {
            Ok(_) => {
                self.account(key, &value);
                true
            }
            Err(_) => {
                self.incomplete();
                false
            }
        }
    }
    fn append(&mut self, change: Change, field: String, text: String) {
        if self.journal.entity(&change.kind, &change.id).is_none()
            && !self.put(&change.kind, &change.id, change.value, true)
        {
            return;
        }
        let mut at = 0;
        while at < text.len() {
            let mut end = (at + CHUNK).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            let chunk = &text[at..end];
            let Some((_, existing)) = self.journal.entity(&change.kind, &change.id) else {
                return;
            };
            if existing["incomplete"] == true {
                return;
            }
            let mut candidate = existing.clone();
            let Some(Value::String(target)) = candidate.pointer_mut(&field) else {
                self.incomplete();
                return;
            };
            target.push_str(chunk);
            if bytes(&candidate) > ENTITY_BYTES - 1024 {
                candidate = existing.clone();
                candidate["incomplete"] = json!(true);
                self.put(&change.kind, &change.id, candidate, true);
                return;
            }
            let key = (change.kind.clone(), change.id.clone());
            if !self.room(&key, bytes(&candidate), control(&change.kind, &candidate)) {
                self.incomplete();
                return;
            }
            if self
                .journal
                .append_text(&change.kind, &change.id, &field, chunk)
                .is_err()
            {
                self.incomplete();
                return;
            }
            self.account(key, &candidate);
            at = end;
        }
    }
    fn apply(&mut self, changes: Vec<Change>) {
        for mut change in changes {
            if let Some((field, text)) = change.append.take() {
                self.append(change, field, text);
            } else {
                self.put(&change.kind, &change.id, change.value, change.create);
            }
        }
    }
    fn snapshot_charge(&self) -> usize {
        // Reserve retained cut values plus worst-case escaped 128-byte kind/id framing and indexes.
        // C3 shares values until mutation; the reservation also covers versions kept by a slow cut.
        self.journal.entity_bytes() + self.journal.len() * 2048 + 1024
    }
}

struct Slot {
    projection: Mutex<Projection>,
    changed: Notify,
    viewers: Arc<Semaphore>,
}
pub struct Observation {
    slots: Vec<Slot>,
    snapshots: Arc<Semaphore>,
    incarnation: String,
}

impl Observation {
    pub fn new(count: usize, incarnation: String) -> Arc<Self> {
        Arc::new(Self {
            slots: (0..count)
                .map(|i| Slot {
                    projection: Mutex::new(Projection::new(format!("{incarnation}:{i}:0"))),
                    changed: Notify::new(),
                    viewers: Arc::new(Semaphore::new(VIEWERS)),
                })
                .collect(),
            snapshots: Arc::new(Semaphore::new(SNAPSHOT_BYTES)),
            incarnation,
        })
    }
    pub fn observer(self: &Arc<Self>) -> Observer {
        let this = self.clone();
        Arc::new(move |index, epoch, changes, reset| {
            let Some(slot) = this.slots.get(index) else {
                return;
            };
            {
                let mut projection = slot.projection.lock().unwrap();
                if reset {
                    projection.reset(epoch);
                }
                projection.apply(changes);
            }
            slot.changed.notify_waiters();
        })
    }
    fn snapshot_permit(
        &self,
        projection: &Projection,
    ) -> Result<OwnedSemaphorePermit, ProviderError> {
        self.snapshots
            .clone()
            .try_acquire_many_owned(projection.snapshot_charge().min(u32::MAX as usize) as u32)
            .map_err(|_| {
                ProviderError::new(
                    "overloaded",
                    "provider snapshot buffer budget full; no snapshot allocated",
                )
            })
    }
    pub fn open(self: &Arc<Self>, request: OpenRequest) {
        let index = request
            .stream
            .r#ref
            .strip_prefix("endpoint-")
            .and_then(|s| s.parse::<usize>().ok())
            .and_then(|i| i.checked_sub(1))
            .filter(|i| *i < self.slots.len());
        let (Some(index), Some(items)) = (index, request.items.clone()) else {
            request.respond(Err(ProviderError::new(
                "unsupported",
                "only item streams on known endpoints are declared",
            )));
            return;
        };
        if request.stream.incarnation != self.incarnation {
            request.respond(Err(ProviderError::new(
                "stale",
                "endpoint incarnation changed",
            )));
            return;
        }
        let viewer = match self.slots[index].viewers.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                request.respond(Err(ProviderError::new(
                    "overloaded",
                    "endpoint viewer capacity full",
                )));
                return;
            }
        };
        let (cursor, snapshot) = {
            let mut projection = self.slots[index].projection.lock().unwrap();
            // A conservative snapshot reservation also covers opens that can continue cheaply.
            let snapshot = match self.snapshot_permit(&projection) {
                Ok(p) => p,
                Err(error) => {
                    request.respond(Err(error));
                    return;
                }
            };
            let cursor = projection.journal.open(items.get("after"));
            let snapshot = if cursor.reset() { Some(snapshot) } else { None };
            (cursor, snapshot)
        };
        let Some(stream) = request.respond_items(cursor.reset(), cursor.epoch()) else {
            return;
        };
        let this = self.clone();
        tokio::spawn(async move {
            this.viewer(index, stream, cursor, snapshot, viewer).await;
        });
    }
    fn advance(
        &self,
        index: usize,
        cursor: &mut ItemCursor,
        snapshot: &mut Option<OwnedSemaphorePermit>,
    ) -> Result<(Next, bool), ProviderError> {
        let mut projection = self.slots[index].projection.lock().unwrap();
        // An epoch restart may replace an unfinished cut with a larger one. Fund the new
        // cut before allocating it, keeping the old lease until `next` drops the old cut.
        let replacement = if cursor.epoch() != projection.journal.epoch() {
            Some(self.snapshot_permit(&projection)?)
        } else {
            None
        };
        let next = cursor.next(&mut projection.journal, ENTITY_BYTES);
        if let Some(replacement) = replacement {
            *snapshot = Some(replacement);
        }
        Ok((next, cursor.snapshot_pending()))
    }
    async fn viewer(
        self: Arc<Self>,
        index: usize,
        mut stream: ProviderStream,
        mut cursor: ItemCursor,
        mut snapshot: Option<OwnedSemaphorePermit>,
        _viewer: OwnedSemaphorePermit,
    ) {
        let run = async {
            loop {
                let slot = &self.slots[index];
                let wake = slot.changed.notified();
                tokio::pin!(wake);
                wake.as_mut().enable();
                let (next, pending) = self.advance(index, &mut cursor, &mut snapshot)?;
                match next {
                    Next::Data(data) => {
                        stream.send(&data).await?;
                        // The final snapshot piece may be credit-blocked: release only after
                        // its send completes, not before, and do not wait for a live-stream Idle.
                        if !pending { drop(snapshot.take()); }
                        continue;
                    }
                    Next::Gap(reason) => {
                        stream.gap(reason).await?;
                        return Ok::<(), ProviderError>(());
                    }
                    Next::Idle => { drop(snapshot.take()); }
                }
                tokio::select! {
                    _ = &mut wake => {},
                    command = stream.next() => match command {
                        Some(StreamCommand::Close) | None => {
                            stream.close(json!({"reason":"closed"})).await?;
                            return Ok(());
                        }
                        Some(StreamCommand::Input(_) | StreamCommand::Resize { .. }) => {
                            stream.close(json!({"reason":"invalid","message":"item streams are output only"})).await?;
                            return Ok(());
                        }
                    }
                }
            }
        }.await;
        if let Err(error) = run {
            let reason = if error.kind == "overloaded" {
                "overloaded"
            } else {
                "closed"
            };
            let _ = stream
                .close(json!({"reason":reason,"message":error.message}))
                .await;
        }
    }
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod tests;
