//! Bounded native child observations in their parent's stream, never emulated controls.
use super::{Change, ENTITY_BYTES, Endpoint, id, reported_string, small};
use base64::Engine;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const CHILDREN: usize = 256;
const SCOPED_IDS: usize = 4096;

#[derive(Default)]
pub(super) struct Children {
    rows: BTreeMap<String, Value>,
    ids: BTreeMap<(String, String, String), String>,
    next: u64,
}

impl Children {
    pub(super) fn contains(&self, thread: &str) -> bool {
        self.rows.contains_key(thread)
    }

    pub(super) fn identity(&mut self, thread: &str, kind: &str, native: &str) -> Option<String> {
        if thread.is_empty() || thread.len() > 128 || native.is_empty() || native.len() > 128 {
            return None;
        }
        // Reversibly encode BOTH native components for every item path, including the
        // main thread. Native strings cannot impersonate generated scoped IDs. JSON tuple
        // framing is injective even for unusual native strings with delimiters/controls.
        let pair = serde_json::to_vec(&(thread, native)).ok()?;
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(pair);
        if encoded.len() <= 126 {
            return Some(format!("n:{encoded}"));
        }
        // Unusually long native IDs need a compact epoch-local mapping. Its separate
        // prefix cannot collide with the reversible namespace above.
        let key = (thread.to_owned(), kind.to_owned(), native.to_owned());
        if let Some(id) = self.ids.get(&key) {
            return Some(id.clone());
        }
        if self.ids.len() >= SCOPED_IDS {
            return None;
        }
        self.next += 1;
        let id = format!("l:{}", self.next);
        self.ids.insert(key, id.clone());
        Some(id)
    }

    fn register(&mut self, thread: &str, parent: Option<Value>) -> Option<String> {
        if thread.is_empty() || thread.len() > 128 {
            return None;
        }
        if let Some(Value::String(parent_id)) = &parent {
            let target_id = self.rows.get(thread).and_then(|row| row["id"].as_str());
            let mut ancestor = Some(parent_id.as_str());
            for _ in 0..=CHILDREN {
                let Some(current) = ancestor else { break };
                if Some(current) == target_id {
                    return None;
                }
                ancestor = self
                    .rows
                    .values()
                    .find(|row| row["id"] == current)
                    .and_then(|row| row["parent"].as_str());
            }
        }
        if let Some(row) = self.rows.get_mut(thread) {
            if let Some(parent) = parent {
                if row.get("parent").is_some_and(|old| old != &parent) {
                    return None; // conflicting lineage is not permission to rebind
                }
                row["parent"] = parent;
            }
            return row["id"].as_str().map(str::to_owned);
        }
        if self.rows.len() >= CHILDREN {
            return None;
        }
        let child = self.identity(thread, "child", thread)?;
        let mut value =
            json!({"id":child,"status":"unknown","items":"scoped","native":{"thread_id":thread}});
        if let Some(parent) = parent {
            value["parent"] = parent;
        }
        self.rows.insert(thread.into(), value);
        Some(child)
    }

    fn change(&self, thread: &str) -> Option<Change> {
        let mut value = self.rows.get(thread)?.clone();
        let child = value.as_object_mut()?.remove("id")?.as_str()?.to_owned();
        Some(Change::new("child", child, value))
    }
}

fn waiting(status: &Value) -> bool {
    status["type"] == "active"
        && status["activeFlags"].as_array().is_some_and(|flags| {
            flags
                .iter()
                .any(|flag| flag == "waitingOnApproval" || flag == "waitingOnUserInput")
        })
}

fn child_status(native: &str) -> &'static str {
    match native {
        "pendingInit" => "pending",
        "running" | "active" | "started" => "running",
        "idle" => "waiting",
        "completed" => "completed",
        "errored" => "failed",
        "interrupted" | "shutdown" => "cancelled",
        _ => "unknown",
    }
}

fn merge_child_status(row: &mut Value, native: &str, is_waiting: bool) {
    // Loading an idle thread is not evidence that its terminal collab run restarted.
    let terminal = matches!(
        row["status"].as_str(),
        Some("completed" | "failed" | "cancelled")
    );
    if native == "idle" && terminal {
        return;
    }
    row["status"] = json!(if is_waiting {
        "waiting"
    } else {
        child_status(native)
    });
    row["native"]["status"] = json!(native);
}

