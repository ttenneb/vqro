//! Encode key events as the bytes a terminal program expects, and name them for bindings.
//!
//! Key names (tmux-like, documented in `docs/spec/core-protocol.md`): modifiers `C-` (Ctrl), `M-`
//! (Alt) and `S-` (Shift, named keys only) in that order, then a character (`n`, `%`, `"`) or a named
//! key: `Up Down Left Right Home End PageUp PageDown Insert Delete Tab BackTab Enter Esc Backspace
//! Space F1`..`F12`. Names parse case-insensitively and compare in canonical form, so `C-space`
//! and `C-Space` are the same key.

use std::sync::atomic::{AtomicU8, Ordering};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, KeyboardEnhancementFlags, ModifierKeyCode};

/// The prefix when the setting is missing or unusable: never "no prefix" (recovery).
pub const DEFAULT_PREFIX: &str = "C-g";

const NAMED: [&str; 14] = ["Up", "Down", "Left", "Right", "Home", "End", "PageUp", "PageDown", "Insert", "Delete", "Tab", "BackTab", "Enter", "Esc"];

/// Which extended key reporting the program in a pane asked for, as seen in its output (`KeyModes`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Extended {
    /// Legacy xterm encoding only.
    #[default]
    Legacy,
    /// xterm `modifyOtherKeys` (`CSI > 4 ; n m`, n >= 1): otherwise ambiguous keys as `CSI 27 ; mod ; code ~`.
    ModifyOtherKeys,
    /// Kitty keyboard protocol with "disambiguate escape codes" (flag 1): those keys as `CSI code ; mod u`.
    Kitty,
}

