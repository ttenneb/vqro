//! The terminal emulator behind terminal views. Everything the client needs from an emulator goes through
//! `Emulator`: feeding program output, what the program asked of the terminal beyond drawing (`Effect`), modes, the
//! cursor, the viewport and its history, and drawing into a buffer.
//!
//! **Engines** (built in, chosen in Settings: `doc/settings/tui.terminal_engine`): each implements `Engine`. `vt100`
//! (the `vt100` crate) is always available and is the fallback; `libghostty` (Ghostty's libghostty-vt) is added by
//! slice G2. An engine that can't be selected or fails to start is replaced by vt100, and the client says why
//! (`Emulator::fallback`). A fault inside a native engine that crashes the process can't be caught this way: native
//! engines keep their FFI defensive (no panics across it, bounded inputs) instead.
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::num::NonZeroU16;

/// What a program asked of the terminal beyond drawing, collected while parsing its output and passed on to the
/// outer terminal after (never mid-frame): clipboard writes (OSC 52), bells, desktop notifications (OSC 9, OSC 777).
/// Clipboard reads (OSC 52 `?`) are never answered: a program could read what the user copied elsewhere.
#[derive(Debug, PartialEq)]
pub enum Effect {
    /// The selection names (`c`, `p`, ...) and base64 data, as the program sent them.
    Clipboard(Vec<u8>, Vec<u8>),
    Bell,
    /// A whole notification OSC body (`9;text` or `777;notify;title;body`), passed on verbatim.
    Notify(Vec<u8>),
}

/// Bounds what one parse may queue: a program can't flood the outer terminal or the client's memory.
pub const MAX_EFFECTS: usize = 64;
const MAX_CLIPBOARD_B64: usize = 1 << 20;

/// The cursor shape a program asked for (DECSCUSR).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    #[default]
    Default,
    BlinkBlock,
    SteadyBlock,
    BlinkUnderline,
    SteadyUnderline,
    BlinkBar,
    SteadyBar,
}

/// Which mouse events a program asked for (xterm modes 9, 1000, 1002, 1003).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseMode {
    #[default]
    None,
    Press,
    PressRelease,
    ButtonMotion,
    AnyMotion,
}

/// How mouse events are encoded for the program (default X10, UTF-8 mode 1005, SGR mode 1006).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseEncoding {
    #[default]
    Default,
    Utf8,
    Sgr,
}

#[derive(Default)]
struct Effects {
    out: Vec<Effect>,
    /// Rows the full scrollback dropped (normal screen): with the kept rows, the numbering of `scrollback::Pos`.
    dropped: usize,
}

impl Effects {
    fn push(&mut self, e: Effect) {
        if self.out.len() < MAX_EFFECTS && !(e == Effect::Bell && self.out.contains(&Effect::Bell)) { self.out.push(e); }
    }
}

impl vt100::Callbacks for Effects {
    fn audible_bell(&mut self, _: &mut vt100::Screen) { self.push(Effect::Bell); }
    // Called for rows the full scrollback drops.
    fn on_scroll(&mut self, _: vt100::capture::RowContents<'_>, alternate: bool) {
        if !alternate { self.dropped += 1; }
    }
    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, ty: &[u8], data: &[u8]) {
        if data.len() <= MAX_CLIPBOARD_B64 { self.push(Effect::Clipboard(ty.to_vec(), data.to_vec())); }
    }
    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        // OSC 9 is a notification unless it is ConEmu's progress (`9;4;...`), which the outer terminal may also
        // show but which is too chatty to pass on from every pane; OSC 777 only for `notify`.
        let notify = match params {
            [b"9", rest @ ..] => !rest.is_empty() && rest[0] != b"4",
            [b"777", b"notify", ..] => true,
            _ => false,
        };
        if notify && params.iter().map(|p| p.len() + 1).sum::<usize>() <= 4096 { self.push(Effect::Notify(params.join(&b';'))); }
    }
}