impl Endpoint {
    pub(super) fn observes_thread(&self, thread: &str) -> bool {
        self.lifecycle != "ended"
            && (self.session.as_deref() == Some(thread) || self.children.contains(thread))
    }

    pub(super) fn children_child_id(&self, thread: &str) -> Option<Value> {
        self.children.rows.get(thread).map(|row| row["id"].clone())
    }

    fn parent_child(&self, thread: &str) -> Option<Value> {
        if self.session.as_deref() == Some(thread) {
            Some(Value::Null)
        } else {
            self.children.rows.get(thread).map(|row| row["id"].clone())
        }
    }

    /// True only for a native child thread whose parent is already associated here.
    pub(super) fn child_thread_started(&mut self, thread: &Value) -> Vec<Change> {
        let Ok(native) = id(thread, "id") else {
            return vec![];
        };
        let source = &thread["source"]["subAgent"]["thread_spawn"];
        let parent_native = thread["parentThreadId"]
            .as_str()
            .or_else(|| source["parent_thread_id"].as_str());
        if self.session.as_deref() == Some(&native) || parent_native == Some(native.as_str()) {
            return vec![];
        }
        let parent = if let Some(parent_native) = parent_native {
            let Some(parent) = self.parent_child(parent_native) else {
                return vec![];
            };
            // Source contains the exact native depth, but must not contradict parentThreadId.
            if source["parent_thread_id"]
                .as_str()
                .is_some_and(|other| other != parent_native)
            {
                return vec![];
            }
            Some(parent)
        } else if self.children.contains(&native) {
            None
        } else {
            return vec![];
        };
        if self.children.register(&native, parent).is_none() {
            return vec![];
        }
        let row = self.children.rows.get_mut(&native).unwrap();
        for (native_field, field) in [
            ("agentNickname", "name"),
            ("agentRole", "role"),
            ("model", "model"),
        ] {
            if let Some(text) = reported_string(&thread[native_field], 256) {
                row[field] = if field == "model" {
                    json!({"id":text})
                } else {
                    json!(text)
                };
            }
        }
        if let Some(depth) = source["depth"].as_u64() {
            row["depth"] = json!(depth);
        }
        if let Some(status) = reported_string(&thread["status"]["type"], 128) {
            merge_child_status(row, &status, waiting(&thread["status"]));
        }
        self.children.change(&native).into_iter().collect()
    }

    /// Item evidence can mention children before thread/started. Only spawn proves direct lineage.
    pub(super) fn collaboration(&mut self, item: &Value, sender: &str) -> Vec<Change> {
        if !small(item, ENTITY_BYTES / 2) || self.parent_child(sender).is_none() {
            return vec![];
        }
        let mut changes = vec![];
        if item["type"] == "collabAgentToolCall" {
            if item["senderThreadId"].as_str() != Some(sender) {
                return vec![];
            }
            let Some(receivers) = item["receiverThreadIds"]
                .as_array()
                .filter(|v| v.len() <= CHILDREN)
            else {
                return vec![];
            };
            for receiver in receivers {
                let Some(thread) = receiver
                    .as_str()
                    .filter(|id| !id.is_empty() && id.len() <= 128)
                else {
                    continue;
                };
                if self.session.as_deref() == Some(thread) || thread == sender {
                    continue;
                }
                let parent = (item["tool"] == "spawnAgent")
                    .then(|| self.parent_child(sender))
                    .flatten();
                // A wait/send may name an existing sibling; association is not direct parent proof.
                if self.children.register(thread, parent).is_none() {
                    continue;
                }
                let row = self.children.rows.get_mut(thread).unwrap();
                if let Some(status) = reported_string(&item["agentsStates"][thread]["status"], 128)
                {
                    row["status"] = json!(child_status(&status));
                    row["native"]["status"] = json!(status);
                }
                if let Some(change) = self.children.change(thread) {
                    changes.push(change)
                }
            }
        } else if item["type"] == "subAgentActivity" {
            let Some(thread) = item["agentThreadId"]
                .as_str()
                .filter(|id| !id.is_empty() && id.len() <= 128)
            else {
                return vec![];
            };
            if self.session.as_deref() == Some(thread)
                || self.children.register(thread, None).is_none()
            {
                return vec![];
            }
            let row = self.children.rows.get_mut(thread).unwrap();
            if let Some(status) = reported_string(&item["kind"], 128) {
                row["status"] = json!(child_status(&status));
                row["native"]["status"] = json!(status);
            }
            if let Some(path) = reported_string(&item["agentPath"], 1024) {
                row["native"]["agent_path"] = json!(path);
            }
            if let Some(model) = reported_string(&item["model"], 256) {
                row["model"] = json!({"id":model});
            }
            if let Some(change) = self.children.change(thread) {
                changes.push(change)
            }
        }
        changes
    }

