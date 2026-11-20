//! Reading a terminal's history (terminal-basics audit, slice T2): positions that stay put while output scrolls,
//! selection by cell, word or line, the text between two positions, and search.
//!
//! A position's `row` counts lines since the parser started: the rows that scrolled off the top so far
//! (`scrolled`: the rows the full scrollback dropped, counted by the parser callbacks, plus those it keeps) and the
//! screen row. Asking for a dropped row gets nothing. Reading moves the parser's scrollback view and puts it back.
//! Clearing the scrollback (`CSI 3 J`) renumbers rows; a selection made before it may then name other text.
use vt100::{Callbacks, Parser};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub row: usize,
    pub col: u16,
}

/// How a selection grows: by cell, by word (double click) or by line (triple click).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Unit {
    #[default]
    Cell,
    Word,
    Line,
}

/// A selection in one terminal view. `anchor` is where it began, `head` where it is now; for words and lines both
/// ends are already widened to whole words or lines.
#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub anchor: Pos,
    pub head: Pos,
    pub unit: Unit,
    /// Moved since the press (a press without a move is a click, not a selection).
    pub dragged: bool,
}

impl Selection {
    pub fn ordered(&self) -> (Pos, Pos) {
        if self.anchor <= self.head { (self.anchor, self.head) } else { (self.head, self.anchor) }
    }

    /// Whether a cell is selected: whole rows between the ends, and the cells from the start and up to the end.
    pub fn contains(&self, p: Pos) -> bool {
        let (a, b) = self.ordered();
        p >= a && p <= b
    }
}

/// Rows above the live screen so far, from the count of rows the scrollback dropped.
pub fn scrolled<CB: Callbacks>(parser: &Parser<CB>, dropped: usize) -> usize {
    dropped + parser.screen().scrollback_len()
}

/// The row at the top of the screen as shown now (scrolled back or not).
pub fn top<CB: Callbacks>(parser: &Parser<CB>, scrolled: usize) -> usize {
    scrolled.saturating_sub(parser.screen().scrollback())
}

/// The oldest row still kept.
pub fn oldest<CB: Callbacks>(parser: &Parser<CB>, scrolled: usize) -> usize {
    scrolled.saturating_sub(parser.screen().scrollback_len())
}

/// The newest row (the bottom of the live screen).
pub fn newest<CB: Callbacks>(parser: &Parser<CB>, scrolled: usize) -> usize {
    scrolled + usize::from(parser.screen().size().0.get()) - 1
}

/// One row's text with trailing blanks removed, the cell where each of its characters starts, and whether the row
/// wraps onto the next (one logical line). None when the row is no longer kept.
pub fn row<CB: Callbacks>(parser: &mut Parser<CB>, scrolled: usize, at: usize) -> Option<(Vec<char>, Vec<u16>, bool)> {
    if at < oldest(parser, scrolled) || at > newest(parser, scrolled) {
        return None;
    }
    let was = parser.screen().scrollback();
    let (offset, r) = if at >= scrolled { (0, (at - scrolled) as u16) } else { (scrolled - at, 0) };
    parser.screen_mut().set_scrollback(offset);
    let screen = parser.screen();
    let cols = screen.size().1.get();
    let (mut chars, mut starts) = (Vec::new(), Vec::new());
    for col in 0..cols {
        let Some(cell) = screen.cell(r, col) else { break };
        if cell.is_wide_continuation() {
            continue;
        }
        let text = cell.contents();
        for c in if text.is_empty() { " " } else { text }.chars() {
            chars.push(c);
            starts.push(col);
        }
    }
    let wrapped = screen.row_wrapped(r);
    parser.screen_mut().set_scrollback(was);
    while !wrapped && chars.last() == Some(&' ') {
        chars.pop();
        starts.pop();
    }
    Some((chars, starts, wrapped))
}

