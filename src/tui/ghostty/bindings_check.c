/* Checks bindings.rs against the vendored libghostty-vt headers: every size, offset and value the hand-written
 * Rust declarations rely on. build.rs compiles this with the Zig that builds the library (64-bit targets); the
 * Rust `layout` test asserts the same sizes and offsets. Change bindings.rs, this file and that test together. */
#include <ghostty/vt.h>
#include <stddef.h>
#include <stdint.h>

#define SAME(a, b) _Static_assert((a) == (b), #a " != " #b)

/* types.h */
SAME(GHOSTTY_SUCCESS, 0); SAME(GHOSTTY_INVALID_VALUE, -2); SAME(GHOSTTY_OUT_OF_SPACE, -3);
SAME(sizeof(GhosttyResult), 4);
SAME(sizeof(GhosttyString), 16); SAME(offsetof(GhosttyString, len), 8);

/* point.h */
SAME(GHOSTTY_POINT_TAG_ACTIVE, 0); SAME(GHOSTTY_POINT_TAG_VIEWPORT, 1); SAME(GHOSTTY_POINT_TAG_SCREEN, 2);
SAME(sizeof(GhosttyPointTag), 4);
SAME(sizeof(GhosttyPointCoordinate), 8); SAME(offsetof(GhosttyPointCoordinate, x), 0); SAME(offsetof(GhosttyPointCoordinate, y), 4);
SAME(sizeof(GhosttyPointValue), 16); SAME(sizeof(GhosttyPoint), 24); SAME(offsetof(GhosttyPoint, value), 8);

/* grid_ref.h, screen.h */
SAME(sizeof(GhosttyGridRef), 16); SAME(offsetof(GhosttyGridRef, x), 8); SAME(offsetof(GhosttyGridRef, y), 10);
SAME(sizeof(GhosttyCell), 8); SAME(sizeof(GhosttyRow), 8);
SAME(GHOSTTY_CELL_DATA_CODEPOINT, 1); SAME(GHOSTTY_CELL_DATA_WIDE, 3); SAME(sizeof(GhosttyCellData), 4);
SAME(GHOSTTY_CELL_WIDE_NARROW, 0); SAME(GHOSTTY_CELL_WIDE_WIDE, 1); SAME(GHOSTTY_CELL_WIDE_SPACER_TAIL, 2);
SAME(GHOSTTY_CELL_WIDE_SPACER_HEAD, 3); SAME(sizeof(GhosttyCellWide), 4);
SAME(GHOSTTY_ROW_DATA_WRAP, 1); SAME(sizeof(GhosttyRowData), 4);

/* color.h, style.h */
SAME(sizeof(GhosttyColorRgb), 3); SAME(sizeof(GhosttyColorPaletteIndex), 1);
SAME(sizeof(GhosttyStyleColorValue), 3); SAME(sizeof(GhosttyStyleColor), 4); SAME(offsetof(GhosttyStyleColor, value), 1);
SAME(GHOSTTY_STYLE_COLOR_NONE, 0); SAME(GHOSTTY_STYLE_COLOR_PALETTE, 1); SAME(GHOSTTY_STYLE_COLOR_RGB, 2);
SAME(sizeof(GhosttyStyle), 16); SAME(_Alignof(GhosttyStyle), 2);
SAME(offsetof(GhosttyStyle, bg_color), 4); SAME(offsetof(GhosttyStyle, underline_color), 8);
SAME(offsetof(GhosttyStyle, flags), 12); SAME(offsetof(GhosttyStyle, reserved), 14);
SAME(GHOSTTY_STYLE_FLAG_BOLD, 1 << 0); SAME(GHOSTTY_STYLE_FLAG_ITALIC, 1 << 1); SAME(GHOSTTY_STYLE_FLAG_FAINT, 1 << 2);
SAME(GHOSTTY_STYLE_FLAG_BLINK, 1 << 3); SAME(GHOSTTY_STYLE_FLAG_INVERSE, 1 << 4); SAME(GHOSTTY_STYLE_FLAG_INVISIBLE, 1 << 5);
SAME(GHOSTTY_STYLE_FLAG_STRIKETHROUGH, 1 << 6); SAME(GHOSTTY_STYLE_UNDERLINE_SHIFT, 8); SAME(GHOSTTY_STYLE_UNDERLINE_MASK, 0x700);

