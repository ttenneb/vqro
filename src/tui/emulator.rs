//! The terminal emulator behind terminal views, as one small interface (libghostty plan, slice G1). Everything the
//! client needs from an emulator goes through `Emulator`: feeding program output, what the program asked of the
//! terminal beyond drawing (`Effect`), modes, the cursor, the viewport and its history, and drawing into a buffer.
//! Today it wraps the `vt100` crate; the rest of the client never names `vt100`.
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

/// One terminal view's emulator.
pub struct Emulator {
    parser: vt100::Parser<Effects>,
}

impl Emulator {
    /// A blank terminal of `cols` x `rows` keeping up to `scrollback` lines of history.
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Self {
        Self { parser: vt100::Parser::new_with_callbacks(nz(rows), nz(cols), scrollback, Effects::default()) }
    }

    /// A blank terminal of the same size and history capacity (a fresh attach never reuses old state).
    pub fn fresh(&self) -> Self {
        Self::new(self.cols(), self.rows(), self.scrollback_capacity())
    }

    /// Program output. Its effects queue until `take_effects`.
    pub fn process(&mut self, bytes: &[u8]) { self.parser.process(bytes); }

    /// The effects queued since the last call (at most `MAX_EFFECTS`).
    pub fn take_effects(&mut self) -> Vec<Effect> { std::mem::take(&mut self.parser.callbacks_mut().out) }

    pub fn resize(&mut self, cols: u16, rows: u16) { self.parser.screen_mut().set_size(nz(rows), nz(cols)); }
    pub fn rows(&self) -> u16 { self.parser.screen().size().0.get() }
    pub fn cols(&self) -> u16 { self.parser.screen().size().1.get() }

    // ---- modes the program set ----
    pub fn application_cursor(&self) -> bool { self.parser.screen().application_cursor() }
    pub fn bracketed_paste(&self) -> bool { self.parser.screen().bracketed_paste() }
    pub fn alternate_screen(&self) -> bool { self.parser.screen().alternate_screen() }
    pub fn mouse(&self) -> (MouseMode, MouseEncoding) {
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

    /// Where the cursor is on screen and its shape, when the program shows it and the view is at the live screen.
    pub fn cursor(&self) -> Option<(u16, u16, CursorShape)> {
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

    // ---- the viewport and its history ----
    /// How many lines the view is scrolled back from the live screen (0: live).
    pub fn scroll_offset(&self) -> usize { self.parser.screen().scrollback() }
    /// Scroll back `n` lines (clamped to the history kept).
    pub fn set_scroll_offset(&mut self, n: usize) { self.parser.screen_mut().set_scrollback(n); }
    pub fn scrollback_len(&self) -> usize { self.parser.screen().scrollback_len() }
    pub fn scrollback_capacity(&self) -> usize { self.parser.screen().scrollback_capacity() }
    /// Rows above the live screen so far (dropped plus kept): the numbering of `scrollback::Pos` rows.
    pub fn scrolled(&self) -> usize { self.parser.callbacks().dropped + self.scrollback_len() }

    /// One row's text (`row` counts like `scrollback::Pos`) with trailing blanks removed, the cell where each of its
    /// characters starts, and whether the row wraps onto the next. None when the row is no longer kept. Reading
    /// moves the view and puts it back.
    pub fn row_text(&mut self, row: usize) -> Option<(Vec<char>, Vec<u16>, bool)> {
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

    /// The screen as shown (scrolled back or live) as plain text (tests and diagnostics).
    #[cfg(test)]
    pub fn contents(&self) -> String { self.parser.screen().contents() }

    /// Draw the screen as shown into `area` of `buf` (cells past the screen's size are left alone).
    pub fn draw(&self, buf: &mut Buffer, area: Rect) {
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
