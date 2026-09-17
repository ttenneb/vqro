#[cfg(unix)]
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Child, Command};
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use tracing::{info, warn};

#[cfg(unix)]
const HANDOFF_VERSION: u32 = 2;
#[cfg(unix)]
const LEGACY_HANDOFF_VERSION: u32 = 1;
#[cfg(unix)]
const MAX_HANDOFF_ENVELOPE_BYTES: usize = 16 * 1024 * 1024;
#[cfg(unix)]
const READY_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(unix)]
const OWNED_ACK_TIMEOUT: Duration = Duration::from_millis(500);
// Descriptors are transferred in batches of this size. A single SCM_RIGHTS
// control message caps out at 253 descriptors on Linux and 254 on macOS, so the
// batch stays well below both limits and the number of panes stays unbounded.
#[cfg(unix)]
const FDS_PER_MESSAGE: usize = 64;
#[cfg(unix)]
pub(crate) const MAX_REPLAY_BYTES_PER_PANE: usize = 8 * 1024;
#[cfg(unix)]
pub(crate) const COMMIT_TIMEOUT: Duration = READY_TIMEOUT;

#[cfg(unix)]
#[derive(Serialize, Deserialize)]
pub(crate) struct HandoffManifest {
    pub version: u32,
    pub source_version: String,
    pub source_protocol: u32,
    pub expected_version: Option<String>,
    pub expected_protocol: Option<u32>,
    pub snapshot: crate::persist::SessionSnapshot,
    pub panes: Vec<crate::handoff_runtime::HandoffRuntimeState>,
    /// Snapshot panes intentionally lacking an FD because native agent resume
    /// is deferred until the replacement has a usable host terminal.
    #[serde(default)]
    pub deferred_panes: Vec<u32>,
    /// An outer window title set over the API outlives the server that took the
    /// call, so a handoff carries it rather than falling back to the config.
    /// Absent from manifests written before this field existed.
    #[serde(default)]
    pub api_window_title: Option<String>,
}

#[cfg(unix)]
pub(crate) struct ReceivedHandoff {
    pub manifest: HandoffManifest,
    /// Owned until each descriptor is moved into an imported runtime. Any
    /// early-return path closes the remainder automatically.
    pub fds: Vec<OwnedFd>,
    pub stream: UnixStream,
}

#[cfg(unix)]
pub(crate) fn handoff_socket_path() -> PathBuf {
    crate::session::data_dir().join(format!("vqro-handoff-{}.sock", std::process::id()))
}

#[cfg(unix)]
pub(crate) fn spawn_handoff_import(
    import_exe: Option<&Path>,
    socket_path: &Path,
    token: &str,
) -> io::Result<Child> {
    let fallback_exe;
    let exe = if let Some(import_exe) = import_exe {
        import_exe
    } else {
        fallback_exe = std::env::current_exe().map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("failed to determine herdr executable path: {err}"),
            )
        })?;
        &fallback_exe
    };
    let mut command = Command::new(exe);
    command
        .arg("server")
        .arg("--handoff-import")
        .arg(socket_path)
        .arg(token)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if crate::session::explicit_session_requested() {
        // The import child no longer has the original `--session` argument, so
        // stale socket overrides must not mask the inherited HERDR_SESSION.
        command
            .env_remove(crate::api::SOCKET_PATH_ENV_VAR)
            .env_remove(crate::server::socket_paths::CLIENT_SOCKET_PATH_ENV_VAR);
    }
    crate::platform::detach_server_daemon_command(&mut command);
    command.spawn().map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "failed to spawn handoff import server at {}: {err}",
                exe.display()
            ),
        )
    })
}

