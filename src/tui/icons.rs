//! Icons: semantic names resolved through the active icon set, with the row's own glyphs as fallback.
//!
//! An icon set is data, `doc/settings/icons` = `{"name", "width"?, "glyphs": {name: glyph}}`, written by
//! an extension (D17, D20), exactly like a theme. The client only resolves and lays out. An icon value is
//! a plain string (drawn as is), or `{"name"?, "nerd"?, "text"?}` where `name` is one name or a list,
//! most specific first (`["working", "agent"]`). Resolution: the first name the active set has, then
//! the value's own `nerd` (when this client uses a Nerd Font) or `text`, then nothing. So vqro looks
//! the same as before with no set, and a set that lacks a name falls back the same way.
//!
//! Widths follow `unicode-width`, the rules ratatui lays text out with. A set's glyphs share one icon
//! column, `width` cells (1 or 2) or its widest glyph, so text after an icon lines up whether the glyph
//! is a 1-cell Nerd Font symbol or a 2-cell emoji.

use std::collections::HashMap;

use serde_json::Value;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Where the active icon set lives.
pub const ENTRY: &str = "doc/settings/icons";

/// The widest icon column a set may ask for: an emoji or a CJK-wide glyph.
const MAX_WIDTH: u16 = 2;

/// Display width of `s` in terminal cells, by the rules ratatui draws with.
pub fn width(s: &str) -> u16 {
    s.width().try_into().unwrap_or(u16::MAX)
}

/// Cut `s` to at most `w` cells, marking the cut with `…`.
pub fn truncate(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw + 1 > w {
            break;
        }
        out.push(c);
        used += cw;
    }
    if w > 0 {
        out.push('…');
    }
    out
}

/// The active icon set and how this client falls back, resolved once per frame.
#[derive(Debug, Default)]
pub struct Icons {
    glyphs: HashMap<String, String>,
    /// Cells of the icon column: the set's `width`, or its widest glyph; 1 with no set.
    column: u16,
    /// Use a value's `nerd` glyph (else its `text`) when the set has no glyph for it.
    nerd: bool,
}

impl Icons {
    /// Icons for a set entry's value (or none), with the client's Nerd Font choice for fallbacks.
    /// Glyphs that are empty, contain control characters, or are wider than two cells are dropped,
    /// so they fall back like a missing name instead of breaking the layout.
    pub fn new(set: Option<&Value>, nerd: bool) -> Icons {
        let glyphs: HashMap<String, String> = set
            .and_then(|s| s["glyphs"].as_object())
            .into_iter()
            .flatten()
            .filter_map(|(name, g)| Some((name.clone(), g.as_str()?.to_string())))
            .filter(|(_, g)| !g.contains(char::is_control) && (1..=MAX_WIDTH).contains(&width(g)))
            .collect();
        let declared = set.and_then(|s| s["width"].as_u64()).map(|w| w.min(MAX_WIDTH as u64) as u16).unwrap_or(1);
        let widest = glyphs.values().map(|g| width(g)).max().unwrap_or(1);
        Icons { column: declared.max(widest).max(1), glyphs, nerd }
    }

    /// The glyph to draw for an icon value, if any (see the module doc for the order).
    pub fn resolve<'a>(&'a self, icon: &'a Value) -> Option<&'a str> {
        if let Some(s) = icon.as_str() {
            return Some(s).filter(|s| !s.is_empty());
        }
        let names: Vec<&str> = match &icon["name"] {
            Value::String(n) => vec![n.as_str()],
            Value::Array(a) => a.iter().filter_map(|n| n.as_str()).collect(),
            _ => vec![],
        };
        if let Some(g) = names.iter().find_map(|n| self.glyphs.get(*n)) {
            return Some(g);
        }
        let own = if self.nerd { icon["nerd"].as_str().or_else(|| icon["text"].as_str()) } else { icon["text"].as_str() };
        own.filter(|s| !s.is_empty())
    }

    /// A glyph padded to the icon column, and the cells it takes. A wider fallback glyph than the
    /// column (a row's own two-character icon) keeps its width rather than being cut.
    pub fn cell(&self, glyph: &str) -> (String, u16) {
        let w = width(glyph);
        let cells = w.max(self.column);
        (format!("{glyph}{}", " ".repeat((cells - w) as usize)), cells)
    }

