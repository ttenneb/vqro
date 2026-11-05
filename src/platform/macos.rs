use std::ffi::OsStr;
use std::io::Write;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::ptr::NonNull;
use std::sync::OnceLock;

mod shutdown;
pub(crate) use shutdown::monitor_host_shutdown;

pub(super) const REMOTE_BRIDGE_CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;

use super::{
    read_limited_reader, ClipboardCommand, ClipboardImage, ForegroundJob, ForegroundProcess,
    LimitedRead, Signal,
};

pub(crate) use super::unix_common::{
    configure_extension_service_command, configure_status_command, create_remote_private_dir,
    create_remote_ssh_config_dir, create_remote_ssh_config_file, fill_secure_random, hostname,
    local_datetime, local_listener_accept_error_is_resource_exhaustion,
    remote_bridge_endpoint_path, remote_private_temp_base, remote_reattach_argument,
    remote_reattach_program, remote_ssh_config_paths, set_default_plugin_pane_pwd,
    shutdown_client_stream, status_commands_supported, terminate_extension_service_process_group,
    try_lock_platform_persistence, wait_client_stream_readable, write_client_stream,
    ClientStreamReader, ExtensionServiceContainment, PlatformPersistenceLock, StatusCommandGuard,
};

const PROC_PGRP_ONLY: u32 = 2;
// Darwin proc_info.h pbi_flags ABI; libc does not expose this flag.
const PROC_FLAG_LP64: u32 = 0x10;
const SERVER_NOFILE_LIMIT_TARGET: libc::rlim_t = 8192;

/// Kernel credentials for the connected peer, not caller-supplied environment.
fn socket_peer_credentials(stream: &crate::ipc::LocalStream) -> Option<(u32, u32)> {
    use std::os::fd::{AsFd as _, AsRawFd as _};
    let crate::ipc::LocalStream::UdSocket(stream) = stream;
    socket_peer_credentials_fd(stream.as_fd().as_raw_fd())
}

fn socket_peer_credentials_fd(fd: RawFd) -> Option<(u32, u32)> {
    let mut pid: libc::pid_t = 0;
    let mut size = std::mem::size_of_val(&pid) as libc::socklen_t;
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: both calls receive live, correctly sized output storage. A bad or
    // disconnected descriptor is an error, never a fallback identity.
    unsafe {
        if libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut pid as *mut libc::pid_t).cast(),
            &mut size,
        ) != 0
            || size as usize != std::mem::size_of_val(&pid)
            || pid <= 0
            || libc::getpeereid(fd, &mut uid, &mut gid) != 0
            || uid != libc::geteuid()
        {
            return None;
        }
    }
    Some((u32::try_from(pid).ok()?, uid))
}

pub(crate) fn client_ancestry_complete(peer: &crate::action_invocation::InvocationPeer) -> bool {
    // A foreign-euid boundary remains valid for legacy positive owned-root
    // attribution, but cannot prove that no server/agent exists above it.
    // New external-client authority requires the genuine complete root chain;
    // otherwise this client stays read-only. Do not query foreign argv here.
    peer.ancestry.len() <= 64
        && peer
            .ancestry
            .last()
            .is_some_and(|last| last.parent_pid == 0)
}

pub(crate) fn ownership_runtime_base() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let mut bytes = vec![0_u8; 4096];
    // SAFETY: confstr receives a writable buffer of the stated size.
    let needed = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            bytes.as_mut_ptr().cast(),
            bytes.len(),
        )
    };
    if needed == 0 || needed > bytes.len() {
        return None;
    }
    bytes.truncate(needed.saturating_sub(1));
    let root = PathBuf::from(std::ffi::OsString::from_vec(bytes));
    super::validate_private_ownership_path(&root, true).ok()?;
    Some(root)
}

pub(crate) fn ownership_server_key() -> Option<crate::pty_ownership::ServerKey> {
    Some(crate::pty_ownership::ServerKey {
        boot_id: invocation_boot_id()?,
        pid: std::process::id(),
        birth: invocation_process_birth(std::process::id())?,
    })
}

fn copied_tty_device_name(get: impl FnOnce() -> *mut libc::c_char) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    static NAMES: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = NAMES.lock().ok()?;
    let pointer = get();
    if pointer.is_null() {
        return None;
    }
    // SAFETY: devname/ptsname return a NUL-terminated static C string or NULL;
    // copy while serialized rather than retain the shared library buffer.
    let bytes = unsafe { std::ffi::CStr::from_ptr(pointer) }.to_bytes();
    if bytes.is_empty() || bytes.len() > 4096 {
        return None;
    }
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

pub(crate) fn pty_slave_identity_from_master(
    fd: RawFd,
) -> Option<crate::pty_ownership::TtyIdentity> {
    // SAFETY: fd is the owned imported/fresh PTY master, and its name is copied.
    let path = copied_tty_device_name(|| unsafe { libc::ptsname(fd) })?;
    super::tty_identity_from_path(&path)
}

pub(crate) fn client_controlling_tty(pid: u32) -> Option<(crate::pty_ownership::TtyIdentity, u64)> {
    let before = invocation_bsdinfo(pid)?;
    let birth = invocation_birth(&before)?;
    if before.e_tdev == u32::MAX || before.e_tpgid == 0 || before.e_tpgid == u32::MAX {
        return None;
    }
    // SAFETY: devname resolves a kernel-observed character-device number.
    let name = copied_tty_device_name(|| unsafe {
        libc::devname(before.e_tdev as libc::dev_t, libc::S_IFCHR as libc::mode_t)
    })?;
    if name.components().count() != 1 {
        return None;
    }
    let identity = super::tty_identity_from_path(&Path::new("/dev").join(name))?;
    let after = invocation_bsdinfo(pid)?;
    if identity.rdev != u64::from(before.e_tdev)
        || after.e_tdev != before.e_tdev
        || invocation_birth(&after) != Some(birth)
        || after.pbi_uid != before.pbi_uid
    {
        return None;
    }
    Some((identity, birth))
}

pub(crate) fn socket_peer_birth(stream: &crate::ipc::LocalStream) -> Option<(u32, u64)> {
    let (pid, _) = socket_peer_credentials(stream)?;
    Some((pid, invocation_process_birth(pid)?))
}

pub(crate) fn invocation_peer(
    stream: &crate::ipc::LocalStream,
) -> Option<crate::action_invocation::InvocationPeer> {
    let (pid, _) = socket_peer_credentials(stream)?;
    invocation_peer_for_process(pid)
}