#[cfg(unix)]
pub(crate) fn cleanup_failed_import_child(child: &mut Child) {
    let pid = child.id();
    match child.try_wait() {
        Ok(Some(status)) => {
            info!(pid, status = %status, "handoff import server exited during rollback");
            return;
        }
        Ok(None) => {}
        Err(err) => {
            warn!(pid, err = %err, "failed to inspect handoff import server before rollback");
        }
    }

    if let Err(err) = child.kill() {
        warn!(pid, err = %err, "failed to kill handoff import server during rollback");
    }
    match child.wait() {
        Ok(status) => {
            info!(pid, status = %status, "handoff import server reaped during rollback");
        }
        Err(err) => {
            warn!(pid, err = %err, "failed to reap handoff import server during rollback");
        }
    }
}

#[cfg(unix)]
pub(crate) fn bind_listener(socket_path: &Path) -> io::Result<UnixListener> {
    let _ = std::fs::remove_file(socket_path);
    let listener = UnixListener::bind(socket_path)?;
    listener.set_nonblocking(true)?;
    restrict_socket_permissions(socket_path)?;
    Ok(listener)
}

#[cfg(unix)]
pub(crate) fn accept_and_validate_on(
    listener: UnixListener,
    socket_path: &Path,
    token: &str,
    manifest: &HandoffManifest,
) -> io::Result<UnixStream> {
    let (mut stream, _) = accept_with_timeout(&listener, READY_TIMEOUT)?;
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(READY_TIMEOUT))?;
    stream.set_write_timeout(Some(READY_TIMEOUT))?;
    let token_line = read_line_unbuffered(&mut stream)?;
    if token_line.trim_end() != token {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "handoff import token mismatch",
        ));
    }

    serde_json::to_writer(&mut stream, manifest).map_err(io::Error::other)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    stream.set_read_timeout(Some(READY_TIMEOUT))?;
    let validated = read_line_unbuffered(&mut stream)?;
    if validated.trim_end() != "validated" {
        return Err(io::Error::other("handoff import did not validate manifest"));
    }
    let _ = std::fs::remove_file(socket_path);
    Ok(stream)
}

