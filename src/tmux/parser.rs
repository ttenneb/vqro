//! Byte-oriented tmux control framing. Block bodies are opaque (even lines starting `%`).
use std::fmt;

pub const MAX_LINE: usize = 1024 * 1024;
const MAX_BLOCK: usize = 4 * MAX_LINE;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PaneId(String);
impl PaneId {
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        if s.starts_with('%') && s.len() > 1 && s[1..].bytes().all(|b| b.is_ascii_digit()) {
            Ok(Self(s.into()))
        } else {
            Err(ParseError("invalid pane id".into()))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockId {
    pub time: u64,
    pub number: u64,
    pub flags: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notification {
    Layout {
        window: String,
        layout: String,
        visible: String,
        flags: String,
    },
    WindowAdded {
        window: String,
        linked: bool,
    },
    WindowClosed {
        window: String,
        linked: bool,
    },
    WindowPaneChanged {
        window: String,
        pane: PaneId,
    },
    PaneModeChanged(PaneId),
    Subscription {
        name: String,
        session: String,
        window: String,
        index: String,
        pane: String,
        value: Vec<u8>,
    },
    Pause(PaneId),
    Continue(PaneId),
    Exit(Vec<u8>),
    /// Client-generated terminal event, including EOF without a native %exit notification.
    Disconnected {
        reason: String,
    },
    /// Unknown/future notifications, including names not documented by this tmux version.
    Other {
        name: String,
        arguments: Vec<u8>,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Record {
    Block { id: BlockId, error: bool, lines: Vec<Vec<u8>> },
    Output { pane: PaneId, age_ms: Option<u64>, bytes: Vec<u8> },
    Notification(Notification),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError(pub String);
impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ParseError {}

#[derive(Default)]
pub struct Parser {
    line: Vec<u8>,
    block: Option<(BlockId, Vec<Vec<u8>>, usize)>,
}
impl Parser {
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Record>, ParseError> {
        let mut out = Vec::new();
        for &b in bytes {
            if b == b'\n' {
                let line = std::mem::take(&mut self.line);
                if let Some(record) = self.record(line)? {
                    out.push(record);
                }
            } else {
                if self.line.len() == MAX_LINE {
                    return bad("control line exceeds limit");
                }
                self.line.push(b);
            }
        }
        Ok(out)
    }
    pub fn finish(&self) -> Result<(), ParseError> {
        if self.line.is_empty() && self.block.is_none() { Ok(()) } else { bad("truncated control record") }
    }
    fn record(&mut self, line: Vec<u8>) -> Result<Option<Record>, ParseError> {
        let (name, args) = split(&line);
        if let Some((id, lines, size)) = self.block.as_mut() {
            if name == b"%end" || name == b"%error" {
                if &block_id(args)? != id {
                    return bad("mismatched command block terminator");
                }
                let (id, lines, _) = self.block.take().unwrap();
                return Ok(Some(Record::Block { id, error: name == b"%error", lines }));
            }
            *size += line.len() + 1;
            if *size > MAX_BLOCK || lines.len() == 65_536 {
                return bad("command block exceeds limit");
            }
            lines.push(line);
            return Ok(None);
        }
        let rec = match name {
            b"%begin" => {
                self.block = Some((block_id(args)?, Vec::new(), 0));
                return Ok(None);
            }
            b"%end" | b"%error" => return bad("command terminator without begin"),
            b"%output" => {
                let (pane, value) = split_required(args)?;
                Record::Output { pane: PaneId::parse(text(pane)?)?, age_ms: None, bytes: unescape(value)? }
            }
            b"%extended-output" => {
                let (header, value) = delimiter(args)?;
                let fields: Vec<_> = header.split(|b| *b == b' ').collect();
                if fields.len() < 2 {
                    return bad("short extended-output header");
                }
                Record::Output { pane: PaneId::parse(text(fields[0])?)?, age_ms: Some(number(fields[1])?), bytes: unescape(value)? }
            }
            b"%pause" => Record::Notification(Notification::Pause(PaneId::parse(text(args)?)?)),
            b"%continue" => Record::Notification(Notification::Continue(PaneId::parse(text(args)?)?)),
            b"%exit" => Record::Notification(Notification::Exit(args.to_vec())),
            b"%pane-mode-changed" => Record::Notification(Notification::PaneModeChanged(PaneId::parse(text(args)?)?)),
            b"%window-add" | b"%unlinked-window-add" => {
                Record::Notification(Notification::WindowAdded { window: text(args)?.into(), linked: name == b"%window-add" })
            }
            b"%window-close" | b"%unlinked-window-close" => {
                Record::Notification(Notification::WindowClosed { window: text(args)?.into(), linked: name == b"%window-close" })
            }
            b"%window-pane-changed" => {
                let (window, pane) = split_required(args)?;
                Record::Notification(Notification::WindowPaneChanged { window: text(window)?.into(), pane: PaneId::parse(text(pane)?)? })
            }
            b"%layout-change" => {
                let fields: Vec<_> = text(args)?.splitn(4, ' ').collect();
                if fields.len() != 4 {
                    return bad("short layout-change");
                }
                Record::Notification(Notification::Layout {
                    window: fields[0].into(),
                    layout: fields[1].into(),
                    visible: fields[2].into(),
                    flags: fields[3].into(),
                })
            }
            b"%subscription-changed" => {
                let (header, value) = delimiter(args)?;
                let fields: Vec<_> = text(header)?.split(' ').collect();
                if fields.len() < 5 {
                    return bad("short subscription header");
                }
                Record::Notification(Notification::Subscription {
                    name: fields[0].into(),
                    session: fields[1].into(),
                    window: fields[2].into(),
                    index: fields[3].into(),
                    pane: fields[4].into(),
                    value: value.to_vec(),
                })
            }
            _ if name.starts_with(b"%") => Record::Notification(Notification::Other { name: text(name)?.into(), arguments: args.to_vec() }),
            _ => return bad("unexpected non-control line"),
        };
        Ok(Some(rec))
    }
}
fn bad<T>(s: &str) -> Result<T, ParseError> {
    Err(ParseError(s.into()))
}
fn text(b: &[u8]) -> Result<&str, ParseError> {
    std::str::from_utf8(b).map_err(|_| ParseError("non-UTF8 control header".into()))
}
fn number(b: &[u8]) -> Result<u64, ParseError> {
    text(b)?.parse().map_err(|_| ParseError("invalid control number".into()))
}
fn split(b: &[u8]) -> (&[u8], &[u8]) {
    b.iter().position(|c| *c == b' ').map(|i| (&b[..i], &b[i + 1..])).unwrap_or((b, &[]))
}
fn split_required(b: &[u8]) -> Result<(&[u8], &[u8]), ParseError> {
    if !b.contains(&b' ') {
        return bad("missing control field");
    }
    Ok(split(b))
}
fn delimiter(b: &[u8]) -> Result<(&[u8], &[u8]), ParseError> {
    b.windows(3)
        .position(|s| s == b" : ")
        .map(|i| (&b[..i], &b[i + 3..]))
        .ok_or_else(|| ParseError("missing extended record delimiter".into()))
}
fn block_id(b: &[u8]) -> Result<BlockId, ParseError> {
    let fields: Vec<_> = b.split(|c| *c == b' ').collect();
    if fields.len() != 3 {
        return bad("invalid block header");
    }
    Ok(BlockId { time: number(fields[0])?, number: number(fields[1])?, flags: number(fields[2])? })
}
/// Only tmux's three-octal-digit escapes, never UTF8/string escape interpretation.
pub fn unescape(b: &[u8]) -> Result<Vec<u8>, ParseError> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let n = b[i + 1..i + 4].iter().fold(0u16, |n, c| n * 8 + u16::from(c - b'0'));
            if n > 255 {
                return bad("octal escape exceeds byte");
            }
            out.push(n as u8);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_byte_boundary_and_opaque_block_body() {
        let input = b"%begin 1 2 1\n%output %9 literal-command-output\n%end 1 2 1\n%output %0 A\\000\\033\\377\\134\n%extended-output %1 1498 future : B\\012\n";
        let mut p = Parser::default();
        let mut records = Vec::new();
        for b in input {
            records.extend(p.feed(&[*b]).unwrap());
        }
        p.finish().unwrap();
        assert!(
            matches!(&records[0], Record::Block { lines, error: false, .. } if lines == &[b"%output %9 literal-command-output".to_vec()])
        );
        assert_eq!(records[1], Record::Output { pane: PaneId::parse("%0").unwrap(), age_ms: None, bytes: vec![b'A', 0, 27, 255, 92] });
        assert_eq!(records[2], Record::Output { pane: PaneId::parse("%1").unwrap(), age_ms: Some(1498), bytes: b"B\n".to_vec() });
    }
    #[test]
    fn errors_and_future_notifications() {
        let mut p = Parser::default();
        assert!(matches!(&p.feed(b"%begin 1 2 1\nbad\n%error 1 2 1\n").unwrap()[0], Record::Block { error: true, .. }));
        assert!(p.feed(b"%begin 1 3 1\n%end 1 4 1\n").is_err());
        assert!(Parser::default().feed(b"%end 1 2 1\n").is_err());
        assert!(Parser::default().feed(&vec![b'x'; MAX_LINE + 1]).is_err());
        assert_eq!(unescape(b"\\12 \\x41 \\089").unwrap(), b"\\12 \\x41 \\089");
        assert!(unescape(b"\\777").is_err());
        let mut p = Parser::default();
        assert!(matches!(&p.feed(b"%pane-future %3 unknown\n").unwrap()[0], Record::Notification(Notification::Other { .. })));
        p.feed(b"%output %0 unfinished").unwrap();
        assert!(p.finish().is_err());
    }
    #[test]
    fn subscription_and_structure() {
        let mut p = Parser::default();
        let records = p.feed(b"%subscription-changed pane $0 @1 2 %3 future : a : b\n%layout-change @1 ab cd *\n%window-add @1\n%unlinked-window-close @2\n%window-pane-changed @1 %3\n%pause %3\n%continue %3\n%exit detached\n").unwrap();
        assert!(
            matches!(&records[0], Record::Notification(Notification::Subscription { pane, value, .. }) if pane == "%3" && value == b"a : b")
        );
        assert!(matches!(&records[1], Record::Notification(Notification::Layout { .. })));
        assert!(matches!(&records[2], Record::Notification(Notification::WindowAdded { linked: true, .. })));
        assert!(matches!(&records[3], Record::Notification(Notification::WindowClosed { linked: false, .. })));
        assert!(matches!(&records[4], Record::Notification(Notification::WindowPaneChanged { .. })));
        assert!(matches!(&records[5], Record::Notification(Notification::Pause(_))));
        assert!(matches!(&records[6], Record::Notification(Notification::Continue(_))));
        assert!(matches!(&records[7], Record::Notification(Notification::Exit(_))));
    }
}