pub(crate) fn invocation_peer_for_process(
    pid: u32,
) -> Option<crate::action_invocation::InvocationPeer> {
    // Socket callers already checked kernel euid. Explicit process observations
    // are restricted to the same host account too, not a synthetic UID.
    // SAFETY: geteuid has no arguments or memory preconditions.
    let uid = unsafe { libc::geteuid() };
    let boot_id = invocation_boot_id()?;
    let ancestry = super::capture_invocation_ancestry(pid, |pid| {
        let info = invocation_bsdinfo(pid)?;
        if info.pbi_uid != uid {
            return Some(super::InvocationAncestor::Boundary);
        }
        let birth = invocation_birth(&info)?;
        // KERN_PROCARGS2 includes environment too, so bound that acquisition
        // separately from the Linux-equivalent 16 KiB argument limit.
        let argv =
            invocation_procargs2_argv(&kern_procargs2_bounded(pid, 1024 * 1024)?, info.pbi_flags)?;
        let argv_bytes = argv.iter().try_fold(0usize, |total, arg| {
            total.checked_add(arg.len())?.checked_add(1)
        })?;
        if argv_bytes > 16384 {
            return None;
        }
        let name = comm_from_bsdinfo(&info)?;
        let current = invocation_bsdinfo(pid)?;
        if current.pbi_uid != uid
            || (current.pbi_flags & PROC_FLAG_LP64) != (info.pbi_flags & PROC_FLAG_LP64)
            || current.pbi_ppid != info.pbi_ppid
            || invocation_birth(&current) != Some(birth)
        {
            return None;
        }
        Some(super::InvocationAncestor::Process(
            crate::action_invocation::InvocationProcess {
                pid,
                parent_pid: info.pbi_ppid,
                birth,
                process: ForegroundProcess {
                    pid,
                    name,
                    argv0: argv.first().cloned(),
                    cmdline: Some(argv.join(" ")),
                    argv: Some(argv),
                },
            },
        ))
    })?;
    let peer = crate::action_invocation::InvocationPeer {
        account: crate::action_invocation::InvocationAccount::Unix { uid },
        boot_id,
        ancestry,
        foreground_cli: false,
        foreground: None,
    };
    invocation_peer_is_current(&peer).then_some(peer)
}

pub(crate) fn invocation_peer_is_current(peer: &crate::action_invocation::InvocationPeer) -> bool {
    let crate::action_invocation::InvocationAccount::Unix { uid } = &peer.account else {
        return false;
    };
    // SAFETY: geteuid has no arguments or memory preconditions.
    if *uid != unsafe { libc::geteuid() } {
        return false;
    }
    super::invocation_ancestry_is_current(&peer.ancestry, |pid| {
        let info = invocation_bsdinfo(pid)?;
        (info.pbi_uid == *uid).then_some((invocation_birth(&info)?, info.pbi_ppid))
    })
}

fn invocation_bsdinfo(pid: u32) -> Option<libc::proc_bsdinfo> {
    if pid == 0 || libc::pid_t::try_from(pid).is_err() {
        return None;
    }
    let info = process_bsdinfo(pid)?;
    (info.pbi_pid == pid && invocation_birth(&info).is_some()).then_some(info)
}

fn invocation_birth(info: &libc::proc_bsdinfo) -> Option<u64> {
    if info.pbi_start_tvsec == 0 || info.pbi_start_tvusec >= 1_000_000 {
        return None;
    }
    info.pbi_start_tvsec
        .checked_mul(1_000_000)?
        .checked_add(info.pbi_start_tvusec)
}

pub(crate) fn invocation_process_birth(pid: u32) -> Option<u64> {
    invocation_birth(&invocation_bsdinfo(pid)?)
}

