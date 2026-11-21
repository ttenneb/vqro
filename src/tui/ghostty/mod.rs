//! libghostty-vt (Ghostty's terminal emulation library, MIT) as the TUI's `libghostty` terminal engine.
//!
//! The patterns below started from herdr's `src/ghostty/` (Apache-2.0); the FFI declarations (`bindings`) are
//! written by hand against the vendored headers and checked by the C compiler (`bindings_check.c`). Only the
//! narrow API the engine needs is wrapped. Rules for every FFI call:
//! - a handle is used only while its owner is alive, and is freed once (`Drop`);
//! - out-pointers point at storage of the documented type, and the one sized struct (the clipboard reply) carries
//!   its `size`;
//! - callbacks never unwind into C (`catch_unwind`), never call back into the terminal, and bound what they keep;
//! - input reaches the library only through `ghostty_terminal_vt_write`, which takes any bytes.
//!
//! Query replies (DA, DSR, XTVERSION, size reports) are not wired (`WRITE_PTY` stays unset): the backend already
//! answers them. OSC 9 and OSC 777 notifications are found by `Notifications`, a small scanner over the program's
//! bytes, the same rule for every engine (the library's own notification callback stays unset).
#![allow(dead_code)]

#[allow(dead_code, non_camel_case_types, non_snake_case)]
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
    fn ok(self) -> Result<(), Error> { if self == ffi::GHOSTTY_SUCCESS { Ok(()) } else { Err(Error(self)) } }
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

// The callbacks have exactly the types the library's options take (ghostty-audit P3-1): checked here by the Rust
// compiler, and the C typedefs by bindings_check.c.
const _: ffi::GhosttyTerminalBellFn = Some(bell_cb);
const _: ffi::GhosttyTerminalClipboardWriteFn = Some(clipboard_cb);

unsafe extern "C" fn bell_cb(_terminal: ffi::GhosttyTerminal, userdata: *mut c_void) {
    let _ = std::panic::catch_unwind(|| {
        if userdata.is_null() { return; }
        // SAFETY: userdata is the boxed `Collected` owned by this engine, alive while the terminal is.
        let c = unsafe { &mut *userdata.cast::<Collected>() };
        c.bells = c.bells.saturating_add(1);
    });
}

unsafe extern "C" fn clipboard_cb(_terminal: ffi::GhosttyTerminal, userdata: *mut c_void, write: *const ffi::GhosttyClipboardWrite) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if write.is_null() { return; }
        // SAFETY: libghostty-vt keeps `write` and everything it points to alive for this synchronous call.
        let result = unsafe { take_clipboard(userdata, &*write) };
        let reply = ffi::GhosttyClipboardWriteReply { size: mem::size_of::<ffi::GhosttyClipboardWriteReply>(), result, remember: false };
        // SAFETY: the request's own reply function, called once during the callback with a sized reply.
        if let Some(answer) = unsafe { (*write).reply } { unsafe { answer(write, &reply) }; }
    }));
}

