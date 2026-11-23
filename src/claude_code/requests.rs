//! Native permission requests. No automatic approval, persistent grants or guessed consent cards.
//! Protocol: Claude Agent SDK 0.3.329 SDKControlPermissionRequest/PermissionResult.
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::hash::{DefaultHasher, Hasher};
use serde_json::{Value, json};

pub const MAX_PENDING: usize = 16;
pub const REQUEST_BYTES: usize = 12 * 1024;
const RECENT: usize = 256;

pub struct Change { pub id: String, pub value: Value }
#[derive(Default)]
pub struct Incoming { pub changes: Vec<Change>, pub outgoing: Vec<Value> }
#[derive(Debug)]
pub struct Refusal { pub kind: &'static str, pub message: &'static str }
struct Pending { native: String, request: Value, value: Value, claimed: bool, user_cancelled: bool }
#[derive(Default)]
pub struct Requests {
    serial: u64,
    pending: BTreeMap<String, Pending>,
    native: HashMap<String, String>,
    recent: VecDeque<(String, u64)>,
}
fn error_response(id: &str) -> Value {
    json!({"type":"control_response","response":{"subtype":"error","request_id":id,
        "error":"vqro cannot faithfully represent this native request"}})
}
fn refusal(kind: &'static str, message: &'static str) -> Refusal { Refusal { kind, message } }
// This bounded cache only suppresses duplicate unrepresentable cancellations, never approves input.
fn fingerprint(value: &Value) -> u64 {
    let mut hash = DefaultHasher::new();
    hash.write(&serde_json::to_vec(value).expect("JSON value serializes"));
    hash.finish()
}

