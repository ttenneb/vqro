//! libghostty-vt (Ghostty's terminal emulation library, MIT) as the TUI's `libghostty` terminal engine.
//!
//! The FFI declarations (`bindings`) and the patterns below start from herdr's `src/ghostty/` (Apache-2.0) as an
//! unvetted baseline: tested here, audited before relying on it (libghostty plan G8). Only the narrow API the
//! engine needs is wrapped. Rules for every FFI call:
//! - a handle is used only while its owner is alive, and is freed once (`Drop`);
//! - out-pointers point at storage of the documented type, and sized structs carry their `size`;
//! - callbacks never unwind into C (`catch_unwind`), never call back into the terminal, and bound what they keep;
//! - input reaches the library only through `ghostty_terminal_vt_write`, which takes any bytes.
//!
//! Query replies (DA, DSR, XTVERSION, size reports) are not wired (`WRITE_PTY` stays unset): the backend already
//! answers them. libghostty has no notification callback (it reads OSC 9 as a working directory), so OSC 9 and
//! OSC 777 notifications are found by `Notifications`, a small scanner over the program's bytes.
#![allow(dead_code)]

#[allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals, clippy::all, rustdoc::all, unsafe_op_in_unsafe_fn)]
pub mod bindings;

use bindings as ffi;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use std::os::raw::c_void;
use std::{mem, ptr, slice};

use super::emulator::{CursorShape, Effect, Engine, MouseEncoding, MouseMode, MAX_EFFECTS};

/// A libghostty-vt call that didn't succeed (its `GhosttyResult`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(ffi::GhosttyResult);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "libghostty-vt error {}", self.0) }
}

trait Ok_ {
    fn ok(self) -> Result<(), Error>;
}
impl Ok_ for ffi::GhosttyResult {
    fn ok(self) -> Result<(), Error> { if self == ffi::GhosttyResult_GHOSTTY_SUCCESS { Ok(()) } else { Err(Error(self)) } }
}

/// What callbacks collect while the library parses one write.
#[derive(Default)]
struct Collected {
    bells: u32,
    /// Clipboard writes, as text (base64-encoded when passed on).
    clipboard: Vec<Vec<u8>>,
    clipboard_bytes: usize,
}

const MAX_CLIPBOARD_BYTES: usize = 768 * 1024; // decoded; within the 1 MiB base64 bound the vt100 engine uses

unsafe extern "C" fn bell_cb(_terminal: ffi::GhosttyTerminal, userdata: *mut c_void) {
    let _ = std::panic::catch_unwind(|| {
        if userdata.is_null() { return; }
        // SAFETY: userdata is the boxed `Collected` owned by this engine, alive while the terminal is.
        let c = unsafe { &mut *userdata.cast::<Collected>() };
        c.bells = c.bells.saturating_add(1);
    });
}

unsafe extern "C" fn clipboard_cb(
    _terminal: ffi::GhosttyTerminal,
    userdata: *mut c_void,
    write: *const ffi::GhosttyClipboardWrite,
) -> ffi::GhosttyClipboardWriteResult {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: libghostty-vt keeps `write` and its strings alive for this synchronous call.
        unsafe { take_clipboard(userdata, write) }
    }))
    .unwrap_or(ffi::GhosttyClipboardWriteResult_GHOSTTY_CLIPBOARD_WRITE_RESULT_INVALID_DATA)
}