/// Keep one text clipboard write to the standard clipboard, bounded: its `text/plain` representation, or an empty
/// write when the program clears it (as vt100 passes on). The library decodes OSC 52 (and OSC 1337 Copy, Kitty's
/// OSC 5522); the engine passes text on as OSC 52 again, base64-encoded.
unsafe fn take_clipboard(userdata: *mut c_void, request: &ffi::GhosttyClipboardWrite) -> ffi::GhosttyClipboardWriteResult {
    use ffi::{GHOSTTY_CLIPBOARD_WRITE_RESULT_INVALID_DATA as INVALID,
              GHOSTTY_CLIPBOARD_WRITE_RESULT_SUCCESS as SUCCESS,
              GHOSTTY_CLIPBOARD_WRITE_RESULT_UNSUPPORTED as UNSUPPORTED};
    if userdata.is_null() { return INVALID; }
    if request.location != ffi::GHOSTTY_CLIPBOARD_LOCATION_STANDARD { return UNSUPPORTED; }
    let data: &[u8] = if request.contents_len == 0 { &[] } else {
        if request.contents.is_null() || request.contents_len > 64 { return INVALID; }
        // SAFETY: `contents_len` entries, alive for the call.
        let contents = unsafe { slice::from_raw_parts(request.contents, request.contents_len) };
        let text = contents.iter().find_map(|content| {
            // SAFETY: borrowed strings, alive for the call.
            let mime = unsafe { borrowed(content.mime) }?;
            let plain = std::str::from_utf8(mime).ok().and_then(|m| m.split(';').next()).is_some_and(|m| m.trim().eq_ignore_ascii_case("text/plain"));
            // SAFETY: as above.
            if plain { unsafe { borrowed(content.data) } } else { None }
        });
        match text { Some(t) => t, None => return UNSUPPORTED }
    };
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

/// OSC 9 and OSC 777 notifications in a program's byte stream: the same rule as the vt100 engine, kept by vqro
/// (the library's DESKTOP_NOTIFICATION callback stays unset). A small, bounded state machine that survives
/// sequences split across writes.
#[derive(Default)]
struct Notifications {
    /// Bytes of an OSC being read (after `ESC ]`), or None outside one.
    osc: Option<Vec<u8>>,
    esc: bool,
}

const MAX_OSC: usize = 4096;
/// The longest grapheme cluster read whole (codepoints); longer ones show their base codepoint.
const MAX_GRAPHEME: usize = 1024;

impl Notifications {
    fn feed(&mut self, bytes: &[u8], out: &mut Vec<Effect>) {
        for &b in bytes {
            match self.osc.as_mut() {
                None => {
                    if self.esc && b == b']' { self.osc = Some(Vec::new()); }
                    self.esc = b == 0x1b;
                }
                Some(body) => {
                    // BEL or ESC \ ends it; ESC then BEL cancels it, as the terminal's parser does.
                    let end = (b == 0x07 && !self.esc) || (self.esc && b == b'\\');
                    if end {
                        let body = self.osc.take().unwrap_or_default();
                        if let Some(e) = notification(&body) { if out.len() < MAX_EFFECTS { out.push(e); } }
                        self.esc = false;
                        continue;
                    }
                    if self.esc {
                        // ESC followed by anything but `\` ends the string unterminated, as the terminal's parser
                        // does (ghostty-audit P1-1): no notification, and this byte may start a new OSC.
                        self.osc = (b == b']').then(Vec::new);
                        self.esc = b == 0x1b;
                        continue;
                    }
                    if b == 0x18 || b == 0x1a { self.osc = None; } // CAN / SUB cancel
                    else if b == 0x1b {} // perhaps the start of ESC \
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
    // The body goes to the outer terminal as it is: well-formed UTF-8 without control characters (C0, DEL, C1).
    if !super::emulator::outer_safe(body) { return None; }
    let mut parts = body.split(|b| *b == b';');
    match (parts.next(), parts.next()) {
        (Some(b"9"), Some(rest)) if !rest.is_empty() && rest != b"4" && rest != b"9" => Some(Effect::Notify(body.to_vec())),
        (Some(b"777"), Some(b"notify")) => Some(Effect::Notify(body.to_vec())),
        _ => None,
    }
}

/// The cursor shape a program set with DECSCUSR (`CSI n SP q`), found in its bytes: libghostty-vt reports the
/// shape but not whether the program chose it, and the user's own cursor must stay when it didn't. A full reset
/// (RIS, `ESC c`) gives the user's cursor back, as the vt100 engine does (neither engine resets it on DECSTR).
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
            if seen[i] == 0x1b && seen[i + 1] == b'c' {
                self.chosen = None;
                i += 2;
                continue;
            }
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
    /// History lines the user asked for. The library's line limit is page-granular (it keeps somewhat more), so
    /// only these many are exposed (`exposed`).
    scrollback: usize,
    /// Rows no longer numbered as history: dropped by the library, or kept beyond `scrollback` and hidden.
    /// Counted from a tracked row at the bottom of the history: when rows above it go, its screen row goes down.
    dropped: usize,
    tracked: ffi::GhosttyTrackedGridRef,
    tracked_y: u32,
}

/// Bytes of history the library needs for `lines` rows of `cols` cells: Ghostty keeps 8 bytes per cell and per
/// row, plus room for styles and grapheme data (twice that). Bounded by the line setting's own bound.
fn history_bytes(lines: usize, cols: u16) -> usize {
    lines.saturating_mul((usize::from(cols.max(1)) * 8 + 8) * 2)
}

/// DEC private mode 2027 (grapheme clustering): flags and ZWJ sequences in one cell, also after a reset.
const GRAPHEME_CLUSTER_MODE: ffi::GhosttyMode = 2027;

impl Ghostty {
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Result<Self, Error> {
        let mut raw = ptr::null_mut();
        // SAFETY: a valid out-pointer; a null allocator means the library's default allocator.
        unsafe { ffi::ghostty_terminal_new(ptr::null(), &mut raw, cols.max(1), rows.max(1)) }.ok()?;
        if raw.is_null() { return Err(Error(ffi::GHOSTTY_INVALID_VALUE)); }
        let mut engine = Self { raw, collected: Box::default(), notes: Notifications::default(), cursor_set: CursorSet::default(),
                                effects: Vec::new(), scrollback, dropped: 0, tracked: ptr::null_mut(), tracked_y: 0 };
        let userdata = (&mut *engine.collected as *mut Collected).cast::<c_void>();
        // SAFETY: a live terminal; `userdata` outlives it (owned by `engine`, freed after the terminal in Drop);
        // each callback has the signature its option documents.
        unsafe {
            ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_USERDATA, userdata).ok()?;
            ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_BELL, (bell_cb as *const ()).cast()).ok()?;
            ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE, (clipboard_cb as *const ()).cast()).ok()?;
            // vqro draws no images, so it keeps none (ghostty-audit P3-1): the library is built without Kitty graphics
            // (build.rs), which also says so here (GHOSTTY_UNSUPPORTED). A build with them gets a zero limit, which
            // turns the protocol off. Input: a uint64_t*.
            let zero: u64 = 0;
            let _ = ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT, (&zero as *const u64).cast());
            // History by bytes, sized for the width and set again on every resize (`set_budget`). The library's line
            // limit (SCROLLBACK_MAX_LINES) is not used: at this pin it keeps fewer lines than asked, and a resize
            // with it set kept 59 of 500 lines (measured, 2026-11-21).
            // Clipboard transactions (Kitty's OSC 5522) buffer at most what one write may pass on.
            let clip: usize = MAX_CLIPBOARD_BYTES;
            ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE_MAX_BYTES, (&clip as *const usize).cast()).ok()?;
            let grapheme = ffi::GhosttyTerminalModeConfig { mode: GRAPHEME_CLUSTER_MODE, value: true };
            ffi::ghostty_terminal_set(raw, ffi::GHOSTTY_TERMINAL_OPT_MODE_DEFAULT, (&grapheme as *const ffi::GhosttyTerminalModeConfig).cast()).ok()?;
        }
        engine.set_budget(cols.max(1))?;
        engine.count_dropped(false);
        Ok(engine)
    }

    fn get<T: Default>(&self, data: ffi::GhosttyTerminalData) -> Option<T> {
        let mut out = T::default();
        // SAFETY: a live terminal; callers name data kinds whose documented output type is `T`.
        unsafe { ffi::ghostty_terminal_get(self.raw, data, (&mut out as *mut T).cast()) }.ok().ok().map(|_| out)
    }

    fn mode(&self, value: u16) -> bool {
        // DEC private mode `value` (ghostty_mode_new(value, false): bit 15 clear).
        let mut config = ffi::GhosttyTerminalModeConfig { mode: value & 0x7fff, value: false };
        // SAFETY: a live terminal; DATA_MODE reads and fills a GhosttyTerminalModeConfig.
        unsafe { ffi::ghostty_terminal_get(self.raw, ffi::GHOSTTY_TERMINAL_DATA_MODE, (&mut config as *mut ffi::GhosttyTerminalModeConfig).cast()) }
            .ok().map(|_| config.value).unwrap_or(false)
    }

    fn scrollbar(&self) -> ffi::GhosttyTerminalScrollbar {
        self.get::<ffi::GhosttyTerminalScrollbar>(ffi::GHOSTTY_TERMINAL_DATA_SCROLLBAR).unwrap_or_default()
    }

    fn scroll(&mut self, tag: ffi::GhosttyTerminalScrollViewportTag, value: ffi::GhosttyTerminalScrollViewportValue) {
        // SAFETY: a live terminal; the value matches the tag.
        unsafe { ffi::ghostty_terminal_scroll_viewport(self.raw, ffi::GhosttyTerminalScrollViewport { tag, value }) };
    }

    fn grid_ref(&self, point: ffi::GhosttyPoint) -> Option<ffi::GhosttyGridRef> {
        let mut r = ffi::GhosttyGridRef::default();
        // SAFETY: a live terminal and a sized out struct. The ref is used at once, before the next write.
        unsafe { ffi::ghostty_terminal_grid_ref(self.raw, point, &mut r) }.ok().ok().map(|_| r)
    }

    /// A cell: its text (a whole grapheme cluster), whether it is the tail of a wide cell, and its style.
    fn cell(&self, point: ffi::GhosttyPoint, with_style: bool) -> Option<(String, ffi::GhosttyCellWide, Option<ffi::GhosttyStyle>)> {
        let r = self.grid_ref(point)?;
        let mut raw: ffi::GhosttyCell = 0;
        // SAFETY: `r` came from this terminal just now.
        unsafe { ffi::ghostty_grid_ref_cell(&r, &mut raw) }.ok().ok()?;
        let mut wide = ffi::GHOSTTY_CELL_WIDE_NARROW;
        // SAFETY: a cell value and an out-pointer of its documented type.
        unsafe { ffi::ghostty_cell_get(raw, ffi::GHOSTTY_CELL_DATA_WIDE, (&mut wide as *mut ffi::GhosttyCellWide).cast()) }.ok().ok()?;
        // The cell's whole grapheme cluster: a small buffer first; when the library says it needs more, a buffer of
        // that size (bounded); if that still fails, the cell's base codepoint. A cell is never blanked.
        let mut cps = vec![0u32; 16];
        let mut n = 0usize;
        // SAFETY: a buffer of `cps.len()` u32; on OUT_OF_SPACE nothing is written past it and `n` is the need.
        let mut res = unsafe { ffi::ghostty_grid_ref_graphemes(&r, cps.as_mut_ptr(), cps.len(), &mut n) };
        if res == ffi::GHOSTTY_OUT_OF_SPACE && n > cps.len() && n <= MAX_GRAPHEME {
            cps = vec![0u32; n];
            // SAFETY: as above, with a buffer of the size the library asked for.
            res = unsafe { ffi::ghostty_grid_ref_graphemes(&r, cps.as_mut_ptr(), cps.len(), &mut n) };
        }
        let text: String = if res == ffi::GHOSTTY_SUCCESS {
            cps[..n.min(cps.len())].iter().filter_map(|c| char::from_u32(*c)).collect()
        } else {
            let mut cp = 0u32;
            // SAFETY: a cell value and a u32 out-pointer, the documented type of CELL_DATA_CODEPOINT.
            let _ = unsafe { ffi::ghostty_cell_get(raw, ffi::GHOSTTY_CELL_DATA_CODEPOINT, (&mut cp as *mut u32).cast()) };
            char::from_u32(cp).filter(|c| *c != '\0').map(String::from).unwrap_or_default()
        };
        let style = with_style.then(|| {
            let mut s = ffi::GhosttyStyle::default();
            // SAFETY: `r` is fresh and `s` is a style struct.
            unsafe { ffi::ghostty_grid_ref_style(&r, &mut s) }.ok().ok().map(|_| s)
        }).flatten();
        Some((text, wide, style))
    }

    /// The hyperlink (OSC 8) of a cell on screen as shown, if any.
    fn link(&self, row: u16, col: u16) -> Option<String> {
        let r = self.grid_ref(point(ffi::GHOSTTY_POINT_TAG_VIEWPORT, col, row as u32))?;
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
            ffi::ghostty_row_get(row, ffi::GHOSTTY_ROW_DATA_WRAP, (&mut wrap as *mut bool).cast()).ok().map(|_| wrap).unwrap_or(false)
        }
    }

    /// Rows the screen holds above the live area (history kept by the library).
    fn kept(&self) -> usize { self.get::<usize>(ffi::GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS).unwrap_or(0) }

    /// The history budget for `cols`: enough bytes for the lines asked for at that width (ghostty-audit P1-2: a
    /// budget fixed at creation kept 514 of 2000 lines in a widened pane). Only `scrollback` lines are exposed.
    fn set_budget(&mut self, cols: u16) -> Result<(), Error> {
        let bytes = history_bytes(self.scrollback, cols);
        // SAFETY: a live terminal; SCROLLBACK_MAX_BYTES takes a size_t*.
        unsafe { ffi::ghostty_terminal_set(self.raw, ffi::GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES, (&bytes as *const usize).cast()) }.ok()
    }

    /// A view pinned on history that output or a resize (reflow) has carried past the exposed lines moves to the
    /// oldest exposed line: the rows it showed are no longer history the user can reach.
    fn clamp_view(&mut self) {
        if self.scroll_offset() > self.exposed() { self.set_scroll_offset(self.exposed()); }
    }

    /// History rows exposed: what the library keeps, up to the lines asked for.
    fn exposed(&self) -> usize { self.kept().min(self.scrollback) }

    /// Row numbers in use: the history's and the screen's.
    fn issued(&self) -> usize { self.scrolled() + usize::from(self.rows()) }

    /// Number the rows from `from`: every row gets a number at or above it.
    fn renumber(&mut self, from: usize) {
        self.untrack();
        self.dropped = from.max(self.dropped);
    }

    fn untrack(&mut self) {
        if !self.tracked.is_null() {
            // SAFETY: a handle from ghostty_terminal_grid_ref_track, freed once.
            unsafe { ffi::ghostty_tracked_grid_ref_free(self.tracked) };
            self.tracked = ptr::null_mut();
        }
    }

    /// After a write: count the rows that left above the tracked row, then track the bottom row again. Only on the
    /// primary screen (the alternate one has no history, and a tracked row belongs to the screen it was made on).
    fn count_dropped(&mut self, was_alternate: bool) {
        let alternate = self.alternate_screen();
        if !self.tracked.is_null() && !was_alternate {
            let mut at = ffi::GhosttyPointCoordinate::default();
            // SAFETY: a live tracked handle and an out struct of the documented type.
            let ok = unsafe { ffi::ghostty_tracked_grid_ref_has_value(self.tracked)
                && ffi::ghostty_tracked_grid_ref_point(self.tracked, ffi::GHOSTTY_POINT_TAG_SCREEN, &mut at) == ffi::GHOSTTY_SUCCESS };
            // The tracked row itself went (more output than history): everything up to it went.
            let gone = if ok { self.tracked_y.saturating_sub(at.y) } else { self.tracked_y as usize as u32 + 1 };
            self.dropped += gone as usize;
        }
        self.untrack();
        if alternate { return; }
        let y = (self.kept() + usize::from(self.rows())).saturating_sub(1) as u32;
        let mut tracked = ptr::null_mut();
        // SAFETY: a live terminal, a screen point inside it and an out-pointer for the handle.
        if unsafe { ffi::ghostty_terminal_grid_ref_track(self.raw, point(ffi::GHOSTTY_POINT_TAG_SCREEN, 0, y), &mut tracked) }
            == ffi::GHOSTTY_SUCCESS {
            self.tracked = tracked;
            self.tracked_y = y;
        }
    }
}

