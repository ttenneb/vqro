//! `prim tui`: a client that renders workspace primitives. It holds no workspace behaviour (D17):
//! layout, views, menus, bindings and commands all come from the core and extensions. It owns only
//! mechanism (rendering, focus, mouse mechanics, menus drawn from `menu/*` entries, text prompts,
//! pickers) and recovery that must work when every extension is broken:
//!
//! Prefix: Ctrl-g, or click the `prim` badge. A menu lists every action; press its key or click it.
//!   built in: : commands · o/O next/previous pane · 1-9 tab · u undo · R reset layout · ! safe mode
//!   · q quit · Ctrl-g sends Ctrl-g. Everything else comes from `binding/*` entries.
//!
//! Mouse mechanics: click to focus, click tabs, drag borders (one layout change on release), wheel
//! scrollback, forwarding to programs that asked for the mouse. What other gestures mean comes from
//! bindings such as `{"key": "mouse.right", "where": "pane", "command": "client.menu"}`.
//! Client commands: `client.menu {where}`, `client.prefix`, `client.palette`.

mod keys;
mod mouse;
mod panel;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::Result;
use base64::Engine;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::client;
use crate::rpc::{Incoming, Peer};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

struct Term {
    resource: String,
    stream: Option<u64>,
    parser: vt100::Parser,
    size: (u16, u16),
    attaching: bool,
    closed: Option<String>,
}

enum Overlay {
    Prefix { sel: usize },
    Palette { query: String, sel: usize },
    /// A menu built from `menu/*` entries for a place (pane, tab, ...), at the pointer.
    Menu { items: Vec<Item>, ctx: Value, sel: usize, at: Position },
    /// One line of text for an item that declared `input`.
    Input { prompt: String, text: String, item: Item, ctx: Value },
    /// Choose a resource for an item that declared `pick: "resource"`.
    Pick { sel: usize, item: Item, ctx: Value },
    /// An extension's view shown on top, at its request (never a terminal).
    Popup { view: String, width: u16, ctx: Value },
}

trait ClonePopup {
    fn clone_popup(&self) -> Option<Overlay>;
}

impl ClonePopup for Option<Overlay> {
    fn clone_popup(&self) -> Option<Overlay> {
        match self {
            Some(Overlay::Popup { view, width, ctx }) => Some(Overlay::Popup { view: view.clone(), width: *width, ctx: ctx.clone() }),
            _ => None,
        }
    }
}

/// A runnable menu item or binding.
#[derive(Clone)]
struct Item {
    label: String,
    command: String,
    args: Value,
    input: Option<String>,
    pick: Option<String>,
    /// For status items: take the label (and style) from this view's live content.
    view: Option<String>,
}

impl Item {
    fn from_entry(v: &Value) -> Item {
        Item {
            label: v["label"].as_str().unwrap_or("").to_string(),
            command: v["command"].as_str().unwrap_or("").to_string(),
            args: v["args"].clone(),
            input: v["input"].as_str().map(String::from),
            pick: v["pick"].as_str().map(String::from),
            view: v["view"].as_str().map(String::from),
        }
    }
}

enum Msg {
    Attached { view: String, resource: String, result: Result<u64, String> },
    Info(String),
    Snapshot(Value),
    ClearOverrides,
    /// A command's reply asked this client to show a view as a popup.
    Popup { view: String, width: u16, ctx: Value },
    ClosePopup,
}

/// Where things were drawn last frame, for mouse hit-testing.
#[derive(Default)]
struct Hit {
    leaves: Vec<(String, Rect, Rect)>,
    /// Layout path of each leaf, aligned with `leaves`.
    leaf_paths: Vec<Vec<usize>>,
    tabs: Vec<(Vec<usize>, usize, Rect)>,
    /// Clickable menu items drawn inline (tab bar, status bar), with their context.
    buttons: Vec<(Rect, Item, Value)>,
    dividers: Vec<Divider>,
    badge: Rect,
    status: Rect,
    /// Panel hit areas by panel key (view id, or "@popup").
    panels: HashMap<String, panel::Hits>,
    overlay: Option<(Rect, Rect, usize)>,
}

#[derive(Clone)]
struct Divider {
    path: Vec<usize>,
    index: usize,
    side_by_side: bool,
    parent: Rect,
    children: Vec<Rect>,
}

struct Drag {
    div: Divider,
    sizes: Vec<u16>,
}

struct App {
    peer: Peer,
    tx: mpsc::UnboundedSender<Msg>,
    entries: BTreeMap<String, Value>,
    versions: BTreeMap<String, u64>,
    resources: BTreeMap<String, Value>,
    commands: BTreeMap<String, Value>,
    extensions: BTreeMap<String, Value>,
    safe_mode: bool,
    terms: HashMap<String, Term>,
    stream_view: HashMap<u64, String>,
    early: HashMap<u64, Vec<Vec<u8>>>,
    leaves: Vec<String>,
    focus: usize,
    overlay: Option<Overlay>,
    /// Client-side panel state (selection, expansion, page, filter, scroll) by panel key.
    pstate: HashMap<String, panel::State>,
    /// Panel key whose filter is being typed in the status bar.
    filtering: Option<String>,
    /// A transient notification: text, style, until.
    toast: Option<(String, Option<String>, std::time::Instant)>,
    /// Live view content from `view.publish`, merged over the view entry.
    live: HashMap<String, Value>,
    hit: Hit,
    drag: Option<Drag>,
    overrides: HashMap<Vec<usize>, Vec<u16>>,
    mouse_owner: Option<String>,
    mouse_on: bool,
    message: String,
    quit: bool,
}

/// The installed binary's identity, to notice a reinstall.
fn exe_stamp() -> Option<(std::path::PathBuf, std::time::SystemTime)> {
    let exe = std::env::current_exe().ok()?;
    let modified = std::fs::metadata(&exe).ok()?.modified().ok()?;
    Some((exe, modified))
}

pub async fn run(socket: &Path) -> Result<()> {
    let started_as = exe_stamp();
    let mut reexec = false;
    let (peer, mut incoming, _) = client::connect(socket, "tui").await?;
    let snap = peer.call("workspace.snapshot", Value::Null).await?;
    let seq = snap["seq"].as_u64().unwrap_or(0);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut app = App {
        peer: peer.clone(),
        tx,
        entries: BTreeMap::new(),
        versions: BTreeMap::new(),
        resources: BTreeMap::new(),
        commands: BTreeMap::new(),
        extensions: BTreeMap::new(),
        safe_mode: false,
        terms: HashMap::new(),
        stream_view: HashMap::new(),
        early: HashMap::new(),
        leaves: Vec::new(),
        focus: 0,
        overlay: None,
        pstate: HashMap::new(),
        filtering: None,
        toast: None,
        live: HashMap::new(),
        hit: Hit::default(),
        drag: None,
        overrides: HashMap::new(),
        mouse_owner: None,
        mouse_on: true,
        message: "click prim or press Ctrl-g for the menu".into(),
        quit: false,
    };
    app.load(&snap);
    peer.call("events.subscribe", json!({"from": seq})).await?;

    let (key_tx, mut keys) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(ev) = crossterm::event::read() {
            if key_tx.send(ev).is_err() {
                break;
            }
        }
    });

    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste, crossterm::event::EnableMouseCapture);
    let result: Result<()> = async {
        let mut dirty = true;
        loop {
            if dirty {
                terminal.draw(|f| app.draw(f))?;
            }
            if app.quit {
                return Ok(());
            }
            dirty = tokio::select! {
                m = incoming.recv() => match m {
                    Some(m) => { app.on_incoming(m); true }
                    None => {
                        // The core went away (restart, reinstall): bring it back and reconnect.
                        app.message = "core disconnected, reconnecting…".into();
                        terminal.draw(|f| app.draw(f))?;
                        incoming = app.reconnect(socket).await;
                        // A reinstall restarts the core; if our own binary changed too, restart into it.
                        // Terminals live in the backend, so nothing is lost.
                        if exe_stamp() != started_as {
                            reexec = true;
                            return Ok(());
                        }
                        true
                    }
                },
                Some(m) = rx.recv() => { app.on_msg(m); true }
                Some(ev) = keys.recv() => app.on_event(ev),
                _ = tokio::time::sleep(std::time::Duration::from_millis(500)), if app.toast.is_some() => true,
                else => return Ok(()),
            };
            // Drain whatever else is ready before redrawing.
            while let Ok(m) = incoming.try_recv() {
                app.on_incoming(m);
                dirty = true;
            }
            // Client options from the settings document.
            let want = app.setting("mouse");
            if want != app.mouse_on {
                app.mouse_on = want;
                let _ = if want {
                    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture)
                } else {
                    crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture)
                };
            }
        }
    }
    .await;
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture, crossterm::event::DisableBracketedPaste);
    ratatui::restore();
    if reexec {
        if let Some((exe, _)) = exe_stamp() {
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                let args: Vec<String> = std::env::args().skip(1).collect();
                let err = std::process::Command::new(exe).args(args).exec();
                anyhow::bail!("could not restart into the new prim: {err}");
            }
        }
    }
    result
}