/// Keep one text/plain clipboard write (standard clipboard), bounded. Adapted from herdr's capture_clipboard_write.
unsafe fn take_clipboard(userdata: *mut c_void, write: *const ffi::GhosttyClipboardWrite) -> ffi::GhosttyClipboardWriteResult {
    use ffi::{GhosttyClipboardWriteResult_GHOSTTY_CLIPBOARD_WRITE_RESULT_INVALID_DATA as INVALID,
              GhosttyClipboardWriteResult_GHOSTTY_CLIPBOARD_WRITE_RESULT_SUCCESS as SUCCESS,
              GhosttyClipboardWriteResult_GHOSTTY_CLIPBOARD_WRITE_RESULT_UNSUPPORTED as UNSUPPORTED};
    if userdata.is_null() || write.is_null() { return INVALID; }
    // The struct is sized: read only fields the library says it filled.
    let needed = mem::offset_of!(ffi::GhosttyClipboardWrite, contents_len) + mem::size_of::<usize>();
    // SAFETY: `size` is the leading field of a live request.
    if unsafe { (*write).size } < needed { return INVALID; }
    // SAFETY: the size check covers the fields read below.
    let request = unsafe { &*write };
    if request.location != ffi::GhosttyClipboardLocation_GHOSTTY_CLIPBOARD_LOCATION_STANDARD { return UNSUPPORTED; }
    if request.contents_len != 1 || request.contents.is_null() { return UNSUPPORTED; }
    // SAFETY: one content, alive for the call.
    let content = unsafe { &*request.contents };
    // SAFETY: borrowed strings, alive for the call.
    let (Some(mime), Some(data)) = (unsafe { borrowed(content.mime) }, unsafe { borrowed(content.data) }) else { return INVALID };
    let text = std::str::from_utf8(mime).ok().and_then(|m| m.split(';').next()).is_some_and(|m| m.trim().eq_ignore_ascii_case("text/plain"));
    if !text || data.is_empty() { return UNSUPPORTED; }
    // SAFETY: userdata is this engine's boxed `Collected`.
    let c = unsafe { &mut *userdata.cast::<Collected>() };
    if data.len() > MAX_CLIPBOARD_BYTES || c.clipboard.len() >= MAX_EFFECTS || c.clipboard_bytes + data.len() > 4 * MAX_CLIPBOARD_BYTES {
        return INVALID;
    }
    c.clipboard_bytes += data.len();
    c.clipboard.push(data.to_vec());
    SUCCESS
}

/// SAFETY: `value` must point at `len` live bytes for the returned lifetime (the callback's duration).
unsafe fn borrowed<'a>(value: ffi::GhosttyString) -> Option<&'a [u8]> {
    if value.len == 0 { Some(&[]) } else if value.ptr.is_null() { None } else { Some(unsafe { slice::from_raw_parts(value.ptr, value.len) }) }
}

/// OSC 9 and OSC 777 notifications in a program's byte stream (libghostty-vt has no callback for them). A small,
/// bounded state machine that survives sequences split across writes.
#[derive(Default)]
struct Notifications {
    /// Bytes of an OSC being read (after `ESC ]`), or None outside one.
    osc: Option<Vec<u8>>,
    esc: bool,
}

const MAX_OSC: usize = 4096;

impl Notifications {
    fn feed(&mut self, bytes: &[u8], out: &mut Vec<Effect>) {
        for &b in bytes {
            match self.osc.as_mut() {
                None => {
                    if self.esc && b == b']' { self.osc = Some(Vec::new()); }
                    self.esc = b == 0x1b;
                }
                Some(body) => {
                    let end = b == 0x07 || (self.esc && b == b'\\');
                    if end {
                        if self.esc { body.pop(); } // the ESC of ESC \
                        let body = self.osc.take().unwrap_or_default();
                        if let Some(e) = notification(&body) { if out.len() < MAX_EFFECTS { out.push(e); } }
                        self.esc = false;
                        continue;
                    }
                    if b == 0x18 || b == 0x1a { self.osc = None; } // CAN / SUB cancel
                    else if body.len() < MAX_OSC { body.push(b); } else { self.osc = None; }
                    self.esc = b == 0x1b;
                }
            }
        }
    }
}

/// OSC 9 is a notification unless it is ConEmu's progress (`9;4;...`) or names a directory (`9;9;...`); OSC 777
/// only for `notify`. Same rule as the vt100 engine.
fn notification(body: &[u8]) -> Option<Effect> {
    let mut parts = body.split(|b| *b == b';');
    match (parts.next(), parts.next()) {
        (Some(b"9"), Some(rest)) if !rest.is_empty() && rest != b"4" && rest != b"9" => Some(Effect::Notify(body.to_vec())),
        (Some(b"777"), Some(b"notify")) => Some(Effect::Notify(body.to_vec())),
        _ => None,
    }
}