fn invocation_boot_id() -> Option<String> {
    // A UUID including its NUL fits exactly in this fixed-size buffer. Never
    // substitute a host UUID or a random per-server value across restarts.
    let mut bytes = [0u8; 37];
    let mut size = bytes.len();
    // SAFETY: the name is NUL-terminated and bytes/size are writable storage;
    // null newp makes this a read-only query.
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || size != bytes.len() || bytes[36] != 0 {
        return None;
    }
    let value = std::str::from_utf8(&bytes[..36]).ok()?;
    if !value.bytes().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            byte == b'-'
        } else {
            byte.is_ascii_hexdigit()
        }
    }) {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

pub(crate) fn should_draw_host_cursor_by_default() -> bool {
    false
}

pub(crate) fn should_query_host_terminal_palette() -> bool {
    true
}

fn raw_command_argv(command: &str, flag: &str) -> Vec<std::ffi::OsString> {
    vec!["/bin/sh".into(), flag.into(), command.into()]
}

pub(crate) fn detached_custom_command_process_platform(command: &str) -> std::process::Command {
    let argv = raw_command_argv(command, "-lc");
    let mut command = std::process::Command::new(&argv[0]);
    command.args(&argv[1..]);
    command
}

pub(crate) fn pane_custom_command_pty_builder_platform(
    command: &str,
) -> portable_pty::CommandBuilder {
    portable_pty::CommandBuilder::from_argv(raw_command_argv(command, "-c"))
}

pub(crate) fn scrollback_editor_argv(path: &Path) -> std::io::Result<Vec<String>> {
    let quoted_path = shell_quote(&path.display().to_string());
    let command = format!(
        r#"scrollback_file={quoted_path}; eval "${{EDITOR:-vi}} \"\$scrollback_file\""; status=$?; rm -f "$scrollback_file"; exit $status"#
    );
    Ok(vec!["/bin/sh".to_string(), "-c".to_string(), command])
}

pub(crate) fn interactive_shell_command(argv: &[String], shell_name: &str) -> Option<String> {
    super::interactive_unix_shell_command(argv, shell_name, shell_quote)
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

#[repr(C)]
struct TisInputSource {
    _private: [u8; 0],
}

type TisInputSourceRef = *const TisInputSource;
type CfTypeRef = *const libc::c_void;
type CfStringRef = *const libc::c_void;
type OsStatus = libc::c_int;
type Boolean = libc::c_uchar;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    #[link_name = "kTISPropertyInputSourceID"]
    static TIS_PROPERTY_INPUT_SOURCE_ID: CfStringRef;

    #[link_name = "TISCopyCurrentKeyboardInputSource"]
    fn tis_copy_current_keyboard_input_source() -> TisInputSourceRef;

    #[link_name = "TISCopyCurrentASCIICapableKeyboardLayoutInputSource"]
    fn tis_copy_current_ascii_capable_keyboard_layout_input_source() -> TisInputSourceRef;

    #[link_name = "TISGetInputSourceProperty"]
    fn tis_get_input_source_property(
        input_source: TisInputSourceRef,
        property_key: CfStringRef,
    ) -> CfTypeRef;

    #[link_name = "TISSelectInputSource"]
    fn tis_select_input_source(input_source: TisInputSourceRef) -> OsStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    #[link_name = "CFRelease"]
    fn cf_release(value: CfTypeRef);

    #[link_name = "CFEqual"]
    fn cf_equal(left: CfTypeRef, right: CfTypeRef) -> Boolean;

    #[link_name = "kCFRunLoopDefaultMode"]
    static CF_RUN_LOOP_DEFAULT_MODE: CfStringRef;

    #[link_name = "CFRunLoopRunInMode"]
    fn cf_run_loop_run_in_mode(
        mode: CfStringRef,
        seconds: f64,
        return_after_source_handled: Boolean,
    ) -> libc::c_int;
}

/// Pump the main thread's run loop once (non-blocking) so the process receives the
/// `kTISNotifySelectedKeyboardInputSourceChanged` notification and refreshes the per-process cache
/// that `TISCopyCurrentKeyboardInputSource` reads. That notification arrives only via the main
/// thread's run loop, so a process that never runs a CFRunLoop (the headless server) reads a stale
/// source. Must run on the main thread.
pub(crate) fn pump_input_source_runloop() {
    debug_assert!(
        // SAFETY: `pthread_main_np` is always safe to call.
        unsafe { libc::pthread_main_np() } != 0,
        "pump_input_source_runloop must run on the main thread"
    );
    // SAFETY: `CFRunLoopRunInMode` is thread-safe; a 0-second call drains the ready sources and
    // returns immediately (no blocking). `CF_RUN_LOOP_DEFAULT_MODE` is a framework-owned constant.
    unsafe {
        let _ = cf_run_loop_run_in_mode(CF_RUN_LOOP_DEFAULT_MODE, 0.0, 0);
    }
}

#[derive(Debug)]
struct RetainedInputSource(NonNull<TisInputSource>);

impl RetainedInputSource {
    /// Takes ownership of a retained reference returned by a TIS `Copy` function.
    unsafe fn from_copy(raw: TisInputSourceRef) -> Option<Self> {
        NonNull::new(raw as *mut TisInputSource).map(Self)
    }

    fn select(&self) -> OsStatus {
        // SAFETY: this wrapper keeps the retained input source alive for the call.
        unsafe { tis_select_input_source(self.0.as_ptr()) }
    }

    fn has_same_id(&self, other: &Self) -> bool {
        // SAFETY: TIS property values stay valid while their input sources are alive;
        // both wrappers outlive this comparison.
        unsafe {
            let left = tis_get_input_source_property(self.0.as_ptr(), TIS_PROPERTY_INPUT_SOURCE_ID);
            let right =
                tis_get_input_source_property(other.0.as_ptr(), TIS_PROPERTY_INPUT_SOURCE_ID);
            !left.is_null() && !right.is_null() && cf_equal(left, right) != 0
        }
    }
}

impl Drop for RetainedInputSource {
    fn drop(&mut self) {
        // SAFETY: `from_copy` gives this wrapper ownership of one retain.
        unsafe { cf_release(self.0.as_ptr().cast()) }
    }
}

#[derive(Debug)]
pub(crate) struct InputSourceRestore {
    previous: RetainedInputSource,
}

impl Drop for InputSourceRestore {
    fn drop(&mut self) {
        let status = self.previous.select();
        if status != 0 {
            tracing::debug!(
                status,
                "failed to restore host input source after prefix mode"
            );
        }
    }
}

pub(crate) fn switch_to_ascii_input_source() -> Option<InputSourceRestore> {
    // SAFETY: both Carbon `Copy` functions transfer one retain to the caller.
    let current =
        unsafe { RetainedInputSource::from_copy(tis_copy_current_keyboard_input_source())? };
    let ascii = unsafe {
        RetainedInputSource::from_copy(
            tis_copy_current_ascii_capable_keyboard_layout_input_source(),
        )?
    };

    if current.has_same_id(&ascii) {
        return None;
    }

    let status = ascii.select();
    if status != 0 {
        tracing::debug!(status, "failed to switch host input source for prefix mode");
        return None;
    }

    Some(InputSourceRestore { previous: current })
}

pub fn raise_server_nofile_limit() {
    match raise_nofile_limit(SERVER_NOFILE_LIMIT_TARGET) {
        Ok(None) => {}
        Ok(Some((previous, target))) => {
            tracing::info!(previous, target, "raised server file descriptor soft limit")
        }
        Err(err) => tracing::warn!(err = %err, "failed to raise server file descriptor limit"),
    }
}

fn raise_nofile_limit(
    target: libc::rlim_t,
) -> std::io::Result<Option<(libc::rlim_t, libc::rlim_t)>> {
    let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }

    let mut limit = unsafe { limit.assume_init() };
    let Some(target) = target_nofile_soft_limit(limit.rlim_cur, limit.rlim_max, target) else {
        return Ok(None);
    };

    let previous = limit.rlim_cur;
    limit.rlim_cur = target;
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(Some((previous, target)))
}

fn target_nofile_soft_limit(
    current: libc::rlim_t,
    hard: libc::rlim_t,
    target: libc::rlim_t,
) -> Option<libc::rlim_t> {
    let target = if hard == libc::RLIM_INFINITY {
        target
    } else {
        target.min(hard)
    };

    (current < target).then_some(target)
}

pub(crate) fn available_pane_shell(child_pid: u32) -> Option<String> {
    super::available_pane_shell_from_job(child_pid, foreground_job(child_pid)?)
}

/// Collect the foreground terminal job for a given child PID.
pub fn foreground_job(child_pid: u32) -> Option<ForegroundJob> {
    if child_pid == 0 {
        return None;
    }

    let fg_pgid = foreground_process_group_id(child_pid)?;
    let mut processes = Vec::new();

    for pid in process_group_pids(fg_pgid) {
        let Some(info) = process_bsdinfo(pid) else {
            continue;
        };
        if info.pbi_pgid != fg_pgid {
            continue;
        }

        let Some(name) = comm_from_bsdinfo(&info) else {
            continue;
        };
        let argv = process_argv(pid);
        processes.push(ForegroundProcess {
            pid,
            name,
            argv0: process_argv0_name(pid),
            cmdline: argv.as_ref().map(|parts| parts.join(" ")),
            argv,
        });
    }

    if processes.is_empty() {
        return None;
    }

    Some(ForegroundJob {
        process_group_id: fg_pgid,
        processes,
    })
}

pub fn foreground_group_leader_job(process_group_id: u32) -> Option<ForegroundJob> {
    let info = process_bsdinfo(process_group_id)?;
    if info.pbi_pgid != process_group_id {
        return None;
    }

    let name = comm_from_bsdinfo(&info)?;
    let argv = process_argv(process_group_id);
    Some(ForegroundJob {
        process_group_id,
        processes: vec![ForegroundProcess {
            pid: process_group_id,
            name,
            argv0: process_argv0_name(process_group_id),
            cmdline: argv.as_ref().map(|parts| parts.join(" ")),
            argv,
        }],
    })
}