#[cfg(unix)]
pub(crate) fn send_fds_and_wait_restored(stream: &mut UnixStream, fds: &[RawFd]) -> io::Result<()> {
    send_fds(stream, fds)?;

    stream.set_read_timeout(Some(READY_TIMEOUT))?;
    let restored = read_line_unbuffered(&mut *stream)?;
    if restored.trim_end() != "restored" {
        return Err(io::Error::other(
            "handoff import did not report restored runtimes",
        ));
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn wait_prepared(stream: &mut UnixStream) -> io::Result<()> {
    stream.set_read_timeout(Some(READY_TIMEOUT))?;
    let prepared = read_line_unbuffered(&mut *stream)?;
    if prepared.trim_end() != "prepared" {
        return Err(io::Error::other("handoff import did not report prepared"));
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn wait_ready(stream: &mut UnixStream) -> io::Result<()> {
    stream.set_read_timeout(Some(READY_TIMEOUT))?;
    let ready = read_line_unbuffered(&mut *stream)?;
    if ready.trim_end() != "ready" {
        return Err(io::Error::other("handoff import did not report ready"));
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn report_committed(stream: &mut UnixStream) -> io::Result<()> {
    stream.write_all(b"committed\n")?;
    stream.flush()
}

#[cfg(unix)]
pub(crate) fn wait_owned_ack(stream: &mut UnixStream) {
    if let Err(err) = stream.set_read_timeout(Some(OWNED_ACK_TIMEOUT)) {
        warn!(err = %err, "failed to set handoff ownership ack timeout");
        return;
    }
    match read_line_unbuffered(&mut *stream) {
        Ok(owned) if owned.trim_end() == "owned" => {}
        Ok(other) => {
            warn!(
                response = %other.trim_end(),
                "handoff import sent unexpected ownership ack after commit"
            );
        }
        Err(err) => {
            warn!(err = %err, "handoff import ownership ack was not received after commit");
        }
    }
}

#[cfg(unix)]
fn validate_and_normalize_manifest(manifest: &mut HandoffManifest) -> io::Result<()> {
    if manifest.version != LEGACY_HANDOFF_VERSION && manifest.version != HANDOFF_VERSION {
        return Err(io::Error::other(format!(
            "unsupported handoff version {}",
            manifest.version
        )));
    }

    let normalized = crate::persist::normalize_terminal_identities(&mut manifest.snapshot, false)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if manifest.version == HANDOFF_VERSION && normalized {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "handoff v2 snapshot was not terminal-identity normalized",
        ));
    }
    crate::persist::validate_snapshot_admission(&manifest.snapshot).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("handoff rejected before terminal ownership transfer: {error}"),
        )
    })?;

    let mut snapshot_terminals = std::collections::HashMap::new();
    let mut deferred_eligible_panes = std::collections::HashSet::new();
    for workspace in &manifest.snapshot.workspaces {
        for tab in &workspace.tabs {
            for (&pane_id, pane) in &tab.panes {
                let terminal_id = crate::persist::terminal_id_for_pane_snapshot(pane)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                if snapshot_terminals.insert(pane_id, terminal_id).is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "handoff snapshot contains duplicate pane identities",
                    ));
                }
                if pane.agent_session.is_some() {
                    deferred_eligible_panes.insert(pane_id);
                }
            }
        }
    }

    let mut runtime_panes = std::collections::HashSet::new();
    let mut runtime_terminals = std::collections::HashSet::new();
    for runtime in &mut manifest.panes {
        if !runtime_panes.insert(runtime.pane_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "handoff contains duplicate runtime pane entries",
            ));
        }
        let expected = snapshot_terminals.get(&runtime.pane_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "handoff runtime has no matching snapshot pane",
            )
        })?;
        if manifest.version == LEGACY_HANDOFF_VERSION && runtime.terminal_id.is_none() {
            runtime.terminal_id = Some(expected.as_str().to_owned());
        }
        let raw = runtime.terminal_id.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "handoff v2 runtime is missing terminal identity",
            )
        })?;
        if raw.len() > crate::terminal::MAX_TERMINAL_ID_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "handoff runtime terminal identity exceeds its size limit",
            ));
        }
        let terminal_id = crate::terminal::TerminalId::parse(raw).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "handoff runtime terminal identity is invalid",
            )
        })?;
        if &terminal_id != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "handoff runtime terminal identity does not match snapshot",
            ));
        }
        if !runtime_terminals.insert(terminal_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "handoff contains duplicate runtime terminal entries",
            ));
        }
    }
    if manifest.version == LEGACY_HANDOFF_VERSION && manifest.deferred_panes.is_empty() {
        manifest.deferred_panes = snapshot_terminals
            .keys()
            .filter(|pane_id| !runtime_panes.contains(pane_id))
            .copied()
            .collect();
        manifest.deferred_panes.sort_unstable();
    }

    let mut deferred_panes = std::collections::HashSet::new();
    for pane_id in &manifest.deferred_panes {
        if !deferred_panes.insert(*pane_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "handoff contains duplicate deferred pane entries",
            ));
        }
        if runtime_panes.contains(pane_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "handoff pane cannot own both an FD and deferred-resume contract",
            ));
        }
        if !snapshot_terminals.contains_key(pane_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "handoff deferred pane has no matching snapshot pane",
            ));
        }
        if !deferred_eligible_panes.contains(pane_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "handoff deferred pane has no persisted agent session",
            ));
        }
    }
    if runtime_panes.len() + deferred_panes.len() != snapshot_terminals.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "handoff runtime and deferred entries do not cover the snapshot",
        ));
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn receive(socket_path: &Path, token: &str) -> io::Result<ReceivedHandoff> {
    let mut stream = UnixStream::connect(socket_path)?;
    stream.write_all(token.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let manifest_line = read_line_unbuffered(&mut stream)?;
    let mut manifest: HandoffManifest =
        serde_json::from_str(&manifest_line).map_err(io::Error::other)?;
    validate_and_normalize_manifest(&mut manifest)?;
    if manifest
        .expected_protocol
        .is_some_and(|protocol| protocol != crate::protocol::PROTOCOL_VERSION)
    {
        return Err(io::Error::other(format!(
            "handoff expected protocol {}, but this server speaks protocol {}",
            manifest.expected_protocol.unwrap_or_default(),
            crate::protocol::PROTOCOL_VERSION
        )));
    }
    if manifest
        .expected_version
        .as_deref()
        .is_some_and(|version| version != crate::build_info::version())
    {
        return Err(io::Error::other(format!(
            "handoff expected herdr v{}, but this server is v{}",
            manifest.expected_version.as_deref().unwrap_or("unknown"),
            crate::build_info::version()
        )));
    }
    stream.write_all(b"validated\n")?;
    stream.flush()?;
    let fds = recv_fds(&stream, manifest.panes.len())?;
    Ok(ReceivedHandoff {
        manifest,
        fds,
        stream,
    })
}

#[cfg(unix)]
pub(crate) fn report_restored(stream: &mut UnixStream) -> io::Result<()> {
    stream.write_all(b"restored\n")?;
    stream.flush()
}

#[cfg(unix)]
pub(crate) fn report_prepared(stream: &mut UnixStream) -> io::Result<()> {
    stream.write_all(b"prepared\n")?;
    stream.flush()
}

#[cfg(unix)]
pub(crate) fn report_ready(stream: &mut UnixStream) -> io::Result<()> {
    stream.write_all(b"ready\n")?;
    stream.flush()
}

#[cfg(unix)]
pub(crate) fn wait_committed(stream: &mut UnixStream) -> io::Result<()> {
    stream.set_read_timeout(Some(READY_TIMEOUT))?;
    let committed = read_line_unbuffered(&mut *stream)?;
    if committed.trim_end() != "committed" {
        return Err(io::Error::other("handoff source did not commit"));
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn report_owned(stream: &mut UnixStream) -> io::Result<()> {
    stream.write_all(b"owned\n")?;
    stream.flush()
}

#[cfg(unix)]
pub(crate) fn manifest_for(
    snapshot: crate::persist::SessionSnapshot,
    panes: Vec<crate::handoff_runtime::HandoffRuntimeState>,
    deferred_panes: Vec<u32>,
    expected_protocol: Option<u32>,
    expected_version: Option<String>,
    api_window_title: Option<String>,
) -> HandoffManifest {
    HandoffManifest {
        version: HANDOFF_VERSION,
        source_version: crate::build_info::version(),
        source_protocol: crate::protocol::PROTOCOL_VERSION,
        expected_version,
        expected_protocol,
        snapshot,
        panes,
        deferred_panes,
        api_window_title,
    }
}

#[cfg(unix)]
fn restrict_socket_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(unix)]
fn accept_with_timeout(
    listener: &UnixListener,
    timeout: Duration,
) -> io::Result<(UnixStream, std::os::unix::net::SocketAddr)> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok(accepted) => return Ok(accepted),
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out waiting for handoff import connection",
                    ));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

