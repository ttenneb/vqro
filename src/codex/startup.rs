//! Bounded native startup snapshots, not filesystem discovery or current settings telemetry.
use super::{Change, Endpoint};
use serde_json::{Value, json};

// Leave space for the journal record envelope/position and other retained controls.
const BYTES: usize = 48 * 1024;
const ENTRIES: usize = 256;

#[derive(Default)]
pub(super) struct Startup {
    identity: Option<Value>,
    identity_incomplete: bool,
    value: Value,
}

fn text(value: &Value, bytes: usize, partial: &mut bool) -> Option<String> {
    let value = value.as_str()?;
    if value.is_empty() || value.chars().any(char::is_control) {
        *partial = true;
        return None;
    }
    if value.len() > bytes {
        *partial = true;
        return None;
    }
    Some(value.into())
}

// Paths are actionable identities: never turn a native path into a different truncated path.
fn native_path(value: &Value, partial: &mut bool) -> Option<String> {
    if value.as_str().is_some_and(|s| s.len() > 4096) {
        *partial = true;
        return None;
    }
    text(value, 4096, partial)
}

impl Startup {
    fn incomplete(&mut self, field: &str) {
        if !self.value["incomplete"].is_array() {
            self.value["incomplete"] = json!([]);
        }
        let fields = self.value["incomplete"].as_array_mut().unwrap();
        if !fields.iter().any(|v| v == field) {
            fields.push(json!(field));
        }
    }
    fn source(&mut self, field: &str, source: &str) {
        if !self.value["sources"].is_object() {
            self.value["sources"] = json!({});
        }
        self.value["sources"][field] = json!(source);
    }
    fn bound(&mut self) {
        let mut total = crate::limits::encoded_bytes(&self.value);
        if total <= BYTES {
            return;
        }
        let mut sizes: Vec<_> = ["instructions", "skills", "plugins", "mcp"]
            .into_iter()
            .map(|field| (field, crate::limits::encoded_bytes(&self.value[field])))
            .collect();
        // Count each payload once, rather than re-encoding a large native inventory per pop.
        // The reserve covers newly added incomplete markers and the journal envelope.
        while total > BYTES - 1024 {
            let Some((field, size)) = sizes
                .iter_mut()
                .filter(|(field, _)| self.value[*field].as_array().is_some_and(|a| !a.is_empty()))
                .max_by_key(|(_, size)| *size)
            else {
                break;
            };
            let array = self.value[*field].as_array_mut().unwrap();
            let removed = crate::limits::encoded_bytes(&array.pop().unwrap())
                + usize::from(!array.is_empty());
            *size = size.saturating_sub(removed);
            total = total.saturating_sub(removed);
            self.incomplete(field);
        }
    }
    fn capture(&mut self, response: &Value, source: &str) {
        self.value = json!({});
        if let Some(identity) = &self.identity {
            self.value["runtime"] = identity.clone();
            self.source("runtime", "rpc:initialize");
        }
        if self.identity_incomplete {
            self.incomplete("runtime");
        }
        let mut partial = false;
        for (field, limit) in [("model", 128), ("cwd", 4096)] {
            let reported = if field == "cwd" {
                native_path(&response[field], &mut partial)
            } else {
                text(&response[field], limit, &mut partial)
            };
            if let Some(value) = reported {
                self.value[field] = json!(value);
                self.source(field, source);
            }
            if partial {
                self.incomplete(field);
                partial = false;
            }
        }
        if let Some(paths) = response["instructionSources"].as_array() {
            let mut entries = Vec::new();
            partial = paths.len() > ENTRIES;
            for path in paths.iter().take(ENTRIES) {
                if let Some(path) = native_path(path, &mut partial) {
                    entries.push(json!({"path":path}));
                } else {
                    partial = true;
                }
            }
            self.value["instructions"] = json!(entries);
            self.source("instructions", source);
            if partial {
                self.incomplete("instructions");
            }
        }
        self.bound();
    }
    fn inventory(&mut self, field: &str, response: Option<&Value>, cwd: Option<&str>) {
        let source = match field {
            "skills" => "rpc:skills/list",
            "plugins" => "rpc:plugin/installed",
            "mcp" => "rpc:mcpServerStatus/list",
            _ => return,
        };
        // Failure never exposes diagnostic/config/account payloads or fabricates an empty list.
        let rows = response.and_then(|response| match field {
            "skills" => response["data"].as_array().and_then(|groups| {
                let group = groups.iter().find(|g| g["cwd"].as_str() == cwd)?;
                if group["errors"].as_array().is_none_or(|a| !a.is_empty()) {
                    return None;
                }
                group["skills"].as_array().cloned()
            }),
            "plugins" => response["marketplaces"].as_array().and_then(|groups| {
                if response
                    .get("marketplaceLoadErrors")
                    .is_some_and(|e| e.as_array().is_none_or(|a| !a.is_empty()))
                {
                    return None;
                }
                let mut rows = Vec::new();
                for group in groups {
                    if group["plugins"].as_array().is_none() {
                        return None;
                    }
                    rows.extend(group["plugins"].as_array()?.iter().cloned());
                }
                Some(rows)
            }),
            "mcp" => response["data"].as_array().cloned(),
            _ => None,
        });
        let Some(rows) = rows else {
            self.value.as_object_mut().unwrap().remove(field);
            if field == "skills" {
                self.value.as_object_mut().unwrap().remove("skills_scope");
            }
            if let Some(sources) = self.value["sources"].as_object_mut() {
                sources.remove(field);
                if field == "skills" {
                    sources.remove("skills_scope");
                }
            }
            self.incomplete(field);
            return;
        };
        let mut entries = Vec::new();
        let mut partial = response.is_some_and(|r| r["_startup_partial"] == true);
        for row in &rows {
            if field == "plugins" && row["installed"] != true {
                if row["installed"].as_bool().is_none() {
                    partial = true;
                }
                continue;
            }
            if field == "skills" && row["enabled"] != true {
                if row["enabled"].as_bool().is_none() {
                    partial = true;
                }
                continue;
            }
            if entries.len() == ENTRIES {
                partial = true;
                break;
            }
            let Some(name) = text(&row["name"], 128, &mut partial) else {
                partial = true;
                continue;
            };
            let mut entry = json!({"name":name});
            if field == "mcp" {
                // Null connection state means unavailable; it is not an invented 'ready'/'unknown'.
                let Some(status) = text(&row["runtimeStatus"], 64, &mut partial) else {
                    partial = true;
                    continue;
                };
                entry["status"] = json!(status);
            } else if field == "plugins" {
                if let Some(enabled) = row["enabled"].as_bool() {
                    entry["enabled"] = json!(enabled);
                }
                if let Some(version) = text(&row["localVersion"], 128, &mut partial) {
                    entry["version"] = json!(version);
                }
                if matches!(row["source"]["type"].as_str(), Some("local" | "git")) {
                    if let Some(path) = native_path(&row["source"]["path"], &mut partial) {
                        entry["path"] = json!(path);
                    }
                }
            } else {
                for (key, limit) in [("description", 1024), ("path", 4096)] {
                    let reported = if key == "path" {
                        native_path(&row[key], &mut partial)
                    } else {
                        text(&row[key], limit, &mut partial)
                    };
                    if let Some(value) = reported {
                        entry[key] = json!(value);
                    }
                }
            }
            entries.push(entry);
        }
        self.value[field] = json!(entries);
        self.source(field, source);
        if field == "skills" {
            self.value["skills_scope"] = json!("available");
            self.source("skills_scope", source);
        }
        if partial {
            self.incomplete(field);
        }
        self.bound();
    }
}