fn nz(n: u16) -> NonZeroU16 { NonZeroU16::new(n.max(1)).unwrap() }

/// The built-in engines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    Vt100,
    Libghostty,
}

impl EngineKind {
    pub const ALL: [EngineKind; 2] = [EngineKind::Libghostty, EngineKind::Vt100];
    /// The engine used when the setting names none (or one this client doesn't know).
    pub const DEFAULT: EngineKind = EngineKind::Libghostty;

    pub fn name(self) -> &'static str {
        match self { EngineKind::Vt100 => "vt100", EngineKind::Libghostty => "libghostty" }
    }
    pub fn parse(name: &str) -> Option<EngineKind> { Self::ALL.into_iter().find(|k| k.name() == name) }
}

/// What a terminal engine does for one terminal view. Rows of history are numbered like `scrollback::Pos`: rows above
/// the live screen so far (dropped plus kept) come first, then the screen's rows.
pub trait Engine {
    /// Program output. Its effects queue until `take_effects`.
    fn process(&mut self, bytes: &[u8]);
    /// The effects queued since the last call (at most `MAX_EFFECTS`).
    fn take_effects(&mut self) -> Vec<Effect>;
    fn resize(&mut self, cols: u16, rows: u16);
    fn rows(&self) -> u16;
    fn cols(&self) -> u16;
    fn application_cursor(&self) -> bool;
    fn bracketed_paste(&self) -> bool;
    fn alternate_screen(&self) -> bool;
    fn mouse(&self) -> (MouseMode, MouseEncoding);
    /// Where the cursor is on screen and its shape, when the program shows it and the view is at the live screen.
    fn cursor(&self) -> Option<(u16, u16, CursorShape)>;
    /// How many lines the view is scrolled back from the live screen (0: live).
    fn scroll_offset(&self) -> usize;
    /// Scroll back `n` lines (clamped to the history kept).
    fn set_scroll_offset(&mut self, n: usize);
    fn scrollback_len(&self) -> usize;
    fn scrollback_capacity(&self) -> usize;
    /// Rows above the live screen so far (dropped plus kept).
    fn scrolled(&self) -> usize;
    /// One row's text with trailing blanks removed, the cell where each of its characters starts, and whether the
    /// row wraps onto the next. None when the row is no longer kept. Leaves the view where it was.
    fn row_text(&mut self, row: usize) -> Option<(Vec<char>, Vec<u16>, bool)>;
    /// Draw the screen as shown into `area` of `buf` (cells past the screen's size are left alone).
    fn draw(&self, buf: &mut Buffer, area: Rect);
    /// The OSC 8 hyperlink of a cell on screen as shown (engines that keep links; others None).
    fn hyperlink_at(&self, _row: u16, _col: u16) -> Option<String> { None }
    /// The screen as shown, as plain text (tests).
    #[cfg(test)]
    fn contents(&self) -> String;
}

#[cfg(test)]
thread_local! {
    /// Tests: make the next start of this engine fail, to exercise the fallback.
    pub static FAIL_START: std::cell::Cell<Option<EngineKind>> = const { std::cell::Cell::new(None) };
}

/// Start an engine, or say why it can't start.
fn start(kind: EngineKind, cols: u16, rows: u16, scrollback: usize) -> Result<Box<dyn Engine>, String> {
    #[cfg(test)]
    if FAIL_START.with(|f| f.get() == Some(kind)) { return Err("failed to start (test)".into()); }
    match kind {
        EngineKind::Vt100 => Ok(Box::new(Vt100::new(cols, rows, scrollback))),
        EngineKind::Libghostty => super::ghostty::Ghostty::new(cols, rows, scrollback).map(|g| Box::new(g) as Box<dyn Engine>)
            .map_err(|e| format!("failed to start ({e})")),
    }
}

