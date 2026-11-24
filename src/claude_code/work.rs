//! Native children and background work. No agent spawning, inferred endpoints or scheduler policy.
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
pub const TASKS: usize = 128;
const IDENTITIES: usize = 1024;
#[cfg(test)]
#[path = "work_tests.rs"]
mod tests;
pub struct Change {
    pub kind: &'static str,
    pub id: String,
    pub value: Value,
}
struct Task {
    id: String,
    child: Option<String>,
    run: Option<String>,
    value: Value,
    child_value: Value,
    terminal: bool,
}
#[derive(Default)]
pub struct Work {
    serial: u64,
    tasks: BTreeMap<String, Task>,
    tools: BTreeMap<String, Option<String>>,
    scoped: BTreeMap<(String, String, String), String>,
    pub incomplete: bool,
}
pub fn native_id(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 128 && !s.contains('\0'))
}
fn cut(row: &mut Value) {
    row["incomplete"] = json!(true);
}
fn text(row: &mut Value, from: &Value, source: &str, target: &str, max: usize) {
    if let Some(value) = from.get(source) {
        if let Some(s) = value.as_str().filter(|s| s.len() <= max) {
            row[target] = json!(s);
        } else {
            row.as_object_mut().unwrap().remove(target);
            cut(row);
        }
    }
}
fn task_status(s: &str) -> &'static str {
    match s {
        "running" => "running",
        "completed" => "completed",
        "failed" => "failed",
        "stopped" | "killed" => "stopped",
        _ => "unknown",
    }
}
fn child_status(s: &str) -> &'static str {
    match s {
        "pending" => "pending",
        "running" => "running",
        "paused" => "waiting",
        "completed" => "completed",
        "failed" => "failed",
        "stopped" | "killed" => "cancelled",
        _ => "unknown",
    }
}
fn usage(row: &mut Value, value: &Value) {
    if let Some(v) = value.get("usage") {
        if let Some(total) = v["total_tokens"].as_u64() {
            row["usage"] = json!({"tokens":{"total":total}});
        } else {
            cut(row);
        }
        let mut native = serde_json::Map::new();
        for field in ["total_tokens", "tool_uses", "duration_ms"] {
            if let Some(n) = v[field].as_u64() {
                native.insert(field.into(), json!(n));
            } else if v.get(field).is_some() {
                cut(row);
            }
        }
        row["native"]["usage"] = Value::Object(native);
    }
}
impl Work {
    fn next(&mut self, prefix: &str) -> String {
        self.serial += 1;
        format!("{prefix}{}", self.serial)
    }
    fn ensure(&mut self, native: &str) -> bool {
        if self.tasks.contains_key(native) {
            return true;
        }
        if self.tasks.len() >= TASKS {
            self.incomplete = true;
            return false;
        }
        let id = self.next("task");
        self.tasks.insert(
            native.into(),
            Task {
                id,
                child: None,
                run: None,
                value: json!({"status":"unknown","native":{"task_id":native}}),
                child_value: Value::Null,
                terminal: false,
            },
        );
        true
    }
    fn ensure_child(&mut self, native: &str) {
        if self.tasks[native].child.is_some() {
            return;
        }
        let id = self.next("child");
        let row = self.tasks.get_mut(native).unwrap();
        row.child = Some(id);
        row.child_value = json!({"status":"unknown","items":"scoped","native":{"task_id":native}});
        row.value["child"] = json!(row.child);
    }
    fn changes(&self, native: &str) -> Vec<Change> {
        let Some(row) = self.tasks.get(native) else {
            return vec![];
        };
        let mut changes = vec![Change {
            kind: "task",
            id: row.id.clone(),
            value: row.value.clone(),
        }];
        if let Some(id) = &row.child {
            changes.push(Change {
                kind: "child",
                id: id.clone(),
                value: row.child_value.clone(),
            });
        }
        changes
    }
    pub fn rows(&self) -> Vec<Change> {
        self.tasks.keys().flat_map(|id| self.changes(id)).collect()
    }
    /// A producer's agent_id is observation identity, NEVER a send destination API.
    pub fn scope(&mut self, frame: &Value) -> (Option<String>, Vec<Change>, bool) {
        let wrapper = frame.get("agent_id").filter(|v| !v.is_null());
        let permission = frame["request"].get("agent_id").filter(|v| !v.is_null());
        // can_use_tool's documented producer identity is IN its permission body. Extra wrapper
        // metadata must not override that authoritative native question context.
        let agent = if frame["type"] == "control_request" {
            permission.or(wrapper)
        } else {
            wrapper.or(permission)
        };
        let parent_value = frame.get("parent_tool_use_id").filter(|v| !v.is_null());
        let parent = parent_value.and_then(native_id);
        if parent_value.is_some() && parent.is_none() {
            self.incomplete = true;
            return (None, vec![], true);
        }
        if let Some(agent) = agent {
            let Some(id) = native_id(agent) else {
                self.incomplete = true;
                return (None, vec![], true);
            };
            if !self.ensure(id) {
                return (None, vec![], true);
            }
            if self.tasks[id].value["native"]["subagent_type"] == "main-session" {
                self.incomplete = true;
                return (None, vec![], true);
            }
            self.ensure_child(id);
            let row = self.tasks.get_mut(id).unwrap();
            text(&mut row.child_value, frame, "subagent_type", "role", 256);
            text(
                &mut row.child_value,
                &frame["message"],
                "model",
                "model",
                128,
            );
            return (row.child.clone(), self.changes(id), true);
        }
        if let Some(parent) = parent {
            if let Some(task) = self.tools.get(parent).and_then(Option::as_ref) {
                if let Some(child) = self.tasks.get(task).and_then(|t| t.child.clone()) {
                    return (Some(child), vec![], true);
                }
            }
            self.incomplete = true;
            return (None, vec![], true);
        }
        (None, vec![], false)
    }
    pub fn identity(
        &mut self,
        child: &str,
        kind: &str,
        native: &str,
        create: bool,
    ) -> Option<String> {
        let key = (child.into(), kind.into(), native.into());
        if let Some(id) = self.scoped.get(&key) {
            return Some(id.clone());
        }
        if !create {
            return None;
        }
        if native.len() > 256 || self.scoped.len() >= IDENTITIES {
            self.incomplete = true;
            return None;
        }
        let id = self.next("scoped");
        self.scoped.insert(key, id.clone());
        Some(id)
    }
    pub fn event(&mut self, frame: &Value) -> Vec<Change> {
        let Some(native) = native_id(&frame["task_id"]) else {
            self.incomplete = true;
            return vec![];
        };
        if !self.ensure(native) {
            return vec![];
        }
        let run = frame.get("run_id").and_then(native_id).map(str::to_owned);
        let old = self.tasks[native].run.clone();
        if let (Some(old), Some(new)) = (&old, &run) {
            if new < old {
                return vec![];
            }
        }
        if old.is_some() && run.is_none() {
            let row = self.tasks.get_mut(native).unwrap();
            cut(&mut row.value);
            if row.child.is_some() {
                cut(&mut row.child_value);
            }
            self.incomplete = true;
            return self.changes(native);
        }
        if run.is_some() && run != old {
            let row = self.tasks.get_mut(native).unwrap();
            row.run = run.clone();
            row.terminal = false;
            row.value = json!({"status":"unknown","native":{"task_id":native,"run_id":run}});
            if row.child.is_some() {
                row.child_value = json!({"status":"unknown","items":"scoped","native":{"task_id":native,"run_id":run}});
                row.value["child"] = json!(row.child);
            }
        }
        let agent = (frame["task_type"] == "local_agent" || frame["subagent_type"].is_string())
            && frame["subagent_type"] != "main-session";
        if agent {
            self.ensure_child(native);
        }
        let parent = frame
            .get("parent_task_id")
            .and_then(native_id)
            .map(str::to_owned);
        let mut cyclic = false;
        let mut cursor = parent.as_deref();
        let mut seen = BTreeSet::new();
        while let Some(id) = cursor {
            if id == native || !seen.insert(id) {
                cyclic = true;
                break;
            }
            cursor = self
                .tasks
                .get(id)
                .and_then(|r| r.value["native"]["parent_task_id"].as_str());
        }
        let parent_row = parent
            .as_ref()
            .filter(|_| !cyclic)
            .and_then(|id| self.tasks.get(id));
        let parent_task = parent_row.map(|r| r.id.clone());
        let parent_child = parent_row.and_then(|r| r.child.clone());
        let row = self.tasks.get_mut(native).unwrap();
        text(&mut row.value, frame, "task_type", "kind", 128);
        text(&mut row.value, frame, "description", "title", 2048);
        text(
            &mut row.value["native"],
            frame,
            "subagent_type",
            "subagent_type",
            256,
        );
        if frame.get("run_id").is_some() && frame.get("run_id").and_then(native_id).is_none() {
            cut(&mut row.value);
            self.incomplete = true;
        }
        if let Some(parent) = &parent {
            row.value["native"]["parent_task_id"] = json!(parent);
            if let Some(id) = parent_task {
                row.value["parent_task"] = json!(id);
            } else {
                row.value.as_object_mut().unwrap().remove("parent_task");
                cut(&mut row.value);
            }
        }
        if let Some(child) = &row.child {
            row.value["child"] = json!(child);
            text(&mut row.child_value, frame, "subagent_type", "role", 256);
            if let Some(depth) = frame["spawn_depth"].as_u64().filter(|d| *d >= 1) {
                row.child_value["depth"] = json!(depth);
                if depth == 1 && parent.is_none() {
                    row.child_value["parent"] = Value::Null;
                }
            }
            if let Some(parent) = parent_child {
                row.child_value["parent"] = json!(parent);
            } else if frame.get("parent_task_id").is_some() {
                row.child_value.as_object_mut().unwrap().remove("parent");
                cut(&mut row.child_value);
            }
        }
        if let Some(tool) = frame.get("tool_use_id").and_then(native_id) {
            row.value["native"]["tool_use_id"] = json!(tool);
            if row.child.is_some() {
                row.child_value["native"]["tool_use_id"] = json!(tool);
            }
            if let Some(old) = self.tools.get_mut(tool) {
                if old.as_deref() != Some(native) {
                    *old = None;
                    self.incomplete = true;
                    cut(&mut row.value);
                }
            } else if self.tools.len() < IDENTITIES {
                self.tools.insert(tool.into(), Some(native.into()));
            } else {
                self.incomplete = true;
                cut(&mut row.value);
            }
        }
        if let Some(run) = run {
            row.value["generation"] = json!(run);
            row.value["native"]["run_id"] = json!(run);
            if row.child.is_some() {
                row.child_value["native"]["run_id"] = json!(run);
            }
        }
        for field in ["ambient", "skip_transcript", "is_backgrounded"] {
            if let Some(v) = frame.get(field) {
                if v.is_boolean() {
                    row.value["native"][field] = v.clone();
                } else {
                    cut(&mut row.value);
                }
            }
        }
        let subtype = frame["subtype"].as_str().unwrap_or("");
        let status = if subtype == "task_notification" {
            frame["status"].as_str()
        } else if subtype == "task_updated" {
            frame["patch"]["status"].as_str()
        } else if matches!(subtype, "task_started" | "task_progress") {
            Some("running")
        } else {
            None
        };
        if status.is_some_and(|s| s.len() > 128)
            || (status.is_none()
                && (subtype == "task_notification"
                    || (subtype == "task_updated" && frame["patch"].get("status").is_some())))
        {
            row.value["status"] = json!("unknown");
            row.value["native"]
                .as_object_mut()
                .unwrap()
                .remove("status");
            cut(&mut row.value);
            if row.child.is_some() {
                row.child_value["status"] = json!("unknown");
                row.child_value["native"]
                    .as_object_mut()
                    .unwrap()
                    .remove("status");
                cut(&mut row.child_value);
            }
        }
        if let Some(status) = status.filter(|s| s.len() <= 128) {
            let terminal = matches!(status, "completed" | "failed" | "stopped" | "killed");
            if row.terminal
                && terminal
                && (row.value["native"]["status"] != status
                    || row.value["native"].get("conflicting_status").is_some())
            {
                row.value["status"] = json!("unknown");
                row.value["native"]["conflicting_status"] = json!(status);
                cut(&mut row.value);
                if row.child.is_some() {
                    row.child_value["status"] = json!("unknown");
                    cut(&mut row.child_value);
                }
            } else if !row.terminal || terminal {
                row.value["status"] = json!(task_status(status));
                row.value["native"]["status"] = json!(status);
                if row.child.is_some() {
                    row.child_value["status"] = json!(child_status(status));
                    row.child_value["native"]["status"] = json!(status);
                }
                row.terminal |= terminal;
            }
        }
        if subtype == "task_updated" {
            text(
                &mut row.value,
                &frame["patch"],
                "description",
                "title",
                2048,
            );
            for field in ["end_time", "total_paused_ms", "is_backgrounded"] {
                if let Some(v) = frame["patch"].get(field) {
                    if v.is_number() || v.is_boolean() {
                        row.value["native"][field] = v.clone();
                    } else {
                        cut(&mut row.value);
                    }
                }
            }
        }
        if frame["summary"].is_string() {
            let mut progress = json!({});
            text(&mut progress, frame, "summary", "summary", 4096);
            row.value["progress"] = progress;
        }
        usage(&mut row.value, frame);
        if row.child.is_some() {
            usage(&mut row.child_value, frame);
        }
        self.changes(native)
    }
    /// Full membership level; leaving it is NOT an observed terminal completion.
    pub fn level(&mut self, frame: &Value) -> (Vec<Change>, Option<Vec<Value>>) {
        let Some(tasks) = frame["tasks"].as_array() else {
            self.incomplete = true;
            return (vec![], None);
        };
        let mut complete = tasks.len() <= TASKS;
        let mut members = BTreeSet::new();
        let mut changes = vec![];
        let mut background = vec![];
        for task in tasks.iter().take(TASKS) {
            let Some(id) = native_id(&task["task_id"]) else {
                complete = false;
                continue;
            };
            if !members.insert(id.to_owned()) {
                complete = false;
                continue;
            }
            let mut event = task.clone();
            event["subtype"] = json!("_level");
            self.event(&event);
            let Some(row) = self.tasks.get_mut(id) else {
                complete = false;
                continue;
            };
            row.value["native"]["in_background_level"] = json!(true);
            let mut value = json!({"id":row.id,"native":{"task_id":id}});
            if let Some(kind) = task["task_type"].as_str().filter(|s| s.len() <= 128) {
                value["kind"] = json!(kind);
            }
            if let Some(child) = &row.child {
                value["child"] = json!(child);
            }
            if let Some(run) = task.get("run_id").and_then(native_id) {
                value["native"]["run_id"] = json!(run);
                row.value["native"]["level_run_id"] = json!(run);
            } else if task.get("run_id").is_some() {
                value["incomplete"] = json!(true);
            }
            background.push(value);
            changes.extend(self.changes(id));
        }
        for (native, row) in &mut self.tasks {
            if !members.contains(native) && row.value["native"]["in_background_level"] == true {
                row.value["native"]["in_background_level"] = json!(false);
                if !row.terminal {
                    row.value["status"] = json!("unknown");
                    if row.child.is_some() {
                        row.child_value["status"] = json!("unknown");
                    }
                }
                changes.push(Change {
                    kind: "task",
                    id: row.id.clone(),
                    value: row.value.clone(),
                });
                if let Some(id) = &row.child {
                    changes.push(Change {
                        kind: "child",
                        id: id.clone(),
                        value: row.child_value.clone(),
                    });
                }
            }
        }
        if !complete {
            self.incomplete = true;
        }
        (changes, complete.then_some(background))
    }
    /// Positive/possibly-background live work revokes a snapshot that does not cover it.
    /// This is NOT a synthesized membership set: only a later native level restores completeness.
    pub fn uncovers_background(&self, frame: &Value, background: Option<&Vec<Value>>) -> bool {
        let direct = frame
            .get("is_backgrounded")
            .or_else(|| frame["patch"].get("is_backgrounded"));
        let id = native_id(&frame["task_id"]);
        let row = id.and_then(|id| self.tasks.get(id));
        let run = frame.get("run_id").and_then(native_id);
        if let (Some(row), Some(run)) = (row, run) {
            if let Some(current) = row.run.as_deref() {
                if run < current || (run == current && row.terminal) {
                    return false;
                }
            }
        }
        let bg = direct.or_else(|| row.and_then(|row| row.value["native"].get("is_backgrounded")));
        if bg == Some(&Value::Bool(false)) {
            return false;
        }
        let active = direct == Some(&Value::Bool(true))
            || matches!(
                frame["subtype"].as_str(),
                Some("task_started" | "task_progress")
            )
            || matches!(
                frame["patch"]["status"].as_str(),
                Some("pending" | "running" | "paused")
            );
        if !active {
            return false;
        }
        !background.is_some_and(|rows| {
            rows.iter().any(|row| {
                id.is_some_and(|id| row["native"]["task_id"].as_str() == Some(id))
                    && run.is_none_or(|run| row["native"]["run_id"] == run)
            })
        })
    }
    pub fn cancel_target(&self, kind: &str, id: &str) -> Option<&str> {
        self.tasks
            .iter()
            .find(|(_, row)| {
                if kind == "child" {
                    row.child.as_deref() == Some(id)
                } else {
                    kind == "task" && row.id == id
                }
            })
            .filter(|(_, row)| !row.terminal || row.value["native"]["in_background_level"] == true)
            .map(|(id, _)| id.as_str())
    }
}
