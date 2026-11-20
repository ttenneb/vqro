//! Formatted text rows (`format: "markdown"`, feature `markdown`, D29; `research/rich-text-and-composer.md`): a small
//! fixed markdown subset laid out into styled lines, the same in every client (`tests/fixtures/client/markdown.json`).
//!
//! Subset: `#` to `###` headings; paragraphs; `-`/`*` and `1.` list items (nested by indenting two spaces per
//! level, continued by indented lines); `**bold**`; `` `inline code` ``; fenced code blocks. Everything else is
//! literal text. Styles are names the renderer maps to semantic palette styles: `""` plain, `h` heading, `b` bold,
//! `c` inline code, `pre` code block.

use unicode_width::UnicodeWidthStr;

/// One styled piece of a line.
pub type Span = (String, &'static str);
pub type Line = Vec<Span>;

enum Block { Heading(String), Para(String), Item { level: usize, marker: String, text: String }, Code(Vec<String>), Blank }

fn width(s: &str) -> usize { s.width() }

fn parse(text: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut para: Option<String> = None;
    let mut in_item = false;
    let source = text.replace('\r', "").replace('\t', "    ");
    let mut lines = source.split('\n');
    let flush = |para: &mut Option<String>, blocks: &mut Vec<Block>| if let Some(p) = para.take() { blocks.push(Block::Para(p)); };
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            flush(&mut para, &mut blocks);
            let mut code = Vec::new();
            for l in lines.by_ref() { if l.trim_start().starts_with("```") { break; } code.push(l.to_string()); }
            blocks.push(Block::Code(code));
            in_item = false;
            continue;
        }
        if line.trim().is_empty() {
            flush(&mut para, &mut blocks);
            if !matches!(blocks.last(), Some(Block::Blank) | None) { blocks.push(Block::Blank); }
            in_item = false;
            continue;
        }
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if (1..=3).contains(&hashes) && line[hashes..].starts_with(' ') {
            flush(&mut para, &mut blocks);
            blocks.push(Block::Heading(line[hashes..].trim().to_string()));
            in_item = false;
            continue;
        }
        let indent = line.len() - trimmed.len();
        let marker = if trimmed.starts_with("- ") || trimmed.starts_with("* ") { Some(("•".to_string(), 2)) } else {
            let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
            ((1..=9).contains(&digits) && trimmed[digits..].starts_with(". ")).then(|| (format!("{}.", &trimmed[..digits]), digits + 2))
        };
        if let Some((marker, len)) = marker {
            flush(&mut para, &mut blocks);
            blocks.push(Block::Item { level: (indent / 2).min(4), marker, text: trimmed[len..].trim().to_string() });
            in_item = true;
            continue;
        }
        if in_item && indent >= 2 {
            if let Some(Block::Item { text, .. }) = blocks.last_mut() { text.push(' '); text.push_str(line.trim()); }
            continue;
        }
        in_item = false;
        match para.as_mut() { Some(p) => { p.push(' '); p.push_str(line.trim()); } None => para = Some(line.trim().to_string()) }
    }
    flush(&mut para, &mut blocks);
    blocks
}

/// Inline runs: `` `code` `` and `**bold**` when closed; anything else literal.
fn inline(s: &str, plain: &'static str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if rest.starts_with('`') {
            if let Some(j) = rest[1..].find('`') {
                if !buf.is_empty() { out.push((std::mem::take(&mut buf), plain)); }
                out.push((rest[1..1 + j].to_string(), "c"));
                rest = &rest[2 + j..];
                continue;
            }
        }
        if rest.starts_with("**") {
            if let Some(j) = rest[2..].find("**").filter(|j| *j > 0) {
                if !buf.is_empty() { out.push((std::mem::take(&mut buf), plain)); }
                out.push((rest[2..2 + j].to_string(), if plain == "h" { "h" } else { "b" }));
                rest = &rest[4 + j..];
                continue;
            }
        }
        buf.push(c);
        rest = &rest[c.len_utf8()..];
    }
    if !buf.is_empty() { out.push((buf, plain)); }
    out
}

