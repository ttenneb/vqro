use super::*;

/// How long the rendered client waits for the running server's protocol before
/// giving up silently. A missing, busy, or slow server must never delay startup.
const PROTOCOL_PROBE_TIMEOUT: Duration = Duration::from_millis(300);

/// Runs the thin client and enters the main event loop.
pub fn run_client() -> io::Result<()> {
    run_client_with_mode(None, None, "connecting to server")
}

/// The one-line notice for the rendered client when the already-running server
/// is an older build or protocol than this binary, which happens when the binary
/// was upgraded while a server kept running. `None` when it is the same build or
/// when no server answers in time. Restarting stays the human's decision: this
/// client never restarts or stops a server by itself.
pub(super) fn stale_server_notice() -> Option<String> {
    let status = running_server_status()?;
    let client_version = crate::build_info::version();
    let notice = super::errors::server_restart_notice(
        &client_version,
        &status.version,
        crate::protocol::PROTOCOL_VERSION,
        status.protocol,
    )?;
    info!(
        client_version = %client_version,
        server_version = %status.version,
        client_protocol = crate::protocol::PROTOCOL_VERSION,
        server_protocol = status.protocol,
        "running server is an older build"
    );
    Some(notice)
}

/// The running server's own build identity, or `None` when no server answers in
/// time. Both facts come from the existing ping status; nothing new is exposed.
struct RunningServerStatus {
    version: String,
    protocol: u32,
}

fn running_server_status() -> Option<RunningServerStatus> {
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
    let result = response.get("result")?;
    Some(RunningServerStatus {
        version: result.get("version")?.as_str()?.to_owned(),
        protocol: u32::try_from(result.get("protocol")?.as_u64()?).ok()?,
    })
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
