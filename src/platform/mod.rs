//! Platform-specific process and filesystem operations.
//!
//! Centralizes OS-dependent behavior behind a clean boundary so core
//! modules don't scatter `#[cfg]` branches through product logic.

/// Keeps an OS shutdown inhibitor/registration alive until the final save.
pub(crate) struct HostShutdownMonitor {
    task: Option<tokio::task::JoinHandle<()>>,
}

impl HostShutdownMonitor {
    pub(crate) fn start(
        requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
        intent: HostShutdownIntentCell,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        // Unit-test server loops inject the shutdown probe/flag and must not
        // acquire inhibitors on the real host. The private-bus integration
        // harness drives this production path through the built server binary.
        if cfg!(test) {
            return Self { task: None };
        }
        Self {
            task: monitor_host_shutdown(requested, intent, wake),
        }
    }
}

impl Drop for HostShutdownMonitor {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// A cancellable macOS restart announcement, measured on the boot-wide clock.
/// Handoff captures it with the manifest; later announcements/cancellations
/// during transfer still reach only the old server until import starts watching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct HostShutdownIntent {
    announced_at_ns: u64,
}

// Windows has no live handoff or cancellable announcement to carry.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Clone, Default)]
pub(crate) struct HostShutdownIntentCell(
    std::sync::Arc<std::sync::Mutex<Option<HostShutdownIntent>>>,
);

// Windows has no live handoff or cancellable announcement to carry.
#[cfg_attr(windows, allow(dead_code))]
impl HostShutdownIntentCell {
    pub(crate) fn get(&self) -> Option<HostShutdownIntent> {
        *self.lock()
    }
    pub(crate) fn set(&self, intent: Option<HostShutdownIntent>) {
        *self.lock() = intent;
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<HostShutdownIntent>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn monitor_host_shutdown(
    _requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
    _intent: HostShutdownIntentCell,
    _wake: impl Fn() + Send + Sync + 'static,
) -> Option<tokio::task::JoinHandle<()>> {
    None
}

#[cfg(not(windows))]
pub(crate) fn host_shutdown_in_progress() -> bool {
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServerQuitSignal {
    #[cfg(unix)]
    Interrupt,
    #[cfg(unix)]
    Terminate,
    #[cfg(not(unix))]
    ConsoleControl,
}

impl std::fmt::Display for ServerQuitSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            #[cfg(unix)]
            Self::Interrupt => "SIGINT",
            #[cfg(unix)]
            Self::Terminate => "SIGTERM",
            #[cfg(not(unix))]
            Self::ConsoleControl => "console control event",
        })
    }
}

#[cfg(not(unix))]
pub(crate) fn spawn_server_signal_monitor(
    on_quit: impl Fn(ServerQuitSignal) + Send + Sync + 'static,
) {
    if let Err(error) = ctrlc::set_handler(move || on_quit(ServerQuitSignal::ConsoleControl)) {
        tracing::warn!(%error, "failed to install server stop handler");
    }
}

#[cfg(not(unix))]
pub(crate) fn ignore_server_hangup() {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundProcess {
    pub pid: u32,
    pub name: String,
    pub argv0: Option<String>,
    pub argv: Option<Vec<String>>,
    pub cmdline: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundJob {
    pub process_group_id: u32,
    pub processes: Vec<ForegroundProcess>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessTelemetry {
    pub(crate) pid: u32,
    pub(crate) name: String,
    pub(crate) state: Option<String>,
    pub(crate) cpu_time_ms: Option<u64>,
    pub(crate) resident_bytes: Option<u64>,
    pub(crate) thread_count: Option<u32>,
    pub(crate) read_bytes: Option<u64>,
    pub(crate) write_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerminalTelemetrySample {
    // Process telemetry is implemented only by the Linux platform module.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Available {
        process_group_id: u32,
        processes: Vec<ProcessTelemetry>,
        truncated: bool,
    },
    Unavailable(&'static str),
    // This variant is produced by non-Linux platform implementations.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    Unsupported,
}

/// Windows integrity label used on Vqro's local pipes.
///
/// `High` requires at least high integrity, so an ordinary (medium) client of
/// the same account cannot connect. `Medium` admits ordinary same-account
/// clients of an elevated server. The label is the only thing that separates
/// "same account, ordinary process" from "elevated process", because the
/// account SID is identical in both.
#[cfg_attr(not(windows), allow(dead_code))] // Consumed by the Windows platform module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowsPipeIntegrity {
    High,
    Medium,
}

impl WindowsPipeIntegrity {
    fn sddl(self) -> &'static str {
        match self {
            Self::High => "HI",
            Self::Medium => "ME",
        }
    }
}

/// Which endpoints take part in the cross-elevation policy.
#[cfg_attr(not(windows), allow(dead_code))] // Consumed by the Windows platform module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowsPipeEndpoint {
    /// The API socket: a client must read and write requests.
    Api,
    /// The private terminal socket: full access for the attaching client.
    Terminal,
}

impl WindowsPipeEndpoint {
    fn access(self) -> &'static str {
        match self {
            Self::Api => "GRGW",
            Self::Terminal => "GA",
        }
    }
}