    pub(super) fn whole_plan(&mut self, params: &Value, child: Option<Value>) -> Vec<Change> {
        let (Some(thread), Some(turn)) = (params["threadId"].as_str(), params["turnId"].as_str())
        else {
            return vec![];
        };
        let Some(id) = self.children.identity(thread, "plan", turn) else {
            return vec![];
        };
        let mut refused = json!({"incomplete":true,"items":[],"update":"whole","source":"native",
            "native":{"thread_id":thread,"turn_id":turn}});
        if let Some(owner) = &child {
            refused["owner"] = owner.clone();
        }
        // The existing projection retains a known entity's old contents and marks both
        // it and the stream incomplete. Never bypass that witness with a silent drop.
        let refusal = || vec![Change::new("plan", id.clone(), refused.clone())];
        let Some(plan) = params["plan"].as_array().filter(|plan| plan.len() <= 256) else {
            return refusal();
        };
        let mut items = vec![];
        for step in plan {
            let Some(text) = reported_string(&step["step"], 8192) else {
                return refusal();
            };
            let status = match step["status"].as_str() {
                Some("pending") => "pending",
                Some("inProgress") => "in_progress",
                Some("completed") => "completed",
                _ => "unknown",
            };
            let mut item = json!({"text":text,"status":status});
            if status == "unknown" {
                if let Some(native) = reported_string(&step["status"], 128) {
                    item["native"] = json!({"status":native});
                }
            }
            items.push(item);
        }
        let mut value = json!({"items":items,"update":"whole","source":"native","native":{"thread_id":thread,"turn_id":turn}});
        if let Some(child) = child {
            value["owner"] = child;
        }
        if params["explanation"]
            .as_str()
            .is_some_and(|text| text.len() > 8192)
        {
            return refusal();
        }
        if let Some(text) = reported_string(&params["explanation"], 8192) {
            value["explanation"] = json!(text);
        }
        if !small(&value, ENTITY_BYTES - 1024) {
            return refusal();
        }
        vec![Change::new("plan", id, value)]
    }

    pub(super) fn goal(&self, method: &str, params: &Value, owner: Option<Value>) -> Vec<Change> {
        let Some(thread) = params["threadId"].as_str() else {
            return vec![];
        };
        let mut value = if method == "thread/goal/cleared" {
            json!({"status":"unknown","native":{"thread_id":thread,"cleared":true}})
        } else {
            let native = &params["goal"];
            if native["threadId"].as_str() != Some(thread) || !small(native, ENTITY_BYTES / 2) {
                return vec![];
            }
            let Some(status) = reported_string(&native["status"], 128) else {
                return vec![];
            };
            let normalized = match status.as_str() {
                "active" => "active",
                "paused" => "paused",
                "blocked" | "usageLimited" | "budgetLimited" => "blocked",
                "complete" => "completed",
                _ => "unknown",
            };
            let mut value =
                json!({"status":normalized,"native":{"thread_id":thread,"status":status}});
            if let Some(objective) = reported_string(&native["objective"], 8192) {
                value["objective"] = json!(objective);
            }
            if let Some(tokens) = native["tokenBudget"].as_u64() {
                value["budget"] = json!({"tokens":tokens});
            }
            let mut used = serde_json::Map::new();
            for (native_field, field) in [("tokensUsed", "tokens"), ("timeUsedSeconds", "seconds")]
            {
                if let Some(number) = native[native_field].as_u64() {
                    used.insert(field.into(), json!(number));
                }
            }
            if !used.is_empty() {
                value["used"] = Value::Object(used);
            }
            if let Some(ms) = native["updatedAt"]
                .as_u64()
                .and_then(|seconds| seconds.checked_mul(1000))
            {
                value["updated_ms"] = json!(ms);
            }
            value
        };
        let id = owner
            .as_ref()
            .and_then(Value::as_str)
            .unwrap_or("current")
            .to_owned();
        if let Some(owner) = owner {
            value["owner"] = owner;
        }
        vec![Change::new("goal", id, value)]
    }

