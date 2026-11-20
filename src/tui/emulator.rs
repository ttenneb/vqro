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
    pub const DEFAULT: EngineKind = EngineKind::Vt100;

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
    /// The screen as shown, as plain text (tests).
    #[cfg(test)]
    fn contents(&self) -> String;
}

/// Start an engine, or say why it can't start.
fn start(kind: EngineKind, cols: u16, rows: u16, scrollback: usize) -> Result<Box<dyn Engine>, String> {
    match kind {
        EngineKind::Vt100 => Ok(Box::new(Vt100::new(cols, rows, scrollback))),
        EngineKind::Libghostty => Err("not built into this client yet".into()),
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
        let mut e = Emulator::with_engine(EngineKind::Libghostty, 20, 4, 100);
        // Until slice G2 builds libghostty in, asking for it falls back; the terminal still works.
        assert_eq!((e.wanted(), e.engine()), (EngineKind::Libghostty, EngineKind::Vt100));
        assert!(e.fallback().is_some());
        e.process(b"hello");
        assert!(e.contents().starts_with("hello"));
        // A fresh terminal (reattach) asks for the same engine again, not for whatever it fell back to.
        let f = e.fresh();
        assert_eq!((f.wanted(), f.engine(), f.fallback().is_some()), (EngineKind::Libghostty, EngineKind::Vt100, true));
        let v = Emulator::with_engine(EngineKind::Vt100, 20, 4, 100);
        assert_eq!((v.engine(), v.fallback()), (EngineKind::Vt100, None));
    }
}
