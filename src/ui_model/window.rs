//! Which layout (workspace) a window shows: window state, like zoom, never workspace state (core-protocol.md,
//! "Several layouts; layout settings"). A window starts on the shared default `workspace/active` (the layout
//! anyone last switched to) and then moves only on its own switches: a command reply's `{"client": {"layout"}}`,
//! `client.layout {name?}`, its own `vqro.layout.reset` (which shows `main` there), or its layout being deleted
//! (it falls back to the existing default, then existing `main`). Every invocation it sends names that layout in `args.context.layout`, and rows
//! with `current_when: {"layout"}` are marked current only in the window showing it.
//! Conformance cases for other clients: `tests/fixtures/client/window.json`.

use serde_json::{json, Value};

use super::workspace::Workspace;

#[derive(Default, Clone, Debug)]
pub struct Window {
    /// The layout this window chose; None until it has one (a new window takes the default once one exists).
    pub pinned: Option<String>,
}

/// What [`Window::settle`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum Settled {
    /// Nothing changed.
    Same,
    /// The window took a layout to show (the default, at start or once one exists).
    Took(String),
    /// The window's layout was deleted: it shows the default now (`now`, or nothing when there is none).
    Closed { was: String, now: Option<String> },
}

fn exists(ws: &Workspace, name: &str) -> bool { ws.entries.contains_key(&format!("layout/{name}")) }

/// An existing shared default, else the existing neutral recovery layout. Never creates state.
pub fn default_layout(ws: &Workspace) -> Option<String> {
    ws.entries.get("workspace/active").and_then(|a| a["layout"].as_str())
        .filter(|name| exists(ws, name)).map(String::from)
        .or_else(|| exists(ws, "main").then(|| "main".to_string()))
}

impl Window {
    /// The layout this window shows: its own while that layout exists, else the shared default.
    pub fn shown(&self, ws: &Workspace) -> Option<String> {
        self.pinned.clone().filter(|p| exists(ws, p)).or_else(|| default_layout(ws))
    }

    /// Call after loading a snapshot and after every `entries` event: keep the window on an existing layout.
    pub fn settle(&mut self, ws: &Workspace) -> Settled {
        match self.pinned.clone() {
            Some(p) if exists(ws, &p) => Settled::Same,
            gone => {
                let now = default_layout(ws).filter(|d| exists(ws, d));
                match gone {
                    Some(was) => { self.pinned = now.clone(); Settled::Closed { was, now } }
                    None => match now { Some(d) => { self.pinned = Some(d.clone()); Settled::Took(d) } None => Settled::Same },
                }
            }
        }
    }

    /// Show `name` in this window only (`client.layout {name}`, a reply hint). Ok(true) when it changed;
    /// a layout that doesn't exist is refused.
    pub fn show(&mut self, ws: &Workspace, name: &str) -> Result<bool, String> {
        if !exists(ws, name) {
            return Err(format!("no layout {name}"));
        }
        let changed = self.pinned.as_deref() != Some(name);
        self.pinned = Some(name.to_string());
        Ok(changed)
    }

    /// `client.layout {name?}`: show `name`, or without one the existing shared default, else main.
    pub fn client_layout(&mut self, ws: &Workspace, args: &Value) -> Result<bool, String> {
        match args["name"].as_str().or_else(|| args["layout"].as_str()) {
            Some(name) => self.show(ws, name),
            None => match default_layout(ws) {
                Some(name) => self.show(ws, &name),
                None => Err("client.layout: no layout to show".into()),
            },
        }
    }

    /// This window's context fields: its layout, that layout's version and its title (`doc/layout/<name>.title`,
    /// else the name). Status items with `label_context` read from it.
    pub fn context(&self, ws: &Workspace) -> Value {
        let layout = self.shown(ws);
        let version = layout.as_ref().and_then(|l| ws.versions.get(&format!("layout/{l}")).copied());
        let title = layout.as_ref().map(|l| ws.entries.get(&format!("doc/layout/{l}")).and_then(|s| s["title"].as_str()).map(String::from).unwrap_or_else(|| l.clone()));
        json!({"layout": layout, "layout_version": version, "layout_title": title})
    }

    /// The context an invocation carries: the place's own (a pane's, a tab's), with this window's layout when the
    /// place names none (palette, status bar, popups).
    pub fn invocation_context(&self, ws: &Workspace, ctx: Value) -> Value {
        let mut ctx = if ctx.is_object() { ctx } else { json!({}) };
        if ctx["layout"].is_null() {
            let window = self.context(ws);
            ctx["layout"] = window["layout"].clone();
            ctx["layout_version"] = window["layout_version"].clone();
        }
        ctx
    }
}

/// The layout a command's reply asks the invoking window to show: its `{"client": {"layout"}}` hint, or `main`
/// after this window's own `vqro.layout.reset` (other windows keep theirs).
pub fn reply_layout(command: &str, reply: &Value) -> Option<String> {
    reply["client"]["layout"].as_str().map(String::from).or_else(|| (command == "vqro.layout.reset").then(|| "main".to_string()))
}

/// Mark rows that are current in this window: a row with `current_when: {"layout": name}` gets the accent style
/// (unless it has one) and a leading "current" badge while the window shows layout `name`. Panel content is
/// shared by every window, so the mark is the window's, not the extension's.
pub fn mark_current(v: &mut Value, layout: Option<&str>) {
    fn rows(items: Option<&mut Value>, layout: Option<&str>) {
        for row in items.and_then(Value::as_array_mut).into_iter().flatten() {
            if layout.is_some() && row["current_when"]["layout"].as_str() == layout {
                if row["style"].is_null() {
                    row["style"] = json!("accent");
                }
                let mut badges = vec![json!({"text": "current", "style": "accent"})];
                badges.extend(row["badges"].as_array().cloned().unwrap_or_default());
                row["badges"] = json!(badges);
            }
            rows(row.get_mut("children"), layout);
        }
    }
    rows(v.get_mut("items"), layout);
    rows(v.get_mut("toolbar"), layout);
    for page in v.get_mut("pages").and_then(Value::as_array_mut).into_iter().flatten() {
        rows(page.get_mut("items"), layout);
    }
}