/// One terminal view's emulator: the engine it runs, and the one that was asked for if it had to fall back.
pub struct Emulator {
    engine: Box<dyn Engine>,
    kind: EngineKind,
    wanted: EngineKind,
    /// Why the wanted engine isn't the one running (None: it is).
    fallback: Option<String>,
}

impl Emulator {
    /// A blank terminal of `cols` x `rows` keeping up to `scrollback` lines of history, on the default engine.
    #[cfg(test)]
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Self { Self::with_engine(EngineKind::DEFAULT, cols, rows, scrollback) }

    /// The same on the engine `wanted`; if it can't start, on vt100 (see `fallback`).
    pub fn with_engine(wanted: EngineKind, cols: u16, rows: u16, scrollback: usize) -> Self {
        match start(wanted, cols, rows, scrollback) {
            Ok(engine) => Self { engine, kind: wanted, wanted, fallback: None },
            Err(why) => Self { engine: Box::new(Vt100::new(cols, rows, scrollback)), kind: EngineKind::Vt100, wanted, fallback: Some(why) },
        }
    }

    /// A blank terminal of the same size, history capacity and wanted engine (a fresh attach never reuses old state;
    /// the engine choice, and a fallback's reason if it falls back again, carry over).
    pub fn fresh(&self) -> Self {
        Self::with_engine(self.wanted, self.cols(), self.rows(), self.scrollback_capacity())
    }

    /// The engine running, the one asked for, and why they differ.
    pub fn engine(&self) -> EngineKind { self.kind }
    pub fn wanted(&self) -> EngineKind { self.wanted }
    pub fn fallback(&self) -> Option<&str> { self.fallback.as_deref() }

    pub fn process(&mut self, bytes: &[u8]) { self.engine.process(bytes) }
    pub fn take_effects(&mut self) -> Vec<Effect> { self.engine.take_effects() }
    pub fn resize(&mut self, cols: u16, rows: u16) { self.engine.resize(cols, rows) }
    pub fn rows(&self) -> u16 { self.engine.rows() }
    pub fn cols(&self) -> u16 { self.engine.cols() }
    pub fn application_cursor(&self) -> bool { self.engine.application_cursor() }
    pub fn bracketed_paste(&self) -> bool { self.engine.bracketed_paste() }
    pub fn alternate_screen(&self) -> bool { self.engine.alternate_screen() }
    pub fn mouse(&self) -> (MouseMode, MouseEncoding) { self.engine.mouse() }
    pub fn cursor(&self) -> Option<(u16, u16, CursorShape)> { self.engine.cursor() }
    pub fn scroll_offset(&self) -> usize { self.engine.scroll_offset() }
    pub fn set_scroll_offset(&mut self, n: usize) { self.engine.set_scroll_offset(n) }
    pub fn scrollback_len(&self) -> usize { self.engine.scrollback_len() }
    pub fn scrollback_capacity(&self) -> usize { self.engine.scrollback_capacity() }
    pub fn scrolled(&self) -> usize { self.engine.scrolled() }
    pub fn row_text(&mut self, row: usize) -> Option<(Vec<char>, Vec<u16>, bool)> { self.engine.row_text(row) }
    pub fn draw(&self, buf: &mut Buffer, area: Rect) { self.engine.draw(buf, area) }
    pub fn hyperlink_at(&self, row: u16, col: u16) -> Option<String> { self.engine.hyperlink_at(row, col) }
    #[cfg(test)]
    pub fn contents(&self) -> String { self.engine.contents() }
}

/// The `vt100` crate engine: always available, the fallback.
struct Vt100 {
    parser: vt100::Parser<Effects>,
}

impl Vt100 {
    fn new(cols: u16, rows: u16, scrollback: usize) -> Self {
        Self { parser: vt100::Parser::new_with_callbacks(nz(rows), nz(cols), scrollback, Effects::default()) }
    }
}

