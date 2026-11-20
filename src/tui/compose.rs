//! Generic read-only panel references: keep live sources independent of their presentation shell.
use std::collections::{BTreeMap, HashMap, HashSet};
use serde_json::{json, Value};

fn readonly(row: &Value) -> Value {
    if !row.is_object() { return json!({"type":"text","text":row.as_str().map(String::from).unwrap_or_else(||row.to_string())}); }
    let mut result = serde_json::Map::new();
    for key in ["type", "text", "style", "description", "detail", "detail_style", "badges", "id", "expanded", "wrap", "focusable"] {
        if let Some(value) = row.get(key) { result.insert(key.into(), value.clone()); }
    }
    for key in ["text", "description", "detail"] {
        if let Some(text) = result.get(key).and_then(Value::as_str) {
            let safe: String = text.chars().take(8192).map(|c| if c.is_control() && !matches!(c, '\n'|'\t') { c.escape_default().to_string() } else { c.to_string() }).collect();
            result.insert(key.into(), Value::String(safe));
        }
    }
    if let Some(badges) = row["badges"].as_array() {
        result.insert("badges".into(), badges.iter().filter(|b| b.is_object()).map(|b|json!({"text":b["text"],"style":b["style"]})).collect());
    }
    if let Some(children) = row["children"].as_array() {
        result.insert("children".into(), children.iter().map(readonly).collect());
    }
    Value::Object(result)
}

/// Missing/cyclic/terminal sources remain visible placeholders; no attachment or effect is created.
pub fn resolve(view: &str, entries: &BTreeMap<String, Value>, live: &HashMap<String, Value>) -> (Value, HashSet<String>) {
    fn value(view: &str, entries: &BTreeMap<String, Value>, live: &HashMap<String, Value>) -> Value {
        let fallback = if live.get(view).is_some_and(|v| v["items"].is_array()) { json!({"kind":"panel"}) } else { json!({}) };
        let mut result = entries.get(&format!("view/{view}")).cloned().unwrap_or(fallback);
        if let (Some(source), Some(target)) = (live.get(view).and_then(Value::as_object), result.as_object_mut()) {
            for (key, value) in source { target.insert(key.clone(), value.clone()); }
        }
        result
    }
    struct Context<'a> { entries: &'a BTreeMap<String, Value>, live: &'a HashMap<String, Value>, deps: HashSet<String>, seen: HashSet<String>, remaining: usize }
    impl Context<'_> {
        fn rows(&mut self, rows: &[Value], depth: usize) -> Vec<Value> {
            let mut result = Vec::new();
            for row in rows {
                if self.remaining == 0 { result.push(json!({"type":"text","style":"warn","text":"Composed panel row limit reached."})); break; }
                self.remaining -= 1;
                if row["type"] == "view" {
                    let source = row["view"].as_str().unwrap_or("");
                    self.deps.insert(source.into());
                    let mut panel = value(source, self.entries, self.live);
                    // View-kind fallback (D29): a source this client can't render is embedded as its fallback panel.
                    for _ in 0..crate::ui_model::view::MAX_CHAIN {
                        if panel["kind"] == "panel" && super::panel::missing(&panel).is_empty() { break; }
                        let Some(next) = panel["fallback"].as_str().map(String::from) else { break };
                        self.deps.insert(next.clone());
                        panel = value(&next, self.entries, self.live);
                    }
                    if depth >= 8 || self.seen.contains(source) || panel["kind"] != "panel" || !panel["items"].is_array() || !super::panel::missing(&panel).is_empty() {
                        result.push(json!({"type":"text","style":"dim","text":row["text"].as_str().unwrap_or("Referenced panel unavailable or unsupported.")}));
                    } else {
                        self.seen.insert(source.into());
                        let embedded = self.rows(panel["items"].as_array().map(Vec::as_slice).unwrap_or(&[]), depth+1);
                        self.seen.remove(source);
                        result.extend(embedded.iter().map(readonly));
                    }
                } else {
                    let mut row = row.clone();
                    if let Some(children) = row["children"].as_array() { row["children"] = Value::Array(self.rows(children, depth+1)); }
                    result.push(row);
                }
            }
            result
        }
    }
    let mut result = value(view, entries, live);
    let mut context = Context { entries, live, deps:HashSet::new(), seen:HashSet::from([view.into()]), remaining:2048 };
    if let Some(items) = result["items"].as_array() { result["items"] = Value::Array(context.rows(items, 0)); }
    (result, context.deps)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn references_are_readonly_cycle_bounded_and_never_attach_terminals() {
        let entries = BTreeMap::from([
            ("view/root".into(),json!({"kind":"panel","items":[{"type":"view","view":"source","readonly":true}]})),
            ("view/source".into(),json!({"kind":"panel","items":[]})),
            ("view/term".into(),json!({"kind":"terminal","resource":"r1"}))]);
        let live = HashMap::from([("source".into(),json!({"items":[{"text":"native","command":"forbidden","args":{},"children":[{"text":"nested","command":"bad"}]},{"type":"view","view":"root"},{"type":"view","view":"term"}]}))]);
        let (panel, deps) = resolve("root",&entries,&live);
        assert_eq!(panel["items"][0]["text"],"native");
        assert!(panel["items"][0].get("command").is_none());
        assert!(panel["items"][0]["children"][0].get("command").is_none());
        assert_eq!(panel["items"].as_array().unwrap().len(),3);
        assert!(deps.contains("source") && deps.contains("term"));
    }
}
