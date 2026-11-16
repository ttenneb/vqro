//! Encode mouse events for terminal programs that asked for them (xterm mouse protocols).

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use vt100::{MouseProtocolEncoding, MouseProtocolMode};

/// Bytes to send to the program for this event at cell (col, row) inside its view (0-based),
/// or None if the program's mouse mode doesn't want it.
pub fn encode(ev: &MouseEvent, col: u16, row: u16, mode: MouseProtocolMode, enc: MouseProtocolEncoding) -> Option<Vec<u8>> {
    use MouseEventKind::*;
    let wants_release = !matches!(mode, MouseProtocolMode::Press);
    let wants_drag = matches!(mode, MouseProtocolMode::ButtonMotion | MouseProtocolMode::AnyMotion);
    let wants_move = matches!(mode, MouseProtocolMode::AnyMotion);
    let button = |b: MouseButton| match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let (mut code, release) = match ev.kind {
        _ if mode == MouseProtocolMode::None => return None,
        Down(b) => (button(b), false),
        Up(b) if wants_release => (button(b), true),
        Drag(b) if wants_drag => (button(b) + 32, false),
        Moved if wants_move => (3 + 32, false),
        ScrollUp => (64, false),
        ScrollDown => (65, false),
        ScrollLeft => (66, false),
        ScrollRight => (67, false),
        _ => return None,
    };
    if ev.modifiers.contains(KeyModifiers::SHIFT) {
        code += 4;
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        code += 8;
    }
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        code += 16;
    }
    let (x, y) = (col as u32 + 1, row as u32 + 1);
    match enc {
        MouseProtocolEncoding::Sgr => Some(format!("\x1b[<{};{};{}{}", code, x, y, if release { 'm' } else { 'M' }).into_bytes()),
        _ => {
            // Legacy encoding: release is button 3; coordinates offset by 32, capped.
            let code = if release { 3 + (code & !3) } else { code };
            let cap = |v: u32| (v + 32).min(255) as u8;
            Some(vec![0x1b, b'[', b'M', (code + 32).min(255) as u8, cap(x), cap(y)])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr_and_legacy() {
        let ev = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 0, row: 0, modifiers: KeyModifiers::NONE };
        assert_eq!(encode(&ev, 4, 2, MouseProtocolMode::PressRelease, MouseProtocolEncoding::Sgr), Some(b"\x1b[<0;5;3M".to_vec()));
        assert_eq!(encode(&ev, 4, 2, MouseProtocolMode::Press, MouseProtocolEncoding::Default), Some(vec![0x1b, b'[', b'M', 32, 37, 35]));
        assert_eq!(encode(&ev, 4, 2, MouseProtocolMode::None, MouseProtocolEncoding::Sgr), None);
        let up = MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), ..ev };
        assert_eq!(encode(&up, 0, 0, MouseProtocolMode::Press, MouseProtocolEncoding::Sgr), None);
    }
}
