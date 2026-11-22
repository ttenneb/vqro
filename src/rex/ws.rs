//! A minimal WebSocket client (RFC 6455) over an already connected byte stream, for Rex's local Unix socket.
//!
//! Only what Rex's control and data channels use: one subprotocol, text and binary messages, ping/pong, close.
//! Client frames are masked; server frames must not be. Fragmented messages are reassembled. Every message is
//! bounded (`max_message`); a larger one fails the connection instead of growing memory.
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
}

pub struct Reader<R> {
    io: R,
    buf: Vec<u8>,
    max_message: usize,
    /// A fragmented message in progress (kept across an interleaved ping).
    partial: Vec<u8>,
    kind: Option<u8>,
}

pub struct Writer<W> {
    io: W,
}

fn invalid(what: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.into())
}

/// Send the HTTP upgrade for `path` with `protocol` and check the server's 101 answer. Returns the bytes that
/// followed the response headers (the start of the first frame, if any).
pub async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(io: &mut S, path: &str, protocol: &str) -> io::Result<Vec<u8>> {
    let mut key = [0u8; 16];
    getrandom::fill(&mut key).map_err(|e| io::Error::other(e.to_string()))?;
    use base64::Engine;
    let key = base64::engine::general_purpose::STANDARD.encode(key);
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: {protocol}\r\n\r\n");
    io.write_all(request.as_bytes()).await?;
    let mut head = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(i) = head.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if head.len() > 16 * 1024 {
            return Err(invalid("WebSocket upgrade response headers are too long"));
        }
        let n = io.read(&mut chunk).await?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed during the WebSocket upgrade"));
        }
        head.extend_from_slice(&chunk[..n]);
    };
    let text = String::from_utf8_lossy(&head[..end]).into_owned();
    let status = text.lines().next().unwrap_or_default();
    if status.split_whitespace().nth(1) != Some("101") {
        return Err(invalid(format!("WebSocket upgrade refused: {status}")));
    }
    let agreed = text.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("sec-websocket-protocol").then(|| v.trim().to_string())
    });
    if agreed.as_deref() != Some(protocol) {
        return Err(invalid(format!("server did not agree to subprotocol {protocol} (got {agreed:?})")));
    }
    Ok(head[end + 4..].to_vec())
}

impl<R: AsyncRead + Unpin> Reader<R> {
    pub fn new(io: R, leftover: Vec<u8>, max_message: usize) -> Self {
        Reader { io, buf: leftover, max_message, partial: Vec::new(), kind: None }
    }

    async fn need(&mut self, n: usize) -> io::Result<Vec<u8>> {
        let mut chunk = [0u8; 16 * 1024];
        while self.buf.len() < n {
            let got = self.io.read(&mut chunk).await?;
            if got == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "WebSocket connection closed"));
            }
            self.buf.extend_from_slice(&chunk[..got]);
        }
        Ok(self.buf.drain(..n).collect())
    }

    /// The next complete message. Pings are reported as `Ok(None)` with their payload in `pong` so the caller can
    /// answer on its writer; a close frame ends with `ConnectionAborted` carrying the close code and reason.
    pub async fn next(&mut self, pong: &mut Option<Vec<u8>>) -> io::Result<Option<Message>> {
        loop {
            let head = self.need(2).await?;
            let (fin, op, masked, len7) = (head[0] & 0x80 != 0, head[0] & 0x0f, head[1] & 0x80 != 0, (head[1] & 0x7f) as u64);
            if masked {
                return Err(invalid("server frame is masked"));
            }
            let len = match len7 {
                126 => u16::from_be_bytes(self.need(2).await?.try_into().unwrap()) as u64,
                127 => u64::from_be_bytes(self.need(8).await?.try_into().unwrap()),
                n => n,
            };
            if len > self.max_message as u64 || self.partial.len() as u64 + len > self.max_message as u64 {
                return Err(invalid(format!("WebSocket message over {} bytes", self.max_message)));
            }
            let payload = self.need(len as usize).await?;
            match op {
                0x8 => {
                    let code = payload.get(..2).map(|c| u16::from_be_bytes([c[0], c[1]]));
                    let reason = String::from_utf8_lossy(payload.get(2..).unwrap_or_default()).into_owned();
                    return Err(io::Error::new(io::ErrorKind::ConnectionAborted, format!("closed by server: {code:?} {reason}")));
                }
                0x9 => {
                    if len > 125 {
                        return Err(invalid("ping payload over 125 bytes"));
                    }
                    *pong = Some(payload);
                    return Ok(None);
                }
                0xA => continue,
                0x1 | 0x2 => {
                    if self.kind.is_some() {
                        return Err(invalid("new data frame inside a fragmented message"));
                    }
                    self.kind = Some(op);
                }
                0x0 => {
                    if self.kind.is_none() {
                        return Err(invalid("continuation frame without a message"));
                    }
                }
                other => return Err(invalid(format!("unknown WebSocket opcode {other}"))),
            }
            self.partial.extend_from_slice(&payload);
            if fin {
                let message = std::mem::take(&mut self.partial);
                return Ok(Some(match self.kind.take() {
                    Some(0x1) => Message::Text(String::from_utf8(message).map_err(|_| invalid("text message is not UTF-8"))?),
                    _ => Message::Binary(message),
                }));
            }
        }
    }
}

