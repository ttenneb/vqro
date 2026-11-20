//! Links in terminal text (core-protocol.md "Links"): plain URLs and file paths a client finds in a row of a
//! terminal pane, so Ctrl-click and the pane menu can hand them to an extension (`links.open`). Detection only:
//! what opening means is the extension's choice (D17).
//!
//! - **URL:** `http://`, `https://`, `file://`, `ftp://` up to whitespace or a quote/angle bracket/backtick.
//!   Trailing sentence punctuation (`.,;:!?`) and closing brackets without a matching opener are not part of it.
//! - **Path:** a token with a `/` (`src/main.rs`, `/etc/hosts`, `~/notes.md`, `./run.sh`), or a bare file name
//!   with an extension and a line (`main.rs:12`), of path characters. A `:line` or `:line:column` suffix is split
//!   off into `line`/`column`. Not checked against any file system: the client may not run where the file lives.
//!
//! Positions are char indices into the row text (`start` inclusive, `end` exclusive).
use serde_json::{json, Value};

const SCHEMES: [&str; 4] = ["https://", "http://", "file://", "ftp://"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub kind: &'static str,
    pub text: String,
    pub start: usize,
    pub end: usize,
    pub line: Option<u64>,
    pub column: Option<u64>,
}

impl Link {
    /// The link as invocation context `link` (core-protocol.md): `{kind, text, line?, column?}`.
    pub fn context(&self) -> Value {
        let mut v = json!({"kind": self.kind, "text": self.text});
        if let Some(l) = self.line { v["line"] = json!(l); }
        if let Some(c) = self.column { v["column"] = json!(c); }
        v
    }

    /// For conformance fixtures: the context plus its position.
    pub fn describe(&self) -> Value {
        let mut v = self.context();
        v["start"] = json!(self.start);
        v["end"] = json!(self.end);
        v
    }
}

fn breaks(c: char) -> bool { c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | '|') }

fn path_char(c: char) -> bool { c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~' | '+' | '@' | '%' | '=' | ',' | ':' | '#') }

/// Drop trailing punctuation and closing brackets that have no opener inside the token.
fn trim_end(chars: &[char]) -> usize {
    let mut end = chars.len();
    while end > 0 {
        let c = chars[end - 1];
        let unmatched = |open: char, close: char| c == close && chars[..end].iter().filter(|&&x| x == open).count() < chars[..end].iter().filter(|&&x| x == close).count();
        if matches!(c, '.' | ',' | ';' | ':' | '!' | '?') || unmatched('(', ')') || unmatched('[', ']') || unmatched('{', '}') {
            end -= 1;
        } else {
            break;
        }
    }
    end
}

/// Split a trailing `:line` or `:line:column` off a path token.
fn split_position(token: &str) -> (&str, Option<u64>, Option<u64>) {
    let mut parts = token.rsplitn(3, ':').collect::<Vec<_>>();
    parts.reverse();
    let num = |s: &str| (!s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse::<u64>().unwrap());
    match parts.as_slice() {
        [path, line, col] if !path.is_empty() && num(line).is_some() && num(col).is_some() => (path, num(line), num(col)),
        _ => match token.rsplit_once(':') {
            Some((path, line)) if !path.is_empty() && num(line).is_some() => (path, num(line), None),
            _ => (token, None, None),
        },
    }
}

fn looks_like_path(path: &str, has_line: bool) -> bool {
    if !path.chars().any(|c| c.is_alphanumeric()) || path.contains("://") {
        return false;
    }
    if path.contains('/') {
        return !path.starts_with("//");
    }
    // A bare name: only with an extension and a line (`main.rs:12`), as compilers print it.
    has_line && path.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// Every link in a row of text, left to right.
pub fn find_all(line: &str) -> Vec<Link> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if breaks(chars[i]) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() && !breaks(chars[j]) {
            j += 1;
        }
        // Within a token: a URL may start after a bracket or `=` (`(https://…)`, `href=https://…`).
        let token: String = chars[i..j].iter().collect();
        let url_at = SCHEMES.iter().filter_map(|s| token.find(s)).min().map(|byte| token[..byte].chars().count());
        if let Some(off) = url_at {
            let start = i + off;
            let end = start + trim_end(&chars[start..j]);
            let text: String = chars[start..end].iter().collect();
            if SCHEMES.iter().any(|s| text.len() > s.len() && text.starts_with(s)) {
                out.push(Link { kind: "url", text, start, end, line: None, column: None });
            }
        } else {
            // Paths: the longest run of path characters, minus leading openers and trailing punctuation.
            let mut start = i;
            while start < j && !path_char(chars[start]) {
                start += 1;
            }
            let mut stop = start;
            while stop < j && path_char(chars[stop]) {
                stop += 1;
            }
            let end = start + trim_end(&chars[start..stop]);
            let token: String = chars[start..end].iter().collect();
            let (path, line, column) = split_position(&token);
            if looks_like_path(path, line.is_some()) {
                out.push(Link { kind: "path", text: path.to_string(), start, end, line, column });
            }
        }
        i = j;
    }
    out
}

/// The link covering char index `col`, if any.
pub fn at(line: &str, col: usize) -> Option<Link> {
    find_all(line).into_iter().find(|l| l.start <= col && col < l.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_paths_in_a_row() {
        let found = find_all("see https://example.org/zz. and src/main.rs:12:5, (https://a.b/x_(y)) ok");
        let texts: Vec<_> = found.iter().map(|l| (l.kind, l.text.as_str(), l.line, l.column)).collect();
        assert_eq!(texts, vec![("url", "https://example.org/zz", None, None), ("path", "src/main.rs", Some(12), Some(5)),
                               ("url", "https://a.b/x_(y)", None, None)]);
        assert_eq!(at("open ~/notes.md now", 7).map(|l| l.text), Some("~/notes.md".into()));
        assert_eq!(at("open ~/notes.md now", 2), None);
    }
}