#[cfg(unix)]
fn read_line_unbuffered(stream: &mut UnixStream) -> io::Result<String> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = stream.read(&mut byte)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "handoff stream closed while reading line",
            ));
        }
        bytes.push(byte[0]);
        if byte[0] == b'\n' {
            return String::from_utf8(bytes)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err));
        }
        if bytes.len() > MAX_HANDOFF_ENVELOPE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "handoff line exceeded maximum size",
            ));
        }
    }
}

#[cfg(unix)]
fn send_fds(stream: &UnixStream, fds: &[RawFd]) -> io::Result<()> {
    for batch in fds.chunks(FDS_PER_MESSAGE) {
        send_fd_batch(stream, batch)?;
    }
    Ok(())
}

#[cfg(unix)]
fn send_fd_batch(stream: &UnixStream, fds: &[RawFd]) -> io::Result<()> {
    if fds.is_empty() {
        return Ok(());
    }
    let byte = [b'F'];
    let iov = [libc::iovec {
        iov_base: byte.as_ptr() as *mut libc::c_void,
        iov_len: byte.len(),
    }];
    let fd_bytes = std::mem::size_of_val(fds);
    let mut control = vec![0u8; unsafe { libc::CMSG_SPACE(fd_bytes as u32) as usize }];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = iov.as_ptr() as *mut libc::iovec;
    msg.msg_iovlen = iov.len() as _;
    msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = control.len() as _;

    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if cmsg.is_null() {
            return Err(io::Error::other("failed to allocate fd control message"));
        }
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(fd_bytes as u32) as _;
        std::ptr::copy_nonoverlapping(fds.as_ptr() as *const u8, libc::CMSG_DATA(cmsg), fd_bytes);
        if libc::sendmsg(stream.as_raw_fd(), &msg, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn close_raw_fds(fds: &[RawFd]) {
    for fd in fds {
        let _ = unsafe { libc::close(*fd) };
    }
}

#[cfg(unix)]
fn recv_fds(stream: &UnixStream, expected: usize) -> io::Result<Vec<OwnedFd>> {
    let mut out: Vec<OwnedFd> = Vec::with_capacity(expected);
    while out.len() < expected {
        let wanted = (expected - out.len()).min(FDS_PER_MESSAGE);
        let batch = recv_fd_batch(stream, wanted)?;
        if batch.is_empty() {
            let received = out.len();
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "handoff stream closed after {received} of {expected} pane file descriptors"
                ),
            ));
        }
        out.extend(
            batch
                .into_iter()
                .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) }),
        );
    }
    Ok(out)
}