/// The cursor shape a program set with DECSCUSR (`CSI n SP q`), found in its bytes: libghostty-vt reports the
/// shape but not whether the program chose it, and the user's own cursor must stay when it didn't.
#[derive(Default)]
struct CursorSet {
    tail: Vec<u8>,
    chosen: Option<u8>,
}

impl CursorSet {
    fn feed(&mut self, bytes: &[u8]) {
        let mut seen = mem::take(&mut self.tail);
        seen.extend_from_slice(bytes);
        let mut i = 0;
        while i + 1 < seen.len() {
            if seen[i] == 0x1b && seen[i + 1] == b'[' {
                let mut j = i + 2;
                while j < seen.len() && seen[j].is_ascii_digit() && j - i < 6 { j += 1; }
                if j + 1 < seen.len() && seen[j] == b' ' && seen[j + 1] == b'q' {
                    let n = std::str::from_utf8(&seen[i + 2..j]).ok().and_then(|s| s.parse::<u8>().ok()).unwrap_or(0);
                    self.chosen = (n != 0).then_some(n);
                    i = j + 2;
                    continue;
                }
            }
            i += 1;
        }
        let keep = seen.len().saturating_sub(7);
        self.tail = seen[keep..].to_vec();
    }
}

/// The libghostty-vt engine for one terminal view.
pub struct Ghostty {
    raw: ffi::GhosttyTerminal,
    collected: Box<Collected>,
    notes: Notifications,
    cursor_set: CursorSet,
    effects: Vec<Effect>,
    scrollback: usize,
    /// Rows the full scrollback dropped. libghostty-vt doesn't report them, so this stays 0 and positions in a full
    /// history shift as it drops rows (libghostty plan G4 decides how to keep selections on their text).
    dropped: usize,
}