impl App {
    /// Reconnect to the core, starting it if needed. Retries until it works; keys still quit.
    async fn reconnect(&mut self, socket: &Path) -> mpsc::UnboundedReceiver<Incoming> {
        loop {
            let attempt: Result<_> = async {
                client::ensure_core().await?;
                let (peer, incoming, _) = client::connect(socket, "tui").await?;
                let snap = peer.call("workspace.snapshot", Value::Null).await?;
                peer.call("events.subscribe", json!({"from": snap["seq"].as_u64().unwrap_or(0)})).await?;
                Ok((peer, incoming, snap))
            }
            .await;
            match attempt {
                Ok((peer, incoming, snap)) => {
                    self.peer = peer;
                    self.terms.clear();
                    self.stream_view.clear();
                    self.early.clear();
                    self.load(&snap);
                    self.message = "reconnected".into();
                    return incoming;
                }
                Err(e) => {
                    self.message = format!("reconnecting: {e}");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    }

    fn load(&mut self, snap: &Value) {
        self.entries.clear();
        self.versions.clear();
        if let Some(es) = snap["entries"].as_object() {
            for (k, e) in es {
                self.entries.insert(k.clone(), e["value"].clone());
                self.versions.insert(k.clone(), e["version"].as_u64().unwrap_or(0));
            }
        }
        self.resources = snap["resources"].as_array().into_iter().flatten().filter_map(|r| Some((r["id"].as_str()?.to_string(), r.clone()))).collect();
        self.commands = snap["commands"].as_array().into_iter().flatten().filter_map(|c| Some((c["name"].as_str()?.to_string(), c.clone()))).collect();
        self.extensions = snap["extensions"].as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
        self.safe_mode = snap["safe_mode"].as_bool().unwrap_or(false);
        self.live = snap["live"].as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
    }

    fn on_incoming(&mut self, m: Incoming) {
        let Incoming::Notification { method, params } = m else { return };
        match method.as_str() {
            "stream.data" => {
                let (Some(sid), Some(data)) = (params["stream"].as_u64(), params["data"].as_str()) else { return };
                let Ok(bytes) = B64.decode(data) else { return };
                match self.stream_view.get(&sid).and_then(|v| self.terms.get_mut(v)) {
                    Some(t) => t.parser.process(&bytes),
                    None => self.early.entry(sid).or_default().push(bytes),
                }
            }
            "stream.closed" => {
                let Some(sid) = params["stream"].as_u64() else { return };
                if let Some(view) = self.stream_view.remove(&sid) {
                    if let Some(t) = self.terms.get_mut(&view) {
                        t.stream = None;
                        t.closed = Some(params["reason"]["reason"].as_str().unwrap_or("closed").to_string());
                    }
                }
            }
            "missed" => {
                let (peer, tx) = (self.peer.clone(), self.tx.clone());
                tokio::spawn(async move {
                    if let Ok(s) = peer.call("workspace.snapshot", Value::Null).await {
                        let _ = tx.send(Msg::Snapshot(s));
                    }
                });
            }
            "event" => self.on_event_record(&params),
            _ => {}
        }
    }

    fn on_event_record(&mut self, ev: &Value) {
        let d = &ev["data"];
        match ev["type"].as_str().unwrap_or("") {
            "entries" => {
                for c in d["changes"].as_array().into_iter().flatten() {
                    let Some(k) = c["key"].as_str() else { continue };
                    if k.starts_with("layout/") {
                        // The committed layout replaces any local drag preview.
                        self.overrides.clear();
                    }
                    if c["value"].is_null() {
                        self.entries.remove(k);
                        self.versions.remove(k);
                    } else {
                        self.entries.insert(k.to_string(), c["value"].clone());
                        self.versions.insert(k.to_string(), c["version"].as_u64().unwrap_or(0));
                    }
                }
                let who = d["author"].as_str().unwrap_or("?");
                if !who.starts_with("tui:") && !who.starts_with("ext:") {
                    self.message = format!("workspace changed by {who} (txn {})", d["txn"]);
                }
            }
            "resource" => {
                if let Some(id) = d["id"].as_str() {
                    if d["state"] == "live" {
                        for t in self.terms.values_mut().filter(|t| t.resource == id) {
                            t.closed = None;
                        }
                    }
                    self.resources.insert(id.to_string(), d.clone());
                }
            }
            "resource.removed" => {
                if let Some(id) = d["id"].as_str() {
                    self.resources.remove(id);
                }
            }
            "command" => {
                if let Some(n) = d["name"].as_str() {
                    self.commands.insert(n.to_string(), d.clone());
                }
            }
            "extension" => {
                if let Some(n) = d["name"].as_str() {
                    let st = d["state"].clone();
                    if st["status"] == "failed" {
                        self.message = format!("extension {n} failed: {}", st["last_exit"].as_str().unwrap_or("?"));
                    }
                    self.extensions.insert(n.to_string(), st);
                }
            }
            "safe_mode" => self.safe_mode = d["on"].as_bool().unwrap_or(false),
            "notify" => {
                let text = d["text"].as_str().unwrap_or("").to_string();
                self.toast = Some((text, d["style"].as_str().map(String::from), std::time::Instant::now() + std::time::Duration::from_secs(4)));
            }
            "view.content" => {
                if let Some(v) = d["view"].as_str() {
                    if d["content"].is_null() {
                        self.live.remove(v);
                    } else {
                        self.live.insert(v.to_string(), d["content"].clone());
                    }
                }
            }
            "adapter" => {
                if d["state"]["connected"] == false {
                    self.message = format!("backend {} disconnected: {}", d["name"], d["state"]["error"].as_str().unwrap_or(""));
                }
            }
            _ => {}
        }
    }

    fn on_msg(&mut self, m: Msg) {
        match m {
            Msg::Info(s) => self.message = s,
            Msg::Snapshot(s) => self.load(&s),
            Msg::ClearOverrides => self.overrides.clear(),
            Msg::Popup { view, width, ctx } => {
                match self.view_kind(&view).as_str() {
                    "" => self.message = format!("popup: no view {view}"),
                    "terminal" => self.message = "popup refused: terminals never pop up".into(),
                    _ => {
                        self.pstate.insert("@popup".into(), panel::State::default());
                        self.overlay = Some(Overlay::Popup { view, width, ctx });
                    }
                }
            }
            Msg::ClosePopup => {
                if matches!(self.overlay, Some(Overlay::Popup { .. })) {
                    self.overlay = None;
                }
            }
            Msg::Attached { view, resource, result } => {
                let Some(t) = self.terms.get_mut(&view) else {
                    if let Ok(sid) = result {
                        self.peer.notify("stream.close", json!({"stream": sid}));
                    }
                    return;
                };
                t.attaching = false;
                if t.resource != resource {
                    if let Ok(sid) = result {
                        self.peer.notify("stream.close", json!({"stream": sid}));
                    }
                    return;
                }
                match result {
                    Ok(sid) => {
                        t.stream = Some(sid);
                        self.stream_view.insert(sid, view);
                        for bytes in self.early.remove(&sid).unwrap_or_default() {
                            t.parser.process(&bytes);
                        }
                    }
                    Err(e) => t.closed = Some(e),
                }
            }
        }
    }

    // ---- input -------------------------------------------------------------------------------

    /// Returns whether a redraw is needed.
    fn on_event(&mut self, ev: Event) -> bool {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                self.on_key(k);
                true
            }
            Event::Mouse(m) => self.on_mouse(m),
            Event::Paste(text) => {
                self.send_to_focused(text.into_bytes());
                false
            }
            Event::Resize(..) => true,
            _ => false,
        }
    }

    fn send_to_focused(&mut self, bytes: Vec<u8>) {
        let Some(view) = self.leaves.get(self.focus).cloned() else { return };
        if let Some(t) = self.terms.get_mut(&view) {
            t.parser.screen_mut().set_scrollback(0);
            if let Some(sid) = t.stream {
                self.peer.notify("stream.input", json!({"stream": sid, "data": B64.encode(bytes)}));
            }
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        let is_prefix = k.code == KeyCode::Char('g') && k.modifiers.contains(KeyModifiers::CONTROL);
        if self.filtering.is_some() {
            return self.on_filter_key(k);
        }
        match self.overlay {
            Some(Overlay::Popup { .. }) => return self.on_popup_key(k),
            Some(Overlay::Prefix { .. }) => return self.on_prefix_key(k, is_prefix),
            Some(_) => return self.on_overlay_key(k),
            None => {}
        }
        if is_prefix {
            self.overlay = Some(Overlay::Prefix { sel: 0 });
            return;
        }
        let Some(view) = self.leaves.get(self.focus).cloned() else { return };
        match self.view_kind(&view).as_str() {
            "terminal" => {
                let Some(t) = self.terms.get(&view) else { return };
                if let Some(bytes) = keys::encode(&k, t.parser.screen().application_cursor()) {
                    self.send_to_focused(bytes);
                }
            }
            "panel" => {
                let ctx = self.ctx_leaf(self.focus);
                self.panel_key(&view.clone(), &view, k, ctx);
            }
            _ => {}
        }
    }

    fn view_kind(&self, view: &str) -> String {
        self.entries.get(&format!("view/{view}")).and_then(|v| v["kind"].as_str()).unwrap_or("").to_string()
    }

    /// A panel's value: the view entry with its live content merged over it.
    fn panel_value(&self, view: &str) -> Value {
        let mut v = self.entries.get(&format!("view/{view}")).cloned().unwrap_or(json!({}));
        if let (Some(live), Some(obj)) = (self.live.get(view).and_then(|l| l.as_object()), v.as_object_mut()) {
            for (k, x) in live {
                obj.insert(k.clone(), x.clone());
            }
        }
        v
    }

    fn panel_rows(&self, key: &str, view: &str) -> Vec<panel::Flat> {
        let st = self.pstate.get(key).cloned().unwrap_or_default();
        panel::rows(&self.panel_value(view), &st)
    }

    /// Keys for a panel (in a pane or a popup). Returns whether it used the key.
    fn panel_key(&mut self, key: &str, view: &str, k: KeyEvent, ctx: Value) -> bool {
        let mut ctx = ctx;
        ctx["panel"] = json!(key);
        ctx["panel_view"] = json!(view);
        let value = self.panel_value(view);
        let mut st = self.pstate.get(key).cloned().unwrap_or_default();
        let rows = panel::rows(&value, &st);
        let pages = panel::page_titles(&value).len();
        let mut run = None;
        let chips = rows.get(st.sel).filter(|r| panel::kind(&r.item) == "chips").map(|r| r.item["chips"].as_array().map(|a| a.len()).unwrap_or(0));
        match k.code {
            KeyCode::Left | KeyCode::Char('h') if chips.is_some() => st.chip = st.chip.saturating_sub(1),
            KeyCode::Right | KeyCode::Char('l') if chips.is_some() => st.chip = (st.chip + 1).min(chips.unwrap_or(1).saturating_sub(1)),
            KeyCode::Up | KeyCode::Char('k') => st.sel = panel::step(&rows, st.sel, -1),
            KeyCode::Down | KeyCode::Char('j') => st.sel = panel::step(&rows, st.sel, 1),
            KeyCode::PageUp => st.sel = panel::step(&rows, st.sel, -10),
            KeyCode::PageDown => st.sel = panel::step(&rows, st.sel, 10),
            KeyCode::Home | KeyCode::Char('g') => st.sel = panel::first(&rows),
            KeyCode::End | KeyCode::Char('G') => st.sel = panel::last(&rows),
            KeyCode::Left | KeyCode::Char('h') => panel::left(&rows, &mut st),
            KeyCode::Right | KeyCode::Char('l') => panel::right(&rows, &mut st),
            KeyCode::Char(']') | KeyCode::Tab if pages > 0 => {
                st.page = (st.page + 1) % pages;
                st.sel = 0;
                st.top = 0;
            }
            KeyCode::Char('[') | KeyCode::BackTab if pages > 0 => {
                st.page = (st.page + pages - 1) % pages;
                st.sel = 0;
                st.top = 0;
            }
            KeyCode::Char('/') => {
                self.filtering = Some(key.to_string());
            }
            KeyCode::Char('m') => {
                if let Some(r) = rows.get(st.sel) {
                    let item = r.item.clone();
                    self.pstate.insert(key.to_string(), st);
                    self.row_menu(&item, ctx, None);
                    return true;
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let at = st.sel;
                run = panel::activate(&rows, &mut st, at);
            }
            _ => return false,
        }
        let sel = st.sel;
        self.pstate.insert(key.to_string(), st);
        if let Some(item) = run {
            if item["options"].is_array() {
                let at = self.row_position(key, sel);
                self.options_menu(&item, ctx, at);
            } else {
                self.activate_item(Item::from_entry(&json!({"label": item["text"], "command": item["command"], "args": item["args"], "input": item["input"], "pick": item["pick"]})), ctx, None);
            }
        }
        true
    }

    /// Where a panel row was drawn, for anchoring dropdowns under it.
    fn row_position(&self, key: &str, row: usize) -> Option<Position> {
        let hits = self.hit.panels.get(key)?;
        let chip = self.pstate.get(key).map(|s| s.chip).unwrap_or(0);
        let r = hits.chips.iter().find(|(_, r, c)| *r == row && *c == chip).map(|x| x.0).or_else(|| hits.rows.iter().find(|(_, i)| *i == row).map(|x| x.0))?;
        Some(Position { x: r.x, y: r.y + 1 })
    }

    /// A pick-one menu for anything with `options`: choosing runs its command with `value` added.
    fn options_menu(&mut self, item: &Value, ctx: Value, at: Option<Position>) {
        let current = item["value"].clone();
        let items: Vec<Item> = item["options"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|o| {
                let mark = if o["value"] == current { "● " } else { "  " };
                Item {
                    label: format!("{mark}{}", o["label"].as_str().or_else(|| o["value"].as_str()).unwrap_or("?")),
                    command: item["command"].as_str().unwrap_or("").to_string(),
                    args: with_arg(&item["args"], "value", o["value"].clone()),
                    input: None,
                    pick: None,
                    view: None,
                }
            })
            .collect();
        if items.is_empty() || item["command"].as_str().is_none() {
            return;
        }
        let at = at.unwrap_or_default();
        self.overlay = Some(Overlay::Menu { items, ctx, sel: 0, at });
    }

    /// A row's own actions (`menu`), shown like any other menu.
    fn row_menu(&mut self, item: &Value, ctx: Value, at: Option<Position>) {
        let items: Vec<Item> = item["menu"].as_array().into_iter().flatten().map(Item::from_entry).filter(|i| !i.command.is_empty()).collect();
        if items.is_empty() {
            return;
        }
        let at = at.or_else(|| self.hit.leaves.get(self.focus).map(|(_, o, _)| Position { x: o.x + 2, y: o.y + 2 })).unwrap_or_default();
        self.overlay = Some(Overlay::Menu { items, ctx, sel: 0, at });
    }

    fn on_filter_key(&mut self, k: KeyEvent) {
        let Some(key) = self.filtering.clone() else { return };
        let st = self.pstate.entry(key).or_default();
        match k.code {
            KeyCode::Esc => {
                st.filter.clear();
                self.filtering = None;
            }
            KeyCode::Enter => self.filtering = None,
            KeyCode::Backspace => {
                st.filter.pop();
            }
            KeyCode::Char(c) => {
                st.filter.push(c);
                st.sel = 0;
                st.top = 0;
            }
            _ => {}
        }
    }

    fn on_popup_key(&mut self, k: KeyEvent) {
        let Some(Overlay::Popup { view, ctx, .. }) = self.overlay.clone_popup() else { return };
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                let st = self.pstate.entry("@popup".into()).or_default();
                if st.filter.is_empty() {
                    self.overlay = None;
                } else {
                    st.filter.clear();
                }
            }
            _ => {
                self.panel_key("@popup", &view, k, ctx);
            }
        }
    }

    /// The prefix menu: built-in recovery keys, then every visible keyboard binding.
    fn prefix_items(&self) -> Vec<(String, String)> {
        let mut items: Vec<(String, String)> = [
            (":", "all commands"),
            ("o", "next pane"),
            ("u", "undo last change"),
            ("R", "reset layout"),
            ("!", if self.safe_mode { "leave safe mode" } else { "safe mode (stop extensions)" }),
            ("q", "quit this UI"),
        ]
        .iter()
        .map(|(k, l)| (k.to_string(), l.to_string()))
        .collect();
        for b in self.bindings().into_iter().filter(|b| !b["key"].as_str().unwrap_or("").starts_with("mouse.")) {
            let key = b["key"].as_str().unwrap_or("").to_string();
            if items.iter().any(|(k, _)| *k == key) {
                continue; // built-ins win
            }
            let cmd = b["command"].as_str().unwrap_or("");
            let label = b["label"].as_str().or_else(|| self.commands.get(cmd).and_then(|c| c["title"].as_str())).unwrap_or(cmd);
            items.push((key, label.to_string()));
        }
        items
    }

    fn on_prefix_key(&mut self, k: KeyEvent, is_prefix: bool) {
        let items = self.prefix_items();
        if let Some(Overlay::Prefix { sel }) = self.overlay.as_mut() {
            match k.code {
                KeyCode::Up => return *sel = sel.saturating_sub(1),
                KeyCode::Down => return *sel = (*sel + 1).min(items.len().saturating_sub(1)),
                KeyCode::Enter => {
                    let key = items.get(*sel).map(|i| i.0.clone()).unwrap_or_default();
                    self.overlay = None;
                    return self.prefix_action(&key);
                }
                _ => {}
            }
        }
        self.overlay = None;
        if is_prefix {
            return self.send_to_focused(vec![7]);
        }
        if let KeyCode::Char(c) = k.code {
            self.prefix_action(&c.to_string());
        }
    }

    fn prefix_action(&mut self, key: &str) {
        let n = self.leaves.len().max(1);
        match key {
            "q" => self.quit = true,
            ":" => self.overlay = Some(Overlay::Palette { query: String::new(), sel: 0 }),
            "o" => self.focus = (self.focus + 1) % n,
            "O" => self.focus = (self.focus + n - 1) % n,
            "u" => self.invoke("prim.revert", Value::Null),
            "R" => self.invoke("prim.layout.reset", Value::Null),
            "!" => self.invoke("prim.safe_mode", Value::Null),
            d if d.len() == 1 && d.chars().all(|c| ('1'..='9').contains(&c)) => {
                let i = d.parse::<usize>().unwrap() - 1;
                if let Some((path, _, _)) = self.hit.tabs.first().cloned() {
                    self.select_tab(&path, i);
                }
            }
            _ => match self.bindings().into_iter().find(|b| b["key"] == key) {
                Some(b) => {
                    let ctx = self.ctx_leaf(self.focus);
                    let at = self.hit.leaves.get(self.focus).map(|(_, o, _)| Position { x: o.x + 1, y: o.y + 1 });
                    self.run_command(b["command"].as_str().unwrap_or(""), b["args"].clone(), ctx, at);
                }
                None => self.message = format!("no binding for {key}"),
            },
        }
    }

    fn overlay_len(&self) -> usize {
        match &self.overlay {
            Some(Overlay::Prefix { .. }) => self.prefix_items().len(),
            Some(Overlay::Palette { .. }) => self.palette_items().len(),
            Some(Overlay::Pick { .. }) => self.resources.len(),
            Some(Overlay::Menu { items, .. }) => items.len(),
            Some(Overlay::Input { .. }) | Some(Overlay::Popup { .. }) | None => 0,
        }
    }

    fn overlay_sel(&mut self) -> Option<&mut usize> {
        match self.overlay.as_mut()? {
            Overlay::Prefix { sel } | Overlay::Palette { sel, .. } | Overlay::Pick { sel, .. } | Overlay::Menu { sel, .. } => Some(sel),
            Overlay::Input { .. } | Overlay::Popup { .. } => None,
        }
    }

    fn activate_overlay(&mut self) {
        let palette = self.palette_items();
        let resources: Vec<String> = self.resources.keys().cloned().collect();
        let prefix = self.prefix_items();
        match self.overlay.take() {
            Some(Overlay::Prefix { sel }) => {
                if let Some((key, _)) = prefix.get(sel) {
                    self.prefix_action(&key.clone());
                }
            }
            Some(Overlay::Palette { sel, query }) => {
                let (typed, rest) = match query.trim().split_once(' ') {
                    Some((n, r)) => (n.to_string(), r.trim().to_string()),
                    None => (query.trim().to_string(), String::new()),
                };
                let name = if self.commands.contains_key(&typed) { Some(typed) } else { palette.get(sel).cloned() };
                let Some(name) = name else { return };
                let ctx = self.ctx_leaf(self.focus);
                if !rest.is_empty() {
                    // Inline arguments: JSON, or plain text passed as `text`.
                    let args = if rest.starts_with('{') {
                        match serde_json::from_str::<Value>(&rest) {
                            Ok(v) => v,
                            Err(e) => return self.message = format!("bad JSON args: {e}"),
                        }
                    } else {
                        json!({"text": rest})
                    };
                    return self.run_command(&name, args, ctx, None);
                }
                // A command with a menu entry asks for what it needs (prompt, pick) like the menu does.
                let item = self.entries.iter().filter(|(k, v)| k.starts_with("menu/") && v["command"] == name.as_str() && self.visible(v)).map(|(_, v)| Item::from_entry(v)).next();
                match item {
                    Some(item) => self.activate_item(item, ctx, None),
                    None => self.run_command(&name, Value::Null, ctx, None),
                }
            }
            Some(Overlay::Menu { items, sel, ctx, at }) => {
                if let Some(item) = items.get(sel).cloned() {
                    self.activate_item(item, ctx, Some(at));
                }
            }
            Some(Overlay::Input { text, item, ctx, .. }) => {
                let args = with_arg(&item.args, "text", json!(text));
                self.run_command(&item.command, args, ctx, None);
            }
            Some(Overlay::Popup { .. }) => {}
            Some(Overlay::Pick { sel, item, ctx }) => {
                if let Some(r) = resources.get(sel) {
                    let args = with_arg(&item.args, "picked", json!(r));
                    self.run_command(&item.command, args, ctx, None);
                }
            }
            None => {}
        }
    }

    fn on_overlay_key(&mut self, k: KeyEvent) {
        let n = self.overlay_len();
        match k.code {
            KeyCode::Esc => self.overlay = None,
            KeyCode::Up => {
                if let Some(sel) = self.overlay_sel() {
                    *sel = sel.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if let Some(sel) = self.overlay_sel() {
                    *sel = (*sel + 1).min(n.saturating_sub(1));
                }
            }
            KeyCode::Enter => self.activate_overlay(),
            KeyCode::Tab => {
                let items = self.palette_items();
                if let Some(Overlay::Palette { query, sel }) = self.overlay.as_mut() {
                    if let Some(name) = items.get(*sel) {
                        *query = format!("{name} ");
                        *sel = 0;
                    }
                }
            }
            KeyCode::Backspace => match self.overlay.as_mut() {
                Some(Overlay::Palette { query, sel }) => {
                    query.pop();
                    *sel = 0;
                }
                Some(Overlay::Input { text, .. }) => {
                    text.pop();
                }
                _ => {}
            },
            KeyCode::Char(c) => match self.overlay.as_mut() {
                Some(Overlay::Palette { query, sel }) => {
                    query.push(c);
                    *sel = 0;
                }
                Some(Overlay::Input { text, .. }) => text.push(c),
                _ => {}
            },
            _ => {}
        }
    }

    // ---- contributions: menus, bindings, context ---------------------------------------------

    /// Whether a contributed entry is live: its owner extension is on (and not in safe mode),
    /// and its command exists. Contributions of a disabled or removed extension disappear.
    fn visible(&self, e: &Value) -> bool {
        if let Some(owner) = e["owner"].as_str() {
            let on = self.entries.get(&format!("extension/{owner}")).map(|x| x["enabled"].as_bool().unwrap_or(true)).unwrap_or(false);
            if !on || self.safe_mode {
                return false;
            }
        }
        let cmd = e["command"].as_str().unwrap_or("");
        cmd.starts_with("client.") || self.commands.get(cmd).map(|c| c["available"] == true).unwrap_or(false)
    }

    fn bindings(&self) -> Vec<Value> {
        self.entries.iter().filter(|(k, _)| k.starts_with("binding/")).map(|(_, v)| v.clone()).filter(|v| self.visible(v)).collect()
    }

    fn gesture(&self, key: &str, place: &str) -> Option<Value> {
        self.bindings().into_iter().find(|b| b["key"] == key && b["where"].as_str().unwrap_or("pane") == place)
    }

    /// Menu items for a place, filtered by `when` against the context, in `order`.
    fn menu_items(&self, place: &str, ctx: &Value) -> Vec<Item> {
        let mut found: Vec<(i64, String, Item)> = self
            .entries
            .iter()
            .filter(|(k, v)| k.starts_with("menu/") && v["where"] == place && self.visible(v))
            .filter(|(_, v)| v["when"].as_object().map(|w| w.iter().all(|(k, want)| ctx[k] == *want)).unwrap_or(true))
            .map(|(k, v)| (v["order"].as_i64().unwrap_or(50), k.clone(), Item::from_entry(v)))
            .collect();
        found.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        found.into_iter().map(|(_, _, i)| i).collect()
    }

    /// Context for a pane: what commands invoked on it are told.
    fn ctx_leaf(&self, idx: usize) -> Value {
        let Some((view, _, _)) = self.hit.leaves.get(idx) else { return json!({}) };
        let (layout, version) = self.active_layout_key();
        let v = self.entries.get(&format!("view/{view}")).cloned().unwrap_or(Value::Null);
        let resource = v["resource"].as_str();
        let live = resource.and_then(|r| self.resources.get(r)).map(|r| r["state"] == "live").unwrap_or(false);
        let path = self.hit.leaf_paths.get(idx).cloned().unwrap_or_default();
        let mut ctx = json!({
            "layout": layout, "layout_version": version, "path": path,
            "view": view, "kind": v["kind"], "resource": resource, "live": live, "title": self.view_title(view),
        });
        // The innermost tab holding this pane, so tab commands work from the keyboard too.
        if let Some((_, mut root)) = self.active_layout() {
            for depth in (0..path.len()).rev() {
                if node_at_mut(&mut root, &path[..depth]).map(|n| n.get("tabs").is_some()).unwrap_or(false) {
                    ctx["tab_path"] = json!(path[..depth]);
                    ctx["tab_index"] = json!(path[depth]);
                    break;
                }
            }
        }
        ctx
    }

    fn ctx_tab(&self, path: &[usize], i: usize) -> Value {
        let (layout, version) = self.active_layout_key();
        json!({"layout": layout, "layout_version": version, "tab_path": path, "tab_index": i})
    }

    fn active_layout_key(&self) -> (Option<String>, Option<u64>) {
        let name = self.entries.get("workspace/active").and_then(|a| a["layout"].as_str()).map(String::from);
        let version = name.as_ref().and_then(|n| self.versions.get(&format!("layout/{n}")).copied());
        (name, version)
    }

    fn view_title(&self, view: &str) -> String {
        let v = self.entries.get(&format!("view/{view}")).cloned().unwrap_or(Value::Null);
        if let Some(t) = v["title"].as_str() {
            return t.to_string();
        }
        let r = v["resource"].as_str().and_then(|r| self.resources.get(r));
        r.and_then(|r| r["name"].as_str().or_else(|| r["argv"][0].as_str()).map(String::from)).unwrap_or_else(|| view.to_string())
    }

    /// Run an item: prompt or pick first if it asks to.
    fn activate_item(&mut self, item: Item, ctx: Value, at: Option<Position>) {
        if let Some(prompt) = item.input.clone() {
            // Prefill with the current name when renaming something that has one.
            let text = ctx["title"].as_str().map(String::from).or_else(|| self.tab_title(&ctx)).unwrap_or_default();
            self.overlay = Some(Overlay::Input { prompt, text, item, ctx });
        } else if item.pick.as_deref() == Some("resource") {
            self.overlay = Some(Overlay::Pick { sel: 0, item, ctx });
        } else {
            self.run_command(&item.command, item.args.clone(), ctx, at);
        }
    }

    fn tab_title(&self, ctx: &Value) -> Option<String> {
        let (_, mut root) = self.active_layout()?;
        let path: Vec<usize> = ctx["tab_path"].as_array()?.iter().filter_map(|x| x.as_u64().map(|n| n as usize)).collect();
        let i = ctx["tab_index"].as_u64()? as usize;
        node_at_mut(&mut root, &path)?["tabs"][i]["title"].as_str().map(String::from)
    }

    /// `client.*` commands are mechanism handled here; everything else goes to the core with context.
    fn run_command(&mut self, name: &str, args: Value, ctx: Value, at: Option<Position>) {
        match name {
            "client.menu" => {
                let place = args["where"].as_str().unwrap_or("pane").to_string();
                let items = self.menu_items(&place, &ctx);
                if items.is_empty() {
                    self.message = format!("no {place} menu items (are their extensions on?)");
                    return;
                }
                let at = at.or_else(|| self.hit.leaves.get(self.focus).map(|(_, o, _)| Position { x: o.x + 1, y: o.y + 1 })).unwrap_or_default();
                self.overlay = Some(Overlay::Menu { items, ctx, sel: 0, at });
            }
            "client.prefix" => self.overlay = Some(Overlay::Prefix { sel: 0 }),
            "client.panel" => {
                // Act on a panel's client-side state: the panel the command came from.
                let Some(key) = ctx["panel"].as_str().map(String::from) else { return };
                let view = ctx["panel_view"].as_str().unwrap_or(&key).to_string();
                let value = self.panel_value(&view);
                let st = self.pstate.entry(key.clone()).or_default();
                match args["action"].as_str().unwrap_or("") {
                    "expand_all" | "collapse_all" => {
                        let open = args["action"] == "expand_all";
                        for id in panel::parent_ids(&value, st) {
                            st.open.insert(id, open);
                        }
                    }
                    "filter" => self.filtering = Some(key),
                    "clear_filter" => st.filter.clear(),
                    other => self.message = format!("client.panel: unknown action {other}"),
                }
            }
            "client.palette" => self.overlay = Some(Overlay::Palette { query: String::new(), sel: 0 }),
            _ => {
                let args = with_arg(&args, "context", ctx);
                self.invoke(name, args);
            }
        }
    }

    fn setting(&self, key: &str) -> bool {
        self.entries.get("doc/settings/tui").and_then(|s| s[key].as_bool()).unwrap_or(true)
    }

    // ---- mouse ---    // ---- mouse -------------------------------------------------------------------------------

    fn on_mouse(&mut self, m: MouseEvent) -> bool {
        let pos = Position { x: m.column, y: m.row };

        // Resizing a split: preview locally, commit once on release.
        if self.drag.is_some() {
            match m.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    self.drag_to(pos);
                    return true;
                }
                MouseEventKind::Up(_) => {
                    self.end_drag();
                    return true;
                }
                _ => return false,
            }
        }

        if matches!(self.overlay, Some(Overlay::Popup { .. })) {
            let Some((outer, _, _)) = self.hit.overlay else { return false };
            match m.kind {
                MouseEventKind::Down(button) => {
                    if !outer.contains(pos) {
                        self.overlay = None;
                        return true;
                    }
                    if let Some(Overlay::Popup { view, ctx, .. }) = self.overlay.clone_popup() {
                        self.panel_click("@popup", &view, pos, button, ctx);
                    }
                    return true;
                }
                MouseEventKind::ScrollUp => self.on_popup_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
                MouseEventKind::ScrollDown => self.on_popup_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
                _ => return false,
            }
            return true;
        }

        if self.overlay.is_some() {
            let Some((outer, list, offset)) = self.hit.overlay else { return false };
            match m.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if list.contains(pos) {
                        let i = offset + (pos.y - list.y) as usize;
                        if i < self.overlay_len() {
                            if let Some(sel) = self.overlay_sel() {
                                *sel = i;
                            }
                            self.activate_overlay();
                        }
                    } else if !outer.contains(pos) {
                        self.overlay = None;
                    }
                    return true;
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    let n = self.overlay_len();
                    let up = m.kind == MouseEventKind::ScrollUp;
                    if let Some(sel) = self.overlay_sel() {
                        *sel = if up { sel.saturating_sub(1) } else { (*sel + 1).min(n.saturating_sub(1)) };
                    }
                    return true;
                }
                _ => return false,
            }
        }

        // A program that took a press gets the rest of the gesture.
        if let Some(owner) = self.mouse_owner.clone() {
            if matches!(m.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_)) {
                self.forward_mouse(&owner, &m);
                if matches!(m.kind, MouseEventKind::Up(_)) {
                    self.mouse_owner = None;
                }
                return false;
            }
        }

        match m.kind {
            MouseEventKind::Down(button) => {
                if self.hit.badge.contains(pos) {
                    self.overlay = Some(Overlay::Prefix { sel: 0 });
                    return true;
                }
                if button == MouseButton::Left {
                    if let Some(div) = self.hit.dividers.iter().find(|d| on_divider(d, pos)).cloned() {
                        let sizes = div.children.iter().map(|r| if div.side_by_side { r.width } else { r.height }).collect();
                        self.drag = Some(Drag { div, sizes });
                        return true;
                    }
                    if let Some((_, item, ctx)) = self.hit.buttons.iter().find(|(r, _, _)| r.contains(pos)).cloned() {
                        self.activate_item(item, ctx, Some(pos));
                        return true;
                    }
                }
                let gesture = match button {
                    MouseButton::Right => Some("mouse.right"),
                    MouseButton::Middle => Some("mouse.middle"),
                    MouseButton::Left => None,
                };
                if let Some((path, i, _)) = self.hit.tabs.iter().find(|(_, _, r)| r.contains(pos)).cloned() {
                    match gesture {
                        None => self.select_tab(&path, i),
                        Some(g) => {
                            if let Some(b) = self.gesture(g, "tab") {
                                let ctx = self.ctx_tab(&path, i);
                                self.run_command(b["command"].as_str().unwrap_or(""), b["args"].clone(), ctx, Some(pos));
                            }
                        }
                    }
                    return true;
                }
                let Some(idx) = self.hit.leaves.iter().position(|(_, outer, _)| outer.contains(pos)) else { return false };
                self.focus = idx;
                let (view, _, inner) = self.hit.leaves[idx].clone();
                if button == MouseButton::Right && self.view_kind(&view) == "panel" {
                    let ctx = self.ctx_leaf(idx);
                    if self.panel_click(&view.clone(), &view, pos, button, ctx) {
                        return true;
                    }
                }
                if let Some(g) = gesture {
                    // Programs that asked for the mouse get it first; Ctrl/Alt makes it prim's.
                    let force = m.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
                    if !force && inner.contains(pos) && self.view_kind(&view) == "terminal" && self.forward_mouse(&view, &m) {
                        self.mouse_owner = Some(view);
                        return false;
                    }
                    if let Some(b) = self.gesture(g, "pane") {
                        let ctx = self.ctx_leaf(idx);
                        self.run_command(b["command"].as_str().unwrap_or(""), b["args"].clone(), ctx, Some(pos));
                    }
                    return true;
                }
                match self.view_kind(&view).as_str() {
                    "terminal" if inner.contains(pos) => {
                        if self.forward_mouse(&view, &m) {
                            self.mouse_owner = Some(view);
                        }
                    }
                    "panel" => {
                        let ctx = self.ctx_leaf(idx);
                        self.panel_click(&view.clone(), &view, pos, button, ctx);
                    }
                    _ => {}
                }
                true
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let Some((view, _, _)) = self.hit.leaves.iter().find(|(_, outer, _)| outer.contains(pos)).cloned() else { return false };
                let up = m.kind == MouseEventKind::ScrollUp;
                match self.view_kind(&view).as_str() {
                    "terminal" => {
                        if self.forward_mouse(&view, &m) {
                            return false;
                        }
                        let Some(t) = self.terms.get_mut(&view) else { return false };
                        let arrows = self.entries.get("doc/settings/tui").and_then(|s| s["wheel_arrows"].as_bool()).unwrap_or(true);
                        if t.parser.screen().alternate_screen() && arrows {
                            // Full-screen program without mouse support: arrows scroll it.
                            let key = KeyEvent::new(if up { KeyCode::Up } else { KeyCode::Down }, KeyModifiers::NONE);
                            let bytes = keys::encode(&key, t.parser.screen().application_cursor()).unwrap_or_default().repeat(3);
                            if let Some(sid) = t.stream {
                                self.peer.notify("stream.input", json!({"stream": sid, "data": B64.encode(bytes)}));
                            }
                            false
                        } else {
                            let cur = t.parser.screen().scrollback();
                            let next = if up { cur + 3 } else { cur.saturating_sub(3) };
                            t.parser.screen_mut().set_scrollback(next);
                            true
                        }
                    }
                    "panel" => {
                        let ctx = self.ctx_leaf(self.focus);
                        self.panel_key(&view.clone(), &view, KeyEvent::new(if up { KeyCode::Up } else { KeyCode::Down }, KeyModifiers::NONE), ctx);
                        true
                    }
                    _ => false,
                }
            }
            MouseEventKind::Moved => {
                if let Some((view, _, _)) = self.hit.leaves.iter().find(|(_, o, _)| o.contains(pos)).cloned() {
                    self.forward_mouse(&view, &m);
                }
                false
            }
            _ => false,
        }
    }

    /// A click in a panel: page tabs, expanders, rows (left activates, right opens the row's menu).
    /// Returns whether it hit something.
    fn panel_click(&mut self, key: &str, view: &str, pos: Position, button: MouseButton, ctx: Value) -> bool {
        let Some(hits) = self.hit.panels.get(key) else { return false };
        if let Some((_, page)) = hits.pages.iter().find(|(r, _)| r.contains(pos)).copied() {
            let st = self.pstate.entry(key.to_string()).or_default();
            st.page = page;
            st.sel = 0;
            st.top = 0;
            return true;
        }
        if let Some((_, row, chip)) = hits.chips.iter().find(|(r, _, _)| r.contains(pos)).copied() {
            let st = self.pstate.entry(key.to_string()).or_default();
            st.sel = row;
            st.chip = chip;
            if button == MouseButton::Left {
                self.panel_key(key, view, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), ctx);
            }
            return true;
        }
        let expander = hits.expanders.iter().find(|(r, _)| r.contains(pos)).map(|x| x.1);
        let Some(i) = expander.or_else(|| hits.rows.iter().find(|(r, _)| r.contains(pos)).map(|x| x.1)) else { return false };
        let rows = self.panel_rows(key, view);
        let mut st = self.pstate.get(key).cloned().unwrap_or_default();
        st.sel = i;
        if expander.is_some() {
            if let Some(r) = rows.get(i) {
                st.open.insert(r.id.clone(), !r.expanded);
            }
            self.pstate.insert(key.to_string(), st);
            return true;
        }
        self.pstate.insert(key.to_string(), st);
        match button {
            MouseButton::Right => {
                let item = rows.get(i).map(|r| r.item.clone()).unwrap_or(Value::Null);
                if item["menu"].is_array() {
                    self.row_menu(&item, ctx, Some(pos));
                    return true;
                }
                false
            }
            _ => {
                self.panel_key(key, view, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), ctx);
                true
            }
        }
    }

    /// Send a mouse event to a terminal program if it asked for the mouse. Returns whether it did.
    fn forward_mouse(&mut self, view: &str, m: &MouseEvent) -> bool {
        let Some((_, _, inner)) = self.hit.leaves.iter().find(|(v, _, _)| v == view).cloned() else { return false };
        let Some(t) = self.terms.get(view) else { return false };
        let screen = t.parser.screen();
        let col = m.column.saturating_sub(inner.x).min(inner.width.saturating_sub(1));
        let row = m.row.saturating_sub(inner.y).min(inner.height.saturating_sub(1));
        let Some(bytes) = mouse::encode(m, col, row, screen.mouse_protocol_mode(), screen.mouse_protocol_encoding()) else { return false };
        if let Some(sid) = t.stream {
            self.peer.notify("stream.input", json!({"stream": sid, "data": B64.encode(bytes)}));
        }
        true
    }

    fn drag_to(&mut self, pos: Position) {
        let Some(d) = self.drag.as_mut() else { return };
        let i = d.div.index;
        let (p, origin) = if d.div.side_by_side { (pos.x, d.div.parent.x) } else { (pos.y, d.div.parent.y) };
        let start: u16 = d.sizes[..i].iter().sum();
        let total = d.sizes[i] + d.sizes[i + 1];
        let min = 3.min(total / 2);
        let a = (p.saturating_sub(origin) + 1).saturating_sub(start).clamp(min, total - min);
        d.sizes[i] = a;
        d.sizes[i + 1] = total - a;
        self.overrides.insert(d.div.path.clone(), d.sizes.clone());
    }

    fn end_drag(&mut self) {
        let Some(d) = self.drag.take() else { return };
        let Some((name, mut root)) = self.active_layout() else { return };
        let Some(node) = node_at_mut(&mut root, &d.div.path) else { return };
        node["sizes"] = json!(d.sizes);
        let key = format!("layout/{name}");
        let expect = self.versions.get(&key).copied();
        self.apply("resize", json!([{"op": "set", "key": key, "value": root, "expect": expect}]));
    }

    // ---- actions (all through public primitives) ---------------------------------------------

    fn palette_items(&self) -> Vec<String> {
        let q = match &self.overlay {
            Some(Overlay::Palette { query, .. }) => query.split_whitespace().next().unwrap_or("").to_lowercase(),
            _ => String::new(),
        };
        self.commands
            .values()
            .filter(|c| c["available"] == true)
            .filter(|c| q.is_empty() || c["name"].as_str().unwrap_or("").to_lowercase().contains(&q) || c["title"].as_str().unwrap_or("").to_lowercase().contains(&q))
            .filter_map(|c| c["name"].as_str().map(String::from))
            .collect()
    }

    fn invoke(&mut self, name: &str, args: Value) {
        let (peer, tx, name) = (self.peer.clone(), self.tx.clone(), name.to_string());
        self.message = format!("running {name}…");
        tokio::spawn(async move {
            let ctx = args["context"].clone();
            let msg = match peer.call("command.invoke", json!({"name": name, "args": args})).await {
                Ok(r) => {
                    // A reply may ask this client for UI: `{"client": {"popup": {"view", "width"?}}}` or `{"client": {"close": true}}`.
                    if let Some(view) = r["client"]["popup"]["view"].as_str() {
                        let width = r["client"]["popup"]["width"].as_u64().unwrap_or(60) as u16;
                        let _ = tx.send(Msg::Popup { view: view.to_string(), width, ctx });
                    }
                    if r["client"]["close"] == true {
                        let _ = tx.send(Msg::ClosePopup);
                    }
                    format!("{name}: done")
                }
                Err(e) => format!("{name}: {e}"),
            };
            let _ = tx.send(Msg::Info(msg));
        });
    }

    fn apply(&mut self, note: &str, ops: Value) {
        let (peer, tx) = (self.peer.clone(), self.tx.clone());
        let note = note.to_string();
        tokio::spawn(async move {
            if let Err(e) = peer.call("workspace.apply", json!({"ops": ops, "note": note})).await {
                let _ = tx.send(Msg::Info(format!("{note}: {e}")));
                let _ = tx.send(Msg::ClearOverrides);
            }
        });
    }

    fn active_layout(&self) -> Option<(String, Value)> {
        let name = self.entries.get("workspace/active")?["layout"].as_str()?.to_string();
        let v = self.entries.get(&format!("layout/{name}"))?.clone();
        Some((name, v))
    }

    fn select_tab(&mut self, path: &[usize], i: usize) {
        let Some((name, mut root)) = self.active_layout() else { return };
        let Some(tabs) = node_at_mut(&mut root, path) else { return };
        let n = tabs["tabs"].as_array().map(|t| t.len()).unwrap_or(0);
        if i >= n || tabs["active"].as_u64() == Some(i as u64) {
            return;
        }
        tabs["active"] = json!(i);
        let key = format!("layout/{name}");
        let expect = self.versions.get(&key).copied();
        self.apply("select tab", json!([{"op": "set", "key": key, "value": root, "expect": expect}]));
    }

    // ---- drawing -----------------------------------------------------------------------------

    fn draw(&mut self, f: &mut Frame) {
        let [main, status] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());
        let mut hit = Hit::default();
        match self.active_layout() {
            Some((_, root)) => {
                let tabbar = self.menu_items("tabbar", &json!({}));
                walk(&root, main, &mut Vec::new(), &self.overrides, &tabbar, &mut hit, f.buffer_mut())
            }
            None => {
                let text = "No layout yet.\n\nClick prim (bottom left) or press Ctrl-g for the menu:\n  n  open a shell\n  R  one tab per terminal\n  :  all commands";
                f.render_widget(Paragraph::new(text).block(Block::bordered().title(" prim ")), main);
            }
        }
        self.leaves = hit.leaves.iter().map(|(v, _, _)| v.clone()).collect();
        if self.focus >= self.leaves.len() {
            self.focus = 0;
        }
        // Drop terminals no longer shown.
        let gone: Vec<String> = self.terms.keys().filter(|v| !self.leaves.contains(v)).cloned().collect();
        for v in gone {
            if let Some(t) = self.terms.remove(&v) {
                if let Some(sid) = t.stream {
                    self.stream_view.remove(&sid);
                    self.peer.notify("stream.close", json!({"stream": sid}));
                }
            }
        }
        let mut cursor = None;
        let leaves = hit.leaves.clone();
        self.hit = hit;
        for (i, (view, rect, _)) in leaves.iter().enumerate() {
            if let Some(c) = self.draw_view(f, view, *rect, i == self.focus) {
                cursor = Some(c);
            }
        }
        self.draw_status(f, status);
        self.draw_overlay(f, main);
        self.draw_toast(f, main);
        if let (Some(c), None) = (cursor, &self.overlay) {
            f.set_cursor_position(c);
        }
    }

    fn draw_view(&mut self, f: &mut Frame, view: &str, rect: Rect, focused: bool) -> Option<(u16, u16)> {
        let border = if focused { Style::new().fg(Color::Cyan) } else { Style::new().fg(Color::DarkGray) };
        let Some(v) = self.entries.get(&format!("view/{view}")).cloned() else {
            f.render_widget(Paragraph::new(format!("view {view} is missing")).block(Block::bordered().border_style(border)), rect);
            return None;
        };
        match v["kind"].as_str().unwrap_or("") {
            "terminal" => {
                let rid = v["resource"].as_str().unwrap_or("").to_string();
                let res = self.resources.get(&rid).cloned();
                let scroll = self.terms.get(view).map(|t| t.parser.screen().scrollback()).unwrap_or(0);
                let title = match &res {
                    Some(r) => {
                        let name = v["title"].as_str().or_else(|| r["name"].as_str()).or_else(|| r["argv"][0].as_str()).unwrap_or(&rid).to_string();
                        let status = r["status"]["state"].as_str().map(|s| format!(" [{s}]")).unwrap_or_default();
                        let fin = if r["state"] == "finished" { " (finished)" } else { "" };
                        let sc = if scroll > 0 { format!(" ↑{scroll}") } else { String::new() };
                        format!(" {name}{status}{fin}{sc} ")
                    }
                    None => format!(" {rid} (gone) "),
                };
                let block = Block::bordered().title(title).border_style(border);
                let inner = block.inner(rect);
                f.render_widget(block, rect);
                if inner.width == 0 || inner.height == 0 {
                    return None;
                }
                let live = res.as_ref().map(|r| r["state"] == "live").unwrap_or(false);
                let dragging = self.drag.is_some();
                let t = self.terms.entry(view.to_string()).or_insert_with(|| Term {
                    resource: rid.clone(),
                    stream: None,
                    parser: new_parser(inner.width, inner.height),
                    size: (inner.width, inner.height),
                    attaching: false,
                    closed: None,
                });
                if t.resource != rid {
                    if let Some(sid) = t.stream.take() {
                        self.stream_view.remove(&sid);
                        self.peer.notify("stream.close", json!({"stream": sid}));
                    }
                    *t = Term { resource: rid.clone(), stream: None, parser: new_parser(inner.width, inner.height), size: (inner.width, inner.height), attaching: false, closed: None };
                }
                // While a border is being dragged, programs keep their size; they resize once on release.
                if t.size != (inner.width, inner.height) && !dragging {
                    t.size = (inner.width, inner.height);
                    t.parser.screen_mut().set_size(nz(inner.height), nz(inner.width));
                    if let Some(sid) = t.stream {
                        self.peer.notify("stream.resize", json!({"stream": sid, "cols": inner.width, "rows": inner.height}));
                    }
                }
                if t.stream.is_none() && !t.attaching && t.closed.is_none() && live {
                    t.attaching = true;
                    let (peer, tx, view, rid) = (self.peer.clone(), self.tx.clone(), view.to_string(), rid.clone());
                    let (cols, rows) = (inner.width, inner.height);
                    tokio::spawn(async move {
                        let result = peer
                            .call("resource.attach", json!({"resource": rid, "cols": cols, "rows": rows, "history": 2000}))
                            .await
                            .map(|r| r["stream"].as_u64().unwrap_or(0))
                            .map_err(|e| e.to_string());
                        let _ = tx.send(Msg::Attached { view, resource: rid, result });
                    });
                }
                render_screen(f.buffer_mut(), inner, t.parser.screen());
                if let Some(reason) = &t.closed {
                    let line = Line::from(Span::styled(format!(" stream closed: {reason} "), Style::new().fg(Color::Yellow)));
                    f.render_widget(Paragraph::new(line), Rect { height: 1, ..inner });
                }
                let screen = t.parser.screen();
                if focused && !screen.hide_cursor() && screen.scrollback() == 0 {
                    let (r, c) = screen.cursor_position();
                    if r < inner.height && c < inner.width {
                        return Some((inner.x + c, inner.y + r));
                    }
                }
                None
            }
            "panel" => {
                let v = self.panel_value(view);
                let title = v["title"].as_str().unwrap_or(view).to_string();
                let block = Block::bordered().title(format!(" {title} ")).border_style(border);
                let inner = block.inner(rect);
                f.render_widget(block, rect);
                let mut st = self.pstate.remove(view).unwrap_or_default();
                let hits = panel::render(f.buffer_mut(), inner, &v, &mut st, focused);
                self.pstate.insert(view.to_string(), st);
                self.hit.panels.insert(view.to_string(), hits);
                None
            }
            other => {
                let msg = format!("view {view}: kind `{other}` is not supported by this client");
                f.render_widget(Paragraph::new(msg).block(Block::bordered().border_style(border)), rect);
                None
            }
        }
    }

    fn draw_status(&mut self, f: &mut Frame, area: Rect) {
        // Keyboard modes turn the bar into an input line, like a multiplexer's prompt.
        let filtering = self.filtering.as_ref().map(|k| self.pstate.get(k).map(|s| s.filter.clone()).unwrap_or_default());
        let prompt = match &self.overlay {
            _ if filtering.is_some() => Some(("/".to_string(), filtering.clone().unwrap_or_default())),
            Some(Overlay::Prefix { .. }) => Some(("C-g".to_string(), String::new())),
            Some(Overlay::Palette { query, .. }) => Some((":".to_string(), query.clone())),
            Some(Overlay::Input { prompt, text, .. }) => Some((format!("{prompt}:"), text.clone())),
            _ => None,
        };
        let open = prompt.is_some();
        let badge = if open { " prim ▴ " } else { " prim ▾ " };
        self.hit.badge = Rect { x: area.x, y: area.y, width: badge.chars().count() as u16, height: 1 };
        self.hit.status = area;
        let mut spans = vec![Span::styled(badge, Style::new().bg(if open { Color::Magenta } else { Color::Cyan }).fg(Color::Black))];
        if let Some((label, text)) = prompt {
            spans.push(Span::styled(format!(" {label} "), Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD)));
            spans.push(Span::raw(text));
            let x = area.x + spans.iter().map(|s| s.content.chars().count() as u16).sum::<u16>();
            f.render_widget(Paragraph::new(Line::from(spans)), area);
            f.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
            return;
        }
        if self.safe_mode {
            spans.push(Span::styled(" SAFE MODE ", Style::new().bg(Color::Yellow).fg(Color::Black)));
        }
        let failed = self.extensions.values().filter(|e| e["status"] == "failed").count();
        if failed > 0 {
            spans.push(Span::styled(format!(" {failed} extension(s) failed "), Style::new().bg(Color::Red).fg(Color::White)));
        }
        // Contributed status-bar buttons.
        let mut x = area.x + spans.iter().map(|s| s.content.chars().count() as u16).sum::<u16>();
        for item in self.menu_items("status", &json!({})) {
            // Live segments take their label and style from a view's live content.
            let (text, style) = match item.view.as_deref().and_then(|v| self.live.get(v)) {
                Some(l) => (l["label"].as_str().unwrap_or(&item.label).to_string(), l["style"].as_str().map(|s| panel::style_named(Some(s))).unwrap_or(Style::new().fg(Color::Cyan))),
                None if item.view.is_some() => continue,
                None => (item.label.clone(), Style::new().fg(Color::Cyan)),
            };
            if text.is_empty() {
                continue;
            }
            let label = format!(" {text} ");
            let w = label.chars().count() as u16;
            if x + w > area.right() {
                break;
            }
            self.hit.buttons.push((Rect { x, y: area.y, width: w, height: 1 }, item, json!({})));
            x += w;
            spans.push(Span::styled(label, style));
        }
        spans.push(Span::styled(format!(" {}", self.message), Style::new().fg(Color::Gray)));
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_toast(&mut self, f: &mut Frame, area: Rect) {
        let Some((text, style, until)) = &self.toast else { return };
        if std::time::Instant::now() > *until {
            self.toast = None;
            return;
        }
        let w = (text.chars().count() as u16 + 4).min(area.width);
        let rect = Rect { x: area.right().saturating_sub(w + 1), y: area.y + 1, width: w, height: 3 };
        let color = match style.as_deref() {
            Some("error") => Color::Red,
            Some("warn") => Color::Yellow,
            Some("ok") => Color::Green,
            _ => Color::Cyan,
        };
        f.render_widget(Clear, rect);
        let block = Block::bordered().border_type(ratatui::widgets::BorderType::Rounded).border_style(Style::new().fg(color));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        f.render_widget(Paragraph::new(text.as_str()), inner);
    }

    fn draw_overlay(&mut self, f: &mut Frame, area: Rect) {
        if let Some(Overlay::Popup { view, width, .. }) = &self.overlay {
            let (view, width) = (view.clone(), *width);
            let v = self.panel_value(&view);
            let mut st = self.pstate.remove("@popup").unwrap_or_default();
            let w = width.clamp(20, area.width);
            // Size to the largest page so switching pages doesn't jump.
            let tallest = (0..panel::page_titles(&v).len().max(1))
                .map(|p| panel::height(&v, &panel::State { page: p, filter: st.filter.clone(), open: st.open.clone(), ..Default::default() }, w.saturating_sub(4)))
                .max()
                .unwrap_or(1);
            let h = (tallest + 4 + (!st.filter.is_empty()) as u16).min(area.height);
            let rect = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height.saturating_sub(h)) / 3, width: w, height: h };
            f.render_widget(Clear, rect);
            let title = v["title"].as_str().unwrap_or(&view).to_string();
            let block = Block::bordered()
                .border_type(ratatui::widgets::BorderType::Rounded)
                .title(Span::styled(format!(" {title} "), Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
                .title_bottom(Line::from(Span::styled(
                    if panel::page_titles(&v).is_empty() { " ↑↓ move · enter select · / filter · esc close " } else { " ↑↓ move · tab page · enter select · / filter · esc close " },
                    Style::new().fg(Color::DarkGray),
                )).right_aligned())
                .border_style(Style::new().fg(Color::Cyan));
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            let body = Rect { x: inner.x + 1, y: inner.y + 1, width: inner.width.saturating_sub(2), height: inner.height.saturating_sub(1) };
            let hits = panel::render(f.buffer_mut(), body, &v, &mut st, true);
            self.pstate.insert("@popup".into(), st);
            self.hit.panels.insert("@popup".into(), hits);
            self.hit.overlay = Some((rect, Rect::default(), 0));
            return;
        }
        let (title, items, sel, query) = match &self.overlay {
            Some(Overlay::Popup { .. }) => return,
            None => {
                self.hit.overlay = None;
                return;
            }
            Some(Overlay::Prefix { sel }) => {
                let items = self.prefix_items().into_iter().map(|(k, l)| format!(" {k:>2}  {l}")).collect();
                (" prim (press a key or click) ", items, *sel, None::<String>)
            }
            Some(Overlay::Palette { query, sel }) => {
                let items = self.palette_items().into_iter().map(|n| format!("{n}  {}", self.commands[&n]["title"].as_str().unwrap_or(""))).collect();
                let _ = query;
                (" commands (type to filter) ", items, *sel, None)
            }
            Some(Overlay::Pick { sel, .. }) => {
                let items = self
                    .resources
                    .values()
                    .map(|r| format!("{}  {}  {}", r["id"].as_str().unwrap_or(""), r["name"].as_str().or_else(|| r["argv"][0].as_str()).unwrap_or(""), r["state"].as_str().unwrap_or("")))
                    .collect::<Vec<_>>();
                (" pick a terminal ", items, *sel, None)
            }
            Some(Overlay::Menu { items, sel, .. }) => (" ", items.iter().map(|i| format!(" {}", i.label)).collect(), *sel, None),
            Some(Overlay::Input { .. }) => {
                // Typed in the status bar; clicking anywhere else cancels.
                self.hit.overlay = Some((self.hit.status, Rect::default(), 0));
                return;
            }
        };
        let items: Vec<String> = items;
        let w = area.width.min(64);
        let h = (items.len() as u16 + 2 + query.is_some() as u16).min(area.height).max(3);
        let rect = if matches!(self.overlay, Some(Overlay::Prefix { .. }) | Some(Overlay::Palette { .. })) {
            // Anchored above the prompt line it belongs to.
            let w = if matches!(self.overlay, Some(Overlay::Prefix { .. })) { w.min(44) } else { w };
            Rect { x: area.x, y: area.bottom().saturating_sub(h), width: w, height: h }
        } else if let Some(Overlay::Menu { at, .. }) = &self.overlay {
            // At the pointer, kept on screen.
            let w = (items.iter().map(|i| i.chars().count()).max().unwrap_or(10) as u16 + 4).min(area.width);
            let x = at.x.min(area.right().saturating_sub(w));
            let y = at.y.min(area.bottom().saturating_sub(h));
            Rect { x, y, width: w, height: h }
        } else {
            Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 3, width: w, height: h }
        };
        f.render_widget(Clear, rect);
        let block = Block::bordered().title(title).border_style(Style::new().fg(Color::Cyan));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let list_area = match query {
            Some(q) => {
                f.render_widget(Paragraph::new(format!("> {q}")), Rect { height: 1, ..inner });
                Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner }
            }
            None => inner,
        };
        let mut state = ListState::default();
        state.select(Some(sel));
        let list = List::new(items.into_iter().map(ListItem::new).collect::<Vec<_>>()).highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        f.render_stateful_widget(list, list_area, &mut state);
        self.hit.overlay = Some((rect, list_area, state.offset()));
    }
}

