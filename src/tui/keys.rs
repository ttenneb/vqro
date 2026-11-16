//! Encode key events as the bytes a terminal program expects, and name them for bindings.
//!
//! Key names (tmux-like, documented in `docs/spec/core-protocol.md`): modifiers `C-` (Ctrl), `M-`
//! (Alt) and `S-` (Shift, named keys only) in that order, then a character (`n`, `%`, `"`) or a named
//! key: `Up Down Left Right Home End PageUp PageDown Insert Delete Tab BackTab Enter Esc Backspace
//! Space F1`..`F12`. Names parse case-insensitively and compare in canonical form, so `C-space`
//! and `C-Space` are the same key.

use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, KeyboardEnhancementFlags, ModifierKeyCode};

/// The prefix when the setting is missing or unusable: never "no prefix" (recovery).
pub const DEFAULT_PREFIX: &str = "C-g";

const NAMED: [&str; 14] = ["Up", "Down", "Left", "Right", "Home", "End", "PageUp", "PageDown", "Insert", "Delete", "Tab", "BackTab", "Enter", "Esc"];

pub fn encode(key: &KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let mut out = match key.code {
        KeyCode::Char(c) if ctrl => {
            let b = match c.to_ascii_lowercase() {
                c @ 'a'..='z' => c as u8 - b'a' + 1,
                ' ' | '@' | '2' => 0,
                '[' | '3' => 27,
                '\\' | '4' => 28,
                ']' | '5' => 29,
                '^' | '6' => 30,
                '_' | '7' | '/' => 31,
                '8' => 127,
                _ => return None,
            };
            vec![b]
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => cursor(b'A', app_cursor),
        KeyCode::Down => cursor(b'B', app_cursor),
        KeyCode::Right => cursor(b'C', app_cursor),
        KeyCode::Left => cursor(b'D', app_cursor),
        KeyCode::Home => cursor(b'H', app_cursor),
        KeyCode::End => cursor(b'F', app_cursor),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::F(n) => match n {
            1 => b"\x1bOP".to_vec(),
            2 => b"\x1bOQ".to_vec(),
            3 => b"\x1bOR".to_vec(),
            4 => b"\x1bOS".to_vec(),
            5 => b"\x1b[15~".to_vec(),
            6 => b"\x1b[17~".to_vec(),
            7 => b"\x1b[18~".to_vec(),
            8 => b"\x1b[19~".to_vec(),
            9 => b"\x1b[20~".to_vec(),
            10 => b"\x1b[21~".to_vec(),
            11 => b"\x1b[23~".to_vec(),
            12 => b"\x1b[24~".to_vec(),
            _ => return None,
        },
        _ => return None,
    };
    if alt {
        out.insert(0, 0x1b);
    }
    Some(out)
}

/// The canonical name of a key event, or None for keys bindings can't name.
pub fn name(key: &KeyEvent) -> Option<String> {
    // Super/Hyper/Meta (only reported with keyboard enhancement) have no name; never misread them.
    if key.modifiers.intersects(KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META) {
        return None;
    }
    let base = match key.code {
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => c.to_ascii_lowercase().to_string(),
        // A terminal that reports Shift without the shifted key: letters are still unambiguous.
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::SHIFT) => c.to_ascii_uppercase().to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Up => "Up".into(),
        KeyCode::Down => "Down".into(),
        KeyCode::Left => "Left".into(),
        KeyCode::Right => "Right".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::PageUp => "PageUp".into(),
        KeyCode::PageDown => "PageDown".into(),
        KeyCode::Insert => "Insert".into(),
        KeyCode::Delete => "Delete".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::BackTab => "BackTab".into(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Backspace => "Backspace".into(),
        KeyCode::F(n @ 1..=12) => format!("F{n}"),
        _ => return None,
    };
    // Shift is already in a character (`O`, `%`) and implied by BackTab; only other named keys carry it.
    let shift = key.modifiers.contains(KeyModifiers::SHIFT) && !matches!(key.code, KeyCode::Char(_) | KeyCode::BackTab);
    Some(compose(key.modifiers.contains(KeyModifiers::CONTROL), key.modifiers.contains(KeyModifiers::ALT), shift, &base))
}

/// The canonical form of a key name, or None if it isn't one (so unknown keys never match).
pub fn normalize(s: &str) -> Option<String> {
    // Long modifier words are accepted too: `Ctrl-a`, `Alt-x`, `Shift-Tab`.
    let mut long = s.to_string();
    for (word, short) in [("ctrl-", "C-"), ("control-", "C-"), ("alt-", "M-"), ("meta-", "M-"), ("option-", "M-"), ("shift-", "S-")] {
        loop {
            let lower = long.to_ascii_lowercase();
            // Only at the front or after another modifier, and only if something follows.
            let at = lower.find(word).filter(|&i| (i == 0 || lower[..i].ends_with('-')) && lower.len() > i + word.len());
            match at {
                Some(i) => long.replace_range(i..i + word.len(), short),
                None => break,
            }
        }
    }
    let s = long.as_str();
    let (mut ctrl, mut alt, mut shift, mut rest) = (false, false, false, s);
    // A modifier needs something after it, so `C--` is Ctrl and `-`, and `C` alone is the letter.
    while rest.len() > 2 && rest.is_char_boundary(2) && rest.as_bytes()[1] == b'-' {
        match rest.as_bytes()[0].to_ascii_uppercase() {
            b'C' => ctrl = true,
            b'M' => alt = true,
            b'S' => shift = true,
            _ => break,
        }
        rest = &rest[2..];
    }
    let mut chars = rest.chars();
    let base = match (chars.next(), chars.next()) {
        (Some(' '), None) => "Space".to_string(),
        // Shift is part of a character already (`S-a` is `A`), so it only names named keys.
        (Some(_), None) if shift => return None,
        (Some(c), None) if ctrl => c.to_ascii_lowercase().to_string(),
        (Some(c), None) => c.to_string(),
        _ => {
            let lower = rest.to_ascii_lowercase();
            let f = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()).filter(|n| (1..=12).contains(n));
            match f {
                Some(n) => format!("F{n}"),
                None => NAMED.iter().chain(&["Backspace", "Space"]).find(|n| n.to_ascii_lowercase() == lower)?.to_string(),
            }
        }
    };
    Some(compose(ctrl, alt, shift, &base))
}