/// The security descriptor string for a Vqro local pipe on Windows.
///
/// System and the account SID get the endpoint's access; a mandatory integrity
/// label then bounds which integrity levels may open the pipe at all. This is a
/// pure contract: the platform module supplies the account SID, the elevated
/// bit and whether the operator consented to ordinary clients.
#[cfg_attr(not(windows), allow(dead_code))] // Consumed by the Windows platform module.
pub(crate) fn windows_local_pipe_sddl(
    endpoint: WindowsPipeEndpoint,
    account_sid: &str,
    integrity: WindowsPipeIntegrity,
) -> String {
    format!(
        "D:P(A;;GA;;;SY)(A;;{};;;{account_sid})S:(ML;;NRNW;;;{})",
        endpoint.access(),
        integrity.sddl()
    )
}

/// An elevated Vqro server admits ordinary same-account clients only with
/// explicit consent. Without consent, an elevated server labels its pipes high
/// so a medium-integrity client of the same account is refused.
#[cfg_attr(not(windows), allow(dead_code))] // Consumed by the Windows platform module.
pub(crate) fn windows_pipe_integrity(
    process_is_elevated: bool,
    allow_unelevated_clients: bool,
) -> WindowsPipeIntegrity {
    if process_is_elevated && !allow_unelevated_clients {
        WindowsPipeIntegrity::High
    } else {
        WindowsPipeIntegrity::Medium
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn sample_terminal_telemetry(
    _shell_pid: u32,
    _max_processes: usize,
) -> TerminalTelemetrySample {
    TerminalTelemetrySample::Unsupported
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn socket_peer_birth(_stream: &crate::ipc::LocalStream) -> Option<(u32, u64)> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn invocation_peer(
    _stream: &crate::ipc::LocalStream,
) -> Option<crate::action_invocation::InvocationPeer> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn invocation_peer_for_process(
    _pid: u32,
) -> Option<crate::action_invocation::InvocationPeer> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn invocation_peer_is_current(_peer: &crate::action_invocation::InvocationPeer) -> bool {
    false
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn invocation_process_birth(_pid: u32) -> Option<u64> {
    None
}

#[cfg(any(target_os = "linux", target_os = "macos", test))]
const INVOCATION_ANCESTRY_LIMIT: usize = 64;

#[cfg(any(target_os = "linux", target_os = "macos", test))]
enum InvocationAncestor {
    Process(crate::action_invocation::InvocationProcess),
    // A positively observed platform boundary, never an I/O/metadata error.
    // Linux currently walks to PID 0; macOS excludes foreign-euid ancestors.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    Boundary,
}

/// A root outside the retained chain cannot be used by caller classification.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn capture_invocation_ancestry(
    mut pid: u32,
    mut read: impl FnMut(u32) -> Option<InvocationAncestor>,
) -> Option<Vec<crate::action_invocation::InvocationProcess>> {
    let mut ancestry = Vec::new();
    while pid != 0 && ancestry.len() < INVOCATION_ANCESTRY_LIMIT {
        if ancestry
            .iter()
            .any(|entry: &crate::action_invocation::InvocationProcess| entry.pid == pid)
        {
            return None;
        }
        match read(pid)? {
            InvocationAncestor::Boundary => break,
            InvocationAncestor::Process(entry) => {
                if entry.pid != pid || entry.process.pid != pid {
                    return None;
                }
                pid = entry.parent_pid;
                ancestry.push(entry);
            }
        }
    }
    // Also reject a cycle which closes exactly at the capture bound.
    if ancestry.is_empty() || ancestry.iter().any(|entry| entry.pid == pid) {
        return None;
    }
    Some(ancestry)
}

/// Recheck lifetime and topology; same-PID exec is deliberately one lifetime.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn invocation_ancestry_is_current(
    ancestry: &[crate::action_invocation::InvocationProcess],
    mut read: impl FnMut(u32) -> Option<(u64, u32)>,
) -> bool {
    !ancestry.is_empty()
        && ancestry.len() <= INVOCATION_ANCESTRY_LIMIT
        && ancestry
            .iter()
            .all(|entry| read(entry.pid) == Some((entry.birth, entry.parent_pid)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Hangup,
    Terminate,
    Kill,
}

/// Why a pane runtime ended, before application persistence policy is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildExitReason {
    Exited,
    Interrupted,
    /// Imported runtimes have no child wait handle in the replacement server.
    #[cfg(unix)]
    Handoff,
    WaitFailed,
}

impl ChildExitReason {
    pub(crate) fn requires_session_checkpoint(self) -> bool {
        match self {
            Self::Interrupted => true,
            #[cfg(unix)]
            Self::Handoff => true,
            _ => false,
        }
    }
}

#[cfg(unix)]
pub(crate) use unix_common::classify_child_exit;

#[cfg(not(any(unix, windows)))]
pub(crate) fn classify_child_exit(_status: &portable_pty::ExitStatus) -> ChildExitReason {
    ChildExitReason::Exited
}

/// Explicit executable-extension CLI invocation. Replacement keeps the native
/// peer PID/direct-shell-child topology; never substitute ordinary spawn here.
pub(crate) fn foreground_cli_command(command: std::process::Command) -> std::io::Result<i32> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        unix_common::foreground_cli_command(command)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = command;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "extension CLI commands are not supported yet on this platform",
        ))
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn launch_executable() -> std::io::Result<std::path::PathBuf> {
    std::env::current_exe()
}

pub(crate) fn detached_custom_command_process(command: &str) -> std::process::Command {
    let mut process = detached_custom_command_process_platform(command);
    configure_background_command(&mut process);
    process
}

pub(crate) fn pane_custom_command_pty_builder(command: &str) -> portable_pty::CommandBuilder {
    pane_custom_command_pty_builder_platform(command)
}

pub(crate) fn apply_pane_runtime_marker(command: &mut portable_pty::CommandBuilder) {
    apply_pane_runtime_marker_platform(command);
}

pub(crate) fn prepare_paste_text_for_pty(text: String) -> String {
    prepare_paste_text_for_pty_platform(text)
}

pub(crate) fn plugin_runtime_path(path: &std::path::Path) -> std::path::PathBuf {
    plugin_runtime_path_platform(path)
}

/// Launch a pane in the path the filesystem actually stores.
///
/// Windows directories are case-insensitive but case-preserving, so a restored
/// or reported cwd can differ in case from the on-disk name. Shells that print
/// their own cwd then disagree with the saved one.
pub(crate) fn normalize_cwd_for_launch(path: &std::path::Path) -> std::path::PathBuf {
    normalize_cwd_for_launch_platform(path)
}

#[cfg(not(windows))]
fn normalize_cwd_for_launch_platform(path: &std::path::Path) -> std::path::PathBuf {
    path.to_path_buf()
}

/// The pane's shell cwd. Unix has no launcher shim in front of the shell, so
/// this is the ordinary process cwd.
#[cfg(not(windows))]
pub(crate) use self::process_cwd as pane_process_cwd;

#[cfg(not(windows))]
fn plugin_runtime_path_platform(path: &std::path::Path) -> std::path::PathBuf {
    path.to_path_buf()
}

#[cfg(not(windows))]
fn prepare_paste_text_for_pty_platform(text: String) -> String {
    text
}

#[cfg(not(windows))]
pub(crate) fn terminal_title_for_presentation(title: &str) -> &str {
    title
}

#[cfg(not(windows))]
fn apply_pane_runtime_marker_platform(_command: &mut portable_pty::CommandBuilder) {}

pub(crate) fn configure_background_command(command: &mut std::process::Command) {
    configure_background_command_platform(command);
}

#[cfg(not(windows))]
fn configure_background_command_platform(_command: &mut std::process::Command) {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlatformCapabilities {
    pub(crate) live_handoff: bool,
    pub(crate) direct_terminal_attach: bool,
    pub(crate) preserve_legacy_doubled_escape_input: bool,
}

pub(crate) const fn capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        live_handoff: cfg!(unix),
        direct_terminal_attach: cfg!(unix),
        preserve_legacy_doubled_escape_input: cfg!(target_os = "macos"),
    }
}

