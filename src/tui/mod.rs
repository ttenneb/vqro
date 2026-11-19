//! `vqro tui`: a client that renders workspace primitives. It holds no workspace behaviour (D17):
//! layout, views, menus, bindings and commands all come from the core and extensions. It owns only
//! mechanism (rendering, focus, mouse mechanics, menus drawn from `menu/*` entries, text prompts,
//! pickers) and recovery that must work when every extension is broken:
//!
//! Prefix: Ctrl-g (setting `doc/settings/tui.prefix`), or click the `vqro` badge. A menu lists every
//!   action; press its key or click it. Default keys: : commands · o/O next/previous pane · 1-9 tab
//!   · u undo · R reset layout · ! safe mode · q quit; the prefix twice sends it to the program.
//!   `binding/*` entries add keys and, with a higher `priority`, override these. Whatever the keymap,
//!   the menu keeps the recovery actions (commands, undo, reset, safe mode, quit).
//!
//! Mouse mechanics: click to focus, click tabs, drag borders (one layout change on release), wheel
//! scrollback, forwarding to programs that asked for the mouse. What other gestures mean comes from
//! bindings such as `{"key": "mouse.right", "where": "pane", "command": "client.menu"}`.
//! Client commands: `client.menu {where}`, `client.prefix`, `client.palette`, `client.focus {dir}`,
//! `client.tab {to}`, `client.quit`, `client.none` (unbinds a key).

mod icons;
mod keys;
mod mouse;
mod panel;
mod compose;
mod probe;
mod frame_budget;
mod theme;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

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
use icons::Icons;
use theme::Palette;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// Largest paste the client sends (after bracketed-paste markers). Bigger pastes are refused whole,
/// with a message; nothing is sent. Sent in ordered chunks of at most `PASTE_CHUNK` bytes (the core's
/// per-message input limit), each confirmed before the next.
const MAX_PASTE: usize = 4 * 1024 * 1024;
const PASTE_CHUNK: usize = crate::adapter::MAX_STREAM_CHUNK;
/// How long one chunk may keep meeting a full input queue (`overloaded`) before the paste stops.
const PASTE_STALL: std::time::Duration = std::time::Duration::from_secs(30);

/// What to send for a paste: the text, wrapped in bracketed-paste markers when the program asked for
/// them (DECSET 2004). Markers inside the text are removed so a paste can't end the bracket early.
fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if !bracketed {
        return text.as_bytes().to_vec();
    }
    let clean = text.replace("\x1b[201~", "").replace("\x1b[200~", "");
    let mut out = Vec::with_capacity(clean.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(clean.as_bytes());
    out.extend_from_slice(b"\x1b[201~");
    out
}

struct Term {
    resource: String,
    stream: Option<u64>,
    parser: vt100::Parser,
    size: (u16, u16),
    attaching: bool,
    closed: Option<String>,
    retry_at: Option<std::time::Instant>,
    /// Extended key reporting the program asked for (kitty protocol, modifyOtherKeys), from its output.
    /// Kept across reattach: the program still has it set even though a repaint doesn't repeat it.
    modes: keys::KeyModes,
}

impl Term {
    /// Program output: to the VT parser and the key-mode tracker.
    fn feed(&mut self, bytes: &[u8]) {
        self.modes.feed(bytes);
        self.parser.process(bytes);
    }

    fn close(&mut self, reason: String) {
        self.stream = None;
        self.attaching = false;
        self.closed = Some(reason);
        self.retry_at = Some(std::time::Instant::now() + std::time::Duration::from_secs(1));
    }

    fn can_attach(&self, live: bool, now: std::time::Instant) -> bool {
        live && self.stream.is_none() && !self.attaching && self.retry_at.is_none_or(|at| now >= at)
    }

    fn begin_attach(&mut self) {
        self.attaching = true;
        self.closed = None;
        // A gap invalidates VT parser state. Only the backend's new attach/history repaint can
        // restore it; never concatenate an old partial escape sequence with a fresh stream.
        self.parser = new_parser(self.size.0, self.size.1);
    }
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
    /// Drawn before the label where items are shown (status bar, tab bar, menus); see `icons.rs`.
    icon: Value,
    command: String,
    args: Value,
    input: Option<String>,
    pick: Option<String>,
    /// For status items: take the label (and style) from this view's live content.
    view: Option<String>,
    /// Explicit initial input text: a string, or {context: field} read from invocation context.
    prefill: Value,
}

/// A row of the prefix menu: the key that runs it (None for a recovery action whose key now does
/// something else), its label, and the binding it runs.
struct PrefixRow {
    key: Option<String>,
    label: String,
    binding: Value,
}

impl Item {
    fn from_entry(v: &Value) -> Item {
        Item {
            label: v["label"].as_str().unwrap_or("").to_string(),
            icon: v["icon"].clone(),
            command: v["command"].as_str().unwrap_or("").to_string(),
            args: v["args"].clone(),
            input: v["input"].as_str().map(String::from),
            pick: v["pick"].as_str().map(String::from),
            view: v["view"].as_str().map(String::from),
            prefill: v["prefill"].clone(),
        }
    }
}

enum Msg {
    Attached { connection: u64, view: String, resource: String, result: Result<u64, String> },
    Info(String),
    /// A command finished: say so unless something newer took the status line.
    Done(String),
    Snapshot { generation: u64, result: crate::rpc::RpcResult },
    ClearOverrides,
    /// A command's reply asked this client to show a view as a popup.
    Popup { view: String, width: u16, ctx: Value },
    ClosePopup,
    /// A command's reply asks to focus a visible view, independently of its kind.
    FocusView(String),
    /// A stream's ordered input sender found its queue empty after `n` items: if nothing was queued
    /// since, its input can go direct again.
    InputIdle(String, u64),
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
    safe_mode_allow: BTreeSet<String>,
    provider_extensions: BTreeSet<String>,
    safe_selected: BTreeSet<String>,
    backends_connected: BTreeSet<String>,
    backend_notice: Option<String>,
    terms: HashMap<String, Term>,
    stream_view: HashMap<u64, String>,
    early: HashMap<u64, Vec<Vec<u8>>>,
    early_closed: HashMap<u64, String>,
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
    /// Merged immutable panel values; invalidate only on this view's entry/live update or snapshot.
    panel_values: Mutex<HashMap<String, Arc<Value>>>,
    panel_dependencies: Mutex<HashMap<String, HashSet<String>>>,
    hit: Hit,
    drag: Option<Drag>,
    overrides: HashMap<Vec<usize>, Vec<u16>>,
    mouse_owner: Option<String>,
    mouse_on: bool,
    /// A popup waiting under a menu, prompt or picker opened from it; it comes back when that closes.
    under: Option<Overlay>,
    /// Streams with a paste in progress: all their input goes through this ordered sender until idle.
    input_queues: HashMap<String, (mpsc::UnboundedSender<Vec<u8>>, u64)>,
    /// Each terminal view's current stream, shared with paste senders so a paste interrupted by a
    /// reattach (a gap ends a stream) continues on the view's new stream.
    view_streams: Arc<Mutex<HashMap<String, u64>>>,
    /// The pane shown alone (`client.zoom`): this window's presentation only, never workspace state.
    zoomed: Option<String>,
    /// Focus this view on the next draw (after leaving zoom the leaves are recomputed).
    pending_focus: Option<String>,
    /// The terminal speaks the kitty keyboard protocol; used only while the prefix menu is open.
    kbd_enhance: bool,
    /// Modifiers held after the prefix (reported only with keyboard enhancement), shown live.
    held: KeyModifiers,
    message: String,
    /// A native hint template; only its key ids are formatted, never arbitrary message text.
    key_hint: Option<(String, String, String)>,
    quit: bool,
    connected: bool,
    connection: u64,
    epoch: String,
    seq: u64,
    /// Incremented on snapshot requests and disconnects to fence late replies.
    generation: u64,
    resyncing: bool,
    buffered: Vec<Value>,
    buffered_bytes: usize,
    recovery_only: bool,
    retry_snapshot: Option<std::time::Instant>,
    snapshot_retries: u32,
}

/// The installed binary's identity, to notice a reinstall.
fn strings(value: &Value) -> BTreeSet<String> { value.as_array().into_iter().flatten().filter_map(Value::as_str).map(String::from).collect() }

fn exe_stamp() -> Option<(std::path::PathBuf, std::time::SystemTime)> {
    let exe = client::executable_path().ok()?;
    let modified = std::fs::metadata(&exe).ok()?.modified().ok()?;
    Some((exe, modified))
}

type Connection = (Peer, crate::rpc::IncomingReceiver, Value);

enum UiInput {
    Incoming(Option<Incoming>),
    Message(Msg),
    Key(Event),
    Reconnected(Connection),
    Tick,
    Frame,
    Closed,
}

/// In particular, a pending reconnect must never take the key receiver out of this select.
async fn next_input(
    app: &App,
    incoming: &mut crate::rpc::IncomingReceiver,
    messages: &mut mpsc::UnboundedReceiver<Msg>,
    keys: &mut mpsc::UnboundedReceiver<Event>,
    reconnecting: &mut Option<tokio::task::JoinHandle<Connection>>,
    redraw: Option<std::time::Instant>,
) -> Result<UiInput> {
    Ok(tokio::select! {
        biased;
        Some(ev) = keys.recv() => UiInput::Key(ev),
        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(redraw.unwrap_or_else(std::time::Instant::now))), if redraw.is_some() => UiInput::Frame,
        result = async { reconnecting.as_mut().unwrap().await }, if reconnecting.is_some() => UiInput::Reconnected(result?),
        // An absolute deadline ahead of incoming events cannot be reset/starved by a storm.
        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(app.retry_snapshot.unwrap_or_else(std::time::Instant::now))), if app.retry_snapshot.is_some() => UiInput::Tick,
        Some(m) = messages.recv() => UiInput::Message(m),
        m = incoming.recv(), if app.connected => UiInput::Incoming(m),
        _ = tokio::time::sleep(std::time::Duration::from_millis(500)), if app.toast.is_some() || app.terms.values().any(|t| t.retry_at.is_some()) => UiInput::Tick,
        else => UiInput::Closed,
    })
}

fn recovery_snapshot(hello: &Value, notice: &str) -> Value {
    json!({"epoch":hello["epoch"], "seq":hello["seq"], "recovery_only":true,
        "recovery_notice":format!("RECOVERY ONLY: {notice}; use undo/safe mode or CLI apply/delete, then retry"),
        "entries":{}, "resources":[], "live":{},
        "commands":crate::core::BUILTINS.iter().map(|(name,title)| json!({"name":name,"title":title,"available":true,"owner":"core"})).collect::<Vec<_>>()})
}

const SNAPSHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

fn snapshot_transient(error: &crate::rpc::RpcError) -> bool {
    matches!(error.kind(), "overloaded" | "unavailable" | "timeout")
}

/// Bounded exponential backoff with independent jitter: 0.5–3 seconds, never a hot retry loop.
fn snapshot_retry_delay(attempt: u32) -> std::time::Duration {
    let base = 250_u64 << attempt.clamp(1,3);
    let mut bytes = [0; 8];
    let _ = getrandom::fill(&mut bytes);
    std::time::Duration::from_millis(base + u64::from_ne_bytes(bytes) % (base / 2 + 1))
}

async fn snapshot_for_client(peer: &Peer, hello: &Value) -> Result<Value> {
    match peer.call_with_timeout("workspace.snapshot", Value::Null, SNAPSHOT_TIMEOUT).await {
        Ok(snapshot) => Ok(snapshot),
        Err(error) if error.kind() == "too_large" => Ok(recovery_snapshot(hello, &error.message)),
        Err(error) if snapshot_transient(&error) => {
            let mut snap = recovery_snapshot(hello, &error.message);
            snap["recovery_notice"] = json!(format!("core busy: {error}; retrying snapshot (input is not replayed)"));
            snap["retry_snapshot"] = json!(true);
            Ok(snap)
        }
        Err(error) => Err(error.into()),
    }
}

pub async fn run(socket: &Path) -> Result<()> {
    let started_as = exe_stamp();
    let mut reexec = false;
    let (peer, mut incoming, hello) = client::connect(socket, "tui").await?;
    let snap = snapshot_for_client(&peer, &hello).await?;
    let seq = snap["seq"].as_u64().unwrap_or(0);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut app = App::new(peer.clone(), tx);
    app.load(&snap);
    if let Some(notice)=snap["recovery_notice"].as_str() {app.message=notice.into();}
    else {app.set_key_hint("click vqro or press ", &app.prefix(), " for the menu");}
    peer.call("events.subscribe", json!({"from": seq, "epoch": snap["epoch"], "partial_resources": true})).await?;

    let mut terminal = ratatui::init();
    // Asked before the input thread starts reading: the query reads the terminal's reply.
    app.kbd_enhance = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    let restore_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        keys::enhance(keys::Enhance::Off);
        restore_hook(info)
    }));
    let (key_tx, mut keys) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(ev) = crossterm::event::read() {
            if key_tx.send(ev).is_err() {
                break;
            }
        }
    });

    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste, crossterm::event::EnableMouseCapture);
    let mut reconnecting = None;
    let mut probe = probe::Probe::from_env();
    let result: Result<()> = async {
        let mut dirty = true;
        let mut interactive = false;
        let mut frames = frame_budget::FrameBudget::new();
        loop {
            let now = std::time::Instant::now();
            if frames.ready(dirty, interactive, now) {
                probe.draw(interactive, || terminal.draw(|f| app.draw(f)))?;
                frames.drawn(now);
                dirty = false;
            }
            if app.quit {
                return Ok(());
            }
            let input = next_input(&app, &mut incoming, &mut rx, &mut keys, &mut reconnecting, frames.deadline(dirty)).await?;
            let key_input = matches!(input, UiInput::Key(_));
            dirty |= match input {
                UiInput::Incoming(m) => match m {
                    Some(m) => { probe.incoming(&m); app.on_incoming(m); true }
                    None => {
                        // The core went away (restart, reinstall): bring it back and reconnect.
                        app.disconnected();
                        reconnecting = Some(tokio::spawn(reconnect(socket.to_path_buf(), app.tx.clone())));
                        true
                    }
                },
                UiInput::Reconnected((peer, new_incoming, snap)) => {
                    reconnecting = None;
                    incoming = new_incoming;
                    app.reconnected(peer, &snap);
                    // The next draw recreates terminal views and attaches with backend history.
                    if exe_stamp() != started_as {
                        reexec = true;
                        return Ok(());
                    }
                    true
                },
                UiInput::Message(m) => { app.on_msg(m); true }
                UiInput::Key(ev) => app.on_event(ev),
                UiInput::Tick => { app.retry_snapshot_if_due(); true },
                UiInput::Frame => false,
                UiInput::Closed => return Ok(()),
            };
            dirty |= app.restore_under();
            // Drain whatever else is ready before redrawing.
            for _ in 0..if key_input { 0 } else { 64 } {
                if !keys.is_empty() { break; }
                let Ok(m) = incoming.try_recv() else { break };
                probe.incoming(&m);
                app.on_incoming(m);
                dirty = true;
            }
            interactive = key_input && dirty;
            // Richer key reports while the prefix menu is open; "disambiguate" only while the focused
            // program asked for extended keys (so Shift-Enter and the like can reach it). Otherwise the
            // outer terminal's keyboard reporting is left as it was.
            let prefix_open = matches!(app.overlay, Some(Overlay::Prefix { .. }));
            if app.kbd_enhance {
                keys::enhance(if prefix_open {
                    keys::Enhance::Full
                } else if app.focused_extended() != keys::Extended::Legacy {
                    keys::Enhance::Disambiguate
                } else {
                    keys::Enhance::Off
                });
            }
            if !prefix_open {
                app.held = KeyModifiers::NONE;
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
    if let Some(task) = reconnecting {
        task.abort();
    }
    // Every way out (quit, error, re-exec) passes here; pop before leaving the alternate screen.
    keys::enhance(keys::Enhance::Off);
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture, crossterm::event::DisableBracketedPaste);
    ratatui::restore();
    if reexec {
        if let Some((exe, _)) = exe_stamp() {
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                let args: Vec<String> = std::env::args().skip(1).collect();
                let err = std::process::Command::new(exe).args(args).exec();
                anyhow::bail!("could not restart into the new vqro: {err}");
            }
        }
    }
    result
}

