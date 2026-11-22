//! The libghostty-vt C API that the `libghostty` engine uses, declared by hand from the vendored headers
//! (`vendor/libghostty-vt/include/ghostty/vt/*.h`, pin 9ad3c422). Only what the engine and `vqro-rex` (snapshot
//! decoding and VT formatting) call is here.
//!
//! bindgen isn't used: these declarations are checked against the headers instead. `bindings_check.c` asserts
//! with the C compiler every struct size and field offset, every constant's value, GhosttyStyle's alignment, and
//! each function's and callback's parameter types (by assignment); `build.rs` compiles it with the same Zig. Still
//! hand-checked: the Rust `extern` declarations themselves and what the `void*` payloads must point at. On the Rust side, the `layout` tests assert the same sizes and offsets, and that every constant here
//! has its `SAME(..)` line there with the same value. Change both files together.
//!
//! C enums here are `int` (`GHOSTTY_ENUM_TYPED`), so they are `c_int` constants.
use std::os::raw::{c_int, c_void};

// ---- types.h --------------------------------------------------------------------------------------------------

pub type GhosttyResult = c_int;
pub const GHOSTTY_SUCCESS: GhosttyResult = 0;
pub const GHOSTTY_INVALID_VALUE: GhosttyResult = -2;
pub const GHOSTTY_OUT_OF_SPACE: GhosttyResult = -3;

/// A borrowed byte string.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GhosttyString {
    pub ptr: *const u8,
    pub len: usize,
}

#[repr(C)]
pub struct GhosttyTerminalImpl {
    _opaque: [u8; 0],
}
pub type GhosttyTerminal = *mut GhosttyTerminalImpl;

#[repr(C)]
pub struct GhosttyTrackedGridRefImpl {
    _opaque: [u8; 0],
}
pub type GhosttyTrackedGridRef = *mut GhosttyTrackedGridRefImpl;

/// Never constructed here: a null allocator means the library's default.
#[repr(C)]
pub struct GhosttyAllocator {
    _opaque: [u8; 0],
}

// ---- point.h --------------------------------------------------------------------------------------------------