fn process_group_pids(process_group_id: u32) -> Vec<u32> {
    let mut capacity = 16usize;

    for _ in 0..8 {
        let mut pids = vec![0 as libc::pid_t; capacity];
        let buffer_bytes = pids.len() * std::mem::size_of::<libc::pid_t>();
        let returned_bytes = unsafe {
            libc::proc_listpids(
                PROC_PGRP_ONLY,
                process_group_id,
                pids.as_mut_ptr() as *mut libc::c_void,
                buffer_bytes as libc::c_int,
            )
        };
        if returned_bytes <= 0 {
            return Vec::new();
        }

        let returned_bytes = returned_bytes as usize;
        let count = returned_bytes / std::mem::size_of::<libc::pid_t>();
        if returned_bytes < buffer_bytes {
            return collect_positive_pids(pids, count);
        }
        capacity = capacity.saturating_mul(2);
    }

    Vec::new()
}

/// Read `e_tpgid` (foreground process group of the controlling terminal)
/// for the given PID.
pub fn foreground_process_group_id(pid: u32) -> Option<u32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;

    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };

    if ret != size {
        return None;
    }

    let fg = info.e_tpgid;
    if fg > 0 {
        #[allow(clippy::unnecessary_cast)] // info.e_tpgid (pid_t) type is platform-dependent
        Some(fg as u32)
    } else {
        None
    }
}

pub fn foreground_process_group_id_for_tty_fd(fd: RawFd) -> Option<u32> {
    let pgid = unsafe { libc::tcgetpgrp(fd) };
    (pgid > 0).then_some(pgid as u32)
}

/// Get the effective process name from `argv[0]` via `sysctl(KERN_PROCARGS2)`.
///
/// This is the macOS equivalent of reading `/proc/{pid}/cmdline` on Linux.
/// It reflects runtime title changes like Node.js `process.title = "pi"`.
fn process_argv0_name(pid: u32) -> Option<String> {
    let buf = kern_procargs2(pid)?;

    // Layout: [argc: i32] [exec_path\0] [padding\0...] [argv[0]\0] [argv[1]\0] ...
    if buf.len() < 4 {
        return None;
    }

    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if argc < 1 {
        return None;
    }

    // Skip past exec_path and null padding to reach argv[0]
    let rest = &buf[4..];
    let exec_end = rest.iter().position(|&b| b == 0)?;
    let mut pos = exec_end;
    while pos < rest.len() && rest[pos] == 0 {
        pos += 1;
    }
    if pos >= rest.len() {
        return None;
    }

    // Read argv[0]
    let argv0_end = rest[pos..]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(rest.len() - pos);
    let argv0 = std::str::from_utf8(&rest[pos..pos + argv0_end]).ok()?;

    if argv0.is_empty() {
        return None;
    }

    // Return basename (argv[0] may be a full path like "/usr/bin/node")
    let basename = Path::new(argv0).file_name()?.to_str()?;

    // Strip leading dash (login shells show as "-zsh")
    let name = basename.strip_prefix('-').unwrap_or(basename);
    if name.is_empty() {
        return None;
    }

    Some(name.to_string())
}

/// Raw `sysctl(KERN_PROCARGS2)` call. Returns the full buffer.
fn kern_procargs2(pid: u32) -> Option<Vec<u8>> {
    kern_procargs2_bounded(pid, usize::MAX)
}

fn kern_procargs2_bounded(pid: u32, max_bytes: usize) -> Option<Vec<u8>> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    if pid <= 0 {
        return None;
    }
    // SAFETY: both read-only sysctl calls receive correctly sized output
    // storage; the first size is bounded before allocating the second buffer.
    unsafe {
        let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];

        // First call: query required buffer size
        let mut size: libc::size_t = 0;
        let ret = libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        );
        if ret != 0 {
            return None;
        }

        // The size probe is advisory across same-PID exec. XNU can return a
        // truncated tail with success and size == capacity. Reserve headroom
        // and reject that ambiguous/saturated result instead of parsing it.
        let capacity = procargs2_read_capacity(size, max_bytes)?;
        let mut buf = vec![0u8; capacity];
        size = capacity;
        let ret = libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        );
        if ret != 0 {
            return None;
        }
        procargs2_complete_read(buf, size)
    }
}

fn procargs2_read_capacity(probed: usize, max_bytes: usize) -> Option<usize> {
    if probed < std::mem::size_of::<i32>() {
        return None;
    }
    let capacity = probed.checked_add(1)?;
    (capacity <= max_bytes).then_some(capacity)
}

fn procargs2_complete_read(mut buf: Vec<u8>, returned: usize) -> Option<Vec<u8>> {
    // sysctl_procargsx's partial-copy branch returns the supplied capacity,
    // including argc. Equality is ambiguous even when sysctl reports success.
    if returned < std::mem::size_of::<i32>() || returned >= buf.len() {
        return None;
    }
    buf.truncate(returned);
    Some(buf)
}

pub fn write_clipboard(bytes: &[u8]) -> bool {
    run_clipboard_command(
        &ClipboardCommand {
            program: "pbcopy",
            args: &[],
        },
        bytes,
    )
}

pub fn read_clipboard_text() -> Option<String> {
    const MAX_CLIPBOARD_TEXT_BYTES: usize = 1024 * 1024;

    let mut child = Command::new("pbpaste")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let read = match read_limited_reader(stdout, MAX_CLIPBOARD_TEXT_BYTES) {
        Ok(LimitedRead::Oversized) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        Ok(read) => read,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };
    let status = child.wait().ok()?;
    if !status.success() {
        return None;
    }
    match read {
        LimitedRead::Complete(bytes) => String::from_utf8(bytes).ok(),
        LimitedRead::Empty => None,
        LimitedRead::Oversized => unreachable!("oversized clipboard text is handled before wait"),
    }
}

mod input;
pub(crate) use input::PtyInputGuard;
#[cfg(test)]
mod input_tests;

pub fn clipboard_text_matches(_bytes: &[u8]) -> Option<bool> {
    None
}

pub fn open_url(url: &str) -> std::io::Result<Option<std::process::Child>> {
    open_path(std::path::Path::new(url))
}

pub fn open_path(path: &std::path::Path) -> std::io::Result<Option<std::process::Child>> {
    Command::new("open")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(Some)
}

