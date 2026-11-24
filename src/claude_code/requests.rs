//! Native permission requests. No automatic approval, persistent grants or guessed consent cards.
//! Protocol: Claude Agent SDK 0.3.329 SDKControlPermissionRequest/PermissionResult.
use crate::native_process::WriteToken;
use serde_json::{Value, json};

pub struct Outbound {
    pub frame: Value,
    pub token: WriteToken,
}
impl std::ops::Deref for Outbound {
    type Target = Value;
    fn deref(&self) -> &Value {
        &self.frame
    }
}
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::hash::{DefaultHasher, Hasher};

pub const MAX_PENDING: usize = 16;
pub const REQUEST_BYTES: usize = 12 * 1024;
const RECENT: usize = 256;

pub struct Change {
    pub id: String,
    pub value: Value,
}
#[derive(Default)]
pub struct Incoming {
    pub changes: Vec<Change>,
    pub outgoing: Vec<Outbound>,
}
#[derive(Debug)]
pub struct Refusal {
    pub kind: &'static str,
    pub message: &'static str,
}
struct Pending {
    native: String,
    request: Value,
    value: Value,
    claimed: bool,
    user_cancelled: bool,
    token: WriteToken,
}
#[derive(Clone, PartialEq, Eq)]
enum RequestScope {
    Root,
    Child(String),
    Unknown,
}
fn request_scope(child: Option<&str>) -> RequestScope {
    child
        .map(|id| RequestScope::Child(id.into()))
        .unwrap_or(RequestScope::Root)
}
struct Recent {
    id: String,
    hash: u64,
    scope: RequestScope,
}
#[derive(Default)]
pub struct Requests {
    serial: u64,
    pending: BTreeMap<String, Pending>,
    native: HashMap<String, String>,
    recent: VecDeque<Recent>,
    unsupported_tokens: BTreeMap<String, WriteToken>,
    unsupported_tools: BTreeMap<String, (RequestScope, String)>,
}
fn error_response(id: &str) -> Value {
    json!({"type":"control_response","response":{"subtype":"error","request_id":id,
        "error":"vqro cannot faithfully represent this native request"}})
}
fn refusal(kind: &'static str, message: &'static str) -> Refusal {
    Refusal { kind, message }
}
// Bounded unsettled tombstones suppress duplicate cancellation writes, never approve input.
fn fingerprint(value: &Value) -> u64 {
    let mut hash = DefaultHasher::new();
    hash.write(&serde_json::to_vec(value).expect("JSON value serializes"));
    hash.finish()
}

