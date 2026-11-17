//! Per-subscriber event shaping for `events.subscribe` (opt-in, mechanism only): a type filter, and
//! partial resource events that carry only what changed. See `docs/spec/core-protocol.md`, Events.
//!
//! Why: at hundreds of agents, resource status changes dominate event volume (in the scale harness,
//! about 80% of event bytes), and each one repeats the whole resource record. A client on a slow
//! link, or an extension that never looks at resources, pays for all of it.

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Deserialize, Default)]
pub struct SubscribeOptions {
    pub from: Option<u64>,
    pub epoch: Option<String>,
    /// Only these event types (`missed` always arrives). Absent: every type. Unknown names are allowed.
    pub types: Option<Vec<String>>,
    /// `resource` events after the first one per resource carry only the id and changed fields,
    /// marked `"partial": true`.
    #[serde(default)]
    pub partial_resources: bool,
}

pub struct EventFilter {
    types: Option<HashSet<String>>,
    partial: bool,
    /// Last full record sent per resource id: the base the client's copy is merged onto.
    sent: HashMap<String, Map<String, Value>>,
}

impl EventFilter {
    pub fn new(opts: &SubscribeOptions) -> EventFilter {
        EventFilter { types: opts.types.as_ref().map(|t| t.iter().cloned().collect()), partial: opts.partial_resources, sent: HashMap::new() }
    }

    /// After `missed` the client reloads a snapshot, so no earlier base is valid: the next event
    /// for each resource is sent in full again.
    pub fn reset(&mut self) {
        self.sent.clear();
    }

    /// The event as this subscriber should get it, or None if filtered out.
    pub fn shape(&mut self, ev: Value) -> Option<Value> {
        let ty = ev["type"].as_str().unwrap_or("");
        // Cache lifecycle is independent of whether this subscriber wants the notification.
        if ty == "resource.removed" {
            if let Some(id) = ev["data"]["id"].as_str() { self.sent.remove(id); }
        }
        if self.types.as_ref().is_some_and(|t| !t.contains(ty)) {
            return None;
        }
        if !self.partial {
            return Some(ev);
        }
        match ty {
            "resource" => {
                let (Some(id), Some(data)) = (ev["data"]["id"].as_str().map(String::from), ev["data"].as_object().cloned()) else { return Some(ev) };
                let Some(prev) = self.sent.insert(id.clone(), data.clone()) else { return Some(ev) };
                let mut diff = Map::new();
                diff.insert("id".into(), Value::String(id));
                for (k, v) in &data {
                    if prev.get(k) != Some(v) {
                        diff.insert(k.clone(), v.clone());
                    }
                }
                for k in prev.keys().filter(|k| !data.contains_key(*k)) {
                    diff.insert(k.clone(), Value::Null);
                }
                let mut ev = ev;
                ev["data"] = Value::Object(diff);
                ev["partial"] = Value::Bool(true);
                Some(ev)
            }
            "resource.removed" => Some(ev),
            _ => Some(ev),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn res(id: &str, status: &str) -> Value {
        json!({"epoch": "e", "seq": 1, "type": "resource", "data": {"id": id, "argv": ["sh"], "status": status}})
    }

    #[test]
    fn partial_after_the_first_full_again_after_reset_or_removal() {
        let mut f = EventFilter::new(&SubscribeOptions { partial_resources: true, ..Default::default() });
        assert_eq!(f.shape(res("r1", "a")).unwrap(), res("r1", "a"));
        let p = f.shape(res("r1", "b")).unwrap();
        assert_eq!(p["partial"], true);
        assert_eq!(p["data"], json!({"id": "r1", "status": "b"}));
        f.reset();
        assert_eq!(f.shape(res("r1", "c")).unwrap(), res("r1", "c"), "full after missed");
        f.shape(json!({"type": "resource.removed", "data": {"id": "r1"}})).unwrap();
        assert_eq!(f.shape(res("r1", "d")).unwrap(), res("r1", "d"), "full after removal");
    }

    #[test]
    fn filtered_removal_still_retires_the_delta_base() {
        let mut f = EventFilter::new(&SubscribeOptions {
            types: Some(vec!["resource".into()]), partial_resources: true, ..Default::default()
        });
        for i in 0..1000 {
            let id = format!("r{i}");
            f.shape(res(&id, "old"));
            assert!(f.shape(json!({"type":"resource.removed", "data":{"id":id}})).is_none());
        }
        assert!(f.sent.is_empty(), "filtered lifecycle events must still evict internal cache state");
        assert!(f.shape(res("r0", "new")).unwrap().get("partial").is_none());
    }

    #[test]
    fn a_dropped_field_is_sent_as_null() {
        let mut f = EventFilter::new(&SubscribeOptions { partial_resources: true, ..Default::default() });
        f.shape(res("r1", "a"));
        let p = f.shape(json!({"type": "resource", "data": {"id": "r1", "status": "a"}})).unwrap();
        assert_eq!(p["data"], json!({"id": "r1", "argv": null}));
    }

    #[test]
    fn type_filter_keeps_only_listed_types() {
        let mut f = EventFilter::new(&SubscribeOptions { types: Some(vec!["entries".into()]), ..Default::default() });
        assert!(f.shape(res("r1", "a")).is_none());
        assert!(f.shape(json!({"type": "entries", "data": {}})).is_some());
    }
}
