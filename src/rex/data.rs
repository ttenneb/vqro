//! Rex's terminal data channel (`rex.data.terminal.v1`).
//!
//! Rex documents the connection setup (`rex api describe data.connect`): a one-time ticket from `data.connect`, a
//! WebSocket at `/api/data`, a binary HELLO carrying the ticket, REJECT reason codes. The rest is not in Rex's docs;
//! it is what community clients implement and what we observed on a private server (vqro-lead-third,
//! 2026-11-22; `docs/research/rex.md`):
//! - HELLO is `0x01` followed by the 16 decoded ticket bytes; the answer is a 32-byte WELCOME starting `0x81`
//!   (bytes 24..32 a little-endian, nonzero channel nonce) or a REJECT starting `0x82` with its reason in byte 1.
//! - The stream starts with a Ghostty snapshot (`GHOSTSNP`, version 1: records of a 2-byte tag, 4-byte payload
//!   length and 4 more header bytes, ending with the FINISH record, tag 6), then the terminal's raw output.
//! - Rex adds its own metadata to that output as `ESC _ rex1;<nonce>;<json> ESC \` (resize, theme, exit).
//!   Only records carrying this channel's nonce are Rex's; anything else is the program's own output.
use serde_json::Value;

/// Upper bound for the initial snapshot (scrollback included). Rex keeps about 64 MiB of scrollback.
pub const MAX_SNAPSHOT: usize = 128 << 20;
/// Upper bound for one Rex metadata record.
pub const MAX_METADATA: usize = 1 << 20;

pub const PROTOCOL: &str = "rex.data.terminal.v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Welcome {
    Accepted { nonce: u64 },
    Rejected { reason: u8 },
}

pub fn hello(ticket: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(ticket.trim_end_matches('='))
        .map_err(|e| format!("data ticket is not base64url: {e}"))?;
    if bytes.len() != 16 {
        return Err(format!("data ticket is {} bytes, expected 16", bytes.len()));
    }
    let mut out = vec![0x01];
    out.extend(bytes);
    Ok(out)
}

pub fn welcome(bytes: &[u8]) -> Result<Welcome, String> {
    match bytes.first() {
        Some(0x82) => Ok(Welcome::Rejected { reason: bytes.get(1).copied().unwrap_or(0) }),
        Some(0x81) if bytes.len() == 32 => {
            let nonce = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
            if nonce == 0 { Err("data WELCOME has a zero nonce".into()) } else { Ok(Welcome::Accepted { nonce }) }
        }
        _ => Err(format!("unexpected data handshake answer ({} bytes)", bytes.len())),
    }
}

/// Finds the end of the initial `GHOSTSNP` snapshot in the first bytes of the stream.
#[derive(Default)]
pub struct Snapshot {
    buf: Vec<u8>,
}

impl Snapshot {
    /// Add bytes. Returns `(snapshot, following)` once the FINISH record is complete.
    pub fn push(&mut self, data: &[u8]) -> Result<Option<(Vec<u8>, Vec<u8>)>, String> {
        if data.len() > MAX_SNAPSHOT.saturating_sub(self.buf.len()) {
            return Err(format!("snapshot over {MAX_SNAPSHOT} bytes"));
        }
        self.buf.extend_from_slice(data);
        if self.buf.len() < 10 {
            return Ok(None);
        }
        if &self.buf[..8] != b"GHOSTSNP" || u16::from_le_bytes([self.buf[8], self.buf[9]]) != 1 {
            return Err("stream does not start with a version 1 GHOSTSNP snapshot".into());
        }
        let mut at = 10usize;
        loop {
            let Some(header) = self.buf.get(at..at + 10) else { return Ok(None) };
            let tag = u16::from_le_bytes([header[0], header[1]]);
            let len = u32::from_le_bytes(header[2..6].try_into().unwrap()) as usize;
            let end = at.checked_add(10).and_then(|n| n.checked_add(len)).filter(|n| *n <= MAX_SNAPSHOT)
                .ok_or_else(|| "snapshot record over the bound".to_string())?;
            if self.buf.len() < end {
                return Ok(None);
            }
            at = end;
            if tag == 6 {
                let following = self.buf.split_off(at);
                return Ok(Some((std::mem::take(&mut self.buf), following)));
            }
        }
    }
}

/// Removes Rex's own metadata records (this channel's nonce only) from the output and returns them parsed.
/// A record split across messages is held until it completes; a prefix that might start one is held too.
pub struct Metadata {
    marker: Vec<u8>,
    held: Vec<u8>,
}

impl Metadata {
    pub fn new(nonce: u64) -> Self {
        Metadata { marker: format!("\x1b_rex1;{nonce};").into_bytes(), held: Vec::new() }
    }