impl Requests {
    pub fn len(&self) -> usize { self.pending.values().filter(|p| p.value["status"] == "pending").count() }
    pub fn is_empty(&self) -> bool { self.len() == 0 }
    pub fn has_unconfirmed(&self) -> bool { self.pending.values().any(|p| p.value["status"] != "pending" && p.value["native"]["waiting"] != "not_waiting") }
    pub fn summaries(&self) -> Vec<Value> {
        self.pending.iter().filter(|(_, p)| p.value["status"] == "pending").map(|(id, p)| json!({"request":id,"revision":1,"method":"form",
            "title":p.value["title"],"status":"pending","incomplete":false})).collect()
    }
    fn remember(&mut self, native: &str, request: &Value) {
        self.recent.push_back((native.into(), fingerprint(request)));
        while self.recent.len() > RECENT { self.recent.pop_front(); }
    }
    pub fn observe(&mut self, frame: &Value, child: Option<&str>) -> Incoming {
        let mut result = Incoming::default();
        let Some(native) = frame["request_id"].as_str() else { return result; };
        if frame["type"] == "control_cancel_request" {
            if let Some(id) = self.native.remove(native) {
                if let Some(mut pending) = self.pending.remove(&id) {
                    if pending.value["status"] == "pending" { pending.value["status"] = json!("cancelled"); }
                    pending.value["native"]["waiting"] = json!("not_waiting");
                    result.changes.push(Change { id, value: pending.value });
                }
            }
            return result;
        }
        if frame["type"] != "control_request" { return result; }
        // A still-pending live question and its initialize snapshot are one request. A reused id
        // after settlement gets a NEW standard id; never bind it to an old approved input.
        if let Some(old_id) = self.native.get(native).cloned() {
            if let Some(old) = self.pending.get(&old_id) {
                if old.value["status"] == "pending" && old.request == frame["request"] { return result; }
            }
            if let Some(mut old) = self.pending.remove(&old_id) {
                old.value["status"] = json!("expired");
                result.changes.push(Change { id: old_id, value: old.value });
            }
            self.native.remove(native);
        }
        let fingerprint = fingerprint(&frame["request"]);
        if self.recent.iter().any(|(id, hash)| id == native && *hash == fingerprint) { return result; }
        self.serial += 1;
        let id = format!("q{}", self.serial);
        let request = &frame["request"];
        let title = request["title"].as_str().or_else(|| request["tool_name"].as_str()).unwrap_or("Native request");
        let mut value = json!({"request":id,"revision":1,"method":"form","title":title,
            "params":{"tool":request["tool_name"],"input":request["input"]},
            "scope":{"operation":format!("permission-{id}")},"status":"pending","incomplete":false,
            "native":{"request_id":native,"tool_use_id":request["tool_use_id"],"waiting":"unknown"}});
        if let Some(child) = child { value["child"] = json!(child); }
        // Preserve every consent restriction and the full warning; never turn it into one-tap allow.
        for field in ["title", "description", "decision_reason", "decision_reason_type", "classifier_approvable",
            "suppress_always_allow_rule", "default_to_no", "requires_user_interaction", "matched_ask_rule", "agent_id"] {
            if let Some(field_value) = request.get(field) { value["native"][field] = field_value.clone(); }
        }
        for field in ["description", "decision_reason", "decision_reason_type", "matched_ask_rule", "default_to_no"] {
            if let Some(field_value) = request.get(field) { value["params"][field] = field_value.clone(); }
        }
        value["schema"] = json!({"type":"object","required":["behavior"],"additionalProperties":false,
            "properties":{"behavior":{"type":"string","enum":["deny","allow"],"default":"deny"},
            "updatedInput":{"type":"object"},"message":{"type":"string"}}});
        let restrictions_valid = ["requires_user_interaction", "default_to_no", "suppress_always_allow_rule", "classifier_approvable"]
            .iter().all(|field| request.get(*field).is_none_or(Value::is_boolean));
        let supported = restrictions_valid && request["subtype"] == "can_use_tool" && request["tool_name"].is_string()
            && request["tool_use_id"].is_string() && request["input"].is_object()
            && !request.get("requires_user_interaction").is_some_and(|v| v != false)
            && native.len() <= 4096 && title.len() <= 1024 && self.pending.len() < MAX_PENDING
            && crate::limits::encoded_bytes(&value) <= REQUEST_BYTES;
        if !supported {
            // No partial input/consent can become approvable. Retain only bounded identity and reason.
            value = json!({"request":id,"revision":1,"method":"_unrepresentable","title":"Native interaction is not representable",
                "params":{},"scope":{"operation":format!("permission-{id}")},"status":"cancelled","incomplete":true,
                "native":{"waiting":"unknown"}});
            if let Some(requires) = request["requires_user_interaction"].as_bool() { value["native"]["requires_user_interaction"] = json!(requires); }
            if native.len() <= 4096 {
                value["native"]["request_id"] = json!(native);
                self.remember(native, request);
                // An undeclared user dialog MUST NOT be answered as a user dismissal. The native CLI
                // discards error replies for it too; local cancellation is not native settlement.
                if request["subtype"] != "request_user_dialog" { result.outgoing.push(error_response(native)); }
            }
        } else {
            self.native.insert(native.into(), id.clone());
            self.pending.insert(id.clone(), Pending { native: native.into(), request: request.clone(), value: value.clone(), claimed: false, user_cancelled: false });
        }
        result.changes.push(Change { id, value });
        result
    }
    /// Validate entirely before claiming. Native tool-input validation remains native, not emulated.
    pub fn answer(&mut self, id: &str, revision: u64, response: &Value) -> Result<Value, Refusal> {
        let pending = self.pending.get_mut(id).ok_or_else(|| refusal("stale", "request is no longer pending"))?;
        if revision != 1 || pending.claimed || pending.value["status"] != "pending" { return Err(refusal("stale", "request revision already claimed")); }
        let fields = response.as_object().ok_or_else(|| refusal("invalid", "response must be an object"))?;
        if fields.keys().any(|k| !matches!(k.as_str(), "action" | "content")) {
            return Err(refusal("invalid", "undeclared response field"));
        }
        if response["action"] != "accept" && fields.contains_key("content") {
            return Err(refusal("invalid", "only an accepted form has content"));
        }
        let answer = match response["action"].as_str() {
            Some("cancel") => json!({"subtype":"error","request_id":pending.native,"error":"vqro user cancelled the request"}),
            Some("decline") => json!({"subtype":"success","request_id":pending.native,
                "response":{"behavior":"deny","message":"Denied by user"}}),
            Some("accept") => {
                let content = response["content"].as_object().ok_or_else(|| refusal("invalid", "form content must be an object"))?;
                if content.keys().any(|k| !matches!(k.as_str(), "behavior" | "updatedInput" | "message"))
                    || crate::limits::encoded_bytes(&response["content"]) > REQUEST_BYTES {
                    return Err(refusal("invalid", "form has undeclared or oversized fields"));
                }
                let result = match response["content"]["behavior"].as_str() {
                    Some("allow") => {
                        if content.contains_key("message") { return Err(refusal("invalid", "allow has no denial message")); }
                        let input = content.get("updatedInput").unwrap_or(&pending.request["input"]);
                        if !input.is_object() { return Err(refusal("invalid", "updatedInput must be an object")); }
                        json!({"behavior":"allow","updatedInput":input})
                    }
                    Some("deny") => {
                        if content.contains_key("updatedInput") { return Err(refusal("invalid", "deny has no updatedInput")); }
                        let message = content.get("message").map(|m| m.as_str().filter(|s| s.len() <= 2048))
                            .unwrap_or(Some("Denied by user")).ok_or_else(|| refusal("invalid", "denial message must be bounded text"))?;
                        json!({"behavior":"deny","message":message})
                    }
                    _ => return Err(refusal("invalid", "behavior must be allow or deny")),
                };
                json!({"subtype":"success","request_id":pending.native,"response":result})
            }
            _ => return Err(refusal("invalid", "unknown form action")),
        };
        pending.claimed = true;
        pending.user_cancelled = response["action"] == "cancel";
        Ok(json!({"type":"control_response","response":answer}))
    }
    /// Only a definite pre-write refusal may release the local claim; a withdrawal wins this race.
    pub fn not_written(&mut self, id: &str) {
        if let Some(pending) = self.pending.get_mut(id) { pending.claimed = false; }
    }
    pub fn dispatched(&mut self, id: &str, known_written: bool) -> Option<Change> {
        let pending = self.pending.get_mut(id)?;
        pending.value["status"] = json!(if pending.user_cancelled { "cancelled" } else { "answered" });
        pending.value["native"]["response"] = json!(if known_written { "dispatched" } else { "unknown" });
        // There is no response ACK. Keep native waiting unknown until a native withdrawal/tool event.
        Some(Change { id: id.into(), value: pending.value.clone() })
    }
    pub fn tool_started(&mut self, tool: &str) -> Vec<Change> {
        let ids: Vec<_> = self.pending.iter().filter(|(_, p)| p.value["native"]["tool_use_id"] == tool)
            .map(|(id, _)| id.clone()).collect();
        ids.into_iter().filter_map(|id| {
            let mut pending = self.pending.remove(&id)?;
            self.native.remove(&pending.native);
            pending.value["status"] = json!("answered");
            pending.value["native"]["waiting"] = json!("not_waiting");
            Some(Change { id, value: pending.value })
        }).collect()
    }
    pub fn expire(&mut self) -> Vec<Change> {
        self.native.clear();
        self.recent.clear();
        self.pending.split_off("").into_iter().map(|(id, mut pending)| {
            pending.value["status"] = json!("expired");
            pending.value["native"]["waiting"] = json!("unknown");
            Change { id, value: pending.value }
        }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn permission() -> Value { json!({"type":"control_request","request_id":"native-p1","request":{
        "subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"tool1","input":{"command":"echo safe"},
        "default_to_no":true,"suppress_always_allow_rule":true,"decision_reason":"Exact warning"}}) }
    #[test]
    fn permission_keeps_restrictions_and_invalid_answer_does_not_claim() {
        let mut book = Requests::default(); let incoming = book.observe(&permission(), None);
        let request = &incoming.changes[0];
        assert_eq!(request.value["native"]["default_to_no"], true);
        assert_eq!(request.value["native"]["decision_reason"], "Exact warning");
        assert!(book.answer(&request.id, 1, &json!({"action":"accept","content":{"behavior":"allow","updatedInput":[]}})).is_err());
        let answer = book.answer(&request.id, 1, &json!({"action":"accept","content":{"behavior":"allow","updatedInput":{"command":"echo changed"}}})).unwrap();
        assert_eq!(answer["response"]["response"]["updatedInput"]["command"], "echo changed");
        assert!(book.answer(&request.id, 1, &json!({"action":"decline"})).is_err());
    }
    #[test]
    fn deny_and_cancel_are_distinct_native_replies() {
        for (action, subtype) in [("decline", "success"), ("cancel", "error")] {
            let mut book = Requests::default(); let id = book.observe(&permission(), None).changes.remove(0).id;
            let answer = book.answer(&id, 1, &json!({"action":action})).unwrap();
            assert_eq!(answer["response"]["subtype"], subtype);
        }
    }
    #[test]
    fn native_user_interaction_is_never_flattened_into_an_approve_form() {
        let mut book = Requests::default(); let mut frame = permission();
        frame["request"]["requires_user_interaction"] = json!(true);
        let incoming = book.observe(&frame, None);
        assert!(book.is_empty()); assert_eq!(incoming.changes[0].value["incomplete"], true);
        assert_eq!(incoming.outgoing[0]["response"]["subtype"], "error");
        assert!(book.observe(&frame, None).outgoing.is_empty());
    }
    #[test]
    fn snapshot_duplicate_withdrawal_and_id_reuse_do_not_reuse_standard_identity() {
        let mut book = Requests::default(); let frame = permission();
        let first = book.observe(&frame, Some("child1")).changes.remove(0);
        assert_eq!(first.value["child"], "child1"); assert!(book.observe(&frame, None).changes.is_empty());
        let withdrawn = book.observe(&json!({"type":"control_cancel_request","request_id":"native-p1"}), None);
        assert_eq!(withdrawn.changes[0].value["status"], "cancelled");
        assert!(book.answer(&first.id, 1, &json!({"action":"decline"})).is_err());
        let reused = book.observe(&frame, None).changes.remove(0);
        assert_ne!(first.id, reused.id);
    }
    #[test]
    fn dispatched_answer_does_not_prove_native_is_no_longer_waiting() {
        let mut book = Requests::default(); let id = book.observe(&permission(), None).changes.remove(0).id;
        book.answer(&id, 1, &json!({"action":"decline"})).unwrap();
        book.dispatched(&id, true).unwrap();
        assert!(book.has_unconfirmed()); assert!(book.summaries().is_empty());
        let done = book.tool_started("tool1");
        assert_eq!(done[0].value["native"]["waiting"], "not_waiting");
        assert!(!book.has_unconfirmed());
    }
    #[test]
    fn oversized_native_input_is_omitted_not_shortened_or_approvable() {
        let mut book = Requests::default(); let mut frame = permission();
        frame["request"]["input"]["command"] = json!("x".repeat(REQUEST_BYTES));
        let incoming = book.observe(&frame, None);
        assert_eq!(incoming.changes[0].value["status"], "cancelled");
        assert!(incoming.changes[0].value["params"].as_object().unwrap().is_empty()); assert!(book.is_empty());
    }
    #[test]
    fn unadvertised_user_dialog_is_not_answered_as_user_cancellation() {
        let mut book = Requests::default();
        let result = book.observe(&json!({"type":"control_request","request_id":"dialog1","request":{"subtype":"request_user_dialog","dialog_kind":"new_kind"}}), None);
        assert!(result.outgoing.is_empty()); assert_eq!(result.changes[0].value["native"]["waiting"], "unknown");
    }
}