/// The byte the host terminal sends for Backspace according to the tty's erase
/// setting (e.g. `^H` for MobaXterm and PuTTY-style terminals), when known.
pub(crate) fn terminal_erase_byte() -> Option<u8> {
    #[cfg(unix)]
    return unix_common::terminal_erase_byte();
    #[cfg(not(unix))]
    None
}

pub(crate) fn terminal_grid_size() -> std::io::Result<(u16, u16)> {
    #[cfg(unix)]
    let (cols, rows) = unix_common::read_terminal_grid_size()?;
    #[cfg(windows)]
    let (cols, rows) = windows::read_terminal_grid_size()?;
    #[cfg(not(any(unix, windows)))]
    let (cols, rows) = fallback::read_terminal_grid_size()?;

    if cols == 0 || rows == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "terminal reported a zero-sized grid",
        ));
    }
    Ok((cols, rows))
}

#[cfg(not(windows))]
pub fn launch_server_daemon_command(command: &mut std::process::Command) -> std::io::Result<u32> {
    command.spawn().map(|child| child.id())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn detach_server_daemon_command(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;

    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn current_process_is_detached_server_daemon() -> bool {
    unsafe { libc::getsid(0) == libc::getpid() }
}

/// Raised by the SIGWINCH handler, consumed by the host resize watcher.
#[cfg(unix)]
static TERMINAL_RESIZE_SIGNALLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn record_terminal_resize_signal(_signal: libc::c_int) {
    TERMINAL_RESIZE_SIGNALLED.store(true, std::sync::atomic::Ordering::Release);
}

/// Records SIGWINCH events that size polling can miss.
#[cfg(unix)]
pub(crate) fn watch_terminal_resize_signal() {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction =
        record_terminal_resize_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // Keep blocking stdin and socket reads from failing with EINTR.
    action.sa_flags = libc::SA_RESTART;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGWINCH, &action, std::ptr::null_mut());
    }
}