impl Ghostty {
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Result<Self, Error> {
        let mut raw = ptr::null_mut();
        let options = ffi::GhosttyTerminalOptions { cols: cols.max(1), rows: rows.max(1), max_scrollback: scrollback };
        // SAFETY: a valid out-pointer and options; a null allocator means the library's default allocator.
        unsafe { ffi::ghostty_terminal_new(ptr::null(), &mut raw, options) }.ok()?;
        if raw.is_null() { return Err(Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE)); }
        let mut engine = Self { raw, collected: Box::default(), notes: Notifications::default(), cursor_set: CursorSet::default(),
                                effects: Vec::new(), scrollback, dropped: 0 };
        let userdata = (&mut *engine.collected as *mut Collected).cast::<c_void>();
        // SAFETY: a live terminal; `userdata` outlives it (owned by `engine`, freed after the terminal in Drop);
        // each callback has the signature its option documents.
        unsafe {
            ffi::ghostty_terminal_set(raw, ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_USERDATA, userdata).ok()?;
            ffi::ghostty_terminal_set(raw, ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_BELL, (bell_cb as *const ()).cast()).ok()?;
            ffi::ghostty_terminal_set(raw, ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE, (clipboard_cb as *const ()).cast()).ok()?;
        }
        Ok(engine)
    }

    fn get<T: Default>(&self, data: ffi::GhosttyTerminalData) -> Option<T> {
        let mut out = T::default();
        // SAFETY: a live terminal; callers name data kinds whose documented output type is `T`.
        unsafe { ffi::ghostty_terminal_get(self.raw, data, (&mut out as *mut T).cast()) }.ok().ok().map(|_| out)
    }

    fn mode(&self, value: u16) -> bool {
        let mut out = false;
        // DEC private mode `value` (ghostty_mode_new(value, false)).
        // SAFETY: a live terminal and a bool out-pointer.
        unsafe { ffi::ghostty_terminal_mode_get(self.raw, value & 0x7fff, &mut out) }.ok().map(|_| out).unwrap_or(false)
    }

    fn scrollbar(&self) -> ffi::GhosttyTerminalScrollbar {
        self.get::<ffi::GhosttyTerminalScrollbar>(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_SCROLLBAR).unwrap_or_default()
    }

    fn scroll(&mut self, tag: ffi::GhosttyTerminalScrollViewportTag, value: ffi::GhosttyTerminalScrollViewportValue) {
        // SAFETY: a live terminal; the value matches the tag.
        unsafe { ffi::ghostty_terminal_scroll_viewport(self.raw, ffi::GhosttyTerminalScrollViewport { tag, value }) };
    }

    fn grid_ref(&self, point: ffi::GhosttyPoint) -> Option<ffi::GhosttyGridRef> {
        let mut r = ffi::GhosttyGridRef { size: mem::size_of::<ffi::GhosttyGridRef>(), ..Default::default() };
        // SAFETY: a live terminal and a sized out struct. The ref is used at once, before the next write.
        unsafe { ffi::ghostty_terminal_grid_ref(self.raw, point, &mut r) }.ok().ok().map(|_| r)
    }

    /// A cell: its text (a whole grapheme cluster), whether it is the tail of a wide cell, and its style.
    fn cell(&self, point: ffi::GhosttyPoint, with_style: bool) -> Option<(String, ffi::GhosttyCellWide, Option<ffi::GhosttyStyle>)> {
        let r = self.grid_ref(point)?;
        let mut raw = ffi::GhosttyCell::default();
        // SAFETY: `r` came from this terminal just now.
        unsafe { ffi::ghostty_grid_ref_cell(&r, &mut raw) }.ok().ok()?;
        let mut wide = ffi::GhosttyCellWide_GHOSTTY_CELL_WIDE_NARROW;
        // SAFETY: a cell value and an out-pointer of its documented type.
        unsafe { ffi::ghostty_cell_get(raw, ffi::GhosttyCellData_GHOSTTY_CELL_DATA_WIDE, (&mut wide as *mut ffi::GhosttyCellWide).cast()) }.ok().ok()?;
        let mut cps = [0u32; 16];
        let mut n = 0usize;
        // SAFETY: a buffer of `cps.len()` u32; on OUT_OF_SPACE nothing is written past it.
        let res = unsafe { ffi::ghostty_grid_ref_graphemes(&r, cps.as_mut_ptr(), cps.len(), &mut n) };
        let text: String = if res == ffi::GhosttyResult_GHOSTTY_SUCCESS { cps[..n.min(cps.len())].iter().filter_map(|c| char::from_u32(*c)).collect() } else { String::new() };
        let style = with_style.then(|| {
            let mut s = ffi::GhosttyStyle { size: mem::size_of::<ffi::GhosttyStyle>(), ..Default::default() };
            // SAFETY: `r` is fresh and `s` is a sized style struct.
            unsafe { ffi::ghostty_grid_ref_style(&r, &mut s) }.ok().ok().map(|_| s)
        }).flatten();
        Some((text, wide, style))
    }

    /// The hyperlink (OSC 8) of a cell on screen as shown, if any.
    fn link(&self, row: u16, col: u16) -> Option<String> {
        let r = self.grid_ref(point(ffi::GhosttyPointTag_GHOSTTY_POINT_TAG_VIEWPORT, col, row as u32))?;
        let mut buf = vec![0u8; 2048];
        let mut n = 0usize;
        // SAFETY: a fresh ref and a buffer of `buf.len()` bytes.
        unsafe { ffi::ghostty_grid_ref_hyperlink_uri(&r, buf.as_mut_ptr(), buf.len(), &mut n) }.ok().ok()?;
        (n > 0).then(|| String::from_utf8_lossy(&buf[..n.min(buf.len())]).into_owned())
    }

    fn wrapped(&self, point: ffi::GhosttyPoint) -> bool {
        let Some(r) = self.grid_ref(point) else { return false };
        let mut row = 0;
        // SAFETY: a fresh ref; row data out-pointers of their documented types.
        unsafe {
            if ffi::ghostty_grid_ref_row(&r, &mut row).ok().is_err() { return false; }
            let mut wrap = false;
            ffi::ghostty_row_get(row, ffi::GhosttyRowData_GHOSTTY_ROW_DATA_WRAP, (&mut wrap as *mut bool).cast()).ok().map(|_| wrap).unwrap_or(false)
        }
    }

    /// Rows the screen holds above the live area (history kept).
    fn kept(&self) -> usize { self.get::<usize>(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS).unwrap_or(0) }
}