pub type GhosttyPointTag = c_int;
pub const GHOSTTY_POINT_TAG_ACTIVE: GhosttyPointTag = 0;
pub const GHOSTTY_POINT_TAG_VIEWPORT: GhosttyPointTag = 1;
pub const GHOSTTY_POINT_TAG_SCREEN: GhosttyPointTag = 2;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct GhosttyPointCoordinate {
    pub x: u16,
    pub y: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union GhosttyPointValue {
    pub coordinate: GhosttyPointCoordinate,
    pub _padding: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyPoint {
    pub tag: GhosttyPointTag,
    pub value: GhosttyPointValue,
}

// ---- grid_ref.h, screen.h ---------------------------------------------------------------------------------------

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GhosttyGridRef {
    pub node: *mut c_void,
    pub x: u16,
    pub y: u16,
}
impl Default for GhosttyGridRef {
    fn default() -> Self { Self { node: std::ptr::null_mut(), x: 0, y: 0 } }
}

pub type GhosttyCell = u64;
pub type GhosttyRow = u64;

pub type GhosttyCellData = c_int;
pub const GHOSTTY_CELL_DATA_CODEPOINT: GhosttyCellData = 1;
pub const GHOSTTY_CELL_DATA_WIDE: GhosttyCellData = 3;

pub type GhosttyCellWide = c_int;
pub const GHOSTTY_CELL_WIDE_NARROW: GhosttyCellWide = 0;
pub const GHOSTTY_CELL_WIDE_WIDE: GhosttyCellWide = 1;
pub const GHOSTTY_CELL_WIDE_SPACER_TAIL: GhosttyCellWide = 2;
pub const GHOSTTY_CELL_WIDE_SPACER_HEAD: GhosttyCellWide = 3;

pub type GhosttyRowData = c_int;
pub const GHOSTTY_ROW_DATA_WRAP: GhosttyRowData = 1;

// ---- color.h, style.h -----------------------------------------------------------------------------------------

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct GhosttyColorRgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union GhosttyStyleColorValue {
    pub palette: u8,
    pub rgb: GhosttyColorRgb,
}

pub const GHOSTTY_STYLE_COLOR_NONE: u8 = 0;
pub const GHOSTTY_STYLE_COLOR_PALETTE: u8 = 1;
pub const GHOSTTY_STYLE_COLOR_RGB: u8 = 2;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyStyleColor {
    /// One of the GHOSTTY_STYLE_COLOR_* tags.
    pub tag: u8,
    pub value: GhosttyStyleColorValue,
}

/// Bits 0-7: attributes (GHOSTTY_STYLE_FLAG_*); bits 8-10: the underline style (0: none).
pub type GhosttyStyleFlags = u16;
pub const GHOSTTY_STYLE_FLAG_BOLD: GhosttyStyleFlags = 1 << 0;
pub const GHOSTTY_STYLE_FLAG_ITALIC: GhosttyStyleFlags = 1 << 1;
pub const GHOSTTY_STYLE_FLAG_FAINT: GhosttyStyleFlags = 1 << 2;
pub const GHOSTTY_STYLE_FLAG_BLINK: GhosttyStyleFlags = 1 << 3;
pub const GHOSTTY_STYLE_FLAG_INVERSE: GhosttyStyleFlags = 1 << 4;
pub const GHOSTTY_STYLE_FLAG_INVISIBLE: GhosttyStyleFlags = 1 << 5;
pub const GHOSTTY_STYLE_FLAG_STRIKETHROUGH: GhosttyStyleFlags = 1 << 6;
pub const GHOSTTY_STYLE_UNDERLINE_SHIFT: u32 = 8;
pub const GHOSTTY_STYLE_UNDERLINE_MASK: GhosttyStyleFlags = 0x7 << GHOSTTY_STYLE_UNDERLINE_SHIFT;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyStyle {
    pub fg_color: GhosttyStyleColor,
    pub bg_color: GhosttyStyleColor,
    pub underline_color: GhosttyStyleColor,
    pub flags: GhosttyStyleFlags,
    pub reserved: u16,
}
impl Default for GhosttyStyle {
    fn default() -> Self {
        let none = GhosttyStyleColor { tag: GHOSTTY_STYLE_COLOR_NONE, value: GhosttyStyleColorValue { palette: 0 } };
        Self { fg_color: none, bg_color: none, underline_color: none, flags: 0, reserved: 0 }
    }
}

// ---- modes.h ----------------------------------------------------------------------------------------------------

/// A mode: bits 0-14 its number, bit 15 set for ANSI modes (clear for DEC private modes).
pub type GhosttyMode = u16;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct GhosttyTerminalModeConfig {
    pub mode: GhosttyMode,
    pub value: bool,
}

// ---- terminal.h -------------------------------------------------------------------------------------------------

pub type GhosttyTerminalOption = c_int;
pub const GHOSTTY_TERMINAL_OPT_USERDATA: GhosttyTerminalOption = 1;
pub const GHOSTTY_TERMINAL_OPT_WRITE_PTY: GhosttyTerminalOption = 2;
pub const GHOSTTY_TERMINAL_OPT_BELL: GhosttyTerminalOption = 3;
pub const GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT: GhosttyTerminalOption = 16;
pub const GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE: GhosttyTerminalOption = 27;
pub const GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES: GhosttyTerminalOption = 28;
pub const GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES: GhosttyTerminalOption = 29;
pub const GHOSTTY_TERMINAL_OPT_MODE_DEFAULT: GhosttyTerminalOption = 34;
pub const GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE_MAX_BYTES: GhosttyTerminalOption = 40;

pub type GhosttyTerminalData = c_int;
pub const GHOSTTY_TERMINAL_DATA_COLS: GhosttyTerminalData = 1;
pub const GHOSTTY_TERMINAL_DATA_ROWS: GhosttyTerminalData = 2;
pub const GHOSTTY_TERMINAL_DATA_CURSOR_X: GhosttyTerminalData = 3;
pub const GHOSTTY_TERMINAL_DATA_CURSOR_Y: GhosttyTerminalData = 4;
pub const GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN: GhosttyTerminalData = 6;
pub const GHOSTTY_TERMINAL_DATA_CURSOR_VISIBLE: GhosttyTerminalData = 7;
pub const GHOSTTY_TERMINAL_DATA_SCROLLBAR: GhosttyTerminalData = 9;
pub const GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS: GhosttyTerminalData = 15;
pub const GHOSTTY_TERMINAL_DATA_KITTY_IMAGE_STORAGE_LIMIT: GhosttyTerminalData = 26;
pub const GHOSTTY_TERMINAL_DATA_MODE: GhosttyTerminalData = 37;

pub type GhosttyTerminalScreen = c_int;
pub const GHOSTTY_TERMINAL_SCREEN_PRIMARY: GhosttyTerminalScreen = 0;
pub const GHOSTTY_TERMINAL_SCREEN_ALTERNATE: GhosttyTerminalScreen = 1;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct GhosttyTerminalScrollbar {
    pub total: u64,
    pub offset: u64,
    pub len: u64,
}

pub type GhosttyTerminalScrollViewportTag = c_int;
pub const GHOSTTY_TERMINAL_SCROLL_VIEWPORT_BOTTOM: GhosttyTerminalScrollViewportTag = 1;
pub const GHOSTTY_TERMINAL_SCROLL_VIEWPORT_ROW: GhosttyTerminalScrollViewportTag = 3;

#[repr(C)]
#[derive(Clone, Copy)]
pub union GhosttyTerminalScrollViewportValue {
    pub delta: isize,
    pub row: usize,
    pub _padding: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GhosttyTerminalScrollViewport {
    pub tag: GhosttyTerminalScrollViewportTag,
    pub value: GhosttyTerminalScrollViewportValue,
}

pub type GhosttyClipboardLocation = c_int;
pub const GHOSTTY_CLIPBOARD_LOCATION_STANDARD: GhosttyClipboardLocation = 0;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GhosttyClipboardContent {
    pub mime: GhosttyString,
    pub data: GhosttyString,
}

pub type GhosttyClipboardWriteResult = c_int;
pub const GHOSTTY_CLIPBOARD_WRITE_RESULT_SUCCESS: GhosttyClipboardWriteResult = 0;
pub const GHOSTTY_CLIPBOARD_WRITE_RESULT_DENIED: GhosttyClipboardWriteResult = 1;
pub const GHOSTTY_CLIPBOARD_WRITE_RESULT_UNSUPPORTED: GhosttyClipboardWriteResult = 2;
pub const GHOSTTY_CLIPBOARD_WRITE_RESULT_INVALID_DATA: GhosttyClipboardWriteResult = 4;

/// A sized struct: `size` is its size in bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GhosttyClipboardWriteReply {
    pub size: usize,
    pub result: GhosttyClipboardWriteResult,
    pub remember: bool,
}

pub type GhosttyClipboardWriteReplyFn = Option<unsafe extern "C" fn(write: *const GhosttyClipboardWrite, reply: *const GhosttyClipboardWriteReply)>;

/// Frozen layout (64 bytes on 64-bit targets). Borrowed for the callback's duration only.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GhosttyClipboardWrite {
    pub location: GhosttyClipboardLocation,
    pub contents: *const GhosttyClipboardContent,
    /// Zero means: clear the destination.
    pub contents_len: usize,
    pub name: GhosttyString,
    pub granted: bool,
    pub can_remember: bool,
    pub ctx: *const c_void,
    pub reply: GhosttyClipboardWriteReplyFn,
}

// ---- snapshot.h, formatter.h (vqro-rex: a Rex terminal's Ghostty snapshot becomes a VT repaint) --------------

#[repr(C)]
pub struct GhosttySnapshotDecoderImpl {
    _opaque: [u8; 0],
}
pub type GhosttySnapshotDecoder = *mut GhosttySnapshotDecoderImpl;

#[repr(C)]
pub struct GhosttyFormatterImpl {
    _opaque: [u8; 0],
}
pub type GhosttyFormatter = *mut GhosttyFormatterImpl;

pub type GhosttyFormatterOption = c_int;
pub const GHOSTTY_FORMATTER_OPT_EMIT: GhosttyFormatterOption = 1;
pub const GHOSTTY_FORMATTER_OPT_TRIM: GhosttyFormatterOption = 3;
pub const GHOSTTY_FORMATTER_OPT_EXTRAS: GhosttyFormatterOption = 5;
/// `GhosttyFormatterFormat`, set through GHOSTTY_FORMATTER_OPT_EMIT.
pub type GhosttyFormatterFormat = c_int;
pub const GHOSTTY_FORMATTER_FORMAT_VT: GhosttyFormatterFormat = 1;
/// `GhosttyFormatterExtras` (uint32_t bitmask), set through GHOSTTY_FORMATTER_OPT_EXTRAS.
pub type GhosttyFormatterExtras = u32;
pub const GHOSTTY_FORMATTER_EXTRAS_ALL: GhosttyFormatterExtras = 0xFFF;
pub const GHOSTTY_FORMATTER_EXTRA_PALETTE: GhosttyFormatterExtras = 1 << 0;
pub const GHOSTTY_FORMATTER_EXTRA_CHARSETS: GhosttyFormatterExtras = 1 << 11;

pub type GhosttyTerminalBellFn = Option<unsafe extern "C" fn(terminal: GhosttyTerminal, userdata: *mut c_void)>;
pub type GhosttyTerminalClipboardWriteFn =
    Option<unsafe extern "C" fn(terminal: GhosttyTerminal, userdata: *mut c_void, write: *const GhosttyClipboardWrite)>;

unsafe extern "C" {
    pub fn ghostty_snapshot_decoder_new_buf(allocator: *const GhosttyAllocator, decoder: *mut GhosttySnapshotDecoder, ptr: *const u8, len: usize) -> GhosttyResult;
    pub fn ghostty_snapshot_decoder_decode(decoder: GhosttySnapshotDecoder, terminal: *mut GhosttyTerminal) -> GhosttyResult;
    pub fn ghostty_snapshot_decoder_free(decoder: GhosttySnapshotDecoder);
    pub fn ghostty_formatter_terminal_new(allocator: *const GhosttyAllocator, formatter: *mut GhosttyFormatter, terminal: GhosttyTerminal) -> GhosttyResult;
    pub fn ghostty_formatter_set(formatter: GhosttyFormatter, option: GhosttyFormatterOption, value: *const c_void) -> GhosttyResult;
    pub fn ghostty_formatter_format_alloc(formatter: GhosttyFormatter, allocator: *const GhosttyAllocator, out_ptr: *mut *mut u8, out_len: *mut usize) -> GhosttyResult;
    pub fn ghostty_formatter_free(formatter: GhosttyFormatter);
    pub fn ghostty_free(allocator: *const GhosttyAllocator, ptr: *mut u8, len: usize);
    pub fn ghostty_terminal_new(allocator: *const GhosttyAllocator, terminal: *mut GhosttyTerminal, cols: u16, rows: u16) -> GhosttyResult;
    pub fn ghostty_terminal_free(terminal: GhosttyTerminal);
    pub fn ghostty_terminal_set(terminal: GhosttyTerminal, option: GhosttyTerminalOption, value: *const c_void) -> GhosttyResult;
    pub fn ghostty_terminal_get(terminal: GhosttyTerminal, data: GhosttyTerminalData, out: *mut c_void) -> GhosttyResult;
    pub fn ghostty_terminal_vt_write(terminal: GhosttyTerminal, data: *const u8, len: usize);
    pub fn ghostty_terminal_resize(terminal: GhosttyTerminal, cols: u16, rows: u16, cell_width_px: u32, cell_height_px: u32) -> GhosttyResult;
    pub fn ghostty_terminal_scroll_viewport(terminal: GhosttyTerminal, behavior: GhosttyTerminalScrollViewport) -> GhosttyResult;
    pub fn ghostty_terminal_grid_ref(terminal: GhosttyTerminal, point: GhosttyPoint, out_ref: *mut GhosttyGridRef) -> GhosttyResult;
    pub fn ghostty_terminal_grid_ref_track(terminal: GhosttyTerminal, point: GhosttyPoint, out_ref: *mut GhosttyTrackedGridRef) -> GhosttyResult;
    pub fn ghostty_tracked_grid_ref_free(tracked: GhosttyTrackedGridRef);
    pub fn ghostty_tracked_grid_ref_has_value(tracked: GhosttyTrackedGridRef) -> bool;
    pub fn ghostty_tracked_grid_ref_point(tracked: GhosttyTrackedGridRef, tag: GhosttyPointTag, out_point: *mut GhosttyPointCoordinate) -> GhosttyResult;
    pub fn ghostty_grid_ref_cell(grid_ref: *const GhosttyGridRef, out_cell: *mut GhosttyCell) -> GhosttyResult;
    pub fn ghostty_grid_ref_row(grid_ref: *const GhosttyGridRef, out_row: *mut GhosttyRow) -> GhosttyResult;
    pub fn ghostty_grid_ref_graphemes(grid_ref: *const GhosttyGridRef, buf: *mut u32, buf_len: usize, out_written: *mut usize) -> GhosttyResult;
    pub fn ghostty_grid_ref_style(grid_ref: *const GhosttyGridRef, out_style: *mut GhosttyStyle) -> GhosttyResult;
    pub fn ghostty_grid_ref_hyperlink_uri(grid_ref: *const GhosttyGridRef, buf: *mut u8, buf_len: usize, out_written: *mut usize) -> GhosttyResult;
    pub fn ghostty_cell_get(cell: GhosttyCell, data: GhosttyCellData, out: *mut c_void) -> GhosttyResult;
    pub fn ghostty_row_get(row: GhosttyRow, data: GhosttyRowData, out: *mut c_void) -> GhosttyResult;
}

/// The Rust side of `bindings_check.c`: the same sizes, alignments and offsets (64-bit targets).
#[cfg(all(test, target_pointer_width = "64"))]
mod layout {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};

    #[test]
    fn matches_the_headers() {
        assert_eq!((size_of::<GhosttyString>(), offset_of!(GhosttyString, len)), (16, 8));
        assert_eq!((size_of::<GhosttyPointCoordinate>(), offset_of!(GhosttyPointCoordinate, y)), (8, 4));
        assert_eq!((size_of::<GhosttyPointValue>(), size_of::<GhosttyPoint>(), offset_of!(GhosttyPoint, value)), (16, 24, 8));
        assert_eq!((size_of::<GhosttyGridRef>(), offset_of!(GhosttyGridRef, x), offset_of!(GhosttyGridRef, y)), (16, 8, 10));
        assert_eq!((size_of::<GhosttyColorRgb>(), size_of::<GhosttyStyleColorValue>(), size_of::<GhosttyStyleColor>(), offset_of!(GhosttyStyleColor, value)), (3, 3, 4, 1));
        assert_eq!((size_of::<GhosttyStyle>(), align_of::<GhosttyStyle>()), (16, 2));
        assert_eq!((offset_of!(GhosttyStyle, bg_color), offset_of!(GhosttyStyle, underline_color), offset_of!(GhosttyStyle, flags), offset_of!(GhosttyStyle, reserved)), (4, 8, 12, 14));
        assert_eq!((size_of::<GhosttyTerminalModeConfig>(), offset_of!(GhosttyTerminalModeConfig, value)), (4, 2));
        assert_eq!((size_of::<GhosttyTerminalScrollbar>(), offset_of!(GhosttyTerminalScrollbar, offset), offset_of!(GhosttyTerminalScrollbar, len)), (24, 8, 16));
        assert_eq!((size_of::<GhosttyTerminalScrollViewportValue>(), size_of::<GhosttyTerminalScrollViewport>(), offset_of!(GhosttyTerminalScrollViewport, value)), (16, 24, 8));
        assert_eq!((size_of::<GhosttyClipboardContent>(), offset_of!(GhosttyClipboardContent, data)), (32, 16));
        assert_eq!((size_of::<GhosttyClipboardWriteReply>(), offset_of!(GhosttyClipboardWriteReply, result), offset_of!(GhosttyClipboardWriteReply, remember)), (16, 8, 12));
        assert_eq!(size_of::<GhosttyClipboardWrite>(), 64);
        assert_eq!((offset_of!(GhosttyClipboardWrite, contents), offset_of!(GhosttyClipboardWrite, contents_len), offset_of!(GhosttyClipboardWrite, name),
                    offset_of!(GhosttyClipboardWrite, granted), offset_of!(GhosttyClipboardWrite, can_remember), offset_of!(GhosttyClipboardWrite, ctx),
                    offset_of!(GhosttyClipboardWrite, reply)), (8, 16, 24, 40, 41, 48, 56));
    }

    /// Every constant here has a `SAME(name, value)` line in bindings_check.c with the same value, and every such line
    /// names a constant here (ghostty-audit P3-2): the C compiler checks those lines against the headers, so an edit
    /// to only one side fails somewhere.
    #[test]
    fn constants_match_the_c_check() {
        macro_rules! values { ($($n:ident),* $(,)?) => { vec![$((stringify!($n), $n as i64)),*] } }
        let rust: std::collections::BTreeMap<&str, i64> = values![GHOSTTY_SUCCESS, GHOSTTY_INVALID_VALUE, GHOSTTY_OUT_OF_SPACE, GHOSTTY_POINT_TAG_ACTIVE, GHOSTTY_POINT_TAG_VIEWPORT, GHOSTTY_POINT_TAG_SCREEN, GHOSTTY_CELL_DATA_CODEPOINT, GHOSTTY_CELL_DATA_WIDE, GHOSTTY_CELL_WIDE_NARROW, GHOSTTY_CELL_WIDE_WIDE, GHOSTTY_CELL_WIDE_SPACER_TAIL, GHOSTTY_CELL_WIDE_SPACER_HEAD, GHOSTTY_ROW_DATA_WRAP, GHOSTTY_STYLE_COLOR_NONE, GHOSTTY_STYLE_COLOR_PALETTE, GHOSTTY_STYLE_COLOR_RGB, GHOSTTY_STYLE_FLAG_BOLD, GHOSTTY_STYLE_FLAG_ITALIC, GHOSTTY_STYLE_FLAG_FAINT, GHOSTTY_STYLE_FLAG_BLINK, GHOSTTY_STYLE_FLAG_INVERSE, GHOSTTY_STYLE_FLAG_INVISIBLE, GHOSTTY_STYLE_FLAG_STRIKETHROUGH, GHOSTTY_STYLE_UNDERLINE_SHIFT, GHOSTTY_STYLE_UNDERLINE_MASK, GHOSTTY_TERMINAL_OPT_USERDATA, GHOSTTY_TERMINAL_OPT_WRITE_PTY, GHOSTTY_TERMINAL_OPT_BELL, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT, GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE, GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES, GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES, GHOSTTY_TERMINAL_OPT_MODE_DEFAULT, GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE_MAX_BYTES, GHOSTTY_TERMINAL_DATA_COLS, GHOSTTY_TERMINAL_DATA_ROWS, GHOSTTY_TERMINAL_DATA_CURSOR_X, GHOSTTY_TERMINAL_DATA_CURSOR_Y, GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN, GHOSTTY_TERMINAL_DATA_CURSOR_VISIBLE, GHOSTTY_TERMINAL_DATA_SCROLLBAR, GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS, GHOSTTY_TERMINAL_DATA_KITTY_IMAGE_STORAGE_LIMIT, GHOSTTY_TERMINAL_DATA_MODE, GHOSTTY_TERMINAL_SCREEN_PRIMARY, GHOSTTY_TERMINAL_SCREEN_ALTERNATE, GHOSTTY_TERMINAL_SCROLL_VIEWPORT_BOTTOM, GHOSTTY_TERMINAL_SCROLL_VIEWPORT_ROW, GHOSTTY_CLIPBOARD_LOCATION_STANDARD, GHOSTTY_CLIPBOARD_WRITE_RESULT_SUCCESS, GHOSTTY_CLIPBOARD_WRITE_RESULT_DENIED, GHOSTTY_CLIPBOARD_WRITE_RESULT_UNSUPPORTED, GHOSTTY_CLIPBOARD_WRITE_RESULT_INVALID_DATA, GHOSTTY_FORMATTER_OPT_EMIT, GHOSTTY_FORMATTER_OPT_TRIM, GHOSTTY_FORMATTER_OPT_EXTRAS, GHOSTTY_FORMATTER_FORMAT_VT, GHOSTTY_FORMATTER_EXTRAS_ALL, GHOSTTY_FORMATTER_EXTRA_PALETTE, GHOSTTY_FORMATTER_EXTRA_CHARSETS].into_iter().collect();
        let declared: std::collections::BTreeSet<&str> = include_str!("bindings.rs").lines()
            .filter_map(|l| l.strip_prefix("pub const ")).filter_map(|l| l.split(':').next()).collect();
        assert_eq!(declared, rust.keys().copied().collect(), "the list above names every constant declared in this file");
        let eval = |v: &str| -> i64 {
            let v = v.trim();
            if let Some((a, b)) = v.split_once("<<") {
                let base = a.trim().trim_start_matches("0x");
                let base = if a.trim().starts_with("0x") { i64::from_str_radix(base, 16).unwrap() } else { base.parse().unwrap() };
                return base << b.trim().parse::<i64>().unwrap();
            }
            if let Some(h) = v.strip_prefix("0x") { return i64::from_str_radix(h, 16).unwrap(); }
            v.parse().unwrap_or_else(|_| panic!("bindings_check.c value {v}"))
        };
        let mut c = std::collections::BTreeMap::new();
        for part in include_str!("bindings_check.c").split("SAME(").skip(1) {
            // `SAME(GHOSTTY_X, value);` lines (the macro's own definition and size/offset lines are skipped).
            let Some((name, rest)) = part.split_once(',') else { continue };
            if !name.trim().starts_with("GHOSTTY_") { continue; }
            c.insert(name.trim(), eval(&rest[..rest.find(");").unwrap()]));
        }
        assert_eq!(c.iter().map(|(k, v)| (*k, *v)).collect::<std::collections::BTreeMap<&str, i64>>(), rust);
    }
}