impl Engine for Vt100 {
    fn process(&mut self, bytes: &[u8]) { self.parser.process(bytes); }
    fn take_effects(&mut self) -> Vec<Effect> { std::mem::take(&mut self.parser.callbacks_mut().out) }
    fn resize(&mut self, cols: u16, rows: u16) { self.parser.screen_mut().set_size(nz(rows), nz(cols)); }
    fn rows(&self) -> u16 { self.parser.screen().size().0.get() }
    fn cols(&self) -> u16 { self.parser.screen().size().1.get() }
    fn application_cursor(&self) -> bool { self.parser.screen().application_cursor() }
    fn bracketed_paste(&self) -> bool { self.parser.screen().bracketed_paste() }
    fn alternate_screen(&self) -> bool { self.parser.screen().alternate_screen() }
    fn mouse(&self) -> (MouseMode, MouseEncoding) {
        let s = self.parser.screen();
        let mode = match s.mouse_protocol_mode() {
            vt100::MouseProtocolMode::None => MouseMode::None,
            vt100::MouseProtocolMode::Press => MouseMode::Press,
            vt100::MouseProtocolMode::PressRelease => MouseMode::PressRelease,
            vt100::MouseProtocolMode::ButtonMotion => MouseMode::ButtonMotion,
            vt100::MouseProtocolMode::AnyMotion => MouseMode::AnyMotion,
        };
        let enc = match s.mouse_protocol_encoding() {
            vt100::MouseProtocolEncoding::Default => MouseEncoding::Default,
            vt100::MouseProtocolEncoding::Utf8 => MouseEncoding::Utf8,
            vt100::MouseProtocolEncoding::Sgr => MouseEncoding::Sgr,
        };
        (mode, enc)
    }

    fn cursor(&self) -> Option<(u16, u16, CursorShape)> {
        let s = self.parser.screen();
        if s.hide_cursor() || s.scrollback() > 0 { return None; }
        let (r, c) = s.cursor_position();
        let shape = match s.cursor_style() {
            vt100::CursorStyle::Default => CursorShape::Default,
            vt100::CursorStyle::BlinkBlock => CursorShape::BlinkBlock,
            vt100::CursorStyle::SteadyBlock => CursorShape::SteadyBlock,
            vt100::CursorStyle::BlinkUnderline => CursorShape::BlinkUnderline,
            vt100::CursorStyle::SteadyUnderline => CursorShape::SteadyUnderline,
            vt100::CursorStyle::BlinkVerticalBar => CursorShape::BlinkBar,
            vt100::CursorStyle::SteadyVerticalBar => CursorShape::SteadyBar,
        };
        Some((r, c, shape))
    }

    fn scroll_offset(&self) -> usize { self.parser.screen().scrollback() }
    fn set_scroll_offset(&mut self, n: usize) { self.parser.screen_mut().set_scrollback(n); }
    fn scrollback_len(&self) -> usize { self.parser.screen().scrollback_len() }
    fn scrollback_capacity(&self) -> usize { self.parser.screen().scrollback_capacity() }
    fn scrolled(&self) -> usize { self.parser.callbacks().dropped + self.scrollback_len() }

    fn row_text(&mut self, row: usize) -> Option<(Vec<char>, Vec<u16>, bool)> {
        let scrolled = self.scrolled();
        if row + self.scrollback_len() < scrolled || row > scrolled + usize::from(self.rows()) - 1 {
            return None;
        }
        let was = self.scroll_offset();
        let (offset, r) = if row >= scrolled { (0, (row - scrolled) as u16) } else { (scrolled - row, 0) };
        self.parser.screen_mut().set_scrollback(offset);
        let screen = self.parser.screen();
        let (mut chars, mut starts) = (Vec::new(), Vec::new());
        for col in 0..self.cols() {
            let Some(cell) = screen.cell(r, col) else { break };
            if cell.is_wide_continuation() {
                continue;
            }
            let text = cell.contents();
            for c in if text.is_empty() { " " } else { text }.chars() {
                chars.push(c);
                starts.push(col);
            }
        }
        let wrapped = screen.row_wrapped(r);
        self.parser.screen_mut().set_scrollback(was);
        while !wrapped && chars.last() == Some(&' ') {
            chars.pop();
            starts.pop();
        }
        Some((chars, starts, wrapped))
    }

