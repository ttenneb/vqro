use std::path::{Path, PathBuf};

pub(crate) fn read_fd(fd: std::os::fd::RawFd, data: &mut [u8]) -> std::io::Result<usize> {
    let result = unsafe { libc::read(fd, data.as_mut_ptr().cast(), data.len()) };
    if result < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(result as usize)
    }
}

/// The tty erase character (VERASE) on stdin, when enabled.
pub(super) fn terminal_erase_byte() -> Option<u8> {
    let mut termios: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut termios) } != 0 {
        return None;
    }
    let erase = termios.c_cc[libc::VERASE];
    (erase != 0 && erase != 0xff).then_some(erase)
}

pub(crate) fn poll_fd_readable(fd: std::os::fd::RawFd, timeout_ms: i32) -> std::io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
    if result < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(result > 0)
    }
}

/// OS primitive only; ownership and no-write-through policy belongs to caller.
pub(crate) fn resource_symlink(source: &Path, target: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, target)
}

/// One executable name on PATH or an absolute executable file. This is an OS
/// availability observation, not an identity/SDK compatibility attestation.
pub(crate) fn unix_executable_available(executable: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let executable_file = |path: &Path| {
        std::fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    };
    let executable = Path::new(executable);
    if executable.is_absolute() {
        executable_file(executable)
    } else {
        std::env::var_os("PATH").is_some_and(|paths| {
            std::env::split_paths(&paths).any(|dir| executable_file(&dir.join(executable)))
        })
    }
}

#[derive(Debug)]
pub(crate) struct PlatformPersistenceLock {
    _file: std::fs::File,
}

#[allow(dead_code)] // Used when the staged persistence coordinator is wired into production.
/// Locks a stable file in a trusted private parent. This cooperative Unix lock cannot stop a
/// same-user actor with parent write access from replacing the pathname; Vqro never unlinks or
/// renames it.
pub(crate) fn try_lock_platform_persistence(
    path: &Path,
) -> Result<PlatformPersistenceLock, super::PersistenceLockError> {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};

    let open = |create_new| {
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create_new(create_new)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        options.open(path)
    };
    let file = match open(true) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            open(false).map_err(super::PersistenceLockError::Io)?
        }
        Err(error) => return Err(super::PersistenceLockError::Io(error)),
    };
    let metadata = file.metadata().map_err(super::PersistenceLockError::Io)?;
    if !metadata.is_file() {
        return Err(super::PersistenceLockError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "persistence ownership path is not a regular file",
        )));
    }
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        let code = error.raw_os_error();
        if code == Some(libc::EWOULDBLOCK) || code == Some(libc::EAGAIN) {
            return Err(super::PersistenceLockError::Busy);
        }
        return Err(super::PersistenceLockError::Io(error));
    }
    // Ownership and permissions are validated or repaired only after this open file description
    // owns the lock. Re-read metadata so a pre-lock observation cannot hide a mode change.
    let metadata = file.metadata().map_err(super::PersistenceLockError::Io)?;
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(super::PersistenceLockError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "persistence ownership file belongs to another user",
        )));
    }
    if metadata.mode() & 0o777 != 0o600 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(super::PersistenceLockError::Io)?;
    }
    Ok(PlatformPersistenceLock { _file: file })
}

pub(crate) fn local_listener_accept_error_is_resource_exhaustion(error: &std::io::Error) -> bool {
    error
        .raw_os_error()
        .is_some_and(|code| code == libc::EMFILE || code == libc::ENFILE)
}

pub(crate) fn classify_child_exit(status: &portable_pty::ExitStatus) -> super::ChildExitReason {
    if status.signal().is_some() {
        super::ChildExitReason::Interrupted
    } else {
        super::ChildExitReason::Exited
    }
}

pub(crate) fn shutdown_client_stream(stream: &crate::ipc::LocalStream) -> std::io::Result<()> {
    let crate::ipc::LocalStream::UdSocket(stream) = stream;
    stream.inner().shutdown(std::net::Shutdown::Both)
}

pub(crate) struct ClientStreamReader<'a>(pub(crate) &'a mut crate::ipc::LocalStream);

impl std::io::Read for ClientStreamReader<'_> {
    fn read(&mut self, data: &mut [u8]) -> std::io::Result<usize> {
        use std::os::fd::AsRawFd as _;

        loop {
            match self.0.read(data) {
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    let crate::ipc::LocalStream::UdSocket(stream) = &*self.0;
                    let mut descriptor = libc::pollfd {
                        fd: stream.inner().as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // Sleep until input or shutdown, without polling quiet observers.
                    if unsafe { libc::poll(&mut descriptor, 1, -1) } < 0 {
                        let error = std::io::Error::last_os_error();
                        if error.kind() != std::io::ErrorKind::Interrupted {
                            return Err(error);
                        }
                    }
                }
                result => return result,
            }
        }
    }
}