/// The prefix key from the client setting, falling back to Ctrl-g if it is missing, unparsable, or
/// a key you'd type (a prefix needs Ctrl or Alt, or is a function key).
pub fn prefix(setting: Option<&str>) -> String {
    setting
        .and_then(normalize)
        .filter(|k| k.starts_with("C-") || k.starts_with("M-") || (k.starts_with('F') && k.len() > 1))
        .unwrap_or_else(|| DEFAULT_PREFIX.to_string())
}

/// Held modifiers as words (`Ctrl-`, `Alt-`, `Shift-`, `Super-`), for live feedback after the prefix:
/// easier to read at a glance than the compact binding notation.
pub fn held_words(m: KeyModifiers) -> String {
    let mut s = String::new();
    for (flag, word) in [(KeyModifiers::CONTROL, "Ctrl-"), (KeyModifiers::ALT, "Alt-"), (KeyModifiers::SHIFT, "Shift-"), (KeyModifiers::SUPER, "Super-")] {
        if m.contains(flag) {
            s.push_str(word);
        }
    }
    s
}

/// The modifier a modifier key press or release is about.
pub fn modifier_of(k: ModifierKeyCode) -> KeyModifiers {
    match k {
        ModifierKeyCode::LeftShift | ModifierKeyCode::RightShift => KeyModifiers::SHIFT,
        ModifierKeyCode::LeftControl | ModifierKeyCode::RightControl => KeyModifiers::CONTROL,
        ModifierKeyCode::LeftAlt | ModifierKeyCode::RightAlt => KeyModifiers::ALT,
        ModifierKeyCode::LeftSuper | ModifierKeyCode::RightSuper => KeyModifiers::SUPER,
        _ => KeyModifiers::NONE,
    }
}

/// Kitty keyboard protocol flags while the prefix is active: every key (bare modifiers too) as an
/// escape code with press/repeat/release, and the shifted key (`%`, not Shift+5) so bindings match.
const ENHANCED: KeyboardEnhancementFlags = KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
    .union(KeyboardEnhancementFlags::REPORT_EVENT_TYPES)
    .union(KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS);

/// Whether we pushed `ENHANCED`; global so every exit path, panics included, can pop it.
static PUSHED: AtomicBool = AtomicBool::new(false);