impl<W: AsyncWrite + Unpin> Writer<W> {
    pub fn new(io: W) -> Self {
        Writer { io }
    }

    async fn frame(&mut self, op: u8, payload: &[u8]) -> io::Result<()> {
        let mut mask = [0u8; 4];
        getrandom::fill(&mut mask).map_err(|e| io::Error::other(e.to_string()))?;
        let mut out = Vec::with_capacity(payload.len() + 14);
        out.push(0x80 | op);
        match payload.len() {
            n if n < 126 => out.push(0x80 | n as u8),
            n if n <= u16::MAX as usize => {
                out.push(0x80 | 126);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                out.push(0x80 | 127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        out.extend_from_slice(&mask);
        out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.io.write_all(&out).await?;
        self.io.flush().await
    }

    pub async fn text(&mut self, text: &str) -> io::Result<()> {
        self.frame(0x1, text.as_bytes()).await
    }
    pub async fn binary(&mut self, data: &[u8]) -> io::Result<()> {
        self.frame(0x2, data).await
    }
    pub async fn pong(&mut self, data: &[u8]) -> io::Result<()> {
        self.frame(0xA, data).await
    }
    /// A normal close (1000). Rex keeps a data stream for a resume unless the client closes it this way.
    pub async fn close(&mut self) -> io::Result<()> {
        self.frame(0x8, &1000u16.to_be_bytes()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_frame(fin: bool, op: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![if fin { 0x80 } else { 0 } | op];
        match payload.len() {
            n if n < 126 => out.push(n as u8),
            n if n <= 65535 => {
                out.push(126);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                out.push(127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        out.extend_from_slice(payload);
        out
    }

    #[tokio::test]
    async fn fragmented_pinged_and_large_messages_are_reassembled_and_bounded() {
        let mut wire = server_frame(false, 0x2, b"ab");
        wire.extend(server_frame(true, 0x9, b"p"));
        wire.extend(server_frame(true, 0x0, b"cd"));
        let big = vec![7u8; 70_000];
        wire.extend(server_frame(true, 0x2, &big));
        wire.extend(server_frame(true, 0x1, "héllo".as_bytes()));
        wire.extend(server_frame(true, 0x8, &[0x03, 0xe8, b'b', b'y', b'e']));
        let mut r = Reader::new(&wire[..], Vec::new(), 100_000);
        let mut pong = None;
        // A ping between fragments (RFC 6455 5.4) is reported for answering; the message in progress survives.
        assert_eq!(r.next(&mut pong).await.unwrap(), None);
        assert_eq!(pong.take(), Some(b"p".to_vec()));
        assert_eq!(r.next(&mut pong).await.unwrap(), Some(Message::Binary(b"abcd".to_vec())));
        assert_eq!(r.next(&mut pong).await.unwrap(), Some(Message::Binary(big.clone())));
        assert_eq!(r.next(&mut pong).await.unwrap(), Some(Message::Text("héllo".into())));
        let closed = r.next(&mut pong).await.unwrap_err();
        assert!(closed.to_string().contains("1000") && closed.to_string().contains("bye"), "{closed}");
        let over = server_frame(true, 0x2, &big);
        let mut r = Reader::new(&over[..], Vec::new(), 1000);
        assert!(r.next(&mut pong).await.is_err(), "over the message bound");
    }

    #[tokio::test]
    async fn client_frames_are_masked() {
        let mut out = Vec::new();
        Writer::new(&mut out).text("hi").await.unwrap();
        assert_eq!(out[0], 0x81);
        assert_eq!(out[1], 0x80 | 2);
        let mask = &out[2..6];
        assert_eq!([out[6] ^ mask[0], out[7] ^ mask[1]], *b"hi");
    }
}
