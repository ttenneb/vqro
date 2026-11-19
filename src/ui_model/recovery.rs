//! The shared recovery content (D15, D19, D21): one definition every client renders through the base panel
//! profile. It is generated here from the workspace cache, never published or supplied by an extension, so it
//! works with every extension broken or in safe mode. Non-Rust clients implement the same content against
//! `tests/fixtures/client/recovery.json`.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::keys;
use super::workspace::{builtin_bindings, resolve, Workspace};

/// The view id clients use for the built-in safe-mode chooser popup.
pub const SAFE_MODE_VIEW: &str = "@vqro-safe-mode";

/// A row of the prefix menu: the key that runs it (None for a recovery action whose key now does something
/// else), its label, and the binding it runs.
#[derive(Debug, Clone, PartialEq)]
pub struct PrefixRow {
    pub key: Option<String>,
    pub label: String,
    pub binding: Value,
}

/// The prefix menu in `ctx` (the focused pane's context): the winning binding of every key (defaults first),
/// restart rows for failed extensions, then any recovery action no key reaches any more, so recovery survives
/// every keymap. Keys are canonical names; `key_label` formats them for display.
pub fn prefix_rows(ws: &Workspace, ctx: &Value, key_label: &dyn Fn(&str) -> String) -> Vec<PrefixRow> {
    let cands = ws.key_candidates(ctx);
    let winners = resolve(&cands);
    let mut rows: Vec<PrefixRow> = cands
        .iter()
        .enumerate()
        .filter_map(|(i, (_, b))| {
            let key = keys::normalize(b["key"].as_str()?)?;
            (winners.get(&key) == Some(&i) && b["command"] != "client.none" && b["hidden"] != true).then(|| PrefixRow { key: Some(key_label(&key)), label: ws.binding_label(b), binding: b.clone() })
        })
        .collect();
    // Non-replaceable recovery: failed Settings/other extensions cannot hide their restart.
    let failed = ws.extensions.iter().filter(|(_, state)| state["status"] == "failed").map(|(name, _)| {
        let allowed = if ws.safe_mode { ws.safe_mode_allow.contains(name) } else { ws.entries.get(&format!("extension/{name}")).is_some_and(|v| v["enabled"] != false) };
        let label = if allowed { format!("restart {name} (failed)") } else { format!("clear failure: {name} (stays stopped)") };
        PrefixRow { key: None, label, binding: json!({"command": "vqro.extension.restart", "args": {"name": name}}) }
    }).collect::<Vec<_>>();
    let at = rows.len().min(1);
    rows.splice(at..at, failed);
    for b in builtin_bindings().into_iter().filter(|b| b["recovery"] == true) {
        if !rows.iter().any(|r| r.binding["command"] == b["command"]) {
            rows.push(PrefixRow { key: None, label: ws.binding_label(&b), binding: b });
        }
    }
    rows
}

/// The safe-mode chooser as panel data: declared extensions grouped into providers/backends and UI extensions,
/// each with a checkbox for this session's recovery set (`selected`: chosen but not yet applied), its description,
/// a failure reason and what it needs (display only, D29), then Enable selected and Leave safe mode.
pub fn safe_chooser(ws: &Workspace, selected: &BTreeSet<String>) -> Value {
    let mut declared: Vec<_> = ws.entries.iter().filter_map(|(key, value)| key.strip_prefix("extension/").map(|name| (name, value))).collect();
    declared.sort_by_key(|(name, _)| *name);
    let mut items = vec![json!({"type":"text","text":"Nothing starts automatically. Choose this session's recovery set; saved enabled settings are unchanged."})];
    for (provider, title) in [(true, "Providers / backends"), (false, "UI extensions")] {
        items.push(json!({"type":"header","text":title}));
        let mut count = 0;
        for (name, value) in &declared {
            if (value["kind"] == "provider" || ws.provider_extensions.contains(*name)) != provider { continue; }
            let check = if selected.contains(*name) { "[x]" } else { "[ ]" };
            let mut description = value["description"].as_str().or_else(|| value["argv"][0].as_str()).unwrap_or("Declared extension").chars().take(160).collect::<String>();
            if let Some(state) = ws.extensions.get(*name) {
                if state["status"] == "failed" { description.push_str(&format!(" -- failed: {}", state["last_exit"].as_str().unwrap_or("unknown failure"))); }
                // D29: the chooser only names what an extension needs; it offers nothing and selects nothing.
                let needs = needs_text(&state["dependencies"]);
                if !needs.is_empty() { description.push_str(&format!(" -- needs {needs}")); }
            }
            items.push(json!({"id":format!("safe/{name}"),"text":format!("{check} {name}"),"description":description,"command":"client.safe_mode.toggle","args":{"name":name}}));
            count += 1;
        }
        if count == 0 { items.push(json!({"type":"text","text":"(none declared)"})); }
    }
    items.push(json!({"type":"separator"}));
    items.push(json!({"id":"safe/apply","text":"Enable selected","command":"client.safe_mode.apply"}));
    items.push(json!({"id":"safe/leave","text":"Leave safe mode","command":"client.safe_mode.leave"}));
    json!({"title":"Safe mode recovery","items":items})
}

/// Who an extension's non-UI needs name, from the core's dependency facts (D29): `tabs, herdr or tmux`.
/// Needs the core itself provides are left out. Display only.
pub fn needs_text(dependencies: &Value) -> String {
    let mut out: Vec<String> = Vec::new();
    for need in dependencies["needs"].as_array().into_iter().flatten() {
        if need["cap"].as_str().is_none_or(|c| c.starts_with("ui/")) { continue; }
        let who: Vec<&str> = need["providers"].as_array().into_iter().flatten().filter_map(|p| p["name"].as_str()).collect();
        if who.contains(&"vqro") { continue; }
        let text = if who.is_empty() { need["suggest"].as_str().or(need["cap"].as_str()).unwrap_or("?").to_string() } else { who.join(" or ") };
        if !out.contains(&text) { out.push(text); }
    }
    out.join(", ").chars().take(120).collect()
}