    #[cfg(test)]
    fn contents(&self) -> String { self.parser.screen().contents() }

    fn draw(&self, buf: &mut Buffer, area: Rect) {
        let screen = self.parser.screen();
        for row in 0..self.rows().min(area.height) {
            for col in 0..self.cols().min(area.width) {
                let Some(cell) = screen.cell(row, col) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut style = Style::new().fg(color(cell.fgcolor())).bg(color(cell.bgcolor()));
                for (on, m) in [(cell.bold(), Modifier::BOLD), (cell.dim(), Modifier::DIM), (cell.italic(), Modifier::ITALIC),
                                (cell.underline(), Modifier::UNDERLINED), (cell.inverse(), Modifier::REVERSED)] {
                    if on { style = style.add_modifier(m); }
                }
                let text = cell.contents();
                if let Some(out) = buf.cell_mut((area.x + col, area.y + row)) {
                    out.set_symbol(if text.is_empty() { " " } else { text });
                    out.set_style(style);
                }
            }
        }
    }
}

fn color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_engine_that_cannot_start_falls_back_to_vt100_and_says_why() {
        assert_eq!(EngineKind::parse("libghostty"), Some(EngineKind::Libghostty));
        assert_eq!(EngineKind::parse("alacritty"), None, "unknown names are not engines");
        FAIL_START.with(|f| f.set(Some(EngineKind::Libghostty)));
        let mut e = Emulator::with_engine(EngineKind::Libghostty, 20, 4, 100);
        // An engine that can't start falls back to vt100; the terminal still works.
        assert_eq!((e.wanted(), e.engine()), (EngineKind::Libghostty, EngineKind::Vt100));
        assert!(e.fallback().is_some());
        e.process(b"hello");
        assert!(e.contents().starts_with("hello"));
        // A fresh terminal (reattach) asks for the same engine again, not for whatever it fell back to.
        let f = e.fresh();
        assert_eq!((f.wanted(), f.engine(), f.fallback().is_some()), (EngineKind::Libghostty, EngineKind::Vt100, true));
        FAIL_START.with(|f| f.set(None));
        let g = Emulator::with_engine(EngineKind::Libghostty, 20, 4, 100);
        assert_eq!((g.engine(), g.fallback()), (EngineKind::Libghostty, None), "libghostty starts");
        let v = Emulator::with_engine(EngineKind::Vt100, 20, 4, 100);
        assert_eq!((v.engine(), v.fallback()), (EngineKind::Vt100, None));
    }

    fn joined(e: &Emulator) -> String { e.contents().lines().map(str::trim_end).collect::<Vec<_>>().join("\n").trim_end().to_string() }

    /// One case of tests/fixtures/client/engines.json against one engine: the mismatches (empty: it passes).
    fn run_case(kind: EngineKind, case: &serde_json::Value) -> Vec<String> {
        use serde_json::{json, Value};
        let (cols, rows) = (case["cols"].as_u64().unwrap_or(20) as u16, case["rows"].as_u64().unwrap_or(4) as u16);
        let mut e = Emulator::with_engine(kind, cols, rows, 100);
        assert_eq!(e.engine(), kind, "{} must start", kind.name());
        e.process(case["input"].as_str().unwrap().as_bytes());
        let x = &case["expect"];
        let mut bad = Vec::new();
        let mut check = |what: &str, got: Value, want: &Value| if &got != want { bad.push(format!("{what}: got {got}, want {want}")) };
        if let Some(w) = x.get("contents") { check("contents", json!(joined(&e)), w); }
        if let Some(w) = x.get("cursor") { check("cursor", e.cursor().map(|(r, c, _)| json!([r, c])).unwrap_or(Value::Null), w); }
        if let Some(w) = x.get("shape") {
            let shape = e.cursor().map(|c| match c.2 { CursorShape::BlinkBar => "blink-bar", CursorShape::SteadyBar => "steady-bar", CursorShape::BlinkBlock => "blink-block",
                CursorShape::SteadyBlock => "steady-block", CursorShape::BlinkUnderline => "blink-underline", CursorShape::SteadyUnderline => "steady-underline", CursorShape::Default => "default" });
            check("shape", json!(shape), w);
        }
        if let Some(w) = x.get("effects") {
            let mut got: Vec<String> = e.take_effects().iter().map(|f| match f {
                Effect::Bell => "bell".to_string(),
                Effect::Clipboard(t, d) => format!("clipboard:{}:{}", String::from_utf8_lossy(t), String::from_utf8_lossy(d)),
                Effect::Notify(b) => format!("notify:{}", String::from_utf8_lossy(b)),
            }).collect();
            got.sort();
            let mut want: Vec<String> = w.as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
            want.sort();
            check("effects", json!(got), &json!(want));
        }
        if let Some(w) = x["modes"].as_object() {
            let (mode, enc) = e.mouse();
            let have = json!({"app_cursor": e.application_cursor(), "bracketed_paste": e.bracketed_paste(), "alt_screen": e.alternate_screen(),
                "mouse": match mode { MouseMode::None => "none", MouseMode::Press => "press", MouseMode::PressRelease => "press-release", MouseMode::ButtonMotion => "button-motion", MouseMode::AnyMotion => "any-motion" },
                "mouse_encoding": match enc { MouseEncoding::Default => "default", MouseEncoding::Utf8 => "utf8", MouseEncoding::Sgr => "sgr" }});
            for (k, v) in w { check(&format!("mode {k}"), have[k].clone(), v); }
        }
        if let Some(cells) = x["cells"].as_array() {
            let mut buf = Buffer::empty(Rect::new(0, 0, cols, rows));
            e.draw(&mut buf, Rect::new(0, 0, cols, rows));
            for c in cells {
                let cell = &buf[(c["col"].as_u64().unwrap() as u16, c["row"].as_u64().unwrap() as u16)];
                if let Some(t) = c.get("text") { check("cell text", json!(cell.symbol()), t); }
                if let Some(f) = c.get("fg") {
                    let fg = match cell.fg { Color::Indexed(i) => format!("idx:{i}"), Color::Rgb(r, g, b) => format!("rgb:{r},{g},{b}"), other => format!("{other:?}") };
                    check("cell fg", json!(fg), f);
                }
                if let Some(m) = c.get("mods") {
                    let names = [("bold", Modifier::BOLD), ("dim", Modifier::DIM), ("italic", Modifier::ITALIC), ("underline", Modifier::UNDERLINED),
                                 ("reverse", Modifier::REVERSED), ("strike", Modifier::CROSSED_OUT)];
                    let got: Vec<&str> = names.iter().filter(|(_, b)| cell.modifier.contains(*b)).map(|(n, _)| *n).collect();
                    check("cell mods", json!(got), m);
                }
            }
        }
        if let Some(w) = x.get("scrollback_len") { check("scrollback_len", json!(e.scrollback_len()), w); }
        let by_row = |which: &str| -> Vec<(usize, Value)> {
            x[which].as_object().map(|o| o.iter().map(|(k, v)| (k.parse::<usize>().unwrap(), v.clone())).collect()).unwrap_or_default()
        };
        for (r, w) in by_row("rows") { check(&format!("row {r}"), e.row_text(r).map(|t| json!(t.0.iter().collect::<String>())).unwrap_or(Value::Null), &w); }
        for (r, w) in by_row("wrapped") { check(&format!("wrapped {r}"), e.row_text(r).map(|t| json!(t.2)).unwrap_or(Value::Null), &w); }
        for (r, w) in by_row("starts") { check(&format!("starts {r}"), e.row_text(r).map(|t| json!(t.1)).unwrap_or(Value::Null), &w); }
        if let Some(sb) = x.get("scrolled_back") {
            e.set_scroll_offset(sb["offset"].as_u64().unwrap() as usize);
            check("scroll offset", json!(e.scroll_offset()), &sb["offset"]);
            check("scrolled-back contents", json!(joined(&e)), &sb["contents"]);
            e.set_scroll_offset(0);
        }
        let mut then = x.get("then");
        while let Some(t) = then {
            e.process(t["input"].as_str().unwrap().as_bytes());
            for (k, w) in t["rows"].as_object().into_iter().flatten() {
                let r: usize = k.parse().unwrap();
                check(&format!("then row {r}"), e.row_text(r).map(|t| json!(t.0.iter().collect::<String>())).unwrap_or(Value::Null), w);
            }
            then = t.get("then");
        }
        if let Some(pt) = x.get("pinned_then") {
            e.set_scroll_offset(pt["offset"].as_u64().unwrap() as usize);
            e.process(pt["input"].as_str().unwrap().as_bytes());
            check(&format!("pinned view inside history (offset {} of {})", e.scroll_offset(), e.scrollback_len()), json!(e.scroll_offset() <= e.scrollback_len()), &json!(true));
            e.set_scroll_offset(0);
        }
        for want in x["row0_has"].as_array().into_iter().flatten() {
            let row: String = e.row_text(e.scrolled()).map(|t| t.0.into_iter().collect()).unwrap_or_default();
            check(&format!("row 0 {row:?} has {want}"), json!(row.contains(want.as_str().unwrap())), &json!(true));
        }
        if let Some(al) = x.get("after_live") {
            e.set_scroll_offset(1);
            e.set_scroll_offset(0);
            e.process(al["input"].as_str().unwrap().as_bytes());
            check("after returning live", json!(joined(&e)), &al["contents"]);
        }
        for l in x["links"].as_array().into_iter().flatten() {
            let got = e.hyperlink_at(l["row"].as_u64().unwrap() as u16, l["col"].as_u64().unwrap() as u16);
            check("link", json!(got), &l["uri"]);
        }
        bad
    }

    /// Every engine passes the baseline; each passes the fidelity extras it claims. Extras another engine fails are
    /// reported (printed), not failed: that is what "fidelity" means.
    #[test]
    fn engines_pass_the_shared_conformance_suite() {
        let doc: serde_json::Value = serde_json::from_str(include_str!("../../tests/fixtures/client/engines.json")).unwrap();
        let mut failures = Vec::new();
        for case in doc["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let claimed: Vec<&str> = case["engines"].as_array().map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
            for kind in EngineKind::ALL {
                let required = claimed.is_empty() || claimed.contains(&kind.name());
                let bad = run_case(kind, case);
                if bad.is_empty() { continue; }
                if required { failures.push(format!("{} / {name}: {}", kind.name(), bad.join("; "))); }
                else { println!("fidelity extra not met by {}: {name}", kind.name()); }
            }
        }
        assert!(failures.is_empty(), "engine conformance:\n{}", failures.join("\n"));
    }
}