/* modes.h */
SAME(sizeof(GhosttyMode), 2);
SAME(sizeof(GhosttyTerminalModeConfig), 4); SAME(offsetof(GhosttyTerminalModeConfig, value), 2);

/* terminal.h */
SAME(GHOSTTY_TERMINAL_OPT_USERDATA, 1); SAME(GHOSTTY_TERMINAL_OPT_WRITE_PTY, 2); SAME(GHOSTTY_TERMINAL_OPT_BELL, 3);
SAME(GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT, 16); SAME(GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE, 27);
SAME(GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES, 28); SAME(GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES, 29);
SAME(GHOSTTY_TERMINAL_OPT_MODE_DEFAULT, 34); SAME(GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE_MAX_BYTES, 40);
SAME(sizeof(GhosttyTerminalOption), 4);
SAME(GHOSTTY_TERMINAL_DATA_COLS, 1); SAME(GHOSTTY_TERMINAL_DATA_ROWS, 2); SAME(GHOSTTY_TERMINAL_DATA_CURSOR_X, 3);
SAME(GHOSTTY_TERMINAL_DATA_CURSOR_Y, 4); SAME(GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN, 6); SAME(GHOSTTY_TERMINAL_DATA_CURSOR_VISIBLE, 7);
SAME(GHOSTTY_TERMINAL_DATA_SCROLLBAR, 9); SAME(GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS, 15);
SAME(GHOSTTY_TERMINAL_DATA_KITTY_IMAGE_STORAGE_LIMIT, 26); SAME(GHOSTTY_TERMINAL_DATA_MODE, 37);
SAME(sizeof(GhosttyTerminalData), 4);
SAME(GHOSTTY_TERMINAL_SCREEN_PRIMARY, 0); SAME(GHOSTTY_TERMINAL_SCREEN_ALTERNATE, 1); SAME(sizeof(GhosttyTerminalScreen), 4);
SAME(sizeof(GhosttyTerminalScrollbar), 24); SAME(offsetof(GhosttyTerminalScrollbar, offset), 8); SAME(offsetof(GhosttyTerminalScrollbar, len), 16);
SAME(GHOSTTY_TERMINAL_SCROLL_VIEWPORT_BOTTOM, 1); SAME(GHOSTTY_TERMINAL_SCROLL_VIEWPORT_ROW, 3);
SAME(sizeof(GhosttyTerminalScrollViewportValue), 16); SAME(sizeof(GhosttyTerminalScrollViewport), 24);
SAME(offsetof(GhosttyTerminalScrollViewport, value), 8);
SAME(GHOSTTY_CLIPBOARD_LOCATION_STANDARD, 0); SAME(sizeof(GhosttyClipboardLocation), 4);
SAME(sizeof(GhosttyClipboardContent), 32); SAME(offsetof(GhosttyClipboardContent, data), 16);
SAME(GHOSTTY_CLIPBOARD_WRITE_RESULT_SUCCESS, 0); SAME(GHOSTTY_CLIPBOARD_WRITE_RESULT_DENIED, 1);
SAME(GHOSTTY_CLIPBOARD_WRITE_RESULT_UNSUPPORTED, 2); SAME(GHOSTTY_CLIPBOARD_WRITE_RESULT_INVALID_DATA, 4);
SAME(sizeof(GhosttyClipboardWriteReply), 16); SAME(offsetof(GhosttyClipboardWriteReply, result), 8);
SAME(offsetof(GhosttyClipboardWriteReply, remember), 12);
SAME(sizeof(GhosttyClipboardWrite), 64); SAME(offsetof(GhosttyClipboardWrite, contents), 8);
SAME(offsetof(GhosttyClipboardWrite, contents_len), 16); SAME(offsetof(GhosttyClipboardWrite, name), 24);
SAME(offsetof(GhosttyClipboardWrite, granted), 40); SAME(offsetof(GhosttyClipboardWrite, can_remember), 41);
SAME(offsetof(GhosttyClipboardWrite, ctx), 48); SAME(offsetof(GhosttyClipboardWrite, reply), 56);