pub(crate) fn write_client_stream(
    stream: &crate::ipc::LocalStream,
    mut data: &[u8],
) -> std::io::Result<()> {
    use std::io::{self, Write as _};
    use std::os::fd::AsRawFd as _;
    use std::time::Instant;

    let crate::ipc::LocalStream::UdSocket(socket) = stream;
    let mut socket = socket.inner();
    let Some(timeout) = socket.write_timeout()? else {
        return socket.write_all(data);
    };
    let timed_out = || {
        // Dropping the writer clone alone would leave the reader blocked.
        let _ = shutdown_client_stream(stream);
        io::Error::new(
            io::ErrorKind::TimedOut,
            "terminal observer stopped receiving output",
        )
    };
    let mut progress = Instant::now();
    while !data.is_empty() {
        match socket.write(data) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => {
                data = &data[written..];
                progress = Instant::now();
                continue;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
        let remaining = timeout
            .checked_sub(progress.elapsed())
            .ok_or_else(timed_out)?;
        let mut descriptor = libc::pollfd {
            fd: socket.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        let wait_ms = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        let ready = unsafe { libc::poll(&mut descriptor, 1, wait_ms) };
        if ready == 0 {
            return Err(timed_out());
        }
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
    Ok(())
}

pub(crate) fn wait_client_stream_readable(stream: &crate::ipc::LocalStream) -> std::io::Result<()> {
    use std::os::fd::{AsFd as _, AsRawFd as _};
    let crate::ipc::LocalStream::UdSocket(stream) = stream;
    let mut descriptor = libc::pollfd {
        fd: stream.as_fd().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // Bound cancellation latency without polling idle connections hundreds of times per second.
    let result = unsafe { libc::poll(&mut descriptor, 1, 100) };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}

pub(crate) fn forward_remote_bridge_stdio(
    stream: crate::ipc::LocalStream,
    idle_timeout: bool,
) -> std::io::Result<()> {
    forward_remote_bridge_stdio_with_timeout(
        stream,
        idle_timeout.then_some(super::remote_bridge::IDLE_TIMEOUT),
    )
}

pub(super) fn forward_remote_bridge_stdio_with_timeout(
    stream: crate::ipc::LocalStream,
    idle_timeout: Option<std::time::Duration>,
) -> std::io::Result<()> {
    use super::remote_bridge::{Activity, TrackedIo};
    use interprocess::TryClone as _;

    let activity = idle_timeout.map(Activity::start).transpose()?;
    let mut stdout = TrackedIo::new(std::io::stdout().lock(), activity.clone());
    let mut socket_to_stdout = TrackedIo::new(stream.try_clone()?, activity.clone());
    let mut stdin_to_socket = stream;
    let _upload = std::thread::spawn(move || {
        let mut stdin = TrackedIo::new(std::io::stdin(), activity.clone());
        let _ = copy_flush(
            &mut stdin,
            &mut TrackedIo::new(&mut stdin_to_socket, activity),
        );
        let crate::ipc::LocalStream::UdSocket(stream) = stdin_to_socket;
        let _ = stream.inner().shutdown(std::net::Shutdown::Write);
    });
    copy_flush(&mut socket_to_stdout, &mut stdout)
}

fn copy_flush<R: std::io::Read, W: std::io::Write>(
    reader: &mut R,
    writer: &mut W,
) -> std::io::Result<()> {
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => read,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        writer.write_all(&buffer[..read])?;
        writer.flush()?;
    }
}

pub(crate) struct RemoteBridgeWake {
    reader: std::os::unix::net::UnixStream,
    writer: std::os::unix::net::UnixStream,
}

impl RemoteBridgeWake {
    pub(crate) fn new() -> std::io::Result<Self> {
        let (reader, writer) = std::os::unix::net::UnixStream::pair()?;
        Ok(Self { reader, writer })
    }

    pub(crate) fn cancel(&self) -> std::io::Result<()> {
        // EOF stays readable, including when cancellation precedes the wait.
        self.writer.shutdown(std::net::Shutdown::Write)
    }

    pub(crate) fn wait(&self, stream: &crate::ipc::LocalStream) -> std::io::Result<()> {
        use std::os::fd::{AsFd as _, AsRawFd as _};
        let crate::ipc::LocalStream::UdSocket(stream) = stream;
        let mut descriptors = [
            libc::pollfd {
                fd: stream.as_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        loop {
            // SAFETY: both descriptors remain borrowed and the array has two entries.
            if unsafe { libc::poll(descriptors.as_mut_ptr(), 2, -1) } >= 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

pub(super) fn read_terminal_grid_size() -> std::io::Result<(u16, u16)> {
    crossterm::terminal::window_size().map(|size| (size.columns, size.rows))
}

fn set_sigpipe_disposition(handler: libc::sighandler_t) {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = handler;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
        // Rust starts with SIGPIPE ignored. If this best-effort transition
        // fails, stdout retains the existing Rust behavior.
        libc::sigaction(libc::SIGPIPE, &action, std::ptr::null_mut());
    }
}

extern "C" fn discard_signal(_signal: libc::c_int) {}

/// Survive the launching shell's hangup. A handler (unlike SIG_IGN) resets
/// on exec so child shells retain ordinary hangup behavior.
pub(crate) fn ignore_server_hangup() {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = discard_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    action.sa_flags = libc::SA_RESTART;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(libc::SIGHUP, &action, std::ptr::null_mut()) != 0 {
            tracing::warn!(error = %std::io::Error::last_os_error(), "failed to ignore server hangup");
        }
    }
}

/// Explicit stop signals still stop; hangup is only a detached-client event.
pub(crate) fn spawn_server_signal_monitor(
    on_quit: impl Fn(super::ServerQuitSignal) + Send + Sync + 'static,
) {
    use tokio::signal::unix::{signal, SignalKind};
    let on_quit = std::sync::Arc::new(on_quit);
    for (kind, quit_signal) in [
        (SignalKind::interrupt(), super::ServerQuitSignal::Interrupt),
        (SignalKind::terminate(), super::ServerQuitSignal::Terminate),
    ] {
        match signal(kind) {
            Ok(mut stream) => {
                let on_quit = on_quit.clone();
                tokio::spawn(async move {
                    while stream.recv().await.is_some() {
                        on_quit(quit_signal);
                    }
                });
            }
            Err(error) => {
                tracing::warn!(%error, signal = %quit_signal, "failed to install server stop handler")
            }
        }
    }
    match signal(SignalKind::hangup()) {
        Ok(mut stream) => {
            tokio::spawn(async move {
                while stream.recv().await.is_some() {
                    tracing::info!("ignoring SIGHUP; server keeps running");
                }
            });
        }
        Err(error) => tracing::warn!(%error, "failed to install SIGHUP handler"),
    }
}

pub(crate) fn begin_cli_output() {
    set_sigpipe_disposition(libc::SIG_DFL);
}

pub(crate) fn end_cli_output() {
    set_sigpipe_disposition(libc::SIG_IGN);
}

pub(crate) fn remote_ssh_config_paths() -> super::RemoteSshConfigPaths {
    super::RemoteSshConfigPaths {
        user_config: std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".ssh").join("config")),
        system_config: Some(PathBuf::from("/etc/ssh/ssh_config")),
        multiplexing: true,
    }
}

pub(crate) fn create_remote_ssh_config_dir(control_socket_name: &str) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;

    let mut bases = vec![std::env::temp_dir()];
    let short_tmp = PathBuf::from("/tmp");
    if bases.first() != Some(&short_tmp) {
        bases.push(short_tmp);
    }

    let mut last_error = None;
    let mut path_fits = false;
    for base in bases {
        for attempt in 0..100 {
            let dir = base.join(format!("vqro-ssh-{}-{attempt}", std::process::id()));
            if !fits_unix_socket_path(&dir.join(control_socket_name)) {
                continue;
            }
            path_fits = true;
            match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => return Ok(dir),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    last_error = Some(err);
                    break;
                }
            }
        }
    }

    if let Some(err) = last_error {
        return Err(err);
    }
    let message = if path_fits {
        "failed to create private herdr ssh config directory"
    } else {
        "SSH control socket path exceeds the Unix socket length limit"
    };
    Err(std::io::Error::new(
        if path_fits {
            std::io::ErrorKind::AlreadyExists
        } else {
            std::io::ErrorKind::InvalidInput
        },
        message,
    ))
}

pub(crate) fn create_remote_ssh_config_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

pub(crate) fn create_remote_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    std::fs::DirBuilder::new().mode(0o700).create(path)
}

