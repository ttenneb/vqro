//! The core's opaque local endpoint. Wire users need only ordered asynchronous byte streams.

use std::io;
use std::path::Path;

#[path = "transport_endpoint.rs"]
mod endpoint;

/// Validate a Windows core endpoint without opening it or normalizing its original spelling.
pub fn validate_windows_endpoint(endpoint: &Path) -> io::Result<()> {
    endpoint::validate(endpoint)
}

#[cfg(any(windows, test))]
#[path = "transport_auth.rs"]
mod auth;
#[cfg(windows)]
#[path = "transport_auth_windows.rs"]
mod windows_auth;

/// Namespace or connected-handle verification refused the endpoint. Never auto-start past this refusal.
pub fn is_server_verification_error(error: &io::Error) -> bool {
    if endpoint::is_refusal(error) {
        return true;
    }
    #[cfg(any(windows, test))]
    {
        auth::is_refusal(error)
    }
    #[cfg(not(any(windows, test)))]
    {
        let _ = error;
        false
    }
}

#[cfg(unix)]
pub type ClientStream = tokio::net::UnixStream;
#[cfg(unix)]
pub type ServerStream = tokio::net::UnixStream;
#[cfg(unix)]
pub type SyncStream = std::os::unix::net::UnixStream;
#[cfg(windows)]
pub type ClientStream = tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(windows)]
pub type ServerStream = tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(windows)]
pub type SyncStream = std::fs::File;

pub struct Listener {
    #[cfg(unix)]
    inner: tokio::net::UnixListener,
    #[cfg(windows)]
    inner: ServerStream,
    #[cfg(windows)]
    endpoint: std::path::PathBuf,
}

impl Listener {
    /// Unix retains the stale-socket probe and 0600 mode. Windows refuses an existing first instance.
    pub fn bind(endpoint: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if endpoint.exists() {
                if SyncStream::connect(endpoint).is_ok() {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!("vqro is already running at {}", endpoint.display()),
                    ));
                }
                std::fs::remove_file(endpoint)?;
            }
            let inner = tokio::net::UnixListener::bind(endpoint)?;
            std::fs::set_permissions(endpoint, std::fs::Permissions::from_mode(0o600))?;
            Ok(Self { inner })
        }
        #[cfg(windows)]
        {
            let inner = endpoint::open(endpoint, |name| {
                windows::instance(name, true).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "cannot bind core pipe {} (another core may already be running): {error}",
                        name.display()
                    ),
                )
            })
            })?;
            Ok(Self {
                inner,
                endpoint: endpoint.into(),
            })
        }
    }

    pub async fn accept(&mut self) -> io::Result<ServerStream> {
        #[cfg(unix)]
        {
            self.inner.accept().await.map(|(stream, _)| stream)
        }
        #[cfg(windows)]
        loop {
            let connected = match self.inner.connect().await {
                Ok(()) => true,
                // A reachability probe may close before ConnectNamedPipe runs (ERROR_NO_DATA).
                // Retire that instance, not the listener; no request was admitted or replayed.
                Err(error) if windows::client_gone(&error) => false,
                Err(error) => return Err(error),
            };
            // Keep an instance available before handing the connected one to the serving task.
            // Cancellation while waiting for connect leaves this same instance in the listener.
            let next = windows::instance(&self.endpoint, false)?;
            let previous = std::mem::replace(&mut self.inner, next);
            if connected {
                return Ok(previous);
            }
        }
    }
}

pub async fn connect(endpoint: impl AsRef<Path>) -> io::Result<ClientStream> {
    #[cfg(unix)]
    {
        ClientStream::connect(endpoint).await
    }
    #[cfg(windows)]
    {
        let deadline = tokio::time::Instant::now() + windows::BUSY_WAIT;
        loop {
            // Tokio 1.54.1 defaults to SECURITY_IDENTIFICATION | SECURITY_SQOS_PRESENT;
            // preserve that ceiling so even a rejected foreign server cannot impersonate this client.
            match endpoint::open(endpoint.as_ref(), |name| {
                tokio::net::windows::named_pipe::ClientOptions::new().open(name)
            }) {
                Ok(stream) => return windows_auth::authenticate(stream, endpoint.as_ref()),
                Err(error) if windows::busy(&error) && tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(windows::BUSY_INTERVAL).await;
                }
                result => return result,
            }
        }
    }
}