    /// An inline label with its icon: `icon text`, the icon alone for an empty text, or the text alone
    /// when nothing resolves. Inline labels (status bar, tab bar, chips) need no column.
    pub fn label(&self, icon: &Value, text: &str) -> String {
        match self.resolve(icon) {
            Some(g) if text.is_empty() => g.to_string(),
            Some(g) => format!("{g} {text}"),
            None => text.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn set(v: Value) -> Icons {
        Icons::new(Some(&v), false)
    }

    #[test]
    fn existing_forms_keep_working() {
        let text = Icons::new(None, false);
        let nerd = Icons::new(None, true);
        let both = json!({"nerd": "\u{e702}", "text": "⎇"});
        assert_eq!(text.resolve(&json!("◆")), Some("◆"));
        assert_eq!(nerd.resolve(&json!("◆")), Some("◆"));
        assert_eq!(text.resolve(&both), Some("⎇"));
        assert_eq!(nerd.resolve(&both), Some("\u{e702}"));
        // Nerd without a nerd glyph uses the text one.
        assert_eq!(nerd.resolve(&json!({"text": "›"})), Some("›"));
        assert_eq!(text.resolve(&json!({"nerd": "\u{e702}"})), None);
        assert_eq!(text.resolve(&json!("")), None);
        assert_eq!(text.resolve(&Value::Null), None);
        assert_eq!(text.column, 1);
    }

    #[test]
    fn the_set_wins_then_the_row_then_nothing() {
        let icons = set(json!({"name": "s", "glyphs": {"repo": "R", "agent": "A"}}));
        assert_eq!(icons.resolve(&json!({"name": "repo", "text": "⎇"})), Some("R"));
        // A name the set lacks falls back to the row's own glyph, then to nothing.
        assert_eq!(icons.resolve(&json!({"name": "folder", "text": "▫"})), Some("▫"));
        assert_eq!(icons.resolve(&json!({"name": "folder"})), None);
        // Unknown name shapes are ignored.
        assert_eq!(icons.resolve(&json!({"name": 7, "text": "x"})), Some("x"));
        // A plain string is literal: a set never remaps it.
        assert_eq!(icons.resolve(&json!("repo")), Some("repo"));
    }

    #[test]
    fn names_are_tried_most_specific_first() {
        let icons = set(json!({"glyphs": {"working": "W", "agent": "A"}}));
        assert_eq!(icons.resolve(&json!({"name": ["working", "agent"], "text": "◆"})), Some("W"));
        assert_eq!(icons.resolve(&json!({"name": ["done", "agent"], "text": "◆"})), Some("A"));
        assert_eq!(icons.resolve(&json!({"name": ["done"], "text": "◆"})), Some("◆"));
    }

    #[test]
    fn bad_glyphs_fall_back() {
        let icons = set(json!({"glyphs": {"a": "", "b": "\u{1b}[31m", "c": "abc", "d": 5, "e": "\u{301}", "ok": "✓"}}));
        for n in ["a", "b", "c", "d", "e"] {
            assert_eq!(icons.resolve(&json!({"name": n, "text": "t"})), Some("t"), "{n}");
        }
        assert_eq!(icons.resolve(&json!({"name": "ok"})), Some("✓"));
        assert_eq!(Icons::new(Some(&json!("nonsense")), false).resolve(&json!({"name": "x", "text": "t"})), Some("t"));
    }

    #[test]
    fn widths_and_the_icon_column() {
        // unicode-width, as ratatui uses: emoji presentation is 2 cells; Nerd Font glyphs (private use) 1.
        assert_eq!(width("🤖"), 2);
        assert_eq!(width("\u{f06a9}"), 1);
        assert_eq!(width("⎇"), 1);
        // The column is the declared width, or the widest glyph if that is wider; at most 2.
        assert_eq!(set(json!({"width": 2, "glyphs": {"a": "x"}})).column, 2);
        assert_eq!(set(json!({"glyphs": {"a": "x", "b": "🤖"}})).column, 2);
        assert_eq!(set(json!({"width": 9, "glyphs": {}})).column, 2);
        assert_eq!(set(json!({"width": "wide", "glyphs": {}})).column, 1);
        // Narrow glyphs pad to the column, so the text after them lines up.
        let wide = set(json!({"width": 2, "glyphs": {"agent": "🤖"}}));
        assert_eq!(wide.cell("🤖"), ("🤖".to_string(), 2));
        assert_eq!(wide.cell("+"), ("+ ".to_string(), 2));
        // A fallback wider than the column keeps its width.
        assert_eq!(Icons::new(None, false).cell("ab"), ("ab".to_string(), 2));
    }

    #[test]
    fn inline_labels() {
        let icons = set(json!({"glyphs": {"settings": "🔧"}}));
        assert_eq!(icons.label(&json!({"name": "settings", "text": "⚙"}), "settings"), "🔧 settings");
        assert_eq!(icons.label(&json!({"name": "add", "text": "+"}), ""), "+");
        assert_eq!(icons.label(&Value::Null, "plain"), "plain");
    }

    #[test]
    fn truncation_counts_cells() {
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abcd", 3), "ab…");
        assert_eq!(truncate("🤖🤖🤖", 4), "🤖…");
        assert_eq!(truncate("abc", 0), "");
    }
}
