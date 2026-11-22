//! Native hook inventory projection. A snapshot never certifies no future input transforms.
use super::Endpoint;
use serde_json::{Value, json};

pub(super) struct Hooks {
    pub entries: Option<Vec<Value>>,
    pub partial: bool,
    pub input_may: bool,
    pub names: Vec<String>,
}
fn bounded(value: &Value, limit: usize) -> Option<&str> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= limit && !s.chars().any(char::is_control))
}

pub(super) fn project(response: Option<&Value>) -> Hooks {
    let mut result = Hooks {
        entries: None,
        partial: true,
        input_may: false,
        names: Vec::new(),
    };
    let Some(groups) = response
        .and_then(|r| r["data"].as_array())
        .filter(|a| a.len() == 1)
    else {
        return result;
    };
    let group = &groups[0];
    if group["cwd"].as_str().is_none() || group["errors"].as_array().is_none_or(|a| !a.is_empty()) {
        return result;
    }
    let Some(warnings) = group["warnings"].as_array() else {
        return result;
    };
    let Some(rows) = group["hooks"].as_array() else {
        return result;
    };
    result.partial = !warnings.is_empty() || rows.len() > 256;
    let mut entries = Vec::new();
    for row in rows.iter().take(256) {
        let supported = match row["handlerType"].as_str() {
            Some("command") => row["command"].is_string(),
            Some("mcpTool") => row["server"].is_string() && row["tool"].is_string(),
            _ => false,
        };
        let input = row["eventName"] == "userPromptSubmit";
        let eligible = supported
            && input
            && row["key"].as_str().is_some_and(|s| !s.is_empty())
            && row["currentHash"].as_str().is_some_and(|s| !s.is_empty())
            && row["isManaged"].is_boolean()
            && row["enabled"] == true
            && matches!(row["trustStatus"].as_str(), Some("trusted" | "managed"));
        // A valid positive can be known even when its over-bound identifier cannot be published.
        result.input_may |= eligible;
        let Some(name) = bounded(&row["key"], 128) else {
            result.partial = true;
            continue;
        };
        if eligible && result.names.len() < 16 && !result.names.iter().any(|s| s == name) {
            result.names.push(name.into());
        }
        let mut entry = json!({"name":name,"kind":"hook"});
        if let Some(path) = bounded(&row["sourcePath"], 4096) {
            entry["path"] = json!(path);
        } else {
            result.partial = true;
        }
        match row["source"].as_str() {
            Some("user") => entry["scope"] = json!("user"),
            Some("project") => entry["scope"] = json!("project"),
            Some("sessionFlags") => entry["scope"] = json!("session"),
            _ => {}
        }
        if row["enabled"] == true {
            entry["state"] = json!("enabled");
        }
        if supported {
            match row["eventName"].as_str() {
                Some("userPromptSubmit") => entry["transforms"] = json!(["input"]),
                Some("preToolUse") => entry["transforms"] = json!(["tool_calls"]),
                _ => {} // No guessed transform classification for other/future events.
            }
        }
        entries.push(entry);
    }
    result.entries = Some(entries);
    result
}

impl Endpoint {
    pub(super) fn observe_input_transform(&mut self, names: &[String]) {
        let mut known: Vec<String> = self
            .input_transform
            .as_ref()
            .and_then(|v| v["names"].as_array())
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        for name in names {
            if known.len() == 16 {
                break;
            }
            if !known.contains(name) {
                known.push(name.clone());
            }
        }
        let mut value = json!({"state":"may","names_complete":false});
        if !known.is_empty() {
            value["names"] = json!(known);
        }
        // No hooks/changed invalidation exists in0.170. Keep positive possibility until new epoch;
        // empty/error inventories do not certify absence or completeness.
        self.input_transform = Some(value);
    }
}
