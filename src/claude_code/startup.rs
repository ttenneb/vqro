//! Bounded native startup observations. Catalogs are available, not proof their bodies were loaded.
use std::collections::BTreeSet;
use serde_json::{Value, json};
pub const BUDGET: usize = 48 * 1024;
#[derive(Default)]
pub struct Startup { value: Value, incomplete: BTreeSet<String>, pub plugins_observed: bool }
impl Startup {
    pub fn new() -> Self {
        Self { value:json!({"runtime":{"name":"Claude Code"},"sources":{"runtime":"vqro:adapter"}}), ..Self::default() }
    }
    fn string(&mut self, target: &mut Value, key: &str, value: &Value, limit: usize, field: &str) {
        if let Some(text) = value.as_str() {
            if text.len() <= limit { target[key] = json!(text); }
            else { self.incomplete.insert(field.into()); }
        }
    }
    pub fn initialize(&mut self, data: &Value) {
        if let Some(version) = data["claude_code_version"].as_str() {
            if version.len() <= 128 {
                self.value["runtime"]["version"] = json!(version);
                self.value["runtime"]["version_source"] = json!("rpc:initialize");
            } else { self.incomplete.insert("runtime".into()); }
        }
        if let Some(commands) = data["commands"].as_array() {
            self.catalog("commands", commands, "rpc:initialize", false);
        }
        self.fit();
    }
    fn catalog(&mut self, field: &str, rows: &[Value], source: &str, strings: bool) {
        let mut entries = Vec::new();
        if rows.len() > 256 { self.incomplete.insert(field.into()); }
        for row in rows.iter().take(256) {
            let name = if strings { row } else { &row["name"] };
            let Some(name) = name.as_str().filter(|name| !name.is_empty() && name.len() <= 128) else {
                self.incomplete.insert(field.into()); continue;
            };
            let mut entry = json!({"name":name});
            if !strings {
                for (key, limit) in [("description",1024),("path",4096),("status",64),("version",128)] {
                    self.string(&mut entry,key,&row[key],limit,field);
                }
            }
            entries.push(entry);
        }
        self.value[field] = json!(entries);
        self.value["sources"][field] = json!(source);
    }
    pub fn init(&mut self, frame: &Value) {
        for (field, limit) in [("model",128),("cwd",4096)] {
            if let Some(text) = frame[field].as_str() {
                if text.len() <= limit { self.value[field] = json!(text); self.value["sources"][field] = json!("native:system/init"); }
                else { self.incomplete.insert(field.into()); }
            }
        }
        if let Some(version) = frame["claude_code_version"].as_str() {
            if version.len() <= 128 { self.value["runtime"]["version"] = json!(version); self.value["runtime"]["version_source"] = json!("native:system/init"); }
            else { self.incomplete.insert("runtime".into()); }
        }
        for (field, native, strings) in [("skills","skills",true),("commands","slash_commands",true),("plugins","plugins",false),("mcp","mcp_servers",false)] {
            if let Some(rows) = frame[native].as_array() {
                self.catalog(field,rows,"native:system/init",strings);
                if field == "skills" { self.value["skills_scope"] = json!("available"); }
                if field == "plugins" {
                    self.plugins_observed = true;
                    self.value["extensions"] = self.value["plugins"].clone();
                    if let Some(entries) = self.value["extensions"].as_array_mut() {
                        for entry in entries { entry["kind"] = json!("plugin"); entry["state"] = json!("active"); }
                    }
                    self.value["sources"]["extensions"] = json!("native:system/init");
                    // This is the loaded plugin list, not a complete hook/mod transformer inventory.
                    self.incomplete.insert("extensions".into());
                }
            }
        }
        if frame["plugin_errors"].as_array().is_some_and(|errors| !errors.is_empty()) { self.incomplete.insert("plugins".into()); }
        self.fit();
    }
    fn fit(&mut self) {
        self.value["incomplete"] = json!(self.incomplete);
        for field in ["extensions","plugins","skills","commands","mcp"] {
            while crate::limits::encoded_bytes(&self.value) > BUDGET {
                let removed = self.value[field].as_array_mut().and_then(Vec::pop).is_some();
                if !removed { break; }
                self.incomplete.insert(field.into()); self.value["incomplete"] = json!(self.incomplete);
            }
        }
    }
    pub fn value(&self) -> Value { self.value.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_native_paths_are_omitted_not_shortened() {
        let mut startup = Startup::new();
        startup.init(&json!({"plugins":[{"name":"plugin","path":"x".repeat(4097)}],"skills":["available-skill"]}));
        let value = startup.value();
        assert!(value["plugins"][0].get("path").is_none());
        assert_eq!(value["skills_scope"],"available");
        assert!(value["incomplete"].as_array().unwrap().contains(&json!("plugins")));
        assert!(value["extensions"][0].get("transforms").is_none());
    }
}
