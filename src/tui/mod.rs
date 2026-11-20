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
//! `client.tab {to}`, `client.layout {name?}`, `client.quit`, `client.none` (unbinds a key).
//!
//! Each window shows its own layout (workspace, `ui_model::window`): it opens on the shared `workspace/active`,
//! then follows only its own switches. Every command it invokes carries that layout in `args.context.layout`.

mod icons;
mod keys;
mod mouse;
mod panel;
mod compose;
mod probe;
mod emulator;
mod ghostty;
mod scrollback;
use emulator::{CursorShape, Effect, Emulator};
mod frame_budget;
mod theme;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use base64::Engine;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::client;
use crate::rpc::{Incoming, Peer};
use crate::ui_model::recovery::{self, PrefixRow, SAFE_MODE_VIEW};
use crate::ui_model::window::{self, Settled, Window};
use crate::ui_model::workspace::{Change, Workspace};
use crate::ui_model::composer::{Composer, Editor, Outcome};
use icons::Icons;
use theme::Palette;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// Largest paste the client sends (after bracketed-paste markers). Bigger pastes are refused whole,
/// with a message; nothing is sent. Sent in ordered chunks of at most `PASTE_CHUNK` bytes (the core's
/// per-message input limit), each confirmed before the next.
const MAX_PASTE: usize = 4 * 1024 * 1024;
const PASTE_CHUNK: usize = crate::adapter::MAX_STREAM_CHUNK;
/// How long one chunk may keep meeting a full input queue (`overloaded`) before the paste stops.
/// How long after a `repeat` binding its key (or another `repeat` key) works without the prefix; each use renews it.
const REPEAT_WINDOW: std::time::Duration = std::time::Duration::from_millis(600);
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
    emu: Emulator,
    size: (u16, u16),
    attaching: bool,
    closed: Option<String>,
    retry_at: Option<std::time::Instant>,
    /// Extended key reporting the program asked for (kitty protocol, modifyOtherKeys), from its output.
    /// Kept across reattach: the program still has it set even though a repaint doesn't repeat it.
    modes: keys::KeyModes,
}

impl Term {
    /// Program output: to the emulator and the key-mode tracker.
    fn feed(&mut self, bytes: &[u8]) {
        self.modes.feed(bytes);
        self.emu.process(bytes);
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
        // A gap invalidates the emulator's state. Only the backend's new attach/history repaint can
        // restore it; never concatenate an old partial escape sequence with a fresh stream.
        self.emu = self.emu.fresh();
    }
}

enum Overlay {
    Prefix { sel: usize },
    Palette { query: String, sel: usize },
    /// A menu built from `menu/*` entries for a place (pane, tab, ...), at the pointer.
    /// `query` filters it once it is long (see `SEARCHABLE`); `sel` indexes the filtered rows.
    Menu { items: Vec<Item>, ctx: Value, sel: usize, at: Position, query: String },
    /// One line of text for an item that declared `input`.
    Input { prompt: String, text: String, item: Item, ctx: Value, multi: Option<Editor> },
    /// Choose a resource for an item that declared `pick: "resource"`. Filtered like `Menu`.
    Pick { sel: usize, item: Item, ctx: Value, query: String },
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
    /// Draw the icon right after the label instead (a compact `+◆` button).
    icon_after: bool,
    command: String,
    args: Value,
    input: Option<String>,
    pick: Option<String>,
    /// For status items: take the label (and style) from this view's live content.
    view: Option<String>,
    /// Explicit initial input text: a string, or {context: field} read from invocation context.
    prefill: Value,
    /// For status items: take the label from this window's context (`layout`, `layout_title`).
    label_context: Option<String>,
    /// A multi-line prompt (feature multiline), and its size limit in bytes.
    multiline: bool,
    max_bytes: Option<u64>,
}


impl Item {
    fn from_entry(v: &Value) -> Item {
        Item {
            label: v["label"].as_str().unwrap_or("").to_string(),
            icon: v["icon"].clone(),
            icon_after: v["icon_after"].as_bool().unwrap_or(false),
            command: v["command"].as_str().unwrap_or("").to_string(),
            args: v["args"].clone(),
            input: v["input"].as_str().map(String::from),
            pick: v["pick"].as_str().map(String::from),
            view: v["view"].as_str().map(String::from),
            prefill: v["prefill"].clone(),
            label_context: v["label_context"].as_str().map(String::from),
            multiline: v["multiline"] == true,
            max_bytes: v["max_bytes"].as_u64(),
        }
    }
}