pub fn read_clipboard_image() -> Option<ClipboardImage> {
    let path = std::env::temp_dir().join(format!(
        "herdr-clipboard-image-{}-{}.png",
        std::process::id(),
        unique_timestamp_nanos()
    ));
    let script = format!(
        "set png_data to (the clipboard as «class PNGf»)\nset fp to open for access POSIX file \"{}\" with write permission\nwrite png_data to fp\nclose access fp",
        path.display()
    );

    let status = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;

    if !status.success() {
        let _ = std::fs::remove_file(&path);
        return None;
    }

    let bytes = match std::fs::File::open(&path).ok().and_then(|file| {
        read_limited_reader(file, crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD).ok()
    }) {
        Some(LimitedRead::Complete(bytes)) => bytes,
        Some(LimitedRead::Empty | LimitedRead::Oversized) | None => {
            let _ = std::fs::remove_file(&path);
            return None;
        }
    };
    let _ = std::fs::remove_file(&path);
    Some(ClipboardImage {
        bytes,
        extension: "png",
    })
}

fn unique_timestamp_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

/// Show a native macOS notification.
///
/// Prefer `terminal-notifier` when it is installed because it can activate the
/// hosting terminal on click. Fall back to built-in AppleScript notifications
/// when it is not available.
pub fn show_desktop_notification(title: &str, body: Option<&str>) -> std::io::Result<bool> {
    show_desktop_notification_with_command(title, body, |program| Command::new(program))
}

fn show_desktop_notification_with_command(
    title: &str,
    body: Option<&str>,
    mut command: impl FnMut(&str) -> Command,
) -> std::io::Result<bool> {
    if show_terminal_notifier_notification(title, body, &mut command).unwrap_or(false) {
        return Ok(true);
    }

    show_osascript_notification(title, body, &mut command)
}

fn show_terminal_notifier_notification(
    title: &str,
    body: Option<&str>,
    command: &mut impl FnMut(&str) -> Command,
) -> std::io::Result<bool> {
    let activate_bundle_id = verified_terminal_bundle_identifier(command);
    show_terminal_notifier_notification_with_options(
        title,
        body,
        activate_bundle_id.as_deref(),
        command,
    )
}

fn show_terminal_notifier_notification_with_options(
    title: &str,
    body: Option<&str>,
    activate_bundle_id: Option<&str>,
    command: &mut impl FnMut(&str) -> Command,
) -> std::io::Result<bool> {
    let mut cmd = command("terminal-notifier");
    build_terminal_notifier_command(&mut cmd, title, body, activate_bundle_id);
    run_notification_command(cmd)
}

fn build_terminal_notifier_command(
    cmd: &mut Command,
    title: &str,
    body: Option<&str>,
    activate_bundle_id: Option<&str>,
) {
    cmd.arg("-title").arg(title);
    cmd.arg("-message").arg(body.unwrap_or_default());
    if let Some(bundle_id) = activate_bundle_id {
        cmd.arg("-activate").arg(bundle_id);
    }
}

fn show_osascript_notification(
    title: &str,
    body: Option<&str>,
    command: &mut impl FnMut(&str) -> Command,
) -> std::io::Result<bool> {
    let mut cmd = command("/usr/bin/osascript");
    cmd.arg("-e")
        .arg("on run argv")
        .arg("-e")
        .arg("display notification (item 2 of argv) with title (item 1 of argv)")
        .arg("-e")
        .arg("end run")
        .arg(title)
        .arg(body.unwrap_or_default());
    run_notification_command(cmd)
}

fn verified_terminal_bundle_identifier(
    command: &mut impl FnMut(&str) -> Command,
) -> Option<String> {
    static BUNDLE_ID: OnceLock<Option<String>> = OnceLock::new();
    BUNDLE_ID
        .get_or_init(|| {
            let bundle_id = detected_terminal_bundle_identifier()?;
            bundle_identifier_available(bundle_id, command).then(|| bundle_id.to_owned())
        })
        .clone()
}

fn bundle_identifier_available(bundle_id: &str, command: &mut impl FnMut(&str) -> Command) -> bool {
    let query = format!("kMDItemCFBundleIdentifier == '{bundle_id}'");
    let output = command("mdfind")
        .arg(query)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();

    match output {
        Ok(output) if output.status.success() => !output.stdout.is_empty(),
        _ => false,
    }
}

fn detected_terminal_bundle_identifier() -> Option<&'static str> {
    terminal_bundle_identifier_from_env(
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var("TERM").ok().as_deref(),
        std::env::var_os("KITTY_WINDOW_ID").is_some(),
        std::env::var_os("ALACRITTY_WINDOW_ID").is_some(),
    )
}

fn terminal_bundle_identifier_from_env(
    term_program: Option<&str>,
    term: Option<&str>,
    has_kitty_window_id: bool,
    has_alacritty_window_id: bool,
) -> Option<&'static str> {
    match term_program {
        Some("ghostty") => return Some("com.mitchellh.ghostty"),
        Some("iTerm.app") => return Some("com.googlecode.iterm2"),
        Some("WezTerm") => return Some("com.github.wez.wezterm"),
        Some("Apple_Terminal") => return Some("com.apple.Terminal"),
        _ => {}
    }

    if has_kitty_window_id || term == Some("xterm-kitty") {
        return Some("net.kovidgoyal.kitty");
    }
    if has_alacritty_window_id {
        return Some("org.alacritty");
    }

    None
}

fn run_notification_command(mut command: Command) -> std::io::Result<bool> {
    let status = match command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => status,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };

    Ok(status.success())
}

fn run_clipboard_command(command: &ClipboardCommand, bytes: &[u8]) -> bool {
    let mut child = match Command::new(command.program)
        .args(command.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };

    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    };

    if stdin.write_all(bytes).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    }
    drop(stdin);

    child.wait().map(|status| status.success()).unwrap_or(false)
}

fn process_bsdinfo(pid: u32) -> Option<libc::proc_bsdinfo> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;

    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };

    complete_bsdinfo(info, ret)
}

fn complete_bsdinfo(info: libc::proc_bsdinfo, bytes: libc::c_int) -> Option<libc::proc_bsdinfo> {
    (bytes as usize == std::mem::size_of::<libc::proc_bsdinfo>()).then_some(info)
}

fn comm_from_bsdinfo(info: &libc::proc_bsdinfo) -> Option<String> {
    let end = info
        .pbi_comm
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(info.pbi_comm.len());
    if end == 0 {
        return None;
    }

    let bytes: Vec<u8> = info.pbi_comm[..end].iter().map(|&b| b as u8).collect();
    String::from_utf8(bytes).ok()
}

fn process_argv(pid: u32) -> Option<Vec<String>> {
    let buf = kern_procargs2(pid)?;
    procargs2_argv(&buf)
}

/// Read a Herdr agent identity hint from a process environment.
pub fn process_agent_hint(pid: u32) -> Option<crate::detect::Agent> {
    if pid == 0 {
        return None;
    }
    let buf = kern_procargs2(pid)?;
    super::parse_agent_env_hint(procargs2_env(&buf)?)
}