#[cfg(not(unix))]
pub(crate) fn watch_terminal_resize_signal() {}

/// Returns whether a terminal size change was signalled since the last call.
#[cfg(unix)]
pub(crate) fn take_terminal_resize_signal() -> bool {
    TERMINAL_RESIZE_SIGNALLED.swap(false, std::sync::atomic::Ordering::AcqRel)
}

/// Windows relies on size polling.
#[cfg(not(unix))]
pub(crate) fn take_terminal_resize_signal() -> bool {
    false
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardCommand {
    pub program: &'static str,
    pub args: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardImage {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LimitedRead {
    Empty,
    Complete(Vec<u8>),
    Oversized,
}

pub(crate) fn read_limited_reader(
    mut reader: impl std::io::Read,
    max_bytes: usize,
) -> std::io::Result<LimitedRead> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];

    while bytes.len() < max_bytes {
        let remaining = max_bytes - bytes.len();
        let read_len = remaining.min(buffer.len());
        let bytes_read = match reader.read(&mut buffer[..read_len]) {
            Ok(bytes_read) => bytes_read,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if bytes_read == 0 {
            return if bytes.is_empty() {
                Ok(LimitedRead::Empty)
            } else {
                Ok(LimitedRead::Complete(bytes))
            };
        }
        bytes.extend_from_slice(&buffer[..bytes_read]);
    }

    let mut sentinel = [0_u8; 1];
    loop {
        return match reader.read(&mut sentinel) {
            Ok(0) if bytes.is_empty() => Ok(LimitedRead::Empty),
            Ok(0) => Ok(LimitedRead::Complete(bytes)),
            Ok(_) => Ok(LimitedRead::Oversized),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => Err(err),
        };
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteSshConfigPaths {
    pub(crate) user_config: Option<std::path::PathBuf>,
    pub(crate) system_config: Option<std::path::PathBuf>,
    pub(crate) multiplexing: bool,
}

pub(crate) const REMOTE_BRIDGE_IDLE_TIMEOUT_SUPPORTED: bool =
    cfg!(any(target_os = "linux", target_os = "macos"));

#[cfg(unix)]
mod remote_bridge;
#[cfg(all(test, unix))]
mod remote_bridge_tests;
#[cfg(unix)]
mod unix_common;
/// Other Unix platforms keep their existing input admission policy; their
/// _PC_MAX_CANON values need not describe actual queue capacity.
#[cfg(all(unix, not(target_os = "macos")))]
#[derive(Clone, Default)]
pub(crate) struct PtyInputGuard;
#[cfg(all(unix, not(target_os = "macos")))]
impl PtyInputGuard {
    pub(crate) fn new(_master: &std::sync::Arc<std::fs::File>) -> Self {
        Self
    }
    pub(crate) fn validate(&self, _bytes: &[u8]) -> std::io::Result<()> {
        Ok(())
    }
}
#[cfg(unix)]
pub(crate) mod unix_image_files;

/// Kernel CoW snapshots are deliberately unsupported outside Linux.
#[cfg(not(target_os = "linux"))]
pub(crate) fn clone_native_image_source(
    _source_fd: i64,
    _destination: &std::fs::File,
    _expected_len: usize,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "native source cloning requires Linux",
    ))
}
#[cfg(unix)]
pub(crate) use unix_common::{
    begin_cli_output, configure_package_nofollow, create_private_package_directory, end_cli_output,
    forward_remote_bridge_stdio, ignore_server_hangup, open_package_staging,
    spawn_server_signal_monitor, write_package_staging_file, PackageStagingBackend,
    RemoteBridgeWake,
};
#[cfg(unix)]
pub(crate) use unix_common::{poll_fd_readable, read_fd, resource_symlink};

mod client_state;
mod package_fs;
mod persistence_lock;
pub(crate) use client_state::{
    create_private_state_file, durable_replace_file, replace_file, resolve_write_target,
    sync_containing_directory, sync_parent_directory,
};
pub(crate) use package_fs::{read_bounded_regular_nofollow, SecurePackageStaging};
pub(crate) use persistence_lock::{
    try_lock_persistence, PersistenceLockError, PersistenceLockGuard,
};

#[cfg(not(unix))]
pub(crate) fn begin_cli_output() {}

#[cfg(not(unix))]
pub(crate) fn end_cli_output() {}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod fallback;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub use fallback::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn available_pane_shell_from_job(child_pid: u32, job: ForegroundJob) -> Option<String> {
    if job.process_group_id != child_pid
        || job.processes.iter().any(|process| process.pid != child_pid)
    {
        return None;
    }
    job.processes
        .into_iter()
        .find(|process| process.pid == child_pid)
        .map(|process| process.name)
        .filter(|name| is_pane_shell_process_name(name))
}

fn normalized_process_name(name: &str) -> String {
    name.rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim_start_matches('-')
        .trim_end_matches(".exe")
        .to_ascii_lowercase()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn is_powershell_process_name(name: &str) -> bool {
    matches!(
        normalized_process_name(name).as_str(),
        "pwsh" | "powershell"
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn interactive_unix_shell_command(
    argv: &[String],
    shell_name: &str,
    quote_posix_arg: fn(&str) -> String,
) -> Option<String> {
    let quote = if is_powershell_process_name(shell_name) {
        quote_powershell_arg
    } else {
        quote_posix_arg
    };
    let mut parts = argv.iter();
    let mut command = quote(parts.next()?);
    for part in parts {
        command.push(' ');
        command.push_str(&quote(part));
    }
    Some(command)
}

pub(crate) fn quote_powershell_arg(value: &str) -> String {
    if !value.is_empty()
        && !value.starts_with('-')
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':' | b'+' | b'=')
        })
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "''"))
}