fn with_arg(args: &Value, key: &str, v: Value) -> Value {
    let mut a = if args.is_object() { args.clone() } else { json!({}) };
    a[key] = v;
    a
}

fn on_divider(d: &Divider, p: Position) -> bool {
    let (a, b) = (d.children[d.index], d.children[d.index + 1]);
    if d.side_by_side {
        p.y >= d.parent.y && p.y < d.parent.bottom() && (p.x + 1 == a.right() || p.x == b.x)
    } else {
        p.x >= d.parent.x && p.x < d.parent.right() && (p.y + 1 == a.bottom() || p.y == b.y)
    }
}

/// The node at a path of child indices (split children or tab nodes).
fn node_at_mut<'a>(root: &'a mut Value, path: &[usize]) -> Option<&'a mut Value> {
    let mut n = root;
    for &i in path {
        n = if n.get("children").is_some() {
            n.get_mut("children")?.get_mut(i)?
        } else if n.get("tabs").is_some() {
            n.get_mut("tabs")?.get_mut(i)?.get_mut("node")?
        } else {
            return None;
        };
    }
    Some(n)
}

/// Lay out a layout node: tabs, splits and view leaves, recording hit areas.
fn walk(node: &Value, area: Rect, path: &mut Vec<usize>, overrides: &HashMap<Vec<usize>, Vec<u16>>, tabbar: &[Item], hit: &mut Hit, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if let Some(v) = node["view"].as_str() {
        let inner = Block::bordered().inner(area);
        hit.leaves.push((v.to_string(), area, inner));
        hit.leaf_paths.push(path.clone());
    } else if let Some(children) = node["children"].as_array() {
        let weights: Vec<u16> = match (overrides.get(path.as_slice()), node["sizes"].as_array()) {
            (Some(o), _) if o.len() == children.len() => o.clone(),
            (_, Some(s)) if s.len() == children.len() => s.iter().map(|x| x.as_u64().unwrap_or(1).clamp(1, 10_000) as u16).collect(),
            _ => vec![1; children.len()],
        };
        let constraints: Vec<Constraint> = weights.iter().map(|w| Constraint::Fill(*w)).collect();
        let side_by_side = node["split"] != "v";
        let rects = if side_by_side { Layout::horizontal(constraints).split(area) } else { Layout::vertical(constraints).split(area) };
        for i in 0..children.len().saturating_sub(1) {
            hit.dividers.push(Divider { path: path.clone(), index: i, side_by_side, parent: area, children: rects.to_vec() });
        }
        for (i, (c, r)) in children.iter().zip(rects.iter()).enumerate() {
            path.push(i);
            walk(c, *r, path, overrides, tabbar, hit, buf);
            path.pop();
        }
    } else if let Some(tabs) = node["tabs"].as_array() {
        let active = (node["active"].as_u64().unwrap_or(0) as usize).min(tabs.len().saturating_sub(1));
        let [bar, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        let mut spans = Vec::new();
        let mut x = bar.x;
        for (i, t) in tabs.iter().enumerate() {
            let label = format!(" {}:{} ", i + 1, t["title"].as_str().unwrap_or("tab"));
            let w = label.chars().count() as u16;
            hit.tabs.push((path.clone(), i, Rect { x, y: bar.y, width: w.min(bar.right().saturating_sub(x)), height: 1 }));
            x = x.saturating_add(w);
            let style = if i == active { Style::new().bg(Color::Blue).fg(Color::White) } else { Style::new().fg(Color::Gray) };
            spans.push(Span::styled(label, style));
        }
        // Contributed tab-bar buttons (e.g. `+` from the tabs extension), on the first tab bar.
        if !hit.buttons.iter().any(|(_, _, c)| c.get("tab_path").is_some()) {
            for item in tabbar {
                let label = format!(" {} ", item.label);
                let w = label.chars().count() as u16;
                if x + w > bar.right() {
                    break;
                }
                hit.buttons.push((Rect { x, y: bar.y, width: w, height: 1 }, item.clone(), json!({"tab_path": path.clone()})));
                x += w;
                spans.push(Span::styled(label, Style::new().fg(Color::Cyan)));
            }
        }
        ratatui::widgets::Widget::render(Paragraph::new(Line::from(spans)), bar, buf);
        if let Some(t) = tabs.get(active) {
            path.push(active);
            walk(&t["node"], body, path, overrides, tabbar, hit, buf);
            path.pop();
        }
    }
}

fn nz(n: u16) -> std::num::NonZeroU16 {
    std::num::NonZeroU16::new(n.max(1)).unwrap()
}

fn new_parser(cols: u16, rows: u16) -> vt100::Parser {
    vt100::Parser::new(nz(rows), nz(cols), 2000)
}

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn render_screen(buf: &mut Buffer, area: Rect, screen: &vt100::Screen) {
    let (rows, cols) = screen.size();
    let (rows, cols) = (u16::from(rows), u16::from(cols));
    for row in 0..rows.min(area.height) {
        for col in 0..cols.min(area.width) {
            let Some(cell) = screen.cell(row, col) else { continue };
            if cell.is_wide_continuation() {
                continue;
            }
            let mut style = Style::new().fg(color(cell.fgcolor())).bg(color(cell.bgcolor()));
            if cell.bold() {
                style = style.add_modifier(Modifier::BOLD);
            }
            if cell.italic() {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if cell.underline() {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            if cell.inverse() {
                style = style.add_modifier(Modifier::REVERSED);
            }
            let text = cell.contents();
            if let Some(out) = buf.cell_mut((area.x + col, area.y + row)) {
                out.set_symbol(if text.is_empty() { " " } else { text });
                out.set_style(style);
            }
        }
    }
}