#[cfg(unix)]
fn recv_fd_batch(stream: &UnixStream, wanted: usize) -> io::Result<Vec<RawFd>> {
    let mut byte = [0u8; 1];
    let mut iov = [libc::iovec {
        iov_base: byte.as_mut_ptr() as *mut libc::c_void,
        iov_len: byte.len(),
    }];
    let fd_bytes = wanted * std::mem::size_of::<RawFd>();
    let mut control = vec![0u8; unsafe { libc::CMSG_SPACE(fd_bytes as u32) as usize }];
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = iov.as_mut_ptr();
    msg.msg_iovlen = iov.len() as _;
    msg.msg_control = control.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = control.len() as _;

    let read = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut msg, 0) };
    if read < 0 {
        return Err(io::Error::last_os_error());
    }

    let mut out = Vec::new();
    unsafe {
        let control_end = control.as_ptr() as usize + msg.msg_controllen as usize;
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(cmsg);
                // Bound the payload by both the header's own length and the
                // bytes the kernel wrote into `control`, so the read below can
                // never run past the buffer.
                let available = control_end.saturating_sub(data as usize);
                let data_len = ((*cmsg).cmsg_len as usize)
                    .saturating_sub(libc::CMSG_LEN(0) as usize)
                    .min(available);
                let count = data_len / std::mem::size_of::<RawFd>();
                let data = data as *const RawFd;
                for idx in 0..count {
                    out.push(*data.add(idx));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }

    // Truncation means the kernel closed the descriptors that did not fit, so
    // the batch is unrecoverable rather than merely short.
    if msg.msg_flags & libc::MSG_CTRUNC != 0 {
        close_raw_fds(&out);
        return Err(io::Error::other("handoff fd control message was truncated"));
    }
    if read == 0 {
        close_raw_fds(&out);
        return Ok(Vec::new());
    }
    if out.len() > wanted {
        let received = out.len();
        close_raw_fds(&out);
        return Err(io::Error::other(format!(
            "handoff fd message carried {received} descriptors, expected at most {wanted}"
        )));
    }
    if out.is_empty() {
        return Err(io::Error::other("handoff fd message missing SCM_RIGHTS"));
    }
    Ok(out)
}