fn point(tag: ffi::GhosttyPointTag, x: u16, y: u32) -> ffi::GhosttyPoint {
    ffi::GhosttyPoint { tag, value: ffi::GhosttyPointValue { coordinate: ffi::GhosttyPointCoordinate { x, y } } }
}

impl Drop for Ghostty {
    fn drop(&mut self) {
        self.untrack();
        // SAFETY: `raw` came from ghostty_terminal_new and is freed once; `collected` is dropped after this.
        unsafe { ffi::ghostty_terminal_free(self.raw) };
    }
}

fn color(c: ffi::GhosttyStyleColor) -> Color {
    match c.tag {
        // SAFETY: the tagged union holds `palette` for this tag.
        ffi::GHOSTTY_STYLE_COLOR_PALETTE => Color::Indexed(unsafe { c.value.palette }),
        ffi::GHOSTTY_STYLE_COLOR_RGB => {
            // SAFETY: the tagged union holds `rgb` for this tag.
            let rgb = unsafe { c.value.rgb };
            Color::Rgb(rgb.r, rgb.g, rgb.b)
        }
        _ => Color::Reset,
    }
}

fn style(s: &ffi::GhosttyStyle) -> Style {
    let mut st = Style::new().fg(color(s.fg_color)).bg(color(s.bg_color));
    let underline = s.flags & ffi::GHOSTTY_STYLE_UNDERLINE_MASK != 0;
    for (flag, m) in [(ffi::GHOSTTY_STYLE_FLAG_BOLD, Modifier::BOLD), (ffi::GHOSTTY_STYLE_FLAG_FAINT, Modifier::DIM), (ffi::GHOSTTY_STYLE_FLAG_ITALIC, Modifier::ITALIC),
                      (ffi::GHOSTTY_STYLE_FLAG_INVERSE, Modifier::REVERSED), (ffi::GHOSTTY_STYLE_FLAG_STRIKETHROUGH, Modifier::CROSSED_OUT),
                      (ffi::GHOSTTY_STYLE_FLAG_BLINK, Modifier::SLOW_BLINK), (ffi::GHOSTTY_STYLE_FLAG_INVISIBLE, Modifier::HIDDEN)] {
        if s.flags & flag != 0 { st = st.add_modifier(m); }
    }
    if underline { st = st.add_modifier(Modifier::UNDERLINED); }
    if underline && s.underline_color.tag != ffi::GHOSTTY_STYLE_COLOR_NONE {
        st = st.underline_color(color(s.underline_color));
    }
    st
}