fn point(tag: ffi::GhosttyPointTag, x: u16, y: u32) -> ffi::GhosttyPoint {
    ffi::GhosttyPoint { tag, value: ffi::GhosttyPointValue { coordinate: ffi::GhosttyPointCoordinate { x, y } } }
}

impl Drop for Ghostty {
    fn drop(&mut self) {
        // SAFETY: `raw` came from ghostty_terminal_new and is freed once; `collected` is dropped after this.
        unsafe { ffi::ghostty_terminal_free(self.raw) };
    }
}

fn color(c: ffi::GhosttyStyleColor) -> Color {
    match c.tag {
        // SAFETY: the tagged union holds `palette` for this tag.
        ffi::GhosttyStyleColorTag_GHOSTTY_STYLE_COLOR_PALETTE => Color::Indexed(unsafe { c.value.palette }),
        ffi::GhosttyStyleColorTag_GHOSTTY_STYLE_COLOR_RGB => {
            // SAFETY: the tagged union holds `rgb` for this tag.
            let rgb = unsafe { c.value.rgb };
            Color::Rgb(rgb.r, rgb.g, rgb.b)
        }
        _ => Color::Reset,
    }
}

fn style(s: &ffi::GhosttyStyle) -> Style {
    let mut st = Style::new().fg(color(s.fg_color)).bg(color(s.bg_color));
    for (on, m) in [(s.bold, Modifier::BOLD), (s.faint, Modifier::DIM), (s.italic, Modifier::ITALIC), (s.underline != 0, Modifier::UNDERLINED),
                    (s.inverse, Modifier::REVERSED), (s.strikethrough, Modifier::CROSSED_OUT), (s.blink, Modifier::SLOW_BLINK), (s.invisible, Modifier::HIDDEN)] {
        if on { st = st.add_modifier(m); }
    }
    if s.underline != 0 && s.underline_color.tag != ffi::GhosttyStyleColorTag_GHOSTTY_STYLE_COLOR_NONE {
        st = st.underline_color(color(s.underline_color));
    }
    st
}

impl Engine for Ghostty {
    fn process(&mut self, bytes: &[u8]) {
        self.notes.feed(bytes, &mut self.effects);
        self.cursor_set.feed(bytes);
        // SAFETY: a live terminal and a byte slice; the library accepts any bytes. Callbacks run during this call
        // and only touch `collected`.
        unsafe { ffi::ghostty_terminal_vt_write(self.raw, bytes.as_ptr(), bytes.len()) };
        let c = &mut *self.collected;
        if mem::take(&mut c.bells) > 0 && !self.effects.contains(&Effect::Bell) && self.effects.len() < MAX_EFFECTS { self.effects.push(Effect::Bell); }
        for data in c.clipboard.drain(..) {
            use base64::Engine as _;
            if self.effects.len() < MAX_EFFECTS {
                self.effects.push(Effect::Clipboard(b"c".to_vec(), base64::engine::general_purpose::STANDARD.encode(data).into_bytes()));
            }
        }
        c.clipboard_bytes = 0;
    }

    fn take_effects(&mut self) -> Vec<Effect> { mem::take(&mut self.effects) }