/// Encode a key the way an xterm-compatible terminal does (`docs/spec/core-protocol.md`, Keys):
/// - cursor keys, Home and End: `CSI A`..`CSI F`, or `SS3` in application cursor mode (DECCKM); with
///   modifiers `CSI 1 ; m X` (DECCKM doesn't apply then), where m = 1 + Shift + 2·Alt + 4·Ctrl;
/// - Insert, Delete, PageUp, PageDown, F5-F12: `CSI n ~`, with modifiers `CSI n ; m ~`; F1-F4: `SS3 P`..`S`,
///   with modifiers `CSI 1 ; m P`..`S`;
/// - characters: Ctrl combinations as control bytes, Alt as an ESC prefix;
/// - Enter, Tab, Backspace, Esc and Ctrl combinations without a control byte: legacy bytes (Shift-Enter is
///   Enter), unless the program asked for extended keys (`ext`): then `CSI 27 ; m ; code ~` for
///   modifyOtherKeys, or `CSI code ; m u` for the kitty protocol (which also covers Alt and plain Esc).
pub fn encode(key: &KeyEvent, app_cursor: bool, ext: Extended) -> Option<Vec<u8>> {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let m = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    let extended = |code: u32, m: u8| -> Option<Vec<u8>> {
        match ext {
            Extended::Kitty if m == 1 => Some(format!("\x1b[{code}u").into_bytes()),
            Extended::Kitty => Some(format!("\x1b[{code};{m}u").into_bytes()),
            Extended::ModifyOtherKeys => Some(format!("\x1b[27;{m};{code}~").into_bytes()),
            Extended::Legacy => None,
        }
    };
    let letter = |c: u8| if m > 1 { format!("\x1b[1;{m}{}", c as char).into_bytes() } else { cursor(c, app_cursor) };
    let tilde = |n: u8| if m > 1 { format!("\x1b[{n};{m}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() };
    let with_alt = |mut v: Vec<u8>| {
        if alt {
            v.insert(0, 0x1b);
        }
        v
    };
    Some(match key.code {
        KeyCode::Up => letter(b'A'),
        KeyCode::Down => letter(b'B'),
        KeyCode::Right => letter(b'C'),
        KeyCode::Left => letter(b'D'),
        KeyCode::Home => letter(b'H'),
        KeyCode::End => letter(b'F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n @ 1..=4) => {
            let c = b"PQRS"[n as usize - 1];
            if m > 1 { format!("\x1b[1;{m}{}", c as char).into_bytes() } else { vec![0x1b, b'O', c] }
        }
        KeyCode::F(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][n as usize - 5]),
        KeyCode::F(_) => return None,
        KeyCode::BackTab => with_alt(b"\x1b[Z".to_vec()),
        KeyCode::Enter | KeyCode::Tab | KeyCode::Backspace | KeyCode::Esc => {
            let code = match key.code { KeyCode::Enter => 13, KeyCode::Tab => 9, KeyCode::Backspace => 127, _ => 27 };
            // Shift or Ctrl can't be told apart in legacy bytes; Alt can (ESC prefix), except under kitty.
            let kitty_alt = alt && ext == Extended::Kitty;
            if shift || ctrl || kitty_alt || (code == 27 && ext == Extended::Kitty) {
                if let Some(bytes) = extended(code, m) {
                    return Some(bytes);
                }
            }
            match key.code {
                KeyCode::Backspace if ctrl => with_alt(vec![0x08]),
                _ => with_alt(vec![code as u8]),
            }
        }
        KeyCode::Char(c) => {
            let base = c.to_ascii_lowercase();
            // Ctrl/Alt combinations under kitty, and Ctrl+Shift+letter under either extended mode.
            let ambiguous = (ext == Extended::Kitty && (ctrl || alt)) || (ctrl && shift && c.is_ascii_alphabetic());
            if ambiguous {
                if let Some(bytes) = extended(base as u32, m) {
                    return Some(bytes);
                }
            }
            if ctrl {
                let b = match base {
                    c @ 'a'..='z' => c as u8 - b'a' + 1,
                    ' ' | '@' | '2' => 0,
                    '[' | '3' => 27,
                    '\\' | '4' => 28,
                    ']' | '5' => 29,
                    '^' | '6' => 30,
                    '_' | '7' | '/' => 31,
                    '8' | '?' => 127,
                    // No control byte (Ctrl-1, Ctrl-;): only an extended encoding carries it.
                    _ => return extended(base as u32, m),
                };
                with_alt(vec![b])
            } else {
                with_alt(c.to_string().into_bytes())
            }
        }
        _ => return None,
    })
}

/// What the program in a pane asked for, tracked from its output stream (the VT parser doesn't):
/// the kitty keyboard protocol's flag stack (`CSI > f u` push, `CSI < n u` pop, `CSI = f ; mode u` set)
/// and xterm's modifyOtherKeys level (`CSI > 4 ; n m`). RIS (`ESC c`) resets both. Sequences split across
/// output chunks are completed from a short carry-over.
#[derive(Clone, Debug, Default)]
pub struct KeyModes {
    kitty: Vec<u32>,
    other: u32,
    tail: Vec<u8>,
}

impl KeyModes {
    pub fn extended(&self) -> Extended {
        if self.kitty.last().is_some_and(|f| f & 1 != 0) {
            Extended::Kitty
        } else if self.other > 0 {
            Extended::ModifyOtherKeys
        } else {
            Extended::Legacy
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        let mut buf = std::mem::take(&mut self.tail);
        buf.extend_from_slice(bytes);
        let mut i = 0;
        while i < buf.len() {
            if buf[i] != 0x1b {
                i += 1;
                continue;
            }
            match buf.get(i + 1) {
                None => {
                    self.tail = buf[i..].to_vec();
                    return;
                }
                Some(b'c') => {
                    self.kitty.clear();
                    self.other = 0;
                    i += 2;
                    continue;
                }
                Some(b'[') => {}
                Some(_) => {
                    i += 1;
                    continue;
                }
            }
            // CSI [<=>?] params final
            let mut j = i + 2;
            let private = buf.get(j).copied().filter(|b| matches!(b, b'<' | b'=' | b'>' | b'?'));
            if private.is_some() {
                j += 1;
            }
            let start = j;
            while j < buf.len() && (buf[j].is_ascii_digit() || buf[j] == b';') {
                j += 1;
            }
            if j >= buf.len() {
                if buf.len() - i <= 32 {
                    self.tail = buf[i..].to_vec();
                    return;
                }
                break;
            }
            let params: Vec<u32> = std::str::from_utf8(&buf[start..j]).unwrap_or("").split(';').map(|p| p.parse().unwrap_or(0)).collect();
            let first = params.first().copied().unwrap_or(0);
            match (private, buf[j]) {
                (Some(b'>'), b'u') => {
                    if self.kitty.len() >= 16 {
                        self.kitty.remove(0);
                    }
                    self.kitty.push(first);
                }
                (Some(b'<'), b'u') => {
                    let n = if start == j { 1 } else { first.max(1) } as usize;
                    let keep = self.kitty.len().saturating_sub(n);
                    self.kitty.truncate(keep);
                }
                (Some(b'='), b'u') => {
                    let mode = params.get(1).copied().unwrap_or(1);
                    let top = self.kitty.last().copied().unwrap_or(0);
                    let next = match mode { 2 => top | first, 3 => top & !first, _ => first };
                    match self.kitty.last_mut() {
                        Some(t) => *t = next,
                        None => self.kitty.push(next),
                    }
                }
                (Some(b'>'), b'm') if first == 4 => self.other = params.get(1).copied().unwrap_or(0),
                _ => {}
            }
            i = j + 1;
        }
    }
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

/// Display only: canonical key ids and whitespace-separated hints keep their input syntax.
/// Unknown label settings use full names; unknown/future key tokens stay untouched.
/// Shared parity cases: tests/fixtures/key_labels.json (also checked by Python prim_display).
pub fn display_labels(text: &str, setting: Option<&str>) -> String {
    let short = setting == Some("short");
    text.split_inclusive(char::is_whitespace).map(|part| {
        let token = part.trim_end_matches(char::is_whitespace);
        let whitespace = &part[token.len()..];
        let mut rest = token;
        let mut prefix = String::new();
        loop {
            let found = [("C-", "C-", "Ctrl-"), ("Ctrl-", "C-", "Ctrl-"),
                ("M-", "M-", "Alt-"), ("Alt-", "M-", "Alt-"),
                ("S-", "S-", "Shift-"), ("Shift-", "S-", "Shift-")]
                .into_iter().find(|(input, _, _)| rest.starts_with(input));
            let Some((input, compact, full)) = found else { break };
            prefix.push_str(if short { compact } else { full });
            rest = &rest[input.len()..];
        }
        let valid = rest.chars().count() == 1 || NAMED.contains(&rest) || matches!(rest, "Space" | "Backspace" | "PgUp/PgDn")
            || rest.strip_prefix('F').filter(|n|n.bytes().all(|b|b.is_ascii_digit())).and_then(|n| n.parse::<u8>().ok()).is_some_and(|n| (1..=12).contains(&n))
            || (rest.is_empty() && !prefix.is_empty()); // held modifiers after prefix
        let label = if !valid { token.to_string() } else {
            format!("{prefix}{}", match (short, rest) { (false, "BackTab") => "Shift-Tab", (false, "Esc") => "Escape", (false, "PgUp/PgDn") => "PageUp/PageDown", _ => rest })
        };
        format!("{label}{whitespace}")
    }).collect()
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

/// What the client asks of the outer terminal's keyboard reporting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enhance {
    Off = 0,
    /// The focused program asked for extended keys: "disambiguate" only, so Shift-Enter, Ctrl-Tab and
    /// the like reach us distinctly and can be passed on. Plain typing is unchanged.
    Disambiguate = 1,
    /// The prefix menu is open: everything, for live modifier feedback.
    Full = 2,
}

/// Which level we pushed; global so every exit path, panics included, can pop it.
static PUSHED: AtomicU8 = AtomicU8::new(0);

/// Push, change or pop the enhancement flags (idempotent). Only ever pops what we pushed, so a shell's
/// or outer program's own flags are left alone. Pop before leaving the alternate screen: the
/// protocol keeps a separate stack per screen.
pub fn enhance(level: Enhance) {
    let was = PUSHED.swap(level as u8, Ordering::SeqCst);
    if was == level as u8 {
        return;
    }
    let mut out = std::io::stdout();
    if was != 0 {
        let _ = crossterm::execute!(out, crossterm::event::PopKeyboardEnhancementFlags);
    }
    let flags = match level {
        Enhance::Off => return,
        Enhance::Disambiguate => KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
        Enhance::Full => ENHANCED,
    };
    let _ = crossterm::execute!(out, crossterm::event::PushKeyboardEnhancementFlags(flags));
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
    fn display_labels_match_the_shared_extension_table() {
        let cases: serde_json::Value = serde_json::from_str(include_str!("../../tests/fixtures/key_labels.json")).unwrap();
        for case in cases.as_array().unwrap() {
            let input=case["input"].as_str().unwrap();
            for mode in [None,Some("full"),Some("unknown"),Some("short")] {
                let expected=case[if mode==Some("short") {"short"} else {"full"}].as_str().unwrap();
                assert_eq!(display_labels(input,mode),expected,"{input:?} {mode:?}");
            }
        }
    }

    #[test]
    fn basics() {
        let k = |code, m| KeyEvent::new(code, m);
        let l = Extended::Legacy;
        assert_eq!(encode(&k(KeyCode::Char('c'), KeyModifiers::CONTROL), false, l), Some(vec![3]));
        assert_eq!(encode(&k(KeyCode::Char('x'), KeyModifiers::ALT), false, l), Some(vec![0x1b, b'x']));
        assert_eq!(encode(&k(KeyCode::Up, KeyModifiers::NONE), true, l), Some(b"\x1bOA".to_vec()));
        assert_eq!(encode(&k(KeyCode::Enter, KeyModifiers::NONE), false, l), Some(vec![b'\r']));
    }

    /// The xterm table: what each key and modifier combination sends.
    #[test]
    fn xterm_table() {
        let (n, s, a, c) = (KeyModifiers::NONE, KeyModifiers::SHIFT, KeyModifiers::ALT, KeyModifiers::CONTROL);
        let e = |code, m, app, ext| String::from_utf8(encode(&KeyEvent::new(code, m), app, ext).unwrap()).unwrap();
        let l = Extended::Legacy;
        let cases: &[(KeyCode, KeyModifiers, bool, &str)] = &[
            (KeyCode::Up, n, false, "\x1b[A"),
            (KeyCode::Up, n, true, "\x1bOA"),
            (KeyCode::Up, s, false, "\x1b[1;2A"),
            (KeyCode::Up, s, true, "\x1b[1;2A"), // DECCKM doesn't apply with modifiers
            (KeyCode::Left, c, false, "\x1b[1;5D"),
            (KeyCode::Right, a, false, "\x1b[1;3C"),
            (KeyCode::Down, c | s, false, "\x1b[1;6B"),
            (KeyCode::Right, c | a | s, false, "\x1b[1;8C"),
            (KeyCode::Home, n, true, "\x1bOH"),
            (KeyCode::Home, c, false, "\x1b[1;5H"),
            (KeyCode::End, s, false, "\x1b[1;2F"),
            (KeyCode::PageUp, n, false, "\x1b[5~"),
            (KeyCode::PageDown, c, false, "\x1b[6;5~"),
            (KeyCode::Delete, s, false, "\x1b[3;2~"),
            (KeyCode::Insert, a, false, "\x1b[2;3~"),
            (KeyCode::F(1), n, false, "\x1bOP"),
            (KeyCode::F(1), s, false, "\x1b[1;2P"),
            (KeyCode::F(4), c, false, "\x1b[1;5S"),
            (KeyCode::F(5), n, false, "\x1b[15~"),
            (KeyCode::F(5), c, false, "\x1b[15;5~"),
            (KeyCode::F(12), a | s, false, "\x1b[24;4~"),
            (KeyCode::BackTab, s, false, "\x1b[Z"),
            (KeyCode::Tab, n, false, "\t"),
            (KeyCode::Tab, a, false, "\x1b\t"),
            (KeyCode::Enter, s, false, "\r"), // legacy: Shift-Enter is Enter
            (KeyCode::Enter, a, false, "\x1b\r"),
            (KeyCode::Backspace, n, false, "\x7f"),
            (KeyCode::Backspace, c, false, "\x08"),
            (KeyCode::Backspace, a, false, "\x1b\x7f"),
            (KeyCode::Esc, n, false, "\x1b"),
            (KeyCode::Char('a'), c, false, "\x01"),
            (KeyCode::Char('A'), c | s, false, "\x01"),
            (KeyCode::Char('x'), c | a, false, "\x1b\x18"),
            (KeyCode::Char(' '), c, false, "\x00"),
            (KeyCode::Char('?'), c, false, "\x7f"),
            (KeyCode::Char('A'), s, false, "A"),
            (KeyCode::Char('é'), a, false, "\x1bé"),
        ];
        for (code, m, app, want) in cases {
            assert_eq!(e(*code, *m, *app, l), *want, "{code:?} {m:?} app={app}");
        }
        // Ctrl combinations without a control byte send nothing in legacy mode (like xterm without modifyOtherKeys).
        assert_eq!(encode(&KeyEvent::new(KeyCode::Char('1'), c), false, l), None);
        assert_eq!(encode(&KeyEvent::new(KeyCode::F(13), n), false, l), None);
    }

    #[test]
    fn extended_keys_only_when_the_program_asked() {
        let (n, s, a, c) = (KeyModifiers::NONE, KeyModifiers::SHIFT, KeyModifiers::ALT, KeyModifiers::CONTROL);
        let e = |code, m, ext| String::from_utf8(encode(&KeyEvent::new(code, m), false, ext).unwrap()).unwrap();
        let (k, o) = (Extended::Kitty, Extended::ModifyOtherKeys);
        assert_eq!(e(KeyCode::Enter, s, k), "\x1b[13;2u");
        assert_eq!(e(KeyCode::Enter, c, k), "\x1b[13;5u");
        assert_eq!(e(KeyCode::Enter, s, o), "\x1b[27;2;13~");
        assert_eq!(e(KeyCode::Tab, c, o), "\x1b[27;5;9~");
        assert_eq!(e(KeyCode::Char('1'), c, o), "\x1b[27;5;49~");
        assert_eq!(e(KeyCode::Char('A'), c | s, k), "\x1b[97;6u");
        assert_eq!(e(KeyCode::Char('x'), a, k), "\x1b[120;3u");
        assert_eq!(e(KeyCode::Esc, n, k), "\x1b[27u");
        assert_eq!(e(KeyCode::Esc, n, o), "\x1b");
        // Unambiguous keys stay legacy even when extended keys are on.
        assert_eq!(e(KeyCode::Enter, n, k), "\r");
        assert_eq!(e(KeyCode::Char('a'), n, k), "a");
        assert_eq!(e(KeyCode::Char('A'), s, k), "A");
        assert_eq!(e(KeyCode::Up, c, k), "\x1b[1;5A");
        assert_eq!(e(KeyCode::Char('x'), a, o), "\x1bx");
    }

    #[test]
    fn key_modes_follow_the_programs_output() {
        let mut m = KeyModes::default();
        assert_eq!(m.extended(), Extended::Legacy);
        m.feed(b"hello\x1b[>1u");
        assert_eq!(m.extended(), Extended::Kitty);
        m.feed(b"\x1b[<u");
        assert_eq!(m.extended(), Extended::Legacy);
        // Split across chunks.
        m.feed(b"text\x1b[>4");
        assert_eq!(m.extended(), Extended::Legacy);
        m.feed(b";2m more");
        assert_eq!(m.extended(), Extended::ModifyOtherKeys);
        m.feed(b"\x1b[>4m");
        assert_eq!(m.extended(), Extended::Legacy);
        // Flags without "disambiguate" (bit 1) are not extended keys.
        m.feed(b"\x1b[>2u");
        assert_eq!(m.extended(), Extended::Legacy);
        m.feed(b"\x1b[=1;2u");
        assert_eq!(m.extended(), Extended::Kitty);
        m.feed(b"\x1b[=1;3u");
        assert_eq!(m.extended(), Extended::Legacy);
        m.feed(b"\x1b[>1u\x1b[>4;1m\x1bc");
        assert_eq!(m.extended(), Extended::Legacy, "RIS resets both");
        // Other CSI sequences are ignored; an unterminated long run is not kept.
        m.feed(b"\x1b[31m\x1b[?2004h\x1b[?u");
        assert_eq!(m.extended(), Extended::Legacy);
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