impl Requests {
    pub fn len(&self) -> usize {
        self.pending
            .values()
            .filter(|p| p.value["status"] == "pending")
            .count()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn has_unconfirmed(&self) -> bool {
        !self.recent.is_empty()
            || self.pending.values().any(|p| {
                p.value["status"] != "pending" && p.value["native"]["waiting"] != "not_waiting"
            })
    }
    pub fn root_is_empty(&self) -> bool {
        !self.pending.values().any(|p| {
            p.value["status"] == "pending"
                && p.value["child"].is_null()
                && p.value["native"]["scope_unknown"] != true
        })
    }
    pub fn root_has_unconfirmed(&self) -> bool {
        self.recent.iter().any(|r| r.scope == RequestScope::Root)
            || self.pending.values().any(|p| {
                p.value["child"].is_null()
                    && p.value["native"]["scope_unknown"] != true
                    && p.value["status"] != "pending"
                    && p.value["native"]["waiting"] != "not_waiting"
            })
    }
    pub fn summaries(&self) -> Vec<Value> {
        self.pending
            .iter()
            .filter(|(_, p)| p.value["status"] == "pending")
            .map(|(id, p)| {
                json!({"request":id,"revision":1,"method":"form",
            "title":p.value["title"],"status":"pending","incomplete":false,"child":p.value["child"]})
            })
            .collect()
    }
    fn remember(&mut self, native: &str, request: &Value) -> bool {
        // Never evict an unsettled tombstone and thereby replay an unknown cancellation.
        if self.recent.len() >= RECENT && !self.recent.iter().any(|r| r.id == native) {
            return false;
        }
        self.recent.retain(|r| r.id != native);
        self.recent.push_back(Recent {
            id: native.into(),
            hash: fingerprint(request),
            scope: RequestScope::Root,
        });
        self.unsupported_tools.remove(native);
        if let Some(tool) = request["tool_use_id"].as_str().filter(|s| s.len() <= 4096) {
            self.unsupported_tools
                .insert(native.into(), (RequestScope::Root, tool.into()));
        }
        true
    }
    fn remember_scope(
        &mut self,
        native: &str,
        request: &Value,
        child: Option<&str>,
        unknown: bool,
    ) -> bool {
        if !self.remember(native, request) {
            return false;
        }
        let scope = if unknown {
            RequestScope::Unknown
        } else {
            request_scope(child)
        };
        if let Some(recent) = self.recent.iter_mut().find(|r| r.id == native) {
            recent.scope = scope.clone();
        }
        if let Some((stored, _)) = self.unsupported_tools.get_mut(native) {
            *stored = scope;
        }
        true
    }
    pub fn answer_token(&self, id: &str) -> Option<WriteToken> {
        self.pending.get(id).map(|p| p.token.clone())
    }
    pub fn observe(&mut self, frame: &Value, child: Option<&str>) -> Incoming {
        self.observe_scoped(frame, child, false)
    }
    pub fn observe_unscoped(&mut self, frame: &Value) -> Incoming {
        self.observe_scoped(frame, None, true)
    }
    fn observe_scoped(
        &mut self,
        frame: &Value,
        child: Option<&str>,
        scope_unknown: bool,
    ) -> Incoming {
        let mut result = Incoming::default();
        let Some(native) = frame["request_id"].as_str() else {
            return result;
        };
        if frame["type"] == "control_cancel_request" {
            if let Some(token) = self.unsupported_tokens.remove(native) {
                token.cancel_before_write();
            }
            self.recent.retain(|r| r.id != native);
            self.unsupported_tools.remove(native);
            if let Some(id) = self.native.remove(native) {
                if let Some(mut pending) = self.pending.remove(&id) {
                    pending.token.cancel_before_write();
                    if pending.value["status"] == "pending" {
                        pending.value["status"] = json!("cancelled");
                    }
                    pending.value["native"]["waiting"] = json!("not_waiting");
                    result.changes.push(Change {
                        id,
                        value: pending.value,
                    });
                }
            }
            return result;
        }
        if frame["type"] != "control_request" {
            return result;
        }
        // Re-initialize snapshots may omit producer context. Pending AND RECENT records are
        // unsettled native identities: absent metadata must not erase their known child/unknown scope.
        let retained_recent = if self
            .native
            .get(native)
            .and_then(|id| self.pending.get(id))
            .is_none()
        {
            self.recent
                .iter()
                .find(|r| r.id == native)
                .map(|r| r.scope.clone())
        } else {
            None
        };
        let scope_unknown =
            scope_unknown || (child.is_none() && retained_recent == Some(RequestScope::Unknown));
        let retained_child = if child.is_none() && !scope_unknown {
            self.native
                .get(native)
                .and_then(|id| self.pending.get(id))
                .and_then(|p| p.value["child"].as_str())
                .map(str::to_owned)
                .or_else(|| match retained_recent.as_ref() {
                    Some(RequestScope::Child(id)) => Some(id.clone()),
                    _ => None,
                })
        } else {
            None
        };
        let child = child.or(retained_child.as_deref());
        // A live question and identical initialize snapshots are one incarnation even after a
        // response write has an unknown outcome. Only independent settlement/reuse retires it.
        if let Some(old_id) = self.native.get(native).cloned() {
            if let Some(old) = self.pending.get(&old_id) {
                if old.request == frame["request"]
                    && old.value["child"].as_str() == child
                    && (old.value["native"]["scope_unknown"] == true) == scope_unknown
                {
                    return result;
                }
            }
            if let Some(mut old) = self.pending.remove(&old_id) {
                // Revoke atomically FIRST. The independent writer can commit after a phase
                // pre-read; only post-revocation classification can retain the winner honestly.
                old.token.cancel_before_write();
                let unsettled_claim =
                    old.claimed && (old.value["status"] != "pending" || old.token.write_begun());
                old.value["status"] = json!("expired");
                if unsettled_claim {
                    // A changed snapshot/reused native ID is not an independent settlement of a
                    // possibly-written consent. Do not mint another approvable incarnation.
                    old.value["incomplete"] = json!(true);
                    result.changes.push(Change {
                        id: old_id.clone(),
                        value: old.value.clone(),
                    });
                    self.pending.insert(old_id, old);
                    return result;
                }
                result.changes.push(Change {
                    id: old_id,
                    value: old.value,
                });
            }
            self.native.remove(native);
        }
        let fingerprint = fingerprint(&frame["request"]);
        if self
            .recent
            .iter()
            .any(|r| r.id == native && r.hash == fingerprint)
        {
            return result;
        }
        if let Some(token) = self.unsupported_tokens.get(native) {
            // Automatic error replies have the same commit boundary as human answers. A changed
            // payload is not settlement after WRITING/WRITTEN (including a later write failure).
            // Keep the original token, fingerprint and tool correlation until native withdrawal
            // or independent tool settlement. Only a revoked/unwritten reply can be replaced.
            if !token.cancel_before_write() && token.write_begun() {
                return result;
            }
        }
        self.unsupported_tokens.remove(native);
        self.serial += 1;
        let id = format!("q{}", self.serial);
        let request = &frame["request"];
        let title = request["title"]
            .as_str()
            .or_else(|| request["tool_name"].as_str())
            .unwrap_or("Native request");
        let mut value = json!({"request":id,"revision":1,"method":"form","title":title,
            "params":{"tool":request["tool_name"],"input":request["input"]},
            "scope":{"operation":format!("permission-{id}")},"status":"pending","incomplete":false,
            "native":{"request_id":native,"tool_use_id":request["tool_use_id"],"waiting":"unknown"}});
        if let Some(child) = child {
            value["child"] = json!(child);
        }
        // Preserve every consent restriction and the full warning; never turn it into one-tap allow.
        for field in [
            "title",
            "description",
            "decision_reason",
            "decision_reason_type",
            "classifier_approvable",
            "suppress_always_allow_rule",
            "default_to_no",
            "requires_user_interaction",
            "matched_ask_rule",
            "agent_id",
        ] {
            if let Some(field_value) = request.get(field) {
                value["native"][field] = field_value.clone();
            }
        }
        for field in [
            "description",
            "decision_reason",
            "decision_reason_type",
            "matched_ask_rule",
            "default_to_no",
        ] {
            if let Some(field_value) = request.get(field) {
                value["params"][field] = field_value.clone();
            }
        }
        value["schema"] = json!({"type":"object","required":["behavior"],"additionalProperties":false,
            "properties":{"behavior":{"type":"string","enum":["deny","allow"],"default":"deny"},
            "updatedInput":{"type":"object"},"message":{"type":"string"}}});
        let restrictions_valid = [
            "requires_user_interaction",
            "default_to_no",
            "suppress_always_allow_rule",
            "classifier_approvable",
        ]
        .iter()
        .all(|field| request.get(*field).is_none_or(Value::is_boolean));
        let supported = !scope_unknown
            && restrictions_valid
            && request["subtype"] == "can_use_tool"
            && request["tool_name"].is_string()
            && request["tool_use_id"].is_string()
            && request["input"].is_object()
            && !request
                .get("requires_user_interaction")
                .is_some_and(|v| v != false)
            && native.len() <= 4096
            && title.len() <= 1024
            && self.pending.len() < MAX_PENDING
            && crate::limits::encoded_bytes(&value) <= REQUEST_BYTES;
        if !supported {
            // No partial input/consent can become approvable. Retain only bounded identity and reason.
            value = json!({"request":id,"revision":1,"method":"_unrepresentable","title":"Native interaction is not representable",
                "params":{},"scope":{"operation":format!("permission-{id}")},"status":"cancelled","incomplete":true,
                "native":{"waiting":"unknown"}});
            if let Some(requires) = request["requires_user_interaction"].as_bool() {
                value["native"]["requires_user_interaction"] = json!(requires);
            }
            if native.len() <= 4096 {
                value["native"]["request_id"] = json!(native);
                let remembered = self.remember_scope(native, request, child, scope_unknown);
                // An undeclared user dialog MUST NOT be answered as a user dismissal. The native CLI
                // discards error replies for it too; local cancellation is not native settlement.
                if remembered && request["subtype"] != "request_user_dialog" {
                    let token = WriteToken::new();
                    self.unsupported_tokens.insert(native.into(), token.clone());
                    result.outgoing.push(Outbound {
                        frame: error_response(native),
                        token,
                    });
                }
            }
        } else {
            self.native.insert(native.into(), id.clone());
            self.pending.insert(
                id.clone(),
                Pending {
                    native: native.into(),
                    request: request.clone(),
                    value: value.clone(),
                    claimed: false,
                    user_cancelled: false,
                    token: WriteToken::new(),
                },
            );
        }
        if let Some(child) = child {
            value["child"] = json!(child);
        }
        if scope_unknown {
            value["native"]["scope_unknown"] = json!(true);
        }
        result.changes.push(Change { id, value });
        result
    }
    /// Validate entirely before claiming. Native tool-input validation remains native, not emulated.
    pub fn answer(&mut self, id: &str, revision: u64, response: &Value) -> Result<Value, Refusal> {
        let pending = self
            .pending
            .get_mut(id)
            .ok_or_else(|| refusal("stale", "request is no longer pending"))?;
        if revision != 1 || pending.claimed || pending.value["status"] != "pending" {
            return Err(refusal("stale", "request revision already claimed"));
        }
        let fields = response
            .as_object()
            .ok_or_else(|| refusal("invalid", "response must be an object"))?;
        if fields
            .keys()
            .any(|k| !matches!(k.as_str(), "action" | "content"))
        {
            return Err(refusal("invalid", "undeclared response field"));
        }
        if response["action"] != "accept" && fields.contains_key("content") {
            return Err(refusal("invalid", "only an accepted form has content"));
        }
        let answer = match response["action"].as_str() {
            Some("cancel") => {
                json!({"subtype":"error","request_id":pending.native,"error":"vqro user cancelled the request"})
            }
            Some("decline") => json!({"subtype":"success","request_id":pending.native,
                "response":{"behavior":"deny","message":"Denied by user"}}),
            Some("accept") => {
                let content = response["content"]
                    .as_object()
                    .ok_or_else(|| refusal("invalid", "form content must be an object"))?;
                if content
                    .keys()
                    .any(|k| !matches!(k.as_str(), "behavior" | "updatedInput" | "message"))
                    || crate::limits::encoded_bytes(&response["content"]) > REQUEST_BYTES
                {
                    return Err(refusal(
                        "invalid",
                        "form has undeclared or oversized fields",
                    ));
                }
                let result = match response["content"]["behavior"].as_str() {
                    Some("allow") => {
                        if content.contains_key("message") {
                            return Err(refusal("invalid", "allow has no denial message"));
                        }
                        let input = content
                            .get("updatedInput")
                            .unwrap_or(&pending.request["input"]);
                        if !input.is_object() {
                            return Err(refusal("invalid", "updatedInput must be an object"));
                        }
                        json!({"behavior":"allow","updatedInput":input})
                    }
                    Some("deny") => {
                        if content.contains_key("updatedInput") {
                            return Err(refusal("invalid", "deny has no updatedInput"));
                        }
                        let message = content
                            .get("message")
                            .map(|m| m.as_str().filter(|s| s.len() <= 2048))
                            .unwrap_or(Some("Denied by user"))
                            .ok_or_else(|| {
                                refusal("invalid", "denial message must be bounded text")
                            })?;
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
        if let Some(pending) = self.pending.get_mut(id) {
            if self
                .native
                .get(&pending.native)
                .is_some_and(|current| current == id)
                && pending.value["status"] == "pending"
            {
                pending.claimed = false;
                pending.token = WriteToken::new();
            }
        }
    }
    pub fn dispatched(&mut self, id: &str, known_written: bool) -> Option<Change> {
        let pending = self.pending.get_mut(id)?;
        pending.value["status"] = json!(if pending.user_cancelled {
            "cancelled"
        } else {
            "answered"
        });
        pending.value["native"]["response"] = json!(if known_written {
            "dispatched"
        } else {
            "unknown"
        });
        // There is no response ACK. Keep native waiting unknown until a native withdrawal/tool event.
        Some(Change {
            id: id.into(),
            value: pending.value.clone(),
        })
    }
    pub fn tool_started(&mut self, tool: &str) -> Vec<Change> {
        self.tool_started_scoped(tool, None)
    }
    pub fn tool_started_scoped(&mut self, tool: &str, child: Option<&str>) -> Vec<Change> {
        let retired: Vec<_> = self
            .unsupported_tools
            .iter()
            .filter(|(_, (scope, id))| id == tool && *scope == request_scope(child))
            .map(|(native, _)| native.clone())
            .collect();
        for native in retired {
            if let Some(token) = self.unsupported_tokens.remove(&native) {
                token.cancel_before_write();
            }
            self.unsupported_tools.remove(&native);
            self.recent.retain(|r| r.id != native);
        }
        let ids: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, p)| {
                p.value["native"]["tool_use_id"] == tool && p.value["child"].as_str() == child
            })
            .map(|(id, _)| id.clone())
            .collect();
        ids.into_iter()
            .filter_map(|id| {
                let mut pending = self.pending.remove(&id)?;
                pending.token.cancel_before_write();
                self.native.remove(&pending.native);
                pending.value["status"] = json!("answered");
                pending.value["native"]["waiting"] = json!("not_waiting");
                Some(Change {
                    id,
                    value: pending.value,
                })
            })
            .collect()
    }
    /// A full request that could not be published must not remain approvable through summaries.
    pub fn unrepresentable(&mut self, id: &str) -> Option<Outbound> {
        let mut pending = self.pending.remove(id)?;
        if !self.remember_scope(
            &pending.native,
            &pending.request,
            pending.value["child"].as_str(),
            pending.value["native"]["scope_unknown"] == true,
        ) {
            // RECENT cannot accept another identity. Retain the already bounded original entry
            // and native mapping: a failed revoke must not erase a possibly-written claim.
            // It must NOT remain approvable through summaries when publication failed either.
            pending.token.cancel_before_write();
            pending.value["status"] = json!("expired");
            pending.value["incomplete"] = json!(true);
            self.pending.insert(id.into(), pending);
            return None;
        }
        self.native.remove(&pending.native);
        let token = pending.token;
        self.unsupported_tokens
            .insert(pending.native.clone(), token.clone());
        Some(Outbound {
            frame: error_response(&pending.native),
            token,
        })
    }
    pub fn expire(&mut self) -> Vec<Change> {
        for token in self.unsupported_tokens.values() {
            token.cancel_before_write();
        }
        let mut changes = Vec::new();
        for (id, mut pending) in std::mem::take(&mut self.pending) {
            pending.token.cancel_before_write();
            pending.value["status"] = json!("expired");
            pending.value["native"]["waiting"] = json!("unknown");
            changes.push(Change {
                id: id.clone(),
                value: pending.value.clone(),
            });
            // Unknown/dispatched consent does not become answerable again after a view epoch.
            if pending.claimed {
                self.pending.insert(id, pending);
            } else {
                self.native.remove(&pending.native);
            }
        }
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn permission() -> Value {
        json!({"type":"control_request","request_id":"native-p1","request":{
        "subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"tool1","input":{"command":"echo safe"},
        "default_to_no":true,"suppress_always_allow_rule":true,"decision_reason":"Exact warning"}})
    }
    #[test]
    fn permission_keeps_restrictions_and_invalid_answer_does_not_claim() {
        let mut book = Requests::default();
        let incoming = book.observe(&permission(), None);
        let request = &incoming.changes[0];
        assert_eq!(request.value["native"]["default_to_no"], true);
        assert_eq!(request.value["native"]["decision_reason"], "Exact warning");
        assert!(
            book.answer(
                &request.id,
                1,
                &json!({"action":"accept","content":{"behavior":"allow","updatedInput":[]}})
            )
            .is_err()
        );
        let answer = book.answer(&request.id, 1, &json!({"action":"accept","content":{"behavior":"allow","updatedInput":{"command":"echo changed"}}})).unwrap();
        assert_eq!(
            answer["response"]["response"]["updatedInput"]["command"],
            "echo changed"
        );
        assert!(
            book.answer(&request.id, 1, &json!({"action":"decline"}))
                .is_err()
        );
    }
    #[test]
    fn deny_and_cancel_are_distinct_native_replies() {
        for (action, subtype) in [("decline", "success"), ("cancel", "error")] {
            let mut book = Requests::default();
            let id = book.observe(&permission(), None).changes.remove(0).id;
            let answer = book.answer(&id, 1, &json!({"action":action})).unwrap();
            assert_eq!(answer["response"]["subtype"], subtype);
        }
    }
    #[test]
    fn native_user_interaction_is_never_flattened_into_an_approve_form() {
        let mut book = Requests::default();
        let mut frame = permission();
        frame["request"]["requires_user_interaction"] = json!(true);
        let incoming = book.observe(&frame, None);
        assert!(book.is_empty());
        assert_eq!(incoming.changes[0].value["incomplete"], true);
        assert_eq!(incoming.outgoing[0]["response"]["subtype"], "error");
        assert!(book.observe(&frame, None).outgoing.is_empty());
    }
    #[test]
    fn unknown_claim_survives_same_snapshot_and_view_epoch_until_native_settlement() {
        let mut book = Requests::default();
        let frame = permission();
        let id = book.observe(&frame, None).changes.remove(0).id;
        book.answer(
            &id,
            1,
            &json!({"action":"accept","content":{"behavior":"allow"}}),
        )
        .unwrap();
        book.dispatched(&id, false).unwrap();
        assert!(book.observe(&frame, None).changes.is_empty());
        let mut changed = frame.clone();
        changed["request"]["input"]["command"] = json!("changed after unknown write");
        assert_eq!(book.observe(&changed, None).changes.len(), 1);
        assert!(
            book.is_empty(),
            "changed snapshot cannot rearm unknown consent"
        );
        assert!(book.answer(&id, 1, &json!({"action":"decline"})).is_err());
        book.expire();
        assert!(book.observe(&frame, None).changes.is_empty());
        assert!(book.answer(&id, 1, &json!({"action":"decline"})).is_err());
        book.tool_started("tool1");
        let next = book.observe(&frame, None).changes.remove(0).id;
        assert_ne!(id, next);
    }
    #[test]
    fn withdrawal_and_reused_payload_revoke_the_unwritten_answer_token() {
        let mut book = Requests::default();
        let frame = permission();
        let id = book.observe(&frame, None).changes.remove(0).id;
        book.answer(
            &id,
            1,
            &json!({"action":"accept","content":{"behavior":"allow"}}),
        )
        .unwrap();
        let token = book.answer_token(&id).unwrap();
        let mut replacement = frame;
        replacement["request"]["input"]["command"] = json!("changed native question");
        let next = book.observe(&replacement, None).changes.remove(1).id;
        assert_ne!(id, next);
        assert!(!token.write_begun());
        assert!(!token.cancel_before_write(), "already revoked by reuse");
        assert!(book.answer(&id, 1, &json!({"action":"decline"})).is_err());
    }
    #[test]
    fn full_recent_unpublished_unclaimed_request_is_bounded_and_not_approvable() {
        let mut book = Requests::default();
        let frame = permission();
        for n in 0..RECENT {
            assert!(book.remember(&format!("occupied-{n}"), &frame["request"]));
        }
        let id = book.observe(&frame, None).changes.remove(0).id;
        let token = book.answer_token(&id).unwrap();
        assert!(book.unrepresentable(&id).is_none());
        assert_eq!(book.pending.len(), 1);
        assert_eq!(book.recent.len(), RECENT);
        assert!(book.summaries().is_empty());
        assert!(!token.write_begun());
        assert!(
            !token.cancel_before_write(),
            "queued answer already revoked"
        );
        assert!(book.answer(&id, 1, &json!({"action":"decline"})).is_err());
        assert!(book.observe(&frame, None).changes.is_empty());
    }
    #[test]
    fn changed_human_claim_revokes_before_reading_write_phase() {
        // Source-order control for the independent writer's permitted interleaving. The memory
        // writer controls cover both winners; this guard prevents a stale pre-revoke phase read.
        let source = include_str!("requests.rs");
        let branch = source
            .split("if let Some(mut old) = self.pending.remove(&old_id) {")
            .nth(1)
            .unwrap()
            .split("if unsettled_claim {")
            .next()
            .unwrap();
        assert!(
            branch.find("old.token.cancel_before_write()").unwrap()
                < branch.find("old.token.write_begun()").unwrap(),
            "human claim must revoke BEFORE reading possible-write phase; a pre-read can become stale"
        );
    }
    #[test]
    fn unsupported_reply_token_is_revoked_by_id_reuse_not_replayed_from_outbox() {
        let mut book = Requests::default();
        let mut frame = permission();
        frame["request"]["requires_user_interaction"] = json!(true);
        let outgoing = book.observe(&frame, None).outgoing.remove(0);
        frame["request"]["requires_user_interaction"] = json!(false);
        assert_eq!(book.observe(&frame, None).changes.len(), 1);
        assert!(
            !outgoing.token.cancel_before_write(),
            "old auto-reply already revoked"
        );
        assert!(!outgoing.token.write_begun());
    }
    #[test]
    fn snapshot_duplicate_withdrawal_and_id_reuse_do_not_reuse_standard_identity() {
        let mut book = Requests::default();
        let frame = permission();
        let first = book.observe(&frame, Some("child1")).changes.remove(0);
        assert_eq!(first.value["child"], "child1");
        assert!(book.observe(&frame, None).changes.is_empty());
        let withdrawn = book.observe(
            &json!({"type":"control_cancel_request","request_id":"native-p1"}),
            None,
        );
        assert_eq!(withdrawn.changes[0].value["status"], "cancelled");
        assert!(
            book.answer(&first.id, 1, &json!({"action":"decline"}))
                .is_err()
        );
        let reused = book.observe(&frame, None).changes.remove(0);
        assert_ne!(first.id, reused.id);
    }
    #[test]
    fn dispatched_answer_does_not_prove_native_is_no_longer_waiting() {
        let mut book = Requests::default();
        let id = book.observe(&permission(), None).changes.remove(0).id;
        book.answer(&id, 1, &json!({"action":"decline"})).unwrap();
        book.dispatched(&id, true).unwrap();
        assert!(book.has_unconfirmed());
        assert!(book.summaries().is_empty());
        let done = book.tool_started("tool1");
        assert_eq!(done[0].value["native"]["waiting"], "not_waiting");
        assert!(!book.has_unconfirmed());
    }
    #[test]
    fn oversized_native_input_is_omitted_not_shortened_or_approvable() {
        let mut book = Requests::default();
        let mut frame = permission();
        frame["request"]["input"]["command"] = json!("x".repeat(REQUEST_BYTES));
        let incoming = book.observe(&frame, None);
        assert_eq!(incoming.changes[0].value["status"], "cancelled");
        assert!(
            incoming.changes[0].value["params"]
                .as_object()
                .unwrap()
                .is_empty()
        );
        assert!(book.is_empty());
    }
    #[test]
    fn unadvertised_user_dialog_is_not_answered_as_user_cancellation() {
        let mut book = Requests::default();
        let result = book.observe(&json!({"type":"control_request","request_id":"dialog1","request":{"subtype":"request_user_dialog","dialog_kind":"new_kind"}}), None);
        assert!(result.outgoing.is_empty());
        assert_eq!(result.changes[0].value["native"]["waiting"], "unknown");
    }
}