/// Push or pop the enhancement flags (idempotent). Only ever pops what we pushed, so a shell's or
/// outer program's own flags are left alone. Pop before leaving the alternate screen: the
/// protocol keeps a separate stack per screen.
pub fn enhance(on: bool) {
    if PUSHED.swap(on, Ordering::SeqCst) == on {
        return;
    }
    let mut out = std::io::stdout();
    let _ = if on {
        crossterm::execute!(out, crossterm::event::PushKeyboardEnhancementFlags(ENHANCED))
    } else {
        crossterm::execute!(out, crossterm::event::PopKeyboardEnhancementFlags)
    };
}

fn compose(ctrl: bool, alt: bool, shift: bool, base: &str) -> String {
    format!("{}{}{}{base}", if ctrl { "C-" } else { "" }, if alt { "M-" } else { "" }, if shift { "S-" } else { "" })
}

fn cursor(c: u8, app: bool) -> Vec<u8> {
    if app {
        vec![0x1b, b'O', c]
    } else {
        vec![0x1b, b'[', c]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basics() {
        let k = |code, m| KeyEvent::new(code, m);
        assert_eq!(encode(&k(KeyCode::Char('c'), KeyModifiers::CONTROL), false), Some(vec![3]));
        assert_eq!(encode(&k(KeyCode::Char('x'), KeyModifiers::ALT), false), Some(vec![0x1b, b'x']));
        assert_eq!(encode(&k(KeyCode::Up, KeyModifiers::NONE), true), Some(b"\x1bOA".to_vec()));
        assert_eq!(encode(&k(KeyCode::Enter, KeyModifiers::NONE), false), Some(vec![b'\r']));
    }

    #[test]
    fn names() {
        let k = |code, m| name(&KeyEvent::new(code, m)).unwrap();
        assert_eq!(k(KeyCode::Char('g'), KeyModifiers::CONTROL), "C-g");
        assert_eq!(k(KeyCode::Char(' '), KeyModifiers::CONTROL), "C-Space");
        assert_eq!(k(KeyCode::Char('O'), KeyModifiers::SHIFT), "O");
        assert_eq!(k(KeyCode::Char('%'), KeyModifiers::SHIFT), "%");
        assert_eq!(k(KeyCode::Left, KeyModifiers::NONE), "Left");
        assert_eq!(k(KeyCode::Left, KeyModifiers::SHIFT), "S-Left");
        assert_eq!(k(KeyCode::BackTab, KeyModifiers::SHIFT), "BackTab");
        assert_eq!(k(KeyCode::F(5), KeyModifiers::ALT), "M-F5");
        assert_eq!(k(KeyCode::Char('o'), KeyModifiers::SHIFT), "O");
        assert_eq!(name(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::SUPER)), None);
    }

    #[test]
    fn normalizing() {
        let n = |s| normalize(s);
        assert_eq!(n("C-space").as_deref(), Some("C-Space"));
        assert_eq!(n("c-B").as_deref(), Some("C-b"));
        assert_eq!(n("left").as_deref(), Some("Left"));
        assert_eq!(n("\"").as_deref(), Some("\""));
        assert_eq!(n(" ").as_deref(), Some("Space"));
        assert_eq!(n("C--").as_deref(), Some("C--"));
        assert_eq!(n("C").as_deref(), Some("C"));
        assert_eq!(n("f12").as_deref(), Some("F12"));
        assert_eq!(n("M-C-a").as_deref(), Some("C-M-a"));
        assert_eq!(n("S-a"), None);
        assert_eq!(n("F13"), None);
        assert_eq!(n("mouse.right"), None);
        assert_eq!(n(""), None);
    }

    #[test]
    fn prefix_falls_back_to_ctrl_g() {
        assert_eq!(prefix(Some("C-a")), "C-a");
        assert_eq!(prefix(Some("C-space")), "C-Space");
        assert_eq!(prefix(Some("F12")), "F12");
        for bad in [None, Some(""), Some("a"), Some("Left"), Some("nonsense"), Some("C-")] {
            assert_eq!(prefix(bad), DEFAULT_PREFIX, "{bad:?}");
        }
    }

    #[test]
    fn long_modifier_words() {
        assert_eq!(normalize("Ctrl-a").as_deref(), Some("C-a"));
        assert_eq!(normalize("ctrl-alt-Left").as_deref(), Some("C-M-Left"));
        assert_eq!(normalize("Shift-Tab").as_deref(), Some("S-Tab"));
        assert_eq!(held_words(KeyModifiers::SHIFT | KeyModifiers::CONTROL), "Ctrl-Shift-");
    }
}