// Legacy best-effort foreground metadata/hints only. Caller authority must use
// invocation_procargs2_argv's exact aligned boundary, including empty argv[0].
fn procargs2_argv_start(rest: &[u8]) -> Option<usize> {
    let exec_end = rest.iter().position(|&byte| byte == 0)?;
    let mut pos = exec_end;
    while pos < rest.len() && rest[pos] == 0 {
        pos += 1;
    }
    (pos < rest.len()).then_some(pos)
}

fn skip_nul_strings(bytes: &[u8], start: usize, count: usize) -> Option<usize> {
    let mut current = start;
    for _ in 0..count {
        let end = bytes.get(current..)?.iter().position(|&byte| byte == 0)?;
        current = current.checked_add(end)?.checked_add(1)?;
    }
    Some(current)
}

/// Caller authority uses XNU's target-pointer-aligned argv boundary, not the
/// best-effort metadata reader's NUL scan. Empty argv[0] is legal on Darwin;
/// skipping all leading NULs could otherwise turn environment into arguments.
fn invocation_procargs2_argv(buf: &[u8], process_flags: u32) -> Option<Vec<String>> {
    let rest = buf.get(4..)?;
    let exec_end = rest.iter().position(|&byte| byte == 0)?;
    if exec_end == 0 {
        return None;
    }
    // XNU exec_extract_strings aligns ip_startargv after executable_path= and
    // the path. The 16-byte key is stripped by sysctl_procargsx, preserving
    // alignment relative to rest. See Apple XNU bsd/kern/kern_exec.c.
    let pointer_size = if process_flags & PROC_FLAG_LP64 != 0 {
        8
    } else {
        4
    };
    let path_end = exec_end.checked_add(1)?;
    let start = path_end.checked_add(pointer_size - 1)? & !(pointer_size - 1);
    if rest.get(path_end..start)?.iter().any(|&byte| byte != 0) {
        return None;
    }
    procargs2_argv_at(buf, start)
}

fn procargs2_argv(buf: &[u8]) -> Option<Vec<String>> {
    let rest = buf.get(4..)?;
    procargs2_argv_at(buf, procargs2_argv_start(rest)?)
}

fn procargs2_argv_at(buf: &[u8], start: usize) -> Option<Vec<String>> {
    if buf.len() < 4 {
        return None;
    }

    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if argc < 1 {
        return None;
    }

    // Layout: [argc: i32] [exec_path\0] [padding\0...] [argv[0]\0] ... [env\0] ...
    let rest = &buf[4..];
    let mut current = start;
    // Even an empty argument requires a NUL byte. Reject impossible counts
    // before reserving memory from the kernel-supplied argc.
    if argc as usize > rest.len().checked_sub(current)? {
        return None;
    }
    let mut argv = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        if current >= rest.len() {
            return None;
        }
        let end = current + rest[current..].iter().position(|&b| b == 0)?;
        if end == current && argv.is_empty() {
            return None;
        }
        argv.push(String::from_utf8_lossy(&rest[current..end]).into_owned());
        current = end + 1;
    }

    Some(argv)
}

fn procargs2_env(buf: &[u8]) -> Option<&[u8]> {
    if buf.len() < 4 {
        return None;
    }

    let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if argc < 1 {
        return None;
    }

    let rest = &buf[4..];
    let argv_start = procargs2_argv_start(rest)?;
    let env_start = skip_nul_strings(rest, argv_start, argc as usize)?;
    rest.get(env_start..)
}

/// Get the current working directory of a process.
///
/// Uses `proc_pidinfo(PROC_PIDVNODEPATHINFO)` to read `pvi_cdir.vip_path`.
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }

    let mut pathinfo: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;

    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            &mut pathinfo as *mut _ as *mut libc::c_void,
            size,
        )
    };

    if ret != size {
        return None;
    }

    // vip_path is [[c_char; 32]; 32] in libc (workaround for old Rust const generics).
    // Reinterpret as flat bytes (total MAXPATHLEN = 1024).
    let vip_path = unsafe {
        std::slice::from_raw_parts(
            pathinfo.pvi_cdir.vip_path.as_ptr() as *const u8,
            libc::MAXPATHLEN as usize,
        )
    };

    let nul = vip_path.iter().position(|&b| b == 0)?;
    if nul == 0 {
        return None;
    }
    Some(PathBuf::from(OsStr::from_bytes(&vip_path[..nul])))
}

pub fn session_processes(child_pid: u32) -> Vec<u32> {
    if child_pid == 0 {
        return Vec::new();
    }

    let target_session = unsafe { libc::getsid(child_pid as libc::c_int) };
    if target_session <= 0 {
        return Vec::new();
    }

    all_pids()
        .into_iter()
        .filter(|pid| unsafe { libc::getsid(*pid as libc::pid_t) } == target_session)
        .collect()
}