    fn resize(&mut self, cols: u16, rows: u16) {
        // SAFETY: a live terminal; cell pixel sizes are nominal (no pixel queries are answered).
        let _ = unsafe { ffi::ghostty_terminal_resize(self.raw, cols.max(1), rows.max(1), 1, 1) };
    }
    fn rows(&self) -> u16 { self.get::<u16>(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_ROWS).unwrap_or(1) }
    fn cols(&self) -> u16 { self.get::<u16>(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_COLS).unwrap_or(1) }
    fn application_cursor(&self) -> bool { self.mode(1) }
    fn bracketed_paste(&self) -> bool { self.mode(2004) }
    fn alternate_screen(&self) -> bool {
        self.get::<ffi::GhosttyTerminalScreen>(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN)
            .is_some_and(|s| s == ffi::GhosttyTerminalScreen_GHOSTTY_TERMINAL_SCREEN_ALTERNATE)
    }
    fn mouse(&self) -> (MouseMode, MouseEncoding) {
        let mode = if self.mode(1003) { MouseMode::AnyMotion } else if self.mode(1002) { MouseMode::ButtonMotion }
            else if self.mode(1000) { MouseMode::PressRelease } else if self.mode(9) { MouseMode::Press } else { MouseMode::None };
        let enc = if self.mode(1006) { MouseEncoding::Sgr } else if self.mode(1005) { MouseEncoding::Utf8 } else { MouseEncoding::Default };
        (mode, enc)
    }
    fn cursor(&self) -> Option<(u16, u16, CursorShape)> {
        if self.scroll_offset() > 0 || !self.get::<bool>(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_CURSOR_VISIBLE).unwrap_or(true) { return None; }
        let x = self.get::<u16>(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_CURSOR_X)?;
        let y = self.get::<u16>(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_CURSOR_Y)?;
        let shape = match self.cursor_set.chosen {
            None => CursorShape::Default,
            Some(1) => CursorShape::BlinkBlock,
            Some(2) => CursorShape::SteadyBlock,
            Some(3) => CursorShape::BlinkUnderline,
            Some(4) => CursorShape::SteadyUnderline,
            Some(5) => CursorShape::BlinkBar,
            Some(6) => CursorShape::SteadyBar,
            Some(_) => CursorShape::Default,
        };
        Some((y, x, shape))
    }
    fn scroll_offset(&self) -> usize {
        let s = self.scrollbar();
        (s.total.saturating_sub(s.offset).saturating_sub(s.len)) as usize
    }
    fn set_scroll_offset(&mut self, n: usize) {
        // 0 follows the live screen as output arrives; a row pins the view to that content.
        if n == 0 {
            return self.scroll(ffi::GhosttyTerminalScrollViewportTag_GHOSTTY_SCROLL_VIEWPORT_BOTTOM, ffi::GhosttyTerminalScrollViewportValue::default());
        }
        let s = self.scrollbar();
        let top = (s.total.saturating_sub(s.len) as usize).saturating_sub(n);
        self.scroll(ffi::GhosttyTerminalScrollViewportTag_GHOSTTY_SCROLL_VIEWPORT_ROW, ffi::GhosttyTerminalScrollViewportValue { row: top });
    }
    fn scrollback_len(&self) -> usize { self.kept() }
    fn scrollback_capacity(&self) -> usize { self.scrollback }
    fn scrolled(&self) -> usize { self.dropped + self.kept() }
    fn row_text(&mut self, row: usize) -> Option<(Vec<char>, Vec<u16>, bool)> {
        // Screen coordinates: 0 is the oldest kept row; `row` counts dropped rows too.
        let y = row.checked_sub(self.dropped)?;
        if y >= self.kept() + usize::from(self.rows()) { return None; }
        let (mut chars, mut starts) = (Vec::new(), Vec::new());
        for col in 0..self.cols() {
            let Some((text, wide, _)) = self.cell(point(ffi::GhosttyPointTag_GHOSTTY_POINT_TAG_SCREEN, col, y as u32), false) else { break };
            if wide == ffi::GhosttyCellWide_GHOSTTY_CELL_WIDE_SPACER_TAIL || wide == ffi::GhosttyCellWide_GHOSTTY_CELL_WIDE_SPACER_HEAD { continue; }
            for c in if text.is_empty() { " ".to_string() } else { text }.chars() {
                chars.push(c);
                starts.push(col);
            }
        }
        let wrapped = self.wrapped(point(ffi::GhosttyPointTag_GHOSTTY_POINT_TAG_SCREEN, 0, y as u32));
        while !wrapped && chars.last() == Some(&' ') {
            chars.pop();
            starts.pop();
        }
        Some((chars, starts, wrapped))
    }
    fn hyperlink_at(&self, row: u16, col: u16) -> Option<String> { self.link(row, col) }
    fn draw(&self, buf: &mut Buffer, area: Rect) {
        let (rows, cols) = (self.rows().min(area.height), self.cols().min(area.width));
        for row in 0..rows {
            for col in 0..cols {
                let Some((text, wide, st)) = self.cell(point(ffi::GhosttyPointTag_GHOSTTY_POINT_TAG_VIEWPORT, col, row as u32), true) else { continue };
                if wide == ffi::GhosttyCellWide_GHOSTTY_CELL_WIDE_SPACER_TAIL { continue; }
                if let Some(out) = buf.cell_mut((area.x + col, area.y + row)) {
                    out.set_symbol(if text.is_empty() || wide == ffi::GhosttyCellWide_GHOSTTY_CELL_WIDE_SPACER_HEAD { " " } else { &text });
                    out.set_style(st.as_ref().map(style).unwrap_or_default());
                }
            }
        }
    }
    #[cfg(test)]
    fn contents(&self) -> String {
        let mut out = String::new();
        for row in 0..self.rows() {
            let mut line = String::new();
            for col in 0..self.cols() {
                let Some((text, wide, _)) = self.cell(point(ffi::GhosttyPointTag_GHOSTTY_POINT_TAG_VIEWPORT, col, row as u32), false) else { break };
                if wide == ffi::GhosttyCellWide_GHOSTTY_CELL_WIDE_SPACER_TAIL { continue; }
                line.push_str(if text.is_empty() { " " } else { &text });
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
        out.trim_end_matches('\n').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vendored library builds, links and runs: a terminal is created, fed bytes and freed.
    #[test]
    fn the_vendored_library_links_and_runs() {
        let mut g = Ghostty::new(20, 4, 100).unwrap();
        g.process(b"hello \xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x92\xbb|");
        assert_eq!(g.contents(), "hello \u{1F469}\u{200D}\u{1F4BB}|", "a ZWJ sequence is one cluster in one wide cell");
    }

    #[test]
    fn notifications_are_found_across_writes_and_bounded() {
        let mut n = Notifications::default();
        let mut out = Vec::new();
        n.feed(b"x\x1b]9;do", &mut out);
        n.feed(b"ne\x07\x1b]9;4;1;50\x07\x1b]777;notify;T;B\x1b\\\x1b]2;title\x07", &mut out);
        assert_eq!(out, vec![Effect::Notify(b"9;done".to_vec()), Effect::Notify(b"777;notify;T;B".to_vec())]);
        let mut big = b"\x1b]9;".to_vec();
        big.extend(std::iter::repeat_n(b'a', MAX_OSC * 2));
        big.push(0x07);
        n.feed(&big, &mut out);
        assert_eq!(out.len(), 2, "an over-long OSC is dropped");
    }

    #[test]
    fn a_cursor_shape_counts_only_when_the_program_set_it() {
        let mut c = CursorSet::default();
        c.feed(b"text");
        assert_eq!(c.chosen, None);
        c.feed(b"\x1b[5");
        c.feed(b" q");
        assert_eq!(c.chosen, Some(5), "split across writes");
        c.feed(b"\x1b[0 q");
        assert_eq!(c.chosen, None, "0 restores the user's own cursor");
    }
}