/// The text between two positions (inclusive), rows joined by newlines except where a row wraps onto the next.
pub fn text<CB: Callbacks>(parser: &mut Parser<CB>, scrolled: usize, a: Pos, b: Pos) -> String {
    let mut out = String::new();
    for at in a.row..=b.row {
        let Some((chars, starts, wrapped)) = row(parser, scrolled, at) else { continue };
        let from = if at == a.row { a.col } else { 0 };
        let to = if at == b.row { b.col } else { u16::MAX };
        out.extend(chars.iter().zip(&starts).filter(|(_, c)| **c >= from && **c <= to).map(|(ch, _)| *ch));
        if at != b.row && !wrapped {
            out.push('\n');
        }
    }
    out
}

/// Characters that make up a word for double-click selection (paths and URLs stay whole), a run of blanks, or one
/// of the brackets and quotes around them.
fn class(c: char) -> u8 {
    if c.is_whitespace() { 0 } else if "\"'`()[]{}<>|│,;".contains(c) { 2 } else { 1 }
}

/// The word (or run of blanks, or bracket) around a position, as its first and last cells.
pub fn word<CB: Callbacks>(parser: &mut Parser<CB>, scrolled: usize, p: Pos) -> (Pos, Pos) {
    let Some((chars, starts, _)) = row(parser, scrolled, p.row) else { return (p, p) };
    let Some(i) = starts.iter().rposition(|c| *c <= p.col) else { return (p, p) };
    let k = class(chars[i]);
    let (mut s, mut e) = (i, i);
    if k != 2 {
        while s > 0 && class(chars[s - 1]) == k { s -= 1; }
        while e + 1 < chars.len() && class(chars[e + 1]) == k { e += 1; }
    }
    (Pos { row: p.row, col: starts[s] }, Pos { row: p.row, col: starts[e] })
}

/// The logical line (rows joined by wrapping) around a row, as its first and last cells.
pub fn line<CB: Callbacks>(parser: &mut Parser<CB>, scrolled: usize, at: usize) -> (Pos, Pos) {
    let mut s = at;
    while s > oldest(parser, scrolled) && row(parser, scrolled, s - 1).is_some_and(|r| r.2) { s -= 1; }
    let mut e = at;
    while row(parser, scrolled, e).is_some_and(|r| r.2) && e < newest(parser, scrolled) { e += 1; }
    (Pos { row: s, col: 0 }, Pos { row: e, col: u16::MAX })
}

/// Grow a selection's ends to its unit (word or line).
pub fn widen<CB: Callbacks>(parser: &mut Parser<CB>, scrolled: usize, sel: &mut Selection) {
    let forward = sel.anchor <= sel.head;
    let span = |parser: &mut Parser<CB>, p: Pos| match sel.unit {
        Unit::Cell => (p, p),
        Unit::Word => word(parser, scrolled, p),
        Unit::Line => line(parser, scrolled, p.row),
    };
    let (a0, a1) = span(parser, sel.anchor);
    let (h0, h1) = span(parser, sel.head);
    if forward { sel.anchor = a0; sel.head = h1; } else { sel.anchor = a1; sel.head = h0; }
}

/// A search match: its row, first cell and width in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub row: usize,
    pub col: u16,
    pub width: u16,
}

/// Every match of `query` (case-insensitive, within a row) in the kept history, oldest first.
pub fn search<CB: Callbacks>(parser: &mut Parser<CB>, scrolled: usize, query: &str) -> Vec<Hit> {
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let mut hits = Vec::new();
    if q.is_empty() {
        return hits;
    }
    for at in oldest(parser, scrolled)..=newest(parser, scrolled) {
        let Some((chars, starts, _)) = row(parser, scrolled, at) else { continue };
        // Lowercase char by char so indexes stay those of `starts` (a few characters lowercase to two).
        let lower: Vec<char> = chars.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
        let mut i = 0;
        while i + q.len() <= lower.len() {
            if lower[i..i + q.len()] == q[..] {
                let last = i + q.len() - 1;
                let end = starts[last] + unicode_width::UnicodeWidthChar::width(chars[last]).unwrap_or(1).max(1) as u16;
                hits.push(Hit { row: at, col: starts[i], width: end.saturating_sub(starts[i]).max(1) });
                i += q.len();
            } else {
                i += 1;
            }
        }
    }
    hits
}