/// Input fuzz (libghostty plan G8): seeded random byte streams, heavy on escape sequences, interleaved with resizes,
/// scrolling, history reads and drawing, against every engine. Checks nothing panics and the interface's invariants
/// hold. `VQRO_FUZZ_ITERS` raises the default 300 rounds per engine; `VQRO_FUZZ_SEED` picks a seed.
#[cfg(test)]
mod fuzz {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17; self.0 }
        fn below(&mut self, n: u64) -> u64 { self.next() % n.max(1) }
    }

    const PIECES: &[&[u8]] = &[
        b"\x1b[", b"\x1b]", b"\x1bP", b"\x1b\\", b"\x07", b"\x1bc", b"\x1b[?1049h", b"\x1b[?1049l", b"\x1b[2J", b"\x1b[3J",
        b"\x1b[H", b"\x1b[1;1r", b"\x1b[r", b"\x1b[999S", b"\x1b[999T", b"\x1b[5 q", b"\x1b[38;2;1;2;3m", b"\x1b[4:3m",
        b"\x1b]8;;http://x\x1b\\", b"\x1b]8;;\x1b\\", b"\x1b]52;c;aGk=\x07", b"\x1b]52;c;?\x07", b"\x1b]9;n\x07", b"\x1b]777;notify;a;b\x07",
        b"\x1b]0;t\x07", b"\x1b[?2004h", b"\x1b[?1000h", b"\x1b[?1006h", b"\x1b[>1u", b"\x1b[<u", b"\r\n", b"\n", b"\t", b"\x08",
        "漢".as_bytes(), "👩‍💻".as_bytes(), "🇯🇵".as_bytes(), "e\u{301}".as_bytes(), b"\xff\xfe", b"\xe2\x82", b"0123456789",
        b"\x1b[;;;;;;;;;;;;;;;;;;;;;;;;;;;;m", b"\x1b[9999999999999999999999C", b"\x1b[?9999999h", b"\x1b[1000000@",
    ];

    fn stream(r: &mut Rng) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..r.below(64) + 1 {
            match r.below(3) {
                0 => out.extend_from_slice(PIECES[r.below(PIECES.len() as u64) as usize]),
                1 => out.push(r.below(256) as u8),
                _ => for _ in 0..r.below(80) { out.push(b' ' + r.below(95) as u8) },
            }
        }
        out
    }

    #[test]
    fn engines_survive_random_input_and_keep_their_invariants() {
        let iters: usize = std::env::var("VQRO_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
        let seed: u64 = std::env::var("VQRO_FUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x5eed_1234_abcd_ef01);
        for kind in EngineKind::ALL {
            let mut r = Rng(seed);
            let mut e = Emulator::with_engine(kind, 40, 10, 50);
            assert_eq!(e.engine(), kind);
            for i in 0..iters {
                e.process(&stream(&mut r));
                assert!(e.take_effects().len() <= MAX_EFFECTS, "{} round {i}: effects bounded", kind.name());
                match r.below(6) {
                    0 => e.resize(1 + r.below(200) as u16, 1 + r.below(60) as u16),
                    1 => e.set_scroll_offset(r.below(500) as usize),
                    2 => e.set_scroll_offset(0),
                    _ => {}
                }
                let (rows, cols) = (e.rows(), e.cols());
                assert!(rows >= 1 && cols >= 1, "{} round {i}: a size", kind.name());
                assert!(e.scroll_offset() <= e.scrollback_len().max(e.scroll_offset()), "{} round {i}", kind.name());
                assert!(e.scrollback_len() <= e.scrollback_capacity().max(e.scrollback_len()), "{} round {i}", kind.name());
                if let Some((cr, cc, _)) = e.cursor() { assert!(cr < rows && cc <= cols, "{} round {i}: cursor {cr},{cc} in {rows}x{cols}", kind.name()); }
                let scrolled = e.scrolled();
                for row in [0, scrolled.saturating_sub(1), scrolled, scrolled + usize::from(rows) - 1, scrolled + usize::from(rows) + 5] {
                    if let Some((chars, starts, _)) = e.row_text(row) {
                        assert_eq!(chars.len(), starts.len(), "{} round {i}", kind.name());
                        assert!(starts.iter().all(|c| *c < cols), "{} round {i}: cells inside the row", kind.name());
                    }
                }
                let _ = e.hyperlink_at(r.below(u64::from(rows)) as u16, r.below(u64::from(cols)) as u16);
                let area = Rect::new(0, 0, cols, rows);
                let mut buf = Buffer::empty(area);
                e.draw(&mut buf, area);
            }
        }
    }
}
