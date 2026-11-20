//! View-kind fallback (D29, `research/element-and-view-fallbacks.md`): a view a client can't render (unknown kind,
//! or unmet outer `requires`) is drawn as its `fallback` view instead, keeping the pane's original identity.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use super::panel;

/// Longest fallback chain followed; longer chains and cycles show the labelled placeholder.
pub const MAX_CHAIN: usize = 4;

/// A view's entry with its live content merged over it (what a client renders), or None if it doesn't exist.
pub fn merged(view: &str, entries: &BTreeMap<String, Value>, live: &HashMap<String, Value>) -> Option<Value> {
    let mut v = entries.get(&format!("view/{view}")).cloned()?;
    if let (Some(target), Some(source)) = (v.as_object_mut(), live.get(view).and_then(Value::as_object)) {
        for (k, x) in source { target.insert(k.clone(), x.clone()); }
    }
    Some(v)
}

/// Whether a client rendering view `kinds` and panel `features` can draw this (merged) view value.
pub fn renders(v: &Value, kinds: &[&str], features: &[&str]) -> bool {
    kinds.contains(&v["kind"].as_str().unwrap_or("")) && panel::missing_for(v, features).is_empty()
}

/// The view to draw for `view` (itself when renderable, else the first renderable view along its `fallback`
/// chain), and every view id visited (a change to any of them changes what is shown). None: nothing renderable,
/// so the client draws its labelled placeholder for the original view.
pub fn shown(view: &str, value: &dyn Fn(&str) -> Option<Value>, can_render: &dyn Fn(&Value) -> bool) -> (Option<String>, Vec<String>) {
    let mut chain: Vec<String> = Vec::new();
    let mut current = view.to_string();
    while chain.len() <= MAX_CHAIN && !chain.contains(&current) {
        chain.push(current.clone());
        let Some(v) = value(&current) else { return (None, chain) };
        if can_render(&v) { return (Some(current), chain); }
        match v["fallback"].as_str() { Some(next) => current = next.to_string(), None => return (None, chain) }
    }
    (None, chain)
}