/// Strict request/reply users only: synchronous Windows pipe handles serialize reads and writes.
pub fn connect_sync(endpoint: impl AsRef<Path>) -> io::Result<SyncStream> {
    #[cfg(unix)]
    {
        SyncStream::connect(endpoint)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;
        let deadline = std::time::Instant::now() + windows::BUSY_WAIT;
        loop {
            match endpoint::open(endpoint.as_ref(), |name| {
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    // OpenOptionsExt adds SECURITY_SQOS_PRESENT; match Tokio's identification ceiling.
                    .security_qos_flags(SECURITY_IDENTIFICATION)
                    .open(name)
            }) {
                Ok(stream) => return windows_auth::authenticate(stream, endpoint.as_ref()),
                Err(error) if windows::busy(&error) && std::time::Instant::now() < deadline => {
                    std::thread::sleep(windows::BUSY_INTERVAL);
                }
                result => return result,
            }
        }
    }
}

/// Endpoint cleanup is a filesystem operation only on Unix. Pipe lifetime belongs to its handles.
pub fn unlink(endpoint: &Path) {
    #[cfg(unix)]
    {
        let _ = std::fs::remove_file(endpoint);
    }
    #[cfg(windows)]
    {
        let _ = endpoint;
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::ptr;
    use tokio::net::windows::named_pipe::ServerOptions;
    use windows_sys::Win32::{
        Foundation::{ERROR_PIPE_BUSY, LocalFree},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            },
            SECURITY_ATTRIBUTES,
        },
    };

    pub const BUSY_WAIT: std::time::Duration = std::time::Duration::from_millis(500);
    pub const BUSY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);
    pub fn busy(error: &io::Error) -> bool {
        error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32)
    }
    pub fn client_gone(error: &io::Error) -> bool {
        use windows_sys::Win32::Foundation::{
            ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED,
        };
        error.raw_os_error().is_some_and(|code| {
            matches!(
                code as u32,
                ERROR_BROKEN_PIPE | ERROR_NO_DATA | ERROR_PIPE_NOT_CONNECTED
            )
        })
    }

    // An explicit owner and protected DACL with exactly TokenUser, not Administrators or Everyone.
    // Set O: too: an elevated token's default owner can be a group, which clients must reject.
    // Microsoft: /windows/win32/ipc/named-pipe-security-and-access-rights;
    // /windows/win32/secauthz/security-descriptor-string-format;
    // /windows/win32/api/securitybaseapi/nf-securitybaseapi-gettokeninformation.
    pub(super) fn user_sddl() -> io::Result<Vec<u16>> {
        let user = windows_auth::User::current()?;
        // SAFETY: the User owns the valid SID; Windows owns the converted SID string until LocalFree.
        unsafe {
            let mut sid = ptr::null_mut();
            if ConvertSidToStringSidW(user.sid(), &mut sid) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut len = 0;
            while *sid.add(len) != 0 {
                len += 1;
            }
            let sid_text = String::from_utf16_lossy(std::slice::from_raw_parts(sid, len));
            LocalFree(sid.cast());
            Ok(auth::server_security(&sid_text)
                .encode_utf16()
                .chain(Some(0))
                .collect())
        }
    }

    pub fn instance(endpoint: &Path, first: bool) -> io::Result<ServerStream> {
        let sddl = user_sddl()?;
        // SAFETY: ConvertString allocates a valid descriptor; both it and SECURITY_ATTRIBUTES
        // remain alive for CreateNamedPipeW. The pipe copies its security descriptor, then we free it.
        unsafe {
            let mut descriptor = ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let mut attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            let result = ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(
                    endpoint,
                    (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
                );
            LocalFree(descriptor);
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn endpoint(dir: &Path) -> std::path::PathBuf {
        #[cfg(unix)]
        {
            dir.join("core.sock")
        }
        #[cfg(windows)]
        {
            let _ = dir;
            format!(
                r"\\.\pipe\vqro-transport-test-{}-{}",
                std::process::id(),
                getrandom::u64().unwrap()
            )
            .into()
        }
    }

    #[tokio::test]
    async fn bind_connect_two_clients_and_refuse_second_server() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = endpoint(dir.path());
        let mut listener = Listener::bind(&endpoint).unwrap();
        let mut live_connections = Vec::new();
        for byte in [17, 42] {
            let mut client = connect(&endpoint).await.unwrap();
            let mut server = listener.accept().await.unwrap();
            client.write_u8(byte).await.unwrap();
            assert_eq!(server.read_u8().await.unwrap(), byte);
            server.write_u8(byte + 1).await.unwrap();
            assert_eq!(client.read_u8().await.unwrap(), byte + 1);
            live_connections.push((client, server));
        }
        // Both connections stay open and remain independent.
        for (mut client, mut server) in live_connections {
            client.write_u8(99).await.unwrap();
            assert_eq!(server.read_u8().await.unwrap(), 99);
        }
        assert!(Listener::bind(&endpoint).is_err());
    }

    #[tokio::test]
    async fn sync_request_reply() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = endpoint(dir.path());
        let mut listener = Listener::bind(&endpoint).unwrap();
        let client = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};
            let mut stream = connect_sync(endpoint).unwrap();
            stream.write_all(&[31]).unwrap();
            let mut reply = [0];
            stream.read_exact(&mut reply).unwrap();
            assert_eq!(reply, [32]);
        });
        let mut server = listener.accept().await.unwrap();
        assert_eq!(server.read_u8().await.unwrap(), 31);
        server.write_u8(32).await.unwrap();
        client.await.unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn busy_pipe_retry_is_bounded_and_accept_keeps_next_instance_available() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = endpoint(dir.path());
        let mut listener = Listener::bind(&endpoint).unwrap();
        let _first = connect(&endpoint).await.unwrap();
        // The only instance is connected but not yet accepted: ERROR_PIPE_BUSY, not a replay.
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), connect(&endpoint))
            .await
            .unwrap();
        assert!(windows::busy(&result.unwrap_err()));
        let _server = listener.accept().await.unwrap();
        // accept created the next instance before returning; no separate accept task is needed.
        let _second = connect(&endpoint).await.unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn disconnected_probe_does_not_end_the_listener() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = endpoint(dir.path());
        let mut listener = Listener::bind(&endpoint).unwrap();
        drop(connect(&endpoint).await.unwrap());
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let client = tokio::spawn(async move {
                let mut client = connect(endpoint).await.unwrap();
                client.write_u8(19).await.unwrap();
                assert_eq!(client.read_u8().await.unwrap(), 20);
            });
            loop {
                let mut server = listener.accept().await.unwrap();
                match server.read_u8().await {
                    Ok(byte) => {
                        assert_eq!(byte, 19);
                        server.write_u8(20).await.unwrap();
                        break;
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
                        ) => {}
                    Err(error) => panic!("probe retirement failed: {error}"),
                }
            }
            client.await.unwrap();
        })
        .await
        .unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn pipe_owner_and_single_dacl_ace_are_the_starting_user() {
        use std::os::windows::io::AsRawHandle;
        use std::ptr;
        use windows_sys::Win32::{
            Foundation::LocalFree,
            Security::{
                Authorization::{
                    ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo,
                    SE_KERNEL_OBJECT,
                },
                DACL_SECURITY_INFORMATION, EqualSid, OWNER_SECURITY_INFORMATION,
            },
        };
        let dir = tempfile::tempdir().unwrap();
        let endpoint = endpoint(dir.path());
        let listener = Listener::bind(&endpoint).unwrap();
        // SAFETY: the live server owns the handle. GetSecurityInfo allocates the descriptor,
        // which owns the DACL; both allocated Windows strings/descriptors are freed below.
        unsafe {
            let mut dacl = ptr::null_mut();
            let mut owner = ptr::null_mut();
            let mut descriptor = ptr::null_mut();
            assert_eq!(
                GetSecurityInfo(
                    listener.inner.as_raw_handle().cast(),
                    SE_KERNEL_OBJECT,
                    DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
                    &mut owner,
                    ptr::null_mut(),
                    &mut dacl,
                    ptr::null_mut(),
                    &mut descriptor
                ),
                0
            );
            assert!(!owner.is_null());
            let user = windows_auth::User::current().unwrap();
            assert_ne!(
                EqualSid(owner, user.sid()),
                0,
                "owner must be TokenUser, never token default owner/group"
            );
            assert!(!dacl.is_null(), "a null DACL would admit everyone");
            assert_eq!((*dacl).AceCount, 1);
            let mut text = ptr::null_mut();
            assert_ne!(
                ConvertSecurityDescriptorToStringSecurityDescriptorW(
                    descriptor,
                    1,
                    DACL_SECURITY_INFORMATION,
                    &mut text,
                    ptr::null_mut()
                ),
                0
            );
            let mut len = 0;
            while *text.add(len) != 0 {
                len += 1;
            }
            let actual = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
            LocalFree(text.cast());
            LocalFree(descriptor);
            let expected = windows::user_sddl().unwrap();
            let expected = String::from_utf16_lossy(&expected[..expected.len() - 1]);
            let sid = expected.split(";;;").nth(1).unwrap();
            assert!(actual.contains("(A;"), "{actual}");
            assert!(actual.ends_with(sid), "{actual} should admit only {sid}");
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cancelled_accept_keeps_listener_usable() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = endpoint(dir.path());
        let mut listener = Listener::bind(&endpoint).unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
        let mut client = connect(&endpoint).await.unwrap();
        let mut server = listener.accept().await.unwrap();
        client.write_u8(7).await.unwrap();
        assert_eq!(server.read_u8().await.unwrap(), 7);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_socket_replaced_and_mode_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let endpoint = endpoint(dir.path());
        drop(tokio::net::UnixListener::bind(&endpoint).unwrap());
        let _listener = Listener::bind(&endpoint).unwrap();
        assert_eq!(
            std::fs::metadata(endpoint).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