pub(crate) fn quote_windows_command_line_arg(value: &str) -> String {
    if !value.is_empty()
        && !value
            .chars()
            .any(|ch| matches!(ch, ' ' | '\t' | '\n' | '\x0b' | '"'))
    {
        return value.to_string();
    }

    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            backslashes += 1;
            continue;
        }
        if ch == '"' {
            quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
        } else {
            quoted.push_str(&"\\".repeat(backslashes));
        }
        backslashes = 0;
        quoted.push(ch);
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

pub(crate) fn is_pane_shell_process_name(name: &str) -> bool {
    let normalized = normalized_process_name(name);
    matches!(
        normalized.as_str(),
        "sh" | "bash"
            | "dash"
            | "zsh"
            | "fish"
            | "ksh"
            | "mksh"
            | "csh"
            | "tcsh"
            | "elvish"
            | "xonsh"
            | "nu"
            | "pwsh"
            | "powershell"
            | "cmd"
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_agent_hint(_pid: u32) -> Option<crate::detect::Agent> {
    None
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn parse_agent_env_hint(environ: &[u8]) -> Option<crate::detect::Agent> {
    for record in environ.split(|&byte| byte == 0) {
        let Some(value) = record.strip_prefix(b"HERDR_AGENT=") else {
            continue;
        };
        return crate::detect::parse_agent_label(std::str::from_utf8(value).ok()?);
    }
    None
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[derive(Debug)]
pub(crate) struct InputSourceRestore;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn switch_to_ascii_input_source() -> Option<InputSourceRestore> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) fn pump_input_source_runloop() {}

/// Switches the host keyboard input source while prefix mode is active.
///
/// `App` drives this through a trait so the prefix-mode transitions can be
/// tested with a fake, without touching the real macOS APIs or leaking a
/// platform-specific restore type into `App`.
pub(crate) trait PrefixInputSource {
    /// Switch to an ASCII-capable input source for prefix commands. No-op if
    /// the current source is already ASCII-capable, the platform is
    /// unsupported, or the switch fails. Calling it again before `restore`
    /// keeps the source saved by the first call.
    fn switch_to_ascii(&mut self);

    /// Restore whatever `switch_to_ascii` saved. No-op if nothing was switched.
    fn restore(&mut self);
}

/// Production [`PrefixInputSource`] backed by the per-platform API.
#[derive(Default)]
pub(crate) struct RealPrefixInputSource {
    restore: Option<InputSourceRestore>,
}

impl PrefixInputSource for RealPrefixInputSource {
    fn switch_to_ascii(&mut self) {
        if self.restore.is_none() {
            // Drain pending input-source-change notifications so the read below is fresh (see
            // `pump_input_source_runloop`); a no-op on non-macOS.
            pump_input_source_runloop();
            self.restore = switch_to_ascii_input_source();
        }
    }

    fn restore(&mut self) {
        let _ = self.restore.take();
    }
}

#[cfg(all(test, any(unix, windows)))]
#[test]
fn child_exit_classification_only_checkpoints_interruptions() {
    for code in [0, 1, 130, 255, 0xC0000005] {
        let reason = classify_child_exit(&portable_pty::ExitStatus::with_exit_code(code));
        assert_eq!(reason, ChildExitReason::Exited, "exit code {code:#x}");
        assert!(!reason.requires_session_checkpoint());
    }
    #[cfg(windows)]
    let status = portable_pty::ExitStatus::with_exit_code(0xC000013A);
    #[cfg(not(windows))]
    let status = portable_pty::ExitStatus::with_signal("Terminated: 15");
    assert_eq!(classify_child_exit(&status), ChildExitReason::Interrupted);
    assert!(classify_child_exit(&status).requires_session_checkpoint());
    #[cfg(unix)]
    assert!(ChildExitReason::Handoff.requires_session_checkpoint());
    assert!(!ChildExitReason::WaitFailed.requires_session_checkpoint());
}

#[cfg(test)]
mod invocation_identity_tests {
    use super::*;
    use crate::action_invocation::{InvocationPeer, InvocationProcess};

    fn process(pid: u32, parent_pid: u32) -> InvocationProcess {
        let name = if pid == 20 { "pi" } else { "bash" };
        InvocationProcess {
            pid,
            parent_pid,
            birth: u64::from(pid) * 10,
            process: ForegroundProcess {
                pid,
                name: name.into(),
                argv0: Some(name.into()),
                argv: Some(vec![name.into()]),
                cmdline: Some(name.into()),
            },
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn invocation_process_observation_uses_host_uid_not_placeholder() {
        let pid = std::process::id();
        let peer = invocation_peer_for_process(pid).expect("current same-user process ancestry");
        // SAFETY: geteuid has no arguments or memory preconditions.
        assert_eq!(peer.uid, unsafe { libc::geteuid() });
        let first = peer.ancestry.first().expect("observed process");
        assert_eq!(first.pid, pid);
        assert_eq!(Some(first.birth), invocation_process_birth(pid));
        assert!(invocation_peer_is_current(&peer));
    }

    #[test]
    fn invocation_capture_excludes_positive_boundary_and_requires_owned_root_inside() {
        for boundary in [1, 10, 20] {
            let ancestry = capture_invocation_ancestry(20, |pid| {
                Some(if pid == boundary {
                    InvocationAncestor::Boundary
                } else {
                    InvocationAncestor::Process(process(pid, if pid == 20 { 10 } else { 1 }))
                })
            });
            if boundary == 20 {
                assert!(ancestry.is_none());
                continue;
            }
            let peer = InvocationPeer {
                uid: 501,
                boot_id: "boot".into(),
                ancestry: ancestry.expect("same-user prefix"),
            };
            assert!(peer.ancestry.iter().all(|entry| entry.pid != boundary));
            let execution = crate::action_invocation::observed_execution(
                &peer,
                "term".into(),
                10,
                Some(100),
                20,
                Some(crate::detect::Agent::Pi),
            );
            assert_eq!(execution, (boundary == 1).then(|| "boot:20:200".into()));
        }
    }

    #[test]
    fn invocation_capture_rejects_errors_mismatched_pid_and_cycles() {
        assert!(capture_invocation_ancestry(0, |_| panic!("must not read PID zero")).is_none());
        assert!(capture_invocation_ancestry(20, |_| None).is_none());
        assert!(capture_invocation_ancestry(20, |_| {
            Some(InvocationAncestor::Process(process(21, 0)))
        })
        .is_none());
        assert!(
            capture_invocation_ancestry(20, |pid| {
                (pid == 20).then(|| InvocationAncestor::Process(process(pid, 10)))
            })
            .is_none(),
            "missing ancestor metadata is not a boundary"
        );
        assert!(capture_invocation_ancestry(20, |pid| {
            Some(InvocationAncestor::Process(process(
                pid,
                if pid == 20 { 10 } else { 20 },
            )))
        })
        .is_none());
    }

    #[test]
    fn invocation_capture_is_bounded_and_cannot_prove_root_beyond_bound() {
        let mut reads = 0;
        let ancestry = capture_invocation_ancestry(1, |pid| {
            reads += 1;
            Some(InvocationAncestor::Process(process(pid, pid + 1)))
        })
        .expect("bounded prefix");
        assert_eq!(reads, INVOCATION_ANCESTRY_LIMIT);
        assert_eq!(ancestry.len(), INVOCATION_ANCESTRY_LIMIT);
        assert!(!ancestry.iter().any(|entry| entry.pid == 65));
        let peer = InvocationPeer {
            uid: 501,
            boot_id: "boot".into(),
            ancestry,
        };
        assert_eq!(
            crate::action_invocation::observed_execution(
                &peer,
                "term".into(),
                65,
                Some(650),
                20,
                Some(crate::detect::Agent::Pi),
            ),
            None
        );
        assert!(
            capture_invocation_ancestry(1, |pid| {
                Some(InvocationAncestor::Process(process(
                    pid,
                    if pid == 64 { 1 } else { pid + 1 },
                )))
            })
            .is_none(),
            "a cycle closing at the bound is rejected too"
        );
    }

    #[test]
    fn invocation_currentness_rejects_pid_reuse_reparenting_and_missing_identity() {
        let ancestry = vec![process(20, 10), process(10, 0)];
        assert!(invocation_ancestry_is_current(&ancestry, |pid| {
            Some((u64::from(pid) * 10, if pid == 20 { 10 } else { 0 }))
        }));
        for replacement in [None, Some((201, 10)), Some((200, 11))] {
            assert!(!invocation_ancestry_is_current(&ancestry, |pid| {
                if pid == 20 {
                    replacement
                } else {
                    Some((100, 0))
                }
            }));
        }
        assert!(!invocation_ancestry_is_current(&[], |_| unreachable!()));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn terminal_resize_signal_is_recorded_once_per_delivery() {
        watch_terminal_resize_signal();
        assert!(!take_terminal_resize_signal());

        unsafe {
            libc::raise(libc::SIGWINCH);
        }

        assert!(take_terminal_resize_signal());
        assert!(!take_terminal_resize_signal());
    }

    #[test]
    fn pane_shell_process_names_reject_exec_replacement_programs() {
        for shell in ["bash", "-zsh", "/bin/fish", "pwsh", "powershell.exe"] {
            assert!(is_pane_shell_process_name(shell), "{shell}");
        }
        for program in ["vim", "nvim", "cargo", "test-runner", "opencode"] {
            assert!(!is_pane_shell_process_name(program), "{program}");
        }
    }

    #[test]
    fn detached_custom_command_preserves_unix_login_shell_flag() {
        let cmd = detached_custom_command_process("echo hello");
        assert_eq!(cmd.get_program(), std::ffi::OsStr::new("/bin/sh"));
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            [
                std::ffi::OsStr::new("-lc"),
                std::ffi::OsStr::new("echo hello")
            ]
        );
    }

    #[test]
    fn pane_custom_command_builder_preserves_unix_shell_flag() {
        let expected: Vec<std::ffi::OsString> =
            vec!["/bin/sh".into(), "-c".into(), "echo hello".into()];
        assert_eq!(
            pane_custom_command_pty_builder("echo hello").get_argv(),
            &expected
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn parse_agent_env_hint_accepts_known_agents() {
        assert_eq!(
            parse_agent_env_hint(b"PATH=/bin\0HERDR_AGENT=claude\0TERM=xterm\0"),
            Some(crate::detect::Agent::Claude)
        );
        assert_eq!(
            parse_agent_env_hint(b"HERDR_AGENT=codex"),
            Some(crate::detect::Agent::Codex)
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn parse_agent_env_hint_ignores_missing_or_unknown_agents() {
        assert_eq!(parse_agent_env_hint(b"PATH=/bin\0TERM=xterm\0"), None);
        assert_eq!(parse_agent_env_hint(b"HERDR_AGENT=not-an-agent\0"), None);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn interactive_shell_command_quotes_for_posix_and_powershell() {
        let argv = vec![
            "pi".into(),
            String::new(),
            "two words".into(),
            "a'b".into(),
            "$HOME".into(),
            "semi;colon".into(),
            "@options".into(),
        ];
        assert_eq!(
            interactive_shell_command(&argv, "bash").as_deref(),
            Some("pi '' 'two words' 'a'\\''b' '$HOME' 'semi;colon' @options")
        );
        assert_eq!(
            interactive_shell_command(&argv, "pwsh").as_deref(),
            Some("pi '' 'two words' 'a''b' '$HOME' 'semi;colon' '@options'")
        );
    }

    #[test]
    fn read_limited_reader_returns_complete_data_under_limit() {
        let input = std::io::Cursor::new(b"image".to_vec());
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Complete(b"image".to_vec())
        );
    }

    #[test]
    fn read_limited_reader_returns_empty_for_empty_input() {
        let input = std::io::Cursor::new(Vec::<u8>::new());
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Empty
        );
    }

    #[test]
    fn read_limited_reader_accepts_data_exactly_at_limit() {
        let input = std::io::Cursor::new(b"four".to_vec());
        assert_eq!(
            read_limited_reader(input, 4).expect("limited read"),
            LimitedRead::Complete(b"four".to_vec())
        );
    }

    #[test]
    fn read_limited_reader_rejects_data_over_limit() {
        let input = std::io::Cursor::new(b"oversized".to_vec());
        assert_eq!(
            read_limited_reader(input, 4).expect("limited read"),
            LimitedRead::Oversized
        );
    }

    #[test]
    fn read_limited_reader_retries_interrupted_reads() {
        struct InterruptedOnce {
            interrupted: bool,
            inner: std::io::Cursor<Vec<u8>>,
        }

        impl std::io::Read for InterruptedOnce {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                self.inner.read(buffer)
            }
        }

        let input = InterruptedOnce {
            interrupted: false,
            inner: std::io::Cursor::new(b"image".to_vec()),
        };
        assert_eq!(
            read_limited_reader(input, 16).expect("limited read"),
            LimitedRead::Complete(b"image".to_vec())
        );
    }

    #[test]
    fn windows_pipe_sddl_authorizes_the_account_and_bounds_integrity() {
        assert_eq!(
            super::windows_local_pipe_sddl(
                super::WindowsPipeEndpoint::Api,
                "S-1-5-21-1-2-3-1001",
                super::WindowsPipeIntegrity::High,
            ),
            "D:P(A;;GA;;;SY)(A;;GRGW;;;S-1-5-21-1-2-3-1001)S:(ML;;NRNW;;;HI)"
        );
        assert_eq!(
            super::windows_local_pipe_sddl(
                super::WindowsPipeEndpoint::Terminal,
                "S-1-5-21-1-2-3-1001",
                super::WindowsPipeIntegrity::Medium,
            ),
            "D:P(A;;GA;;;SY)(A;;GA;;;S-1-5-21-1-2-3-1001)S:(ML;;NRNW;;;ME)"
        );
        // Both endpoints deny read and write below their integrity label.
        for endpoint in [
            super::WindowsPipeEndpoint::Api,
            super::WindowsPipeEndpoint::Terminal,
        ] {
            let sddl = super::windows_local_pipe_sddl(
                endpoint,
                "S-1-5-18",
                super::WindowsPipeIntegrity::High,
            );
            assert!(sddl.ends_with("S:(ML;;NRNW;;;HI)"), "{sddl}");
            assert!(sddl.contains("(A;;GA;;;SY)"), "{sddl}");
        }
    }

    #[test]
    fn elevated_servers_need_consent_before_admitting_ordinary_clients() {
        use super::{windows_pipe_integrity, WindowsPipeIntegrity};
        assert_eq!(
            windows_pipe_integrity(true, false),
            WindowsPipeIntegrity::High,
            "an elevated server refuses ordinary same-account clients by default"
        );
        assert_eq!(
            windows_pipe_integrity(true, true),
            WindowsPipeIntegrity::Medium,
            "explicit consent admits ordinary same-account clients"
        );
        assert_eq!(
            windows_pipe_integrity(false, false),
            WindowsPipeIntegrity::Medium,
            "an ordinary server is reachable by its own account"
        );
    }
}
