//! Tab bar labels (core-protocol.md "Tab bar"). The look is the tabs extension's choice, stored in the shared
//! setting `doc/settings/tabs`: `{numbers: "subscript"|"superscript"|"plain"|"none", chips: bool}`. Missing means
//! the default (subscript numbers, chips); an unknown `numbers` value reads as `plain`, a non-boolean `chips` as
//! the default. Colours stay with the client's palette: active chip `tab_active`/`tab_active_text` (bold),
//! inactive chip `surface` with `muted` text, the gap and the rest of the row `tab_bar`.
use serde_json::{json, Value};

pub const ENTRY: &str = "doc/settings/tabs";
const SUBSCRIPT: [char; 10] = ['₀', '₁', '₂', '₃', '₄', '₅', '₆', '₇', '₈', '₉'];
const SUPERSCRIPT: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Numbers { Subscript, Superscript, Plain, None }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style { pub numbers: Numbers, pub chips: bool }

impl Default for Style {
    fn default() -> Self { Style { numbers: Numbers::Subscript, chips: true } }
}

impl Style {
    /// From the setting's value (or None when absent).
    pub fn from_setting(value: Option<&Value>) -> Style {
        let mut style = Style::default();
        let Some(v) = value.filter(|v| v.is_object()) else { return style };
        style.numbers = match v.get("numbers").and_then(Value::as_str) {
            None => style.numbers,
            Some("subscript") => Numbers::Subscript,
            Some("superscript") => Numbers::Superscript,
            Some("none") => Numbers::None,
            Some(_) => Numbers::Plain,
        };
        if let Some(chips) = v.get("chips").and_then(Value::as_bool) { style.chips = chips; }
        style
    }

    /// The tab's number (1-based) in this style; empty for `none`.
    pub fn number(&self, n: usize) -> String {
        let digits = n.to_string();
        match self.numbers {
            Numbers::Subscript => digits.chars().map(|d| SUBSCRIPT[d.to_digit(10).unwrap() as usize]).collect(),
            Numbers::Superscript => digits.chars().map(|d| SUPERSCRIPT[d.to_digit(10).unwrap() as usize]).collect(),
            Numbers::Plain => digits,
            Numbers::None => String::new(),
        }
    }

    /// The label text for tab `index` (0-based), padding included: ` ₁ zsh `, ` ¹zsh `, ` 1 zsh `, ` zsh `, or the
    /// flat ` 1:zsh ` when chips are off and numbers are plain.
    pub fn label(&self, index: usize, title: &str) -> String {
        let number = self.number(index + 1);
        let joiner = match (self.numbers, self.chips) {
            (Numbers::None, _) => "",
            (Numbers::Superscript, _) => "",
            (Numbers::Plain, false) => ":",
            _ => " ",
        };
        format!(" {number}{joiner}{title} ")
    }

    /// Cells between two labels: one with chips, none for the flat bar.
    pub fn gap(&self) -> usize { if self.chips { 1 } else { 0 } }
}

/// Renderer-independent description of a tab bar (conformance): labels, the active index, the gap and chips.
pub fn describe(setting: Option<&Value>, titles: &[&str], active: usize) -> Value {
    let style = Style::from_setting(setting);
    json!({"labels": titles.iter().enumerate().map(|(i, t)| style.label(i, t)).collect::<Vec<_>>(),
           "active": active, "gap": style.gap(), "chips": style.chips})
}