impl Engine for Ghostty {
    fn process(&mut self, bytes: &[u8]) {
        self.notes.feed(bytes, &mut self.effects);
        self.cursor_set.feed(bytes);
        // In pieces small enough that the tracked bottom row can't scroll out of the whole history within one piece
        // (a byte scrolls at most one row, bar CSI S), so every dropped row is counted.
        for piece in bytes.chunks((self.scrollback / 2).max(256)) {
            if piece.is_empty() { continue; }
            let (was_alternate, before) = (self.alternate_screen(), self.issued());
            // SAFETY: a live terminal and a byte slice; the library accepts any bytes. Callbacks run during this call
            // and only touch `collected`.
            unsafe { ffi::ghostty_terminal_vt_write(self.raw, piece.as_ptr(), piece.len()) };
            self.count_dropped(was_alternate);
            // A switch between the screens (a full-screen program starting or ending, or a reset) shows other rows,
            // and the primary screen isn't tracked meanwhile: number them above every number in use, so a row number
            // never names two rows' text.
            if self.alternate_screen() != was_alternate {
                self.renumber(before);
                self.count_dropped(true);
            }
        }
        self.clamp_view();
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
        // Resize with the view following the live screen, then go back to the same distance from the bottom (0: keep
        // following; reflow could otherwise leave a following view scrolled back, P1-3). This started as the
        // workaround for ghostty-audit P0-1: at pin c5a21edf a resize with the view pinned on history could leave the
        // library's viewport pin on a freed page. 9ad3c422 no longer needs it (the audit's re-review ran the P0-1
        // logs without it); it is kept because it is cheap and keeps the view's distance explicit.
        let back = self.scroll_offset();
        if back > 0 { self.set_scroll_offset(0); }
        let before = self.issued();
        // The history budget follows the width: raised before a widening reflow (so it prunes nothing), lowered
        // after a narrowing one.
        let wider = cols.max(1) > self.cols();
        if wider { let _ = self.set_budget(cols.max(1)); }
        // SAFETY: a live terminal; cell pixel sizes are nominal (no pixel queries are answered).
        let _ = unsafe { ffi::ghostty_terminal_resize(self.raw, cols.max(1), rows.max(1), 1, 1) };
        if !wider { let _ = self.set_budget(cols.max(1)); }
        // Reflow re-wraps the history, so rows change without being dropped (P2-3): counting them as dropped would
        // number rows backwards and a remembered row would name other text. Instead the rows after a resize are
        // numbered above every number in use before, so an old number reads nothing. Row numbers hold between
        // resizes. Re-track without counting.
        self.renumber(before);
        self.count_dropped(true);
        self.set_scroll_offset(back);
        self.clamp_view();
    }
    fn rows(&self) -> u16 { self.get::<u16>(ffi::GHOSTTY_TERMINAL_DATA_ROWS).unwrap_or(1) }
    fn cols(&self) -> u16 { self.get::<u16>(ffi::GHOSTTY_TERMINAL_DATA_COLS).unwrap_or(1) }
    fn application_cursor(&self) -> bool { self.mode(1) }
    fn bracketed_paste(&self) -> bool { self.mode(2004) }
    fn alternate_screen(&self) -> bool {
        self.get::<ffi::GhosttyTerminalScreen>(ffi::GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN)
            .is_some_and(|s| s == ffi::GHOSTTY_TERMINAL_SCREEN_ALTERNATE)
    }
    fn mouse(&self) -> (MouseMode, MouseEncoding) {
        let mode = if self.mode(1003) { MouseMode::AnyMotion } else if self.mode(1002) { MouseMode::ButtonMotion }
            else if self.mode(1000) { MouseMode::PressRelease } else if self.mode(9) { MouseMode::Press } else { MouseMode::None };
        let enc = if self.mode(1006) { MouseEncoding::Sgr } else if self.mode(1005) { MouseEncoding::Utf8 } else { MouseEncoding::Default };
        (mode, enc)
    }
    fn cursor(&self) -> Option<(u16, u16, CursorShape)> {
        if self.scroll_offset() > 0 || !self.get::<bool>(ffi::GHOSTTY_TERMINAL_DATA_CURSOR_VISIBLE).unwrap_or(true) { return None; }
        let x = self.get::<u16>(ffi::GHOSTTY_TERMINAL_DATA_CURSOR_X)?;
        let y = self.get::<u16>(ffi::GHOSTTY_TERMINAL_DATA_CURSOR_Y)?;
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
        let n = n.min(self.exposed());
        // 0 follows the live screen as output arrives; a row pins the view to that content.
        if n == 0 {
            return self.scroll(ffi::GHOSTTY_TERMINAL_SCROLL_VIEWPORT_BOTTOM, ffi::GhosttyTerminalScrollViewportValue { _padding: [0; 2] });
        }
        let s = self.scrollbar();
        let top = (s.total.saturating_sub(s.len) as usize).saturating_sub(n);
        self.scroll(ffi::GHOSTTY_TERMINAL_SCROLL_VIEWPORT_ROW, ffi::GhosttyTerminalScrollViewportValue { row: top });
    }
    fn scrollback_len(&self) -> usize { self.exposed() }
    fn scrolled(&self) -> usize { self.dropped + self.kept() }
    fn renumber_from(&mut self, from: usize) {
        self.renumber(from);
        self.count_dropped(true);
    }
    fn row_text(&mut self, row: usize) -> Option<(Vec<char>, Vec<u16>, bool)> {
        // Screen coordinates: 0 is the oldest kept row; `row` counts dropped rows too. Rows kept beyond the lines
        // asked for are hidden.
        let y = row.checked_sub(self.dropped)?;
        if y < self.kept() - self.exposed() || y >= self.kept() + usize::from(self.rows()) { return None; }
        let (mut chars, mut starts) = (Vec::new(), Vec::new());
        for col in 0..self.cols() {
            let Some((text, wide, _)) = self.cell(point(ffi::GHOSTTY_POINT_TAG_SCREEN, col, y as u32), false) else { break };
            if wide == ffi::GHOSTTY_CELL_WIDE_SPACER_TAIL || wide == ffi::GHOSTTY_CELL_WIDE_SPACER_HEAD { continue; }
            for c in if text.is_empty() { " ".to_string() } else { text }.chars() {
                chars.push(c);
                starts.push(col);
            }
        }
        let wrapped = self.wrapped(point(ffi::GHOSTTY_POINT_TAG_SCREEN, 0, y as u32));
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
                let Some((text, wide, st)) = self.cell(point(ffi::GHOSTTY_POINT_TAG_VIEWPORT, col, row as u32), true) else { continue };
                if wide == ffi::GHOSTTY_CELL_WIDE_SPACER_TAIL { continue; }
                if let Some(out) = buf.cell_mut((area.x + col, area.y + row)) {
                    out.set_symbol(if text.is_empty() || wide == ffi::GHOSTTY_CELL_WIDE_SPACER_HEAD { " " } else { &text });
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
                let Some((text, wide, _)) = self.cell(point(ffi::GHOSTTY_POINT_TAG_VIEWPORT, col, row as u32), false) else { break };
                if wide == ffi::GHOSTTY_CELL_WIDE_SPACER_TAIL { continue; }
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
        c.feed(b"\x1b[6 q");
        assert_eq!(c.chosen, Some(6));
        c.feed(b"\x1bc");
        assert_eq!(c.chosen, None, "a full reset (RIS) restores the user's cursor");
    }

    /// ghostty-audit P1-1: an OSC ends at an ESC not followed by `\`, as the terminal's parser ends it, so a program
    /// can't carry escape sequences through a notification to the user's terminal.
    #[test]
    fn a_notification_never_carries_escapes_or_controls() {
        let mut n = Notifications::default();
        let mut out = Vec::new();
        n.feed(b"\x1b]9;hi\x1b[2J\x1b]52;c;?\x07", &mut out);
        n.feed(b"\x1b]9;a\rb\x07\x1b]9;tab\there\x07\x1b]9;del\x7f\x07", &mut out);
        assert_eq!(out, vec![], "unterminated before ESC [, then OSC 52 (not a notification), then control bytes");
        n.feed(b"\x1b]9;one\x1b\x1b]9;two\x07", &mut out);
        assert_eq!(out, vec![Effect::Notify(b"9;two".to_vec())], "ESC ESC ] starts a new OSC");
        let mut g = Ghostty::new(40, 4, 100).unwrap();
        g.process(b"\x1b]9;hi\x1b[2J\x1b]52;c;?\x07");
        assert!(g.take_effects().is_empty());
    }

    /// ghostty-audit P1-2: a pane made narrow and then widened keeps the lines asked for (the budget follows the
    /// width on every resize), and only those are exposed when narrow.
    #[test]
    fn a_widened_pane_keeps_its_history() {
        for (from, to) in [(40u16, 200u16), (20, 240), (80, 200), (20, 400)] {
            let mut g = Ghostty::new(from, 10, 2000).unwrap();
            g.resize(to, 10);
            let line = "x".repeat(usize::from(to) - 1) + "\r\n";
            for _ in 0..3000 { g.process(line.as_bytes()); }
            assert_eq!(g.scrollback_len(), 2000, "{from} -> {to} columns");
        }
        let mut g = Ghostty::new(80, 10, 2000).unwrap();
        for _ in 0..8000 { g.process(b"line\r\n"); }
        assert_eq!(g.scrollback_len(), 2000, "narrow: the extra budget stays hidden");
    }

    /// ghostty-audit P2-3 and P1-3: a resize (reflow) never numbers rows backwards or reuses a number, so a row
    /// remembered before reads nothing rather than other text; a following view keeps following.
    #[test]
    fn a_resize_never_reuses_row_numbers() {
        let mut g = Ghostty::new(40, 10, 500).unwrap();
        for i in 0..600 { g.process(format!("{i:03} {}\r\n", "w".repeat(50)).as_bytes()); }
        let before = g.scrolled() + usize::from(g.rows());
        let remembered: Vec<usize> = (g.scrolled() - g.scrollback_len()..before).collect();
        for (c, r) in [(80, 10), (30, 12), (120, 8)] {
            g.resize(c, r);
            assert_eq!(g.scroll_offset(), 0, "following stays following");
            assert!(g.scrolled() - g.scrollback_len() >= before, "numbers above every earlier one");
            assert!(remembered.iter().all(|row| g.row_text(*row).is_none()), "an old number reads nothing");
        }
        g.set_scroll_offset(30);
        g.resize(50, 10);
        assert_eq!(g.scroll_offset(), 30, "a view scrolled back stays as far back");
    }

    /// ghostty-audit P3-1: images are not kept (vqro draws none).
    #[test]
    fn kitty_images_are_not_stored() {
        let mut g = Ghostty::new(40, 4, 100).unwrap();
        // Built without Kitty graphics: no storage to ask about, or none allowed.
        assert!(matches!(g.get::<u64>(ffi::GHOSTTY_TERMINAL_DATA_KITTY_IMAGE_STORAGE_LIMIT), None | Some(0)));
        g.process(b"\x1b_Ga=T,f=24,s=1,v=1;AAAA\x1b\\after");
        assert!(g.contents().contains("after"), "an image command is consumed and ignored");
    }
}

#[cfg(test)]
/// vqro-lead-secondary's security controls (lead-secondary/native-fixes/security), kept as regression tests.
mod independent_security_scanner_controls {
    use super::*;

