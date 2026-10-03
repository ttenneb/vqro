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

/// The offer shown when the running server is older than this client, or `None`
/// when it is not older. The restart stays the human's action: this client never
/// restarts or stops a server by itself.
pub(super) fn older_server_message(client_protocol: u32, server_protocol: u32) -> Option<String> {
    if server_protocol >= client_protocol {
        return None;
    }
    Some(format!(
        "the running {} server is older than this client (server protocol {server_protocol}, client protocol {client_protocol}).\n\
         Restart it yourself when you choose; this client never restarts or stops a server by itself.\n\
         {}",
        crate::product::DISPLAY_NAME,
        crate::session::active_restart_after_update_guidance(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_older_server_message_names_the_command_and_refuses_to_restart() {
        let message = older_server_message(23, 22).expect("older server message");
        assert!(message.contains("is older than this client"), "{message}");
        assert!(message.contains("server protocol 22"), "{message}");
        assert!(message.contains("client protocol 23"), "{message}");
        assert!(
            message.contains("never restarts or stops a server by itself"),
            "{message}"
        );
        assert!(
            message.contains(&crate::session::active_restart_after_update_guidance()),
            "{message}"
        );
    }

    #[test]
    fn a_matching_or_newer_server_keeps_whatever_the_server_sent() {
        assert!(older_server_message(23, 23).is_none());
        assert!(older_server_message(23, 24).is_none());
        assert!(older_server_message(22, 23).is_none());
    }
}