/// Runs outside the UI state: neither connection attempts nor retry sleeps block keys or drawing.
async fn reconnect(socket: std::path::PathBuf, tx: mpsc::UnboundedSender<Msg>) -> Connection {
    loop {
        let attempt: Result<_> = async {
            client::ensure_core().await?;
            let (peer, incoming, hello) = client::connect(&socket, "tui").await?;
            let snap = snapshot_for_client(&peer, &hello).await?;
            peer.call("events.subscribe", json!({"from": snap["seq"], "epoch": snap["epoch"], "partial_resources": true})).await?;
            Ok((peer, incoming, snap))
        }.await;
        match attempt {
            Ok(connection) => return connection,
            Err(e) => {
                let _ = tx.send(Msg::Info(format!("reconnecting: {e}")));
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
}

impl App {
    fn new(peer: Peer, tx: mpsc::UnboundedSender<Msg>) -> Self {
        Self {
            peer, tx,
            entries: BTreeMap::new(), versions: BTreeMap::new(), resources: BTreeMap::new(),
            commands: BTreeMap::new(), extensions: BTreeMap::new(), safe_mode: false,
            safe_mode_allow: BTreeSet::new(), provider_extensions: BTreeSet::new(), safe_selected: BTreeSet::new(), backends_connected:BTreeSet::new(), backend_notice:None,
            terms: HashMap::new(), stream_view: HashMap::new(), early: HashMap::new(), early_closed: HashMap::new(),
            leaves: Vec::new(), focus: 0, overlay: None, pstate: HashMap::new(),
            filtering: None, toast: None, live: HashMap::new(), panel_values: Mutex::new(HashMap::new()), panel_dependencies: Mutex::new(HashMap::new()), hit: Hit::default(),
            drag: None, overrides: HashMap::new(), mouse_owner: None, mouse_on: true,
            under: None, input_queues: HashMap::new(), view_streams: Arc::default(), zoomed: None, pending_focus: None, kbd_enhance: false,
            held: KeyModifiers::NONE, message: String::new(), key_hint: None,
            quit: false, connected: true, connection: 0, epoch: String::new(), seq: 0,
            generation: 0, resyncing: false, buffered: Vec::new(), buffered_bytes: 0, recovery_only: false, retry_snapshot: None, snapshot_retries: 0,
        }
    }

    fn disconnected(&mut self) {
        self.message = "core disconnected, reconnecting…".into();
        self.connected = false;
        self.connection += 1;
        self.generation += 1;
        self.resyncing = false;
        self.buffered.clear();
        self.buffered_bytes = 0;
        self.retry_snapshot = None;
        self.snapshot_retries = 0;
    }

    fn reconnected(&mut self, peer: Peer, snap: &Value) {
        self.peer = peer;
        self.connected = true;
        self.terms.clear();
        self.stream_view.clear();
        self.early.clear();
        self.early_closed.clear();
        self.load(snap);
        self.message = snap["recovery_notice"].as_str().unwrap_or("reconnected").into();
    }

    fn load(&mut self, snap: &Value) {
        self.recovery_only = snap["recovery_only"].as_bool().unwrap_or(false);
        self.snapshot_retries = u32::from(snap["retry_snapshot"] == true);
        self.retry_snapshot = (self.snapshot_retries > 0).then(|| std::time::Instant::now() + snapshot_retry_delay(self.snapshot_retries));
        self.epoch = snap["epoch"].as_str().unwrap_or("").to_string();
        self.seq = snap["seq"].as_u64().unwrap_or(0);
        self.panel_values.lock().unwrap().clear();
        self.panel_dependencies.lock().unwrap().clear();
        for state in self.pstate.values_mut() { state.prepared = None; }
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
        let was_safe=self.safe_mode;
        self.safe_mode = snap["safe_mode"].as_bool().unwrap_or(false);
        self.safe_mode_allow = strings(&snap["safe_mode_allow"]);
        self.provider_extensions = strings(&snap["provider_extensions"]);
        self.backend_notice = snap["backend_notice"].as_str().filter(|s| !s.is_empty()).map(String::from);
        self.backends_connected=snap["providers"].as_object().into_iter().flatten().filter(|(_,p)|p["role"]=="backend" && p["connected"]==true).map(|(name,_)|name.clone()).collect();
        self.live = snap["live"].as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
        if self.safe_mode && !was_safe { self.open_safe_chooser(); }
        if !self.safe_mode && matches!(&self.overlay,Some(Overlay::Popup{view,..}) if view=="@vqro-safe-mode") { self.overlay=None; }
    }

    fn on_incoming(&mut self, m: Incoming) {
        let Incoming::Notification { method, params } = m else { return };
        match method.as_str() {
            "stream.data" => {
                let (Some(sid), Some(data)) = (params["stream"].as_u64(), params["data"].as_str()) else { return };
                let Ok(bytes) = B64.decode(data) else { return };
                match self.stream_view.get(&sid).and_then(|v| self.terms.get_mut(v)) {
                    Some(t) => t.feed(&bytes),
                    None => {
                        if self.early_closed.contains_key(&sid) { return; }
                        if !self.terms.values().any(|t| t.attaching) { self.peer.notify("stream.close", json!({"stream":sid})); return; }
                        let charged = |chunks: &Vec<Vec<u8>>| chunks.iter().map(|b| b.len().max(64)).sum::<usize>();
                        let total: usize = self.early.values().map(charged).sum();
                        let pane = self.early.get(&sid).map(charged).unwrap_or(0);
                        if pane + bytes.len().max(64) > crate::adapter::STREAM_BUFFER_BYTES || total + bytes.len().max(64) > crate::rpc::STREAM_OUTBOUND_BYTES {
                            self.early.remove(&sid);
                            self.remember_early_close(sid, "slow_consumer".into());
                            self.peer.notify("stream.close", json!({"stream":sid}));
                        } else { self.early.entry(sid).or_default().push(bytes); }
                    },
                }
            }
            "stream.closed" | "stream.gap" => {
                let Some(sid) = params["stream"].as_u64() else { return };
                let reason = params["reason"]["reason"].as_str().or_else(|| params["reason"].as_str()).unwrap_or("closed").to_string();
                self.early.remove(&sid);
                if method == "stream.gap" { self.peer.notify("stream.close", json!({"stream": sid})); }
                if let Some(view) = self.stream_view.remove(&sid) {
                    if let Some(t) = self.terms.get_mut(&view) { t.close(reason); }
                } else if self.terms.values().any(|t| t.attaching) {
                    // Output/closure can race the attach reply's delivery to the UI task.
                    self.remember_early_close(sid, reason);
                }
            }
            "stream.error" => self.message = format!("stream input rejected (not replayed): {}", params["error"]["message"].as_str().unwrap_or("error")),
            "missed" => self.resync(),
            "event" => {
                // The next snapshot subsumes these events. Do not mutate a stale cache or
                // let an event storm bypass admission backoff while waiting to retry.
                if self.retry_snapshot.is_some() { return; }
                if self.recovery_only && !self.resyncing {
                    self.retry_snapshot.get_or_insert_with(|| std::time::Instant::now() + std::time::Duration::from_secs(1));
                    return;
                }
                if self.resyncing {
                    let bytes = crate::limits::encoded_bytes(&params).max(64);
                    // Bound recovery memory. A fresh snapshot subsumes a discarded batch.
                    if bytes > crate::core::LOG_BYTES { self.resync(); return; }
                    if self.buffered.len() == 10_000 || self.buffered_bytes + bytes > crate::core::LOG_BYTES { self.resync(); }
                    self.buffered_bytes += bytes;
                    self.buffered.push(params);
                } else {
                    self.on_event_record(&params);
                }
            }
            _ => {}
        }
    }

    fn remember_early_close(&mut self, sid: u64, reason: String) {
        if self.early_closed.len() >= crate::rpc::MAX_PENDING_CALLS && !self.early_closed.contains_key(&sid) { self.peer.close(); return; }
        self.early_closed.insert(sid, reason);
    }

    fn retry_snapshot_if_due(&mut self) {
        if self.retry_snapshot.is_some_and(|at| std::time::Instant::now() >= at) && !self.resyncing {
            self.retry_snapshot = None;
            self.resync();
        }
    }

    fn resync(&mut self) {
        if self.retry_snapshot.is_some() { return; }
        self.generation += 1;
        self.resyncing = true;
        self.buffered.clear();
        self.buffered_bytes = 0;
        let generation = self.generation;
        let (peer, tx) = (self.peer.clone(), self.tx.clone());
        tokio::spawn(async move {
            let result = peer.call_with_timeout("workspace.snapshot", Value::Null, SNAPSHOT_TIMEOUT).await;
            let _ = tx.send(Msg::Snapshot { generation, result });
        });
    }

    fn on_event_record(&mut self, ev: &Value) {
        if ev["epoch"].as_str() != Some(self.epoch.as_str()) {
            self.resync();
            return;
        }
        let seq = ev["seq"].as_u64().unwrap_or(0);
        if seq <= self.seq {
            return;
        }
        self.seq = seq;
        let d = &ev["data"];
        if d.get("backend_notice").is_some() {
            self.backend_notice = d["backend_notice"].as_str().filter(|s| !s.is_empty()).map(String::from);
        }
        match ev["type"].as_str().unwrap_or("") {
            "entries" => {
                for c in d["changes"].as_array().into_iter().flatten() {
                    let Some(k) = c["key"].as_str() else { continue };
                    if let Some(view) = k.strip_prefix("view/") {
                        self.invalidate_panel(view);
                        for state in self.pstate.values_mut() { state.prepared = None; }
                    }
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
                    // Merge the complete record before evaluating availability: partial status
                    // events must not accidentally make a stale resource available again.
                    let record = if ev["partial"] == true {
                        let Some(mut base) = self.resources.get(id).cloned() else {
                            self.resync();
                            return;
                        };
                        for (key, value) in d.as_object().into_iter().flatten() { base[key] = value.clone(); }
                        base
                    } else { d.clone() };
                    let available = record["available"].as_bool().unwrap_or(true);
                    let restored = available && self.resources.get(id).is_some_and(|old| old["available"] == false);
                    for term in self.terms.values_mut().filter(|t| t.resource == id) {
                        if !available {
                            if let Some(stream) = term.stream.take() {
                                self.stream_view.remove(&stream);
                                self.peer.notify("stream.close", json!({"stream": stream}));
                            }
                            term.close("resource unavailable".into());
                        } else if record["state"] == "live" {
                            term.closed = None;
                            if restored { term.retry_at = Some(std::time::Instant::now()); }
                        }
                    }
                    self.resources.insert(id.to_string(), record);
                }
            }
            "resource.removed" => {
                if let Some(id) = d["id"].as_str() {
                    self.resources.remove(id);
                }
            }
            "command" => {
                if let Some(n) = d["name"].as_str() {
                    if d["available"]==false {self.commands.remove(n);}
                    else {self.commands.insert(n.to_string(), d.clone());}
                }
            }
            "extension" => {
                if let Some(n) = d["name"].as_str() {
                    let st = d["state"].clone();
                    if st["status"] == "failed" {
                        self.message = format!("extension {n} failed: {}", st["failure"]["reason"].as_str().or_else(||st["last_exit"].as_str()).unwrap_or("?"));
                    }
                    self.extensions.insert(n.to_string(), st);
                }
            }
            "safe_mode" => {
                let was=self.safe_mode; self.safe_mode=d["on"].as_bool().unwrap_or(false);
                self.safe_mode_allow=strings(&d["allow"]); self.provider_extensions=strings(&d["provider_extensions"]);
                if self.safe_mode && !was { self.open_safe_chooser(); }
                if !self.safe_mode && matches!(&self.overlay,Some(Overlay::Popup{view,..}) if view=="@vqro-safe-mode") { self.overlay=None; }
            }
            "notify" => {
                let text = d["text"].as_str().unwrap_or("").to_string();
                self.toast = Some((text, d["style"].as_str().map(String::from), std::time::Instant::now() + std::time::Duration::from_secs(4)));
            }
            "view.content" => {
                if let Some(v) = d["view"].as_str() {
                    self.invalidate_panel(v);
                    for state in self.pstate.values_mut() { state.prepared = None; }
                    if d["content"].is_null() {
                        self.live.remove(v);
                    } else {
                        self.live.insert(v.to_string(), d["content"].clone());
                    }
                }
            }
            "adapter" | "provider" => {
                if let Some(name)=d["name"].as_str() { if d["state"]["role"]=="backend" && d["state"]["connected"]==true {self.backends_connected.insert(name.into());} else {self.backends_connected.remove(name);} }
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
            Msg::Done(name) => {
                if self.recovery_only && !self.resyncing { self.resync(); }
                if self.message == format!("running {name}…") {
                    self.message = format!("{name}: done");
                }
            }
            Msg::Snapshot { generation, result } => {
                if generation != self.generation || !self.connected {
                    return;
                }
                match result {
                    Ok(s) => {
                        let was_busy = self.snapshot_retries > 0;
                        self.load(&s);
                        if was_busy && self.message.starts_with("core busy:") { self.message = "core synchronized".into(); }
                        self.resyncing = false;
                        self.buffered_bytes = 0;
                        for ev in std::mem::take(&mut self.buffered) {
                            if ev["epoch"].as_str() == Some(self.epoch.as_str()) {
                                self.on_event_record(&ev);
                            }
                        }
                    }
                    Err(e) if snapshot_transient(&e) => {
                        self.resyncing = false;
                        self.buffered.clear(); self.buffered_bytes = 0;
                        self.snapshot_retries = self.snapshot_retries.saturating_add(1);
                        self.retry_snapshot = Some(std::time::Instant::now() + snapshot_retry_delay(self.snapshot_retries));
                        self.message = format!("core busy: {e}; retrying snapshot (input is not replayed)");
                    }
                    Err(e) if e.kind() == "too_large" => {
                        self.snapshot_retries = 0;
                        self.recovery_only = true;
                        self.resyncing = false;
                        self.buffered.clear(); self.buffered_bytes = 0;
                        self.message = format!("RECOVERY ONLY: {e}; use undo/safe mode or CLI delete");
                    }
                    Err(e) => {
                        // A local RPC deadline does not close the transport. Close it explicitly
                        // so the responsive reconnect state machine can recover (never replay input).
                        self.message = format!("resync failed: {e}");
                        self.peer.close();
                    }
                }
            }
            Msg::ClearOverrides => self.overrides.clear(),
            Msg::Popup { view, width, ctx } => {
                match self.view_kind(&view).as_str() {
                    "" => self.message = format!("popup: no view {view}"),
                    "terminal" => self.message = "popup refused: terminals never pop up".into(),
                    _ => {
                        self.under = None;
                        self.pstate.insert("@popup".into(), panel::State::default());
                        self.overlay = Some(Overlay::Popup { view, width, ctx });
                    }
                }
            }
            Msg::FocusView(view) => {
                self.zoomed = None;
                self.pending_focus = Some(view);
            }
            Msg::InputIdle(view, done) => {
                // Everything queued was sent: dropping the sender ends the task, and input goes direct
                // again without overtaking anything.
                if self.input_queues.get(&view).is_some_and(|(_, queued)| *queued == done) {
                    self.input_queues.remove(&view);
                }
            }
            Msg::ClosePopup => {
                if matches!(self.overlay, Some(Overlay::Popup { .. })) {
                    self.overlay = None;
                }
            }
            Msg::Attached { connection, view, resource, result } => {
                // Stream ids are scoped to a connection. Never close or adopt a stale id on the new peer.
                if connection != self.connection || !self.connected {
                    return;
                }
                let Some(t) = self.terms.get_mut(&view) else {
                    if let Ok(sid) = result {
                        self.early.remove(&sid);
                        self.early_closed.remove(&sid);
                        self.peer.notify("stream.close", json!({"stream": sid}));
                    }
                    return;
                };
                t.attaching = false;
                if t.resource != resource {
                    if let Ok(sid) = result {
                        self.early.remove(&sid);
                        self.early_closed.remove(&sid);
                        self.peer.notify("stream.close", json!({"stream": sid}));
                    }
                    return;
                }
                match result {
                    Ok(sid) => {
                        if let Some(reason) = self.early_closed.remove(&sid) {
                            self.early.remove(&sid);
                            t.close(reason);
                        } else {
                            t.stream = Some(sid);
                            t.retry_at = None;
                            t.closed = None;
                            self.view_streams.lock().unwrap().insert(view.clone(), sid);
                            self.stream_view.insert(sid, view);
                            for bytes in self.early.remove(&sid).unwrap_or_default() {
                                t.feed(&bytes);
                            }
                        }
                    }
                    Err(e) => t.close(e),
                }
            }
        }
    }

    // ---- input -------------------------------------------------------------------------------

    /// Returns whether a redraw is needed.
    fn on_event(&mut self, ev: Event) -> bool {
        match ev {
            // Bare modifiers (keyboard enhancement, prefix menu open): live feedback only.
            Event::Key(KeyEvent { code: KeyCode::Modifier(m), kind, modifiers, .. }) => {
                self.held = if kind == KeyEventKind::Release { modifiers - keys::modifier_of(m) } else { modifiers };
                matches!(self.overlay, Some(Overlay::Prefix { .. }))
            }
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                self.on_key(k);
                true
            }
            Event::Mouse(m) => self.on_mouse(m),
            Event::Paste(text) => {
                self.paste(&text);
                false
            }
            Event::Resize(..) => true,
            _ => false,
        }
    }

    /// Extended key reporting asked for by the program in the focused terminal (Legacy otherwise).
    fn focused_extended(&self) -> keys::Extended {
        self.leaves.get(self.focus).and_then(|v| self.terms.get(v)).map(|t| t.modes.extended()).unwrap_or_default()
    }

    fn send_to_focused(&mut self, bytes: Vec<u8>) {
        let Some(view) = self.leaves.get(self.focus).cloned() else { return };
        if let Some(t) = self.terms.get_mut(&view) {
            t.parser.screen_mut().set_scrollback(0);
            if let Some(sid) = t.stream {
                self.send_input(sid, bytes);
            }
        }
    }

    /// Input for a stream, in order. Direct, unless a paste is still being sent on that stream: then
    /// it queues behind the paste so keys never overtake it.
    fn send_input(&mut self, sid: u64, bytes: Vec<u8>) {
        let view = self.stream_view.get(&sid).cloned().unwrap_or_default();
        if let Some((tx, queued)) = self.input_queues.get_mut(&view) {
            if tx.send(bytes.clone()).is_ok() {
                *queued += 1;
                return;
            }
            self.input_queues.remove(&view);
        }
        self.peer.notify("stream.input", json!({"stream": sid, "data": B64.encode(bytes)}));
    }

    /// A paste into the focused terminal: bracketed when its program enabled bracketed paste (DECSET
    /// 2004), raw otherwise, like a terminal. Small pastes go as one input; large ones in ordered chunks,
    /// each confirmed by the core (a full input queue is waited out, never dropped). Over `MAX_PASTE`
    /// nothing is sent. A failure part-way says how much was sent; nothing is resent after an error.
    fn paste(&mut self, text: &str) {
        let Some(view) = self.leaves.get(self.focus).cloned() else { return };
        let Some(t) = self.terms.get_mut(&view) else { return };
        let Some(sid) = t.stream else {
            self.message = "paste not sent: the terminal is not attached".into();
            return;
        };
        t.parser.screen_mut().set_scrollback(0);
        let bytes = paste_bytes(text, t.parser.screen().bracketed_paste());
        if bytes.len() > MAX_PASTE {
            self.message = format!("paste not sent: {} bytes is over the {} MiB limit", bytes.len(), MAX_PASTE / (1024 * 1024));
            return;
        }
        // Small, or behind a paste still being sent (the queue chunks it): the ordinary path.
        if bytes.len() <= PASTE_CHUNK || self.input_queues.contains_key(&view) {
            return self.send_input(sid, bytes);
        }
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let total = bytes.len();
        let _ = tx.send(bytes);
        self.input_queues.insert(view.clone(), (tx, 1));
        let (peer, msgs, streams) = (self.peer.clone(), self.tx.clone(), self.view_streams.clone());
        tokio::spawn(async move {
            let mut done: u64 = 0;
            let mut sid = sid;
            loop {
                let item = match rx.try_recv() {
                    Ok(item) => item,
                    Err(mpsc::error::TryRecvError::Empty) => {
                        let _ = msgs.send(Msg::InputIdle(view.clone(), done));
                        match rx.recv().await {
                            Some(item) => item,
                            None => break,
                        }
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => break,
                };
                done += 1;
                // Each queued input is tried once in order; an error ends only that input (never resent).
                let mut failed = false;
                let mut sent = 0;
                for chunk in item.chunks(PASTE_CHUNK) {
                    let started = std::time::Instant::now();
                    loop {
                        match peer.call("stream.input", json!({"stream": sid, "data": B64.encode(chunk)})).await {
                            Ok(_) => break,
                            // A full input queue rejects without effect: wait and offer the same chunk again.
                            Err(e) if e.kind() == "overloaded" && started.elapsed() < PASTE_STALL => {
                                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                            }
                            // The stream ended (a gap or slow-viewer close) before this chunk was taken:
                            // refused, so it had no effect. Continue with it on the view's reattached
                            // stream; chunks already taken are never sent again.
                            Err(e) if matches!(e.kind(), "not_found" | "unavailable") && started.elapsed() < PASTE_STALL => {
                                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                                if let Some(next) = streams.lock().unwrap().get(&view).copied().filter(|n| *n != sid) {
                                    sid = next;
                                }
                            }
                            Err(e) => {
                                failed = true;
                                let _ = msgs.send(Msg::Info(format!("paste stopped after {sent} of {} bytes ({}); the rest was not sent", item.len(), e)));
                                break;
                            }
                        }
                    }
                    if failed {
                        break;
                    }
                    sent += chunk.len();
                }
                if !failed && item.len() == total && total > PASTE_CHUNK {
                    let _ = msgs.send(Msg::Info(format!("pasted {total} bytes")));
                }
            }
        });
    }

    fn on_key(&mut self, k: KeyEvent) {
        let is_prefix = keys::name(&k) == Some(self.prefix());
        // The prefix reaches recovery from popups, filters and anything opened over them (a drop-down,
        // prompt or picker is cancelled; a popup waiting underneath returns when the prefix menu closes).
        if is_prefix && (self.filtering.is_some() || matches!(self.overlay,Some(Overlay::Popup{..}|Overlay::Menu{..}|Overlay::Input{..}|Overlay::Pick{..}))) {self.filtering=None;self.overlay=Some(Overlay::Prefix{sel:0});return;}
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
                if let Some(bytes) = keys::encode(&k, t.parser.screen().application_cursor(), t.modes.extended()) {
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

    fn open_safe_chooser(&mut self) {
        self.safe_selected=self.safe_mode_allow.clone();
        self.pstate.remove("@popup");
        self.overlay=Some(Overlay::Popup { view:"@vqro-safe-mode".into(),width:76,ctx:json!({}) });
    }

    /// Recovery UI is generated in Rust, never published or supplied by an extension.
    fn safe_chooser_value(&self) -> Value {
        let mut declared:Vec<_>=self.entries.iter().filter_map(|(key,value)|key.strip_prefix("extension/").map(|name|(name,value))).collect();
        declared.sort_by_key(|(name,_)|*name);
        let mut items=vec![json!({"type":"text","text":"Nothing starts automatically. Choose this session's recovery set; saved enabled settings are unchanged."})];
        for (provider,title) in [(true,"Providers / backends"),(false,"UI extensions")] {
            items.push(json!({"type":"header","text":title}));
            let mut count=0;
            for (name,value) in &declared {
                if (value["kind"]=="provider" || self.provider_extensions.contains(*name)) != provider { continue; }
                let check=if self.safe_selected.contains(*name) { "[x]" } else { "[ ]" };
                let mut description=value["description"].as_str().or_else(||value["argv"][0].as_str()).unwrap_or("Declared extension").chars().take(160).collect::<String>();
                if let Some(state)=self.extensions.get(*name) { if state["status"]=="failed" {description.push_str(&format!(" -- failed: {}",state["last_exit"].as_str().unwrap_or("unknown failure")));} }
                items.push(json!({"id":format!("safe/{name}"),"text":format!("{check} {name}"),"description":description,"command":"client.safe_mode.toggle","args":{"name":name}}));count+=1;
            }
            if count==0 { items.push(json!({"type":"text","text":"(none declared)"})); }
        }
        items.push(json!({"type":"separator"}));
        items.push(json!({"id":"safe/apply","text":"Enable selected","command":"client.safe_mode.apply"}));
        items.push(json!({"id":"safe/leave","text":"Leave safe mode","command":"client.safe_mode.leave"}));
        json!({"title":"Safe mode recovery","items":items})
    }

    /// A panel's value: the view entry with its live content merged over it.
    fn panel_value(&self, view: &str) -> Arc<Value> {
        if view=="@vqro-safe-mode" { return Arc::new(self.safe_chooser_value()); }
        if let Some(value) = self.panel_values.lock().unwrap().get(view).cloned() { return value; }
        let (v, deps) = compose::resolve(view, &self.entries, &self.live);
        self.panel_dependencies.lock().unwrap().insert(view.into(), deps);
        let v = Arc::new(v);
        self.panel_values.lock().unwrap().insert(view.to_string(), v.clone());
        v
    }

    fn invalidate_panel(&self, source: &str) {
        let mut deps = self.panel_dependencies.lock().unwrap();
        let mut values = self.panel_values.lock().unwrap();
        values.retain(|view, _| view != source && !deps.get(view).is_some_and(|d| d.contains(source)));
        deps.retain(|view, _| values.contains_key(view));
    }

    /// Keys for a panel (in a pane or a popup). Returns whether it used the key.
    fn panel_key(&mut self, key: &str, view: &str, k: KeyEvent, ctx: Value) -> bool {
        let mut ctx = ctx;
        ctx["panel"] = json!(key);
        ctx["panel_view"] = json!(view);
        let value = self.panel_value(view);
        let mut container = self.pstate.get(key).cloned().unwrap_or_default();
        let pages = panel::page_titles(&value).len();
        if pages>0 && matches!(k.code,KeyCode::Char('[')|KeyCode::Char(']')) {
            container.page=if k.code==KeyCode::Char(']'){(container.page+1)%pages}else{(container.page+pages-1)%pages};container.sel=0;container.top=0;container.detail_focus=false;
            self.pstate.insert(key.into(),container);return true;
        }
        if let Some((id,_))=panel::selected_detail(&value,&container) {
            if matches!(k.code,KeyCode::Tab|KeyCode::BackTab) {
                panel::sync_detail(&mut container,&id);container.detail_focus=!container.detail_focus;self.pstate.insert(key.into(),container);return true;
            }
        }
        if k.code==KeyCode::Char('/') {container.detail_focus=false;self.pstate.insert(key.into(),container);self.filtering=Some(key.into());return true;}
        let selected=panel::selected_detail(&value,&container);
        let in_detail=container.detail_focus && selected.is_some();
        let (value,mut st)=if in_detail {let(id,v)=selected.unwrap();panel::sync_detail(&mut container,&id);(Arc::new(v),*container.detail.take().unwrap_or_default())}else{(value,container.clone())};
        let was_scrolling=st.manual_scroll;st.manual_scroll=false;
        let save=|mut root:panel::State,state:panel::State|{if in_detail{root.detail=Some(Box::new(state));root}else{state}};
        // A wheel-scrolled pane may have moved its selection out of view: reveal before activating.
        if was_scrolling && matches!(k.code,KeyCode::Enter|KeyCode::Char(' ')){self.pstate.insert(key.into(),save(container,st));return true;}
        let rows = panel::rows(&value, &st);
        let mut run = None;
        let chips = rows.get(st.sel).filter(|r| panel::kind(&r.item) == "chips").map(|r| r.item["chips"].as_array().map(|a| a.len()).unwrap_or(0));
        match k.code {
            KeyCode::Left | KeyCode::Char('h') if chips.is_some() => st.chip = st.chip.saturating_sub(1),
            KeyCode::Right | KeyCode::Char('l') if chips.is_some() => st.chip = (st.chip + 1).min(chips.unwrap_or(1).saturating_sub(1)),
            KeyCode::Up | KeyCode::Char('k') => st.sel = panel::step(&rows, st.sel, -1),
            KeyCode::Down | KeyCode::Char('j') => st.sel = panel::step(&rows, st.sel, 1),
            KeyCode::PageUp => st.sel = panel::step(&rows, st.sel, -(st.page_step.max(1) as i64)),
            KeyCode::PageDown => st.sel = panel::step(&rows, st.sel, st.page_step.max(1) as i64),
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
                    self.pstate.insert(key.to_string(), save(container,st));
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
        self.pstate.insert(key.to_string(), save(container,st));
        if let Some(item) = run {
            if item["options"].is_array() {
                let at = self.row_position(key, sel);
                self.options_menu(&item, ctx, at);
            } else {
                self.activate_item(Item::from_entry(&json!({"label": item["text"], "command": item["command"], "args": item["args"], "input": item["input"], "pick": item["pick"], "prefill": item["prefill"]})), ctx, None);
            }
        }
        true
    }

    /// Where a panel row was drawn, for anchoring dropdowns under it.
    fn row_position(&self, key: &str, row: usize) -> Option<Position> {
        let root_hits = self.hit.panels.get(key)?;
        let state=self.pstate.get(key);
        let detail=state.is_some_and(|s|s.detail_focus);
        let hits=if detail{root_hits.detail.as_deref()?}else{root_hits};
        let chip = state.map(|s|if detail{s.detail.as_deref().map(|s|s.chip).unwrap_or(0)}else{s.chip}).unwrap_or(0);
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
                    icon: Value::Null,
                    command: item["command"].as_str().unwrap_or("").to_string(),
                    args: with_arg(&item["args"], "value", o["value"].clone()),
                    // An option with `input` asks for its value instead (e.g. "custom…"), as `text`.
                    input: o["input"].as_str().map(String::from),
                    pick: None,
                    view: None,
                    prefill: o.get("prefill").or_else(|| item.get("prefill")).cloned().unwrap_or(Value::Null),
                }
            })
            .collect();
        if items.is_empty() || item["command"].as_str().is_none() {
            return;
        }
        let at = at.unwrap_or_default();
        self.open_over(Overlay::Menu { items, ctx, sel: 0, at });
    }

    /// A row's own actions (`menu`), shown like any other menu.
    fn row_menu(&mut self, item: &Value, ctx: Value, at: Option<Position>) {
        let items: Vec<Item> = item["menu"].as_array().into_iter().flatten().map(Item::from_entry).filter(|i| !i.command.is_empty()).collect();
        if items.is_empty() {
            return;
        }
        let at = at.or_else(|| self.hit.leaves.get(self.focus).map(|(_, o, _)| Position { x: o.x + 2, y: o.y + 2 })).unwrap_or_default();
        self.open_over(Overlay::Menu { items, ctx, sel: 0, at });
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

    fn set_key_hint(&mut self, before: &str, keys: &str, after: &str) {
        self.message=format!("{before}{keys}{after}");
        self.key_hint=Some((before.into(),keys.into(),after.into()));
    }

    fn message_label(&self) -> String {
        if let Some((before,keys,after))=&self.key_hint {
            if self.message==format!("{before}{keys}{after}") {
                return format!("{before}{}{after}",self.key_labels(keys));
            }
        }
        self.message.clone()
    }

    fn key_labels(&self, text: &str) -> String {
        keys::display_labels(text, self.entries.get("doc/settings/tui").and_then(|s|s["key_labels"].as_str()))
    }

    /// The prefix key in canonical form; Ctrl-g if the setting is missing or unusable.
    fn prefix(&self) -> String {
        keys::prefix(self.entries.get("doc/settings/tui").and_then(|s| s["prefix"].as_str()))
    }

    /// Every keyboard binding with its id that applies in `ctx` (the focused pane's context): the
    /// client's defaults, then visible `binding/*` entries whose `when` holds, exactly as for menu
    /// items. A binding whose `when` does not hold is no candidate, so the key falls through to the
    /// next binding for it (or to none).
    fn key_candidates(&self, ctx: &Value) -> Vec<(String, Value)> {
        let mut found: Vec<(String, Value)> = builtin_bindings().into_iter().map(|b| (format!("{BUILTIN}{}", b["key"].as_str().unwrap_or("")), b)).collect();
        found.extend(
            self.entries
                .iter()
                .filter(|(k, v)| k.starts_with("binding/") && !v["key"].as_str().unwrap_or("").starts_with("mouse.") && self.visible(v) && when_holds(v, ctx))
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        found
    }

    /// The binding a key (canonical name) runs on the focused pane, if any; `client.none` leaves
    /// the key unbound.
    fn binding_for(&self, key: &str, ctx: &Value) -> Option<Value> {
        let cands = self.key_candidates(ctx);
        let (_, b) = &cands[*resolve(&cands).get(key)?];
        (b["command"] != "client.none").then(|| b.clone())
    }

    /// A binding for this key that applies only on other panes (its `when` does not hold here).
    fn bound_elsewhere(&self, key: &str, ctx: &Value) -> Option<Value> {
        self.entries.iter()
            .filter(|(k, v)| k.starts_with("binding/") && self.visible(v) && !when_holds(v, ctx))
            .find(|(_, v)| v["key"].as_str().and_then(keys::normalize).as_deref() == Some(key))
            .map(|(_, v)| v.clone())
    }

    /// The prefix menu: the winning binding of every key (defaults first), then any recovery
    /// action no key reaches any more, so recovery survives every keymap.
    fn prefix_items(&self) -> Vec<PrefixRow> {
        let cands = self.key_candidates(&self.ctx_leaf(self.focus));
        let winners = resolve(&cands);
        let mut rows: Vec<PrefixRow> = cands
            .iter()
            .enumerate()
            .filter_map(|(i, (_, b))| {
                let key = keys::normalize(b["key"].as_str()?)?;
                (winners.get(&key) == Some(&i) && b["command"] != "client.none" && b["hidden"] != true).then(|| PrefixRow { key: Some(self.key_labels(&key)), label: self.binding_label(b), binding: b.clone() })
            })
            .collect();
        // Non-replaceable recovery: failed Settings/other extensions cannot hide their restart.
        let failed=self.extensions.iter().filter(|(_,state)|state["status"]=="failed").map(|(name,_)| {
            let allowed=if self.safe_mode {self.safe_mode_allow.contains(name)} else {self.entries.get(&format!("extension/{name}")).is_some_and(|v|v["enabled"]!=false)};
            let label=if allowed {format!("restart {name} (failed)")} else {format!("clear failure: {name} (stays stopped)")};
            PrefixRow {key:None,label,binding:json!({"command":"vqro.extension.restart","args":{"name":name}})}
        }).collect::<Vec<_>>();
        let at=rows.len().min(1);rows.splice(at..at,failed);
        for b in builtin_bindings().into_iter().filter(|b| b["recovery"] == true) {
            if !rows.iter().any(|r| r.binding["command"] == b["command"]) {
                rows.push(PrefixRow { key: None, label: self.binding_label(&b), binding: b });
            }
        }
        rows
    }

    fn binding_label(&self, b: &Value) -> String {
        let cmd = b["command"].as_str().unwrap_or("");
        if cmd == "vqro.safe_mode" && self.safe_mode {
            return "leave safe mode".into();
        }
        b["label"].as_str().or_else(|| self.commands.get(cmd).and_then(|c| c["title"].as_str())).unwrap_or(cmd).to_string()
    }

    fn on_prefix_key(&mut self, k: KeyEvent, is_prefix: bool) {
        if is_prefix {
            // The prefix twice sends it to the program.
            self.overlay = None;
            let (app_cursor, ext) = self.leaves.get(self.focus).and_then(|v| self.terms.get(v)).map(|t| (t.parser.screen().application_cursor(), t.modes.extended())).unwrap_or_default();
            if let Some(bytes) = keys::encode(&k, app_cursor, ext) {
                self.send_to_focused(bytes);
            }
            return;
        }
        let name = keys::name(&k);
        let ctx = self.ctx_leaf(self.focus);
        // A bound key runs; otherwise arrows and Enter work the menu.
        if let Some((n, b)) = name.as_deref().and_then(|n| Some((n, self.binding_for(n, &ctx)?))) {
            self.overlay = None;
            self.run_binding(&b);
            // Echo what ran, unless the binding opened a prompt that owns the status line.
            if self.overlay.is_none() {
                self.set_key_hint("", &format!("{} {n}",self.prefix()), &format!(" → {}",self.binding_label(&b)));
            }
            return;
        }
        let n = self.prefix_items().len();
        if let Some(Overlay::Prefix { sel }) = self.overlay.as_mut() {
            match k.code {
                KeyCode::Up => return *sel = sel.saturating_sub(1),
                KeyCode::Down => return *sel = (*sel + 1).min(n.saturating_sub(1)),
                KeyCode::Enter => return self.activate_overlay(),
                _ => {}
            }
        }
        self.overlay = None;
        if let Some(name) = name.filter(|n| n != "Esc") {
            // Say so before anything prompts: a pane-specific key does nothing here, harmlessly.
            match self.bound_elsewhere(&name, &ctx) {
                Some(b) => self.set_key_hint("nothing for ", &format!("{} {name}",self.prefix()), &format!(" on this pane ({} works on other panes)",self.binding_label(&b))),
                None => self.set_key_hint("no binding for ", &format!("{} {name}",self.prefix()), ""),
            }
        }
    }

    /// Run a key binding on the focused pane (prompting or picking first if it asks to).
    fn run_binding(&mut self, b: &Value) {
        let ctx = self.ctx_leaf(self.focus);
        let at = self.hit.leaves.get(self.focus).map(|(_, o, _)| Position { x: o.x + 1, y: o.y + 1 });
        self.activate_item(Item::from_entry(b), ctx, at);
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
                if let Some(row) = prefix.get(sel) {
                    self.run_binding(&row.binding);
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
            if if self.safe_mode { !self.safe_mode_allow.contains(owner) } else { !on } { return false; }
        }
        let cmd = e["command"].as_str().unwrap_or("");
        cmd.starts_with("client.") || self.commands.get(cmd).map(|c| c["available"] == true).unwrap_or(false)
    }

    /// The binding a gesture runs in a place: the same precedence as keys (see `rank`).
    fn gesture(&self, key: &str, place: &str) -> Option<Value> {
        self.entries
            .iter()
            .filter(|(k, b)| k.starts_with("binding/") && b["key"] == key && b["where"].as_str().unwrap_or("pane") == place && self.visible(b))
            .min_by_key(|(k, b)| rank(k, b))
            .map(|(_, b)| b.clone())
            .filter(|b| b["command"] != "client.none")
    }

    /// Menu items for a place, filtered by `when` against the context, in `order`.
    fn menu_items(&self, place: &str, ctx: &Value) -> Vec<Item> {
        let mut found: Vec<(i64, String, Item)> = self
            .entries
            .iter()
            .filter(|(k, v)| k.starts_with("menu/") && v["where"] == place && self.visible(v))
            .filter(|(_, v)| when_holds(v, ctx))
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
        // Lets `when` select by what the pane shows, e.g. {"resource_kind": "agent"}.
        let resource_kind = resource.and_then(|r| self.resources.get(r)).map(|r| r["kind"].clone()).unwrap_or(Value::Null);
        let path = self.hit.leaf_paths.get(idx).cloned().unwrap_or_default();
        let mut ctx = json!({
            "layout": layout, "layout_version": version, "path": path,
            "view": view, "kind": v["kind"], "resource": resource, "resource_kind": resource_kind, "live": live, "title": self.view_title(view),
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
        ctx["tab_title"] = json!(self.tab_title(&ctx));
        ctx
    }

    fn ctx_tab(&self, path: &[usize], i: usize) -> Value {
        let (layout, version) = self.active_layout_key();
        let mut ctx = json!({"layout": layout, "layout_version": version, "tab_path": path, "tab_index": i});
        ctx["tab_title"] = json!(self.tab_title(&ctx));
        ctx
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
            // Input is empty unless its author explicitly supplies text or a context source.
            let text = item.prefill.as_str().or_else(|| item.prefill["context"].as_str().and_then(|field| ctx[field].as_str())).unwrap_or_default().to_string();
            self.open_over(Overlay::Input { prompt, text, item, ctx });
        } else if item.pick.as_deref() == Some("resource") {
            self.open_over(Overlay::Pick { sel: 0, item, ctx });
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
                self.open_over(Overlay::Menu { items, ctx, sel: 0, at });
            }
            "client.prefix" => self.overlay = Some(Overlay::Prefix { sel: 0 }),
            "client.zoom" => {
                // Toggle: the focused pane alone, or back to the layout.
                if self.zoomed.is_some() {
                    self.unzoom();
                } else if let Some(v) = self.leaves.get(self.focus).cloned() {
                    self.zoomed = Some(v);
                }
            }
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
            "client.focus" => self.focus_to(args["dir"].as_str().unwrap_or("next")),
            "client.tab" => self.tab_to(&args["to"], &ctx),
            "client.safe_mode" => {
                if self.safe_mode { self.open_safe_chooser(); } else { self.invoke("vqro.safe_mode",json!({"on":true})); }
            }
            "client.safe_mode.toggle" => {
                if let Some(name)=args["name"].as_str() { if !self.safe_selected.remove(name) { self.safe_selected.insert(name.to_string()); } }
            }
            "client.safe_mode.apply" => {
                let selected=self.safe_selected.clone(); let names:Vec<_>=self.entries.keys().filter_map(|key|key.strip_prefix("extension/").map(String::from)).collect();
                let(peer,tx)=(self.peer.clone(),self.tx.clone());
                tokio::spawn(async move {
                    for name in names {
                        if let Err(error)=peer.call("command.invoke",json!({"name":"vqro.safe_mode.allow","args":{"on":selected.contains(&name),"name":name}})).await { let _=tx.send(Msg::Info(error.to_string())); return; }
                    }
                    let _=tx.send(Msg::Info("Safe-mode choices applied; enabled declarations were not changed".into()));
                });
            }
            "client.safe_mode.leave" => self.invoke("vqro.safe_mode",json!({"on":false})),
            "client.quit" => self.quit = true,
            "client.none" => {}
            // Never sent to the core: a client that doesn't know one says so.
            other if other.starts_with("client.") => self.message = format!("{other}: not supported by this client"),
            _ => {
                let args = with_arg(&args, "context", ctx);
                self.invoke(name, args);
            }
        }
    }

    /// Leave zoom, keeping focus on the pane that was zoomed.
    fn unzoom(&mut self) {
        if let Some(z) = self.zoomed.take() {
            self.pending_focus = Some(z);
        }
    }

    /// `client.focus {dir}`: next/prev cycle panes; left/right/up/down go to the adjacent pane.
    fn focus_to(&mut self, dir: &str) {
        self.unzoom();
        let n = self.leaves.len().max(1);
        self.focus = match dir {
            "next" => (self.focus + 1) % n,
            "prev" => (self.focus + n - 1) % n,
            "left" | "right" | "up" | "down" => {
                let rects: Vec<Rect> = self.hit.leaves.iter().map(|(_, r, _)| *r).collect();
                match neighbour(&rects, self.focus, dir) {
                    Some(i) => i,
                    None => return,
                }
            }
            other => return self.message = format!("client.focus: unknown dir {other}"),
        };
    }

    /// `client.tab {to: next|prev|n}` (n counts from 1) in the tab group holding the focused pane,
    /// or the first tab bar.
    fn tab_to(&mut self, to: &Value, ctx: &Value) {
        let path = match ctx["tab_path"].as_array() {
            Some(p) => p.iter().filter_map(|x| x.as_u64().map(|n| n as usize)).collect(),
            None => match self.hit.tabs.first() {
                Some((p, _, _)) => p.clone(),
                None => return,
            },
        };
        let Some((_, mut root)) = self.active_layout() else { return };
        let Some(group) = node_at_mut(&mut root, &path) else { return };
        let n = group["tabs"].as_array().map(|t| t.len()).unwrap_or(0);
        if n == 0 {
            return;
        }
        let active = group["active"].as_u64().unwrap_or(0) as usize;
        let i = match (to.as_str(), to.as_u64()) {
            (Some("next"), _) => (active + 1) % n,
            (Some("prev"), _) => (active + n - 1) % n,
            (_, Some(k)) if k >= 1 => k as usize - 1,
            _ => return self.message = format!("client.tab: unknown target {to}"),
        };
        self.select_tab(&path, i);
    }

    /// Open a menu, prompt or picker. From inside a popup, the popup waits underneath and returns.
    fn open_over(&mut self, ov: Overlay) {
        if matches!(self.overlay, Some(Overlay::Popup { .. })) {
            self.under = self.overlay.take();
        }
        self.overlay = Some(ov);
    }

    /// Bring back a popup once whatever was opened over it has closed.
    fn restore_under(&mut self) -> bool {
        if self.overlay.is_none() && self.under.is_some() {
            self.overlay = self.under.take();
            return true;
        }
        false
    }

    /// Nerd Font icons: the `icons` setting (`nerd`, `text`, or `auto`). Fonts can't be detected from
    /// a terminal, so `auto` trusts terminals that bundle the Nerd Font symbols (Ghostty, WezTerm), or
    /// `VQRO_NERD_FONT=1`.
    fn nerd_icons(&self) -> bool {
        match self.entries.get("doc/settings/tui").and_then(|s| s["icons"].as_str()) {
            Some("nerd") => true,
            Some("text") => false,
            _ => {
                std::env::var("VQRO_NERD_FONT").map(|v| v == "1").unwrap_or(false)
                    || matches!(std::env::var("TERM_PROGRAM").as_deref(), Ok("ghostty") | Ok("WezTerm"))
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
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    if let Some(Overlay::Popup{view,ctx,..})=self.overlay.clone_popup(){self.panel_scroll("@popup",&view,pos,m.kind==MouseEventKind::ScrollUp,ctx);}
                },
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
                    // Programs that asked for the mouse get it first; Ctrl/Alt makes it vqro's.
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
                            let bytes = keys::encode(&key, t.parser.screen().application_cursor(), keys::Extended::Legacy).unwrap_or_default().repeat(3);
                            if let Some(sid) = t.stream {
                                self.send_input(sid, bytes);
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
                        self.panel_scroll(&view,&view,pos,up,ctx);
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

    fn panel_scroll(&mut self,key:&str,view:&str,pos:Position,up:bool,ctx:Value) {
        let detail=self.hit.panels.get(key).and_then(|h|h.detail_area).is_some_and(|r|r.contains(pos));
        if detail {let root=self.pstate.entry(key.into()).or_default();root.detail_focus=true;let st=root.detail.get_or_insert_with(Box::default);st.manual_scroll=true;st.top=if up{st.top.saturating_sub(3)}else{st.top.saturating_add(3)};}
        else {self.pstate.entry(key.into()).or_default().detail_focus=false;self.panel_key(key,view,KeyEvent::new(if up{KeyCode::Up}else{KeyCode::Down},KeyModifiers::NONE),ctx);}
    }

    /// A click in a panel: page tabs, expanders, rows (left activates, right opens the row's menu).
    /// Returns whether it hit something.
    fn panel_click(&mut self, key: &str, view: &str, pos: Position, button: MouseButton, ctx: Value) -> bool {
        let Some(root_hits) = self.hit.panels.get(key) else { return false };
        let in_detail=root_hits.detail_area.is_some_and(|r|r.contains(pos));
        let hits=if in_detail {let Some(h)=root_hits.detail.as_deref()else{return false};h}else{root_hits};
        if let Some((_, page)) = hits.pages.iter().find(|(r, _)| r.contains(pos)).copied() {
            let st = self.pstate.entry(key.to_string()).or_default();
            st.page = page;
            st.sel = 0;
            st.top = 0;
            st.detail_focus=false;
            return true;
        }
        let toggle=hits.toggles.iter().any(|(r,_)|r.contains(pos));
        if let Some((_, row, chip)) = hits.chips.iter().find(|(r, _, _)| r.contains(pos)).copied() {
            let root = self.pstate.entry(key.to_string()).or_default();root.detail_focus=in_detail;
            let st=if in_detail{root.detail.get_or_insert_with(Box::default).as_mut()}else{root};
            st.sel = row;
            st.chip = chip;
            if button == MouseButton::Left {
                self.panel_key(key, view, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), ctx);
            }
            return true;
        }
        let expander = hits.expanders.iter().find(|(r, _)| r.contains(pos)).map(|x| x.1);
        let Some(i) = expander.or_else(|| hits.rows.iter().find(|(r, _)| r.contains(pos)).map(|x| x.1)) else { return false };
        let value=self.panel_value(view);
        let mut container=self.pstate.get(key).cloned().unwrap_or_default();container.detail_focus=in_detail;
        let (value,mut st)=if in_detail {let Some((id,v))=panel::selected_detail(&value,&container)else{return false};panel::sync_detail(&mut container,&id);(Arc::new(v),*container.detail.take().unwrap_or_default())}else{(value,container.clone())};
        let rows=panel::rows(&value,&st);
        let save=|mut root:panel::State,state:panel::State|{if in_detail{root.detail=Some(Box::new(state));root}else{state}};
        st.sel = i;st.manual_scroll=false;
        if expander.is_some() {
            if let Some(r) = rows.get(i) {
                st.open.insert(r.id.clone(), !r.expanded);
            }
            self.pstate.insert(key.to_string(), save(container,st));
            return true;
        }
        self.pstate.insert(key.to_string(), save(container,st));
        if !in_detail && !toggle && button==MouseButton::Left && rows.get(i).is_some_and(|r|r.item["detail_panel"].is_object()){return true;}
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
            self.send_input(sid, bytes);
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
            match peer.call("command.invoke", json!({"name": name, "args": args})).await {
                Ok(r) => {
                    // A reply may ask this client for UI: `{"client": {"popup": {"view", "width"?}}}` or `{"client": {"close": true}}`.
                    if let Some(view) = r["client"]["popup"]["view"].as_str() {
                        let width = r["client"]["popup"]["width"].as_u64().unwrap_or(60) as u16;
                        let _ = tx.send(Msg::Popup { view: view.to_string(), width, ctx });
                    }
                    if r["client"]["close"] == true {
                        let _ = tx.send(Msg::ClosePopup);
                    }
                    if let Some(view) = r["client"]["focus"]["view"].as_str() {
                        let _ = tx.send(Msg::FocusView(view.to_string()));
                    }
                    let _ = tx.send(Msg::Done(name));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Info(format!("{name}: {e}")));
                }
            }
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
        // Resolved every frame, so a theme or icon set change shows on the next redraw. Safe mode uses
        // the built-in palette and no icon set, so neither can lock anyone out.
        let pal = if self.safe_mode { Palette::default() } else { Palette::from_theme(self.entries.get(theme::ENTRY)) };
        let icons = Icons::new(if self.safe_mode { None } else { self.entries.get(icons::ENTRY) }, self.nerd_icons());
        let [main, status] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());
        let mut hit = Hit::default();
        match self.active_layout() {
            Some((_, root)) => {
                let tabbar = self.menu_items("tabbar", &json!({}));
                match self.zoomed.clone() {
                    None => walk(&root, main, &mut Vec::new(), &self.overrides, &tabbar, &mut hit, f.buffer_mut(), &pal, &icons),
                    Some(z) => {
                        // Lay out normally off-screen to find the pane, then give it the whole area.
                        let mut scratch = Buffer::empty(main);
                        walk(&root, main, &mut Vec::new(), &self.overrides, &tabbar, &mut hit, &mut scratch, &pal, &icons);
                        match hit.leaves.iter().position(|(v, _, _)| *v == z) {
                            Some(i) => {
                                let path = hit.leaf_paths[i].clone();
                                hit = Hit::default();
                                hit.leaves.push((z, main, Block::bordered().inner(main)));
                                hit.leaf_paths.push(path);
                                self.focus = 0;
                            }
                            None => {
                                // The zoomed pane is gone (closed, other tab): stop zooming.
                                self.zoomed = None;
                                hit = Hit::default();
                                walk(&root, main, &mut Vec::new(), &self.overrides, &tabbar, &mut hit, f.buffer_mut(), &pal, &icons);
                            }
                        }
                    }
                }
            }
            None => {
                let text = format!("No layout yet.\n\nClick vqro (bottom left) or press {} for the menu:\n  n  open a shell\n  R  one tab per terminal\n  :  all commands", self.key_labels(&self.prefix()));
                f.render_widget(Paragraph::new(text).style(pal.style(None)).block(Block::bordered().title(" vqro ").border_style(pal.border(true))), main);
            }
        }
        self.leaves = hit.leaves.iter().map(|(v, _, _)| v.clone()).collect();
        if let Some(v) = self.pending_focus.take() {
            if let Some(i) = self.leaves.iter().position(|l| *l == v) {
                self.focus = i;
            }
        }
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
            if let Some(c) = self.draw_view(f, view, *rect, i == self.focus, &pal, &icons) {
                cursor = Some(c);
            }
        }
        self.draw_status(f, status, &pal, &icons);
        self.draw_overlay(f, main, &pal, &icons);
        self.draw_toast(f, main, &pal);
        if let (Some(c), None) = (cursor, &self.overlay) {
            f.set_cursor_position(c);
        }
    }

    fn draw_view(&mut self, f: &mut Frame, view: &str, rect: Rect, focused: bool, pal: &Palette, icons: &Icons) -> Option<(u16, u16)> {
        let border = pal.border(focused);
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
                        let fin = if r["available"] == false { " (unavailable)" } else if r["state"] == "finished" { " (finished)" } else { "" };
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
                let available = res.as_ref().is_some_and(|r| r["available"] != false);
                let live = res.as_ref().map(|r| r["state"] == "live").unwrap_or(false) && available;
                let dragging = self.drag.is_some();
                let t = self.terms.entry(view.to_string()).or_insert_with(|| Term {
                    resource: rid.clone(),
                    stream: None,
                    parser: new_parser(inner.width, inner.height),
                    size: (inner.width, inner.height),
                    attaching: false,
                    closed: None,
                    retry_at: None, modes: Default::default(),
                });
                if t.resource != rid {
                    if let Some(sid) = t.stream.take() {
                        self.stream_view.remove(&sid);
                        self.peer.notify("stream.close", json!({"stream": sid}));
                    }
                    *t = Term { resource: rid.clone(), stream: None, parser: new_parser(inner.width, inner.height), size: (inner.width, inner.height), attaching: false, closed: None, retry_at: None, modes: Default::default() };
                }
                if !available && (t.stream.is_some() || t.attaching) {
                    if let Some(stream) = t.stream.take() {
                        self.stream_view.remove(&stream);
                        self.peer.notify("stream.close", json!({"stream": stream}));
                    }
                    t.close("resource unavailable".into());
                }
                // While a border is being dragged, programs keep their size; they resize once on release.
                if t.size != (inner.width, inner.height) && !dragging {
                    t.size = (inner.width, inner.height);
                    t.parser.screen_mut().set_size(nz(inner.height), nz(inner.width));
                    if let Some(sid) = t.stream {
                        self.peer.notify("stream.resize", json!({"stream": sid, "cols": inner.width, "rows": inner.height}));
                    }
                }
                if self.connected && t.can_attach(live, std::time::Instant::now()) {
                    t.begin_attach();
                    let (peer, tx, view, rid) = (self.peer.clone(), self.tx.clone(), view.to_string(), rid.clone());
                    let (cols, rows) = (inner.width, inner.height);
                    let connection = self.connection;
                    tokio::spawn(async move {
                        let result = peer
                            .call("resource.attach", json!({"resource": rid, "cols": cols, "rows": rows, "history": 2000}))
                            .await
                            .map(|r| r["stream"].as_u64().unwrap_or(0))
                            .map_err(|e| e.to_string());
                        let _ = tx.send(Msg::Attached { connection, view, resource: rid, result });
                    });
                }
                render_screen(f.buffer_mut(), inner, t.parser.screen());
                if let Some(reason) = &t.closed {
                    let line = Line::from(Span::styled(format!(" stream closed: {reason} "), pal.style(Some("warn"))));
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
                let hits = panel::render(f.buffer_mut(), inner, &v, &mut st, focused, pal, icons, self.entries.get("doc/settings/tui").and_then(|s|s["key_labels"].as_str()));
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

    fn draw_status(&mut self, f: &mut Frame, area: Rect, pal: &Palette, icons: &Icons) {
        // Keyboard modes turn the bar into an input line, like a multiplexer's prompt.
        let filtering = self.filtering.as_ref().map(|k| self.pstate.get(k).map(|s| s.filter.clone()).unwrap_or_default());
        let prompt = match &self.overlay {
            _ if filtering.is_some() => Some(("/".to_string(), filtering.clone().unwrap_or_default())),
            Some(Overlay::Prefix { .. }) => Some((self.key_labels(&self.prefix()), self.key_labels(&keys::held_words(self.held)))),
            Some(Overlay::Palette { query, .. }) => Some((":".to_string(), query.clone())),
            Some(Overlay::Input { prompt, text, .. }) => Some((format!("{prompt}:"), text.clone())),
            _ => None,
        };
        let open = prompt.is_some();
        let badge = if open { " vqro ▴ " } else { " vqro ▾ " };
        self.hit.badge = Rect { x: area.x, y: area.y, width: badge.chars().count() as u16, height: 1 };
        self.hit.status = area;
        let mut spans = vec![Span::styled(badge, Style::new().bg(if open { pal.prompt } else { pal.accent }).fg(pal.badge_text))];
        if self.snapshot_retries > 0 { spans.push(Span::styled(" core busy ", pal.badge(Some("warn")))); }
        else if self.recovery_only { spans.push(Span::styled(" recovery only ", pal.badge(Some("warn")))); }
        if !self.connected {
            spans.push(Span::styled(" reconnecting ", pal.badge(Some("warn"))));
        }
        if let Some((label, text)) = prompt {
            spans.push(Span::styled(format!(" {label} "), Style::new().fg(pal.prompt).add_modifier(Modifier::BOLD)));
            // Held modifiers after the prefix are a hint, not input: greyed out.
            let text_style = if matches!(self.overlay, Some(Overlay::Prefix { .. })) && self.filtering.is_none() { pal.style(Some("dim")) } else { pal.style(None) };
            spans.push(Span::styled(text, text_style));
            let x = area.x + spans.iter().map(|s| s.content.chars().count() as u16).sum::<u16>();
            f.render_widget(Paragraph::new(Line::from(spans)), area);
            f.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
            return;
        }
        if self.safe_mode {
            spans.push(Span::styled(" SAFE MODE ", pal.badge(Some("warn"))));
        }
        if self.zoomed.is_some() {
            spans.push(Span::styled(" ZOOM ", pal.badge(Some("accent"))));
        }
        let failed = self.extensions.values().filter(|e| e["status"] == "failed").count();
        if failed > 0 {
            let label=format!(" {failed} extension(s) failed · recover ");
            let x=area.x+spans.iter().map(|s|s.content.chars().count() as u16).sum::<u16>();
            let width=(label.chars().count() as u16).min(area.right().saturating_sub(x));
            if width>0 {self.hit.buttons.push((Rect{x,y:area.y,width,height:1},Item::from_entry(&json!({"label":"Recover failed extensions","command":"client.prefix"})),json!({})));}
            spans.push(Span::styled(label, pal.badge(Some("error"))));
        }
        if !self.safe_mode {
            if let Some(notice) = &self.backend_notice {
                spans.push(Span::styled(format!(" {notice} "), pal.style(Some("warn"))));
            }
        }
        // Contributed status-bar buttons.
        let mut x = area.x + spans.iter().map(|s| s.content.chars().count() as u16).sum::<u16>();
        for item in self.menu_items("status", &json!({})) {
            // Live segments take their label, icon and style from a view's live content. An empty
            // label hides a live segment (nothing to report), whatever its icon.
            let (text, style) = match item.view.as_deref().and_then(|v| self.live.get(v)) {
                Some(l) => match l["label"].as_str().unwrap_or(&item.label) {
                    "" => continue,
                    t => (icons.label(if l["icon"].is_null() { &item.icon } else { &l["icon"] }, t), pal.style(Some(l["style"].as_str().unwrap_or("accent")))),
                },
                None if item.view.is_some() => continue,
                None => (icons.label(&item.icon, &item.label), pal.style(Some("accent"))),
            };
            if text.is_empty() {
                continue;
            }
            let label = format!(" {text} ");
            let w = icons::width(&label);
            if x + w > area.right() {
                break;
            }
            self.hit.buttons.push((Rect { x, y: area.y, width: w, height: 1 }, item, json!({})));
            x += w;
            spans.push(Span::styled(label, style));
        }
        if !self.safe_mode && self.backend_notice.is_none() && self.backends_connected.is_empty() {
            spans.push(Span::styled(" No backend connected; prefix ! opens recovery choices.",pal.style(Some("warn"))));
        }
        spans.push(Span::styled(format!(" {}", self.message_label()), pal.style(Some("muted"))));
        f.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn draw_toast(&mut self, f: &mut Frame, area: Rect, pal: &Palette) {
        let Some((text, style, until)) = &self.toast else { return };
        if std::time::Instant::now() > *until {
            self.toast = None;
            return;
        }
        let w = (text.chars().count() as u16 + 4).min(area.width);
        let rect = Rect { x: area.right().saturating_sub(w + 1), y: area.y + 1, width: w, height: 3 };
        let edge = match style.as_deref() {
            Some(s @ ("error" | "warn" | "ok")) => pal.style(Some(s)),
            _ => pal.style(Some("accent")),
        };
        f.render_widget(Clear, rect);
        let block = Block::bordered().border_type(ratatui::widgets::BorderType::Rounded).border_style(edge);
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        f.render_widget(Paragraph::new(text.as_str()).style(pal.style(None)), inner);
    }

    fn draw_overlay(&mut self, f: &mut Frame, area: Rect, pal: &Palette, icons: &Icons) {
        if let Some(Overlay::Popup { view, width, .. }) = &self.overlay {
            let (view, width) = (view.clone(), *width);
            self.draw_popup(f, area, pal, icons, &view, width);
            return;
        }
        // A menu, prompt or picker opened from a popup is drawn over it: the popup stays visible
        // underneath (it waits in `under` and takes input again when the menu closes).
        if let Some(Overlay::Popup { view, width, .. }) = &self.under {
            let (view, width) = (view.clone(), *width);
            self.draw_popup(f, area, pal, icons, &view, width);
        }
        self.draw_menu_overlay(f, area, pal, icons);
    }

    fn draw_popup(&mut self, f: &mut Frame, area: Rect, pal: &Palette, icons: &Icons, view: &str, width: u16) {
        {
            let view = view.to_string();
            let v = self.panel_value(&view);
            let mut st = self.pstate.remove("@popup").unwrap_or_default();
            let w = width.max(20).min(area.width);
            // Size to the largest page so switching pages doesn't jump.
            let tallest = (0..panel::page_titles(&v).len().max(1))
                .map(|p| panel::height(&v, &panel::State { page: p, filter: st.filter.clone(), open: st.open.clone(), ..Default::default() }, w.saturating_sub(4), icons))
                .max()
                .unwrap_or(1);
            let h = (tallest + 4 + (!st.filter.is_empty()) as u16).min(area.height);
            let rect = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height.saturating_sub(h)) / 3, width: w, height: h };
            f.render_widget(Clear, rect);
            let title = v["title"].as_str().unwrap_or(&view).to_string();
            let block = Block::bordered()
                .border_type(ratatui::widgets::BorderType::Rounded)
                .title(Span::styled(format!(" {title} "), pal.style(Some("accent")).add_modifier(Modifier::BOLD)))
                .title_bottom(Line::from(Span::styled(
                    self.key_labels(if panel::list_detail(&v,&st) {" ↑↓ list · Tab details · [ ] pages · / search · Esc close "}else if panel::page_titles(&v).is_empty() { " ↑↓ move · Enter select · / filter · Esc close " } else { " ↑↓ move · Tab page · Enter select · / filter · Esc close " }),
                    pal.style(Some("dim")),
                )).right_aligned())
                .border_style(pal.border(true));
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            let body = Rect { x: inner.x + 1, y: inner.y + 1, width: inner.width.saturating_sub(2), height: inner.height.saturating_sub(1) };
            let hits = panel::render(f.buffer_mut(), body, &v, &mut st, true, pal, icons, self.entries.get("doc/settings/tui").and_then(|s|s["key_labels"].as_str()));
            self.pstate.insert("@popup".into(), st);
            self.hit.panels.insert("@popup".into(), hits);
            self.hit.overlay = Some((rect, Rect::default(), 0));
        }
    }

    /// Menus, prompts, pickers, the prefix menu and the palette (everything but popups).
    fn draw_menu_overlay(&mut self, f: &mut Frame, area: Rect, pal: &Palette, icons: &Icons) {
        let (title, items, sel, query) = match &self.overlay {
            Some(Overlay::Popup { .. }) => return,
            None => {
                self.hit.overlay = None;
                return;
            }
            Some(Overlay::Prefix { sel }) => {
                // Recovery actions that lost their key show `·`: click or Enter runs them.
                let rows = self.prefix_items();
                let w = rows.iter().filter_map(|r| r.key.as_ref()).map(|k| k.chars().count()).max().unwrap_or(1).max(2);
                let items = rows.into_iter().map(|r| format!(" {:>w$}  {}", r.key.unwrap_or_else(|| "·".into()), r.label)).collect();
                (" vqro (press a key or click) ", items, *sel, None::<String>)
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
            Some(Overlay::Menu { items, sel, .. }) => (" ", menu_lines(items, icons), *sel, None),
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
            let w = (items.iter().map(|i| icons::width(i)).max().unwrap_or(10) + 4).min(area.width);
            let x = at.x.min(area.right().saturating_sub(w));
            let y = at.y.min(area.bottom().saturating_sub(h));
            Rect { x, y, width: w, height: h }
        } else {
            Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 3, width: w, height: h }
        };
        f.render_widget(Clear, rect);
        let block = Block::bordered().title(title).border_style(pal.border(true));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let list_area = match query {
            Some(q) => {
                f.render_widget(Paragraph::new(format!("> {q}")).style(pal.style(None)), Rect { height: 1, ..inner });
                Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner }
            }
            None => inner,
        };
        let mut state = ListState::default();
        state.select(Some(sel));
        let list = List::new(items.into_iter().map(ListItem::new).collect::<Vec<_>>()).style(pal.style(None)).highlight_style(pal.selected());
        f.render_stateful_widget(list, list_area, &mut state);
        self.hit.overlay = Some((rect, list_area, state.offset()));
    }
}

/// Ids of the client's default bindings, which win ties (see `rank`).
const BUILTIN: &str = "builtin:";

/// The client's default prefix keys, as bindings. `recovery` ones stay in the prefix menu even
/// when a binding takes their key; `hidden` ones work but stay out of the menu.
fn builtin_bindings() -> Vec<Value> {
    let mut b = vec![
        json!({"key": ":", "command": "client.palette", "label": "all commands", "recovery": true}),
        json!({"key": "o", "command": "client.focus", "args": {"dir": "next"}, "label": "next pane"}),
        json!({"key": "O", "command": "client.focus", "args": {"dir": "prev"}, "label": "previous pane", "hidden": true}),
        json!({"key": "u", "command": "vqro.revert", "label": "undo last change", "recovery": true}),
        json!({"key": "R", "command": "vqro.layout.reset", "label": "reset layout", "recovery": true}),
        json!({"key": "!", "command": "client.safe_mode", "label": "safe mode / recovery choices", "recovery": true}),
        json!({"key": "q", "command": "client.quit", "label": "quit this UI", "recovery": true}),
    ];
    b.extend((1..=9).map(|n| json!({"key": n.to_string(), "command": "client.tab", "args": {"to": n}, "label": format!("tab {n}"), "hidden": true})));
    b
}

/// Whether an entry's `when` holds in a context: every field equal. No `when` always holds.
fn when_holds(v: &Value, ctx: &Value) -> bool {
    v["when"].as_object().map(|w| w.iter().all(|(k, want)| ctx[k] == *want)).unwrap_or(true)
}

/// Which of several bindings for one key wins (smallest rank): the highest `priority` (default 0),
/// then the client's defaults, then the lowest id, so the outcome never depends on load order.
fn rank(id: &str, b: &Value) -> (std::cmp::Reverse<i64>, bool, String) {
    (std::cmp::Reverse(b["priority"].as_i64().unwrap_or(0)), !id.starts_with(BUILTIN), id.to_string())
}

/// For each key (canonical name), the index of the binding that wins it. Keys that don't parse
/// are ignored, so a binding from a newer protocol can't break the keymap.
fn resolve(cands: &[(String, Value)]) -> HashMap<String, usize> {
    let mut won: HashMap<String, usize> = HashMap::new();
    for (i, (id, b)) in cands.iter().enumerate() {
        let Some(key) = b["key"].as_str().and_then(keys::normalize) else { continue };
        let better = won.get(&key).map(|&j| rank(id, b) < rank(&cands[j].0, &cands[j].1)).unwrap_or(true);
        if better {
            won.insert(key, i);
        }
    }
    won
}

/// The pane next to `from` in a direction: among panes on that side that overlap it across the
/// direction, the nearest, then the best aligned.
fn neighbour(rects: &[Rect], from: usize, dir: &str) -> Option<usize> {
    let a = *rects.get(from)?;
    let span = |lo: u16, hi: u16, blo: u16, bhi: u16| blo < hi && bhi > lo;
    let mid = |lo: u16, len: u16| 2 * lo as i32 + len as i32;
    rects
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != from)
        .filter_map(|(i, b)| {
            let (gap, offset) = match dir {
                "left" if b.right() <= a.x && span(a.y, a.bottom(), b.y, b.bottom()) => (a.x - b.right(), mid(a.y, a.height) - mid(b.y, b.height)),
                "right" if b.x >= a.right() && span(a.y, a.bottom(), b.y, b.bottom()) => (b.x - a.right(), mid(a.y, a.height) - mid(b.y, b.height)),
                "up" if b.bottom() <= a.y && span(a.x, a.right(), b.x, b.right()) => (a.y - b.bottom(), mid(a.x, a.width) - mid(b.x, b.width)),
                "down" if b.y >= a.bottom() && span(a.x, a.right(), b.x, b.right()) => (b.y - a.bottom(), mid(a.x, a.width) - mid(b.x, b.width)),
                _ => return None,
            };
            Some(((gap, offset.abs()), i))
        })
        .min()
        .map(|(_, i)| i)
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
#[allow(clippy::too_many_arguments)]
fn walk(node: &Value, area: Rect, path: &mut Vec<usize>, overrides: &HashMap<Vec<usize>, Vec<u16>>, tabbar: &[Item], hit: &mut Hit, buf: &mut Buffer, pal: &Palette, icons: &Icons) {
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
            walk(c, *r, path, overrides, tabbar, hit, buf, pal, icons);
            path.pop();
        }
    } else if let Some(tabs) = node["tabs"].as_array() {
        let active = (node["active"].as_u64().unwrap_or(0) as usize).min(tabs.len().saturating_sub(1));
        let [bar, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        let mut spans = Vec::new();
        let mut x = bar.x;
        for (i, t) in tabs.iter().enumerate() {
            let label = format!(" {}:{} ", i + 1, t["title"].as_str().unwrap_or("tab"));
            let w = icons::width(&label);
            hit.tabs.push((path.clone(), i, Rect { x, y: bar.y, width: w.min(bar.right().saturating_sub(x)), height: 1 }));
            x = x.saturating_add(w);
            let style = if i == active { Style::new().bg(pal.tab_active).fg(pal.tab_active_text) } else { pal.style(Some("muted")) };
            spans.push(Span::styled(label, style));
        }
        // Contributed tab-bar buttons (e.g. `+` from the tabs extension), on the first tab bar.
        if !hit.buttons.iter().any(|(_, _, c)| c.get("tab_path").is_some()) {
            for item in tabbar {
                let label = format!(" {} ", icons.label(&item.icon, &item.label));
                let w = icons::width(&label);
                if x + w > bar.right() {
                    break;
                }
                hit.buttons.push((Rect { x, y: bar.y, width: w, height: 1 }, item.clone(), json!({"tab_path": path.clone()})));
                x += w;
                spans.push(Span::styled(label, pal.style(Some("accent"))));
            }
        }
        // The whole row gets the tab-bar band, not just the labels.
        ratatui::widgets::Widget::render(Paragraph::new(Line::from(spans)).style(Style::new().bg(pal.tab_bar)), bar, buf);
        if let Some(t) = tabs.get(active) {
            path.push(active);
            walk(&t["node"], body, path, overrides, tabbar, hit, buf, pal, icons);
            path.pop();
        }
    }
}

/// A menu's lines. When any item has an icon, every label starts after one shared icon column.
fn menu_lines(items: &[Item], icons: &Icons) -> Vec<String> {
    let glyphs: Vec<Option<&str>> = items.iter().map(|i| icons.resolve(&i.icon)).collect();
    let column = glyphs.iter().flatten().map(|g| icons.cell(g).1).max();
    items
        .iter()
        .zip(glyphs)
        .map(|(i, g)| match (column, g) {
            (None, _) => format!(" {}", i.label),
            (Some(c), g) => {
                let (cell, cells) = g.map(|g| icons.cell(g)).unwrap_or_default();
                format!(" {cell}{} {}", " ".repeat((c - cells) as usize), i.label)
            }
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> (App, mpsc::UnboundedReceiver<Msg>) {
        let (peer, _) = Peer::spawn(tokio::io::empty(), tokio::io::sink());
        let (tx, rx) = mpsc::unbounded_channel();
        (App::new(peer, tx), rx)
    }

    #[tokio::test]
    async fn disconnected_command_events_drop_cached_names() {
        let(mut app,_)=app();let epoch=app.epoch.clone();
        app.on_event_record(&json!({"epoch":epoch,"seq":1,"type":"command","data":{"name":"temporary","available":true}}));
        assert!(app.commands.contains_key("temporary"));
        app.on_event_record(&json!({"epoch":epoch,"seq":2,"type":"command","data":{"name":"temporary","available":false}}));
        assert!(!app.commands.contains_key("temporary"));
    }

    #[tokio::test]
    async fn text_inputs_require_explicit_prefill_and_preserve_rename_context() {
        let (mut app, _) = app();
        app.entries.insert("workspace/active".into(), json!({"layout": "main"}));
        app.entries.insert("layout/main".into(), json!({"tabs": [{"title": "Named tab", "node": {"view": "form"}}], "active": 0}));
        app.entries.insert("view/form".into(), json!({"kind": "panel", "title": "Agent r8", "items": []}));
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let pane = app.ctx_leaf(0);
        for (entry, expected) in [
            (json!({"command": "any.prompt", "input": "Prompt"}), ""),
            (json!({"command": "any.prompt", "input": "Prompt", "prefill": "explicit text"}), "explicit text"),
            (json!({"command": "any.prompt", "input": "Prompt", "prefill": ""}), ""),
            (json!({"command": "any.rename", "input": "Name", "prefill": {"context": "title"}}), "Agent r8"),
            (json!({"command": "any.prompt", "input": "Prompt", "prefill": {"context": "missing"}}), ""),
        ] {
            app.activate_item(Item::from_entry(&entry), pane.clone(), None);
            assert!(matches!(&app.overlay, Some(Overlay::Input { text, .. }) if text == expected));
        }
        let tab = app.ctx_tab(&[], 0);
        assert_eq!(tab["tab_title"], "Named tab");
        assert_eq!(pane["tab_title"], "Named tab");
        app.activate_item(Item::from_entry(&json!({"command": "any.rename", "input": "Name", "prefill": {"context": "tab_title"}})), tab, None);
        assert!(matches!(&app.overlay, Some(Overlay::Input { text, .. }) if text == "Named tab"));
        app.entries.insert("view/row-form".into(), json!({"kind": "panel", "items": [{"text": "Edit", "command": "any.edit", "input": "Value", "prefill": "row initial value"}]}));
        app.panel_key("row-form", "row-form", KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), pane.clone());
        assert!(matches!(&app.overlay, Some(Overlay::Input { text, .. }) if text == "row initial value"));
        app.options_menu(&json!({"command": "any.option", "value": "implicit value", "options": [{"label": "Custom", "input": "Value"}]}), pane.clone(), None);
        app.activate_overlay();
        assert!(matches!(&app.overlay, Some(Overlay::Input { text, .. }) if text.is_empty()));
        app.options_menu(&json!({"command": "any.option", "value": "implicit value", "options": [{"label": "Custom", "input": "Value", "prefill": "explicit option"}]}), pane, None);
        app.activate_overlay();
        assert!(matches!(&app.overlay, Some(Overlay::Input { text, .. }) if text == "explicit option"));
    }

    #[tokio::test]
    async fn backend_notice_is_drawn_even_with_an_alternative_backend_and_tracks_events() {
        let (mut app, _) = app();
        app.load(&json!({"epoch":"boot","seq":1,"entries":{},"providers":{
            "alternative":{"role":"backend","connected":true}},"backend_notice":"Configured backend is unreachable"}));
        let mut terminal=ratatui::Terminal::new(ratatui::backend::TestBackend::new(100,24)).unwrap();
        terminal.draw(|f|app.draw(f)).unwrap();
        let screen:String=terminal.backend().buffer().content.iter().map(|c|c.symbol()).collect();
        assert!(screen.contains("Configured backend is unreachable"),"{screen}");
        app.on_event_record(&json!({"epoch":"boot","seq":2,"type":"provider","data":{
            "name":"configured","state":{"role":"backend","connected":true},"backend_notice":null}}));
        terminal.draw(|f|app.draw(f)).unwrap();
        let screen:String=terminal.backend().buffer().content.iter().map(|c|c.symbol()).collect();
        assert!(!screen.contains("Configured backend is unreachable"));
        app.on_event_record(&json!({"epoch":"boot","seq":3,"type":"entries","data":{
            "changes":[],"backend_notice":"New default is unavailable"}}));
        terminal.draw(|f|app.draw(f)).unwrap();
        let screen:String=terminal.backend().buffer().content.iter().map(|c|c.symbol()).collect();
        assert!(screen.contains("New default is unavailable"),"{screen}");
    }

    #[tokio::test]
    async fn key_labels_default_full_short_and_unknown_work_without_extensions() {
        let (mut app,_)=app();
        app.safe_mode=true;
        app.entries.insert("binding/modified".into(),json!({"key":"C-S-Left","command":"client.focus","label":"modified"}));
        app.on_key(KeyEvent::new(KeyCode::Char('g'),KeyModifiers::CONTROL));
        let mut terminal=ratatui::Terminal::new(ratatui::backend::TestBackend::new(100,24)).unwrap();
        for (setting,prefix,modified) in [(None,"Ctrl-g","Ctrl-Shift-Left"),(Some("short"),"C-g","C-S-Left"),(Some("unknown"),"Ctrl-g","Ctrl-Shift-Left")] {
            if let Some(setting)=setting {app.entries.insert("doc/settings/tui".into(),json!({"key_labels":setting}));}
            terminal.draw(|f|app.draw(f)).unwrap();
            let screen:String=terminal.backend().buffer().content.iter().map(|c|c.symbol()).collect();
            assert!(screen.contains(prefix),"{screen}");
            assert!(screen.contains(modified),"{screen}");
        }
        assert_eq!(app.prefix(),"C-g","matching/storage remains canonical");
        app.set_key_hint("", "C-g ,", " → settings");
        assert_eq!(app.message_label(),"Ctrl-g , → settings");
        app.entries.insert("doc/settings/tui".into(),json!({"key_labels":"short"}));
        assert_eq!(app.message_label(),"C-g , → settings","cached hints follow the current choice");
        app.entries.insert("doc/settings/tui".into(),json!({"key_labels":"full"}));
        app.message="backend C-g unavailable".into();
        assert_eq!(app.message_label(),"backend C-g unavailable","identifiers in arbitrary messages are not key hints");
    }

    #[tokio::test]
    async fn failed_extensions_have_builtin_restart_recovery_without_settings() {
        let (mut app,_)=app();
        app.load(&json!({"epoch":"boot","seq":1,"entries":{"extension/pi":{"version":1,"value":{"argv":["missing"],"enabled":true}}},
            "extensions":{"pi":{"status":"failed","crashes":0,"crashes_total":3,"last_exit":"signal9",
                "failure":{"kind":"crash_budget","at_ms":1234,"crashes":3,"window_seconds":300}}}}));
        let rows=app.prefix_items();
        let restart=rows.iter().find(|r|r.binding["command"]=="vqro.extension.restart" && r.binding["args"]["name"]=="pi").expect("built-in recovery restart row");
        assert!(restart.label.contains("restart pi"));
        let mut terminal=ratatui::Terminal::new(ratatui::backend::TestBackend::new(100,24)).unwrap();
        terminal.draw(|f|app.draw(f)).unwrap();
        let badge=app.hit.buttons.iter().find(|(_,item,_)|item.command=="client.prefix").expect("failed recovery badge").0;
        app.on_mouse(MouseEvent {kind:MouseEventKind::Down(MouseButton::Left),column:badge.x,row:badge.y,modifiers:KeyModifiers::NONE});
        assert!(matches!(app.overlay,Some(Overlay::Prefix{..})),"failed badge opens built-in recovery without an extension");
        app.safe_mode=true;
        assert!(app.prefix_items().iter().any(|r|r.binding["command"]=="vqro.extension.restart"),"recovery remains reachable; core still controls safe-mode admission");
    }

    #[tokio::test]
    async fn command_focus_targets_a_visible_view_without_kind_policy() {
        let (mut app, _) = app();
        app.entries.insert("workspace/active".into(), json!({"layout": "test"}));
        app.entries.insert("layout/test".into(), json!({"split": "h", "children": [{"view": "list"}, {"view": "detail"}]}));
        for view in ["list", "detail"] {
            app.entries.insert(format!("view/{view}"), json!({"kind": "panel", "items": []}));
        }
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        app.on_msg(Msg::FocusView("detail".into()));
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(app.leaves[app.focus], "detail");
        app.on_msg(Msg::FocusView("missing".into()));
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(app.leaves[app.focus], "detail", "a hidden/missing view is ignored, never opened");
    }

    fn snapshot(seq: u64, value: &str) -> Value {
        json!({"epoch": "boot", "seq": seq, "entries": {"doc/test/value": {"version": seq, "value": value}}})
    }

    fn delta(seq: u64, value: &str) -> Incoming {
        Incoming::Notification { method: "event".into(), params: json!({
            "epoch": "boot", "seq": seq, "type": "entries", "data": {"changes": [
                {"key": "doc/test/value", "version": seq, "value": value}
            ]}
        }) }
    }

    fn resource_event(seq: u64, data: Value, partial: bool) -> Incoming {
        let mut params = json!({"epoch": "boot", "seq": seq, "type": "resource", "data": data});
        if partial {
            params["partial"] = json!(true);
        }
        Incoming::Notification { method: "event".into(), params }
    }

    #[tokio::test]
    async fn generic_details_keep_chip_keys_paging_and_prefix_recovery() {
        let(mut app,_)=app();
        let items=(0..10).map(|i|json!({"id":format!("row{i}"),"text":format!("Row {i}"),"detail_panel":{"items":[],"toolbar":[{"type":"chips","chips":[{"text":"one","command":"client.quit"},{"text":"two"}]}]}})).collect::<Vec<_>>();
        app.entries.insert("view/test".into(),json!({"kind":"panel","list_detail":{"requires":["list_detail"]},"items":items}));
        app.pstate.entry("test".into()).or_default().page_step=3;
        app.panel_key("test","test",KeyEvent::new(KeyCode::PageDown,KeyModifiers::NONE),json!({}));assert_eq!(app.pstate["test"].sel,3);
        app.panel_key("test","test",KeyEvent::new(KeyCode::Tab,KeyModifiers::NONE),json!({}));assert!(app.pstate["test"].detail_focus);
        app.panel_key("test","test",KeyEvent::new(KeyCode::Right,KeyModifiers::NONE),json!({}));assert_eq!(app.pstate["test"].detail.as_ref().unwrap().chip,1);
        app.panel_key("test","test",KeyEvent::new(KeyCode::Left,KeyModifiers::NONE),json!({}));assert_eq!(app.pstate["test"].detail.as_ref().unwrap().chip,0);assert!(app.pstate["test"].detail_focus);
        app.pstate.get_mut("test").unwrap().detail.as_mut().unwrap().manual_scroll=true;
        app.panel_key("test","test",KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE),json!({}));assert!(!app.quit,"off-screen action must be revealed before execution");
        app.panel_key("test","test",KeyEvent::new(KeyCode::BackTab,KeyModifiers::NONE),json!({}));assert!(!app.pstate["test"].detail_focus);
        app.overlay=Some(Overlay::Popup{view:"test".into(),width:100,ctx:json!({})});app.filtering=Some("@popup".into());
        app.on_key(KeyEvent::new(KeyCode::Char('g'),KeyModifiers::CONTROL));assert!(matches!(app.overlay,Some(Overlay::Prefix{..})));assert!(app.filtering.is_none());
    }

    #[tokio::test]
    async fn safe_snapshot_and_reconnect_preserve_unapplied_choices_and_dismissal() {
        let (mut app,_) = app();let snap=json!({"epoch":"one","seq":1,"safe_mode":true,"safe_mode_allow":[],"entries":{"extension/draft":{"value":{"argv":["false"]}}}});
        app.load(&snap);app.safe_selected.insert("draft".into());app.load(&snap);
        assert!(app.safe_selected.contains("draft"));
        app.overlay=None;let mut later=snap.clone();later["epoch"]=json!("two");
        app.reconnected(app.peer.clone(),&later);assert!(app.overlay.is_none(),"reconnect must not steal focus");assert!(app.safe_selected.contains("draft"));
    }

    #[tokio::test]
    async fn safe_chooser_is_builtin_grouped_and_temporary_even_with_no_extensions_running() {
        let (mut app,_) = app();
        app.load(&json!({"epoch":"boot","seq":1,"safe_mode":true,"safe_mode_allow":["ui"],"provider_extensions":["remembered"],"entries":{
            "extension/herdr":{"value":{"kind":"provider","default":true,"description":"Backend connection","enabled":false,"argv":["vqro-herdr"]}},
            "extension/tmux":{"value":{"kind":"provider","description":"Optional tmux backend","enabled":false,"argv":["vqro-tmux"]}},
            "extension/remembered":{"value":{"description":"Registered provider","argv":["false"]}},
            "extension/ui":{"value":{"description":"User interface","enabled":false,"argv":["false"]}}
        }}));
        assert!(matches!(&app.overlay,Some(Overlay::Popup{view,..}) if view=="@vqro-safe-mode"));
        let panel=app.panel_value("@vqro-safe-mode");let rows=panel["items"].as_array().unwrap();
        let at=|text:&str| rows.iter().position(|r|r["text"].as_str()==Some(text)).unwrap();
        assert!(at("Providers / backends")<at("[ ] herdr"));
        assert!(at("[ ] tmux")<at("UI extensions"));
        assert!(at("[ ] remembered")<at("UI extensions"));
        assert!(at("UI extensions")<at("[x] ui"));
        assert!(app.visible(&json!({"owner":"ui","command":"client.none"})),"explicit safe allowance overrides saved disabled without changing it");
        app.run_command("client.safe_mode.toggle",json!({"name":"herdr"}),Value::Null,None);
        assert!(app.safe_selected.contains("herdr"));assert!(!app.safe_mode_allow.contains("herdr"));
        assert_eq!(app.entries["extension/herdr"]["enabled"],false);
        app.on_key(KeyEvent::new(KeyCode::Char('g'),KeyModifiers::CONTROL));
        assert!(matches!(app.overlay,Some(Overlay::Prefix{..})),"prefix recovery remains reachable inside chooser");
        assert!(app.prefix_items().iter().any(|r|r.binding["command"]=="client.safe_mode"));
    }

    #[tokio::test]
    async fn unchanged_panel_values_share_storage_and_all_content_paths_invalidate() {
        let (mut app, _) = app();
        let snap=json!({"epoch":"boot","seq":1,"entries":{"view/p":{"version":1,"value":{"kind":"panel","title":"base","items":[{"text":"base"}]}}},"live":{"p":{"items":[{"text":"live"}]}}});
        app.load(&snap);
        let first=app.panel_value("p"); assert!(Arc::ptr_eq(&first,&app.panel_value("p")));
        app.on_incoming(resource_event(2,json!({"id":"r1","state":"live"}),false));
        assert!(Arc::ptr_eq(&first,&app.panel_value("p")),"unrelated resource/flood updates do not rebuild panels");
        app.on_event_record(&json!({"epoch":"boot","seq":3,"type":"view.content","data":{"view":"p","content":{"items":[{"text":"new live"}]}}}));
        let live=app.panel_value("p"); assert!(!Arc::ptr_eq(&first,&live)); assert_eq!(live["items"][0]["text"],"new live");
        app.on_event_record(&json!({"epoch":"boot","seq":4,"type":"entries","data":{"changes":[{"key":"view/p","version":2,"value":{"kind":"panel","title":"new base","items":[{"text":"new base"}]}}]}}));
        let entry=app.panel_value("p"); assert!(!Arc::ptr_eq(&live,&entry)); assert_eq!(entry["title"],"new base");
        app.on_event_record(&json!({"epoch":"boot","seq":5,"type":"view.content","data":{"view":"p","content":null}}));
        assert_eq!(app.panel_value("p")["items"][0]["text"],"new base");
        let before_reload=app.panel_value("p"); app.load(&snap);
        assert!(!Arc::ptr_eq(&before_reload,&app.panel_value("p")));
        app.on_event_record(&json!({"epoch":"boot","seq":2,"type":"entries","data":{"changes":[{"key":"view/p","value":null}]}}));
        assert!(!app.panel_values.lock().unwrap().contains_key("p"));
    }

    #[tokio::test]
    async fn source_updates_invalidate_composed_panels_but_unrelated_views_do_not() {
        let (mut app, _) = app();
        app.load(&json!({"epoch":"boot","seq":1,"entries":{
            "view/root":{"version":1,"value":{"kind":"panel","items":[{"type":"view","view":"source","readonly":true}]}},
            "view/source":{"version":1,"value":{"kind":"panel","items":[{"text":"fallback"}]}}},
            "live":{"source":{"items":[{"text":"native"}]}}}));
        let first = app.panel_value("root");
        assert_eq!(first["items"][0]["text"],"native");
        app.on_event_record(&json!({"epoch":"boot","seq":2,"type":"view.content","data":{"view":"unrelated","content":{"items":[]}}}));
        assert!(Arc::ptr_eq(&first,&app.panel_value("root")));
        app.on_event_record(&json!({"epoch":"boot","seq":3,"type":"view.content","data":{"view":"source","content":{"items":[{"text":"changed"}]}}}));
        let changed = app.panel_value("root");
        assert!(!Arc::ptr_eq(&first,&changed));
        assert_eq!(changed["items"][0]["text"],"changed");
        app.on_event_record(&json!({"epoch":"boot","seq":4,"type":"view.content","data":{"view":"source","content":null}}));
        assert_eq!(app.panel_value("root")["items"][0]["text"],"fallback");
    }

    #[tokio::test]
    async fn queued_key_beats_both_ready_background_input_and_frame_deadline() {
        let (app, _) = app();
        let (_,mut incoming)=Peer::spawn(tokio::io::empty(),tokio::io::sink());
        let (_,mut messages)=mpsc::unbounded_channel(); let (key_tx,mut keys)=mpsc::unbounded_channel();
        key_tx.send(Event::Key(KeyEvent::new(KeyCode::Char('x'),KeyModifiers::NONE))).unwrap();
        let mut reconnecting=None;
        assert!(matches!(next_input(&app,&mut incoming,&mut messages,&mut keys,&mut reconnecting,Some(std::time::Instant::now())).await.unwrap(),UiInput::Key(_)));
    }

    #[tokio::test]
    async fn partial_resource_events_merge_onto_the_known_record() {
        let (mut app, _) = app();
        app.load(&snapshot(1, "x"));
        app.on_incoming(resource_event(2, json!({"id": "r1", "argv": ["pi"], "state": "live", "status": {"state": "idle"}}), false));
        app.on_incoming(resource_event(3, json!({"id": "r1", "status": {"state": "working"}}), true));
        assert_eq!(app.resources["r1"]["status"]["state"], "working");
        assert_eq!(app.resources["r1"]["argv"][0], "pi", "unchanged fields are kept");
        let before = app.generation;
        app.on_incoming(resource_event(4, json!({"id": "r9", "status": null}), true));
        assert!(!app.resources.contains_key("r9"));
        assert!(app.resyncing && app.generation > before, "a partial event without a base forces a resync");
    }

    #[tokio::test]
    async fn partial_status_cannot_restore_an_unavailable_terminal_but_availability_can() {
        let (mut app, _) = app();
        app.load(&snapshot(1, "x"));
        app.on_incoming(resource_event(2, json!({"id": "r1", "state": "live", "available": true, "status": null}), false));
        app.terms.insert("view".into(), Term { resource: "r1".into(), stream: Some(9), parser: new_parser(80, 24), size: (80, 24), attaching: false, closed: None, retry_at: None, modes: Default::default() });
        app.stream_view.insert(9, "view".into());
        app.on_incoming(resource_event(3, json!({"id": "r1", "available": false, "stale": true}), true));
        assert!(app.terms["view"].stream.is_none());
        assert!(app.terms["view"].closed.is_some());
        app.on_incoming(resource_event(4, json!({"id": "r1", "status": {"state": "working"}}), true));
        assert_eq!(app.resources["r1"]["available"], false);
        assert!(app.terms["view"].closed.is_some(), "partial live status must not clear unavailability");
        app.on_incoming(resource_event(5, json!({"id": "r1", "available": true, "stale": false}), true));
        assert!(app.terms["view"].closed.is_none());
        assert!(app.terms["view"].can_attach(true, std::time::Instant::now()));
    }

    #[tokio::test]
    async fn oversized_legacy_snapshot_starts_a_usable_recovery_ui() {
        let (a, b) = tokio::io::duplex(4096);
        let (ar, aw) = tokio::io::split(a); let (br, bw) = tokio::io::split(b);
        let (client, _) = Peer::spawn(ar, aw); let (server, mut incoming) = Peer::spawn(br, bw);
        server.set_max_record_bytes(1024).unwrap();
        tokio::spawn(async move {
            while let Some(Incoming::Request { id, method, .. }) = incoming.recv().await {
                server.respond(id, Ok(if method == "workspace.snapshot" { json!("x".repeat(2048)) } else { json!({"reverted":true}) }));
            }
        });
        let snap = snapshot_for_client(&client, &json!({"epoch":"boot","seq":1})).await.unwrap();
        assert_eq!(snap["recovery_only"], true);
        let (tx, _) = mpsc::unbounded_channel();
        let mut app = App::new(client.clone(), tx); app.load(&snap);
        assert!(app.prefix_items().iter().any(|r| r.binding["command"] == "vqro.revert"));
        assert!(app.prefix_items().iter().any(|r| r.binding["command"] == "client.quit"));
        assert!(client.call("command.invoke", json!({"name":"vqro.revert"})).await.is_ok());
        assert!(!client.is_closed());
        app.run_command("client.quit", Value::Null, Value::Null, None);
        assert!(app.quit);
    }

    #[tokio::test]
    async fn early_attach_buffer_overflow_is_a_gap_not_retained_bytes() {
        let (mut app, _) = app();
        app.terms.insert("view".into(), Term { resource:"r1".into(), stream:None, parser:new_parser(80,24), size:(80,24), attaching:true, closed:None, retry_at:None, modes:Default::default() });
        let bytes = B64.encode(vec![b'x'; 65536]);
        for _ in 0..256 { app.on_incoming(Incoming::Notification { method:"stream.data".into(), params:json!({"stream":1,"data":bytes}) }); }
        assert!(!app.early.contains_key(&1));
        assert_eq!(app.early_closed[&1], "slow_consumer");
        app.on_msg(Msg::Attached { connection:app.connection, view:"view".into(), resource:"r1".into(), result:Ok(1) });
        assert_eq!(app.terms["view"].stream, None);
        assert!(app.terms["view"].retry_at.is_some());
    }

    #[tokio::test]
    async fn busy_snapshot_starts_recovery_and_retries_without_exiting() {
        for kind in ["overloaded","unavailable","timeout"] {
            let (a,b)=tokio::io::duplex(4096);
            let (ar,aw)=tokio::io::split(a);let(br,bw)=tokio::io::split(b);
            let(client,_)=Peer::spawn(ar,aw);let(server,mut incoming)=Peer::spawn(br,bw);
            tokio::spawn(async move {while let Some(Incoming::Request{id,..})=incoming.recv().await {
                server.respond(id,Err(crate::rpc::RpcError::new(kind,"snapshot busy")));
            }});
            let snap=snapshot_for_client(&client,&json!({"epoch":"boot","seq":1})).await.expect("busy startup must reach recovery UI");
            assert_eq!(snap["retry_snapshot"],true);
            let(tx,_)=mpsc::unbounded_channel();let mut app=App::new(client.clone(),tx);app.load(&snap);
            assert!(app.retry_snapshot.is_some(),"startup and reconnect must schedule recovery without waiting for an event");
            let mut terminal=ratatui::Terminal::new(ratatui::backend::TestBackend::new(100,24)).unwrap();
            terminal.draw(|f|app.draw(f)).unwrap();
            assert!(terminal.backend().buffer().content.iter().map(|c|c.symbol()).collect::<String>().contains("core busy"));
            assert!(!client.is_closed());
            app.run_command("client.quit",Value::Null,Value::Null,None);assert!(app.quit);
        }
    }

    #[test]
    fn snapshot_backoff_is_jittered_and_bounded_even_after_long_outage() {
        for attempt in [0,1,2,3,99,u32::MAX] {for _ in 0..16 {
            let delay=snapshot_retry_delay(attempt);
            assert!((std::time::Duration::from_millis(500)..=std::time::Duration::from_secs(3)).contains(&delay));
        }}
    }

    #[tokio::test]
    async fn busy_snapshot_retry_deadline_is_not_starved_by_ready_messages() {
        let(mut app,mut messages)=app();
        app.retry_snapshot=Some(std::time::Instant::now()-std::time::Duration::from_secs(1));
        app.tx.send(Msg::Info("event storm".into())).unwrap();
        let(_,mut incoming)=Peer::spawn(tokio::io::empty(),tokio::io::sink());
        let(key_tx,mut keys)=mpsc::unbounded_channel();let mut reconnecting=None;
        key_tx.send(Event::Key(KeyEvent::new(KeyCode::Char('g'),KeyModifiers::CONTROL))).unwrap();
        let UiInput::Key(key)=next_input(&app,&mut incoming,&mut messages,&mut keys,&mut reconnecting,None).await.unwrap() else{panic!("keys retain priority")};
        app.on_event(key);assert!(matches!(app.overlay,Some(Overlay::Prefix{..})));
        assert!(matches!(next_input(&app,&mut incoming,&mut messages,&mut keys,&mut reconnecting,None).await.unwrap(),UiInput::Tick));
    }

    #[tokio::test]
    async fn transient_snapshot_refusal_retries_in_place_and_preserves_event_fencing() {
        for kind in ["overloaded","unavailable","timeout"] {
            let(mut app,_)=app();app.load(&snapshot(1,"old"));app.resyncing=true;
            app.on_msg(Msg::Snapshot{generation:app.generation,result:Err(crate::rpc::RpcError::new(kind,"snapshot busy"))});
            assert!(!app.peer.is_closed(),"{kind} must not reconnect/reattach terminals");
            assert!(app.retry_snapshot.is_some());
            app.on_incoming(delta(2,"discard-until-snapshot"));
            assert_eq!(app.entries["doc/test/value"],"old","retry wait must remain fenced");
            let before=app.generation;
            app.on_incoming(Incoming::Notification{method:"missed".into(),params:Value::Null});
            assert_eq!(app.generation,before,"events must not bypass backoff");
            app.retry_snapshot=Some(std::time::Instant::now());app.retry_snapshot_if_due();
            assert!(app.resyncing&&app.generation>before);
            app.on_incoming(delta(4,"new"));
            app.on_msg(Msg::Snapshot{generation:app.generation,result:Ok(snapshot(3,"base"))});
            assert_eq!(app.entries["doc/test/value"],"new");assert_eq!(app.seq,4);
            assert!(app.retry_snapshot.is_none());assert!(!app.resyncing);
            assert!(!app.message.contains("core busy"),"busy notice clears once a fresh snapshot arrives");
        }
    }

    #[tokio::test]
    async fn resync_snapshot_cannot_overwrite_newer_entries() {
        let (mut app, _) = app();
        app.load(&snapshot(1, "old"));
        app.on_incoming(Incoming::Notification { method: "missed".into(), params: Value::Null });
        app.on_incoming(delta(2, "new"));
        assert_eq!(app.entries["doc/test/value"], "old", "deltas are fenced while snapshot is in flight");
        app.on_msg(Msg::Snapshot { generation: app.generation, result: Ok(snapshot(1, "old")) });
        assert_eq!(app.entries["doc/test/value"], "new");
        assert_eq!(app.seq, 2);
        app.on_incoming(delta(1, "stale"));
        assert_eq!(app.entries["doc/test/value"], "new", "pre-snapshot events cannot regress state");
    }

    #[tokio::test]
    async fn resync_discards_subsumed_events_and_late_snapshot_replies() {
        let (mut app, _) = app();
        app.load(&snapshot(1, "old"));
        app.resync();
        let first = app.generation;
        app.resync();
        app.on_incoming(delta(2, "older-than-snapshot"));
        app.on_msg(Msg::Snapshot { generation: first, result: Ok(snapshot(1, "stale")) });
        assert!(app.resyncing);
        app.on_msg(Msg::Snapshot { generation: app.generation, result: Ok(snapshot(3, "current")) });
        assert_eq!(app.entries["doc/test/value"], "current");
        app.disconnected();
        app.on_msg(Msg::Snapshot { generation: first, result: Ok(snapshot(1, "stale")) });
        assert_eq!(app.entries["doc/test/value"], "current");
    }

    #[tokio::test]
    async fn recovery_buffer_is_bounded_and_overflow_requests_a_new_snapshot() {
        let (mut app, _) = app();
        app.load(&snapshot(0, "old"));
        app.resync();
        let first = app.generation;
        for seq in 1..=10_001 {
            app.on_incoming(delta(seq, "new"));
        }
        assert!(app.generation > first);
        assert_eq!(app.buffered.len(), 1);
    }

    #[tokio::test]
    async fn reconnect_pending_still_selects_prefix_and_quit_without_a_pty() {
        let (mut app, mut messages) = app();
        app.disconnected();
        assert!(app.message.contains("reconnecting"));
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let screen: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(screen.contains("reconnecting"));
        let (_, mut incoming) = Peer::spawn(tokio::io::empty(), tokio::io::sink()); // Closed old connection must not starve keys.
        let (keys_tx, mut keys) = mpsc::unbounded_channel();
        let task = tokio::spawn(std::future::pending::<Connection>());
        let mut reconnecting = Some(task);
        for key in [KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL), KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)] {
            keys_tx.send(Event::Key(key)).unwrap();
            let input = tokio::time::timeout(std::time::Duration::from_secs(1), next_input(&app, &mut incoming, &mut messages, &mut keys, &mut reconnecting, None)).await.unwrap().unwrap();
            let UiInput::Key(ev) = input else { panic!("key must stay reachable") };
            app.on_event(ev);
        }
        assert!(app.quit);
        reconnecting.unwrap().abort();
    }

    #[tokio::test]
    async fn stale_attach_reply_cannot_adopt_or_close_a_new_connections_stream() {
        use tokio::io::AsyncBufReadExt;
        let (mut app, _) = app();
        let old_connection = app.connection;
        app.disconnected();
        let (local, remote) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(local);
        let (peer, _) = Peer::spawn(reader, writer);
        app.reconnected(peer, &snapshot(1, "current"));
        app.terms.insert("view".into(), Term {
            resource: "r1".into(), stream: None, parser: new_parser(80, 24),
            size: (80, 24), attaching: true, closed: None, retry_at: None, modes: Default::default(),
        });
        app.on_msg(Msg::Attached { connection: old_connection, view: "view".into(), resource: "r1".into(), result: Ok(1) });
        assert!(app.terms["view"].attaching);
        assert_eq!(app.terms["view"].stream, None);
        // Even a removed view's stale successful reply must not close the same id on the new peer.
        app.on_msg(Msg::Attached { connection: old_connection, view: "removed".into(), resource: "r1".into(), result: Ok(1) });
        let mut lines = tokio::io::BufReader::new(remote).lines();
        assert!(tokio::time::timeout(std::time::Duration::from_millis(20), lines.next_line()).await.is_err());
        app.on_msg(Msg::Attached { connection: app.connection, view: "view".into(), resource: "r1".into(), result: Ok(1) });
        assert_eq!(app.terms["view"].stream, Some(1));
    }

    #[tokio::test]
    async fn reconnect_draw_reattaches_terminal_and_repaints_backend_output() {
        use crate::server::{start, Backend};
        let dir = tempfile::Builder::new().prefix("pr").tempdir_in("/tmp").unwrap();
        let socket = dir.path().join("s");
        let _core = start(socket.clone(), dir.path().join("state"), false, Backend::Fake, None).unwrap();
        let (peer, mut incoming, _) = client::connect(&socket, "test").await.unwrap();
        peer.call("resource.spawn", json!({"argv": ["sh"]})).await.unwrap();
        peer.call("command.invoke", json!({"name": "vqro.layout.reset"})).await.unwrap();
        let snap = peer.call("workspace.snapshot", Value::Null).await.unwrap();
        let (mut app, mut messages) = app();
        app.terms.insert("term-r1".into(), Term {
            resource: "r1".into(), stream: Some(99), parser: new_parser(80, 24),
            size: (80, 24), attaching: false, closed: None, retry_at: None, modes: Default::default(),
        });
        app.stream_view.insert(99, "term-r1".into());
        app.early.insert(99, vec![b"stale".to_vec()]);
        app.disconnected();
        app.reconnected(peer, &snap);
        assert!(app.terms.is_empty());
        assert!(app.stream_view.is_empty());
        assert!(app.early.is_empty());
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                tokio::select! {
                    Some(m) = incoming.recv() => app.on_incoming(m),
                    Some(m) = messages.recv() => app.on_msg(m),
                }
                if app.terms.values().any(|t| t.stream.is_some() && t.parser.screen().contents().contains("fake f1")) {
                    break;
                }
            }
        }).await.unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let screen: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(screen.contains("fake f1"), "reattached output must be repainted");
        // A stream can close while its resource stays live and emits no new resource event.
        let sid = app.terms["term-r1"].stream.unwrap();
        app.on_incoming(Incoming::Notification { method: "stream.closed".into(), params: json!({"stream": sid, "reason": {"reason": "slow_consumer"}}) });
        assert!(app.terms["term-r1"].stream.is_none());
        app.terms.get_mut("term-r1").unwrap().retry_at = Some(std::time::Instant::now());
        terminal.draw(|f| app.draw(f)).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                tokio::select! {
                    Some(m) = incoming.recv() => app.on_incoming(m),
                    Some(m) = messages.recv() => app.on_msg(m),
                }
                let term = &app.terms["term-r1"];
                if term.stream.is_some_and(|new| new != sid) && term.parser.screen().contents().contains("fake f1") { break; }
            }
        }).await.unwrap();
    }

    #[tokio::test]
    async fn closure_before_attach_reply_cannot_adopt_a_closed_stream() {
        let (mut app, _) = app();
        app.terms.insert("view".into(), Term {
            resource: "r1".into(), stream: None, parser: new_parser(80, 24),
            size: (80, 24), attaching: true, closed: None, retry_at: None, modes: Default::default(),
        });
        app.on_incoming(Incoming::Notification { method: "stream.closed".into(), params: json!({"stream": 7, "reason": {"reason": "slow_consumer"}}) });
        app.on_msg(Msg::Attached { connection: app.connection, view: "view".into(), resource: "r1".into(), result: Ok(7) });
        assert!(app.terms["view"].stream.is_none());
        assert_eq!(app.terms["view"].closed.as_deref(), Some("slow_consumer"));
        assert!(app.early_closed.is_empty());
    }

    #[test]
    fn closed_live_terminal_retries_without_a_resource_event_and_resets_parser() {
        let mut term = Term {
            resource: "r1".into(), stream: Some(1), parser: new_parser(80, 24),
            size: (80, 24), attaching: false, closed: None, retry_at: None, modes: Default::default(),
        };
        term.parser.process(b"old\x1b[");
        term.close("slow_consumer".into());
        let now = std::time::Instant::now();
        assert!(!term.can_attach(true, now), "retry is paced");
        assert!(!term.can_attach(false, now + std::time::Duration::from_secs(2)));
        assert!(term.can_attach(true, now + std::time::Duration::from_secs(2)));
        term.begin_attach();
        assert!(term.attaching);
        assert!(term.closed.is_none());
        term.parser.process(b"repaint");
        assert_eq!(term.parser.screen().contents(), "repaint", "fresh attach cannot reuse old VT state");
    }

    #[tokio::test]
    async fn bindings_honour_when_and_fall_through_while_profiles_keep_winning() {
        let (mut app, _) = app();
        app.entries.insert("workspace/active".into(), json!({"layout": "main"}));
        app.entries.insert("layout/main".into(), json!({"split": "h", "children": [{"view": "agents-r8"}, {"view": "t1"}]}));
        app.entries.insert("view/agents-r8".into(), json!({"kind": "panel", "title": "Agent r8", "resource": "r8", "items": []}));
        app.entries.insert("view/t1".into(), json!({"kind": "terminal", "resource": "r1"}));
        app.resources.insert("r8".into(), json!({"id": "r8", "kind": "agent", "state": "live", "available": true}));
        app.resources.insert("r1".into(), json!({"id": "r1", "kind": "terminal", "state": "live", "available": true}));
        app.entries.insert("binding/agents.prompt".into(), json!({"key": "p", "command": "agents.prompt", "input": "Prompt agent", "label": "prompt agent", "when": {"resource_kind": "agent"}}));
        for name in ["agents.prompt", "x.paste"] {
            app.commands.insert(name.into(), json!({"name": name, "available": true}));
        }
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let (agent, shell) = (app.hit.leaves.iter().position(|l| l.0 == "agents-r8").unwrap(), app.hit.leaves.iter().position(|l| l.0 == "t1").unwrap());
        let p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE);
        let on = |app: &App, pane: usize| app.binding_for("p", &app.ctx_leaf(pane)).map(|b| b["command"].as_str().unwrap().to_string());
        let listed = |app: &mut App, pane: usize| { app.focus = pane; app.prefix_items().iter().any(|r| r.binding["command"] == "agents.prompt") };

        assert_eq!(app.ctx_leaf(agent)["resource_kind"], "agent");
        assert_eq!(app.ctx_leaf(shell)["resource_kind"], "terminal");
        assert_eq!(on(&app, agent).as_deref(), Some("agents.prompt"));
        assert_eq!(on(&app, shell), None, "a binding whose `when` fails is no candidate");
        assert!(listed(&mut app, agent) && !listed(&mut app, shell), "the prefix menu shows what the key does here");

        // On a shell pane, p neither prompts nor runs anything; it says why.
        app.focus = shell;
        app.on_prefix_key(p, false);
        assert!(app.overlay.is_none(), "no prompt that would only fail after typing");
        // On the agent pane, p asks for the text first.
        app.focus = agent;
        app.on_prefix_key(p, false);
        assert!(matches!(&app.overlay, Some(Overlay::Input { prompt, item, .. }) if prompt == "Prompt agent" && item.command == "agents.prompt"));
        app.overlay = None;

        // Fall-through: an unconditional binding for the same key serves panes where `when` fails.
        app.entries.insert("binding/z.p".into(), json!({"key": "p", "command": "x.paste"}));
        assert_eq!(on(&app, agent).as_deref(), Some("agents.prompt"), "same priority: lowest id");
        assert_eq!(on(&app, shell).as_deref(), Some("x.paste"));
        // A selected keymap profile (priority 10, no `when`) keeps winning everywhere.
        app.entries.insert("binding/keys.p".into(), json!({"key": "p", "command": "client.tab", "args": {"to": "prev"}, "priority": 10, "owner": "keys"}));
        app.entries.insert("extension/keys".into(), json!({"argv": ["x"]}));
        assert_eq!(on(&app, agent).as_deref(), Some("client.tab"));
        assert_eq!(on(&app, shell).as_deref(), Some("client.tab"));
    }

    fn winner(cands: &[(String, Value)], key: &str) -> String {
        cands[resolve(cands)[key]].0.clone()
    }

    #[test]
    fn priority_then_defaults_then_id() {
        let b = |id: &str, v: Value| (id.to_string(), v);
        let mut cands: Vec<(String, Value)> = builtin_bindings().into_iter().map(|v| (format!("{BUILTIN}{}", v["key"].as_str().unwrap()), v)).collect();
        cands.push(b("binding/tabs.new", json!({"key": "n", "command": "tabs.open"})));
        cands.push(b("binding/a.q", json!({"key": "q", "command": "x.y"})));
        cands.push(b("binding/z.n", json!({"key": "n", "command": "x.y"})));
        cands.push(b("binding/keys.n", json!({"key": "n", "command": "client.tab", "priority": 10})));
        cands.push(b("binding/keys.u", json!({"key": "u", "command": "client.none", "priority": 10})));
        cands.push(b("binding/keys.left", json!({"key": "left", "command": "client.focus"})));
        cands.push(b("binding/future", json!({"key": "Hyper-x", "command": "x.y"})));
        assert_eq!(winner(&cands, "q"), "builtin:q", "defaults win ties");
        assert_eq!(winner(&cands, "n"), "binding/keys.n", "priority wins");
        assert_eq!(winner(&cands, "u"), "binding/keys.u", "unbinding is a binding that wins");
        assert_eq!(winner(&cands, "Left"), "binding/keys.left", "named keys are normalized");
        assert!(!resolve(&cands).contains_key("Hyper-x"), "unknown keys are ignored");
        cands.retain(|(id, _)| id != "binding/keys.n");
        assert_eq!(winner(&cands, "n"), "binding/tabs.new", "then the lowest id");
    }

    #[test]
    fn directional_focus() {
        // +---+-----+
        // | 0 |  1  |
        // |   +--+--+
        // |   |2 |3 |
        // +---+--+--+
        let r = |x, y, w, h| Rect { x, y, width: w, height: h };
        let rects = [r(0, 0, 10, 20), r(10, 0, 20, 10), r(10, 10, 10, 10), r(20, 10, 10, 10)];
        assert_eq!(neighbour(&rects, 0, "right"), Some(1), "touching panes: the better aligned, then the first");
        assert_eq!(neighbour(&rects, 1, "down"), Some(2));
        assert_eq!(neighbour(&rects, 3, "left"), Some(2));
        assert_eq!(neighbour(&rects, 3, "up"), Some(1));
        assert_eq!(neighbour(&rects, 2, "left"), Some(0));
        assert_eq!(neighbour(&rects, 0, "left"), None);
        assert_eq!(neighbour(&rects, 0, "sideways"), None);
    }

    #[test]
    fn paste_is_bracketed_only_when_the_program_asked_and_cannot_close_the_bracket_early() {
        assert_eq!(paste_bytes("a\nb\n", false), b"a\nb\n".to_vec());
        assert_eq!(paste_bytes("a\nb\n", true), b"\x1b[200~a\nb\n\x1b[201~".to_vec());
        // A paste that contains the end marker must not end the bracket and run the rest as typed input.
        assert_eq!(paste_bytes("x\x1b[201~rm -rf ~\n", true), b"\x1b[200~xrm -rf ~\n\x1b[201~".to_vec());
    }

    #[tokio::test]
    async fn a_dropdown_is_drawn_over_its_popup_and_the_prefix_reaches_recovery_from_it() {
        let (mut app, _) = app();
        app.entries.insert("view/pop".into(), json!({"kind": "panel", "title": "Popup Title", "items": [
            {"type": "select", "text": "Choice", "value": "a", "command": "x.pick", "options": [{"label": "Alpha", "value": "a"}, {"label": "Beta", "value": "b"}]}]}));
        app.overlay = Some(Overlay::Popup { view: "pop".into(), width: 60, ctx: json!({}) });
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.overlay, Some(Overlay::Menu { .. })) && app.under.is_some(), "Enter on a select opens its menu over the popup");
        terminal.draw(|f| app.draw(f)).unwrap();
        let screen: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(screen.contains("Popup Title"), "the popup stays visible under its drop-down");
        assert!(screen.contains("Beta"), "the drop-down is drawn");
        // Anchored under the row, not at the screen origin.
        let Some(Overlay::Menu { at, .. }) = &app.overlay else { unreachable!() };
        assert!(at.x > 0 && at.y > 0);
        // The prefix cancels the drop-down and opens the prefix menu; the popup returns afterwards.
        app.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert!(matches!(app.overlay, Some(Overlay::Prefix { .. })));
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.restore_under();
        assert!(matches!(&app.overlay, Some(Overlay::Popup { view, .. }) if view == "pop"));
    }
}