pub(crate) fn remote_private_temp_base() -> PathBuf {
    std::env::temp_dir()
}

pub(crate) fn remote_bridge_endpoint_path(readable_name: &str, short_name: &str) -> PathBuf {
    let tmp = std::env::temp_dir();
    let readable = tmp.join(readable_name);
    if fits_unix_socket_path(&readable) {
        return readable;
    }
    let short = tmp.join(short_name);
    if fits_unix_socket_path(&short) {
        return short;
    }
    PathBuf::from("/tmp").join(short_name)
}

pub(crate) fn remote_reattach_program(program: &str) -> String {
    shell_quote(if program.is_empty() { "herdr" } else { program })
}

pub(crate) fn remote_reattach_argument(value: &str) -> String {
    shell_quote(value)
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
                )
        })
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn fits_unix_socket_path(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;

    path.as_os_str().as_bytes().len() <= 103
}

/// The machine's node name, as shown by tmux's `#h`.
pub(crate) fn hostname() -> Option<String> {
    let mut buffer = [0_u8; 256];
    let result =
        unsafe { libc::gethostname(buffer.as_mut_ptr().cast::<libc::c_char>(), buffer.len()) };
    if result != 0 {
        return None;
    }
    let end = buffer
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(buffer.len());
    let name = String::from_utf8_lossy(&buffer[..end]).into_owned();
    (!name.is_empty()).then_some(name)
}

