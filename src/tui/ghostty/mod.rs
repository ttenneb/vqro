//! libghostty-vt (Ghostty's terminal emulation library, MIT) for the TUI's `libghostty` terminal engine.
//!
//! The FFI declarations (`bindings`) and, from slice G2, the safe wrapper start from herdr's `src/ghostty/`
//! (Apache-2.0) as an unvetted baseline: tested here, audited before relying on it (libghostty plan G8).
#![allow(dead_code)]

#[allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals, clippy::all, rustdoc::all, unsafe_op_in_unsafe_fn)]
pub mod bindings;

#[cfg(test)]
mod tests {
    use super::bindings as ffi;

    /// The vendored library builds, links and runs: a terminal is created, fed bytes and freed.
    #[test]
    fn the_vendored_library_links_and_runs() {
        let mut raw: ffi::GhosttyTerminal = std::ptr::null_mut();
        let options = ffi::GhosttyTerminalOptions { cols: 20, rows: 4, max_scrollback: 100 };
        // SAFETY: a valid out pointer and options; a null allocator means libghostty's default allocator.
        let result = unsafe { ffi::ghostty_terminal_new(std::ptr::null(), &mut raw, options) };
        assert_eq!(result, ffi::GhosttyResult_GHOSTTY_SUCCESS);
        assert!(!raw.is_null());
        let bytes = b"hello \xf0\x9f\x91\xa9\xe2\x80\x8d\xf0\x9f\x92\xbb";
        // SAFETY: `raw` is a live terminal and the pointer/length describe `bytes`.
        unsafe { ffi::ghostty_terminal_vt_write(raw, bytes.as_ptr(), bytes.len()) };
        // SAFETY: `raw` came from ghostty_terminal_new and is freed once.
        unsafe { ffi::ghostty_terminal_free(raw) };
    }
}