fn all_pids() -> Vec<u32> {
    let initial_count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let mut capacity = if initial_count > 0 {
        initial_count as usize + 128
    } else {
        4096
    };

    for _ in 0..8 {
        let mut pids = vec![0 as libc::pid_t; capacity];
        let count = unsafe {
            libc::proc_listallpids(
                pids.as_mut_ptr() as *mut libc::c_void,
                (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int,
            )
        };
        if count <= 0 {
            return Vec::new();
        }

        let count = count as usize;
        if count < capacity {
            return collect_positive_pids(pids, count);
        }
        capacity = capacity.saturating_mul(2);
    }

    Vec::new()
}

fn collect_positive_pids(pids: Vec<libc::pid_t>, count: usize) -> Vec<u32> {
    pids.into_iter()
        .take(count)
        .filter(|pid| *pid > 0)
        .map(|pid| pid as u32)
        .collect()
}

pub fn signal_processes(pids: &[u32], signal: Signal) {
    let sig = match signal {
        Signal::Hangup => libc::SIGHUP,
        Signal::Terminate => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };

    for &pid in pids {
        if pid == 0 {
            continue;
        }
        unsafe {
            libc::kill(pid as libc::c_int, sig);
        }
    }
}

pub fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid as libc::c_int, 0) };
    if result == 0 {
        true
    } else {
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nofile_target_raises_low_soft_limit_to_cap_when_hard_is_unlimited() {
        assert_eq!(
            target_nofile_soft_limit(256, libc::RLIM_INFINITY, 8192),
            Some(8192)
        );
    }

    #[test]
    fn nofile_target_respects_finite_hard_limit() {
        assert_eq!(target_nofile_soft_limit(256, 4096, 8192), Some(4096));
    }

    #[test]
    fn nofile_target_does_not_lower_existing_soft_limit() {
        assert_eq!(
            target_nofile_soft_limit(16_384, libc::RLIM_INFINITY, 8192),
            None
        );
    }

    #[test]
    fn invocation_kernel_peer_credentials_match_connected_process() {
        use std::os::fd::AsRawFd;
        use std::os::unix::net::{UnixListener, UnixStream};
        let directory = std::env::temp_dir().join(format!("vqro-peer-{}", std::process::id()));
        std::fs::create_dir(&directory).expect("private socket directory");
        let path = directory.join("peer.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let client = UnixStream::connect(&path).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        assert_eq!(
            socket_peer_credentials_fd(server.as_raw_fd()),
            Some((std::process::id(), unsafe { libc::geteuid() },))
        );
        assert!(socket_peer_credentials_fd(-1).is_none());
        drop((client, server, listener));
        std::fs::remove_file(path).expect("remove owned socket");
        std::fs::remove_dir(directory).expect("remove owned directory");
    }

    #[test]
    fn invocation_libproc_identity_rechecks_birth_parent_uid_and_exit() {
        let mut child = Command::new("/bin/sleep").arg("30").spawn().expect("child");
        let pid = child.id();
        let info = invocation_bsdinfo(pid).expect("child BSD info");
        let birth = invocation_birth(&info).expect("child birth");
        let uid = unsafe { libc::geteuid() };
        let entry = crate::action_invocation::InvocationProcess {
            pid,
            parent_pid: info.pbi_ppid,
            birth,
            process: ForegroundProcess {
                pid,
                name: "sleep".into(),
                argv0: None,
                argv: None,
                cmdline: None,
            },
        };
        let mut peer = crate::action_invocation::InvocationPeer {
            account: crate::action_invocation::InvocationAccount::Unix { uid },
            boot_id: invocation_boot_id().expect("boot namespace"),
            ancestry: vec![entry],
            foreground_cli: false,
            foreground: None,
        };
        assert_eq!(invocation_boot_id().as_deref(), Some(peer.boot_id.as_str()));
        assert_eq!(invocation_process_birth(pid), Some(birth));
        assert_eq!(info.pbi_ppid, std::process::id());
        assert!(invocation_peer_is_current(&peer));
        peer.ancestry[0].birth += 1;
        assert!(!invocation_peer_is_current(&peer));
        peer.ancestry[0].birth = birth;
        peer.ancestry[0].parent_pid = 0;
        assert!(!invocation_peer_is_current(&peer));
        peer.ancestry[0].parent_pid = info.pbi_ppid;
        peer.account = crate::action_invocation::InvocationAccount::Unix {
            uid: uid.wrapping_add(1),
        };
        assert!(!invocation_peer_is_current(&peer));
        peer.account = crate::action_invocation::InvocationAccount::Unix { uid };
        child.kill().expect("stop owned child");
        child.wait().expect("reap owned child");
        assert!(!invocation_peer_is_current(&peer));
        assert!(invocation_process_birth(0).is_none());
        assert!(invocation_process_birth(u32::MAX).is_none());
    }

    #[test]
    fn invocation_bsdinfo_rejects_short_reads_and_invalid_birth() {
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        for bytes in [-1, 0, size - 1, size + 1] {
            let info = unsafe { std::mem::zeroed() };
            assert!(complete_bsdinfo(info, bytes).is_none());
        }
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        assert!(invocation_birth(&info).is_none());
        info.pbi_start_tvsec = 42;
        info.pbi_start_tvusec = 999_999;
        assert_eq!(invocation_birth(&info), Some(42_999_999));
        info.pbi_start_tvusec = 1_000_000;
        assert!(invocation_birth(&info).is_none());
        info.pbi_start_tvsec = u64::MAX;
        info.pbi_start_tvusec = 0;
        assert!(invocation_birth(&info).is_none());
    }

    #[test]
    fn invocation_procargs_rejects_truncation_and_impossible_argc() {
        let mut buf = build_procargs2("/usr/bin/node", &["node", "pi"], &[]);
        buf.pop();
        assert!(procargs2_argv(&buf).is_none(), "unterminated last argv");
        buf[..4].copy_from_slice(&i32::MAX.to_ne_bytes());
        assert!(procargs2_argv(&buf).is_none(), "impossible count");
        for argc in [-1i32, 0] {
            buf[..4].copy_from_slice(&argc.to_ne_bytes());
            assert!(procargs2_argv(&buf).is_none());
        }
        assert!(kern_procargs2_bounded(std::process::id(), 1).is_none());
        let buf = build_procargs2("/bin/echo", &["echo", "", "hello"], &["AGENT=pi"]);
        assert_eq!(
            procargs2_argv(&buf),
            Some(vec!["echo".into(), "".into(), "hello".into()])
        );
    }

    #[test]
    fn invocation_procargs_rejects_saturated_reads_after_size_growth() {
        assert_eq!(procargs2_read_capacity(100, 1024), Some(101));
        assert!(procargs2_read_capacity(1024, 1024).is_none());
        assert!(procargs2_read_capacity(usize::MAX, usize::MAX).is_none());
        assert!(procargs2_read_capacity(0, 1024).is_none());
        let capacity = procargs2_read_capacity(100, 1024).expect("bounded headroom");
        // Model XNU success after exec grows args: its tail/padded copy returns
        // exactly capacity, not an overflow indication. Never parse that tail.
        assert!(procargs2_complete_read(vec![b'p'; capacity], capacity).is_none());
        assert!(procargs2_complete_read(vec![0; capacity], capacity + 1).is_none());
        assert!(procargs2_complete_read(vec![0; capacity], 3).is_none());
        assert_eq!(
            procargs2_complete_read(vec![0; capacity], 100).map(|buf| buf.len()),
            Some(100)
        );
        // Growth within rounded probe slack is complete if it leaves headroom.
        assert_eq!(
            procargs2_complete_read(vec![0; 108], 104).map(|buf| buf.len()),
            Some(104)
        );
    }

    #[test]
    fn invocation_procargs_uses_target_alignment_and_rejects_empty_argv0() {
        for (pointer_size, flags) in [(4, 0), (8, PROC_FLAG_LP64)] {
            for length in 1..=32 {
                let path = format!("/{}", "x".repeat(length));
                for argv in [&[""][..], &["", "node"][..], &["", "pi"][..]] {
                    let buf = build_procargs2_aligned(&path, argv, &["pi", "claude"], pointer_size);
                    assert!(
                        invocation_procargs2_argv(&buf, flags).is_none(),
                        "empty argv0 must not shift later arguments/environment into authority"
                    );
                }
                let buf = build_procargs2_aligned(
                    &path,
                    &["node", "", "pi"],
                    &["claude", "codex"],
                    pointer_size,
                );
                assert_eq!(
                    invocation_procargs2_argv(&buf, flags),
                    Some(vec!["node".into(), "".into(), "pi".into()])
                );
            }
        }
        let mut buf = build_procargs2_aligned("/bin/node", &["node"], &["pi"], 8);
        buf[4 + "/bin/node".len() + 1] = b'x';
        assert!(
            invocation_procargs2_argv(&buf, PROC_FLAG_LP64).is_none(),
            "nonzero alignment padding"
        );
    }

    fn build_procargs2(exec_path: &str, argv: &[&str], env: &[&str]) -> Vec<u8> {
        build_procargs2_aligned(exec_path, argv, env, 8)
    }

    fn build_procargs2_aligned(
        exec_path: &str,
        argv: &[&str],
        env: &[&str],
        pointer_size: usize,
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(argv.len() as i32).to_ne_bytes());
        buf.extend_from_slice(exec_path.as_bytes());
        buf.push(0);
        while (buf.len() - 4) % pointer_size != 0 {
            buf.push(0);
        }
        for arg in argv {
            buf.extend_from_slice(arg.as_bytes());
            buf.push(0);
        }
        for entry in env {
            buf.extend_from_slice(entry.as_bytes());
            buf.push(0);
        }
        buf
    }

    #[test]
    fn procargs2_argv_excludes_environment_entries() {
        let buf = build_procargs2(
            "/usr/bin/node",
            &["node", "/Users/can/.local/bin/pi"],
            &[
                "PATH=/usr/bin:/var/run/com.apple.security.cryptexd/codex.system/bootstrap/usr/bin",
                "TERM=tmux-256color",
            ],
        );

        let argv = procargs2_argv(&buf).expect("expected argv");
        assert_eq!(argv, vec!["node", "/Users/can/.local/bin/pi"]);
        assert_eq!(argv.join(" "), "node /Users/can/.local/bin/pi");
        assert!(!argv.join(" ").contains("codex.system"));
    }

    #[test]
    fn procargs2_env_reads_agent_hint_after_argv() {
        let buf = build_procargs2(
            "/opt/homebrew/bin/nono",
            &["nono", "run", "HERDR_AGENT=codex", "--", "claude"],
            &["PATH=/usr/bin", "HERDR_AGENT=claude", "TERM=xterm-256color"],
        );

        let env = procargs2_env(&buf).expect("expected env block");
        assert_eq!(
            crate::platform::parse_agent_env_hint(env),
            Some(crate::detect::Agent::Claude)
        );
    }

    #[test]
    fn procargs2_env_does_not_treat_argv_as_environment() {
        let buf = build_procargs2(
            "/opt/homebrew/bin/nono",
            &["nono", "run", "HERDR_AGENT=claude"],
            &["PATH=/usr/bin"],
        );

        let env = procargs2_env(&buf).expect("expected env block");
        assert_eq!(crate::platform::parse_agent_env_hint(env), None);
    }

    #[test]
    fn terminal_bundle_identifier_maps_known_terminal_env() {
        assert_eq!(
            terminal_bundle_identifier_from_env(Some("ghostty"), None, false, false),
            Some("com.mitchellh.ghostty")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(Some("iTerm.app"), None, false, false),
            Some("com.googlecode.iterm2")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(Some("WezTerm"), None, false, false),
            Some("com.github.wez.wezterm")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(Some("Apple_Terminal"), None, false, false),
            Some("com.apple.Terminal")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(None, Some("xterm-kitty"), false, false),
            Some("net.kovidgoyal.kitty")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(None, None, true, false),
            Some("net.kovidgoyal.kitty")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(None, None, false, true),
            Some("org.alacritty")
        );
        assert_eq!(
            terminal_bundle_identifier_from_env(None, None, false, false),
            None
        );
    }

    #[test]
    fn terminal_notifier_command_includes_icon_and_activation() {
        let mut cmd = Command::new("terminal-notifier");
        build_terminal_notifier_command(
            &mut cmd,
            "pi finished",
            Some("workspace 1"),
            Some("com.mitchellh.ghostty"),
        );
        let args = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                "-title",
                "pi finished",
                "-message",
                "workspace 1",
                "-activate",
                "com.mitchellh.ghostty"
            ]
        );
    }

    #[test]
    fn terminal_notifier_success_skips_osascript() {
        let path = std::env::temp_dir().join(format!(
            "herdr-terminal-notifier-args-{}",
            std::process::id()
        ));
        let script = "printf '%s:%s\\n' \"$0\" \"$*\" >> \"$HERDR_NOTIFY_ARGS\"";
        let mut command = |program: &str| {
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg(script)
                .arg(program)
                .env("HERDR_NOTIFY_ARGS", &path);
            cmd
        };

        let shown = show_terminal_notifier_notification_with_options(
            "title",
            Some("body"),
            Some("com.mitchellh.ghostty"),
            &mut command,
        )
        .expect("terminal-notifier command should run");

        assert!(shown);
        let args = std::fs::read_to_string(&path).expect("args file");
        let _ = std::fs::remove_file(&path);
        assert!(args.starts_with("terminal-notifier:"), "{args}");
        assert!(args.contains("-activate com.mitchellh.ghostty"), "{args}");
        assert!(!args.contains("osascript"), "{args}");
    }

    #[test]
    fn desktop_notification_falls_back_to_osascript_when_terminal_notifier_fails() {
        let path =
            std::env::temp_dir().join(format!("herdr-osascript-args-{}", std::process::id()));
        let script = r#"
if [ "$0" = "terminal-notifier" ]; then
  exit 1
fi
printf '%s\n' "$@" > "$HERDR_NOTIFY_ARGS"
"#;
        let mut command = |program: &str| {
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg(script)
                .arg(program)
                .env("HERDR_NOTIFY_ARGS", &path);
            cmd
        };
        let shown = show_desktop_notification_with_command("title", Some("body"), &mut command)
            .expect("osascript fallback should run");

        assert!(shown);
        let args = std::fs::read_to_string(&path).expect("args file");
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            args,
            "-e\non run argv\n-e\ndisplay notification (item 2 of argv) with title (item 1 of argv)\n-e\nend run\ntitle\nbody\n"
        );
    }

    #[test]
    fn scrollback_editor_argv_preserves_unix_editor_shell_semantics() {
        let path = std::path::Path::new("/tmp/herdr scrollback.txt");
        let argv = scrollback_editor_argv(path).unwrap();

        assert_eq!(argv[0], "/bin/sh");
        assert_eq!(argv[1], "-c");
        assert!(argv[2].contains("EDITOR:-vi"));
        assert!(argv[2].contains("/tmp/herdr scrollback.txt"));
    }
}
