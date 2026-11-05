//! Bounded explicit ownership-status probes. No App actor, render or provider work.

use std::io::{self, Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Instant;

use interprocess::local_socket::traits::Stream as _;

use crate::pty_ownership::{OwnershipStatus, ServerKey, ServerLiveness};

pub(crate) fn ownership_server_liveness(key: &ServerKey, boot_id: &str) -> ServerLiveness {
    if key.boot_id != boot_id {
        return ServerLiveness::Reused;
    }
    if let Some(birth) = crate::platform::invocation_process_birth(key.pid) {
        return if birth == key.birth {
            ServerLiveness::Live(key.clone())
        } else {
            ServerLiveness::Reused
        };
    }
    let Ok(pid) = i32::try_from(key.pid) else {
        return ServerLiveness::Unknown;
    };
    // SAFETY: signal zero performs a liveness/permission query, not a signal.
    if unsafe { libc::kill(pid, 0) } == -1
        && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    {
        ServerLiveness::Dead
    } else {
        ServerLiveness::Unknown
    }
}

fn remaining(deadline: Instant) -> io::Result<std::time::Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "ownership probe deadline expired"))
}

fn connect(path: &Path, deadline: Instant) -> io::Result<crate::ipc::LocalStream> {
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: sockaddr_un is a plain C structure; every byte starts initialized.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.is_empty() || bytes.contains(&0) || bytes.len() >= address.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid ownership endpoint path",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (to, from) in address.sun_path.iter_mut().zip(bytes) {
        *to = *from as libc::c_char;
    }
    #[cfg(target_os = "macos")]
    {
        address.sun_len = std::mem::size_of::<libc::sockaddr_un>() as u8;
    }
    // SAFETY: socket creates a new descriptor, owned immediately below.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: fcntl changes flags only on our newly owned descriptor.
    if unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) } < 0
        || unsafe { libc::fcntl(raw, libc::F_SETFL, libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    loop {
        remaining(deadline)?;
        // SAFETY: address is initialized and the supplied length is its actual size.
        let result = unsafe {
            libc::connect(
                raw,
                (&address as *const libc::sockaddr_un).cast(),
                std::mem::size_of_val(&address) as libc::socklen_t,
            )
        };
        if result == 0 {
            break;
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EISCONN) => break,
            Some(libc::EINTR) => continue,
            Some(libc::EINPROGRESS) | Some(libc::EAGAIN) | Some(libc::EALREADY) => {
                let mut poll = libc::pollfd {
                    fd: raw,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let milliseconds =
                    remaining(deadline)?.as_millis().clamp(1, i32::MAX as u128) as i32;
                // SAFETY: poll receives one writable pollfd of the stated count.
                let ready = unsafe { libc::poll(&mut poll, 1, milliseconds) };
                if ready == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "ownership endpoint connect timed out",
                    ));
                }
                if ready < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    return Err(io::Error::last_os_error());
                }
                if error.raw_os_error() == Some(libc::EAGAIN) {
                    std::thread::sleep(
                        remaining(deadline)?.min(std::time::Duration::from_millis(2)),
                    );
                }
            }
            _ => return Err(error),
        }
    }
    let stream = crate::ipc::LocalStream::UdSocket(fd.into());
    stream.set_nonblocking(false)?;
    stream.set_recv_timeout(Some(remaining(deadline)?))?;
    stream.set_send_timeout(Some(remaining(deadline)?))?;
    Ok(stream)
}

pub(crate) fn ownership_status_at(
    endpoint: &Path,
    expected: &ServerKey,
    deadline: Instant,
) -> Result<OwnershipStatus, String> {
    let mut stream = connect(endpoint, deadline).map_err(|error| error.to_string())?;
    let before = crate::platform::invocation_peer(&stream)
        .ok_or("ownership endpoint kernel peer is unprovable")?;
    let first = before
        .ancestry
        .first()
        .ok_or("missing ownership endpoint peer")?;
    let peer = ServerKey {
        boot_id: before.boot_id.clone(),
        pid: first.pid,
        birth: first.birth,
    };
    if &peer != expected {
        return Err("ownership endpoint belongs to another incarnation".into());
    }
    stream
        .set_send_timeout(Some(
            remaining(deadline).map_err(|error| error.to_string())?,
        ))
        .map_err(|error| error.to_string())?;
    stream.write_all(b"{\"id\":\"ownership-proof\",\"method\":\"server.pty_ownership.status.v1\",\"params\":{}}\n").map_err(|error| error.to_string())?;
    stream.flush().map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    loop {
        if bytes.len() >= 8192 {
            return Err("ownership status exceeds response bound".into());
        }
        stream
            .set_recv_timeout(Some(
                remaining(deadline).map_err(|error| error.to_string())?,
            ))
            .map_err(|error| error.to_string())?;
        let mut chunk = [0_u8; 1024];
        let read = stream.read(&mut chunk).map_err(|error| error.to_string())?;
        if read == 0 {
            return Err("ownership status response was lost".into());
        }
        let newline = chunk[..read].iter().position(|byte| *byte == b'\n');
        let used = newline.unwrap_or(read);
        if bytes.len() + used > 8192 {
            return Err("ownership status exceeds response bound".into());
        }
        bytes.extend_from_slice(&chunk[..used]);
        if newline.is_some() {
            break;
        }
    }
    if crate::platform::socket_peer_birth(&stream) != Some((expected.pid, expected.birth))
        || !crate::platform::invocation_peer_is_current(&before)
    {
        return Err("ownership endpoint incarnation changed".into());
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if value["id"] != "ownership-proof" || value["result"]["type"] != "pty_ownership_status" {
        return Err("ownership status method is unavailable".into());
    }
    let status: OwnershipStatus = serde_json::from_value(value["result"]["status"].clone())
        .map_err(|error| error.to_string())?;
    if &status.server != expected {
        return Err("ownership status caller identity mismatch".into());
    }
    Ok(status)
}
