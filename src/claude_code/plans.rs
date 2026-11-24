//! Observed tool OUTPUT only. Never turn model intent into a plan, tool invocation or authority.
use super::work::{Change, native_id};
use serde_json::{Value, json};
use std::collections::BTreeMap;
#[cfg(test)]
#[path = "plans_tests.rs"]
mod tests;
const LISTS: usize = 64;
const ITEMS: usize = 256;
const BYTES: usize = 48 * 1024;
struct List {
    id: String,
    value: Value,
}
#[derive(Default)]
pub struct Plans {
    serial: u64,
    lists: BTreeMap<(String, String), List>,
    pub incomplete: bool,
}
fn status(s: &str) -> &'static str {
    match s {
        "pending" => "pending",
        "in_progress" => "in_progress",
        "completed" => "completed",
        "cancelled" => "cancelled",
        _ => "unknown",
    }
}
fn field(out: &mut Value, src: &Value, name: &str, key: &str, max: usize, cut: &mut bool) {
    if let Some(v) = src.get(name) {
        if let Some(s) = v.as_str().filter(|s| s.len() <= max) {
            out[key] = json!(s);
        } else {
            *cut = true;
        }
    }
}
fn item(src: &Value, todo: bool) -> Option<(Value, bool)> {
    let text = src[if todo { "content" } else { "subject" }]
        .as_str()
        .filter(|s| s.len() <= 2048)?;
    let mut value = json!({"text":text,"status":"unknown","native":{}});
    let mut cut = false;
    if let Some(s) = src["status"].as_str().filter(|s| s.len() <= 128) {
        value["status"] = json!(status(s));
        value["native"]["status"] = json!(s);
    } else {
        cut = true;
    }
    if !todo {
        let id = native_id(&src["id"])?;
        value["id"] = json!(id);
        field(
            &mut value,
            src,
            "description",
            "description",
            4096,
            &mut cut,
        );
        field(&mut value["native"], src, "owner", "owner", 128, &mut cut);
        if let Some(rows) = src.get("blockedBy") {
            if let Some(rows) = rows.as_array() {
                let mut ids = vec![];
                for id in rows.iter().take(64) {
                    if let Some(id) = native_id(id) {
                        ids.push(json!(id));
                    } else {
                        cut = true;
                    }
                }
                cut |= rows.len() > 64;
                value["blocked_by"] = json!(ids);
            } else {
                cut = true;
            }
        }
    } else {
        field(&mut value, src, "activeForm", "active_form", 2048, &mut cut);
    }
    if cut {
        value["incomplete"] = json!(true);
    }
    Some((value, cut))
}
impl Plans {
    fn ensure(&mut self, key: &(String, String)) -> bool {
        if self.lists.contains_key(key) {
            return true;
        }
        if self.lists.len() >= LISTS {
            self.incomplete = true;
            return false;
        }
        self.serial += 1;
        self.lists.insert(
            key.clone(),
            List {
                id: format!("plan{}", self.serial),
                value: json!({"items":[],"update":"whole","source":"tool","incomplete":true}),
            },
        );
        true
    }
    pub fn observe(
        &mut self,
        name: &str,
        tool: &str,
        child: Option<&str>,
        output: &Value,
        error: bool,
    ) -> Vec<Change> {
        let todo = name == "TodoWrite";
        if !todo && !matches!(name, "TaskList" | "TaskCreate" | "TaskGet" | "TaskUpdate") {
            return vec![];
        }
        // Native lists have no wire list owner/id. Namespace observed snapshots by producer context;
        // do NOT claim that producing a shared TaskList makes that child the list's owner.
        let key = (
            child.unwrap_or("").to_owned(),
            if todo { "todos" } else { "tasks" }.to_owned(),
        );
        if error {
            self.incomplete = true;
            if !self.lists.contains_key(&key) {
                return vec![];
            }
        }
        let whole = if !error && todo {
            output.get("newTodos").and_then(Value::as_array)
        } else if !error && name == "TaskList" {
            output.get("tasks").and_then(Value::as_array)
        } else {
            None
        };
        let single = if !error && matches!(name, "TaskCreate" | "TaskGet") {
            output.get("task").filter(|v| v.is_object())
        } else {
            None
        };
        if whole.is_none() && single.is_none() && !self.lists.contains_key(&key) {
            self.incomplete = true;
            return vec![];
        }
        if !self.ensure(&key) {
            return vec![];
        }
        let list = self.lists.get_mut(&key).unwrap();
        let mut cut;
        list.value["tool"] = json!(tool);
        list.value["native"] = json!({"tool_name":name});
        if let Some(child) = child {
            list.value["native"]["observer_child"] = json!(child);
        }
        if let Some(rows) = whole {
            let mut items = vec![];
            let mut seen = std::collections::BTreeSet::new();
            cut = rows.len() > ITEMS;
            for row in rows.iter().take(ITEMS) {
                let Some((value, partial)) = item(row, todo) else {
                    cut = true;
                    continue;
                };
                if !todo && !seen.insert(value["id"].as_str().unwrap().to_owned()) {
                    cut = true;
                    continue;
                }
                let mut candidate = items.clone();
                candidate.push(value.clone());
                if crate::limits::encoded_bytes(&json!(candidate)) > BYTES {
                    cut = true;
                    break;
                }
                items.push(value);
                cut |= partial;
            }
            list.value["items"] = json!(items);
            list.value["update"] = json!("whole");
            list.value["incomplete"] = json!(cut);
        } else if let Some(row) = single {
            cut = true;
            list.value["update"] = json!("items");
            if let Some((value, partial)) = item(row, false) {
                let items = list.value["items"].as_array_mut().unwrap();
                if let Some(at) = items.iter().position(|v| v["id"] == value["id"]) {
                    items.remove(at);
                }
                if items.len() < ITEMS
                    && crate::limits::encoded_bytes(&json!(items))
                        + crate::limits::encoded_bytes(&value)
                        <= BYTES
                {
                    items.push(value);
                } else {
                    cut = true;
                }
                cut |= partial;
            } else {
                if let Some(id) = native_id(&row["id"]) {
                    list.value["items"]
                        .as_array_mut()
                        .unwrap()
                        .retain(|v| v["id"] != id);
                }
                cut = true;
            }
            // A single read cannot prove the list inventory, nor clear unknown mutations from earlier.
        } else {
            list.value["update"] = json!(if todo || name == "TaskList" {
                "whole"
            } else {
                "items"
            });
            cut = true;
            // This is reported ACTUAL status change, not the TaskUpdate input (hooks may change it).
            if !error && name == "TaskUpdate" && output["success"] == true {
                if let (Some(id), Some(to)) = (
                    native_id(&output["taskId"]),
                    output["statusChange"]["to"]
                        .as_str()
                        .filter(|s| s.len() <= 128),
                ) {
                    if let Some(item) = list.value["items"]
                        .as_array_mut()
                        .unwrap()
                        .iter_mut()
                        .find(|v| v["id"] == id)
                    {
                        item["status"] = json!(status(to));
                        item["native"]["status"] = json!(to);
                    }
                }
            }
        }
        if cut {
            list.value["incomplete"] = json!(true);
            self.incomplete = true;
        }
        vec![Change {
            kind: "plan",
            id: list.id.clone(),
            value: list.value.clone(),
        }]
    }
    pub fn goal(&mut self, frame: &Value) -> Vec<Change> {
        let Some(raw) = frame.get("value") else {
            self.incomplete = true;
            return vec![];
        };
        let mut value =
            json!({"owner":null,"status":"unknown","native":{"event":"active_goal","status":null}});
        if raw.is_null() {
            value["native"]["cleared"] = json!(true);
        } else if raw.is_object() {
            value["status"] = json!("active");
            value["native"]["status"] = json!("active_goal");
            let mut cut = false;
            field(&mut value, raw, "condition", "objective", 4096, &mut cut);
            field(
                &mut value["native"],
                raw,
                "last_reason",
                "last_reason",
                2048,
                &mut cut,
            );
            for key in ["iterations", "set_at", "tokens_at_start"] {
                if let Some(n) = raw[key].as_u64() {
                    value["native"][key] = json!(n);
                } else if raw.get(key).is_some() {
                    cut = true;
                }
            }
            if cut || value.get("objective").is_none() {
                value["incomplete"] = json!(true);
                self.incomplete = true;
            }
        } else {
            value["incomplete"] = json!(true);
            self.incomplete = true;
        }
        vec![Change {
            kind: "goal",
            id: "current".into(),
            value,
        }]
    }
}