    pub(super) fn observed_items(&mut self, method: &str, params: &Value) -> Vec<Change> {
        let Some(thread) = params["threadId"].as_str() else {
            return vec![];
        };
        let child = self.children_child_id(thread);
        if matches!(method, "item/started" | "item/completed") {
            let item = &params["item"];
            let mut changes = self.collaboration(item, thread);
            for mut change in self.item(item, method == "item/completed") {
                let Some(id) = self.children.identity(thread, &change.kind, &change.id) else {
                    continue;
                };
                change.id = id;
                if let Some(child) = &child {
                    change.value["child"] = child.clone();
                }
                changes.push(change);
            }
            return changes;
        }
        let (Ok(native), Some(text)) = (id(params, "itemId"), params["delta"].as_str()) else {
            return vec![];
        };
        let (kind, mut base, field) = if method == "item/agentMessage/delta" {
            (
                "message",
                json!({"role":"assistant","content":[{"type":"text","text":""}],"final":false}),
                "/content/0/text",
            )
        } else if method == "item/commandExecution/outputDelta" {
            (
                "tool",
                json!({"title":"Codex command","status":"running","content":[{"type":"text","text":""}]}),
                "/content/0/text",
            )
        } else {
            return vec![];
        };
        if let Some(child) = child {
            base["child"] = child;
        }
        let Some(id) = self.children.identity(thread, kind, &native) else {
            return vec![];
        };
        if text.len() > ENTITY_BYTES {
            base["incomplete"] = json!(true);
            // This witnesses omitted content, not a native lifecycle update. In particular
            // the synthetic delta base must not reopen an already final message.
            if let Some(fields) = base.as_object_mut() {
                fields.remove("final");
                fields.remove("status");
            }
            return vec![Change::new(kind, id, base)];
        }
        vec![Change::append(kind, id, base, field, text.into())]
    }

    pub(super) fn child_notification(&mut self, method: &str, params: &Value) -> Vec<Change> {
        let Some(thread) = params["threadId"]
            .as_str()
            .filter(|id| self.children.contains(id))
        else {
            return vec![];
        };
        let child = self.children.rows[thread]["id"].clone();
        match method {
            "thread/status/changed" | "turn/started" | "turn/completed" => {
                let status = if method == "turn/started" {
                    Some("running")
                } else if method == "turn/completed" {
                    Some("idle")
                } else {
                    params["status"]["type"].as_str()
                };
                if let Some(status) = status.filter(|s| s.len() <= 128) {
                    let row = self.children.rows.get_mut(thread).unwrap();
                    merge_child_status(row, status, waiting(&params["status"]));
                }
                self.children.change(thread).into_iter().collect()
            }
            "thread/tokenUsage/updated" => {
                let reported = &params["tokenUsage"];
                let mut usage = json!({});
                if let Some(window) = reported["modelContextWindow"].as_u64() {
                    usage["context"] = json!({"window":window});
                }
                let total = &reported["total"];
                if let (Some(input), Some(output)) = (
                    total["inputTokens"].as_u64(),
                    total["outputTokens"].as_u64(),
                ) {
                    let mut tokens = json!({"input":input,"output":output});
                    for (native, field) in [
                        ("cachedInputTokens", "cache_read"),
                        ("cacheWriteInputTokens", "cache_write"),
                    ] {
                        if let Some(count) = total[native].as_u64() {
                            tokens[field] = json!(count);
                        }
                    }
                    usage["tokens"] = tokens;
                }
                if usage.as_object().is_some_and(|value| !value.is_empty()) {
                    self.children.rows.get_mut(thread).unwrap()["usage"] = usage;
                    self.children.change(thread).into_iter().collect()
                } else {
                    vec![]
                }
            }
            "turn/plan/updated" => self.whole_plan(params, Some(child)),
            "thread/goal/updated" | "thread/goal/cleared" => self.goal(method, params, Some(child)),
            "item/started"
            | "item/completed"
            | "item/agentMessage/delta"
            | "item/commandExecution/outputDelta" => self.observed_items(method, params),
            _ => vec![],
        }
    }
}
