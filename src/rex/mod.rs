//! Rex (Superlogical's terminal multiplexer) client pieces for the `vqro-rex` provider: no provider protocol or
//! core dependency here. Rex is proprietary alpha software; this speaks only to its local server socket.
//! `docs/research/rex.md` records what is documented by Rex and what was observed.
pub mod control;
pub mod data;
pub mod ws;
pub mod transport;

use std::path::Path;
use std::time::Duration;

use serde_json::json;

pub use control::{Control, Error, Event};

/// An open terminal data channel: Rex's VT stream for one block, after the snapshot.
pub struct DataChannel {
    pub reader: ws::Reader<transport::ReadHalf>,
    pub writer: ws::Writer<transport::WriteHalf>,
    pub nonce: u64,
}

/// Get a ticket on `control` (which must be attached to `session`) and open the data channel for `block`.
pub async fn open_data(socket: &Path, control: &Control, session: &str, block: &str, timeout: Duration) -> Result<DataChannel, String> {
    let ticket = control.call("data.connect", json!({"session_id": session, "block_id": block, "data_ref_id": "terminal",
        "subprotocol": data::PROTOCOL})).await.map_err(|e| e.to_string())?;
    let ticket = ticket["ticket"].as_str().ok_or("data.connect returned no ticket")?.to_string();
    let hello = data::hello(&ticket)?;
    tokio::time::timeout(timeout, async {
        let mut stream = transport::connect(socket).await.map_err(|e| e.to_string())?;
        let leftover = ws::handshake(&mut stream, "/api/data", data::PROTOCOL).await.map_err(|e| e.to_string())?;
        let (read, write) = tokio::io::split(stream);
        let mut reader = ws::Reader::new(read, leftover, data::MAX_SNAPSHOT);
        let mut writer = ws::Writer::new(write);
        writer.binary(&hello).await.map_err(|e| e.to_string())?;
        let mut pong = None;
        let answer = loop {
            match reader.next(&mut pong).await.map_err(|e| e.to_string())? {
                Some(ws::Message::Binary(b)) => break b,
                Some(ws::Message::Text(_)) => return Err("text message on the data channel".to_string()),
                None => {
                    if let Some(p) = pong.take() {
                        writer.pong(&p).await.map_err(|e| e.to_string())?;
                    }
                }
            }
        };
        match data::welcome(&answer)? {
            data::Welcome::Accepted { nonce } => Ok(DataChannel { reader, writer, nonce }),
            data::Welcome::Rejected { reason } => Err(format!("rex refused the data connection (reason {reason:#04x})")),
        }
    }).await.map_err(|_| "rex data handshake timed out".to_string())?
}