/* The functions bindings.rs declares, with the header's parameter types (a mismatch fails to compile). */
static GhosttyResult (*const f_new)(const GhosttyAllocator*, GhosttyTerminal*, uint16_t, uint16_t) = ghostty_terminal_new;
static void (*const f_free)(GhosttyTerminal) = ghostty_terminal_free;
static GhosttyResult (*const f_set)(GhosttyTerminal, GhosttyTerminalOption, const void*) = ghostty_terminal_set;
static GhosttyResult (*const f_get)(GhosttyTerminal, GhosttyTerminalData, void*) = ghostty_terminal_get;
static void (*const f_write)(GhosttyTerminal, const uint8_t*, size_t) = ghostty_terminal_vt_write;
static GhosttyResult (*const f_resize)(GhosttyTerminal, uint16_t, uint16_t, uint32_t, uint32_t) = ghostty_terminal_resize;
static GhosttyResult (*const f_scroll)(GhosttyTerminal, GhosttyTerminalScrollViewport) = ghostty_terminal_scroll_viewport;
static GhosttyResult (*const f_ref)(GhosttyTerminal, GhosttyPoint, GhosttyGridRef*) = ghostty_terminal_grid_ref;
static GhosttyResult (*const f_track)(GhosttyTerminal, GhosttyPoint, GhosttyTrackedGridRef*) = ghostty_terminal_grid_ref_track;
static void (*const f_tfree)(GhosttyTrackedGridRef) = ghostty_tracked_grid_ref_free;
static bool (*const f_thas)(GhosttyTrackedGridRef) = ghostty_tracked_grid_ref_has_value;
static GhosttyResult (*const f_tpoint)(GhosttyTrackedGridRef, GhosttyPointTag, GhosttyPointCoordinate*) = ghostty_tracked_grid_ref_point;
static GhosttyResult (*const f_cell)(const GhosttyGridRef*, GhosttyCell*) = ghostty_grid_ref_cell;
static GhosttyResult (*const f_row)(const GhosttyGridRef*, GhosttyRow*) = ghostty_grid_ref_row;
static GhosttyResult (*const f_graphemes)(const GhosttyGridRef*, uint32_t*, size_t, size_t*) = ghostty_grid_ref_graphemes;
static GhosttyResult (*const f_style)(const GhosttyGridRef*, GhosttyStyle*) = ghostty_grid_ref_style;
static GhosttyResult (*const f_uri)(const GhosttyGridRef*, uint8_t*, size_t, size_t*) = ghostty_grid_ref_hyperlink_uri;
static GhosttyResult (*const f_cell_get)(GhosttyCell, GhosttyCellData, void*) = ghostty_cell_get;
static GhosttyResult (*const f_row_get)(GhosttyRow, GhosttyRowData, void*) = ghostty_row_get;
/* The callback typedefs, restated with their parameters: a typedef whose parameters change no longer converts to
 * these declared types, which fails to compile (incompatible function pointer types). The engine registers its
 * callbacks through the same typedefs on the Rust side. */
static void (*const f_bell)(GhosttyTerminal, void*) = (GhosttyTerminalBellFn)0;
static void (*const f_clip)(GhosttyTerminal, void*, const GhosttyClipboardWrite*) = (GhosttyTerminalClipboardWriteFn)0;
static void (*const f_reply_type)(const GhosttyClipboardWrite*, const GhosttyClipboardWriteReply*) = (GhosttyClipboardWriteReplyFn)0;