#[cfg(unix)]
pub(crate) fn log_import_result(panes: usize) {
    info!(panes, "handoff import ready");
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn empty_snapshot() -> crate::persist::SessionSnapshot {
        crate::persist::SessionSnapshot {
            delegations: Vec::new(),
            version: 0,
            repositories: Vec::new(),
            space_order: Vec::new(),
            workspaces: Vec::new(),
            active: None,
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
        }
    }

    fn one_pane_snapshot() -> crate::persist::SessionSnapshot {
        let mut snapshot = crate::persist::SessionSnapshot {
            delegations: Vec::new(),
            version: 4,
            repositories: Vec::new(),
            space_order: Vec::new(),
            workspaces: vec![crate::persist::WorkspaceSnapshot {
                id: Some("w1".into()),
                custom_name: None,
                identity_cwd: "/tmp".into(),
                checkout: None,
                worktree_space: None,
                public_pane_numbers: std::collections::HashMap::from([(1, 1)]),
                next_public_pane_number: 2,
                public_tab_numbers: vec![1],
                next_public_tab_number: 2,
                tabs: vec![crate::persist::TabSnapshot {
                    custom_name: None,
                    layout: crate::persist::LayoutSnapshot::Pane(1),
                    collections: Vec::new(),
                    panes: std::collections::HashMap::from([(
                        1,
                        crate::persist::PaneSnapshot::without_terminal_id("/tmp".into()),
                    )]),
                    zoomed: false,
                    focused: Some(1),
                    focused_leaf: None,
                    root_pane: Some(1),
                }],
                active_tab: 0,
            }],
            active: Some(0),
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
        };
        crate::persist::normalize_terminal_identities(&mut snapshot, false).unwrap();
        snapshot
    }

    fn runtime_for(
        pane_id: u32,
        terminal_id: Option<String>,
    ) -> crate::handoff_runtime::HandoffRuntimeState {
        crate::handoff_runtime::HandoffRuntimeState {
            pane_id,
            terminal_id,
            child_pid: 1,
            rows: 24,
            cols: 80,
            cell_width_px: 0,
            cell_height_px: 0,
            keyboard_protocol_flags: 0,
            keyboard_protocol_ansi: None,
            input_state: None,
            terminal_title: None,
            initial_history_ansi: None,
        }
    }

    #[test]
    fn legacy_v1_handoff_fixture_normalizes_identity_before_fd_transfer() {
        let mut manifest: HandoffManifest = serde_json::from_str(include_str!(
            "../../tests/fixtures/handoff-manifest-v1.json"
        ))
        .expect("legacy v1 fixture should retain its original wire shape");

        validate_and_normalize_manifest(&mut manifest).unwrap();
        assert_eq!(manifest.snapshot.version, 5);
        let snapshot_id = crate::persist::terminal_id_for_pane_snapshot(
            &manifest.snapshot.workspaces[0].tabs[0].panes[&1],
        )
        .unwrap();
        assert_eq!(
            manifest.panes[0].terminal_id.as_deref(),
            Some(snapshot_id.as_str())
        );

        let path = std::env::temp_dir().join(format!(
            "vqro-handoff-v1-normalized-{}-{}.json",
            std::process::id(),
            snapshot_id.as_str()
        ));
        crate::persist::publish_handoff_snapshot_to_path(&path, &manifest.snapshot).unwrap();
        let persisted = std::fs::read_to_string(&path).unwrap();
        let restored = crate::persist::parse_snapshot(&persisted).unwrap();
        assert_eq!(
            crate::persist::terminal_id_for_pane_snapshot(
                &restored.workspaces[0].tabs[0].panes[&1]
            )
            .unwrap(),
            snapshot_id
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn handoff_v2_rejects_duplicate_or_non_bijective_runtime_entries() {
        let snapshot = one_pane_snapshot();
        let terminal_id = crate::persist::terminal_id_for_pane_snapshot(
            &snapshot.workspaces[0].tabs[0].panes[&1],
        )
        .unwrap();
        let runtime = runtime_for(1, Some(terminal_id.to_string()));
        let mut duplicate = HandoffManifest {
            version: HANDOFF_VERSION,
            source_version: "current".into(),
            source_protocol: crate::protocol::PROTOCOL_VERSION,
            expected_version: None,
            expected_protocol: None,
            snapshot: snapshot.clone(),
            panes: vec![runtime.clone(), runtime],
            deferred_panes: Vec::new(),
            api_window_title: None,
        };
        assert!(validate_and_normalize_manifest(&mut duplicate).is_err());

        let mut missing = HandoffManifest {
            panes: Vec::new(),
            ..duplicate
        };
        assert!(validate_and_normalize_manifest(&mut missing).is_err());
    }

    #[test]
    fn handoff_v2_accepts_explicit_deferred_resume_without_an_fd() {
        let mut snapshot = one_pane_snapshot();
        snapshot.workspaces[0].tabs[0]
            .panes
            .get_mut(&1)
            .unwrap()
            .agent_session = Some(crate::persist::PaneAgentSessionSnapshot {
            source: "vqro:codex".into(),
            agent: "codex".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Id,
            value: "session-1".into(),
        });
        let mut manifest = HandoffManifest {
            version: HANDOFF_VERSION,
            source_version: "current".into(),
            source_protocol: crate::protocol::PROTOCOL_VERSION,
            expected_version: None,
            expected_protocol: None,
            snapshot: snapshot.clone(),
            panes: Vec::new(),
            deferred_panes: vec![1],
            api_window_title: None,
        };
        validate_and_normalize_manifest(&mut manifest).unwrap();

        let mut legacy = HandoffManifest {
            version: LEGACY_HANDOFF_VERSION,
            source_version: "legacy".into(),
            source_protocol: crate::protocol::PROTOCOL_VERSION,
            expected_version: None,
            expected_protocol: None,
            snapshot,
            panes: Vec::new(),
            deferred_panes: Vec::new(),
            api_window_title: None,
        };
        validate_and_normalize_manifest(&mut legacy).unwrap();
        assert_eq!(legacy.deferred_panes, vec![1]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn partial_fd_receive_closes_already_received_descriptors() {
        let before = std::fs::read_dir("/proc/self/fd").unwrap().count();
        let (sender, receiver) = UnixStream::pair().unwrap();
        let mut pipe = [0; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        let during = std::fs::read_dir("/proc/self/fd").unwrap().count();
        let send_fd = pipe[0];
        let writer = std::thread::spawn(move || {
            send_fds(&sender, &[send_fd]).unwrap();
            drop(sender);
        });
        assert!(recv_fds(&receiver, 2).is_err());
        writer.join().unwrap();
        assert_eq!(
            std::fs::read_dir("/proc/self/fd").unwrap().count(),
            during - 1
        );
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
        drop(receiver);
        assert_eq!(std::fs::read_dir("/proc/self/fd").unwrap().count(), before);
    }

    #[test]
    fn a_handoff_carries_an_api_set_window_title() {
        let manifest = manifest_for(
            empty_snapshot(),
            Vec::new(),
            Vec::new(),
            None,
            None,
            Some("deploying".to_string()),
        );

        assert_eq!(manifest.api_window_title.as_deref(), Some("deploying"));
    }

    #[test]
    fn a_manifest_written_before_the_title_field_still_loads() {
        let manifest = manifest_for(
            empty_snapshot(),
            Vec::new(),
            Vec::new(),
            None,
            None,
            Some("deploying".to_string()),
        );
        let mut value = serde_json::to_value(&manifest).expect("manifest should serialize");
        value
            .as_object_mut()
            .expect("manifest should be a json object")
            .remove("api_window_title");

        let older: HandoffManifest =
            serde_json::from_value(value).expect("an older manifest should still load");

        assert!(older.api_window_title.is_none());
    }
}
