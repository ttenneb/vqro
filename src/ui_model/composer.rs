//! The composer (feature `composer`, D29; `research/rich-text-and-composer.md`) and multi-line prompts (feature
//! `multiline`): the editing every client implements the same way (`tests/fixtures/client/composer.json`), and the
//! client-local state a composer row keeps (draft, cursor, control values) until Send.

use std::collections::BTreeMap;

use serde_json::{json, Value};

/// Default and largest prompt size, in UTF-8 bytes.
pub const DEFAULT_MAX_BYTES: usize = 16 * 1024;
pub const MAX_BYTES: usize = 64 * 1024;

/// What a key did to the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome { Edited, Submit, Leave, Refused, Ignored }

impl Outcome {
    pub fn name(self) -> &'static str {
        match self { Outcome::Edited => "edit", Outcome::Submit => "submit", Outcome::Leave => "leave", Outcome::Refused => "refused", Outcome::Ignored => "ignored" }
    }
}

/// A multi-line text buffer with a cursor (a character index). `submit_key` is `Enter` (default; Shift-Enter,
/// Alt-Enter and Ctrl-J add a line) or `C-Enter` (Enter adds a line).
#[derive(Debug, Clone, Default)]
pub struct Editor {
    pub text: Vec<char>,
    pub cursor: usize,
    pub max_bytes: usize,
    pub submit_ctrl: bool,
}

impl Editor {
    pub fn new(text: &str, max_bytes: Option<u64>, submit: Option<&str>) -> Editor {
        let max = max_bytes.map(|m| (m as usize).min(MAX_BYTES)).unwrap_or(DEFAULT_MAX_BYTES);
        let text: Vec<char> = text.chars().collect();
        Editor { cursor: text.len(), text, max_bytes: max, submit_ctrl: submit == Some("C-Enter") }
    }

    pub fn string(&self) -> String { self.text.iter().collect() }

    fn bytes(&self) -> usize { self.text.iter().map(|c| c.len_utf8()).sum() }

    /// Insert text at the cursor; refused whole when it would pass `max_bytes`. Control characters other than
    /// newline are dropped; tabs become four spaces.
    pub fn insert(&mut self, s: &str) -> Outcome {
        let clean: Vec<char> = s.replace("\r\n", "\n").replace('\r', "\n").replace('\t', "    ").chars().filter(|c| *c == '\n' || !c.is_control()).collect();
        if clean.is_empty() { return Outcome::Ignored; }
        if self.bytes() + clean.iter().map(|c| c.len_utf8()).sum::<usize>() > self.max_bytes { return Outcome::Refused; }
        let n = clean.len();
        self.text.splice(self.cursor..self.cursor, clean);
        self.cursor += n;
        Outcome::Edited
    }

    fn line_start(&self, at: usize) -> usize { self.text[..at].iter().rposition(|c| *c == '\n').map(|i| i + 1).unwrap_or(0) }
    fn line_end(&self, at: usize) -> usize { self.text[at..].iter().position(|c| *c == '\n').map(|i| at + i).unwrap_or(self.text.len()) }