    fn collected(bytes: &[u8], split: usize) -> Vec<Effect> {
        let mut scanner = Notifications::default();
        let mut out = Vec::new();
        scanner.feed(&bytes[..split], &mut out);
        scanner.feed(&bytes[split..], &mut out);
        out
    }
    fn every_split(bytes: &[u8], expected: Vec<Effect>) {
        for split in 0..=bytes.len() {
            assert_eq!(collected(bytes, split), expected, "split={split}");
        }
        let mut scanner = Notifications::default();
        let mut out = Vec::new();
        for byte in bytes { scanner.feed(std::slice::from_ref(byte), &mut out); }
        assert_eq!(out, expected, "one-byte writes");
    }

    #[test]
    fn independent_scanner_preserves_valid_notifications_across_every_split() {
        every_split(b"\x1b]9;done\x07", vec![Effect::Notify(b"9;done".to_vec())]);
        every_split(b"\x1b]777;notify;T;B\x1b\\", vec![Effect::Notify(b"777;notify;T;B".to_vec())]);
        let unicode_body = "9;通知 \u{1f980}".as_bytes();
        let wire = [b"\x1b]".as_slice(), unicode_body, b"\x07"].concat();
        every_split(&wire, vec![Effect::Notify(unicode_body.to_vec())]);
        every_split(b"\x1b]9;4;1;50\x07\x1b]9;9;/w\x07\x1b]777;other\x07", vec![]);
    }

