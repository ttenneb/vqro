use super::*;

/// How long the rendered client waits for the running server's protocol before
/// giving up silently. A missing, busy, or slow server must never delay startup.
const PROTOCOL_PROBE_TIMEOUT: Duration = Duration::from_millis(300);

/// Runs the thin client and enters the main event loop.
pub fn run_client() -> io::Result<()> {
    run_client_with_mode(None, None, "connecting to server")
}

/// Offers a restart when the already-running server predates this client, which
/// happens when the binary was upgraded while a server kept running. This only
/// reports the fact and names the exact command: restarting stays the human's
/// decision, and this client never restarts or stops a server by itself.
pub(super) fn offer_server_restart_if_older() {
    let Some(server_protocol) = running_server_protocol() else {
        return;
    };
    let client_protocol = crate::protocol::PROTOCOL_VERSION;
    let Some(offer) = super::errors::older_server_message(client_protocol, server_protocol) else {
        return;
    };
    warn!(client_protocol, server_protocol, "running server is older");
    eprintln!("{offer}");
}

/// The running server's protocol, or `None` when no server answers in time.
fn running_server_protocol() -> Option<u32> {
    use crate::api::client::ApiClient;
    use crate::api::schema::{Method, PingParams, Request};

    let client = ApiClient::local();
    let request = Request {
        id: "client:protocol-probe".to_owned(),
        method: Method::Ping(PingParams::default()),
    };
    let response = client
        .request_value_with_timeout(&request, PROTOCOL_PROBE_TIMEOUT)
        .ok()?;
    response
        .get("result")
        .and_then(|result| result.get("protocol"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|protocol| u32::try_from(protocol).ok())
}

#[cfg(unix)]
pub fn run_terminal_attach(terminal_id: String, takeover: bool) -> io::Result<()> {
    run_client_with_mode(
        Some((terminal_id, takeover)),
        Some(AttachEscapeState::default()),
        "attaching to terminal",
    )
}

#[cfg(windows)]
pub fn run_terminal_attach(_terminal_id: String, _takeover: bool) -> io::Result<()> {
    debug_assert!(!crate::platform::capabilities().direct_terminal_attach);
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "direct terminal attach is not supported on Windows yet",
    ))
}
