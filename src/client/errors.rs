use std::io;

use crate::protocol;
use crate::server::socket_paths::client_socket_path;

/// Errors that can occur during client operation.
#[derive(Debug)]
pub enum ClientError {
    /// Could not connect to the server's client socket.
    ConnectionFailed(io::Error),
    /// Server rejected our handshake.
    HandshakeRejected { version: u32, error: String },
    /// Server shut down.
    ServerShutdown { reason: Option<String> },
    /// Lost connection to the server.
    ConnectionLost(io::Error),
    /// Protocol error (framing, deserialization).
    Protocol(protocol::FramingError),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::ConnectionFailed(err) => {
                write!(f, "failed to connect to server: {err}")?;
                let path = client_socket_path();
                write!(
                    f,
                    "\nIs herdr server running? Start it with `herdr server`."
                )?;
                write!(f, "\nSocket path: {}", path.display())
            }
            ClientError::HandshakeRejected { version, error } => {
                write!(f, "server rejected handshake (version {version}): {error}")
            }
            ClientError::ServerShutdown { reason } => {
                match reason.as_deref() {
                    Some("detached") => {
                        if let Ok(reattach_command) =
                            std::env::var(crate::remote::REATTACH_COMMAND_ENV_VAR)
                        {
                            write!(f, "detached from remote server")?;
                            write!(f, "\nRun `{reattach_command}` to reattach")?;
                        } else {
                            write!(f, "detached from server")?;
                            write!(
                                f,
                                "\nRun `{}` to reattach",
                                crate::session::local_attach_command()
                            )?;
                        }
                    }
                    _ => {
                        write!(f, "server shut down")?;
                        if let Some(reason) = reason {
                            write!(f, ": {reason}")?;
                        }
                    }
                }
                Ok(())
            }
            ClientError::ConnectionLost(err) => {
                if let Ok(reattach_command) = std::env::var(crate::remote::REATTACH_COMMAND_ENV_VAR)
                {
                    write!(f, "lost connection to remote vqro: {err}")?;
                    write!(f, "\nIf the remote server survived the SSH or network drop, its panes may still be running.")?;
                    write!(f, "\nRun `{reattach_command}` to reattach")
                } else {
                    write!(f, "lost connection to server: {err}")
                }
            }
            ClientError::Protocol(err) => write!(f, "protocol error: {err}"),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClientError::ConnectionFailed(err) => Some(err),
            ClientError::ConnectionLost(err) => Some(err),
            ClientError::Protocol(err) => Some(err),
            _ => None,
        }
    }
}

impl From<protocol::FramingError> for ClientError {
    fn from(err: protocol::FramingError) -> Self {
        match err {
            protocol::FramingError::UnexpectedEof => ClientError::ConnectionLost(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "server closed connection",
            )),
            protocol::FramingError::Io(err) => ClientError::ConnectionLost(err),
            err => ClientError::Protocol(err),
        }
    }
}

/// The one-line notice the rendered client shows for a running server that is an
/// older build or protocol than this client. `None` when they are the same build.
/// The restart stays the human's action: this client never restarts or stops a
/// server by itself.
pub(super) fn server_restart_notice(
    client_version: &str,
    server_version: &str,
    client_protocol: u32,
    server_protocol: u32,
) -> Option<String> {
    let server = crate::product::DISPLAY_NAME;
    let what = if server_protocol < client_protocol {
        format!(
            "the running {server} server speaks an older protocol (server {server_protocol}, client {client_protocol})"
        )
    } else if server_version != client_version {
        format!(
            "the running {server} server is a different build (server {server_version}, client {client_version})"
        )
    } else {
        return None;
    };
    Some(format!(
        "{what}\nrestart it yourself: `{}`; stopping it exits its pane processes, and this client never restarts it.",
        crate::session::stop_command_with_socket_override(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_different_build_is_reported_even_when_the_protocol_matches() {
        let notice = server_restart_notice("0.9.0", "0.9.0-dev.561da0e3", 23, 23)
            .expect("different build notice");
        assert!(
            notice.contains("the running vqro server is a different build"),
            "{notice}"
        );
        assert!(notice.contains("0.9.0-dev.561da0e3"), "{notice}");
        assert!(notice.contains("0.9.0"), "{notice}");
        assert!(notice.contains("server stop"), "{notice}");
        assert!(notice.contains("never restarts it"), "{notice}");
        assert!(notice.contains("exits its pane processes"), "{notice}");
    }

    #[test]
    fn an_older_protocol_is_reported_with_the_command() {
        let notice = server_restart_notice("0.9.0", "0.9.0", 23, 22).expect("older protocol");
        assert!(
            notice.contains("the running vqro server speaks an older protocol"),
            "{notice}"
        );
        assert!(notice.contains("server 22, client 23"), "{notice}");
        assert!(
            notice.contains(&crate::session::stop_command_with_socket_override()),
            "{notice}"
        );
        assert!(notice.contains("never restarts it"), "{notice}");
    }

    #[test]
    fn the_same_build_needs_no_notice() {
        assert!(server_restart_notice("0.9.0", "0.9.0", 23, 23).is_none());
    }
}