enum Msg {
    Attached { connection: u64, view: String, resource: String, result: Result<u64, String> },
    Info(String),
    /// A command finished: say so unless something newer took the status line.
    Done(String),
    /// A composer's send succeeded: clear its draft if it still holds the sent text.
    ComposerSent { key: String, row: String, text: String },
    Snapshot { generation: u64, result: crate::rpc::RpcResult },
    ClearOverrides,
    /// A command's reply asked this client to show a view as a popup.
    Popup { view: String, width: u16, ctx: Value },
    ClosePopup,
    /// A command's reply asks to focus a visible view, independently of its kind.
    FocusView(String),
    /// A command's reply (or this window's reset) asked this window to show a layout.
    ShowLayout(String),
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

/// A left press on a tab chip, for the `mouse.drag` tab gesture: (tab group path, tab index).
type TabPress = (Vec<usize>, usize);

struct App {
    peer: Peer,
    tx: mpsc::UnboundedSender<Msg>,
    /// The renderer-independent model: the workspace cache and its contributions.
    ws: Workspace,
    /// Safe-mode chooser choices not applied yet (this client's UI state).
    safe_selected: BTreeSet<String>,
    /// This connection resolved no owner at hello; cache refreshes cannot retag it.
    owner_unset: bool,
    terms: HashMap<String, Term>,
    stream_view: HashMap<u64, String>,
    early: HashMap<u64, Vec<Vec<u8>>>,
    early_closed: HashMap<u64, String>,
    /// The stream last typed into and when: its output right after is an echo, drawn at once (`echo`).
    typed: Option<(u64, std::time::Instant)>,
    leaves: Vec<String>,
    focus: usize,
    overlay: Option<Overlay>,
    /// Client-side panel state (selection, expansion, page, filter, scroll) by panel key.
    pstate: HashMap<String, panel::State>,
    /// Panel key whose filter is being typed in the status bar.
    filtering: Option<String>,
    /// The filter being typed applies to the panel's detail pane, not its list.
    filter_detail: bool,
    /// A transient notification: text, style, until.
    toast: Option<(String, Option<String>, std::time::Instant)>,
    /// When each view last showed a bell notice (rate limit).
    bells: HashMap<String, std::time::Instant>,
    /// Text selected in terminal views (by mouse), and the view a selecting drag is in.
    selections: HashMap<String, scrollback::Selection>,
    selecting: Option<String>,
    /// The last left press in a terminal: when, where and how many in a row (double and triple clicks).
    last_press: Option<(std::time::Instant, String, scrollback::Pos, u8)>,
    scroll_mode: Option<ScrollMode>,
    /// Engine fallbacks already reported (each reason once per session).
    engine_notes: HashSet<String>,
    /// A popup whose view hasn't arrived yet: view, width, context, and until when it may still open.
    pending_popup: Option<(String, u16, Value, std::time::Instant)>,
    /// The cursor shape the focused terminal's program asked for, and the one last sent to the outer terminal.
    cursor_style: CursorShape,
    cursor_sent: CursorShape,
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
    /// The focused view composer `autofocus` last looked at (it acts once per focus change).
    autofocused: Option<String>,
    /// The layout (workspace) this window shows: window state, never workspace state (`ui_model::window`).
    window: Window,
    /// Focus this view on the next draw (after leaving zoom the leaves are recomputed).
    pending_focus: Option<String>,
    /// A tab chip pressed with the left button, while a `mouse.drag` tab gesture is bound.
    tab_press: Option<TabPress>,
    /// After a `repeat` binding ran from the prefix: until this instant, a key with a `repeat` binding runs again
    /// without the prefix (tmux's `-r`); any other key ends it and goes where it would have.
    repeat_until: Option<std::time::Instant>,
    /// The terminal whose selecting press had Ctrl: released without a drag over a link, it runs `mouse.link`.
    link_click: Option<String>,
    /// The focused view at the last draw, and the one focused before it (`client.focus {dir: "last"}`).
    focus_now: Option<String>,
    focus_last: Option<String>,
    /// What each resource reported when its tab was last shown (`ui_model::tabbar::seen_of`), for tab marks.
    seen: HashMap<String, Value>,
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

/// How long after typing into a stream its output counts as the key's echo (drawn at once). Beyond it,
/// output is background and coalesced to the frame budget, so a program streaming after Enter can't
/// make every chunk a frame.
const ECHO_WINDOW: std::time::Duration = std::time::Duration::from_millis(100);

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
    let result:Result<Value>=match peer.call_with_timeout("workspace.snapshot", Value::Null, SNAPSHOT_TIMEOUT).await {
        Ok(snapshot) => Ok(snapshot),
        Err(error) if error.kind() == "too_large" => Ok(recovery_snapshot(hello, &error.message)),
        Err(error) if snapshot_transient(&error) => {
            let mut snap = recovery_snapshot(hello, &error.message);
            snap["recovery_notice"] = json!(format!("core busy: {error}; retrying snapshot (input is not replayed)"));
            snap["retry_snapshot"] = json!(true);
            Ok(snap)
        }
        Err(error) => Err(error.into()),
    };
    result.map(|mut snap|{snap["client_owner_unset"]=json!(hello["owner_unset"].as_bool().unwrap_or(false));snap})
}

/// This client's `hello`: name, build, profiles and the panel features it renders (D29).
fn hello_params() -> Value {
    let mut hello=crate::ui_model::hello("tui", client::build_stamp(), crate::ui_model::PROFILES, panel::SUPPORTED);
    hello["identity"]=match std::env::var("VQRO_PRINCIPAL").ok().filter(|id|id!=crate::principal::UNDECLARED){Some(principal)=>json!({"kind":"principal","principal":principal}),None=>json!({"kind":"owner"})};
    hello
}

/// Carries a window's shown layout across its re-exec into a new build (read only at start).
const LAYOUT_ENV: &str = "VQRO_TUI_LAYOUT";

pub async fn run(socket: &Path) -> Result<()> {
    let started_as = exe_stamp();
    let mut reexec = false;
    let (peer, mut incoming, hello) = client::connect_with(socket, hello_params()).await?;
    let snap = snapshot_for_client(&peer, &hello).await?;
    let seq = snap["seq"].as_u64().unwrap_or(0);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut app = App::new(peer.clone(), tx);
    // A UI restarting into a new build keeps showing its window's workspace.
    app.window.pinned = std::env::var(LAYOUT_ENV).ok().filter(|s| !s.is_empty());
    app.load(&snap);
    if let Some(notice)=snap["recovery_notice"].as_str() {app.message=notice.into();}
    else if app.owner_unset {app.message=crate::principal::OWNER_NOTICE.into();}
    else {app.set_key_hint("click vqro or press ", &app.prefix(), " for the menu");}
    peer.call("events.subscribe", json!({"from": seq, "epoch": snap["epoch"], "partial_resources": true})).await?;

    let mut terminal = ratatui::init();
    // Asked before the input thread starts reading: the query reads the terminal's reply.
    app.kbd_enhance = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    keys::set_shift_enter_reported(app.kbd_enhance);
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
                app.sync_cursor_style();
                frames.drawn(now);
                dirty = false;
            }
            if app.quit {
                return Ok(());
            }
            let input = next_input(&app, &mut incoming, &mut rx, &mut keys, &mut reconnecting, frames.deadline(dirty)).await?;
            let key_input = matches!(input, UiInput::Key(_));
            let echo = matches!(&input, UiInput::Incoming(Some(m)) if app.echo(m));
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
            interactive = (key_input || echo) && dirty;
            // Richer key reports while the prefix menu is open; "disambiguate" only while the focused
            // program asked for extended keys (so Shift-Enter and the like can reach it). Otherwise the
            // outer terminal's keyboard reporting is left as it was.
            let prefix_open = matches!(app.overlay, Some(Overlay::Prefix { .. }));
            if app.kbd_enhance {
                keys::enhance(if prefix_open {
                    keys::Enhance::Full
                } else if app.focused_extended() != keys::Extended::Legacy || app.typing_lines() {
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
    let shown = app.window.shown(&app.ws);
    if let Some(task) = reconnecting {
        task.abort();
    }
    // Every way out (quit, error, re-exec) passes here; pop before leaving the alternate screen.
    keys::enhance(keys::Enhance::Off);
    if app.cursor_sent != CursorShape::Default { let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::SetCursorStyle::DefaultUserShape); }
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture, crossterm::event::DisableBracketedPaste);
    ratatui::restore();
    if reexec {
        if let Some((exe, _)) = exe_stamp() {
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                let args: Vec<String> = std::env::args().skip(1).collect();
                let mut cmd = std::process::Command::new(exe);
                // The restarted UI keeps showing this window's workspace.
                if let Some(layout) = &shown { cmd.env(LAYOUT_ENV, layout); }
                let err = cmd.args(args).exec();
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
            let (peer, incoming, hello) = client::connect_with(&socket, hello_params()).await?;
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
            peer, tx, ws: Workspace::default(), safe_selected: BTreeSet::new(), owner_unset:false,
            terms: HashMap::new(), stream_view: HashMap::new(), early: HashMap::new(), early_closed: HashMap::new(), typed: None,
            leaves: Vec::new(), focus: 0, overlay: None, pstate: HashMap::new(),
            filtering: None, filter_detail: false, toast: None, bells: HashMap::new(), selections: HashMap::new(), selecting: None, last_press: None, scroll_mode: None, engine_notes: HashSet::new(), pending_popup: None, cursor_style: CursorShape::Default, cursor_sent: CursorShape::Default, panel_values: Mutex::new(HashMap::new()), panel_dependencies: Mutex::new(HashMap::new()), hit: Hit::default(),
            drag: None, overrides: HashMap::new(), mouse_owner: None, mouse_on: true,
            under: None, input_queues: HashMap::new(), view_streams: Arc::default(), zoomed: None, autofocused: None, window: Window::default(), pending_focus: None, tab_press: None, link_click: None, repeat_until: None, focus_now: None, focus_last: None, seen: HashMap::new(), kbd_enhance: false,
            held: KeyModifiers::NONE, message: String::new(), key_hint: None,
            quit: false, connected: true, connection: 0,
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
        self.typed = None; // stream ids belong to a connection
        self.terms.clear();
        self.stream_view.clear();
        self.early.clear();
        self.early_closed.clear();
        self.load(snap);
        self.message = snap["recovery_notice"].as_str().unwrap_or(if self.owner_unset {crate::principal::OWNER_NOTICE}else{"reconnected"}).into();
    }

    /// Open a popup a command reply asked for. A command may write the view and reply at once; the reply and the
    /// entries event travel separately, so a view this client doesn't know yet waits for the next events (up to
    /// `POPUP_WAIT`, or until the user does something) instead of being refused.
    fn open_popup(&mut self, view: String, width: u16, ctx: Value) {
        match self.view_kind(&view).as_str() {
            "" => {
                self.message = format!("opening {view}…");
                self.pending_popup = Some((view, width, ctx, std::time::Instant::now() + POPUP_WAIT));
            }
            "terminal" => self.message = "popup refused: terminals never pop up".into(),
            _ => {
                self.pending_popup = None;
                // Asked again for the popup already shown (a second `settings.open`, a late reply): keep where the
                // user is in it (filter, page, selection) instead of starting over.
                let same = matches!(&self.overlay, Some(Overlay::Popup { view: shown, .. }) if *shown == view);
                if !same {
                    self.under = None;
                    self.pstate.insert("@popup".into(), panel::State::default());
                }
                self.overlay = Some(Overlay::Popup { view, width, ctx });
                if self.message.starts_with("opening ") { self.message.clear(); }
            }
        }
    }

    /// After events arrive (or on input): open the waiting popup if its view is known now; past its wait, say so.
    fn settle_pending_popup(&mut self) {
        let Some((view, width, ctx, until)) = self.pending_popup.take() else { return };
        if !self.view_kind(&view).is_empty() { return self.open_popup(view, width, ctx); }
        if std::time::Instant::now() >= until { self.message = format!("popup: no view {view}"); }
        else { self.pending_popup = Some((view, width, ctx, until)); }
    }

    fn load(&mut self, snap: &Value) {
        if let Some(unset)=snap["client_owner_unset"].as_bool(){self.owner_unset=unset;}
        self.recovery_only = snap["recovery_only"].as_bool().unwrap_or(false);
        self.snapshot_retries = u32::from(snap["retry_snapshot"] == true);
        self.retry_snapshot = (self.snapshot_retries > 0).then(|| std::time::Instant::now() + snapshot_retry_delay(self.snapshot_retries));
        self.panel_values.lock().unwrap().clear();
        self.panel_dependencies.lock().unwrap().clear();
        for state in self.pstate.values_mut() { state.prepared = None; }
        let was_safe = self.ws.load(snap);
        self.safe_mode_changed(self.ws.safe_mode, was_safe);
        self.settle_pending_popup();
        self.settle_window();
    }

    /// Entering safe mode opens the built-in chooser; leaving it closes the chooser.
    fn safe_mode_changed(&mut self, on: bool, was: bool) {
        if on && !was { self.open_safe_chooser(); }
        if !on && matches!(&self.overlay,Some(Overlay::Popup{view,..}) if view==SAFE_MODE_VIEW) { self.overlay=None; }
    }

    /// Output of the stream just typed into, shortly after the input: the program's echo or answer to a
    /// key. Drawn like a key press (at once), not held for the background frame budget.
    fn echo(&self, m: &Incoming) -> bool {
        let Some((sid, at)) = self.typed else { return false };
        matches!(m, Incoming::Notification { method, params } if method == "stream.data" && params["stream"].as_u64() == Some(sid))
            && at.elapsed() < ECHO_WINDOW
    }

    fn on_incoming(&mut self, m: Incoming) {
        let Incoming::Notification { method, params } = m else { return };
        match method.as_str() {
            "stream.data" => {
                let (Some(sid), Some(data)) = (params["stream"].as_u64(), params["data"].as_str()) else { return };
                let Ok(bytes) = B64.decode(data) else { return };
                match self.stream_view.get(&sid).and_then(|v| self.terms.get_mut(v)) {
                    Some(t) => {
                        t.feed(&bytes);
                        let effects = t.emu.take_effects();
                        if !effects.is_empty() { let view = self.stream_view[&sid].clone(); self.pass_effects(&view, effects); }
                    }
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
        for change in self.ws.apply(ev) {
            match change {
                Change::Resync => return self.resync(),
                Change::View(view) => {
                    self.invalidate_panel(&view);
                    for state in self.pstate.values_mut() { state.prepared = None; }
                    if self.pending_popup.as_ref().is_some_and(|p| p.0 == view) { self.settle_pending_popup(); }
                }
                // The committed layout replaces any local drag preview.
                Change::Layout => self.overrides.clear(),
                Change::ChangedBy { author, txn } => {
                    if !author.starts_with("tui:") && !author.starts_with("ext:") {
                        self.message = format!("workspace changed by {author} (txn {txn})");
                    }
                    self.settle_window();
                }
                Change::Resource { id, available, live, restored } => {
                    for term in self.terms.values_mut().filter(|t| t.resource == id) {
                        if !available {
                            if let Some(stream) = term.stream.take() {
                                self.stream_view.remove(&stream);
                                self.peer.notify("stream.close", json!({"stream": stream}));
                            }
                            term.close("resource unavailable".into());
                        } else if live {
                            term.closed = None;
                            if restored { term.retry_at = Some(std::time::Instant::now()); }
                        }
                    }
                }
                Change::ExtensionFailed { name, reason } => self.message = format!("extension {name} failed: {reason}"),
                Change::SafeMode { on, was } => self.safe_mode_changed(on, was),
                Change::Notify { text, style } => self.toast = Some((text, style, std::time::Instant::now() + std::time::Duration::from_secs(4))),
                Change::BackendDisconnected { name, error } => self.message = format!("backend {} disconnected: {error}", json!(name)),
            }
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
                            if ev["epoch"].as_str() == Some(self.ws.epoch.as_str()) {
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
            Msg::ComposerSent { key, row, text } => {
                if let Some(c) = self.pstate.get_mut(&key).and_then(|st| st.composers.get_mut(&row)) {
                    if c.editor.string() == text { c.editor.text.clear(); c.editor.cursor = 0; }
                }
            }
            Msg::Popup { view, width, ctx } => self.open_popup(view, width, ctx),
            Msg::ShowLayout(name) => self.show_layout(&name),
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
                            // Output from before the attach reply is the backend's history repaint: never re-run a
                            // copy, bell or notification from it.
                            t.emu.take_effects();
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
        self.settle_pending_popup();
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
                if !self.paste_text(&text) { self.paste(&text); }
                true
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
            t.emu.set_scroll_offset(0);
            if let Some(sid) = t.stream {
                self.send_input(sid, bytes);
            }
        }
    }

    /// Input for a stream, in order. Direct, unless a paste is still being sent on that stream: then
    /// it queues behind the paste so keys never overtake it.
    fn send_input(&mut self, sid: u64, bytes: Vec<u8>) {
        self.typed = Some((sid, std::time::Instant::now()));
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
        t.emu.set_scroll_offset(0);
        let bytes = paste_bytes(text, t.emu.bracketed_paste());
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
        if self.scroll_mode.is_some() && self.overlay.is_none() {
            if is_prefix || self.scroll_mode.as_ref().is_some_and(|m| self.leaves.get(self.focus) != Some(&m.view)) {
                self.leave_scroll_mode();
            } else {
                return self.on_scroll_key(k);
            }
        }
        match self.overlay {
            Some(Overlay::Popup { .. }) => return self.on_popup_key(k),
            Some(Overlay::Prefix { .. }) => return self.on_prefix_key(k, is_prefix),
            Some(_) => return self.on_overlay_key(k),
            None => {}
        }
        if let Some(until) = self.repeat_until.take() {
            let repeat = keys::name(&k).filter(|_| std::time::Instant::now() < until && !is_prefix)
                .and_then(|n| self.binding_for(&n, &self.ctx_leaf(self.focus))).filter(|b| b["repeat"] == true);
            if let Some(b) = repeat {
                self.run_binding(&b);
                self.arm_repeat(&b);
                return;
            }
        }
        if is_prefix {
            self.overlay = Some(Overlay::Prefix { sel: 0 });
            return;
        }
        let Some(view) = self.leaves.get(self.focus).cloned() else { return };
        match self.view_kind(&view).as_str() {
            "terminal" => {
                let Some(t) = self.terms.get(&view) else { return };
                if let Some(bytes) = keys::encode(&k, t.emu.application_cursor(), t.modes.extended()) {
                    self.selections.remove(&view);
                    self.send_to_focused(bytes);
                }
            }
            "panel" => {
                let ctx = self.ctx_leaf(self.focus);
                // Pane- and composer-scoped bindings (D29): keys without the prefix, before the panel's own keys.
                if let Some(name) = keys::name(&k) {
                    let typing = self.pstate.get(&view).is_some_and(|s| s.editing.is_some());
                    if let Some(b) = self.ws.scoped_binding(&name, if typing { "composer" } else { "pane" }, &ctx) {
                        let mut c = ctx.clone();
                        c["panel"] = json!(view);
                        c["panel_view"] = json!(view);
                        self.activate_item(Item::from_entry(&b), c, None);
                        return;
                    }
                }
                self.panel_key(&view.clone(), &view, k, ctx);
            }
            _ => {}
        }
    }

    /// The kind actually drawn for a view: its own, or its fallback's when this client can't render it.
    fn view_kind(&self, view: &str) -> String {
        if view == SAFE_MODE_VIEW { return "panel".into(); }
        let shown = self.shown_view(view).0.unwrap_or_else(|| view.to_string());
        self.ws.entries.get(&format!("view/{shown}")).and_then(|v| v["kind"].as_str()).unwrap_or("").to_string()
    }

    /// View-kind fallback (D29): the view drawn for `view`, and the views its chain visited.
    fn shown_view(&self, view: &str) -> (Option<String>, Vec<String>) {
        let (entries, live) = (&self.ws.entries, &self.ws.live);
        crate::ui_model::view::shown(view, &|id| crate::ui_model::view::merged(id, entries, live),
            &|v| crate::ui_model::view::renders(v, &["terminal", "panel"], panel::SUPPORTED))
    }

    fn open_safe_chooser(&mut self) {
        self.safe_selected=self.ws.safe_mode_allow.clone();
        self.pstate.remove("@popup");
        self.overlay=Some(Overlay::Popup { view:SAFE_MODE_VIEW.into(),width:76,ctx:json!({}) });
    }

    /// Recovery UI is the shared model's content (`ui_model::recovery`), never published or supplied by an extension.
    fn safe_chooser_value(&self) -> Value {
        recovery::safe_chooser(&self.ws, &self.safe_selected)
    }

    /// A panel's value: the view entry with its live content merged over it.
    fn panel_value(&self, view: &str) -> Arc<Value> {
        if view==SAFE_MODE_VIEW { return Arc::new(self.safe_chooser_value()); }
        if let Some(value) = self.panel_values.lock().unwrap().get(view).cloned() { return value; }
        // A pane keeps its view id; its content may come from the view's fallback (D29).
        let (shown, chain) = self.shown_view(view);
        let (mut v, mut deps) = compose::resolve(shown.as_deref().unwrap_or(view), &self.ws.entries, &self.ws.live);
        deps.extend(chain);
        self.panel_dependencies.lock().unwrap().insert(view.into(), deps);
        window::mark_current(&mut v, self.window.shown(&self.ws).as_deref());
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
        // The focused pane is a full panel: its own pages, filter, facets and sort (core-protocol.md, list_detail).
        let focused_detail = container.detail_focus.then(|| panel::selected_detail(&value, &container)).flatten();
        if let Some((id, detail)) = focused_detail.clone() {
            panel::sync_detail(&mut container, &id);
            let dst = container.detail.get_or_insert_with(Box::default);
            if pane_key(&detail, dst, &k, true) { self.pstate.insert(key.into(), container); return true; }
            if k.code == KeyCode::Char('/') && dst.editing.is_none() { self.pstate.insert(key.into(), container); self.filtering = Some(key.into()); self.filter_detail = true; return true; }
        } else if pane_key(&value, &mut container, &k, false) {
            container.detail_focus = false;
            self.pstate.insert(key.into(), container);
            return true;
        }
        let pages = panel::page_titles(&value).len();
        // Typing in a composer: every key edits the draft (below), none switches pages, filters or searches.
        let typing = container.editing.is_some();
        // An active search: n/N step through matches, Esc ends it (feature search).
        if !typing && !container.search.is_empty() && matches!(k.code, KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc) {
            if k.code == KeyCode::Esc { container.search.clear(); self.message.clear(); self.pstate.insert(key.into(), container); }
            else { self.pstate.insert(key.into(), container); self.search_step(key, view, if k.code == KeyCode::Char('n') { 1 } else { -1 }); }
            return true;
        }
        if !typing && pages>0 && matches!(k.code,KeyCode::Char('[')|KeyCode::Char(']')) {
            container.page=if k.code==KeyCode::Char(']'){(container.page+1)%pages}else{(container.page+pages-1)%pages};container.sel=0;container.top=0;container.detail_focus=false;
            self.pstate.insert(key.into(),container);return true;
        }
        if let Some((id,_))=panel::selected_detail(&value,&container) {
            if !typing && matches!(k.code,KeyCode::Tab|KeyCode::BackTab) {
                panel::sync_detail(&mut container,&id);container.detail_focus=!container.detail_focus;self.pstate.insert(key.into(),container);return true;
            }
        }
        if !typing && k.code==KeyCode::Char('/') {
            // `/` searches panels that ask for it (`search: true`), else filters.
            container.detail_focus=false;container.searching=value["search"]==true;
            if container.searching { container.search.clear(); container.search_hit=0; }
            self.pstate.insert(key.into(),container);self.filtering=Some(key.into());self.filter_detail=false;return true;
        }
        let selected=panel::selected_detail(&value,&container);
        let in_detail=container.detail_focus && selected.is_some();
        let (value,mut st)=if in_detail {let(id,v)=selected.unwrap();panel::sync_detail(&mut container,&id);(Arc::new(v),*container.detail.take().unwrap_or_default())}else{(value,container.clone())};
        let was_scrolling=st.manual_scroll;st.manual_scroll=false;
        let save=|mut root:panel::State,state:panel::State|{if in_detail{root.detail=Some(Box::new(state));root}else{state}};
        // A wheel-scrolled pane may have moved its selection out of view: reveal before activating.
        if was_scrolling && matches!(k.code,KeyCode::Enter|KeyCode::Char(' ')){self.pstate.insert(key.into(),save(container,st));return true;}
        let rows = panel::rows(&value, &st);
        // Composer (feature composer): while typing, keys edit its draft (the prefix was handled before).
        if let Some(id) = st.editing.clone() {
            // PgUp/PgDn while typing scroll the conversation above the composer (the editor has no use for them).
            if matches!(k.code, KeyCode::PageUp | KeyCode::PageDown) {
                let page = st.page_step.max(1);
                st.manual_scroll = true;
                st.top = if k.code == KeyCode::PageUp { st.top.saturating_sub(page) } else { st.top.saturating_add(page) };
                self.pstate.insert(key.to_string(), save(container, st));
                return true;
            }
            if let Some(item) = rows.iter().find(|r| r.id == id && panel::kind(&r.item) == "composer").map(|r| r.item.clone()) {
                let name = keys::name(&k).unwrap_or_default();
                let comp = st.composers.entry(id.clone()).or_insert_with(|| Composer::new(&item));
                let mut send = None;
                if name == "Tab" { st.editing = None; st.chip = 1; }
                else {
                    match comp.editor.key(&name) {
                        Outcome::Submit => send = Some(comp.submit_args(&item)),
                        Outcome::Leave => st.editing = None,
                        Outcome::Refused => self.message = "too long: the composer's size limit".into(),
                        _ => {}
                    }
                }
                self.pstate.insert(key.to_string(), save(container, st));
                if let Some(args) = send { self.send_composer(key, &id, &item, args, ctx); }
                return true;
            }
            st.editing = None;
        }
        if let Some(r) = rows.get(st.sel).filter(|r| panel::kind(&r.item) == "composer") {
            let (item, id) = (r.item.clone(), r.id.clone());
            let buttons = crate::ui_model::composer::buttons(&item);
            st.composers.entry(id.clone()).or_insert_with(|| Composer::new(&item));
            let mut handled = true;
            match k.code {
                KeyCode::Left => st.chip = st.chip.saturating_sub(1),
                KeyCode::Right => st.chip = (st.chip + 1).min(buttons.len()),
                KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('i') if st.chip == 0 => { st.editing = Some(id.clone()); }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    let (arg, value) = buttons.get(st.chip - 1).cloned().unwrap_or_default();
                    let pos = self.row_position(key, st.sel);
                    let comp = st.composers.get_mut(&id).unwrap();
                    if arg.is_empty() {
                        let args = comp.submit_args(&item);
                        let empty = comp.editor.string().trim().is_empty();
                        self.pstate.insert(key.to_string(), save(container, st));
                        if empty { self.message = "nothing to send".into(); } else { self.send_composer(key, &id, &item, args, ctx); }
                        return true;
                    } else if let Some(v) = value {
                        comp.values.insert(arg, v);
                    } else if let Some(ctl) = crate::ui_model::composer::controls(&item).into_iter().find(|c| c["arg"] == arg.as_str()) {
                        // A select control: pick a value for this composer only (client-local until Send).
                        let menu = json!({"command": "client.composer", "args": {"action": "set", "panel": key, "row": id, "arg": arg},
                            "value": comp.values.get(&arg).cloned().unwrap_or(Value::Null), "options": ctl["options"].clone()});
                        self.pstate.insert(key.to_string(), save(container, st));
                        self.options_menu(&menu, ctx, pos);
                        return true;
                    }
                }
                _ => handled = false,
            }
            if handled { self.pstate.insert(key.to_string(), save(container, st)); return true; }
        }
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

    /// Send a composer's draft: its command with args, control values and text; the draft is cleared only after
    /// a successful reply (on an error it stays, and the error is shown).
    fn send_composer(&mut self, key: &str, id: &str, item: &Value, args: Value, ctx: Value) {
        let Some(command) = item["command"].as_str().map(String::from) else { self.message = "composer has no command".into(); return };
        let after = Msg::ComposerSent { key: key.into(), row: id.into(), text: args["text"].as_str().unwrap_or("").into() };
        let args = with_arg(&args, "context", ctx);
        self.invoke_then(&command, args, Some(after));
    }

    /// `client.composer {action: set {panel, row, arg, value} | focus | send {set?}}` (client-local).
    fn composer_command(&mut self, args: &Value, ctx: &Value) {
        let key = args["panel"].as_str().or_else(|| ctx["panel"].as_str()).or_else(|| self.leaves.get(self.focus).map(String::as_str)).unwrap_or("").to_string();
        let view = ctx["panel_view"].as_str().unwrap_or(&key).to_string();
        let value = self.panel_value(&view);
        let mut st = self.pstate.get(&key).cloned().unwrap_or_default();
        let rows = panel::rows(&value, &st);
        let Some((at, r)) = rows.iter().enumerate().find(|(_, r)| panel::kind(&r.item) == "composer" && args["row"].as_str().is_none_or(|id| id == r.id)) else {
            self.message = "no composer here".into();
            return;
        };
        let (item, id) = (r.item.clone(), r.id.clone());
        let comp = st.composers.entry(id.clone()).or_insert_with(|| Composer::new(&item));
        for (k, v) in args["set"].as_object().into_iter().flatten() { comp.values.insert(k.clone(), v.clone()); }
        let mut send = None;
        match args["action"].as_str().unwrap_or("focus") {
            "set" => { if let Some(arg) = args["arg"].as_str() { comp.values.insert(arg.into(), args["value"].clone()); } }
            "send" => { if comp.editor.string().trim().is_empty() { self.message = "nothing to send".into(); } else { send = Some(comp.submit_args(&item)); } }
            _ => { st.sel = at; st.editing = Some(id.clone()); st.chip = 0; }
        }
        self.pstate.insert(key.clone(), st);
        if let Some(a) = send { self.send_composer(&key, &id, &item, a, ctx.clone()); }
    }

    /// Composer `autofocus`: when a pane gains focus, typing goes straight into its composer.
    fn autofocus(&mut self) {
        let Some(view) = self.leaves.get(self.focus).cloned() else { return };
        if self.autofocused.as_deref() == Some(view.as_str()) { return; }
        self.autofocused = Some(view.clone());
        if self.view_kind(&view) != "panel" { return; }
        let value = self.panel_value(&view);
        let mut st = self.pstate.get(&view).cloned().unwrap_or_default();
        let rows = panel::rows(&value, &st);
        if let Some((at, r)) = rows.iter().enumerate().find(|(_, r)| panel::kind(&r.item) == "composer" && r.item["autofocus"] == true) {
            st.sel = at;
            st.editing = Some(r.id.clone());
            self.pstate.insert(view, st);
        }
    }

    /// Paste into the multi-line prompt or the composer being typed into; true when it went there.
    fn paste_text(&mut self, text: &str) -> bool {
        if let Some(Overlay::Input { multi, text: line, .. }) = self.overlay.as_mut() {
            match multi {
                Some(ed) => { if ed.insert(text) == Outcome::Refused { self.message = "paste refused: over the prompt's size limit".into(); } }
                None => line.push_str(&text.replace(['\n', '\r'], " ")),
            }
            return true;
        }
        if self.overlay.is_some() { return false; }
        let Some(view) = self.leaves.get(self.focus).cloned() else { return false };
        let Some(st) = self.pstate.get_mut(&view) else { return false };
        let Some(id) = st.editing.clone() else { return false };
        match st.composers.get_mut(&id) {
            Some(c) => { if c.editor.insert(text) == Outcome::Refused { self.message = "paste refused: over the composer's size limit".into(); } true }
            None => false,
        }
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
                    icon_after: false,
                    command: item["command"].as_str().unwrap_or("").to_string(),
                    args: with_arg(&item["args"], "value", o["value"].clone()),
                    // An option with `input` asks for its value instead (e.g. "custom…"), as `text`.
                    input: o["input"].as_str().map(String::from),
                    pick: None,
                    view: None,
                    prefill: o.get("prefill").or_else(|| item.get("prefill")).cloned().unwrap_or(Value::Null),
                    label_context: None,
                    multiline: o["multiline"] == true || item["multiline"] == true,
                    max_bytes: item["max_bytes"].as_u64(),
                }
            })
            .collect();
        if items.is_empty() || item["command"].as_str().is_none() {
            return;
        }
        let at = at.unwrap_or_default();
        self.open_over(Overlay::Menu { items, ctx, sel: 0, at, query: String::new() });
    }

    /// A row's own actions (`menu`), shown like any other menu.
    fn row_menu(&mut self, item: &Value, ctx: Value, at: Option<Position>) {
        let items: Vec<Item> = item["menu"].as_array().into_iter().flatten().map(Item::from_entry).filter(|i| !i.command.is_empty()).collect();
        if items.is_empty() {
            return;
        }
        let at = at.or_else(|| self.hit.leaves.get(self.focus).map(|(_, o, _)| Position { x: o.x + 2, y: o.y + 2 })).unwrap_or_default();
        self.open_over(Overlay::Menu { items, ctx, sel: 0, at, query: String::new() });
    }

    fn on_filter_key(&mut self, k: KeyEvent) {
        let Some(key) = self.filtering.clone() else { return };
        if !self.filter_detail && self.pstate.get(&key).is_some_and(|s| s.searching) { return self.on_search_key(&key, k); }
        let root = self.pstate.entry(key).or_default();
        let st = if self.filter_detail { root.detail.get_or_insert_with(Box::default).as_mut() } else { root };
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

    /// Typing a search (feature search): each change jumps to the first match; Enter keeps it for n/N, Esc ends it.
    fn on_search_key(&mut self, key: &str, k: KeyEvent) {
        let st = self.pstate.entry(key.to_string()).or_default();
        match k.code {
            KeyCode::Esc => { st.search.clear(); st.searching = false; self.filtering = None; self.message.clear(); return; }
            KeyCode::Enter => { st.searching = false; self.filtering = None; return; }
            KeyCode::Backspace => { st.search.pop(); }
            KeyCode::Char(c) => st.search.push(c),
            _ => return,
        }
        st.search_hit = 0;
        let view = self.panel_view_for(key);
        self.search_step(key, &view, 0);
    }

    /// Show search match `search_hit + delta` (wrapping): open the rows around it and select it.
    fn search_step(&mut self, key: &str, view: &str, delta: i64) {
        let value = self.panel_value(view);
        let st = self.pstate.entry(key.to_string()).or_default();
        let hits = panel::search(&value, st, &st.search, panel::SUPPORTED);
        if hits.is_empty() {
            self.message = if st.search.is_empty() { String::new() } else { format!("/{}: no match", st.search) };
            return;
        }
        let n = hits.len() as i64;
        st.search_hit = (st.search_hit as i64 + delta).rem_euclid(n) as usize;
        let (id, open) = &hits[st.search_hit];
        for a in open { st.open.insert(a.clone(), true); }
        if let Some(i) = panel::rows(&value, st).iter().position(|r| r.id == *id) { st.sel = i; st.manual_scroll = false; }
        self.message = format!("/{}  {} of {}  n/N  Esc", st.search, st.search_hit + 1, n);
    }

    /// The view a panel state key shows (the key itself, or the popup's view).
    fn panel_view_for(&self, key: &str) -> String {
        if key == "@popup" { if let Some(Overlay::Popup { view, .. }) = &self.overlay { return view.clone(); } }
        key.to_string()
    }

    /// Typing in a composer or a multi-line prompt: the outer terminal should report Shift-Enter apart from Enter.
    fn typing_lines(&self) -> bool {
        let editing = |key: &str| self.pstate.get(key).is_some_and(|s| s.editing.is_some());
        matches!(&self.overlay, Some(Overlay::Input { multi: Some(_), .. }))
            || (matches!(&self.overlay, Some(Overlay::Popup { .. })) && editing("@popup"))
            || self.leaves.get(self.focus).is_some_and(|v| editing(v))
    }

    /// `doc/settings/tui.terminal_engine`: the built-in engine new and reattached terminal panes use.
    /// An engine name this client doesn't know (a newer setting, a typo) uses the default, and says so once.
    fn terminal_engine(&mut self) -> emulator::EngineKind {
        let named = self.ws.entries.get("doc/settings/tui").and_then(|s| s["terminal_engine"].as_str()).map(String::from);
        match named.as_deref().map(|n| (n, emulator::EngineKind::parse(n))) {
            Some((_, Some(kind))) => kind,
            Some((name, None)) => {
                let note = format!("terminal engine {name:?} is not built into this client; using {}", emulator::EngineKind::DEFAULT.name());
                if self.engine_notes.insert(note.clone()) { self.message = note; }
                emulator::EngineKind::DEFAULT
            }
            None => emulator::EngineKind::DEFAULT,
        }
    }

    /// `doc/settings/tui.scrollback_lines`, bounded (see `SCROLLBACK_RANGE`); new and reattached panes use it.
    fn scrollback_lines(&self) -> usize {
        let v = self.ws.entries.get("doc/settings/tui").and_then(|s| s["scrollback_lines"].as_u64()).map(|n| n as usize);
        v.map(|n| n.clamp(*SCROLLBACK_RANGE.start(), *SCROLLBACK_RANGE.end())).unwrap_or(DEFAULT_SCROLLBACK)
    }

    /// Copy text through the user's terminal (OSC 52). Terminals report nothing back: say what was sent.
    fn copy_text(&mut self, t: &str) {
        use base64::Engine;
        use std::io::Write;
        let b64 = base64::engine::general_purpose::STANDARD.encode(t.as_bytes());
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]52;c;{b64}\x07").and_then(|_| out.flush());
        self.message = format!("copy: sent {} characters to the terminal clipboard (OSC 52; some terminals ignore it)", t.chars().count());
    }

    /// The text selected in a terminal view, if any.
    fn selection_text(&mut self, view: &str) -> Option<String> {
        let sel = self.selections.get(view).filter(|s| s.dragged || s.unit != scrollback::Unit::Cell)?.clone();
        let t = self.terms.get_mut(view)?;
        let scrolled = t.emu.scrolled();
        let (a, b) = sel.ordered();
        Some(scrollback::text(&mut t.emu, scrolled, a, b)).filter(|s| !s.is_empty())
    }

    /// A mouse position in a terminal view's text, if it is inside the view.
    fn terminal_pos(&self, view: &str, pos: Position, clamp: bool) -> Option<scrollback::Pos> {
        let (_, _, inner) = self.hit.leaves.iter().find(|(v, _, _)| v == view)?;
        if !clamp && !inner.contains(pos) { return None; }
        let t = self.terms.get(view)?;
        let y = pos.y.clamp(inner.y, inner.bottom().saturating_sub(1)) - inner.y;
        let x = pos.x.clamp(inner.x, inner.right().saturating_sub(1)) - inner.x;
        let scrolled = t.emu.scrolled();
        Some(scrollback::Pos { row: scrollback::top(&t.emu, scrolled) + y as usize, col: x })
    }

    /// A left press in a terminal that vqro handles: start a selection (a double press selects a word, a triple
    /// press a line, each copied at once when `copy_on_select` is on).
    fn selection_press(&mut self, view: &str, pos: Position) {
        let Some(p) = self.terminal_pos(view, pos, false) else { return };
        let now = std::time::Instant::now();
        let count = match &self.last_press {
            Some((at, v, q, n)) if v == view && q.row == p.row && q.col.abs_diff(p.col) <= 1 && now.duration_since(*at) < std::time::Duration::from_millis(400) => n % 3 + 1,
            _ => 1,
        };
        self.last_press = Some((now, view.to_string(), p, count));
        let unit = [scrollback::Unit::Cell, scrollback::Unit::Word, scrollback::Unit::Line][count as usize - 1];
        let mut sel = scrollback::Selection { anchor: p, head: p, unit, ..Default::default() };
        if unit != scrollback::Unit::Cell {
            if let Some(t) = self.terms.get_mut(view) {
                let scrolled = t.emu.scrolled();
                scrollback::widen(&mut t.emu, scrolled, &mut sel);
            }
        }
        self.selections.insert(view.to_string(), sel);
        self.selecting = Some(view.to_string());
    }

    /// The selecting drag moved: extend to the pointer (past the top or bottom edge, scroll a line that way).
    fn selection_drag(&mut self, view: &str, pos: Position) {
        if let (Some((_, _, inner)), Some(t)) = (self.hit.leaves.iter().find(|(v, _, _)| v == view).cloned(), self.terms.get_mut(view)) {
            let at = t.emu.scroll_offset();
            if pos.y < inner.y { t.emu.set_scroll_offset(at + 1); }
            else if pos.y >= inner.bottom() { t.emu.set_scroll_offset(at.saturating_sub(1)); }
        }
        let Some(p) = self.terminal_pos(view, pos, true) else { return };
        let Some(mut sel) = self.selections.get(view).cloned() else { return };
        sel.head = p;
        sel.dragged = true;
        if sel.unit != scrollback::Unit::Cell {
            if let Some(t) = self.terms.get_mut(view) {
                let scrolled = t.emu.scrolled();
                scrollback::widen(&mut t.emu, scrolled, &mut sel);
            }
        }
        self.selections.insert(view.to_string(), sel);
    }

    /// The selecting press ended: a click (no drag, single) clears the selection; otherwise it stays shown and is
    /// copied when `copy_on_select` is on (default).
    fn selection_release(&mut self, view: &str) {
        self.selecting = None;
        let ctrl = self.link_click.take().as_deref() == Some(view);
        let Some(sel) = self.selections.get_mut(view) else { return };
        if !sel.dragged && sel.unit == scrollback::Unit::Cell {
            let at = sel.anchor;
            self.selections.remove(view);
            if ctrl {
                self.open_link(view, at);
            }
            return;
        }
        let copy = self.ws.entries.get("doc/settings/tui").and_then(|s| s["copy_on_select"].as_bool()).unwrap_or(true);
        if copy {
            if let Some(t) = self.selection_text(view) { self.copy_text(&t); }
        }
    }

    fn leave_scroll_mode(&mut self) {
        if let Some(m) = self.scroll_mode.take() {
            if let Some(t) = self.terms.get_mut(&m.view) { t.emu.set_scroll_offset(0); }
            self.message.clear();
        }
    }

    /// Keys in scroll mode: arrows/j/k a line, PgUp/PgDn (b/f, Space) a page, g/G (Home/End) the oldest/newest
    /// line, `/` search, n/N the next match up/down, y copy the selection, Esc or q leave. Other keys do nothing.
    fn on_scroll_key(&mut self, k: KeyEvent) {
        let Some(mut m) = self.scroll_mode.take() else { return };
        let Some(t) = self.terms.get_mut(&m.view) else { return };
        let scrolled = t.emu.scrolled();
        if m.typing {
            match k.code {
                KeyCode::Esc => { m.typing = false; m.query.clear(); m.hits.clear(); }
                KeyCode::Enter => m.typing = false,
                KeyCode::Backspace => { m.query.pop(); }
                KeyCode::Char(c) if !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => m.query.push(c),
                _ => {}
            }
            if !matches!(k.code, KeyCode::Enter | KeyCode::Esc) {
                // Each change searches again and shows the nearest match at or above the bottom of the view.
                m.hits = scrollback::search(&mut t.emu, scrolled, &m.query);
                let bottom = scrollback::top(&t.emu, scrolled) + usize::from(t.emu.rows()) - 1;
                m.at = m.hits.iter().rposition(|h| h.row <= bottom).unwrap_or(m.hits.len().saturating_sub(1));
                if let Some(h) = m.hits.get(m.at) { scrollback::reveal(&mut t.emu, scrolled, h.row); }
            }
        } else {
            let rows = usize::from(t.emu.rows());
            let at = t.emu.scroll_offset();
            let len = t.emu.scrollback_len();
            let mut set = |to: usize| t.emu.set_scroll_offset(to.min(len));
            match (k.code, keys::name(&k).unwrap_or_default().as_str()) {
                (KeyCode::Esc, _) | (_, "q") => { self.scroll_mode = Some(m); return self.leave_scroll_mode(); }
                (KeyCode::Up, _) | (_, "k") => set(at + 1),
                (KeyCode::Down, _) | (_, "j") => set(at.saturating_sub(1)),
                (KeyCode::PageUp, _) | (_, "b") | (_, "C-b") => set(at + rows.saturating_sub(1)),
                (KeyCode::PageDown, _) | (_, "f") | (_, "C-f") | (_, "Space") => set(at.saturating_sub(rows.saturating_sub(1))),
                (KeyCode::Home, _) | (_, "g") => set(len),
                (KeyCode::End, _) | (_, "G") => set(0),
                (_, "/") | (_, "?") => { m.typing = true; m.query.clear(); m.hits.clear(); }
                (_, "n") | (_, "N") if !m.hits.is_empty() => {
                    let n = m.hits.len();
                    m.at = if keys::name(&k).as_deref() == Some("n") { (m.at + n - 1) % n } else { (m.at + 1) % n };
                    let row = m.hits[m.at].row;
                    scrollback::reveal(&mut t.emu, scrolled, row);
                }
                (_, "y") => {
                    let view = m.view.clone();
                    self.scroll_mode = Some(m);
                    match self.selection_text(&view) {
                        Some(text) => self.copy_text(&text),
                        None => self.message = "copy: select text with the mouse first".into(),
                    }
                    return;
                }
                _ => {}
            }
        }
        self.scroll_mode = Some(m);
        self.scroll_status();
    }

    /// The status line in scroll mode: where the view is, and the search's count.
    fn scroll_status(&mut self) {
        let Some(m) = &self.scroll_mode else { return };
        let Some(t) = self.terms.get(&m.view) else { return };
        let (at, len) = (t.emu.scroll_offset(), t.emu.scrollback_len());
        let found = if m.query.is_empty() { String::new() }
            else if m.hits.is_empty() { format!("  /{}: no match", m.query) }
            else { format!("  /{}  {} of {}  n/N", m.query, m.hits.len() - m.at, m.hits.len()) };
        self.message = if m.typing { found.trim_start().to_string() }
            else { format!("scroll ↑{at} of {len}{found}  ·  ↑↓ PgUp/PgDn g/G  / search  y copy  Esc") };
    }

    /// Pass a program's clipboard writes, bells and notifications on to the outer terminal, and say what happened:
    /// the terminal reports nothing back. A bell in a pane that isn't focused also shows its name (once a second).
    fn pass_effects(&mut self, view: &str, effects: Vec<Effect>) {
        use std::io::Write;
        let mut out = std::io::stdout();
        for e in &effects {
            let _ = out.write_all(&effect_bytes(e));
        }
        let _ = out.flush();
        let focused = self.leaves.get(self.focus).is_some_and(|v| v == view);
        for e in effects {
            match e {
                Effect::Clipboard(_, data) => self.message = format!("{}: sent {} bytes to the terminal clipboard (OSC 52; some terminals ignore it)", self.view_title(view), data.len() / 4 * 3),
                Effect::Bell if !focused => {
                    let now = std::time::Instant::now();
                    if self.bells.get(view).is_none_or(|at| now.duration_since(*at) >= std::time::Duration::from_secs(1)) {
                        self.bells.insert(view.to_string(), now);
                        self.message = format!("bell in {}", self.view_title(view));
                    }
                }
                _ => {}
            }
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
        keys::display_labels(text, self.ws.entries.get("doc/settings/tui").and_then(|s|s["key_labels"].as_str()))
    }

    /// The prefix key in canonical form; Ctrl-g if the setting is missing or unusable.
    fn prefix(&self) -> String {
        keys::prefix(self.ws.entries.get("doc/settings/tui").and_then(|s| s["prefix"].as_str()))
    }

    /// The binding a key (canonical name) runs on the focused pane, if any (`ui_model::workspace`).
    fn binding_for(&self, key: &str, ctx: &Value) -> Option<Value> { self.ws.binding_for(key, ctx) }

    /// A binding for this key that applies only on other panes (its `when` does not hold here).
    fn bound_elsewhere(&self, key: &str, ctx: &Value) -> Option<Value> { self.ws.bound_elsewhere(key, ctx) }

    /// The prefix menu, including recovery whatever the keymap: the shared model's content.
    fn prefix_items(&self) -> Vec<PrefixRow> {
        recovery::prefix_rows(&self.ws, &self.ctx_leaf(self.focus), &|key| self.key_labels(key))
    }

    fn binding_label(&self, b: &Value) -> String { self.ws.binding_label(b) }

    fn on_prefix_key(&mut self, k: KeyEvent, is_prefix: bool) {
        if is_prefix {
            // The prefix twice sends it to the program.
            self.overlay = None;
            let (app_cursor, ext) = self.leaves.get(self.focus).and_then(|v| self.terms.get(v)).map(|t| (t.emu.application_cursor(), t.modes.extended())).unwrap_or_default();
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
            self.arm_repeat(&b);
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
    /// A `repeat` binding that ran (and opened nothing) keeps its keys live without the prefix for REPEAT_WINDOW.
    fn arm_repeat(&mut self, b: &Value) {
        if b["repeat"] == true && self.overlay.is_none() {
            self.repeat_until = Some(std::time::Instant::now() + REPEAT_WINDOW);
        }
    }

    fn run_binding(&mut self, b: &Value) {
        let ctx = self.ctx_leaf(self.focus);
        let at = self.hit.leaves.get(self.focus).map(|(_, o, _)| Position { x: o.x + 1, y: o.y + 1 });
        self.activate_item(Item::from_entry(b), ctx, at);
    }

    fn overlay_len(&self) -> usize {
        match &self.overlay {
            Some(Overlay::Prefix { .. }) => self.prefix_items().len(),
            Some(Overlay::Palette { .. }) => self.palette_items().len(),
            Some(Overlay::Pick { .. }) | Some(Overlay::Menu { .. }) => self.pick_matches().len(),
            Some(Overlay::Input { .. }) | Some(Overlay::Popup { .. }) | None => 0,
        }
    }

    /// One line per resource offered by the resource picker, in `resources` order.
    fn pick_lines(&self) -> Vec<String> {
        self.ws.resources
            .values()
            .map(|r| format!("{}  {}  {}", r["id"].as_str().unwrap_or(""), r["name"].as_str().or_else(|| r["argv"][0].as_str()).unwrap_or(""), r["state"].as_str().unwrap_or("")))
            .collect()
    }

    /// The open pick-one list (menu or resource picker): every row's text and the current query.
    fn pick_list(&self) -> Option<(Vec<String>, &str)> {
        match &self.overlay {
            Some(Overlay::Menu { items, query, .. }) => Some((items.iter().map(|i| i.label.clone()).collect(), query)),
            Some(Overlay::Pick { query, .. }) => Some((self.pick_lines(), query)),
            _ => None,
        }
    }

    /// Indices of the rows the query keeps (all of them for a short list or an empty query).
    fn pick_matches(&self) -> Vec<usize> {
        self.pick_list().map(|(rows, query)| filter_rows(&rows, query)).unwrap_or_default()
    }

    /// Whether the open pick-one list is long enough to type into.
    fn pick_searchable(&self) -> bool {
        self.pick_list().is_some_and(|(rows, _)| rows.len() > SEARCHABLE)
    }

    fn pick_query(&mut self) -> Option<&mut String> {
        match self.overlay.as_mut()? {
            Overlay::Menu { query, .. } | Overlay::Pick { query, .. } => Some(query),
            _ => None,
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
        let resources: Vec<String> = self.ws.resources.keys().cloned().collect();
        let prefix = self.prefix_items();
        // `sel` of a menu or picker counts the rows its query keeps.
        let matches = self.pick_matches();
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
                let name = if self.ws.commands.contains_key(&typed) { Some(typed) } else { palette.get(sel).cloned() };
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
                let item = self.ws.entries.iter().filter(|(k, v)| k.starts_with("menu/") && v["command"] == name.as_str() && self.visible(v)).map(|(_, v)| Item::from_entry(v)).next();
                match item {
                    Some(item) => self.activate_item(item, ctx, None),
                    None => self.run_command(&name, Value::Null, ctx, None),
                }
            }
            Some(Overlay::Menu { items, sel, ctx, at, .. }) => {
                if let Some(item) = matches.get(sel).and_then(|&i| items.get(i)).cloned() {
                    self.activate_item(item, ctx, Some(at));
                }
            }
            Some(Overlay::Input { text, item, ctx, .. }) => {
                let args = with_arg(&item.args, "text", json!(text));
                self.run_command(&item.command, args, ctx, None);
            }
            Some(Overlay::Popup { .. }) => {}
            Some(Overlay::Pick { sel, item, ctx, .. }) => {
                if let Some(r) = matches.get(sel).and_then(|&i| resources.get(i)) {
                    let args = with_arg(&item.args, "picked", json!(r));
                    self.run_command(&item.command, args, ctx, None);
                }
            }
            None => {}
        }
    }

    fn on_overlay_key(&mut self, k: KeyEvent) {
        // A multi-line prompt (feature multiline): the shared editor; Enter sends, Alt-Enter/Ctrl-J add a line.
        if let Some(Overlay::Input { multi: Some(ed), item, ctx, .. }) = self.overlay.as_mut() {
            let Some(name) = keys::name(&k) else { return };
            match ed.key(&name) {
                Outcome::Submit => {
                    let (args, ctx, command) = (with_arg(&item.args, "text", json!(ed.string())), ctx.clone(), item.command.clone());
                    self.overlay = None;
                    self.restore_under();
                    self.run_command(&command, args, ctx, None);
                }
                Outcome::Leave => { self.overlay = None; self.restore_under(); }
                Outcome::Refused => self.message = "too long: the prompt's size limit".into(),
                _ => {}
            }
            return;
        }
        // A long menu or picker: typing filters it, Backspace edits, Esc clears the query first.
        if self.pick_list().is_some() {
            let searchable = self.pick_searchable();
            let plain = !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
            let edit = match k.code {
                KeyCode::Esc => self.pick_query().is_some_and(|q| !q.is_empty()).then(String::new),
                KeyCode::Backspace if searchable => self.pick_query().map(|q| { let mut q = q.clone(); q.pop(); q }),
                KeyCode::Char(c) if searchable && plain => self.pick_query().map(|q| format!("{q}{c}")),
                _ => None,
            };
            if let Some(text) = edit {
                if let Some(q) = self.pick_query() {
                    *q = text;
                }
                if let Some(sel) = self.overlay_sel() {
                    *sel = 0;
                }
                return;
            }
            if k.code == KeyCode::Enter && self.overlay_len() == 0 && self.pick_query().is_some_and(|q| !q.is_empty()) {
                return; // nothing matches: keep the menu and the text, so it can be corrected
            }
        }
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

    /// Whether a contributed entry is live (`ui_model::workspace`).
    fn visible(&self, e: &Value) -> bool { self.ws.visible(e) }

    /// The binding a gesture runs in a place: the same precedence as keys.
    fn gesture(&self, key: &str, place: &str) -> Option<Value> { self.ws.gesture(key, place) }

    /// Menu items for a place, filtered by `when` against the context, in `order`.
    fn menu_items(&self, place: &str, ctx: &Value) -> Vec<Item> {
        self.ws.menu_entries(place, ctx).iter().map(Item::from_entry).collect()
    }

    /// Context for a pane: what commands invoked on it are told.
    fn ctx_leaf(&self, idx: usize) -> Value {
        let Some((view, _, _)) = self.hit.leaves.get(idx) else { return json!({}) };
        let (layout, version) = self.active_layout_key();
        let v = self.ws.entries.get(&format!("view/{view}")).cloned().unwrap_or(Value::Null);
        let resource = v["resource"].as_str();
        let live = resource.and_then(|r| self.ws.resources.get(r)).map(|r| r["state"] == "live").unwrap_or(false);
        // Lets `when` select by what the pane shows, e.g. {"resource_kind": "agent"}.
        let resource_kind = resource.and_then(|r| self.ws.resources.get(r)).map(|r| r["kind"].clone()).unwrap_or(Value::Null);
        let path = self.hit.leaf_paths.get(idx).cloned().unwrap_or_default();
        let mut ctx = json!({
            "layout": layout, "layout_version": version, "path": path,
            "view": view, "kind": v["kind"], "resource": resource, "resource_kind": resource_kind, "live": live, "title": self.view_title(view),
        });
        // Drawn through its fallback (D29): commands still address the pane's own view.
        if let Some(shown) = self.shown_view(view).0.filter(|s| s != view) { ctx["shown_view"] = json!(shown); }
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

    /// A pane's context plus the link under a screen position, if any: `link` `{kind, text, line?, column?}` and
    /// `link_kind`, so menu items can match `when: {"link_kind": "url"}`.
    fn ctx_link(&mut self, idx: usize, pos: Position) -> Value {
        let mut ctx = self.ctx_leaf(idx);
        let view = self.hit.leaves.get(idx).map(|(v, _, _)| v.clone()).unwrap_or_default();
        if let Some(link) = self.terminal_pos(&view, pos, false).and_then(|p| self.link_at(&view, p)) {
            ctx["link"] = link.context();
            ctx["link_kind"] = json!(link.kind);
        }
        ctx
    }

    /// The link (`ui_model::links`) at a cell of a terminal's text, read from its logical line (rows the terminal
    /// wrapped are joined, so a long URL is whole).
    fn link_at(&mut self, view: &str, p: scrollback::Pos) -> Option<crate::ui_model::links::Link> {
        let t = self.terms.get_mut(view)?;
        let scrolled = t.emu.scrolled();
        // An OSC 8 hyperlink the program put on this cell (engines that keep links) wins over text detection.
        let top = scrollback::top(&t.emu, scrolled);
        if let Some(uri) = p.row.checked_sub(top).and_then(|r| u16::try_from(r).ok()).and_then(|r| t.emu.hyperlink_at(r, p.col)) {
            return Some(crate::ui_model::links::Link { kind: "url", end: uri.chars().count(), text: uri, start: 0, line: None, column: None });
        }
        let (first, last) = scrollback::line(&mut t.emu, scrolled, p.row);
        let (mut text, mut cells) = (String::new(), Vec::new());
        for at in first.row..=last.row {
            let Some((chars, starts, _)) = scrollback::row(&mut t.emu, scrolled, at) else { continue };
            for (c, col) in chars.into_iter().zip(starts) {
                text.push(c);
                cells.push((at, col, c));
            }
        }
        let i = cells.iter().rposition(|&(r, col, c)| r == p.row && col <= p.col && p.col < col + icons::width(&c.to_string()).max(1))?;
        crate::ui_model::links::at(&text, i)
    }

    /// Ctrl-click (no drag) in a terminal: run the `mouse.link` pane gesture when a link is at that cell.
    fn open_link(&mut self, view: &str, at: scrollback::Pos) {
        let Some(b) = self.gesture("mouse.link", "pane") else { return };
        let Some(link) = self.link_at(view, at) else { return };
        let Some(idx) = self.hit.leaves.iter().position(|(v, _, _)| v == view) else { return };
        let mut ctx = self.ctx_leaf(idx);
        ctx["link"] = link.context();
        ctx["link_kind"] = json!(link.kind);
        self.run_command(b["command"].as_str().unwrap_or(""), b["args"].clone(), ctx, None);
    }

    fn active_layout_key(&self) -> (Option<String>, Option<u64>) {
        let name = self.window.shown(&self.ws);
        let version = name.as_ref().and_then(|n| self.ws.versions.get(&format!("layout/{n}")).copied());
        (name, version)
    }

    fn view_title(&self, view: &str) -> String {
        let v = self.ws.entries.get(&format!("view/{view}")).cloned().unwrap_or(Value::Null);
        if let Some(t) = v["title"].as_str() {
            return t.to_string();
        }
        let r = v["resource"].as_str().and_then(|r| self.ws.resources.get(r));
        r.and_then(resource_name).unwrap_or_else(|| view.to_string())
    }

    /// Run an item: prompt or pick first if it asks to.
    fn activate_item(&mut self, item: Item, ctx: Value, at: Option<Position>) {
        if let Some(prompt) = item.input.clone() {
            // Input is empty unless its author explicitly supplies text or a context source.
            let text = item.prefill.as_str().or_else(|| item.prefill["context"].as_str().and_then(|field| ctx[field].as_str())).unwrap_or_default().to_string();
            let multi = item.multiline.then(|| Editor::new(&text, item.max_bytes, None));
            self.open_over(Overlay::Input { prompt, text, item, ctx, multi });
        } else if item.pick.as_deref() == Some("resource") {
            self.open_over(Overlay::Pick { sel: 0, item, ctx, query: String::new() });
        } else {
            self.run_command(&item.command, item.args.clone(), ctx, at);
        }
    }

    /// Replace each tab's title with the name it shows (`ui_model::tabbar::tab_title`: program titles).
    fn shown_tab_titles(&self, node: &mut Value) {
        let setting = self.ws.entries.get(crate::ui_model::tabbar::ENTRY);
        if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
            children.iter_mut().for_each(|c| self.shown_tab_titles(c));
        }
        let Some(tabs) = node.get_mut("tabs").and_then(Value::as_array_mut) else { return };
        for tab in tabs {
            let resource = tab["node"]["view"].as_str()
                .and_then(|v| self.ws.entries.get(&format!("view/{v}")))
                .and_then(|v| v["resource"].as_str())
                .and_then(|r| self.ws.resources.get(r));
            if let Some(title) = tab["title"].as_str() {
                tab["title"] = json!(crate::ui_model::tabbar::tab_title(setting, title, resource));
            }
            self.shown_tab_titles(&mut tab["node"]);
        }
    }

    /// Mark inactive tabs (`tab["mark"]`, `ui_model::tabbar::tab_mark`) and remember what the shown tabs' resources
    /// report now. Client-local: nothing is written.
    fn tab_marks(&mut self, node: &mut Value) {
        if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
            children.iter_mut().for_each(|c| self.tab_marks(c));
        }
        let active = node["active"].as_u64().unwrap_or(0) as usize;
        let Some(tabs) = node.get_mut("tabs").and_then(Value::as_array_mut) else { return };
        let setting = self.ws.entries.get(crate::ui_model::tabbar::ENTRY).cloned();
        let shown = active.min(tabs.len().saturating_sub(1));
        for (i, tab) in tabs.iter_mut().enumerate() {
            let rids: Vec<String> = views_of(&tab["node"]).iter()
                .filter_map(|v| self.ws.entries.get(&format!("view/{v}"))?["resource"].as_str().map(String::from))
                .filter(|r| self.ws.resources.contains_key(r))
                .collect();
            if i == shown {
                for r in &rids {
                    self.seen.insert(r.clone(), crate::ui_model::tabbar::seen_of(&self.ws.resources[r]));
                    self.ws.reached.remove(r);
                }
            } else {
                for r in &rids {  // first sight: nothing happened yet
                    self.seen.entry(r.clone()).or_insert_with(|| crate::ui_model::tabbar::seen_of(&self.ws.resources[r]));
                }
                let now: Vec<&Value> = rids.iter().map(|r| &self.ws.resources[r]).collect();
                let seen: Vec<&Value> = rids.iter().map(|r| &self.seen[r]).collect();
                let reached: Vec<Option<&str>> = rids.iter().map(|r| self.ws.reached.get(r).copied()).collect();
                if let Some(mark) = crate::ui_model::tabbar::tab_mark(setting.as_ref(), &now, &seen, &reached) {
                    tab["mark"] = json!(mark);
                }
            }
            self.tab_marks(&mut tab["node"]);
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
                self.open_over(Overlay::Menu { items, ctx, sel: 0, at, query: String::new() });
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
                let root = self.pstate.entry(key.clone()).or_default();
                // `pane`: "list" or "detail"; by default the focused one.
                let detail = match args["pane"].as_str() { Some("detail") => true, Some("list") => false, _ => root.detail_focus };
                let selected = if detail { panel::selected_detail(&value, root) } else { None };
                let (value, st) = match &selected {
                    Some((id, v)) => { panel::sync_detail(root, id); (Arc::new(v.clone()), root.detail.get_or_insert_with(Box::default).as_mut()) }
                    None => (value, root),
                };
                match args["action"].as_str().unwrap_or("") {
                    "expand_all" | "collapse_all" => {
                        let open = args["action"] == "expand_all";
                        for id in panel::parent_ids(&value, st) {
                            st.open.insert(id, open);
                        }
                    }
                    "filter" => { self.filtering = Some(key); self.filter_detail = selected.is_some(); }
                    "clear_filter" => st.filter.clear(),
                    "search" => { st.searching = true; st.search.clear(); st.search_hit = 0; self.filtering = Some(key); }
                    "search_next" | "search_prev" => { let d = if args["action"] == "search_next" { 1 } else { -1 }; self.search_step(&key, &view, d); }
                    // Navigation for pane-scoped keys (D29): j/k, Home/End, next/previous matching row, expand.
                    action @ ("down" | "up" | "first" | "last" | "end" | "next" | "prev" | "toggle") => {
                        let rows = panel::rows(&value, st);
                        st.manual_scroll = false;
                        match action {
                            "down" => st.sel = panel::step(&rows, st.sel, 1),
                            "up" => st.sel = panel::step(&rows, st.sel, -1),
                            "first" => st.sel = panel::first(&rows),
                            "last" => st.sel = panel::last(&rows),
                            "end" => { st.sel = rows.iter().rposition(|r| panel::selectable(&r.item) && !r.pinned && !r.bottom).unwrap_or(st.sel); st.follow_end = None; }
                            "next" | "prev" => if let Some(i) = panel::find_row(&rows, st.sel, action == "next", &args["match"]) { st.sel = i; },
                            _ => if let Some(r) = rows.get(st.sel).filter(|r| r.has_children) { st.open.insert(r.id.clone(), !r.expanded); },
                        }
                    }
                    "activate" => { let (key, view) = (key.clone(), view.clone()); self.panel_key(&key, &view, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), ctx.clone()); }
                    "facet" => if let Some(k) = args["key"].as_str() { panel::choose_facet(&value, st, k, args["value"].as_str()); },
                    "sort" => panel::choose_sort(&value, st, args["key"].as_str()),
                    other => self.message = format!("client.panel: unknown action {other}"),
                }
            }
            "client.copy" => {
                // Copy `text`, or the selected row's `copy` (else its text), through the terminal (OSC 52). The
                // terminal reports nothing back, so the notice says what was sent, not that it arrived.
                let text = args["text"].as_str().map(String::from).or_else(|| {
                    let key = ctx["panel"].as_str().map(String::from).or_else(|| self.hit.leaves.get(self.focus).map(|l| l.0.clone()))?;
                    if self.view_kind(&key) != "panel" { return None; }
                    let view = ctx["panel_view"].as_str().map(String::from).unwrap_or_else(|| key.clone());
                    let value = self.panel_value(&view);
                    let st = self.pstate.get(&key).cloned().unwrap_or_default();
                    let item = panel::rows(&value, &st).get(st.sel)?.item.clone();
                    item["copy"].as_str().or(item["text"].as_str()).map(String::from)
                });
                // A focused terminal: its selection.
                let text = text.or_else(|| {
                    let view = self.leaves.get(self.focus)?.clone();
                    self.selection_text(&view)
                });
                match text {
                    Some(t) => self.copy_text(&t),
                    None => self.message = "copy: nothing selected".into(),
                }
            }
            "client.scroll" => {
                // Scroll mode for the focused terminal (keys read its history; `/` searches it).
                let Some(view) = self.leaves.get(self.focus).cloned().filter(|v| self.view_kind(v) == "terminal" && self.terms.contains_key(v)) else {
                    self.message = "scroll: focus a terminal first".into();
                    return;
                };
                self.scroll_mode = Some(ScrollMode { view, query: String::new(), typing: args["action"] == "search", hits: Vec::new(), at: 0 });
                self.scroll_status();
            }
            "client.palette" => self.overlay = Some(Overlay::Palette { query: String::new(), sel: 0 }),
            "client.focus" => self.focus_to(args["dir"].as_str().unwrap_or("next")),
            "client.tab" => self.tab_to(&args["to"], &ctx),
            "client.safe_mode" => {
                if self.ws.safe_mode { self.open_safe_chooser(); } else { self.invoke("vqro.safe_mode",json!({"on":true})); }
            }
            "client.safe_mode.toggle" => {
                if let Some(name)=args["name"].as_str() { if !self.safe_selected.remove(name) { self.safe_selected.insert(name.to_string()); } }
            }
            "client.safe_mode.apply" => {
                let selected=self.safe_selected.clone(); let names:Vec<_>=self.ws.entries.keys().filter_map(|key|key.strip_prefix("extension/").map(String::from)).collect();
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
            // Show a layout in this window only; without a name, the shared default.
            "client.layout" => match self.window.client_layout(&self.ws, &args) {
                Ok(true) => self.window_changed(),
                Ok(false) => {}
                Err(message) => self.message = message,
            },
            "client.composer" => self.composer_command(&args, &ctx),
            "client.none" => {}
            // Never sent to the core: a client that doesn't know one says so.
            other if other.starts_with("client.") => self.message = format!("{other}: not supported by this client"),
            _ => {
                // Every invocation names this window's layout (palette, status bar and popups too).
                let args = with_arg(&args, "context", self.window.invocation_context(&self.ws, ctx));
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

    /// `client.focus {dir}`: next/prev cycle panes; left/right/up/down go to the adjacent pane; last goes back to
    /// the pane focused before this one.
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
            "last" => match self.focus_last.as_ref().and_then(|v| self.leaves.iter().position(|l| l == v)) {
                Some(i) => i,
                None => return self.message = "no other pane was focused here".into(),
            },
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
        match self.ws.entries.get("doc/settings/tui").and_then(|s| s["icons"].as_str()) {
            Some("nerd") => true,
            Some("text") => false,
            _ => {
                std::env::var("VQRO_NERD_FONT").map(|v| v == "1").unwrap_or(false)
                    || matches!(std::env::var("TERM_PROGRAM").as_deref(), Ok("ghostty") | Ok("WezTerm"))
            }
        }
    }

    fn setting(&self, key: &str) -> bool {
        self.ws.entries.get("doc/settings/tui").and_then(|s| s[key].as_bool()).unwrap_or(true)
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

        // Dragging a tab chip: released over another chip of the same bar runs the `mouse.drag` tab gesture with
        // the dragged tab's context plus `drop_index` (core-protocol.md, `binding/<id>`).
        if let Some((path, from)) = self.tab_press.clone() {
            match m.kind {
                MouseEventKind::Drag(MouseButton::Left) => return false,
                MouseEventKind::Up(_) => {
                    self.tab_press = None;
                    let drop = self.hit.tabs.iter().find(|(p, _, r)| *p == path && r.contains(pos)).map(|(_, i, _)| *i);
                    if let (Some(to), Some(b)) = (drop.filter(|to| *to != from), self.gesture("mouse.drag", "tab")) {
                        let mut ctx = self.ctx_tab(&path, from);
                        ctx["drop_index"] = json!(to);
                        self.run_command(b["command"].as_str().unwrap_or(""), b["args"].clone(), ctx, Some(pos));
                    }
                    return true;
                }
                _ => self.tab_press = None,
            }
        }

        // A selecting drag in a terminal gets the rest of the gesture.
        if let Some(view) = self.selecting.clone() {
            match m.kind {
                MouseEventKind::Drag(MouseButton::Left) => { self.selection_drag(&view, pos); return true; }
                MouseEventKind::Up(_) => { self.selection_release(&view); return true; }
                _ => self.selecting = None,
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
                        None => {
                            self.select_tab(&path, i);
                            if self.gesture("mouse.drag", "tab").is_some() {
                                self.tab_press = Some((path.clone(), i));
                            }
                        }
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
                        let ctx = self.ctx_link(idx, pos);
                        self.run_command(b["command"].as_str().unwrap_or(""), b["args"].clone(), ctx, Some(pos));
                    }
                    return true;
                }
                match self.view_kind(&view).as_str() {
                    "terminal" if inner.contains(pos) => {
                        // A program that asked for the mouse gets it; otherwise (or with Alt or Ctrl) the press selects
                        // text.
                        let force = m.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL);
                        if !force && self.forward_mouse(&view, &m) {
                            self.mouse_owner = Some(view);
                        } else if button == MouseButton::Left {
                            self.link_click = m.modifiers.contains(KeyModifiers::CONTROL).then(|| view.clone());
                            self.selection_press(&view, pos);
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
                        let arrows = self.ws.entries.get("doc/settings/tui").and_then(|s| s["wheel_arrows"].as_bool()).unwrap_or(true);
                        if t.emu.alternate_screen() && arrows {
                            // Full-screen program without mouse support: arrows scroll it.
                            let key = KeyEvent::new(if up { KeyCode::Up } else { KeyCode::Down }, KeyModifiers::NONE);
                            let bytes = keys::encode(&key, t.emu.application_cursor(), keys::Extended::Legacy).unwrap_or_default().repeat(3);
                            if let Some(sid) = t.stream {
                                self.send_input(sid, bytes);
                            }
                            false
                        } else {
                            let cur = t.emu.scroll_offset();
                            let next = if up { cur + 3 } else { cur.saturating_sub(3) };
                            t.emu.set_scroll_offset(next);
                            self.scroll_status();
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
        else {
            // The wheel scrolls the list's view, whatever has the keys: a composer being typed in (agent panes
            // autofocus theirs) would otherwise take Up/Down and the conversation could not be scrolled. Scrolling
            // stops `follow` until a key is pressed; scrolled back to the end, following resumes.
            let _ = (view, ctx);
            let st = self.pstate.entry(key.into()).or_default();
            st.detail_focus = false;
            st.manual_scroll = true;
            st.top = if up { st.top.saturating_sub(3) } else { st.top.saturating_add(3) };
        }
    }

    /// A click in a panel: page tabs, expanders, rows (left activates, right opens the row's menu).
    /// Returns whether it hit something.
    fn panel_click(&mut self, key: &str, view: &str, pos: Position, button: MouseButton, ctx: Value) -> bool {
        // Every click path (inline actions, a row's menu, chips, rows) runs in the panel's context, as keys do.
        let mut ctx = ctx;
        ctx["panel"] = json!(key);
        ctx["panel_view"] = json!(view);
        let Some(root_hits) = self.hit.panels.get(key) else { return false };
        let in_detail=root_hits.detail_area.is_some_and(|r|r.contains(pos));
        let hits=if in_detail {let Some(h)=root_hits.detail.as_deref()else{return false};h}else{root_hits};
        if let Some((_, page)) = hits.pages.iter().find(|(r, _)| r.contains(pos)).copied() {
            let root = self.pstate.entry(key.to_string()).or_default();
            root.detail_focus = in_detail;
            let st = if in_detail { root.detail.get_or_insert_with(Box::default).as_mut() } else { root };
            st.page = page;
            st.sel = 0;
            st.top = 0;
            return true;
        }
        if let Some((_, row, ai)) = hits.actions.iter().find(|(r, _, _)| r.contains(pos)).copied() {
            // An inline row button: its command, in the row's context (the row is selected first).
            let value = self.panel_value(view);
            let root = self.pstate.entry(key.to_string()).or_default();
            root.detail_focus = in_detail;
            let value = if in_detail { panel::selected_detail(&value, root).map(|(_, v)| Arc::new(v)).unwrap_or(value) } else { value };
            let st = if in_detail { root.detail.get_or_insert_with(Box::default).as_mut() } else { root };
            st.sel = row;
            let st = st.clone();
            if let Some(a) = panel::rows(&value, &st).get(row).and_then(|r| r.item["actions"].get(ai).cloned()) {
                // `ctx` names this panel (set above), so client.panel and client.copy act on it.
                if button == MouseButton::Left { self.activate_item(Item::from_entry(&a), ctx, Some(pos)); }
            }
            return true;
        }
        if hits.latest.is_some_and(|r| r.contains(pos)) && !in_detail {
            // "↓ Latest · End": back to the newest row, following again.
            self.run_command("client.panel", json!({"action": "end"}), json!({"panel": key, "panel_view": view}), None);
            if let Some(st) = self.pstate.get_mut(key) { st.manual_scroll = false; }
            return true;
        }
        let facet = hits.facets.iter().find(|(r, _, _)| r.contains(pos)).map(|(_, k, v)| (k.clone(), v.clone()));
        let sort = hits.sort.is_some_and(|r| r.contains(pos));
        if facet.is_some() || sort {
            let value = self.panel_value(view);
            let root = self.pstate.entry(key.to_string()).or_default();
            root.detail_focus = in_detail;
            let selected = if in_detail { panel::selected_detail(&value, root) } else { None };
            let (value, st) = match &selected {
                Some((id, v)) => { panel::sync_detail(root, id); (Arc::new(v.clone()), root.detail.get_or_insert_with(Box::default).as_mut()) }
                None => (value, root),
            };
            match facet { Some((k, v)) => panel::choose_facet(&value, st, &k, v.as_deref()), None => panel::cycle_sort(&value, st) }
            return true;
        }
        let toggle=hits.toggles.iter().any(|(r,_)|r.contains(pos));
        if let Some((_, row, chip)) = hits.chips.iter().find(|(r, _, _)| r.contains(pos)).copied() {
            let root = self.pstate.entry(key.to_string()).or_default();root.detail_focus=in_detail;
            let st=if in_detail{root.detail.get_or_insert_with(Box::default).as_mut()}else{root};
            st.sel = row;
            st.chip = chip;
            // A composer's control (or Send) acts as that control, never as a key for the draft being typed: a click
            // on [steer] while typing must choose steer, not send the draft. Typing goes on after a choice.
            let typing = st.editing.take();
            if button == MouseButton::Left {
                self.panel_key(key, view, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), ctx);
            }
            if let Some(id) = typing {
                let root = self.pstate.entry(key.to_string()).or_default();
                let st = if in_detail { root.detail.get_or_insert_with(Box::default).as_mut() } else { root };
                if st.editing.is_none() { st.editing = Some(id); st.chip = 0; }
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
                // A tree row opens or closes; a folding row (its "▸ N more lines" / "▾ less" line) unfolds or folds.
                let open = if !r.has_children && panel::fold(&r.item).is_some() { !panel::unfolded(r, &st) } else { !r.expanded };
                st.open.insert(r.id.clone(), open);
            }
            self.pstate.insert(key.to_string(), save(container,st));
            return true;
        }
        // A click on a composer's text while typing keeps typing (as a key, Enter would send the draft).
        if st.editing.is_some() && rows.get(i).is_some_and(|r| panel::kind(&r.item) == "composer") { self.pstate.insert(key.to_string(), save(container,st)); return true; }
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
        let col = m.column.saturating_sub(inner.x).min(inner.width.saturating_sub(1));
        let row = m.row.saturating_sub(inner.y).min(inner.height.saturating_sub(1));
        let (mode, encoding) = t.emu.mouse();
        let Some(bytes) = mouse::encode(m, col, row, mode, encoding) else { return false };
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
        let expect = self.ws.versions.get(&key).copied();
        self.apply("resize", json!([{"op": "set", "key": key, "value": root, "expect": expect}]));
    }

    // ---- actions (all through public primitives) ---------------------------------------------

    fn palette_items(&self) -> Vec<String> {
        let q = match &self.overlay {
            Some(Overlay::Palette { query, .. }) => query.split_whitespace().next().unwrap_or("").to_lowercase(),
            _ => String::new(),
        };
        self.ws.commands
            .values()
            .filter(|c| c["available"] == true)
            .filter(|c| q.is_empty() || c["name"].as_str().unwrap_or("").to_lowercase().contains(&q) || c["title"].as_str().unwrap_or("").to_lowercase().contains(&q))
            .filter_map(|c| c["name"].as_str().map(String::from))
            .collect()
    }

    fn invoke(&mut self, name: &str, args: Value) { self.invoke_then(name, args, None) }

    /// Invoke a command; `after` is delivered when it succeeds.
    fn invoke_then(&mut self, name: &str, args: Value, after: Option<Msg>) {
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
                    // Layout before focus: the focused view may live in the layout being shown.
                    if let Some(layout) = window::reply_layout(&name, &r) {
                        let _ = tx.send(Msg::ShowLayout(layout));
                    }
                    if let Some(view) = r["client"]["focus"]["view"].as_str() {
                        let _ = tx.send(Msg::FocusView(view.to_string()));
                    }
                    if let Some(after) = after { let _ = tx.send(after); }
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
        let name = self.window.shown(&self.ws)?;
        let v = self.ws.entries.get(&format!("layout/{name}"))?.clone();
        Some((name, v))
    }

    /// Redraw for another layout in this window: leave zoom, drop previews and `current_when` marks.
    fn window_changed(&mut self) {
        self.unzoom();
        self.overrides.clear();
        self.panel_values.lock().unwrap().clear();
    }

    /// Show `name` in this window only (a reply hint, `client.layout`, this window's reset).
    fn show_layout(&mut self, name: &str) {
        match self.window.show(&self.ws, name) {
            Ok(true) => self.window_changed(),
            Ok(false) => {}
            Err(message) => self.message = message,
        }
    }

    /// Keep this window on an existing layout (after a snapshot or a workspace change).
    fn settle_window(&mut self) {
        match self.window.settle(&self.ws) {
            Settled::Same => {}
            Settled::Took(_) => self.window_changed(),
            Settled::Closed { was, now } => {
                self.message = match now { Some(now) => format!("workspace {was} closed; showing {now}"), None => format!("workspace {was} closed") };
                self.window_changed();
            }
        }
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
        let expect = self.ws.versions.get(&key).copied();
        self.apply("select tab", json!([{"op": "set", "key": key, "value": root, "expect": expect}]));
    }

    // ---- drawing -----------------------------------------------------------------------------

    fn draw(&mut self, f: &mut Frame) {
        // Resolved every frame, so a theme or icon set change shows on the next redraw. Safe mode uses
        // the built-in palette and no icon set, so neither can lock anyone out.
        let pal = if self.ws.safe_mode { Palette::default() } else { Palette::from_theme(self.ws.entries.get(theme::ENTRY)) };
        let icons = Icons::new(if self.ws.safe_mode { None } else { self.ws.entries.get(icons::ENTRY) }, self.nerd_icons());
        let [main, status] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());
        let mut hit = Hit::default();
        // Only a focused terminal's cursor carries its program's shape; anything else gets the user's default.
        self.cursor_style = CursorShape::Default;
        match self.active_layout() {
            Some((_, mut root)) => {
                self.tab_marks(&mut root);
                self.shown_tab_titles(&mut root);
                let tabbar = self.menu_items("tabbar", &json!({}));
                let tabstyle = crate::ui_model::tabbar::Style::from_setting(self.ws.entries.get(crate::ui_model::tabbar::ENTRY));
                match self.zoomed.clone() {
                    None => walk(&root, main, &mut Vec::new(), &self.overrides, &tabbar, &tabstyle, &mut hit, f.buffer_mut(), &pal, &icons),
                    Some(z) => {
                        // Lay out normally off-screen to find the pane, then give it the whole area.
                        let mut scratch = Buffer::empty(main);
                        walk(&root, main, &mut Vec::new(), &self.overrides, &tabbar, &tabstyle, &mut hit, &mut scratch, &pal, &icons);
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
                                walk(&root, main, &mut Vec::new(), &self.overrides, &tabbar, &tabstyle, &mut hit, f.buffer_mut(), &pal, &icons);
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
        let now = self.leaves.get(self.focus).cloned();
        if now.is_some() && now != self.focus_now {
            self.focus_last = std::mem::replace(&mut self.focus_now, now);
        }
        self.autofocus();
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
        } else {
            self.cursor_style = CursorShape::Default;
        }
    }

    /// After a frame: send the cursor shape the focused program asked for (DECSCUSR), only when it changed.
    fn sync_cursor_style(&mut self) {
        if self.cursor_style == self.cursor_sent { return; }
        self.cursor_sent = self.cursor_style;
        let _ = crossterm::execute!(std::io::stdout(), cursor_shape(self.cursor_style));
    }

    fn draw_view(&mut self, f: &mut Frame, view: &str, rect: Rect, focused: bool, pal: &Palette, icons: &Icons) -> Option<(u16, u16)> {
        let border = pal.border(focused);
        let Some(original) = self.ws.entries.get(&format!("view/{view}")).cloned() else {
            f.render_widget(Paragraph::new(format!("view {view} is missing")).block(Block::bordered().border_style(border)), rect);
            return None;
        };
        // Draw the view's fallback when this client can't render it; the pane keeps the original view id.
        let v = match self.shown_view(view).0 {
            Some(shown) if shown != view => self.ws.entries.get(&format!("view/{shown}")).cloned().unwrap_or(original),
            _ => original,
        };
        match v["kind"].as_str().unwrap_or("") {
            "terminal" => {
                let rid = v["resource"].as_str().unwrap_or("").to_string();
                let res = self.ws.resources.get(&rid).cloned();
                let scroll = self.terms.get(view).map(|t| t.emu.scroll_offset()).unwrap_or(0);
                let title = match &res {
                    Some(r) => {
                        let name = v["title"].as_str().map(String::from).or_else(|| resource_name(r)).unwrap_or_else(|| rid.clone());
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
                let lines = self.scrollback_lines();
                let engine = self.terminal_engine();
                let t = self.terms.entry(view.to_string()).or_insert_with(|| Term {
                    resource: rid.clone(),
                    stream: None,
                    emu: Emulator::with_engine(engine, inner.width, inner.height, lines),
                    size: (inner.width, inner.height),
                    attaching: false,
                    closed: None,
                    retry_at: None, modes: Default::default(),
                });
                // Another resource, or another engine chosen in Settings: start over and reattach (the backend's history
                // repaints it). Never while an attach is in flight: its reply belongs to the current state.
                if t.resource != rid || (t.emu.wanted() != engine && !t.attaching) {
                    if let Some(sid) = t.stream.take() {
                        self.stream_view.remove(&sid);
                        self.peer.notify("stream.close", json!({"stream": sid}));
                    }
                    *t = Term { resource: rid.clone(), stream: None, emu: Emulator::with_engine(engine, inner.width, inner.height, lines), size: (inner.width, inner.height), attaching: false, closed: None, retry_at: None, modes: Default::default() };
                }
                if let Some(why) = t.emu.fallback() {
                    // Say once per reason that the chosen engine isn't running (the pane still works on vt100).
                    let note = format!("terminal engine {}: {why}; using {}", t.emu.wanted().name(), t.emu.engine().name());
                    if self.engine_notes.insert(note.clone()) { self.message = note; }
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
                    t.emu.resize(inner.width, inner.height);
                    if let Some(sid) = t.stream {
                        self.peer.notify("stream.resize", json!({"stream": sid, "cols": inner.width, "rows": inner.height}));
                    }
                }
                if self.connected && t.can_attach(live, std::time::Instant::now()) {
                    t.begin_attach();
                    let (peer, tx, view, rid) = (self.peer.clone(), self.tx.clone(), view.to_string(), rid.clone());
                    let (cols, rows) = (inner.width, inner.height);
                    let connection = self.connection;
                    let history = t.emu.scrollback_capacity();
                    tokio::spawn(async move {
                        let result = peer
                            .call("resource.attach", json!({"resource": rid, "cols": cols, "rows": rows, "history": history}))
                            .await
                            .map(|r| r["stream"].as_u64().unwrap_or(0))
                            .map_err(|e| e.to_string());
                        let _ = tx.send(Msg::Attached { connection, view, resource: rid, result });
                    });
                }
                t.emu.draw(f.buffer_mut(), inner);
                // The selection and search matches, by row so they stay on their text while output scrolls.
                let scrolled = t.emu.scrolled();
                let top = scrollback::top(&t.emu, scrolled);
                let buf = f.buffer_mut();
                if let Some(sel) = self.selections.get(view) {
                    for r in 0..inner.height {
                        for c in 0..inner.width {
                            if sel.contains(scrollback::Pos { row: top + r as usize, col: c }) {
                                if let Some(cell) = buf.cell_mut((inner.x + c, inner.y + r)) { cell.set_style(pal.selected()); }
                            }
                        }
                    }
                }
                if let Some(sm) = self.scroll_mode.as_ref().filter(|m| m.view == view) {
                    for (i, h) in sm.hits.iter().enumerate().filter(|(_, h)| h.row >= top && h.row < top + inner.height as usize) {
                        let style = if i == sm.at { pal.selected() } else { Style::new().add_modifier(Modifier::UNDERLINED | Modifier::BOLD) };
                        for c in h.col..(h.col + h.width).min(inner.width) {
                            if let Some(cell) = buf.cell_mut((inner.x + c, inner.y + (h.row - top) as u16)) { cell.set_style(style); }
                        }
                    }
                }
                if let Some(reason) = &t.closed {
                    let line = Line::from(Span::styled(format!(" stream closed: {reason} "), pal.style(Some("warn"))));
                    f.render_widget(Paragraph::new(line), Rect { height: 1, ..inner });
                }
                if let Some((r, c, shape)) = t.emu.cursor().filter(|_| focused) {
                    if r < inner.height && c < inner.width {
                        self.cursor_style = shape;
                        return Some((inner.x + c, inner.y + r));
                    }
                }
                None
            }
            "panel" => {
                let v = self.panel_value(view);
                let title = v["title"].as_str().unwrap_or(view).to_string();
                let mut block = Block::bordered().title(format!(" {title} ")).border_style(border);
                // `title_right`: a short status at the frame's right end (an agent's state), in `title_right_style`.
                if let Some(right) = v["title_right"].as_str().filter(|s| !s.is_empty()) {
                    let style = v["title_right_style"].as_str().map(|s| pal.style(Some(s))).unwrap_or_else(|| pal.style(None));
                    block = block.title(Line::from(Span::styled(format!(" {right} "), style)).right_aligned());
                }
                let inner = block.inner(rect);
                f.render_widget(block, rect);
                let mut st = self.pstate.remove(view).unwrap_or_default();
                let hits = panel::render(f.buffer_mut(), inner, &v, &mut st, focused, pal, icons, self.ws.entries.get("doc/settings/tui").and_then(|s|s["key_labels"].as_str()));
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
        let filtering = self.filtering.as_ref().map(|k| self.pstate.get(k).map(|s| if self.filter_detail { s.detail.as_ref().map(|d| d.filter.clone()).unwrap_or_default() } else if s.searching { s.search.clone() } else { s.filter.clone() }).unwrap_or_default());
        let prompt = match &self.overlay {
            _ if filtering.is_some() => Some(("/".to_string(), filtering.clone().unwrap_or_default())),
            Some(Overlay::Prefix { .. }) => Some((self.key_labels(&self.prefix()), self.key_labels(&keys::held_words(self.held)))),
            Some(Overlay::Palette { query, .. }) => Some((":".to_string(), query.clone())),
            Some(Overlay::Input { prompt, multi: Some(_), .. }) => Some((format!("{prompt}:"), "(multi-line, above)".to_string())),
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
        if self.ws.safe_mode {
            spans.push(Span::styled(" SAFE MODE ", pal.badge(Some("warn"))));
        }
        if self.zoomed.is_some() {
            spans.push(Span::styled(" ZOOM ", pal.badge(Some("accent"))));
        }
        let failed = self.ws.extensions.values().filter(|e| e["status"] == "failed").count();
        if failed > 0 {
            let label=format!(" {failed} extension(s) failed · recover ");
            let x=area.x+spans.iter().map(|s|s.content.chars().count() as u16).sum::<u16>();
            let width=(label.chars().count() as u16).min(area.right().saturating_sub(x));
            if width>0 {self.hit.buttons.push((Rect{x,y:area.y,width,height:1},Item::from_entry(&json!({"label":"Recover failed extensions","command":"client.prefix"})),json!({})));}
            spans.push(Span::styled(label, pal.badge(Some("error"))));
        }
        if !self.ws.safe_mode {
            if let Some(notice) = &self.ws.backend_notice {
                spans.push(Span::styled(format!(" {notice} "), pal.style(Some("warn"))));
            }
        }
        if self.owner_unset {
            spans.push(Span::styled(" OWNER UNSET: bootstrap + reconnect ",pal.style(Some("warn"))));
        }
        // Contributed status-bar buttons.
        let mut x = area.x + spans.iter().map(|s| s.content.chars().count() as u16).sum::<u16>();
        let window = self.window.context(&self.ws);
        for item in self.menu_items("status", &json!({})) {
            // A segment may name this window's own state, e.g. the workspace it shows; empty hides it.
            if let Some(field) = &item.label_context {
                let value = window[field.as_str()].as_str().unwrap_or("");
                if value.is_empty() { continue; }
                let label = format!(" {} ", icons.label(&item.icon, value));
                let w = icons::width(&label);
                if x + w > area.right() { break; }
                self.hit.buttons.push((Rect { x, y: area.y, width: w, height: 1 }, item, json!({})));
                x += w;
                spans.push(Span::styled(label, pal.style(Some("muted"))));
                continue;
            }
            // Live segments take their label, icon and style from a view's live content. An empty
            // label hides a live segment (nothing to report), whatever its icon.
            let (text, style) = match item.view.as_deref().and_then(|v| self.ws.live.get(v)) {
                Some(l) => match l["label"].as_str().unwrap_or(&item.label) {
                    "" => continue,
                    t => (icons.label(if l["icon"].is_null() { &item.icon } else { &l["icon"] }, t), pal.style(Some(l["style"].as_str().unwrap_or("accent")))),
                },
                None if item.view.is_some() => continue,
                None => (icons.item_label(&item.icon, &item.label, item.icon_after), pal.style(Some("accent"))),
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
        if !self.ws.safe_mode && self.ws.backend_notice.is_none() && self.ws.backends_connected.is_empty() {
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
        self.draw_editor(f, area, pal);
    }

    /// A multi-line prompt (feature multiline): a box above the status bar with the draft and the cursor.
    fn draw_editor(&mut self, f: &mut Frame, area: Rect, pal: &Palette) {
        let Some(Overlay::Input { prompt, multi: Some(ed), .. }) = &self.overlay else { return };
        let (lines, (cl, cc)) = ed.lines();
        let w = area.width.saturating_sub(4).clamp(10, 100);
        let rows = (lines.len() as u16).clamp(3, 10);
        let r = Rect { x: area.x + (area.width.saturating_sub(w)) / 2, y: area.bottom().saturating_sub(rows + 3), width: w, height: rows + 2 };
        f.render_widget(Clear, r);
        let block = Block::bordered().title(format!(" {prompt} ")).title_bottom(if keys::shift_enter_reported() { " Enter send · Shift-Enter new line · Escape cancel " } else { " Enter send · Alt-Enter new line · Escape cancel " }).border_style(Style::new().fg(pal.prompt));
        let inner = block.inner(r);
        f.render_widget(block, r);
        let first = cl.saturating_sub(inner.height as usize - 1);
        let buf = f.buffer_mut();
        for (dy, line) in lines.iter().skip(first).take(inner.height as usize).enumerate() {
            buf.set_string(inner.x, inner.y + dy as u16, icons::truncate(line, inner.width as usize), Style::new().fg(pal.text));
        }
        let before: String = lines.get(cl).map(|l| l.chars().take(cc).collect()).unwrap_or_default();
        let x = inner.x + icons::width(&before).min(inner.width.saturating_sub(1));
        let y = inner.y + (cl - first) as u16;
        let under = lines.get(cl).and_then(|l| l.chars().nth(cc)).map(String::from).unwrap_or(" ".into());
        buf.set_string(x, y, under, Style::new().add_modifier(Modifier::REVERSED));
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
            let hits = panel::render(f.buffer_mut(), body, &v, &mut st, true, pal, icons, self.ws.entries.get("doc/settings/tui").and_then(|s|s["key_labels"].as_str()));
            self.pstate.insert("@popup".into(), st);
            self.hit.panels.insert("@popup".into(), hits);
            self.hit.overlay = Some((rect, Rect::default(), 0));
        }
    }

    /// Menus, prompts, pickers, the prefix menu and the palette (everything but popups).
    fn draw_menu_overlay(&mut self, f: &mut Frame, area: Rect, pal: &Palette, icons: &Icons) {
        // `all`: every row of a filtered list, so its size doesn't jump while typing.
        let (title, items, sel, query, all) = match &self.overlay {
            Some(Overlay::Popup { .. }) => return,
            None => {
                self.hit.overlay = None;
                return;
            }
            Some(Overlay::Prefix { sel }) => {
                // Recovery actions that lost their key show `·`: click or Enter runs them.
                let rows = self.prefix_items();
                let w = rows.iter().filter_map(|r| r.key.as_ref()).map(|k| k.chars().count()).max().unwrap_or(1).max(2);
                let items: Vec<String> = rows.into_iter().map(|r| format!(" {:>w$}  {}", r.key.unwrap_or_else(|| "·".into()), r.label)).collect();
                (" vqro (press a key or click) ".to_string(), items.clone(), *sel, None::<String>, items)
            }
            Some(Overlay::Palette { query, sel }) => {
                let items: Vec<String> = self.palette_items().into_iter().map(|n| format!("{n}  {}", self.ws.commands[&n]["title"].as_str().unwrap_or(""))).collect();
                let _ = query;
                (" commands (type to filter) ".to_string(), items.clone(), *sel, None, items)
            }
            Some(Overlay::Pick { sel, query, .. }) => {
                let rows = self.pick_lines();
                let keep = filter_rows(&rows, query);
                let (title, query) = pick_title(" pick a terminal ", keep.len(), rows.len(), query);
                (title, keep.into_iter().map(|i| rows[i].clone()).collect(), *sel, query, rows)
            }
            Some(Overlay::Menu { items, sel, query, .. }) => {
                // Filter on the labels; draw menu_lines (icons, padding).
                let rows = menu_lines(items, icons);
                let labels: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
                let keep = filter_rows(&labels, query);
                let (title, query) = pick_title(" ", keep.len(), labels.len(), query);
                (title, keep.into_iter().filter_map(|i| rows.get(i).cloned()).collect(), *sel, query, rows)
            }
            Some(Overlay::Input { .. }) => {
                // Typed in the status bar; clicking anywhere else cancels.
                self.hit.overlay = Some((self.hit.status, Rect::default(), 0));
                return;
            }
        };
        let items: Vec<String> = items;
        let w = area.width.min(64);
        let h = (all.len().max(items.len()) as u16 + 2 + query.is_some() as u16).min(area.height).max(3);
        let rect = if matches!(self.overlay, Some(Overlay::Prefix { .. }) | Some(Overlay::Palette { .. })) {
            // Anchored above the prompt line it belongs to.
            let w = if matches!(self.overlay, Some(Overlay::Prefix { .. })) { w.min(44) } else { w };
            Rect { x: area.x, y: area.bottom().saturating_sub(h), width: w, height: h }
        } else if let Some(Overlay::Menu { at, .. }) = &self.overlay {
            // At the pointer, kept on screen.
            let w = (all.iter().map(|i| icons::width(i)).max().unwrap_or(10).max(icons::width(&title)) + 4).min(area.width);
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

/// Keys a pane's own declarations add, on the focused pane: `[`/`]` its pages (`own_pages`: a detail pane; the list's
/// pages keep their handling in `panel_key`), `f` its first facet, `s` its sort. Returns whether the key was used; a
/// pane without pages, facets or sorts leaves these keys alone.
fn pane_key(v: &Value, st: &mut panel::State, k: &KeyEvent, own_pages: bool) -> bool {
    if st.editing.is_some() { return false; }  // a composer being typed into takes every key
    let pages = panel::page_titles(v).len();
    match k.code {
        KeyCode::Char('[') | KeyCode::Char(']') if own_pages && pages > 0 => {
            st.page = if k.code == KeyCode::Char(']') { (st.page + 1) % pages } else { (st.page + pages - 1) % pages };
            st.sel = 0; st.top = 0;
            true
        }
        KeyCode::Char('f') if k.modifiers.is_empty() && panel::declared(v, st, "facets").is_some_and(|f| !f.is_empty()) => { panel::cycle_facet(v, st, panel::SUPPORTED); true }
        KeyCode::Char('s') if k.modifiers.is_empty() && panel::declared(v, st, "sorts").is_some_and(|f| !f.is_empty()) => { panel::cycle_sort(v, st); true }
        _ => false,
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
#[allow(clippy::too_many_arguments)]
/// Every view a layout node shows or holds (all tabs, all panes), depth-first.
fn views_of(node: &Value) -> Vec<String> {
    let mut out: Vec<String> = node["view"].as_str().map(String::from).into_iter().collect();
    for c in node["children"].as_array().into_iter().flatten() {
        out.extend(views_of(c));
    }
    for t in node["tabs"].as_array().into_iter().flatten() {
        out.extend(views_of(&t["node"]));
    }
    out
}

fn walk(node: &Value, area: Rect, path: &mut Vec<usize>, overrides: &HashMap<Vec<usize>, Vec<u16>>, tabbar: &[Item], tabstyle: &crate::ui_model::tabbar::Style, hit: &mut Hit, buf: &mut Buffer, pal: &Palette, icons: &Icons) {
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
            walk(c, *r, path, overrides, tabbar, tabstyle, hit, buf, pal, icons);
            path.pop();
        }
    } else if let Some(tabs) = node["tabs"].as_array() {
        let active = (node["active"].as_u64().unwrap_or(0) as usize).min(tabs.len().saturating_sub(1));
        let [bar, body] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        let mut spans = Vec::new();
        let mut x = bar.x;
        // Labels and chip spacing come from the shared model (ui_model::tabbar, the tabs setting); colours from the
        // palette: active chip tab_active/tab_active_text (bold), inactive chip surface/muted, gaps tab_bar.
        for (i, t) in tabs.iter().enumerate() {
            if i > 0 && tabstyle.gap() > 0 {
                spans.push(Span::raw(" ".repeat(tabstyle.gap())));
                x = x.saturating_add(tabstyle.gap() as u16);
            }
            let label = tabstyle.label(i, t["title"].as_str().unwrap_or("tab"));
            // A mark (inactive tabs, the tabs setting's `marks`) sits inside the chip after the name.
            let mark = t["mark"].as_str().map(|m| {
                let (text, style) = match m { "blocked" => ("⚠", "warn"), "error" => ("✗", "error"), "done" => ("✓", "ok"), _ => ("•", "accent") };
                (icons.resolve(&json!({"name": m, "text": text})).unwrap_or(text).to_string(), style)
            });
            let w = icons::width(&label) + mark.as_ref().map(|(g, _)| icons::width(g) + 1).unwrap_or(0);
            hit.tabs.push((path.clone(), i, Rect { x, y: bar.y, width: w.min(bar.right().saturating_sub(x)), height: 1 }));
            x = x.saturating_add(w);
            // A tab whose programs ended (`state` hint from the tabs extension) is drawn dim, its title unchanged.
            let ended = crate::ui_model::tabbar::tab_state(t).is_some();
            let style = match (i == active, tabstyle.chips) {
                (true, _) if ended => Style::new().bg(pal.tab_active).fg(pal.tab_active_text).add_modifier(Modifier::DIM),
                (true, _) => Style::new().bg(pal.tab_active).fg(pal.tab_active_text).add_modifier(Modifier::BOLD),
                (false, true) => Style::new().bg(pal.surface).fg(if ended { pal.dim } else { pal.muted }),
                (false, false) => pal.style(Some(if ended { "dim" } else { "muted" })),
            };
            match mark {
                Some((glyph, name)) => {
                    spans.push(Span::styled(label.trim_end_matches(' ').to_string() + " ", style));
                    let fg = pal.style(Some(name)).fg.unwrap_or(pal.muted);
                    spans.push(Span::styled(glyph, style.fg(fg)));
                    spans.push(Span::styled(" ", style));
                }
                None => spans.push(Span::styled(label, style)),
            }
        }
        if tabstyle.chips && !tabbar.is_empty() {
            spans.push(Span::raw(" "));
            x = x.saturating_add(1);
        }
        // Contributed tab-bar buttons (e.g. `+` from the tabs extension), on the first tab bar.
        if !hit.buttons.iter().any(|(_, _, c)| c.get("tab_path").is_some()) {
            for item in tabbar {
                let label = format!(" {} ", icons.item_label(&item.icon, &item.label, item.icon_after));
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
            walk(&t["node"], body, path, overrides, tabbar, tabstyle, hit, buf, pal, icons);
            path.pop();
        }
    }
}

/// A menu's lines. When any item has an icon, every label starts after one shared icon column.
/// Pick-one lists (menus, option drop-downs, the resource picker) with more rows than this can be
/// filtered by typing. Shorter ones keep plain arrows/Enter, so typing never hides a short menu.
const SEARCHABLE: usize = 10;

/// Indices of `rows` containing `query` (case-insensitive); all rows for an empty query.
fn filter_rows(rows: &[String], query: &str) -> Vec<usize> {
    let query = query.to_lowercase();
    rows.iter().enumerate().filter(|(_, row)| query.is_empty() || row.to_lowercase().contains(&query)).map(|(i, _)| i).collect()
}

/// Title and query line of a pick-one list: long lists say "n of m" and show what was typed.
fn pick_title(title: &str, kept: usize, total: usize, query: &str) -> (String, Option<String>) {
    if total <= SEARCHABLE {
        return (title.to_string(), None);
    }
    let hint = if query.is_empty() { "type to filter" } else { "Esc clears" };
    let name = title.trim();
    let name = if name.is_empty() { String::new() } else { format!("{name} · ") };
    (format!(" {name}{kept} of {total} · {hint} "), Some(query.to_string()))
}

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

/// A terminal's name: one the user gave, else the title its program set (OSC 0/2, as the backend reports it), else
/// its command.
fn resource_name(r: &Value) -> Option<String> {
    r["name"].as_str().or_else(|| r["title"].as_str().filter(|t| !t.trim().is_empty())).or_else(|| r["argv"][0].as_str()).map(String::from)
}

#[cfg(test)]
fn new_parser(cols: u16, rows: u16) -> Emulator {
    Emulator::new(cols, rows, DEFAULT_SCROLLBACK)
}

/// Scrollback lines per terminal pane (`doc/settings/tui.scrollback_lines`): each line costs 32 bytes per column,
/// about 6 MB per 1000 lines in a 200-column pane, so the setting is bounded.
const DEFAULT_SCROLLBACK: usize = 2000;
const SCROLLBACK_RANGE: std::ops::RangeInclusive<usize> = 100..=20_000;

/// How long a popup waits for its view's entries event (see `App::open_popup`).
const POPUP_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Where a terminal view's scroll mode is: reading its history with keys, and searching it.
struct ScrollMode {
    view: String,
    query: String,
    /// The search line is being typed.
    typing: bool,
    hits: Vec<scrollback::Hit>,
    at: usize,
}

fn cursor_shape(s: CursorShape) -> crossterm::cursor::SetCursorStyle {
    use crossterm::cursor::SetCursorStyle as C;
    match s {
        CursorShape::Default => C::DefaultUserShape,
        CursorShape::BlinkBlock => C::BlinkingBlock,
        CursorShape::SteadyBlock => C::SteadyBlock,
        CursorShape::BlinkUnderline => C::BlinkingUnderScore,
        CursorShape::SteadyUnderline => C::SteadyUnderScore,
        CursorShape::BlinkBar => C::BlinkingBar,
        CursorShape::SteadyBar => C::SteadyBar,
    }
}

/// The bytes that pass a program's effect on to the outer terminal.
fn effect_bytes(e: &Effect) -> Vec<u8> {
    match e {
        Effect::Clipboard(ty, data) => [b"\x1b]52;".as_slice(), ty, b";", data, b"\x07"].concat(),
        Effect::Bell => b"\x07".to_vec(),
        Effect::Notify(body) => [b"\x1b]".as_slice(), body, b"\x07"].concat(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_model::workspace::{builtin_bindings, resolve, BUILTIN};

    fn app() -> (App, mpsc::UnboundedReceiver<Msg>) {
        let (peer, _) = Peer::spawn(tokio::io::empty(), tokio::io::sink());
        let (tx, rx) = mpsc::unbounded_channel();
        (App::new(peer, tx), rx)
    }

    #[test]
    fn program_effects_are_collected_bounded_and_passed_on_verbatim() {
        let mut p = new_parser(80, 24);
        p.process(b"a\x07b\x07\x1b]52;c;aGk=\x07\x1b]52;c;?\x07\x1b]9;done\x07\x1b]9;4;1;50\x07\x1b]777;notify;T;B\x07\x1b]777;other\x07\x1b]8;;http://x\x07");
        let mut out = p.take_effects();
        // Kinds may queue in a different order per engine: compare in a fixed one.
        let rank = |e: &Effect| match e { Effect::Bell => 0, Effect::Clipboard(..) => 1, Effect::Notify(..) => 2 };
        out.sort_by_key(rank);
        assert_eq!(out, vec![Effect::Bell, Effect::Clipboard(b"c".to_vec(), b"aGk=".to_vec()), Effect::Notify(b"9;done".to_vec()), Effect::Notify(b"777;notify;T;B".to_vec())],
            "one bell per parse; clipboard reads and progress OSC 9;4 are not passed on");
        assert_eq!(effect_bytes(&out[1]), b"\x1b]52;c;aGk=\x07");
        assert_eq!(effect_bytes(&out[3]), b"\x1b]777;notify;T;B\x07");
        for _ in 0..100 { p.process(b"\x1b]9;x\x07"); }
        assert_eq!(p.take_effects().len(), emulator::MAX_EFFECTS);
        // Dim is drawn; the cursor shape a program sets is tracked for the outer terminal.
        let mut p = new_parser(10, 2);
        p.process(b"\x1b[2mD\x1b[5 q");
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 2));
        p.draw(&mut buf, Rect::new(0, 0, 10, 2));
        assert!(buf[(0, 0)].modifier.contains(Modifier::DIM));
        assert_eq!(p.cursor().map(|c| c.2), Some(CursorShape::BlinkBar));
    }

    #[tokio::test]
    async fn terminal_selection_scroll_mode_and_search() {
        let (mut app, _) = app();
        app.load(&json!({"epoch":"e","seq":1,"entries":{"view/v":{"value":{"kind":"terminal","resource":"r1"}}},"resources":[{"id":"r1","kind":"terminal","state":"live"}]}));
        let mut emu = Emulator::new(20, 3, 100);
        emu.process(b"one alpha\r\ntwo /tmp/x.log\r\nthree alpha\r\nfour\r\nfive alpha");
        app.terms.insert("v".into(), Term { resource: "r1".into(), stream: None, emu, size: (20, 3), attaching: false, closed: None, retry_at: None, modes: Default::default() });
        app.leaves = vec!["v".into()];
        let inner = Rect::new(1, 1, 20, 3);
        app.hit.leaves = vec![("v".into(), Rect::new(0, 0, 22, 5), inner)];
        let at = |x: u16, y: u16| Position { x: inner.x + x, y: inner.y + y };
        // Screen rows now: three alpha / four / five alpha (rows 2-4; rows 0-1 in the scrollback).
        app.selection_press("v", at(0, 0));
        app.selection_drag("v", at(3, 1));
        app.selection_release("v");
        assert!(app.message.starts_with("copy: sent 16 characters"), "{}", app.message);
        assert_eq!(app.selection_text("v").as_deref(), Some("three alpha\nfour"));
        // A click clears it; a double click selects a word (a path stays whole), a triple click the line.
        app.last_press = None;
        app.selection_press("v", at(2, 1)); app.selection_release("v");
        assert!(!app.selections.contains_key("v"));
        app.last_press = None;
        app.run_command("client.scroll", json!({}), json!({}), None);
        assert!(app.message.starts_with("scroll ↑0 of 2"), "{}", app.message);
        app.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(app.terms["v"].emu.scroll_offset(), 2, "g: the oldest line");
        app.selection_press("v", at(6, 1)); app.selection_press("v", at(6, 1)); app.selection_release("v");
        assert_eq!(app.selection_text("v").as_deref(), Some("/tmp/x.log"));
        app.selection_press("v", at(6, 1)); app.selection_release("v");
        assert_eq!(app.selection_text("v").as_deref(), Some("two /tmp/x.log"), "third press: the line");
        // Search from the bottom up: the nearest match first, n goes further up, N back down.
        app.on_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for c in "ALPHA".chars() { app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT)); }
        assert_eq!(app.message, "/ALPHA  1 of 3  n/N");
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(app.message.contains("/ALPHA  3 of 3"), "{}", app.message);
        assert_eq!(app.terms["v"].emu.scroll_offset(), 2, "the oldest match is in view");
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.scroll_mode.is_none());
        assert_eq!(app.terms["v"].emu.scroll_offset(), 0, "leaving returns to the live screen");
        // The setting is bounded.
        app.ws.entries.insert("doc/settings/tui".into(), json!({"scrollback_lines": 10_000_000}));
        assert_eq!(app.scrollback_lines(), 20_000);
    }

    #[tokio::test]
    async fn a_popup_reply_before_its_view_waits_for_the_entries_event() {
        let (mut app, _) = app();
        app.load(&json!({"epoch":"e","seq":1,"entries":{}}));
        // The reply arrives first: the view is unknown, so it waits instead of being refused.
        app.on_msg(Msg::Popup { view: "req".into(), width: 50, ctx: json!({}) });
        assert!(app.overlay.is_none());
        assert_eq!(app.message, "opening req…");
        app.on_event_record(&json!({"epoch":"e","seq":2,"type":"entries","data":{"changes":[{"key":"view/req","value":{"kind":"panel","items":[{"text":"answer?"}]},"version":1}]}}));
        assert!(matches!(&app.overlay, Some(Overlay::Popup { view, .. }) if view == "req"), "opened when its view arrived");
        assert!(!app.message.starts_with("opening"), "{}", app.message);
        // A view that never comes: past the wait, the next event or input says so and opens nothing.
        app.overlay = None;
        app.on_msg(Msg::Popup { view: "never".into(), width: 50, ctx: json!({}) });
        app.pending_popup.as_mut().unwrap().3 = std::time::Instant::now();
        app.on_event(Event::FocusGained);
        assert!(app.pending_popup.is_none() && app.overlay.is_none());
        assert_eq!(app.message, "popup: no view never");
        // Asked again for the popup already open (a late reply): the user's filter and place in it stay.
        app.on_msg(Msg::Popup { view: "req".into(), width: 50, ctx: json!({}) });
        app.pstate.get_mut("@popup").unwrap().filter = "ans".into();
        app.on_msg(Msg::Popup { view: "req".into(), width: 60, ctx: json!({}) });
        assert_eq!(app.pstate["@popup"].filter, "ans");
    }

    #[tokio::test]
    async fn a_composer_grows_with_its_draft_and_names_the_new_line_key_that_works() {
        let (mut app, _) = app();
        app.load(&json!({"epoch":"e","seq":1,"entries":{"view/p":{"value":{"kind":"panel","items":[
            {"text":"conversation"},{"type":"composer","id":"c","text":"Message","rows":5,"pin":"bottom","command":"x"}]}}}}));
        app.leaves = vec!["p".into()];
        let draw = |app: &mut App| {
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(70, 16)).unwrap();
            let mut out = Vec::new();
            terminal.draw(|f| { app.draw_view(f, "p", Rect::new(0, 0, 70, 16), true, &Palette::default(), &Icons::new(None, false));
                let b = f.buffer_mut(); out = (0..16).map(|y| (0..70).map(|x| b[(x, y)].symbol().to_string()).collect::<String>()).collect(); }).unwrap();
            out
        };
        let composer_rows = |screen: &[String]| screen.iter().filter(|l| l.contains('▌')).count();
        keys::set_shift_enter_reported(false);
        let screen = draw(&mut app);
        assert_eq!(composer_rows(&screen), 1, "an empty composer is one line: {screen:#?}");
        assert!(screen.iter().any(|l| l.contains("Alt-Enter new line")));
        let st = app.pstate.get_mut("p").unwrap();
        st.composers.entry("c".into()).or_insert_with(|| crate::ui_model::composer::Composer::new(&json!({"type":"composer"}))).editor =
            crate::ui_model::composer::Editor::new("one\ntwo\nthree", None, None);
        keys::set_shift_enter_reported(true);
        let screen = draw(&mut app);
        assert_eq!(composer_rows(&screen), 3, "it grows with the draft: {screen:#?}");
        assert!(screen.iter().any(|l| l.contains("Shift-Enter new line")), "the terminal reports Shift-Enter");
        st_cap(&mut app);
        keys::set_shift_enter_reported(false);
    }

    /// A draft longer than `rows` stops growing at `rows`.
    fn st_cap(app: &mut App) {
        let st = app.pstate.get_mut("p").unwrap();
        st.composers.get_mut("c").unwrap().editor = crate::ui_model::composer::Editor::new("1\n2\n3\n4\n5\n6\n7", None, None);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(70, 16)).unwrap();
        let mut n = 0;
        terminal.draw(|f| { app.draw_view(f, "p", Rect::new(0, 0, 70, 16), true, &Palette::default(), &Icons::new(None, false));
            let b = f.buffer_mut(); n = (0..16).filter(|y| (0..70).any(|x| b[(x, *y)].symbol() == "▌")).count(); }).unwrap();
        assert_eq!(n, 5);
    }

    #[tokio::test]
    async fn the_engine_setting_switches_open_panes_and_reports_a_fallback_once() {
        let (mut app, _) = app();
        app.load(&json!({"epoch":"e","seq":1,"entries":{"view/t":{"value":{"kind":"terminal","resource":"r1"}}},
            "resources":[{"id":"r1","kind":"terminal","state":"live","argv":["/bin/sh"]}]}));
        app.leaves = vec!["t".into()];
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 8)).unwrap();
        let mut draw = |app: &mut App| { terminal.draw(|f| { app.draw_view(f, "t", Rect::new(0, 0, 40, 8), true, &Palette::default(), &Icons::new(None, false)); }).unwrap(); };
        emulator::FAIL_START.with(|f| f.set(Some(emulator::EngineKind::Libghostty)));
        draw(&mut app);
        assert_eq!(app.terms["t"].emu.engine(), emulator::EngineKind::Vt100);
        app.terms.get_mut("t").unwrap().attaching = false;
        app.ws.entries.insert("doc/settings/tui".into(), json!({"terminal_engine": "libghostty"}));
        draw(&mut app);
        assert_eq!(app.terms["t"].emu.wanted(), emulator::EngineKind::Libghostty, "an open pane switches");
        assert!(app.message.starts_with("terminal engine libghostty: "), "{}", app.message);
        app.message.clear();
        app.terms.get_mut("t").unwrap().attaching = false;
        draw(&mut app);
        assert!(app.message.is_empty(), "each fallback reason is reported once");
        emulator::FAIL_START.with(|f| f.set(None));
        // A name this client doesn't know (a newer engine, a typo) uses the default and says so.
        app.ws.entries.insert("doc/settings/tui".into(), json!({"terminal_engine": "alacritty"}));
        app.terms.get_mut("t").unwrap().attaching = false;
        draw(&mut app);
        assert!(app.message.contains("\"alacritty\" is not built into this client"), "{}", app.message);
        assert_eq!(app.terms["t"].emu.wanted(), emulator::EngineKind::DEFAULT);
    }

    #[tokio::test]
    async fn agent_panes_scroll_by_wheel_and_page_keys_while_the_composer_has_the_keys() {
        let (mut app, _) = app();
        let mut items: Vec<Value> = (0..60).map(|i| json!({"type":"text","wrap":true,"text":format!("message {i}")})).collect();
        items.push(json!({"type":"composer","id":"c","text":"Message","pin":"bottom","autofocus":true,"command":"x"}));
        app.load(&json!({"epoch":"e","seq":1,"entries":{"view/p":{"value":{"kind":"panel","follow":true,"items":items}}}}));
        app.leaves = vec!["p".into()];
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 16)).unwrap();
        let mut draw = |app: &mut App| -> String {
            let mut out = String::new();
            terminal.draw(|f| { app.draw_view(f, "p", Rect::new(0, 0, 40, 16), true, &Palette::default(), &Icons::new(None, false));
                let b = f.buffer_mut(); out = (0..16).map(|y| (0..40).map(|x| b[(x, y)].symbol().to_string()).collect::<String>() + "\n").collect(); }).unwrap();
            out
        };
        draw(&mut app);
        app.pstate.get_mut("p").unwrap().editing = Some("c".into());  // typing in the composer (autofocus)
        let screen = draw(&mut app);
        assert!(screen.contains("message 59"), "following: the newest message shows\n{screen}");
        // The wheel scrolls the conversation even though the composer has the keys.
        for _ in 0..5 { app.panel_scroll("p", "p", Position { x: 5, y: 3 }, true, json!({})); }
        let screen = draw(&mut app);
        assert!(!screen.contains("message 59") && app.pstate["p"].editing.is_some(), "scrolled back, still typing\n{screen}");
        // PgUp/PgDn while typing scroll it too.
        let top = app.pstate["p"].top;
        app.panel_key("p", "p", KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE), json!({}));
        draw(&mut app);
        assert!(app.pstate["p"].top < top, "PgUp scrolls up while typing");
        // Back down to the end: following resumes.
        for _ in 0..40 { app.panel_scroll("p", "p", Position { x: 5, y: 3 }, false, json!({})); }
        draw(&mut app);
        assert!(!app.pstate["p"].manual_scroll, "at the end it follows again");
        assert!(draw(&mut app).contains("message 59"));
    }

    #[tokio::test]
    async fn clicking_a_composer_control_while_typing_chooses_it_and_never_sends() {
        let (mut app, mut rx) = app();
        app.load(&json!({"epoch":"e","seq":1,"entries":{"view/p":{"value":{"kind":"panel","items":[{"text":"conversation"},
            {"type":"composer","id":"c","text":"Message","pin":"bottom","command":"agents.send",
             "controls":[{"type":"chips","arg":"mode","chips":[{"text":"auto","value":"auto","checked":true},{"text":"steer","value":"steer"}]}]}]}}}}));
        app.leaves = vec!["p".into()];
        app.hit.leaves = vec![("p".into(), Rect::new(0, 0, 60, 10), Rect::new(1, 1, 58, 8))];
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 10)).unwrap();
        let mut find = |app: &mut App, label: &str| -> Position {
            let mut at = None;
            terminal.draw(|f| { app.draw_view(f, "p", Rect::new(0, 0, 60, 10), true, &Palette::default(), &Icons::new(None, false));
                let b = f.buffer_mut();
                for y in 0..10 { let line: String = (0..60).map(|x| b[(x, y)].symbol().to_string()).collect();
                    if let Some(x) = line.find(label) { at = Some(Position { x: line[..x].chars().count() as u16 + 1, y }); } } }).unwrap();
            at.unwrap_or_else(|| panic!("{label} not drawn"))
        };
        find(&mut app, "steer");
        let st = app.pstate.get_mut("p").unwrap();
        st.editing = Some("c".into());
        st.composers.entry("c".into()).or_insert_with(|| Composer::new(&json!({"type":"composer"}))).editor = crate::ui_model::composer::Editor::new("abc", None, None);
        let steer = find(&mut app, "steer");
        app.panel_click("p", "p", steer, MouseButton::Left, json!({}));
        let st = &app.pstate["p"];
        assert_eq!(st.composers["c"].values.get("mode"), Some(&json!("steer")), "the click chose steer");
        assert_eq!(st.composers["c"].editor.string(), "abc", "the draft is kept");
        assert_eq!(st.editing.as_deref(), Some("c"), "typing goes on");
        while let Ok(m) = rx.try_recv() { assert!(!matches!(m, Msg::ComposerSent { .. }), "nothing was sent"); }
        assert!(!app.message.starts_with("running "), "no command ran: {}", app.message);
        // A click on the draft's text while typing keeps typing and sends nothing.
        let text = find(&mut app, "abc");
        app.message.clear();
        app.panel_click("p", "p", text, MouseButton::Left, json!({}));
        assert_eq!(app.pstate["p"].composers["c"].editor.string(), "abc");
        assert!(app.message.is_empty(), "{}", app.message);
        // [Send] still sends.
        let send = find(&mut app, "[Send]");
        app.panel_click("p", "p", send, MouseButton::Left, json!({}));
        assert!(app.message.starts_with("running agents.send"), "{}", app.message);
    }

    #[tokio::test]
    async fn message_blocks_are_framed_and_long_text_folds() {
        let (mut app, _) = app();
        app.load(&json!({"epoch":"e","seq":1,"entries":{"view/p":{"value":{"kind":"panel","items":[
            {"type":"text","text":"Pi · 21:08","style":"bold"},
            {"type":"text","id":"think","text":"one\ntwo\nthree\nfour","wrap":true,"fold":2,"style":"dim"},
            {"type":"message","id":"m","text":"Message from coord","title":"Message from coord · 21:11 · follow-up","body":"a\nb\nc\nd\ne\nf","fold":4}]}}}}));
        app.leaves = vec!["p".into()];
        app.hit.leaves = vec![("p".into(), Rect::new(0, 0, 50, 20), Rect::new(1, 1, 48, 18))];
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(50, 20)).unwrap();
        let mut draw = |app: &mut App| -> Vec<String> {
            let mut out = Vec::new();
            terminal.draw(|f| { app.draw_view(f, "p", Rect::new(0, 0, 50, 20), true, &Palette::default(), &Icons::new(None, false));
                let b = f.buffer_mut(); out = (0..20).map(|y| (0..50).map(|x| b[(x, y)].symbol().to_string()).collect::<String>()).collect(); }).unwrap();
            out
        };
        let screen = draw(&mut app);
        let has = |s: &[String], t: &str| s.iter().any(|l| l.contains(t));
        assert!(has(&screen, "┏━ Message from coord · 21:11 · follow-up") && has(&screen, "┃ d") && !has(&screen, "┃ e") && has(&screen, "┃ ▸ 2 more lines") && has(&screen, "┗━"), "{screen:#?}");
        assert!(has(&screen, "two") && !has(&screen, "three") && has(&screen, "▸ 2 more lines"), "the text row shows 2 lines and its fold line");
        // A click on a fold line unfolds that row; again folds it.
        let y = screen.iter().position(|l| l.contains("┃ ▸ 2 more lines")).unwrap() as u16;
        let x = screen[y as usize].find("▸").map(|i| screen[y as usize][..i].chars().count() as u16).unwrap();
        app.panel_click("p", "p", Position { x, y }, MouseButton::Left, json!({}));
        let screen = draw(&mut app);
        assert!(has(&screen, "┃ f") && has(&screen, "┃ ▾ less"), "{screen:#?}");
        // Enter on the selected folding text row unfolds it too.
        let st = app.pstate.get_mut("p").unwrap();
        st.sel = 1;
        app.panel_key("p", "p", KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), json!({}));
        let screen = draw(&mut app);
        assert!(has(&screen, "four"), "{screen:#?}");
    }

    #[tokio::test]
    async fn toolbar_parts_frame_status_latest_hint_and_short_labels() {
        let (mut app, _) = app();
        let chips = json!([{"text":"Stop…","short":"■","command":"x"},{"text":"⚙ Settings","short":"⚙","command":"x"},
            {"text":"Thinking","align":"right","command":"x"},{"text":"/ search","short":"/","align":"right","command":"x"}]);
        let mut items: Vec<Value> = (0..40).map(|i| json!({"type":"text","text":format!("message {i}"),"focusable":true})).collect();
        items.push(json!({"type":"composer","id":"c","text":"Message","pin":"bottom","command":"x"}));
        app.load(&json!({"epoch":"e","seq":1,"entries":{"view/p":{"value":{"kind":"panel","title":"Pi r52 · ~/vqro","title_right":"● idle","title_right_style":"ok",
            "follow":true,"toolbar":[{"type":"chips","chips":chips}],"items":items}}}}));
        app.leaves = vec!["p".into()];
        let draw = |app: &mut App, w: u16| -> Vec<String> {
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, 16)).unwrap();
            let mut out = Vec::new();
            terminal.draw(|f| { app.draw_view(f, "p", Rect::new(0, 0, w, 16), true, &Palette::default(), &Icons::new(None, false));
                let b = f.buffer_mut(); out = (0..16).map(|y| (0..w).map(|x| b[(x, y)].symbol().to_string()).collect::<String>()).collect(); }).unwrap();
            out
        };
        let wide = draw(&mut app, 80);
        assert!(wide[0].contains(" Pi r52 · ~/vqro ") && wide[0].trim_end_matches(['┐', '─']).ends_with("● idle "), "frame title and status: {}", wide[0]);
        let bar = &wide[1];
        assert!(bar.find("Stop…").unwrap() < bar.find("⚙ Settings").unwrap() && bar.find("Thinking").unwrap() > 40 && bar.trim_end_matches(['│', ' ']).ends_with("/ search"),
            "buttons left, filters right: {bar}");
        let narrow = draw(&mut app, 34);
        assert!(narrow[1].contains(" ■ ") && narrow[1].contains(" ⚙ ") && !narrow[1].contains("Settings"), "short labels when narrow: {}", narrow[1]);
        // Following: no hint at the end; scrolled up, "↓ Latest · End", and a click on it returns.
        assert!(!wide.iter().any(|l| l.contains("↓ Latest")));
        for _ in 0..5 { app.panel_scroll("p", "p", Position { x: 5, y: 5 }, true, json!({})); }
        let up = draw(&mut app, 80);
        let y = up.iter().position(|l| l.contains("↓ Latest · End")).expect("hint when scrolled up") as u16;
        let x = up[y as usize].find("↓").map(|i| up[y as usize][..i].chars().count() as u16).unwrap();
        app.panel_click("p", "p", Position { x, y }, MouseButton::Left, json!({}));
        let back = draw(&mut app, 80);
        assert!(back.iter().any(|l| l.contains("message 39")) && !back.iter().any(|l| l.contains("↓ Latest")), "{back:#?}");
    }

    #[tokio::test]
    async fn inline_row_actions_are_drawn_on_the_right_and_run_on_a_click() {
        let (mut app, _) = app();
        app.load(&json!({"epoch":"e","seq":1,"entries":{"view/p":{"value":{"kind":"panel","items":[
            {"text":"✓ read   README.md · lines 1–40","detail":"0.1 s","actions":[{"text":"open","command":"links.open","args":{"path":"README.md"}},{"text":"copy","command":"client.copy","args":{"text":"x"}}],
             "menu":[{"label":"Open","command":"links.open"}]}]}}}}));
        app.leaves = vec!["p".into()];
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(70, 6)).unwrap();
        let mut line = String::new();
        terminal.draw(|f| { app.draw_view(f, "p", Rect::new(0, 0, 70, 6), true, &Palette::default(), &Icons::new(None, false));
            let b = f.buffer_mut(); line = (0..70).map(|x| b[(x, 1)].symbol().to_string()).collect(); }).unwrap();
        assert!(line.trim_end_matches(['│', ' ']).ends_with("0.1 s [open] [copy]"), "{line}");
        let x = line.find("[open]").map(|i| line[..i].chars().count() as u16).unwrap();
        app.panel_click("p", "p", Position { x: x + 1, y: 1 }, MouseButton::Left, json!({}));
        assert!(app.message.starts_with("running links.open"), "{}", app.message);
        let x = line.find("[copy]").map(|i| line[..i].chars().count() as u16).unwrap();
        app.panel_click("p", "p", Position { x: x + 1, y: 1 }, MouseButton::Left, json!({}));
        assert!(app.message.starts_with("copy: sent 1 character"), "{}", app.message);
        // An action that acts on its panel (client.panel) gets the panel in its context, as a key would: the
        // mouse caller's context names only the pane (vqro-lead-secondary's probe).
        app.load(&json!({"epoch":"e","seq":2,"entries":{"view/p":{"value":{"kind":"panel","items":[
            {"text":"first","actions":[{"text":"last","command":"client.panel","args":{"action":"last"}}]},{"text":"second"},{"text":"third"}]}}}}));
        let mut line = String::new();
        terminal.draw(|f| { app.draw_view(f, "p", Rect::new(0, 0, 70, 6), true, &Palette::default(), &Icons::new(None, false));
            let b = f.buffer_mut(); line = (0..70).map(|x| b[(x, 1)].symbol().to_string()).collect(); }).unwrap();
        let x = line.find("[last]").map(|i| line[..i].chars().count() as u16).unwrap();
        app.panel_click("p", "p", Position { x: x + 1, y: 1 }, MouseButton::Left, json!({"view": "p"}));
        assert_eq!(app.pstate["p"].sel, 2, "client.panel from an inline action acted on this panel");
    }

    #[tokio::test]
    async fn a_composer_without_a_send_button_and_a_select_control() {
        let (mut app, _) = app();
        app.load(&json!({"epoch":"e","seq":1,"entries":{"view/p":{"value":{"kind":"panel","items":[
            {"type":"composer","id":"c","text":"Message","command":"x","send_button":false,
             "controls":[{"type":"select","arg":"mode","text":"Input","value":"auto","options":[{"label":"auto","value":"auto"},{"label":"steer","value":"steer"}]}]}]}}}}));
        app.leaves = vec!["p".into()];
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(70, 8)).unwrap();
        let mut screen = String::new();
        terminal.draw(|f| { app.draw_view(f, "p", Rect::new(0, 0, 70, 8), true, &Palette::default(), &Icons::new(None, false));
            let b = f.buffer_mut(); screen = (0..8).map(|y| (0..70).map(|x| b[(x, y)].symbol().to_string()).collect::<String>() + "\n").collect(); }).unwrap();
        assert!(screen.contains("Input: auto ▾") && !screen.contains("[Send]"), "one select button, no Send: {screen}");
        // The select is the only control: Tab reaches it and Enter opens its options; Enter in the draft still sends.
        app.pstate.get_mut("p").unwrap().sel = 0;
        app.panel_key("p", "p", KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), json!({}));
        app.panel_key("p", "p", KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), json!({}));
        assert!(matches!(app.overlay, Some(Overlay::Menu { .. })), "the select's options open");
    }

    #[tokio::test]
    async fn pane_titles_fall_back_to_the_programs_title() {
        let (mut app, _) = app();
        app.load(&json!({"epoch":"e","seq":1,"entries":{"view/t":{"value":{"kind":"terminal","resource":"r1"}}},
            "resources":[{"id":"r1","kind":"terminal","state":"live","argv":["/bin/bash"],"title":"vim notes.md"}]}));
        assert_eq!(app.view_title("t"), "vim notes.md");
        app.leaves = vec!["t".into()];
        let backend = ratatui::backend::TestBackend::new(60, 6);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 6));
        terminal.draw(|f| { app.draw_view(f, "t", Rect::new(0, 0, 60, 5), true, &Palette::default(), &Icons::new(None, false)); buf = f.buffer_mut().clone(); }).unwrap();
        let top: String = (0..60).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(top.contains(" vim notes.md "), "the border shows the program's title: {top}");
        app.load(&json!({"epoch":"e","seq":2,"entries":{"view/t":{"value":{"kind":"terminal","resource":"r1"}}},
            "resources":[{"id":"r1","kind":"terminal","state":"live","argv":["/bin/bash"],"title":" ","name":"build"}]}));
        assert_eq!(app.view_title("t"), "build", "a name the user gave wins; a blank title is no title");
    }

    #[tokio::test]
    async fn disconnected_command_events_drop_cached_names() {
        let(mut app,_)=app();let epoch=app.ws.epoch.clone();
        app.on_event_record(&json!({"epoch":epoch,"seq":1,"type":"command","data":{"name":"temporary","available":true}}));
        assert!(app.ws.commands.contains_key("temporary"));
        app.on_event_record(&json!({"epoch":epoch,"seq":2,"type":"command","data":{"name":"temporary","available":false}}));
        assert!(!app.ws.commands.contains_key("temporary"));
    }

    #[tokio::test]
    async fn text_inputs_require_explicit_prefill_and_preserve_rename_context() {
        let (mut app, _) = app();
        app.ws.entries.insert("workspace/active".into(), json!({"layout": "main"}));
        app.ws.entries.insert("layout/main".into(), json!({"tabs": [{"title": "Named tab", "node": {"view": "form"}}], "active": 0}));
        app.ws.entries.insert("view/form".into(), json!({"kind": "panel", "title": "Agent r8", "items": []}));
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
        app.ws.entries.insert("view/row-form".into(), json!({"kind": "panel", "items": [{"text": "Edit", "command": "any.edit", "input": "Value", "prefill": "row initial value"}]}));
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
    async fn owner_unset_hello_reaches_the_real_snapshot_and_renderer() {
        let root=tempfile::Builder::new().prefix("own-ui").tempdir().unwrap();let socket=root.path().join("s");
        let core=crate::server::start(socket.clone(),root.path().join("st"),false,crate::server::Backend::Fake,None).unwrap();
        let(owner,_,_)=client::connect_as(&socket,"owner","owner").await.unwrap();
        owner.call("workspace.apply",json!({"ops":[{"op":"delete","key":crate::principal::OWNER_KEY}]})).await.unwrap();
        let(peer,_,hello)=client::connect_with_identity(&socket,"owner-ui",Some(crate::principal::Identity::Owner)).await.unwrap();
        let snap=snapshot_for_client(&peer,&hello).await.unwrap();assert_eq!(snap["client_owner_unset"],true);
        let(tx,_)=mpsc::unbounded_channel();let mut app=App::new(peer,tx);app.load(&snap);
        let mut terminal=ratatui::Terminal::new(ratatui::backend::TestBackend::new(120,30)).unwrap();terminal.draw(|f|app.draw(f)).unwrap();
        let screen:String=terminal.backend().buffer().content.iter().map(|c|c.symbol()).collect();assert!(screen.contains("OWNER UNSET"));
        crate::server::shutdown(&core).await;
    }

    #[tokio::test]
    async fn owner_unset_notice_is_visible_without_extensions_and_in_safe_mode() {
        let(mut app,_)=app();
        app.load(&json!({"epoch":"boot","seq":1,"entries":{},"client_owner_unset":true}));
        let mut terminal=ratatui::Terminal::new(ratatui::backend::TestBackend::new(120,30)).unwrap();
        for safe in [false,true] {
            app.ws.safe_mode=safe;terminal.draw(|f|app.draw(f)).unwrap();
            let screen:String=terminal.backend().buffer().content.iter().map(|c|c.symbol()).collect();
            assert!(screen.contains("OWNER UNSET"),"safe={safe}: {screen}");assert!(screen.contains("bootstrap"));
        }
        // A cache refresh is not a new hello and must not silently assign this connection an owner.
        app.load(&json!({"epoch":"boot","seq":2,"entries":{}}));
        terminal.draw(|f|app.draw(f)).unwrap();
        let screen:String=terminal.backend().buffer().content.iter().map(|c|c.symbol()).collect();assert!(screen.contains("OWNER UNSET"));
        app.load(&json!({"epoch":"boot","seq":3,"entries":{},"client_owner_unset":false}));
        terminal.draw(|f|app.draw(f)).unwrap();
        let screen:String=terminal.backend().buffer().content.iter().map(|c|c.symbol()).collect();assert!(!screen.contains("OWNER UNSET"));
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
        app.ws.safe_mode=true;
        app.ws.entries.insert("binding/modified".into(),json!({"key":"C-S-Left","command":"client.focus","label":"modified"}));
        app.on_key(KeyEvent::new(KeyCode::Char('g'),KeyModifiers::CONTROL));
        let mut terminal=ratatui::Terminal::new(ratatui::backend::TestBackend::new(100,24)).unwrap();
        for (setting,prefix,modified) in [(None,"Ctrl-g","Ctrl-Shift-Left"),(Some("short"),"C-g","C-S-Left"),(Some("unknown"),"Ctrl-g","Ctrl-Shift-Left")] {
            if let Some(setting)=setting {app.ws.entries.insert("doc/settings/tui".into(),json!({"key_labels":setting}));}
            terminal.draw(|f|app.draw(f)).unwrap();
            let screen:String=terminal.backend().buffer().content.iter().map(|c|c.symbol()).collect();
            assert!(screen.contains(prefix),"{screen}");
            assert!(screen.contains(modified),"{screen}");
        }
        assert_eq!(app.prefix(),"C-g","matching/storage remains canonical");
        app.set_key_hint("", "C-g ,", " → settings");
        assert_eq!(app.message_label(),"Ctrl-g , → settings");
        app.ws.entries.insert("doc/settings/tui".into(),json!({"key_labels":"short"}));
        assert_eq!(app.message_label(),"C-g , → settings","cached hints follow the current choice");
        app.ws.entries.insert("doc/settings/tui".into(),json!({"key_labels":"full"}));
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
        app.ws.safe_mode=true;
        assert!(app.prefix_items().iter().any(|r|r.binding["command"]=="vqro.extension.restart"),"recovery remains reachable; core still controls safe-mode admission");
    }

    #[tokio::test]
    async fn command_focus_targets_a_visible_view_without_kind_policy() {
        let (mut app, _) = app();
        app.ws.entries.insert("workspace/active".into(), json!({"layout": "test"}));
        app.ws.entries.insert("layout/test".into(), json!({"split": "h", "children": [{"view": "list"}, {"view": "detail"}]}));
        for view in ["list", "detail"] {
            app.ws.entries.insert(format!("view/{view}"), json!({"kind": "panel", "items": []}));
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
    async fn the_focused_detail_is_a_full_panel_with_pages_filter_facets_and_sort() {
        let(mut app,_)=app();
        let provides=json!({"title":"Provides","facets":[{"key":"kind","label":"Show"}],"sorts":[{"key":"name","label":"name"}],"items":[
            {"text":"command · b","facets":{"kind":"command"},"sort":{"name":"b"}},{"text":"view · a","facets":{"kind":"view"},"sort":{"name":"a"}}]});
        let detail=json!({"title":"Ext","pages":[{"title":"About","items":[{"type":"text","text":"about","focusable":true}]},provides]});
        app.ws.entries.insert("view/test".into(),json!({"kind":"panel","list_detail":{"requires":["list_detail"]},"pages":[{"title":"P1","items":[{"id":"e","text":"Ext","detail_panel":detail}]},{"title":"P2","items":[]}]}));
        let key=|app:&mut App,code|app.panel_key("test","test",KeyEvent::new(code,KeyModifiers::NONE),json!({}));
        key(&mut app,KeyCode::Tab);assert!(app.pstate["test"].detail_focus);
        key(&mut app,KeyCode::Char(']'));
        let st=&app.pstate["test"];assert_eq!((st.page,st.detail.as_ref().unwrap().page,st.detail_focus),(0,1,true),"] moves the detail's pages, not the list's");
        key(&mut app,KeyCode::Char('f'));assert_eq!(app.pstate["test"].detail.as_ref().unwrap().facets.get("kind").map(String::as_str),Some("command"));
        key(&mut app,KeyCode::Char('s'));assert_eq!(app.pstate["test"].detail.as_ref().unwrap().sort,Some(0));
        key(&mut app,KeyCode::Char('/'));assert!(app.filtering.is_some()&&app.filter_detail);
        app.on_filter_key(KeyEvent::new(KeyCode::Char('b'),KeyModifiers::NONE));
        let st=&app.pstate["test"];assert_eq!((st.filter.as_str(),st.detail.as_ref().unwrap().filter.as_str()),("","b"),"/ filters the focused detail");
        app.on_filter_key(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE));
        key(&mut app,KeyCode::BackTab);assert!(!app.pstate["test"].detail_focus);
        key(&mut app,KeyCode::Char(']'));assert_eq!(app.pstate["test"].page,1,"with the list focused, ] moves the list's pages");
        // client.panel names a pane and acts on its facets and sort.
        let ctx=json!({"panel":"test","panel_view":"test"});
        app.pstate.get_mut("test").unwrap().page=0;
        app.run_command("client.panel",json!({"action":"facet","pane":"detail","key":"kind","value":"view"}),ctx.clone(),None);
        assert_eq!(app.pstate["test"].detail.as_ref().unwrap().facets.get("kind").map(String::as_str),Some("view"));
        app.run_command("client.panel",json!({"action":"sort","pane":"detail"}),ctx,None);assert_eq!(app.pstate["test"].detail.as_ref().unwrap().sort,None);
    }

    #[tokio::test]
    async fn generic_details_keep_chip_keys_paging_and_prefix_recovery() {
        let(mut app,_)=app();
        let items=(0..10).map(|i|json!({"id":format!("row{i}"),"text":format!("Row {i}"),"detail_panel":{"items":[],"toolbar":[{"type":"chips","chips":[{"text":"one","command":"client.quit"},{"text":"two"}]}]}})).collect::<Vec<_>>();
        app.ws.entries.insert("view/test".into(),json!({"kind":"panel","list_detail":{"requires":["list_detail"]},"items":items}));
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
    async fn composer_rows_edit_choose_controls_and_multiline_prompts() {
        let (mut app,_) = app();
        let composer = json!({"type":"composer","id":"c","command":"x.send","args":{"to":"r2"},"autofocus":true,
            "controls":[{"type":"chips","arg":"mode","chips":[{"text":"auto","value":"auto","checked":true},{"text":"steer","value":"steer"}]}]});
        app.load(&json!({"epoch":"e","seq":1,"entries":{"view/p":{"value":{"kind":"panel","items":[{"text":"msg"},composer]}}}}));
        let key = |c: KeyCode, m: KeyModifiers| KeyEvent::new(c, m);
        let (none, alt) = (KeyModifiers::NONE, KeyModifiers::ALT);
        // Not editing yet: Enter on the composer row starts typing.
        app.panel_key("p","p",key(KeyCode::Down,none),json!({}));
        app.panel_key("p","p",key(KeyCode::Enter,none),json!({}));
        assert_eq!(app.pstate["p"].editing.as_deref(),Some("c"));
        for c in "hi".chars() { app.panel_key("p","p",key(KeyCode::Char(c),none),json!({})); }
        app.panel_key("p","p",key(KeyCode::Enter,alt),json!({}));
        app.panel_key("p","p",key(KeyCode::Char('j'),KeyModifiers::CONTROL),json!({}));
        app.panel_key("p","p",key(KeyCode::Char('x'),none),json!({}));
        // `/`, `[` and `]` are text while typing, not filter or page keys.
        for c in "/[]".chars() { app.panel_key("p","p",key(KeyCode::Char(c),none),json!({})); }
        assert!(app.filtering.is_none());
        for _ in 0..3 { app.panel_key("p","p",key(KeyCode::Backspace,none),json!({})); }
        assert_eq!(app.pstate["p"].composers["c"].editor.string(),"hi\n\nx");
        // Esc leaves with the draft kept; the control chip sets a client-local value.
        app.panel_key("p","p",key(KeyCode::Esc,none),json!({}));
        assert!(app.pstate["p"].editing.is_none());
        app.panel_key("p","p",key(KeyCode::Right,none),json!({}));
        app.panel_key("p","p",key(KeyCode::Right,none),json!({}));
        app.panel_key("p","p",key(KeyCode::Enter,none),json!({}));
        let comp=&app.pstate["p"].composers["c"];
        assert_eq!(comp.values["mode"],"steer");
        assert_eq!(comp.submit_args(&composer),json!({"to":"r2","mode":"steer","text":"hi\n\nx"}));
        // client.composer (bindings use it): set and focus.
        app.run_command("client.composer",json!({"action":"set","panel":"p","arg":"mode","value":"auto"}),json!({}),None);
        assert_eq!(app.pstate["p"].composers["c"].values["mode"],"auto");
        app.run_command("client.composer",json!({"panel":"p"}),json!({}),None);
        assert_eq!(app.pstate["p"].editing.as_deref(),Some("c"));
        assert!(app.paste_text("pasted\ntwo") || app.leaves.is_empty(), "paste goes to the composer being typed into (when its pane is focused)");
        // client.panel navigation (what pane-scoped keys run): next/previous matching row, first/last.
        app.load(&json!({"epoch":"e","seq":2,"entries":{"view/p":{"value":{"kind":"panel","items":[
            {"text":"you: a","role":"user"},{"text":"pi: b"},{"text":"pi: c"},{"text":"you: d","role":"user"},{"text":"pi: e"}]}}}}));
        app.pstate.insert("p".into(), panel::State::default());
        let ctx = json!({"panel":"p","panel_view":"p"});
        app.run_command("client.panel",json!({"action":"next","match":{"role":"user"}}),ctx.clone(),None);
        assert_eq!(app.pstate["p"].sel,3);
        app.run_command("client.panel",json!({"action":"prev","match":{"role":"user"}}),ctx.clone(),None);
        assert_eq!(app.pstate["p"].sel,0);
        app.run_command("client.panel",json!({"action":"down"}),ctx.clone(),None);
        assert_eq!(app.pstate["p"].sel,1);
        app.run_command("client.panel",json!({"action":"end"}),ctx.clone(),None);
        assert_eq!(app.pstate["p"].sel,4);
        // Search (feature search): `/` on a panel with search: true, matches inside collapsed rows, n/N, Esc.
        app.load(&json!({"epoch":"e","seq":3,"entries":{"view/p":{"value":{"kind":"panel","search":true,"items":[
            {"text":"you: lockfile?"},{"id":"act","text":"Activity","children":[{"text":"cargo update lockfile"}]},{"text":"pi: done"}]}}}}));
        app.pstate.insert("p".into(), panel::State::default());
        app.panel_key("p","p",KeyEvent::new(KeyCode::Char('/'),KeyModifiers::NONE),json!({}));
        assert_eq!(app.filtering.as_deref(),Some("p"));
        for c in "lock".chars() { app.on_key(KeyEvent::new(KeyCode::Char(c),KeyModifiers::NONE)); }
        assert_eq!((app.pstate["p"].sel, app.message.as_str()), (0, "/lock  1 of 2  n/N  Esc"));
        app.on_key(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE));
        assert!(app.filtering.is_none());
        app.panel_key("p","p",KeyEvent::new(KeyCode::Char('n'),KeyModifiers::NONE),json!({}));
        assert_eq!(app.pstate["p"].open.get("act"),Some(&true),"the match's collapsed block opens");
        assert_eq!((app.pstate["p"].sel, app.message.as_str()), (2, "/lock  2 of 2  n/N  Esc"));
        app.panel_key("p","p",KeyEvent::new(KeyCode::Char('n'),KeyModifiers::NONE),json!({}));
        assert_eq!(app.pstate["p"].sel, 0, "n wraps");
        app.panel_key("p","p",KeyEvent::new(KeyCode::Esc,KeyModifiers::NONE),json!({}));
        assert!(app.pstate["p"].search.is_empty());
        // client.copy with text: the notice reports what was sent.
        app.run_command("client.copy",json!({"text":"héllo"}),json!({}),None);
        assert!(app.message.starts_with("copy: sent 5 characters"), "{}", app.message);
        // A multi-line prompt: the shared editor in an overlay.
        app.activate_item(Item::from_entry(&json!({"command":"x.answer","input":"Answer","multiline":true})),json!({}),None);
        for c in "a".chars() { app.on_overlay_key(key(KeyCode::Char(c),none)); }
        app.on_overlay_key(key(KeyCode::Enter,alt));
        app.on_overlay_key(key(KeyCode::Char('b'),none));
        assert!(matches!(&app.overlay,Some(Overlay::Input{multi:Some(ed),..}) if ed.string()=="a\nb"));
        app.on_overlay_key(key(KeyCode::Esc,none));
        assert!(app.overlay.is_none());
    }

    #[tokio::test]
    async fn unknown_view_kinds_draw_their_fallback_and_follow_its_live_content() {
        let (mut app,_) = app();
        app.load(&json!({"epoch":"e","seq":1,"entries":{
            "view/canvas":{"value":{"kind":"x.acme.canvas","fallback":"canvas.rows"}},
            "view/canvas.rows":{"value":{"kind":"panel","items":[{"text":"static"}]}},
            "view/odd":{"value":{"kind":"x.acme.odd"}}}}));
        assert_eq!(app.view_kind("canvas"),"panel");
        assert_eq!(app.panel_value("canvas")["items"][0]["text"],"static");
        assert_eq!(app.view_kind("odd"),"x.acme.odd","no fallback: the labelled placeholder");
        // Live content of the fallback view invalidates the pane that shows it.
        app.on_event_record(&json!({"epoch":"e","seq":2,"type":"view.content","data":{"view":"canvas.rows","content":{"items":[{"text":"live"}]}}}));
        assert_eq!(app.panel_value("canvas")["items"][0]["text"],"live");
        // A newer client claims the kind; this one keeps the fallback, and the original changing is seen too.
        app.on_event_record(&json!({"epoch":"e","seq":3,"type":"entries","data":{"author":"x","txn":1,"changes":[
            {"key":"view/canvas","version":2,"value":{"kind":"panel","items":[{"text":"now a panel"}]}}]}}));
        assert_eq!(app.panel_value("canvas")["items"][0]["text"],"now a panel");
    }

    #[tokio::test]
    async fn safe_chooser_names_needs_and_keeps_them_across_runtime_events() {
        let (mut app,_) = app();
        let deps=json!({"needs":[{"cap":"command/tabs.show","providers":[{"name":"tabs"}]},{"cap":"ability/vqro.terminals","providers":[{"name":"herdr"},{"name":"tmux"}]},
            {"cap":"ability/x.gpu","suggest":"gpu","providers":[]},{"cap":"command/vqro.revert","providers":[{"name":"vqro"}]},{"cap":"ui/vqro.items"}]});
        app.load(&json!({"epoch":"boot","seq":1,"safe_mode":true,"safe_mode_allow":[],"entries":{"extension/agents":{"value":{"description":"Agent controls","argv":["false"]}}},
            "extensions":{"agents":{"status":"stopped","dependencies":deps}}}));
        let row=|app:&App|app.panel_value("@vqro-safe-mode")["items"].as_array().unwrap().iter().find(|r|r["text"]=="[ ] agents").cloned().unwrap();
        assert_eq!(row(&app)["description"],"Agent controls -- needs tabs, herdr or tmux, gpu");
        app.on_event_record(&json!({"epoch":"boot","seq":2,"type":"extension","data":{"name":"agents","state":{"status":"stopped"}}}));
        assert!(row(&app)["description"].as_str().unwrap().ends_with("needs tabs, herdr or tmux, gpu"),"runtime events keep static dependency facts");
        assert!(app.safe_selected.is_empty(),"naming needs selects nothing");
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
        assert!(app.safe_selected.contains("herdr"));assert!(!app.ws.safe_mode_allow.contains("herdr"));
        assert_eq!(app.ws.entries["extension/herdr"]["enabled"],false);
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
        assert_eq!(app.ws.resources["r1"]["status"]["state"], "working");
        assert_eq!(app.ws.resources["r1"]["argv"][0], "pi", "unchanged fields are kept");
        let before = app.generation;
        app.on_incoming(resource_event(4, json!({"id": "r9", "status": null}), true));
        assert!(!app.ws.resources.contains_key("r9"));
        assert!(app.resyncing && app.generation > before, "a partial event without a base forces a resync");
    }

    #[tokio::test]
    async fn partial_status_cannot_restore_an_unavailable_terminal_but_availability_can() {
        let (mut app, _) = app();
        app.load(&snapshot(1, "x"));
        app.on_incoming(resource_event(2, json!({"id": "r1", "state": "live", "available": true, "status": null}), false));
        app.terms.insert("view".into(), Term { resource: "r1".into(), stream: Some(9), emu: new_parser(80, 24), size: (80, 24), attaching: false, closed: None, retry_at: None, modes: Default::default() });
        app.stream_view.insert(9, "view".into());
        app.on_incoming(resource_event(3, json!({"id": "r1", "available": false, "stale": true}), true));
        assert!(app.terms["view"].stream.is_none());
        assert!(app.terms["view"].closed.is_some());
        app.on_incoming(resource_event(4, json!({"id": "r1", "status": {"state": "working"}}), true));
        assert_eq!(app.ws.resources["r1"]["available"], false);
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
        app.terms.insert("view".into(), Term { resource:"r1".into(), stream:None, emu:new_parser(80,24), size:(80,24), attaching:true, closed:None, retry_at:None, modes:Default::default() });
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
            assert_eq!(app.ws.entries["doc/test/value"],"old","retry wait must remain fenced");
            let before=app.generation;
            app.on_incoming(Incoming::Notification{method:"missed".into(),params:Value::Null});
            assert_eq!(app.generation,before,"events must not bypass backoff");
            app.retry_snapshot=Some(std::time::Instant::now());app.retry_snapshot_if_due();
            assert!(app.resyncing&&app.generation>before);
            app.on_incoming(delta(4,"new"));
            app.on_msg(Msg::Snapshot{generation:app.generation,result:Ok(snapshot(3,"base"))});
            assert_eq!(app.ws.entries["doc/test/value"],"new");assert_eq!(app.ws.seq,4);
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
        assert_eq!(app.ws.entries["doc/test/value"], "old", "deltas are fenced while snapshot is in flight");
        app.on_msg(Msg::Snapshot { generation: app.generation, result: Ok(snapshot(1, "old")) });
        assert_eq!(app.ws.entries["doc/test/value"], "new");
        assert_eq!(app.ws.seq, 2);
        app.on_incoming(delta(1, "stale"));
        assert_eq!(app.ws.entries["doc/test/value"], "new", "pre-snapshot events cannot regress state");
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
        assert_eq!(app.ws.entries["doc/test/value"], "current");
        app.disconnected();
        app.on_msg(Msg::Snapshot { generation: first, result: Ok(snapshot(1, "stale")) });
        assert_eq!(app.ws.entries["doc/test/value"], "current");
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
            resource: "r1".into(), stream: None, emu: new_parser(80, 24),
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
            resource: "r1".into(), stream: Some(99), emu: new_parser(80, 24),
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
                if app.terms.values().any(|t| t.stream.is_some() && t.emu.contents().contains("fake f1")) {
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
                if term.stream.is_some_and(|new| new != sid) && term.emu.contents().contains("fake f1") { break; }
            }
        }).await.unwrap();
    }

    #[tokio::test]
    async fn closure_before_attach_reply_cannot_adopt_a_closed_stream() {
        let (mut app, _) = app();
        app.terms.insert("view".into(), Term {
            resource: "r1".into(), stream: None, emu: new_parser(80, 24),
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
            resource: "r1".into(), stream: Some(1), emu: new_parser(80, 24),
            size: (80, 24), attaching: false, closed: None, retry_at: None, modes: Default::default(),
        };
        term.emu.process(b"old\x1b[");
        term.close("slow_consumer".into());
        let now = std::time::Instant::now();
        assert!(!term.can_attach(true, now), "retry is paced");
        assert!(!term.can_attach(false, now + std::time::Duration::from_secs(2)));
        assert!(term.can_attach(true, now + std::time::Duration::from_secs(2)));
        term.begin_attach();
        assert!(term.attaching);
        assert!(term.closed.is_none());
        term.emu.process(b"repaint");
        assert_eq!(term.emu.contents(), "repaint", "fresh attach cannot reuse old VT state");
    }

    #[tokio::test]
    async fn bindings_honour_when_and_fall_through_while_profiles_keep_winning() {
        let (mut app, _) = app();
        app.ws.entries.insert("workspace/active".into(), json!({"layout": "main"}));
        app.ws.entries.insert("layout/main".into(), json!({"split": "h", "children": [{"view": "agents-r8"}, {"view": "t1"}]}));
        app.ws.entries.insert("view/agents-r8".into(), json!({"kind": "panel", "title": "Agent r8", "resource": "r8", "items": []}));
        app.ws.entries.insert("view/t1".into(), json!({"kind": "terminal", "resource": "r1"}));
        app.ws.resources.insert("r8".into(), json!({"id": "r8", "kind": "agent", "state": "live", "available": true}));
        app.ws.resources.insert("r1".into(), json!({"id": "r1", "kind": "terminal", "state": "live", "available": true}));
        app.ws.entries.insert("binding/agents.prompt".into(), json!({"key": "p", "command": "agents.prompt", "input": "Prompt agent", "label": "prompt agent", "when": {"resource_kind": "agent"}}));
        for name in ["agents.prompt", "x.paste"] {
            app.ws.commands.insert(name.into(), json!({"name": name, "available": true}));
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
        app.ws.entries.insert("binding/z.p".into(), json!({"key": "p", "command": "x.paste"}));
        assert_eq!(on(&app, agent).as_deref(), Some("agents.prompt"), "same priority: lowest id");
        assert_eq!(on(&app, shell).as_deref(), Some("x.paste"));
        // A selected keymap profile (priority 10, no `when`) keeps winning everywhere.
        app.ws.entries.insert("binding/keys.p".into(), json!({"key": "p", "command": "client.tab", "args": {"to": "prev"}, "priority": 10, "owner": "keys"}));
        app.ws.entries.insert("extension/keys".into(), json!({"argv": ["x"]}));
        assert_eq!(on(&app, agent).as_deref(), Some("client.tab"));
        assert_eq!(on(&app, shell).as_deref(), Some("client.tab"));
        // Shift-p is its own key: P opens Agent settings on agent panes while a profile owns p.
        app.ws.entries.insert("binding/agents.config".into(), json!({"key": "P", "command": "agents.config", "label": "agent settings", "when": {"resource_kind": "agent"}}));
        app.ws.commands.insert("agents.config".into(), json!({"name": "agents.config", "available": true}));
        let shift_p = KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT);
        assert_eq!(keys::name(&shift_p).as_deref(), Some("P"));
        assert_eq!(app.binding_for("P", &app.ctx_leaf(agent)).map(|b| b["command"].clone()), Some(json!("agents.config")));
        assert_eq!(app.binding_for("P", &app.ctx_leaf(shell)), None);
        app.focus = agent;
        assert!(app.prefix_items().iter().any(|r| r.binding["command"] == "agents.config" && r.key.as_deref() == Some("P")));
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
    async fn output_right_after_typing_into_its_stream_is_an_echo_drawn_at_once() {
        let (mut app, _) = app();
        let data = |sid: u64| Incoming::Notification { method: "stream.data".into(), params: json!({"stream": sid, "data": "eA=="}) };
        assert!(!app.echo(&data(7)), "nothing typed yet");
        app.send_input(7, b"x".to_vec());
        assert!(app.echo(&data(7)));
        assert!(!app.echo(&data(8)), "another pane's output stays background");
        assert!(!app.echo(&Incoming::Notification { method: "stream.closed".into(), params: json!({"stream": 7}) }));
        app.typed = Some((7, std::time::Instant::now() - ECHO_WINDOW));
        assert!(!app.echo(&data(7)), "a program streaming after the key is background again");
    }

    #[test]
    fn pick_filter_rows_and_titles() {
        let rows: Vec<String> = ["Alpha", "beta", "GAMMA", "alphabet"].iter().map(|s| s.to_string()).collect();
        assert_eq!(filter_rows(&rows, ""), vec![0, 1, 2, 3]);
        assert_eq!(filter_rows(&rows, "ALPHA"), vec![0, 3], "case-insensitive substring");
        assert_eq!(filter_rows(&rows, "zz"), Vec::<usize>::new());
        assert_eq!(pick_title(" ", 3, 3, ""), (" ".to_string(), None), "short lists stay plain");
        assert_eq!(pick_title(" ", 11, 11, ""), (" 11 of 11 · type to filter ".to_string(), Some(String::new())));
        assert_eq!(pick_title(" pick a terminal ", 2, 40, "r1"), (" pick a terminal · 2 of 40 · Esc clears ".to_string(), Some("r1".into())));
    }

    #[tokio::test]
    async fn long_option_menus_filter_as_you_type_and_esc_clears_before_closing() {
        let (mut app, _) = app();
        let options: Vec<Value> = (0..24).map(|i| json!({"label": format!("Model {i:02}"), "value": format!("m{i:02}"), "input": format!("chosen m{i:02}")})).collect();
        app.options_menu(&json!({"command": "x.model", "value": "m03", "options": options}), json!({}), None);
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 40)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let screen: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(screen.contains("24 of 24") && screen.contains("type to filter"), "{screen}");
        for c in ['1', '7'] {
            app.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.overlay_len(), 1);
        terminal.draw(|f| app.draw(f)).unwrap();
        let screen: String = terminal.backend().buffer().content.iter().map(|c| c.symbol()).collect();
        assert!(screen.contains("1 of 24") && screen.contains("> 17") && screen.contains("Model 17") && !screen.contains("Model 16"), "{screen}");
        // Backspace widens again; arrows move within the kept rows.
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.overlay_len(), 12, "\"1\": 01, 10-19 and 21");
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));
        assert!(matches!(&app.overlay, Some(Overlay::Input { prompt, .. }) if prompt == "chosen m10"), "Enter runs the selected kept row");
        // Esc clears the query first, then closes.
        app.overlay = None;
        app.options_menu(&json!({"command": "x.model", "options": (0..24).map(|i| json!({"label": format!("Model {i:02}"), "value": i})).collect::<Vec<_>>()}), json!({}), None);
        app.on_key(key(KeyCode::Char('z')));
        assert_eq!(app.overlay_len(), 0);
        app.on_key(key(KeyCode::Enter));
        assert!(matches!(&app.overlay, Some(Overlay::Menu { query, .. }) if query == "z"), "Enter with no match runs nothing and keeps the text");
        app.on_key(key(KeyCode::Esc));
        assert!(matches!(&app.overlay, Some(Overlay::Menu { query, .. }) if query.is_empty()));
        assert_eq!(app.overlay_len(), 24);
        app.on_key(key(KeyCode::Esc));
        assert!(app.overlay.is_none());
    }

    #[tokio::test]
    async fn short_menus_ignore_typing_and_the_resource_picker_filters_too() {
        let (mut app, _) = app();
        app.options_menu(&json!({"command": "x.pick", "options": [{"label": "Alpha", "value": "a"}, {"label": "Beta", "value": "b"}]}), json!({}), None);
        app.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert!(matches!(&app.overlay, Some(Overlay::Menu { query, .. }) if query.is_empty()));
        assert_eq!(app.overlay_len(), 2);
        app.overlay = None;
        for i in 1..=12 {
            app.ws.resources.insert(format!("r{i}"), json!({"id": format!("r{i}"), "name": format!("shell {i}"), "state": "live"}));
        }
        app.activate_item(Item::from_entry(&json!({"command": "x.show", "pick": "resource"})), json!({}), None);
        for c in "shell 12".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let keys: Vec<&String> = app.ws.resources.keys().collect();
        assert_eq!(app.pick_matches().iter().map(|&i| keys[i].as_str()).collect::<Vec<_>>(), vec!["r12"]);
        // Ctrl/Alt chords are not typed into the query.
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT));
        assert!(matches!(&app.overlay, Some(Overlay::Pick { query, .. }) if query == "shell 12"));
    }

    #[tokio::test]
    async fn a_dropdown_is_drawn_over_its_popup_and_the_prefix_reaches_recovery_from_it() {
        let (mut app, _) = app();
        app.ws.entries.insert("view/pop".into(), json!({"kind": "panel", "title": "Popup Title", "items": [
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