pub(crate) fn local_datetime() -> Option<time::PrimitiveDateTime> {
    let mut timestamp: libc::time_t = 0;
    if unsafe { libc::time(&mut timestamp) } == -1 {
        return None;
    }
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&timestamp, &mut local) }.is_null() {
        return None;
    }
    datetime_from_tm(&local)
}

pub(crate) fn foreground_cli_command(mut command: std::process::Command) -> std::io::Result<i32> {
    use std::os::unix::process::CommandExt;
    Err(command.exec())
}

pub(crate) fn status_commands_supported() -> bool {
    true
}

pub(crate) fn fill_secure_random(bytes: &mut [u8]) -> std::io::Result<()> {
    use std::io::Read as _;

    std::fs::File::open("/dev/urandom")?.read_exact(bytes)
}

#[derive(Debug)]
pub(crate) struct ExtensionServiceContainment {
    process_group: i32,
}

impl ExtensionServiceContainment {
    pub(crate) fn new(child: &std::process::Child) -> std::io::Result<Self> {
        let process_group = i32::try_from(child.id())
            .map_err(|_| std::io::Error::other("extension process ID exceeds i32"))?;
        Ok(Self { process_group })
    }
}

impl Drop for ExtensionServiceContainment {
    fn drop(&mut self) {
        // The service is launched as its own process group. Killing the group also
        // covers idle root-process exits whose descendants retained no stdio.
        unsafe {
            libc::kill(-self.process_group, libc::SIGKILL);
        }
    }
}

pub(crate) fn configure_extension_service_command(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

pub(crate) fn terminate_extension_service_process_group(child: &std::process::Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        // The service is launched as its own process group. Killing the group prevents a
        // descendant from retaining captured stdout/stderr indefinitely.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

pub(crate) fn configure_status_command(process: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;

    process.process_group(0);
}

pub(crate) struct StatusCommandGuard {
    process_group_id: Option<i32>,
}

impl StatusCommandGuard {
    pub(crate) fn new(child: &tokio::process::Child) -> std::io::Result<Self> {
        let process_id = child
            .id()
            .ok_or_else(|| std::io::Error::other("status command has no process id"))?;
        let process_group_id = i32::try_from(process_id)
            .map_err(|_| std::io::Error::other("status command process id exceeds i32"))?;
        Ok(Self {
            process_group_id: Some(process_group_id),
        })
    }
}

impl StatusCommandGuard {
    pub(crate) fn terminate(&mut self) {
        if let Some(process_group_id) = self.process_group_id.take() {
            // The command was spawned as this process group's leader. Killing the
            // group also cleans up background descendants on completion/cancellation.
            unsafe {
                libc::kill(-process_group_id, libc::SIGKILL);
            }
        }
    }
}

impl Drop for StatusCommandGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn datetime_from_tm(value: &libc::tm) -> Option<time::PrimitiveDateTime> {
    let month = time::Month::try_from(u8::try_from(value.tm_mon + 1).ok()?).ok()?;
    let date = time::Date::from_calendar_date(
        value.tm_year + 1900,
        month,
        u8::try_from(value.tm_mday).ok()?,
    )
    .ok()?;
    let time = time::Time::from_hms(
        u8::try_from(value.tm_hour).ok()?,
        u8::try_from(value.tm_min).ok()?,
        u8::try_from(value.tm_sec).ok()?,
    )
    .ok()?;
    Some(time::PrimitiveDateTime::new(date, time))
}

pub(crate) fn set_default_plugin_pane_pwd(env: &mut Vec<(String, String)>, cwd: &std::path::Path) {
    if !env.iter().any(|(key, _)| key == "PWD") {
        env.push(("PWD".to_string(), cwd.display().to_string()));
    }
}

pub(crate) struct PackageStagingBackend(std::os::fd::OwnedFd);

pub(crate) fn configure_package_nofollow(options: &mut std::fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
}

pub(crate) fn create_private_package_directory(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700).create(path)
}

pub(crate) fn open_package_staging(path: &Path) -> std::io::Result<PackageStagingBackend> {
    use std::ffi::CString;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in staging path")
    })?;
    // SAFETY: path is NUL-terminated and the returned descriptor is owned below.
    let descriptor = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: open returned a fresh owned descriptor.
    Ok(PackageStagingBackend(unsafe {
        OwnedFd::from_raw_fd(descriptor)
    }))
}