    #[test]
    fn independent_scanner_cancellation_overflow_and_ascii_escape_attack() {
        every_split(b"\x1b]9;hi\x1b[2J\x1b]52;c;?\x07", vec![]);
        every_split(b"\x1b]9;x\x18\x07\x1b]9;x\x1a\x07", vec![]);
        every_split(b"\x1b]9;x\rY\x07\x1b]777;notify;T;tab\there\x07\x1b]9;del\x7f\x07", vec![]);
        every_split(b"\x1b]9;one\x1b\x1b]9;two\x07", vec![Effect::Notify(b"9;two".to_vec())]);
        let mut scanner = Notifications::default();
        let mut out = Vec::new();
        scanner.feed(b"\x1b]9;unfinished\x1b", &mut out);
        assert!(out.is_empty());
        scanner.feed(b"[2J\x1b]9;recovered\x07", &mut out);
        assert_eq!(out, vec![Effect::Notify(b"9;recovered".to_vec())]);
        let exact_body = [b"9;".as_slice(), vec![b'x'; MAX_OSC - 2].as_slice()].concat();
        let exact = [b"\x1b]".as_slice(), &exact_body, b"\x1b\\"].concat();
        assert_eq!(collected(&exact, exact.len()-1), vec![Effect::Notify(exact_body)]);
        let over = [b"\x1b]9;".as_slice(), vec![b'x'; MAX_OSC].as_slice(), b"\x07\x1b]9;ok\x07"].concat();
        assert_eq!(collected(&over, MAX_OSC), vec![Effect::Notify(b"9;ok".to_vec())]);
        let mut scanner = Notifications::default(); let mut out = Vec::new();
        for _ in 0..MAX_EFFECTS+3 { scanner.feed(b"\x1b]9;x\x07", &mut out); }
        assert_eq!(out.len(), MAX_EFFECTS);
    }

    #[test]
    fn independent_scanner_rejects_c1_notification_body() {
        every_split(b"\x1b]9;safe\x9c\x9d52;c;?\x9c\x07", vec![]);
    }
    #[test]
    fn independent_scanner_rejects_unicode_c1_and_invalid_utf8() {
        assert!(notification("9;bad\u{009c}after".as_bytes()).is_none());
        assert!(notification(b"9;bad\xff").is_none());
    }

    // ESC then BEL cancels the string, as the terminal's parser does.
    #[test]
    fn independent_scanner_escape_then_bel_is_not_a_valid_terminator() {
        every_split(b"\x1b]9;cancelled\x1b\x07", vec![]);
    }
}