    /// Returns the program's output and Rex's records found in this chunk, in order. A record that never ends
    /// within `MAX_METADATA` is an error (the channel is not trustworthy any more).
    pub fn feed(&mut self, data: &[u8]) -> Result<(Vec<u8>, Vec<Value>), String> {
        let mut input = std::mem::take(&mut self.held);
        input.extend_from_slice(data);
        let (mut out, mut records) = (Vec::with_capacity(input.len()), Vec::new());
        let mut i = 0;
        while i < input.len() {
            let rest = &input[i..];
            if rest.starts_with(&self.marker) {
                let body = &rest[self.marker.len()..];
                match body.windows(2).position(|w| w == b"\x1b\\") {
                    Some(end) => {
                        // A record that isn't JSON is still Rex's (it carries our nonce): dropped, not shown.
                        if let Ok(v) = serde_json::from_slice::<Value>(&body[..end]) {
                            records.push(v);
                        }
                        i += self.marker.len() + end + 2;
                        continue;
                    }
                    None => {
                        if rest.len() > self.marker.len() + MAX_METADATA {
                            return Err("Rex metadata record over 1 MiB".into());
                        }
                        self.held = rest.to_vec();
                        return Ok((out, records));
                    }
                }
            }
            if rest.len() < self.marker.len() && self.marker.starts_with(rest) {
                self.held = rest.to_vec();
                return Ok((out, records));
            }
            out.push(input[i]);
            i += 1;
        }
        Ok((out, records))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(tag: u16, payload: &[u8]) -> Vec<u8> {
        let mut r = tag.to_le_bytes().to_vec();
        r.extend((payload.len() as u32).to_le_bytes());
        r.extend([0u8; 4]);
        r.extend(payload);
        r
    }

    #[test]
    fn the_snapshot_ends_at_finish_however_it_is_split() {
        let mut wire = b"GHOSTSNP\x01\x00".to_vec();
        wire.extend(record(1, b"abc"));
        wire.extend(record(7, b""));
        wire.extend(record(6, b""));
        let snapshot_len = wire.len();
        wire.extend(b"live");
        for cut in 0..wire.len() {
            let mut s = Snapshot::default();
            let (snapshot, following) = match s.push(&wire[..cut]).unwrap() {
                // Complete in the first chunk: what follows is its tail plus the bytes not pushed yet.
                Some((snap, mut tail)) => { tail.extend_from_slice(&wire[cut..]); (snap, tail) }
                None => s.push(&wire[cut..]).unwrap().expect("complete"),
            };
            assert_eq!(snapshot.len(), snapshot_len, "cut at {cut}");
            assert_eq!(following, b"live", "cut at {cut}");
        }
        assert!(Snapshot::default().push(b"NOTASNAPSHOT").is_err());
    }

    #[test]
    fn only_records_with_this_nonce_are_removed() {
        let mut m = Metadata::new(42);
        let wire = b"a\x1b_rex1;42;{\"type\":\"resize\",\"cols\":9}\x1b\\b\x1b_rex1;7;{}\x1b\\c";
        for cut in 0..wire.len() {
            let mut m2 = Metadata::new(42);
            let (mut out, mut recs) = m2.feed(&wire[..cut]).unwrap();
            let (o2, r2) = m2.feed(&wire[cut..]).unwrap();
            out.extend(o2);
            recs.extend(r2);
            assert_eq!(out, b"ab\x1b_rex1;7;{}\x1b\\c", "cut at {cut}");
            assert_eq!(recs, vec![serde_json::json!({"type": "resize", "cols": 9})], "cut at {cut}");
        }
        let (out, recs) = m.feed(b"plain \x1b_other\x1b\\ text").unwrap();
        assert_eq!((out.as_slice(), recs.len()), (&b"plain \x1b_other\x1b\\ text"[..], 0));
        let mut m = Metadata::new(42);
        let mut flood = b"\x1b_rex1;42;".to_vec();
        flood.extend(std::iter::repeat_n(b'x', MAX_METADATA + 10));
        assert!(m.feed(&flood).is_err(), "an unterminated record is bounded");
    }

    #[test]
    fn handshake_messages() {
        let h = hello("EQ-Uv6EGjp_Jul3hljGh0Q").unwrap();
        assert_eq!((h[0], h.len()), (1, 17));
        assert!(hello("short").is_err());
        let mut w = vec![0x81];
        w.extend([0u8; 23]);
        w.extend(5u64.to_le_bytes());
        assert_eq!(welcome(&w), Ok(Welcome::Accepted { nonce: 5 }));
        assert_eq!(welcome(&[0x82, 0x07]), Ok(Welcome::Rejected { reason: 7 }));
        assert!(welcome(&[0x81, 0]).is_err());
    }
}