/// Scroll so a row is in view (about a third from the top), or back to the live screen when it is on it.
pub fn reveal<CB: Callbacks>(parser: &mut Parser<CB>, scrolled: usize, at: usize) {
    let rows = usize::from(parser.screen().size().0.get());
    let shown = top(parser, scrolled);
    if at >= shown && at < shown + rows {
        return;
    }
    let offset = (scrolled + rows / 3).saturating_sub(at);
    parser.screen_mut().set_scrollback(offset);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Count(usize);
    impl Callbacks for Count {
        // Called for rows the full scrollback drops.
        fn on_scroll(&mut self, _: vt100::capture::RowContents<'_>, alternate: bool) {
            if !alternate { self.0 += 1; }
        }
    }

    fn parser(lines: &str) -> Parser<Count> {
        let mut p = Parser::new_with_callbacks(std::num::NonZeroU16::new(4).unwrap(), std::num::NonZeroU16::new(20).unwrap(), 100, Count::default());
        p.process(lines.as_bytes());
        p
    }

    #[test]
    fn positions_text_words_lines_and_search_survive_scrolling() {
        let mut p = parser("one alpha\r\ntwo beta\r\nthree alpha\r\nfour\r\nfive ALPHA six");
        let n = scrolled(&p, p.callbacks().0);
        assert_eq!(n, 1, "one row left the 4-row screen");
        assert_eq!((oldest(&p, n), newest(&p, n)), (0, 4));
        assert_eq!(text(&mut p, n, Pos { row: 0, col: 4 }, Pos { row: 1, col: 2 }), "alpha\ntwo");
        assert_eq!(word(&mut p, n, Pos { row: 2, col: 8 }), (Pos { row: 2, col: 6 }, Pos { row: 2, col: 10 }));
        let hits = search(&mut p, n, "Alpha");
        assert_eq!(hits.iter().map(|h| (h.row, h.col, h.width)).collect::<Vec<_>>(), vec![(0, 4, 5), (2, 6, 5), (4, 5, 5)]);
        // More output: the same positions still name the same text; the view is restored after reading.
        p.process(b"\r\nseven\r\neight");
        let n = scrolled(&p, p.callbacks().0);
        assert_eq!(p.screen().scrollback(), 0);
        assert_eq!(text(&mut p, n, Pos { row: 2, col: 0 }, Pos { row: 2, col: u16::MAX }), "three alpha");
        reveal(&mut p, n, 0);
        assert_eq!(top(&p, n), 0, "the oldest row is in view");
        // A long line wraps over rows and is one logical line.
        let mut p = parser("x\r\n0123456789012345678901234");
        let n = scrolled(&p, p.callbacks().0);
        let (a, b) = line(&mut p, n, 2);
        assert_eq!((a.row, b.row), (1, 2));
        assert_eq!(text(&mut p, n, a, b), "0123456789012345678901234");
        // A small scrollback drops old rows: positions keep counting, dropped rows read as nothing.
        let mut p = Parser::new_with_callbacks(std::num::NonZeroU16::new(2).unwrap(), std::num::NonZeroU16::new(10).unwrap(), 2, Count::default());
        p.process(b"r0\r\nr1\r\nr2\r\nr3\r\nr4\r\nr5");
        let n = scrolled(&p, p.callbacks().0);
        assert_eq!((n, oldest(&p, n)), (4, 2));
        assert_eq!(text(&mut p, n, Pos { row: 0, col: 0 }, Pos { row: 5, col: 9 }), "r2\nr3\nr4\nr5");
    }
}