impl Endpoint {
    /// Raw native agent identity; never semver-parsed or replaced by vqro's version.
    pub fn initialize_startup(&mut self, response: &Value) -> Vec<Change> {
        let mut partial = false;
        if let Some(name) = text(&response["userAgent"], 128, &mut partial) {
            self.startup.identity = Some(json!({"name":name}));
            self.startup.identity_incomplete = partial;
            if !self.startup.value.is_object() {
                self.startup.value = json!({});
            }
            self.startup.value["runtime"] = self.startup.identity.clone().unwrap();
            self.startup.source("runtime", "rpc:initialize");
            if partial {
                self.startup.incomplete("runtime");
            }
            vec![self.startup_change()]
        } else if partial {
            self.startup.identity_incomplete = true;
            if !self.startup.value.is_object() {
                self.startup.value = json!({});
            }
            self.startup.incomplete("runtime");
            vec![self.startup_change()]
        } else {
            Vec::new()
        }
    }
    pub(super) fn startup_change(&self) -> Change {
        Change::new("startup", "current", self.startup.value.clone())
    }
    pub(super) fn capture_startup(&mut self, response: &Value, source: &str) -> Change {
        self.startup.capture(response, source);
        self.startup_change()
    }
    pub fn finish_startup_inventory(
        &mut self,
        thread: &str,
        epoch: &str,
        field: &str,
        response: Option<&Value>,
    ) -> Vec<Change> {
        if self.lifecycle == "ended"
            || self.session.as_deref() != Some(thread)
            || self.epoch != epoch
            || !self.startup.value.is_object()
        {
            return Vec::new();
        }
        let cwd = self.startup.value["cwd"].as_str().map(str::to_owned);
        self.startup.inventory(field, response, cwd.as_deref());
        vec![self.startup_change()]
    }
}