/// Greedy word wrap of styled runs at `w` cells. Words glued without whitespace (`x`, then `,`) stay together;
/// a word wider than the line breaks by characters. A space between two words of one style keeps that style.
fn wrap(spans: &[Span], w: usize) -> Vec<Line> {
    let w = w.max(1);
    // Units: words glued without whitespace, each a list of (text, style).
    let mut units: Vec<Vec<Span>> = Vec::new();
    let mut space = true;
    for (text, style) in spans {
        let mut word = String::new();
        for c in text.chars() {
            if c.is_whitespace() {
                if !word.is_empty() { push_word(&mut units, std::mem::take(&mut word), style, space); }
                space = true;
            } else { word.push(c); }
        }
        if !word.is_empty() { push_word(&mut units, word, style, space); space = false; }
    }
    let mut out: Vec<Line> = Vec::new();
    let mut line: Line = Vec::new();
    let mut used = 0;
    for unit in units {
        let uw: usize = unit.iter().map(|(t, _)| width(t)).sum();
        if used > 0 && used + 1 + uw > w { out.push(std::mem::take(&mut line)); used = 0; }
        if used > 0 {
            let prev = line.last().map(|s| s.1).unwrap_or("");
            line.push((" ".into(), if prev == unit[0].1 { prev } else { "" }));
            used += 1;
        }
        for (text, style) in unit {
            for c in text.chars() {
                let cw = width(c.encode_utf8(&mut [0; 4]));
                if used + cw > w && used > 0 { out.push(std::mem::take(&mut line)); used = 0; }
                line.push((c.to_string(), style));
                used += cw;
            }
        }
    }
    if !line.is_empty() || out.is_empty() { out.push(line); }
    out.into_iter().map(merge).collect()
}

fn push_word(units: &mut Vec<Vec<Span>>, word: String, style: &'static str, space: bool) {
    match units.last_mut() { Some(u) if !space => u.push((word, style)), _ => units.push(vec![(word, style)]) }
}

fn merge(line: Line) -> Line {
    let mut out: Line = Vec::new();
    for (t, s) in line {
        match out.last_mut() { Some(last) if last.1 == s => last.0.push_str(&t), _ => out.push((t, s)) }
    }
    out
}

/// The text's lines at `width` cells. Blocks are separated by one empty line, except items of one list.
pub fn lines(text: &str, width_cells: usize) -> Vec<Line> {
    let w = width_cells.max(1);
    let mut out: Vec<Line> = Vec::new();
    let mut prev_item = false;
    let mut gap = false;
    for block in parse(text) {
        if let Block::Blank = block { gap = true; continue; }
        let is_item = matches!(block, Block::Item { .. });
        if !out.is_empty() && (gap || !(prev_item && is_item)) { out.push(Vec::new()); }
        gap = false;
        prev_item = is_item;
        match block {
            Block::Heading(t) => out.extend(wrap(&inline(&t, "h"), w)),
            Block::Para(t) => out.extend(wrap(&inline(&t, ""), w)),
            Block::Item { level, marker, text } => {
                let prefix = format!("{}{} ", "  ".repeat(level), marker);
                let pw = width(&prefix);
                for (n, mut l) in wrap(&inline(&text, ""), w.saturating_sub(pw).max(1)).into_iter().enumerate() {
                    l.insert(0, (if n == 0 { prefix.clone() } else { " ".repeat(pw) }, ""));
                    out.push(merge(l));
                }
            }
            Block::Code(code) => for l in code {
                let shown = format!("  {l}");
                out.push(vec![(if width(&shown) > w { cut(&shown, w) } else { shown }, "pre")]);
            },
            Block::Blank => {}
        }
    }
    out
}

fn cut(s: &str, w: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = width(c.encode_utf8(&mut [0; 4]));
        if used + cw > w.saturating_sub(1) { break; }
        out.push(c);
        used += cw;
    }
    out + "…"
}
