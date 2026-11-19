//! Key names for bindings, renderer-independent (moved from `tui/keys.rs`, which keeps byte encoding).
//!
//! Key names (tmux-like, documented in `docs/spec/core-protocol.md`): modifiers `C-` (Ctrl), `M-`
//! (Alt) and `S-` (Shift, named keys only) in that order, then a character (`n`, `%`, `"`) or a named
//! key: `Up Down Left Right Home End PageUp PageDown Insert Delete Tab BackTab Enter Esc Backspace
//! Space F1`..`F12`. Names parse case-insensitively and compare in canonical form, so `C-space`
//! and `C-Space` are the same key.

/// The prefix when the setting is missing or unusable: never "no prefix" (recovery).
pub const DEFAULT_PREFIX: &str = "C-g";

pub const NAMED: [&str; 14] = ["Up", "Down", "Left", "Right", "Home", "End", "PageUp", "PageDown", "Insert", "Delete", "Tab", "BackTab", "Enter", "Esc"];

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
/// Shared parity cases: tests/fixtures/key_labels.json (also checked by Python vqro_display).
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
            || matches!(rest, "F1" | "F2" | "F3" | "F4" | "F5" | "F6" | "F7" | "F8" | "F9" | "F10" | "F11" | "F12")
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

pub fn compose(ctrl: bool, alt: bool, shift: bool, base: &str) -> String {
    format!("{}{}{}{base}", if ctrl { "C-" } else { "" }, if alt { "M-" } else { "" }, if shift { "S-" } else { "" })
}
