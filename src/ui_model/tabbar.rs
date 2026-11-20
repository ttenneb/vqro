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
/// (the command of the one terminal it shows, as `tabs.open` named it)
/// shows that program's own title (`resource.title`, from OSC 0/2) instead, so shell tabs don't all read `zsh`.
/// Renamed tabs and named resources keep their names. `resource` is the resource of the tab's only view, if any.
/// Derived on every draw, never written back (titles change too often for transactions).
pub fn tab_title(setting: Option<&Value>, title: &str, resource: Option<&Value>) -> String {
    let on = setting.and_then(|s| s.get("program_titles")).and_then(Value::as_bool).unwrap_or(true);
    let Some(r) = resource.filter(|_| on) else { return title.to_string() };
    let program = r["title"].as_str().map(str::trim).filter(|t| !t.is_empty());
    let named = r["name"].as_str().is_some_and(|n| !n.is_empty());
    let command = r["argv"][0].as_str().map(|a| a.rsplit('/').next().unwrap_or(a));
    match (program, named, command) {
        (Some(program), false, Some(command)) if title == command => {
            if program.chars().count() > PROGRAM_TITLE_MAX {
                program.chars().take(PROGRAM_TITLE_MAX - 1).chain(['…']).collect()
            } else {
                program.to_string()
            }
        }
        _ => title.to_string(),
    }
}

/// A tab's lifecycle hint (core-protocol.md "Tab bar"): the tabs extension sets `state` to `ended` (its programs all
/// ended) or `gone` (one was removed); clients draw such a tab dim, title unchanged. Unknown values: no hint.
pub fn tab_state(tab: &Value) -> Option<&'static str> {
    match tab["state"].as_str() {
        Some("ended") => Some("ended"),
        Some("gone") => Some("gone"),
        _ => None,
    }
}

/// Marks on inactive tabs, most urgent first (the setting's `marks`, default off).
pub const MARKS: [&str; 4] = ["blocked", "error", "done", "activity"];

/// What a client remembers about a resource when its tab is shown: `{title}` (the program title), for `activity`.
pub fn seen_of(resource: &Value) -> Value {
    json!({"title": resource["title"]})
}

/// The finish a resource update reaches: `done` or `error` when its status state turns into it (from `old`, the
/// record before the update, or none). Clients latch it from every resource event until the resource's tab is shown,
/// because a backend may report `done` only for a moment (herdr-core clears the status when the reporting command
/// returns to the shell), so a client that looked only when drawing could miss it.
pub fn reached(old: Option<&Value>, new: &Value) -> Option<&'static str> {
    let was = old.and_then(|o| o["status"]["state"].as_str());
    match new["status"]["state"].as_str() {
        Some("done") if was != Some("done") => Some("done"),
        Some("error") if was != Some("error") => Some("error"),
        _ => None,
    }
}

/// The more urgent of two latched finishes (`error` over `done`).
pub fn latch(held: Option<&'static str>, new: &'static str) -> &'static str {
    if held == Some("error") { "error" } else { new }
}

/// The mark an inactive tab shows (with the setting's `marks` on), from its resources now, what they were when the
/// tab was last shown (`seen`, one `seen_of` per resource, null when never seen) and the finish each reached since
/// (`reached`, latched by `reached`/`latch`, null for none): `blocked` while any agent there waits for you; `error`
/// or `done` when one reached it since; `activity` when a program title changed since. The most urgent wins
/// (`MARKS`). Derived on draw, never written (core-protocol.md "Tab bar").
pub fn tab_mark(setting: Option<&Value>, now: &[&Value], seen: &[&Value], reached: &[Option<&str>]) -> Option<&'static str> {
    if setting.and_then(|s| s.get("marks")).and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let mark = |i: usize, r: &Value| -> Option<&'static str> {
        if r["status"]["state"].as_str() == Some("blocked") {
            return Some("blocked");
        }
        match reached.get(i).copied().flatten() {
            Some("error") => return Some("error"),
            Some("done") => return Some("done"),
            _ => {}
        }
        let was = seen.get(i).copied().unwrap_or(&Value::Null);
        (!was.is_null() && r["title"].is_string() && r["title"] != was["title"]).then_some("activity")
    };
    now.iter().enumerate().filter_map(|(i, r)| mark(i, r)).min_by_key(|m| MARKS.iter().position(|x| x == m))
}