pub(crate) fn write_package_staging_file(
    staging: &PackageStagingBackend,
    components: &[std::ffi::OsString],
    bytes: &[u8],
    mode: u32,
) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    // SAFETY: fcntl duplicates a valid descriptor and returns a new owned descriptor.
    let duplicate = unsafe { libc::fcntl(staging.0.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fcntl returned a fresh descriptor.
    let mut directory = unsafe { OwnedFd::from_raw_fd(duplicate) };
    for component in &components[..components.len() - 1] {
        let component = CString::new(component.as_bytes()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in package path")
        })?;
        // SAFETY: directory and component are valid; EEXIST is handled below.
        let created = unsafe { libc::mkdirat(directory.as_raw_fd(), component.as_ptr(), 0o700) };
        if created < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EEXIST) {
                return Err(error);
            }
        }
        // SAFETY: arguments are valid and the returned descriptor is owned below.
        let next = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if next < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openat returned a fresh descriptor.
        directory = unsafe { OwnedFd::from_raw_fd(next) };
    }
    let name = CString::new(components[components.len() - 1].as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in package path")
    })?;
    // SAFETY: arguments are valid and the returned descriptor is owned by File.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: openat returned a fresh descriptor.
    let mut file = unsafe { std::fs::File::from_raw_fd(descriptor) };
    file.write_all(bytes)?;
    file.sync_all()?;
    // SAFETY: file owns a valid descriptor; mode is limited by package validation.
    if unsafe { libc::fchmod(file.as_raw_fd(), mode as libc::mode_t) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_availability_requires_a_regular_executable_file() {
        use std::os::unix::fs::PermissionsExt;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("vqro-executable-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let executable = root.join("pi1 space;$literal");
        assert!(!unix_executable_available(executable.to_str().unwrap()));
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(!unix_executable_available(executable.to_str().unwrap()));
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(unix_executable_available(executable.to_str().unwrap()));
        assert!(!unix_executable_available(root.to_str().unwrap()));
        let alias = root.join("pi1-alias");
        std::os::unix::fs::symlink(&executable, &alias).unwrap();
        assert!(unix_executable_available(alias.to_str().unwrap()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn listener_resource_exhaustion_classifies_descriptor_limits_only() {
        assert!(local_listener_accept_error_is_resource_exhaustion(
            &std::io::Error::from_raw_os_error(libc::EMFILE)
        ));
        assert!(local_listener_accept_error_is_resource_exhaustion(
            &std::io::Error::from_raw_os_error(libc::ENFILE)
        ));
        assert!(!local_listener_accept_error_is_resource_exhaustion(
            &std::io::Error::from_raw_os_error(libc::ECONNABORTED)
        ));
    }

    #[test]
    fn plugin_pane_pwd_defaults_to_cwd_without_overriding_explicit_env() {
        let cwd = Path::new("/plugin-cwd");
        let mut derived = vec![("OTHER".to_string(), "value".to_string())];
        set_default_plugin_pane_pwd(&mut derived, cwd);
        assert!(derived.contains(&("PWD".to_string(), "/plugin-cwd".to_string())));

        let mut explicit = vec![("PWD".to_string(), "/caller-pwd".to_string())];
        set_default_plugin_pane_pwd(&mut explicit, cwd);
        assert_eq!(explicit, [("PWD".to_string(), "/caller-pwd".to_string())]);
    }

    #[test]
    fn remote_ssh_config_dir_rejects_overlong_control_socket_name() {
        let err = create_remote_ssh_config_dir(&"x".repeat(200)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }
}