    /// One key, by its canonical name (core-protocol.md, Keys); a printable character is its own name.
    pub fn key(&mut self, key: &str) -> Outcome {
        match key {
            "Enter" if !self.submit_ctrl => self.submit(),
            "C-Enter" if self.submit_ctrl => self.submit(),
            "Enter" | "S-Enter" | "M-Enter" | "C-j" => self.insert("\n"),
            "Esc" => Outcome::Leave,
            "Backspace" => { if self.cursor == 0 { return Outcome::Ignored; } self.cursor -= 1; self.text.remove(self.cursor); Outcome::Edited }
            "Delete" => { if self.cursor >= self.text.len() { return Outcome::Ignored; } self.text.remove(self.cursor); Outcome::Edited }
            "Left" => { if self.cursor == 0 { return Outcome::Ignored; } self.cursor -= 1; Outcome::Edited }
            "Right" => { if self.cursor >= self.text.len() { return Outcome::Ignored; } self.cursor += 1; Outcome::Edited }
            "Home" => { self.cursor = self.line_start(self.cursor); Outcome::Edited }
            "End" => { self.cursor = self.line_end(self.cursor); Outcome::Edited }
            "Up" | "Down" => {
                let start = self.line_start(self.cursor);
                let column = self.cursor - start;
                let target = if key == "Up" {
                    if start == 0 { return Outcome::Ignored; }
                    self.line_start(start - 1)
                } else {
                    let end = self.line_end(self.cursor);
                    if end >= self.text.len() { return Outcome::Ignored; }
                    end + 1
                };
                self.cursor = (target + column).min(self.line_end(target));
                Outcome::Edited
            }
            "Space" => self.insert(" "),
            k if k.chars().count() == 1 => self.insert(k),
            _ => Outcome::Ignored,
        }
    }

    fn submit(&self) -> Outcome { if self.text.iter().all(|c| c.is_whitespace()) { Outcome::Ignored } else { Outcome::Submit } }

    /// Lines of the text and the cursor's (line, column), for drawing.
    pub fn lines(&self) -> (Vec<String>, (usize, usize)) {
        let lines: Vec<String> = self.string().split('\n').map(String::from).collect();
        let before = &self.text[..self.cursor];
        let line = before.iter().filter(|c| **c == '\n').count();
        let column = self.cursor - self.line_start(self.cursor);
        (lines, (line, column))
    }
}

/// A composer row's client-local state: its draft and the current value of each control (by `arg`).
#[derive(Debug, Clone, Default)]
pub struct Composer {
    pub editor: Editor,
    pub values: BTreeMap<String, Value>,
}

impl Composer {
    /// A fresh state for a row: `prefill` fills an empty draft; controls start at their declared values.
    pub fn new(row: &Value) -> Composer {
        let mut values = BTreeMap::new();
        for c in controls(row) {
            if let Some(arg) = c["arg"].as_str() {
                let v = if c["type"] == "chips" {
                    c["chips"].as_array().into_iter().flatten().find(|x| x["checked"] == true).map(|x| x["value"].clone()).unwrap_or(Value::Null)
                } else { c["value"].clone() };
                values.insert(arg.to_string(), v);
            }
        }
        Composer { editor: Editor::new(row["prefill"].as_str().unwrap_or(""), row["max_bytes"].as_u64(), row["submit"].as_str()), values }
    }

    /// The arguments Send invokes the row's command with: its `args`, the controls' values, then `text`.
    pub fn submit_args(&self, row: &Value) -> Value {
        let mut args = if row["args"].is_object() { row["args"].clone() } else { json!({}) };
        for (k, v) in &self.values { if !v.is_null() { args[k] = v.clone(); } }
        args["text"] = json!(self.editor.string());
        args
    }
}

/// A composer row's controls: `select` and `chips` rows with an `arg`.
pub fn controls(row: &Value) -> Vec<&Value> {
    row["controls"].as_array().into_iter().flatten().filter(|c| c["arg"].is_string() && matches!(c["type"].as_str(), Some("select" | "chips"))).collect()
}

/// The buttons on a composer's control line, in order: each control's choices (a select is one button opening its
/// options; a chips row is one button per chip), then Send. `(arg, value)`; Send is `("", Null)`.
pub fn buttons(row: &Value) -> Vec<(String, Option<Value>)> {
    let mut out = Vec::new();
    for c in controls(row) {
        let arg = c["arg"].as_str().unwrap_or("").to_string();
        if c["type"] == "chips" {
            for chip in c["chips"].as_array().into_iter().flatten() { out.push((arg.clone(), Some(chip["value"].clone()))); }
        } else { out.push((arg, None)); }
    }
    // Send, unless the row hides it (`send_button: false`: Enter still sends).
    if row["send_button"] != false { out.push((String::new(), None)); }
    out
}
