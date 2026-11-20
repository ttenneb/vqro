//! Tab bar labels (core-protocol.md "Tab bar"). The look is the tabs extension's choice, stored in the shared
//! setting `doc/settings/tabs`: `{numbers: "subscript"|"superscript"|"plain"|"none", position: "after"|"before",
//! chips: bool}`. Missing means the default (subscript numbers after the name, chips); an unknown `numbers` value
//! reads as `plain`, an unknown `position` as `after`, a non-boolean `chips` as the default. Colours stay with the client's palette: active chip `tab_active`/`tab_active_text` (bold),
//! inactive chip `surface` with `muted` text, the gap and the rest of the row `tab_bar`.
use serde_json::{json, Value};

pub const ENTRY: &str = "doc/settings/tabs";
const SUBSCRIPT: [char; 10] = ['₀', '₁', '₂', '₃', '₄', '₅', '₆', '₇', '₈', '₉'];
const SUPERSCRIPT: [char; 10] = ['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹'];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Numbers { Subscript, Superscript, Plain, None }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style { pub numbers: Numbers, pub before: bool, pub chips: bool }

impl Default for Style {
    fn default() -> Self { Style { numbers: Numbers::Subscript, before: false, chips: true } }
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
        style.before = v.get("position").and_then(Value::as_str) == Some("before");
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

    /// The label text for tab `index` (0-based), padding included. After the name (default): ` zsh₁ `, ` zsh¹ `,
    /// ` zsh 1 `. Before it: ` ₁ zsh `, ` ¹zsh `, ` 1 zsh `, or the flat ` 1:zsh ` when chips are off and numbers
    /// are plain. No number: ` zsh `.
    pub fn label(&self, index: usize, title: &str) -> String {
        let number = self.number(index + 1);
        if number.is_empty() { return format!(" {title} "); }
        if !self.before {
            let joiner = if self.numbers == Numbers::Plain { " " } else { "" };
            return format!(" {title}{joiner}{number} ");
        }
        let joiner = match (self.numbers, self.chips) {
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

/// Longest program title a tab shows, in characters; longer ones end in `…`.
pub const PROGRAM_TITLE_MAX: usize = 24;

/// The name a tab shows. With the setting's `program_titles` (default on), a tab that still has its automatic name
/// (the command of the one terminal it shows, as `tabs.open` named it, maybe with an ` [ended]` or ` [gone]` mark)
/// shows that program's own title (`resource.title`, from OSC 0/2) instead, so shell tabs don't all read `zsh`.
/// Renamed tabs and named resources keep their names. `resource` is the resource of the tab's only view, if any.
/// Derived on every draw, never written back (titles change too often for transactions).
pub fn tab_title(setting: Option<&Value>, title: &str, resource: Option<&Value>) -> String {
    let on = setting.and_then(|s| s.get("program_titles")).and_then(Value::as_bool).unwrap_or(true);
    let Some(r) = resource.filter(|_| on) else { return title.to_string() };
    let program = r["title"].as_str().map(str::trim).filter(|t| !t.is_empty());
    let named = r["name"].as_str().is_some_and(|n| !n.is_empty());
    let command = r["argv"][0].as_str().map(|a| a.rsplit('/').next().unwrap_or(a));
    let (base, mark) = [" [ended]", " [gone]"].iter().find_map(|m| title.strip_suffix(m).map(|b| (b, *m))).unwrap_or((title, ""));
    match (program, named, command) {
        (Some(program), false, Some(command)) if base == command => {
            let short: String = if program.chars().count() > PROGRAM_TITLE_MAX {
                program.chars().take(PROGRAM_TITLE_MAX - 1).chain(['…']).collect()
            } else {
                program.to_string()
            };
            format!("{short}{mark}")
        }
        _ => title.to_string(),
    }
}
