mod native_image_sources;
pub(crate) use native_image_sources::clone_native_image_source;

use std::{
    collections::{HashSet, VecDeque},
    io::{Read, Write},
    os::fd::RawFd,
    path::PathBuf,
    process::{Command, Stdio},
    sync::OnceLock,
};

mod shutdown;
pub(crate) use shutdown::monitor_host_shutdown;

pub(super) const REMOTE_BRIDGE_CLOCK: libc::clockid_t = libc::CLOCK_BOOTTIME;

pub(crate) fn socket_peer_birth(stream: &crate::ipc::LocalStream) -> Option<(u32, u64)> {
    use interprocess::local_socket::traits::StreamCommon;
    let credentials = stream.peer_creds().ok()?;
    let pid = u32::try_from(credentials.pid()?).ok()?;
    Some((pid, invocation_process_birth(pid)?))
}

pub(crate) fn invocation_peer(
    stream: &crate::ipc::LocalStream,
) -> Option<crate::action_invocation::InvocationPeer> {
    use interprocess::local_socket::traits::StreamCommon;
    let credentials = stream.peer_creds().ok()?;
    let uid = credentials.euid()?;
    // SAFETY: geteuid has no arguments or memory preconditions.
    if uid != unsafe { libc::geteuid() } {
        return None;
    }
    invocation_peer_for_process(u32::try_from(credentials.pid()?).ok()?)
}

pub(crate) fn invocation_peer_for_process(
    pid: u32,
) -> Option<crate::action_invocation::InvocationPeer> {
    // Socket callers already checked kernel euid; process-only observations use
    // the host account, never a caller-supplied or placeholder UID.
    // SAFETY: geteuid has no arguments or memory preconditions.
    let uid = unsafe { libc::geteuid() };
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()?
        .trim()
        .to_owned();
    let ancestry = super::capture_invocation_ancestry(pid, |pid| {
        invocation_process(pid).map(super::InvocationAncestor::Process)
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

fn invocation_process(pid: u32) -> Option<crate::action_invocation::InvocationProcess> {
    let mut stat = String::new();
    std::fs::File::open(format!("/proc/{pid}/stat"))
        .ok()?
        .take(8192)
        .read_to_string(&mut stat)
        .ok()?;
    let start = stat.find('(')?;
    let end = stat.rfind(')')?;
    let fields: Vec<_> = stat.get(end + 2..)?.split_whitespace().collect();
    let parent_pid = fields.get(1)?.parse().ok()?;
    let birth = fields.get(19)?.parse().ok()?;
    let mut command = Vec::new();
    std::fs::File::open(format!("/proc/{pid}/cmdline"))
        .ok()?
        .take(16385)
        .read_to_end(&mut command)
        .ok()?;
    if command.len() > 16384 {
        return None;
    }
    let argv: Vec<_> = command
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();
    Some(crate::action_invocation::InvocationProcess {
        pid,
        parent_pid,
        birth,
        process: ForegroundProcess {
            pid,
            name: stat.get(start + 1..end)?.to_string(),
            argv0: argv.first().cloned(),
            cmdline: Some(argv.join(" ")),
            argv: Some(argv),
        },
    })
}

pub(crate) fn invocation_process_birth(pid: u32) -> Option<u64> {
    let mut stat = String::new();
    std::fs::File::open(format!("/proc/{pid}/stat"))
        .ok()?
        .take(8192)
        .read_to_string(&mut stat)
        .ok()?;
    stat.get(stat.rfind(')')? + 2..)?
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

pub(crate) fn invocation_peer_is_current(peer: &crate::action_invocation::InvocationPeer) -> bool {
    super::invocation_ancestry_is_current(&peer.ancestry, |pid| {
        let current = invocation_process(pid)?;
        Some((current.birth, current.parent_pid))
    })
}

use super::{
    read_limited_reader, ClipboardCommand, ClipboardImage, ForegroundJob, ForegroundProcess,
    LimitedRead, ProcessTelemetry, Signal, TerminalTelemetrySample,
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

const WSL_MARKER_ENV_VARS: &[&str] = &["WSL_DISTRO_NAME", "WSL_INTEROP"];
const PROCESS_DETECTION_ENV_VAR: &str = "VQRO_PROCESS_DETECTION";
const CHILD_GROUPS_SCAN_LIMIT: usize = 64;
/// Upper bound on the number of processes visited while resolving a pane's
/// foreground process-group tree. Foreground-job detection reads /proc/<pid>/stat
/// and task/children files for every visited process on a repeated (per-tick/5s)
/// cadence, so an unbounded walk lets accumulated descendants or unreaped zombies
/// under the pane shell grow the server's read-syscall rate and CPU without limit
/// at a constant pane count (see AGENTS.md multiplicative performance paths). The
/// foreground-group leader's subtree and the pane shell's descendants advance
/// round-robin under a shared candidate ceiling, with independent per-root work
/// budgets, so a pathologically large accumulation on either side cannot starve the
/// other. Discovery is best effort once a budget is exhausted.
const FOREGROUND_TREE_SCAN_LIMIT: usize = 512;
/// Number of `/proc/<pid>/task` entries a root's subtree may consume, bounding how
/// far one process's thread count can multiply the walk's work.
const FOREGROUND_TASK_ENTRY_LIMIT: usize = 2_048;
/// Number of `/proc/<pid>/task/<tid>/children` bytes a root's subtree may read,
/// stopping a parent that accumulates unreaped children from growing read work
/// without limit.
const FOREGROUND_CHILD_BYTE_LIMIT: usize = 128 * 1024;
/// Aggregate number of child pids a root's subtree may parse and enqueue, bounding
/// the walk's pending queues and allocations.
const FOREGROUND_CHILD_PID_LIMIT: usize = 2_048;

/// Per-root work budget for foreground process discovery. The foreground-group
/// leader and the pane shell each get their own budget, so one side's expansion
/// cannot exhaust the other's allowance. Every task entry, child byte, and parsed
/// child pid is charged against the owning root's budget, keeping total `/proc` work
/// bounded independently of the process-tree size and of uptime. Discovery is best
/// effort once a budget is exhausted.
#[derive(Debug)]
struct ForegroundScanBudget {
    task_entries: usize,
    child_bytes: usize,
    child_pids: usize,
}

impl ForegroundScanBudget {
    fn for_probe() -> Self {
        Self {
            task_entries: FOREGROUND_TASK_ENTRY_LIMIT,
            child_bytes: FOREGROUND_CHILD_BYTE_LIMIT,
            child_pids: FOREGROUND_CHILD_PID_LIMIT,
        }
    }
}
const TELEMETRY_TREE_SCAN_LIMIT: usize = 128;
const TELEMETRY_TASK_SCAN_LIMIT: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessDetectionMode {
    Native,
    ChildGroups,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcGroupMember {
    pid: u32,
    comm: String,
    state: char,
}

pub(crate) fn launch_executable() -> std::io::Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    let executable = std::env::current_exe()?;
    if !executable.is_file() {
        // Linux marks the old inode as deleted after an update replaces the binary.
        if let Some(path) = executable
            .as_os_str()
            .as_bytes()
            .strip_suffix(b" (deleted)")
        {
            let replacement = PathBuf::from(std::ffi::OsStr::from_bytes(path));
            if replacement.is_file() {
                return Ok(replacement);
            }
        }
    }
    Ok(executable)
}

/// Raise only the soft limit: the hard limit is the server's existing ceiling.
/// Selective adaptation of fork dbdff708; macOS retains its fixed target.
pub fn raise_server_nofile_limit() {
    let result = raise_nofile_limit_to_hard_with(
        || {
            let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
            // SAFETY: getrlimit initializes the supplied rlimit on success.
            if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(unsafe { limit.assume_init() })
        },
        |limit| {
            // SAFETY: limit is an initialized rlimit; only its soft limit changed.
            if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, limit) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        },
    );
    match result {
        Ok(None) => {}
        Ok(Some((previous, target))) => {
            tracing::info!(previous, target, "raised server file descriptor soft limit")
        }
        Err(err) => tracing::warn!(err = %err, "failed to raise server file descriptor limit"),
    }
}

fn raise_nofile_limit_to_hard_with(
    get: impl FnOnce() -> std::io::Result<libc::rlimit>,
    set: impl FnOnce(&libc::rlimit) -> std::io::Result<()>,
) -> std::io::Result<Option<(libc::rlim_t, libc::rlim_t)>> {
    let mut limit = get()?;
    if limit.rlim_cur >= limit.rlim_max {
        return Ok(None);
    }
    let previous = limit.rlim_cur;
    limit.rlim_cur = limit.rlim_max;
    set(&limit)?;
    Ok(Some((previous, limit.rlim_cur)))
}

#[cfg(test)]
mod nofile_tests {
    use super::*;

    #[test]
    fn raises_soft_to_hard_without_changing_hard_limit() {
        for hard in [4096, libc::RLIM_INFINITY] {
            let result = raise_nofile_limit_to_hard_with(
                || {
                    Ok(libc::rlimit {
                        rlim_cur: 256,
                        rlim_max: hard,
                    })
                },
                |limit| {
                    assert_eq!(limit.rlim_cur, hard);
                    assert_eq!(limit.rlim_max, hard);
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(result, Some((256, hard)));
        }
    }

    #[test]
    fn already_at_hard_limit_does_not_set() {
        assert_eq!(
            raise_nofile_limit_to_hard_with(
                || Ok(libc::rlimit {
                    rlim_cur: 4096,
                    rlim_max: 4096
                }),
                |_| panic!("must not set an unchanged limit"),
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn get_and_set_failures_are_returned_without_retry() {
        assert!(raise_nofile_limit_to_hard_with(
            || Err(std::io::Error::other("get failed")),
            |_| panic!("get failed"),
        )
        .is_err());
        assert!(raise_nofile_limit_to_hard_with(
            || Ok(libc::rlimit {
                rlim_cur: 256,
                rlim_max: 4096
            }),
            |_| Err(std::io::Error::other("set failed")),
        )
        .is_err());
    }
}

pub(crate) fn should_draw_host_cursor_by_default() -> bool {
    running_inside_wsl()
}

pub(crate) fn should_query_host_terminal_palette() -> bool {
    !running_inside_wsl()
}

fn running_inside_wsl() -> bool {
    static RUNNING_INSIDE_WSL: OnceLock<bool> = OnceLock::new();
    *RUNNING_INSIDE_WSL.get_or_init(detect_running_inside_wsl)
}

fn detect_running_inside_wsl() -> bool {
    proc_file_indicates_wsl("/proc/sys/kernel/osrelease")
        || proc_file_indicates_wsl("/proc/version")
        || WSL_MARKER_ENV_VARS
            .iter()
            .any(|key| std::env::var_os(key).is_some())
        || std::path::Path::new("/run/WSL").exists()
}

fn proc_file_indicates_wsl(path: &str) -> bool {
    std::fs::read_to_string(path)
        .map(|text| text_indicates_wsl(&text))
        .unwrap_or(false)
}

fn text_indicates_wsl(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("microsoft") || text.contains("wsl")
}

fn parse_process_detection_mode(value: Option<&str>) -> Result<ProcessDetectionMode, &str> {
    match value {
        None | Some("") | Some("native") => Ok(ProcessDetectionMode::Native),
        Some("child-groups") => Ok(ProcessDetectionMode::ChildGroups),
        Some(value) => Err(value),
    }
}

fn process_detection_mode() -> ProcessDetectionMode {
    static MODE: OnceLock<ProcessDetectionMode> = OnceLock::new();
    *MODE.get_or_init(|| {
        let value = std::env::var(PROCESS_DETECTION_ENV_VAR).ok();
        parse_process_detection_mode(value.as_deref()).unwrap_or_else(|value| {
            tracing::warn!(
                variable = PROCESS_DETECTION_ENV_VAR,
                %value,
                "unknown process detection mode; using native detection"
            );
            ProcessDetectionMode::Native
        })
    })
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

pub(crate) fn scrollback_editor_argv(path: &std::path::Path) -> std::io::Result<Vec<String>> {
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

/// Collect the foreground terminal job for a given child PID.
pub(crate) fn available_pane_shell(child_pid: u32) -> Option<String> {
    super::available_pane_shell_from_job(child_pid, foreground_job(child_pid)?)
}

pub fn foreground_job(child_pid: u32) -> Option<ForegroundJob> {
    let process_group_id = foreground_process_group_id(child_pid).or_else(|| {
        (process_detection_mode() == ProcessDetectionMode::ChildGroups)
            .then(|| child_groups_foreground_process_group(child_pid))
            .flatten()
    })?;
    foreground_job_for_group(child_pid, process_group_id)
}

fn foreground_job_for_group(child_pid: u32, process_group_id: u32) -> Option<ForegroundJob> {
    let members = foreground_process_group_members(child_pid, process_group_id)?;
    foreground_job_from_members(
        process_group_id,
        members,
        running_inside_wsl(),
        process_argv,
    )
}

fn foreground_job_from_members(
    process_group_id: u32,
    members: Vec<ProcGroupMember>,
    running_inside_wsl: bool,
    mut read_argv: impl FnMut(u32) -> Option<Vec<String>>,
) -> Option<ForegroundJob> {
    let processes = members
        .into_iter()
        .map(|member| {
            // Reading procfs cmdline enters access_remote_vm. On WSL, that read can
            // block indefinitely while a multithreaded process is exiting. A state
            // check alone has a race, so WSL uses the cheap comm-based identity when
            // it already identifies a supported agent without inspecting cmdline.
            let argv =
                process_allows_remote_memory_read(member.state, &member.comm, running_inside_wsl)
                    .then(|| read_argv(member.pid))
                    .flatten();
            ForegroundProcess {
                pid: member.pid,
                name: member.comm,
                argv0: None,
                cmdline: argv.as_ref().map(|parts| parts.join(" ")),
                argv,
            }
        })
        .collect::<Vec<_>>();

    if processes.is_empty() {
        return None;
    }

    Some(ForegroundJob {
        process_group_id,
        processes,
    })
}

/// Best-effort foreground group for environments that do not expose terminal
/// foreground groups. This mode is explicit because background jobs cannot be
/// distinguished from foreground jobs without the native terminal signal.
fn child_groups_foreground_process_group(child_pid: u32) -> Option<u32> {
    let shell_group_id = process_pgrp_comm_and_state(child_pid)
        .map(|(pgrp, _, _)| pgrp)
        .filter(|pgrp| *pgrp > 0)? as u32;

    child_groups_foreground_process_group_with(
        child_pid,
        shell_group_id,
        process_task_ids,
        process_task_children,
        |pid| process_pgrp_comm_and_state(pid).map(|(pgrp, _, _)| pgrp),
    )
}

fn child_groups_foreground_process_group_with(
    child_pid: u32,
    shell_group_id: u32,
    mut task_ids: impl FnMut(u32, &mut ForegroundScanBudget) -> Vec<u32>,
    mut task_children: impl FnMut(u32, u32, &mut ForegroundScanBudget) -> Vec<u32>,
    mut process_group_id: impl FnMut(u32) -> Option<i32>,
) -> Option<u32> {
    let mut budget = ForegroundScanBudget::for_probe();
    let mut newest = None;
    let mut scanned = 0usize;
    for tid in task_ids(child_pid, &mut budget) {
        for child in task_children(child_pid, tid, &mut budget) {
            if scanned >= CHILD_GROUPS_SCAN_LIMIT {
                return None;
            }
            scanned += 1;

            let Some(pgrp) = process_group_id(child) else {
                continue;
            };
            if pgrp <= 0 {
                continue;
            }
            let pgrp = pgrp as u32;
            if pgrp == shell_group_id {
                continue;
            }
            newest = Some(newest.map_or(pgrp, |current: u32| current.max(pgrp)));
        }
    }
    newest.or(Some(shell_group_id))
}

fn foreground_process_group_members(
    child_pid: u32,
    process_group_id: u32,
) -> Option<Vec<ProcGroupMember>> {
    foreground_process_group_members_with(
        child_pid,
        process_group_id,
        process_task_ids,
        process_task_children,
        live_process_group_member,
    )
}

fn foreground_process_group_members_with(
    child_pid: u32,
    process_group_id: u32,
    task_ids: impl FnMut(u32, &mut ForegroundScanBudget) -> Vec<u32>,
    task_children: impl FnMut(u32, u32, &mut ForegroundScanBudget) -> Vec<u32>,
    mut live_member: impl FnMut(u32, u32) -> Option<ProcGroupMember>,
) -> Option<Vec<ProcGroupMember>> {
    // The leader is passed first; `process_tree_pids` advances both roots round-robin
    // so a truncated scan cannot let the pane shell's unrelated descendants starve
    // the foreground group, or vice versa.
    let mut members = process_tree_pids([process_group_id, child_pid], task_ids, task_children)
        .into_iter()
        .filter_map(|pid| live_member(process_group_id, pid))
        .collect::<Vec<_>>();
    members.sort_unstable_by_key(|member| member.pid);
    (!members.is_empty()).then_some(members)
}

fn process_tree_pids(
    roots: impl IntoIterator<Item = u32>,
    mut task_ids: impl FnMut(u32, &mut ForegroundScanBudget) -> Vec<u32>,
    mut task_children: impl FnMut(u32, u32, &mut ForegroundScanBudget) -> Vec<u32>,
) -> Vec<u32> {
    let mut visited = HashSet::new();
    let mut pids = Vec::new();
    // Keep one breadth-first frontier with its own work budget per root, so a large
    // expansion on one side cannot consume the other side's allowance. Frontier turns
    // advance round-robin, sharing the candidate ceiling between the foreground-group
    // leader's subtree and the pane shell's descendants.
    struct Frontier {
        pending: VecDeque<u32>,
        budget: ForegroundScanBudget,
    }
    let mut frontiers: Vec<Frontier> = Vec::new();
    for root in roots {
        if root > 0 && visited.insert(root) {
            frontiers.push(Frontier {
                pending: VecDeque::from([root]),
                budget: ForegroundScanBudget::for_probe(),
            });
        }
    }

    loop {
        let mut progressed = false;
        for frontier in &mut frontiers {
            if pids.len() >= FOREGROUND_TREE_SCAN_LIMIT {
                return pids;
            }
            let Some(pid) = frontier.pending.pop_front() else {
                continue;
            };
            progressed = true;
            pids.push(pid);
            for tid in task_ids(pid, &mut frontier.budget) {
                for child_pid in task_children(pid, tid, &mut frontier.budget) {
                    if child_pid > 0 && visited.insert(child_pid) {
                        frontier.pending.push_back(child_pid);
                    }
                }
            }
        }
        if !progressed {
            return pids;
        }
    }
}

fn process_task_ids(pid: u32, budget: &mut ForegroundScanBudget) -> Vec<u32> {
    let mut ids = Vec::new();
    let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return ids;
    };
    for entry in entries.flatten() {
        if budget.task_entries == 0 {
            break;
        }
        budget.task_entries -= 1;
        if let Some(tid) = numeric_file_name(&entry) {
            ids.push(tid);
        }
    }
    ids
}

fn process_task_children(pid: u32, tid: u32, budget: &mut ForegroundScanBudget) -> Vec<u32> {
    if budget.child_bytes == 0 || budget.child_pids == 0 {
        return Vec::new();
    }
    let Ok(file) = std::fs::File::open(format!("/proc/{pid}/task/{tid}/children")) else {
        return Vec::new();
    };
    read_bounded_pid_list(file, budget)
}

/// Read a whitespace-separated pid list, charging the shared budget for every byte
/// read and every parsed pid. A token cut off by the byte budget is discarded so a
/// partial value is never parsed as a different pid; the final token is only kept
/// when the reader reaches end-of-file.
fn read_bounded_pid_list(mut reader: impl Read, budget: &mut ForegroundScanBudget) -> Vec<u32> {
    let mut pids = Vec::new();
    let mut token = Vec::new();
    let mut buffer = [0_u8; 4096];

    while budget.child_bytes > 0 && budget.child_pids > 0 {
        let read_len = budget.child_bytes.min(buffer.len());
        let bytes_read = match reader.read(&mut buffer[..read_len]) {
            Ok(0) => {
                push_pid_token(&mut pids, &mut token, budget);
                return pids;
            }
            Ok(bytes_read) => bytes_read,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return pids,
        };
        budget.child_bytes -= bytes_read;
        for &byte in &buffer[..bytes_read] {
            if byte.is_ascii_digit() {
                token.push(byte);
                continue;
            }
            push_pid_token(&mut pids, &mut token, budget);
            if budget.child_pids == 0 {
                return pids;
            }
        }
    }

    // The byte budget ran out mid-stream: drop the trailing token in case the read
    // truncated it.
    pids
}

fn push_pid_token(pids: &mut Vec<u32>, token: &mut Vec<u8>, budget: &mut ForegroundScanBudget) {
    if token.is_empty() || budget.child_pids == 0 {
        token.clear();
        return;
    }
    if let Some(pid) = std::str::from_utf8(token)
        .ok()
        .and_then(|text| text.parse::<u32>().ok())
    {
        budget.child_pids -= 1;
        pids.push(pid);
    }
    token.clear();
}

fn numeric_file_name(entry: &std::fs::DirEntry) -> Option<u32> {
    let file_name = entry.file_name();
    let value = file_name.to_str()?;
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn live_process_group_member(process_group_id: u32, pid: u32) -> Option<ProcGroupMember> {
    let (pgrp, comm, state) = process_pgrp_comm_and_state(pid)?;
    (pgrp > 0 && pgrp as u32 == process_group_id).then_some(ProcGroupMember { pid, comm, state })
}

pub fn foreground_group_leader_job(process_group_id: u32) -> Option<ForegroundJob> {
    let (pgrp, name, state) = process_pgrp_comm_and_state(process_group_id)?;
    if pgrp as u32 != process_group_id {
        return None;
    }

    let argv = process_allows_remote_memory_read(state, &name, running_inside_wsl())
        .then(|| process_argv(process_group_id))
        .flatten();
    Some(ForegroundJob {
        process_group_id,
        processes: vec![ForegroundProcess {
            pid: process_group_id,
            name,
            argv0: None,
            cmdline: argv.as_ref().map(|parts| parts.join(" ")),
            argv,
        }],
    })
}

pub(crate) fn sample_terminal_telemetry(
    shell_pid: u32,
    max_processes: usize,
) -> TerminalTelemetrySample {
    let Some(process_group_id) = foreground_process_group_id(shell_pid) else {
        return TerminalTelemetrySample::Unavailable("foreground process group is unavailable");
    };
    let scan_limit = TELEMETRY_TREE_SCAN_LIMIT.max(max_processes);
    let candidates = process_tree_pids_bounded([shell_pid, process_group_id], scan_limit);
    let scan_truncated = candidates.len() == scan_limit;
    let mut processes = Vec::new();
    let mut matching = 0usize;
    for pid in candidates {
        let Some((pgrp, _, _)) = process_pgrp_comm_and_state(pid) else {
            continue;
        };
        if pgrp <= 0 || pgrp as u32 != process_group_id {
            continue;
        }
        matching = matching.saturating_add(1);
        if processes.len() < max_processes {
            if let Some(process) = process_telemetry(pid) {
                processes.push(process);
            }
        }
    }
    processes.sort_unstable_by_key(|process| process.pid);
    if processes.is_empty() {
        return TerminalTelemetrySample::Unavailable("foreground processes exited while sampling");
    }
    TerminalTelemetrySample::Available {
        process_group_id,
        truncated: scan_truncated || matching > processes.len(),
        processes,
    }
}

fn process_tree_pids_bounded(roots: impl IntoIterator<Item = u32>, max_pids: usize) -> Vec<u32> {
    let mut pending = VecDeque::new();
    let mut visited = HashSet::new();
    for pid in roots {
        if pid > 0 && visited.insert(pid) {
            pending.push_back(pid);
        }
    }
    let mut pids = Vec::new();
    while let Some(pid) = pending.pop_front() {
        if pids.len() >= max_pids {
            break;
        }
        pids.push(pid);
        let task_path = format!("/proc/{pid}/task");
        let tasks = std::fs::read_dir(task_path)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| numeric_file_name(&entry))
            .take(TELEMETRY_TASK_SCAN_LIMIT);
        for tid in tasks {
            let Some(children) =
                std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/children")).ok()
            else {
                continue;
            };
            for child in children
                .split_whitespace()
                .filter_map(|value| value.parse::<u32>().ok())
            {
                if visited.len() >= max_pids || !visited.insert(child) {
                    continue;
                }
                pending.push_back(child);
            }
        }
    }
    pids
}

fn process_telemetry(pid: u32) -> Option<ProcessTelemetry> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let close = stat.rfind(')')?;
    let name = stat.get(1 + stat.find('(')?..close)?.to_owned();
    let fields = stat
        .get(close + 2..)?
        .split_whitespace()
        .collect::<Vec<_>>();
    let state = fields.first().map(|value| (*value).to_owned());
    let user_ticks = fields.get(11).and_then(|value| value.parse::<u64>().ok());
    let system_ticks = fields.get(12).and_then(|value| value.parse::<u64>().ok());
    let cpu_ticks = user_ticks
        .zip(system_ticks)
        .map(|(user, system)| user + system);
    let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let cpu_time_ms = (ticks_per_second > 0)
        .then_some(ticks_per_second as u64)
        .and_then(|ticks_per_second| {
            cpu_ticks.map(|ticks| ticks.saturating_mul(1_000) / ticks_per_second)
        });
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let resident_bytes = (page_size > 0)
        .then_some(page_size as u64)
        .and_then(|page_size| {
            fields
                .get(21)
                .and_then(|value| value.parse::<u64>().ok())
                .map(|pages| pages.saturating_mul(page_size))
        });
    let thread_count = fields.get(17).and_then(|value| value.parse::<u32>().ok());
    let io = std::fs::read_to_string(format!("/proc/{pid}/io")).ok();
    let io_value = |key: &str| {
        io.as_deref().and_then(|contents| {
            contents
                .lines()
                .find_map(|line| line.strip_prefix(key)?.trim().parse::<u64>().ok())
        })
    };
    Some(ProcessTelemetry {
        pid,
        name,
        state,
        cpu_time_ms,
        resident_bytes,
        thread_count,
        read_bytes: io_value("read_bytes:"),
        write_bytes: io_value("write_bytes:"),
    })
}

pub fn foreground_process_group_id(child_pid: u32) -> Option<u32> {
    // /proc/<pid>/stat format: "pid (comm) state ppid pgrp session tty_nr tpgid ..."
    // The (comm) field can contain spaces and parens, so we find the last ')' first.
    let stat = std::fs::read_to_string(format!("/proc/{child_pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 2..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After (comm): state(0) ppid(1) pgrp(2) session(3) tty_nr(4) tpgid(5)
    let tpgid: i32 = fields.get(5)?.parse().ok()?;
    (tpgid > 0).then_some(tpgid as u32)
}

pub fn foreground_process_group_id_for_tty_fd(fd: RawFd) -> Option<u32> {
    let pgid = unsafe { libc::tcgetpgrp(fd) };
    (pgid > 0).then_some(pgid as u32)
}

fn process_pgrp_comm_and_state(pid: u32) -> Option<(i32, String, char)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    process_pgrp_comm_and_state_from_stat(&stat)
}

fn process_pgrp_comm_and_state_from_stat(stat: &str) -> Option<(i32, String, char)> {
    let close = stat.rfind(')')?;
    let comm = stat.get(1 + stat.find('(')?..close)?.to_string();
    let rest = stat.get(close + 2..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let state = fields.first()?.chars().next()?;
    let pgrp: i32 = fields.get(2)?.parse().ok()?;
    Some((pgrp, comm, state))
}

fn process_state_allows_remote_memory_read(state: char) -> bool {
    !matches!(state, 'D' | 'Z' | 'X' | 'x')
}

fn process_allows_remote_memory_read(state: char, comm: &str, running_inside_wsl: bool) -> bool {
    process_state_allows_remote_memory_read(state)
        && (!running_inside_wsl || crate::detect::identify_agent(comm).is_none())
}

fn process_argv(pid: u32) -> Option<Vec<String>> {
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let parts: Vec<String> = bytes
        .split(|&b| b == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();
    (!parts.is_empty()).then_some(parts)
}

/// Get the current working directory of a process.
/// Uses /proc/<pid>/cwd symlink.
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// Read a Herdr agent identity hint from a process environment.
pub fn process_agent_hint(pid: u32) -> Option<crate::detect::Agent> {
    if pid == 0 {
        return None;
    }
    let (_, comm, state) = process_pgrp_comm_and_state(pid)?;
    if !process_allows_remote_memory_read(state, &comm, running_inside_wsl()) {
        return None;
    }
    let environ = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    super::parse_agent_env_hint(&environ)
}

pub fn session_processes(child_pid: u32) -> Vec<u32> {
    let Some(session_id) = process_session_id(child_pid) else {
        return Vec::new();
    };

    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let file_name = entry.file_name();
        let Some(pid_str) = file_name.to_str() else {
            continue;
        };
        if !pid_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }

        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if process_session_id(pid) == Some(session_id) {
            pids.push(pid);
        }
    }
    pids
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
            libc::kill(pid as i32, sig);
        }
    }
}

pub fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        true
    } else {
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

pub fn write_clipboard(bytes: &[u8]) -> bool {
    for command in clipboard_commands() {
        if run_clipboard_command(&command, bytes) {
            return true;
        }
    }
    false
}

pub fn read_clipboard_text() -> Option<String> {
    for command in read_clipboard_text_commands() {
        if let Some(text) = read_clipboard_text_with_command(&command) {
            return Some(text);
        }
    }
    None
}

pub fn clipboard_text_matches(_bytes: &[u8]) -> Option<bool> {
    None
}

pub fn open_url(url: &str) -> std::io::Result<Option<std::process::Child>> {
    open_path(std::path::Path::new(url))
}

pub fn open_path(path: &std::path::Path) -> std::io::Result<Option<std::process::Child>> {
    Command::new("xdg-open")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(Some)
}

pub fn read_clipboard_image() -> Option<ClipboardImage> {
    if running_inside_wsl() {
        if let Some(image) = read_wsl_clipboard_image_with_command(|program| Command::new(program))
        {
            return Some(image);
        }
    }

    for (mime, extension) in [
        ("image/png", "png"),
        ("image/jpeg", "jpg"),
        ("image/jpg", "jpg"),
        ("image/gif", "gif"),
        ("image/webp", "webp"),
        ("image/bmp", "bmp"),
    ] {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            if let Some(image) =
                read_validated_clipboard_image("wl-paste", &["--type", mime], extension)
            {
                return Some(image);
            }
        }

        if std::env::var_os("DISPLAY").is_some() {
            if let Some(image) = read_validated_clipboard_image(
                "xclip",
                &["-selection", "clipboard", "-t", mime, "-o"],
                extension,
            ) {
                return Some(image);
            }
        }
    }

    None
}

fn read_wsl_clipboard_image_with_command(
    mut command: impl FnMut(&str) -> Command,
) -> Option<ClipboardImage> {
    let mut command = command("powershell.exe");
    command.args([
        "-NoProfile",
        "-NonInteractive",
        "-STA",
        "-Command",
        "$ErrorActionPreference='Stop'; Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; $image=[System.Windows.Forms.Clipboard]::GetImage(); if ($null -eq $image) { exit 1 }; $stream=[System.IO.MemoryStream]::new(); try { $image.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png); [Console]::OpenStandardOutput().Write($stream.GetBuffer(), 0, [int]$stream.Length) } finally { $stream.Dispose(); $image.Dispose() }",
    ]);
    let bytes = read_clipboard_image_with_spawned_command(command)?;
    bytes_match_image_signature("png", &bytes).then_some(ClipboardImage {
        bytes,
        extension: "png",
    })
}

fn read_validated_clipboard_image(
    program: &str,
    args: &[&str],
    extension: &'static str,
) -> Option<ClipboardImage> {
    let bytes = read_clipboard_image_with_command(program, args)?;
    if !bytes_match_image_signature(extension, &bytes) {
        return None;
    }
    Some(ClipboardImage { bytes, extension })
}

fn bytes_match_image_signature(extension: &str, bytes: &[u8]) -> bool {
    match extension {
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "webp" => bytes.len() >= 12 && bytes.starts_with(b"RIFF") && bytes[8..12] == *b"WEBP",
        "bmp" => {
            if bytes.len() < 26 || !bytes.starts_with(b"BM") {
                return false;
            }
            let offset = u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]) as usize;
            (26..=bytes.len()).contains(&offset)
        }
        _ => false,
    }
}

/// Show a native desktop notification through libnotify's command-line helper.
pub fn show_desktop_notification(title: &str, body: Option<&str>) -> std::io::Result<bool> {
    show_desktop_notification_with_command(title, body, |program| Command::new(program))
}

fn show_desktop_notification_with_command(
    title: &str,
    body: Option<&str>,
    mut command: impl FnMut(&str) -> Command,
) -> std::io::Result<bool> {
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Ok(false);
    }

    let mut cmd = command("notify-send");
    cmd.arg("--app-name")
        .arg(crate::product::DISPLAY_NAME)
        .arg("--")
        .arg(title);
    if let Some(body) = body.filter(|body| !body.is_empty()) {
        cmd.arg(body);
    }
    run_notification_command(cmd)
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

fn read_clipboard_image_with_command(program: &str, args: &[&str]) -> Option<Vec<u8>> {
    let mut command = Command::new(program);
    command.args(args);
    read_clipboard_image_with_spawned_command(command)
}

fn read_clipboard_image_with_spawned_command(command: Command) -> Option<Vec<u8>> {
    read_clipboard_image_with_spawned_command_max(
        command,
        crate::protocol::MAX_CLIPBOARD_IMAGE_PAYLOAD,
    )
}

fn read_clipboard_image_with_spawned_command_max(
    mut command: Command,
    max_bytes: usize,
) -> Option<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;

    let read = match read_limited_reader(stdout, max_bytes) {
        Ok(read) => read,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
    };

    if read == LimitedRead::Oversized {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }

    let status = child.wait().ok()?;
    if !status.success() {
        return None;
    }

    match read {
        LimitedRead::Complete(bytes) => Some(bytes),
        LimitedRead::Empty | LimitedRead::Oversized => None,
    }
}

fn clipboard_commands() -> Vec<ClipboardCommand> {
    let mut commands = Vec::new();

    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "wl-copy",
            args: &["--type", "text/plain;charset=utf-8"],
        });
    }

    if std::env::var_os("DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "xclip",
            args: &["-selection", "clipboard", "-in"],
        });
        commands.push(ClipboardCommand {
            program: "xsel",
            args: &["--clipboard", "--input"],
        });
    }

    commands
}

fn read_clipboard_text_commands() -> Vec<ClipboardCommand> {
    let mut commands = Vec::new();

    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "wl-paste",
            args: &["--type", "text/plain;charset=utf-8"],
        });
        commands.push(ClipboardCommand {
            program: "wl-paste",
            args: &["--type", "text/plain"],
        });
    }

    if std::env::var_os("DISPLAY").is_some() {
        commands.push(ClipboardCommand {
            program: "xclip",
            args: &["-selection", "clipboard", "-out"],
        });
        commands.push(ClipboardCommand {
            program: "xsel",
            args: &["--clipboard", "--output"],
        });
    }

    commands
}

fn read_clipboard_text_with_command(command: &ClipboardCommand) -> Option<String> {
    const MAX_CLIPBOARD_TEXT_BYTES: usize = 1024 * 1024;

    let mut child = Command::new(command.program)
        .args(command.args)
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

    if command.program == "wl-copy" {
        return wait_for_wl_copy_startup(child);
    }

    child.wait().map(|status| status.success()).unwrap_or(false)
}

fn wait_for_wl_copy_startup(mut child: std::process::Child) -> bool {
    const STARTUP_WAIT: std::time::Duration = std::time::Duration::from_millis(100);
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);

    let deadline = std::time::Instant::now() + STARTUP_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(POLL_INTERVAL);
            }
            Ok(None) => return detach_clipboard_owner(child),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn detach_clipboard_owner(child: std::process::Child) -> bool {
    let pid = child.id();
    let child = std::sync::Arc::new(std::sync::Mutex::new(child));
    let reaper_child = std::sync::Arc::clone(&child);
    let reaper = std::thread::Builder::new()
        .name("herdr-wl-copy-reaper".to_string())
        .spawn(move || {
            let wait_result = match reaper_child.lock() {
                Ok(mut child) => child.wait(),
                Err(poisoned) => poisoned.into_inner().wait(),
            };
            if let Err(err) = wait_result {
                tracing::warn!(pid, %err, "failed to reap wl-copy clipboard owner");
            }
        });

    if let Err(err) = reaper {
        tracing::warn!(pid, %err, "failed to start wl-copy clipboard owner reaper");
        let mut child = match child.lock() {
            Ok(child) => child,
            Err(poisoned) => poisoned.into_inner(),
        };
        let _ = child.kill();
        let _ = child.wait();
        return false;
    }

    true
}

fn process_session_id(pid: u32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 2..)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    fields.get(3)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use std::{cell::RefCell, collections::HashMap};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn wsl_marker_detection_matches_kernel_release_text() {
        assert!(text_indicates_wsl("5.15.167.4-microsoft-standard-WSL2"));
        assert!(text_indicates_wsl("4.4.0-19041-Microsoft"));
        assert!(!text_indicates_wsl("6.8.0-64-generic"));
        assert!(!text_indicates_wsl(""));
    }

    #[test]
    fn process_detection_mode_requires_explicit_child_groups_value() {
        assert_eq!(
            parse_process_detection_mode(None),
            Ok(ProcessDetectionMode::Native)
        );
        assert_eq!(
            parse_process_detection_mode(Some("")),
            Ok(ProcessDetectionMode::Native)
        );
        assert_eq!(
            parse_process_detection_mode(Some("native")),
            Ok(ProcessDetectionMode::Native)
        );
        assert_eq!(
            parse_process_detection_mode(Some("child-groups")),
            Ok(ProcessDetectionMode::ChildGroups)
        );
        assert_eq!(parse_process_detection_mode(Some("gvisor")), Err("gvisor"));
    }

    #[test]
    fn child_groups_foreground_group_picks_the_newest_job() {
        let tasks = HashMap::from([(100, vec![100])]);
        let children = HashMap::from([((100, 100), vec![200, 300])]);
        let groups = HashMap::from([(200, 200), (300, 300)]);

        let group = child_groups_foreground_process_group_with(
            100,
            100,
            |pid, _budget| tasks.get(&pid).cloned().unwrap_or_default(),
            |pid, tid, _budget| children.get(&(pid, tid)).cloned().unwrap_or_default(),
            |pid| groups.get(&pid).copied(),
        );

        assert_eq!(group, Some(300));
    }

    #[test]
    fn child_groups_foreground_group_returns_to_the_shell_group() {
        let tasks = HashMap::from([(100, vec![100])]);
        let children = HashMap::from([((100, 100), vec![150, 160])]);
        let groups = HashMap::from([(150, 90), (160, 90)]);

        let group = child_groups_foreground_process_group_with(
            100,
            90,
            |pid, _budget| tasks.get(&pid).cloned().unwrap_or_default(),
            |pid, tid, _budget| children.get(&(pid, tid)).cloned().unwrap_or_default(),
            |pid| groups.get(&pid).copied(),
        );

        assert_eq!(group, Some(90));
    }

    #[test]
    fn child_groups_foreground_group_skips_the_shell_group() {
        let tasks = HashMap::from([(100, vec![100])]);
        let children = HashMap::from([((100, 100), vec![150, 160, 300])]);
        let groups = HashMap::from([(150, 90), (160, 90), (300, 300)]);

        let group = child_groups_foreground_process_group_with(
            100,
            90,
            |pid, _budget| tasks.get(&pid).cloned().unwrap_or_default(),
            |pid, tid, _budget| children.get(&(pid, tid)).cloned().unwrap_or_default(),
            |pid| groups.get(&pid).copied(),
        );

        assert_eq!(group, Some(300));
    }

    #[test]
    fn child_groups_foreground_group_fails_closed_at_the_scan_limit() {
        let children: Vec<u32> = (1..=(CHILD_GROUPS_SCAN_LIMIT as u32 + 10)).collect();
        let mut inspected = 0usize;

        let group = child_groups_foreground_process_group_with(
            100,
            100,
            |_, _budget| vec![100],
            |_, _, _budget| children.clone(),
            |pid| {
                inspected += 1;
                Some(pid as i32)
            },
        );

        assert_eq!(inspected, CHILD_GROUPS_SCAN_LIMIT);
        assert_eq!(group, None);
    }

    #[test]
    fn foreground_members_follow_the_pane_tree_and_filter_by_process_group() {
        let tasks = HashMap::from([
            (100, vec![100, 101]),
            (200, vec![200]),
            (201, vec![201]),
            (210, vec![210]),
            (220, vec![220]),
            (221, vec![221]),
            (300, vec![300]),
        ]);
        let children = HashMap::from([
            ((100, 100), vec![200, 201, 300]),
            ((100, 101), vec![210]),
            ((200, 200), vec![220]),
            ((220, 220), vec![221]),
        ]);
        let processes = HashMap::from([
            (100, (100, "shell")),
            (200, (200, "leader")),
            (201, (200, "pipeline")),
            (210, (200, "thread-child")),
            (220, (220, "intermediate")),
            (221, (200, "nested-agent")),
            (300, (300, "background")),
            (9999, (200, "unrelated-host-process")),
        ]);
        let task_reads = RefCell::new(Vec::new());
        let child_reads = RefCell::new(Vec::new());
        let member_reads = RefCell::new(Vec::new());

        let members = foreground_process_group_members_with(
            100,
            200,
            |pid, _budget| {
                task_reads.borrow_mut().push(pid);
                tasks.get(&pid).cloned().unwrap_or_default()
            },
            |pid, tid, _budget| {
                child_reads.borrow_mut().push((pid, tid));
                children.get(&(pid, tid)).cloned().unwrap_or_default()
            },
            |process_group_id, pid| {
                member_reads.borrow_mut().push(pid);
                let (pgrp, comm) = processes.get(&pid)?;
                (*pgrp == process_group_id).then(|| ProcGroupMember {
                    pid,
                    comm: (*comm).to_string(),
                    state: 'S',
                })
            },
        )
        .unwrap();

        assert_eq!(
            members
                .into_iter()
                .map(|member| (member.pid, member.comm))
                .collect::<Vec<_>>(),
            vec![
                (200, "leader".to_string()),
                (201, "pipeline".to_string()),
                (210, "thread-child".to_string()),
                (221, "nested-agent".to_string()),
            ]
        );
        assert!(child_reads.borrow().contains(&(100, 101)));
        assert!(task_reads.borrow().contains(&220));
        assert!(!task_reads.borrow().contains(&9999));
        assert!(!member_reads.borrow().contains(&9999));
    }

    #[test]
    fn foreground_tree_traversal_is_bounded_by_the_scan_limit() {
        // A pane shell whose descendant tree is far larger than the bound (long-lived
        // agents accumulating children or unreaped zombies) must not make foreground
        // detection read /proc/<pid>/stat for an unbounded number of processes, and
        // the foreground group's own subtree must win the limited scan budget.
        let child_count = FOREGROUND_TREE_SCAN_LIMIT + 200;
        let shell_children: Vec<u32> = (10..10 + child_count as u32).collect();
        // The leader's subtree: leader(2) -> agent(9000) -> agent-child(9001). Both
        // descendants sit behind the shell's backlog and must still be reached.
        let agent_pid = 9000u32;
        let agent_child_pid = 9001u32;
        let stat_reads = RefCell::new(Vec::new());

        let members = foreground_process_group_members_with(
            1,
            2,
            // Every pid has its own single task.
            |pid, _budget| vec![pid],
            |pid, _tid, _budget| match pid {
                // The shell exposes the whole huge unrelated child list.
                1 => shell_children.clone(),
                // The leader exposes its agent child, which exposes its own child.
                2 => vec![agent_pid],
                _ if pid == agent_pid => vec![agent_child_pid],
                _ => Vec::new(),
            },
            |process_group_id, pid| {
                // Every visited pid triggers a /proc/<pid>/stat read; count them.
                stat_reads.borrow_mut().push(pid);
                (process_group_id == 2).then(|| ProcGroupMember {
                    pid,
                    comm: format!("p{pid}"),
                    state: 'S',
                })
            },
        )
        .unwrap();

        // Bounded: foreground detection inspects at most the scan limit processes,
        // regardless of how large the descendant tree has grown.
        assert!(
            stat_reads.borrow().len() <= FOREGROUND_TREE_SCAN_LIMIT,
            "foreground traversal read /proc/stat for {} processes, exceeding the {} bound",
            stat_reads.borrow().len(),
            FOREGROUND_TREE_SCAN_LIMIT
        );
        assert!(members.len() <= FOREGROUND_TREE_SCAN_LIMIT);
        // The foreground-group leader and its descendants are visited before the
        // shell's unrelated backlog, so the detected agent survives truncation.
        assert!(
            members.iter().any(|member| member.pid == 2),
            "group leader must survive truncation"
        );
        assert!(
            members.iter().any(|member| member.pid == agent_child_pid),
            "leader descendants must be visited before unrelated shell descendants"
        );
    }

    #[test]
    fn foreground_tree_traversal_shares_the_scan_limit_between_roots() {
        // A foreground-group leader with more descendants than the scan limit must not
        // starve the pane shell's own foreground-group children, such as pipeline
        // members that live under the shell rather than under the leader.
        let leader_children: Vec<u32> =
            (100..100 + FOREGROUND_TREE_SCAN_LIMIT as u32 + 200).collect();
        let pipeline_pid = 9000u32;
        let stat_reads = RefCell::new(Vec::new());

        let members = foreground_process_group_members_with(
            1,
            2,
            |pid, _budget| vec![pid],
            |pid, _tid, _budget| match pid {
                // The shell exposes a foreground-group pipeline child...
                1 => vec![pipeline_pid],
                // ...while the leader's own subtree already exceeds the bound.
                2 => leader_children.clone(),
                _ => Vec::new(),
            },
            |process_group_id, pid| {
                stat_reads.borrow_mut().push(pid);
                (process_group_id == 2).then(|| ProcGroupMember {
                    pid,
                    comm: format!("p{pid}"),
                    state: 'S',
                })
            },
        )
        .unwrap();

        assert!(stat_reads.borrow().len() <= FOREGROUND_TREE_SCAN_LIMIT);
        assert!(
            members.iter().any(|member| member.pid == pipeline_pid),
            "shell-side foreground members must survive an oversized leader subtree"
        );
    }

    #[test]
    fn bounded_child_list_read_keeps_complete_tokens_at_eof() {
        let mut budget = ForegroundScanBudget::for_probe();
        let pids = read_bounded_pid_list(std::io::Cursor::new(b"10 20 30"), &mut budget);
        assert_eq!(pids, vec![10, 20, 30]);
        assert!(budget.child_bytes < FOREGROUND_CHILD_BYTE_LIMIT);
    }

    #[test]
    fn bounded_child_list_read_drops_a_token_cut_off_by_the_byte_budget() {
        let mut budget = ForegroundScanBudget::for_probe();
        // The byte budget ends inside the trailing pid, which must not parse as 3.
        budget.child_bytes = 8;
        let pids = read_bounded_pid_list(std::io::Cursor::new(b" 10 20 300"), &mut budget);
        assert_eq!(pids, vec![10, 20]);
        assert_eq!(budget.child_bytes, 0);
    }

    #[test]
    fn bounded_child_list_read_stops_at_the_pid_budget() {
        let mut budget = ForegroundScanBudget::for_probe();
        budget.child_pids = 2;
        let pids = read_bounded_pid_list(std::io::Cursor::new(b"10 20 30 40"), &mut budget);
        assert_eq!(pids, vec![10, 20]);
        assert_eq!(budget.child_pids, 0);
    }

    #[test]
    fn foreground_tree_traversal_reserves_enumeration_budget_per_root() {
        // The leader expansion spending its whole budget must not stop the shell root
        // from enumerating its own children.
        let leader_consumed = RefCell::new(false);
        let shell_saw_budget = RefCell::new(false);

        let members = foreground_process_group_members_with(
            1,
            2,
            |pid, budget| {
                if pid == 2 {
                    *leader_consumed.borrow_mut() = true;
                } else if pid == 1 {
                    *shell_saw_budget.borrow_mut() = budget.task_entries > 0;
                }
                budget.task_entries = 0;
                budget.child_bytes = 0;
                budget.child_pids = 0;
                vec![pid]
            },
            |_pid, _tid, _budget| Vec::new(),
            |_process_group_id, _pid| None,
        );

        assert!(*leader_consumed.borrow());
        assert!(
            *shell_saw_budget.borrow(),
            "shell root must keep its own budget after the leader spends its own"
        );
        assert!(members.is_none());
    }

    #[test]
    fn foreground_members_degrade_to_the_direct_group_leader() {
        let members = foreground_process_group_members_with(
            100,
            200,
            |_, _budget| Vec::new(),
            |_, _, _budget| Vec::new(),
            |process_group_id, pid| {
                (pid == process_group_id).then(|| ProcGroupMember {
                    pid,
                    comm: "leader".to_string(),
                    state: 'S',
                })
            },
        )
        .unwrap();

        assert_eq!(
            members,
            vec![ProcGroupMember {
                pid: 200,
                comm: "leader".to_string(),
                state: 'S',
            }]
        );
    }

    #[test]
    fn foreground_members_observe_new_children_without_a_snapshot_cache() {
        let children = RefCell::new(HashMap::from([((100, 100), vec![200])]));
        let discover = || {
            foreground_process_group_members_with(
                100,
                200,
                |pid, _budget| vec![pid],
                |pid, tid, _budget| {
                    children
                        .borrow()
                        .get(&(pid, tid))
                        .cloned()
                        .unwrap_or_default()
                },
                |process_group_id, pid| {
                    [200, 201]
                        .contains(&pid)
                        .then(|| ProcGroupMember {
                            pid,
                            comm: format!("member-{pid}"),
                            state: 'S',
                        })
                        .filter(|_| process_group_id == 200)
                },
            )
            .unwrap()
            .into_iter()
            .map(|member| member.pid)
            .collect::<Vec<_>>()
        };

        assert_eq!(discover(), vec![200]);
        children.borrow_mut().insert((100, 100), vec![200, 201]);
        assert_eq!(discover(), vec![200, 201]);
    }

    #[test]
    fn proc_stat_parsing_keeps_group_leader_inputs_live() {
        assert_eq!(
            process_pgrp_comm_and_state_from_stat("123 (name with ) paren) S 1 456 789 0 456"),
            Some((456, "name with ) paren".to_string(), 'S'))
        );
    }

    #[test]
    fn foreground_job_does_not_read_remote_memory_for_uninterruptible_members() {
        let argv_reads = RefCell::new(Vec::new());
        let job = foreground_job_from_members(
            200,
            vec![
                ProcGroupMember {
                    pid: 200,
                    comm: "codex".to_string(),
                    state: 'D',
                },
                ProcGroupMember {
                    pid: 201,
                    comm: "helper".to_string(),
                    state: 'S',
                },
            ],
            true,
            |pid| {
                argv_reads.borrow_mut().push(pid);
                Some(vec![format!("process-{pid}")])
            },
        )
        .unwrap();

        assert_eq!(argv_reads.into_inner(), vec![201]);
        assert_eq!(job.processes[0].name, "codex");
        assert_eq!(job.processes[0].argv, None);
        assert_eq!(job.processes[1].argv, Some(vec!["process-201".to_string()]));
    }

    #[test]
    fn foreground_job_on_wsl_skips_known_agents_but_reads_wrappers() {
        let argv_reads = RefCell::new(Vec::new());
        let job = foreground_job_from_members(
            200,
            vec![
                ProcGroupMember {
                    pid: 200,
                    comm: "codex".to_string(),
                    state: 'S',
                },
                ProcGroupMember {
                    pid: 201,
                    comm: "node".to_string(),
                    state: 'S',
                },
            ],
            true,
            |pid| {
                argv_reads.borrow_mut().push(pid);
                Some(vec![format!("process-{pid}")])
            },
        )
        .unwrap();

        assert_eq!(argv_reads.into_inner(), vec![201]);
        assert_eq!(job.processes[0].name, "codex");
        assert_eq!(job.processes[0].argv, None);
        assert_eq!(job.processes[1].argv, Some(vec!["process-201".to_string()]));
    }

    #[test]
    fn remote_memory_reads_reject_dead_and_uninterruptible_states() {
        for state in ['D', 'Z', 'X', 'x'] {
            assert!(!process_state_allows_remote_memory_read(state));
        }
        for state in ['R', 'S', 'I', 'T', 't'] {
            assert!(process_state_allows_remote_memory_read(state));
        }
    }

    #[test]
    fn process_telemetry_reads_safe_numeric_fields_for_current_process() {
        let process = process_telemetry(std::process::id()).expect("current process telemetry");
        assert_eq!(process.pid, std::process::id());
        assert!(!process.name.is_empty());
        assert!(process.cpu_time_ms.is_some());
        assert!(process.resident_bytes.is_some());
        assert!(process.thread_count.is_some());
    }

    #[test]
    fn clipboard_commands_prefer_wayland_when_available() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
            std::env::remove_var("DISPLAY");
        }
        let commands = clipboard_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].program, "wl-copy");
    }

    #[test]
    fn wl_copy_owner_does_not_block_clipboard_write() {
        use std::ffi::OsString;
        use std::os::unix::fs::PermissionsExt;
        use std::path::PathBuf;
        use std::sync::mpsc;
        use std::time::{Duration, Instant, SystemTime};

        struct Cleanup {
            old_path: Option<OsString>,
            temp_dir: PathBuf,
            owner_pid: Option<i32>,
        }

        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Some(pid) = self.owner_pid {
                    unsafe {
                        libc::kill(pid, libc::SIGTERM);
                    }
                }
                unsafe {
                    match self.old_path.take() {
                        Some(path) => std::env::set_var("PATH", path),
                        None => std::env::remove_var("PATH"),
                    }
                    std::env::remove_var("HERDR_TEST_WL_COPY_MARKER");
                    std::env::remove_var("HERDR_TEST_WL_COPY_PAYLOAD");
                    std::env::remove_var("HERDR_TEST_WL_COPY_ARGS");
                }
                let _ = std::fs::remove_dir_all(&self.temp_dir);
            }
        }

        let _guard = env_lock().lock().unwrap();
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system time should follow unix epoch")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!(
            "herdr-fake-wl-copy-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should be created");
        let mut cleanup = Cleanup {
            old_path: std::env::var_os("PATH"),
            temp_dir: temp_dir.clone(),
            owner_pid: None,
        };
        let fake_wl_copy = temp_dir.join("wl-copy");
        let marker = temp_dir.join("owner-pid");
        let payload = temp_dir.join("payload");
        let args = temp_dir.join("args");
        std::fs::write(
            &fake_wl_copy,
            "#!/bin/sh\ncat > \"$HERDR_TEST_WL_COPY_PAYLOAD\"\nprintf '%s\\n' \"$@\" > \"$HERDR_TEST_WL_COPY_ARGS\"\nprintf '%s' \"$$\" > \"$HERDR_TEST_WL_COPY_MARKER\"\nexec sleep 30\n",
        )
        .expect("fake wl-copy should be written");
        let mut permissions = std::fs::metadata(&fake_wl_copy)
            .expect("fake wl-copy metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&fake_wl_copy, permissions)
            .expect("fake wl-copy should be executable");

        let test_path = match cleanup.old_path.as_ref() {
            Some(path) => {
                let mut paths = vec![temp_dir.clone()];
                paths.extend(std::env::split_paths(path));
                std::env::join_paths(paths).expect("test path should be valid")
            }
            None => temp_dir.clone().into_os_string(),
        };
        unsafe {
            std::env::set_var("PATH", test_path);
            std::env::set_var("HERDR_TEST_WL_COPY_MARKER", &marker);
            std::env::set_var("HERDR_TEST_WL_COPY_PAYLOAD", &payload);
            std::env::set_var("HERDR_TEST_WL_COPY_ARGS", &args);
        }

        let (result_tx, result_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            let command = ClipboardCommand {
                program: "wl-copy",
                args: &["--type", "text/plain;charset=utf-8"],
            };
            let _ = result_tx.send(run_clipboard_command(&command, b"clipboard text"));
        });

        let marker_deadline = Instant::now() + Duration::from_secs(2);
        while !marker.exists() && Instant::now() < marker_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let owner_pid: i32 = std::fs::read_to_string(&marker)
            .expect("fake wl-copy should enter its clipboard-owner phase")
            .parse()
            .expect("owner pid should be numeric");
        cleanup.owner_pid = Some(owner_pid);
        let returned_while_owner_running = result_rx
            .recv_timeout(Duration::from_secs(2))
            .is_ok_and(|result| result);
        let actual_payload = std::fs::read(&payload).expect("fake wl-copy should record stdin");
        let actual_args = std::fs::read_to_string(&args).expect("fake wl-copy should record args");

        unsafe {
            libc::kill(owner_pid, libc::SIGTERM);
        }
        let reap_deadline = Instant::now() + Duration::from_secs(2);
        while process_exists(owner_pid as u32) && Instant::now() < reap_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let owner_was_reaped = !process_exists(owner_pid as u32);
        cleanup.owner_pid = None;
        writer.join().expect("clipboard writer thread should join");
        drop(cleanup);

        assert!(
            returned_while_owner_running,
            "clipboard writes must return while wl-copy remains alive to own the selection"
        );
        assert_eq!(actual_payload, b"clipboard text");
        assert_eq!(actual_args, "--type\ntext/plain;charset=utf-8\n");
        assert!(
            owner_was_reaped,
            "wl-copy owner should be reaped after exit"
        );
    }

    #[test]
    fn failed_wl_copy_uses_x11_fallback() {
        use std::ffi::OsString;
        use std::os::unix::fs::PermissionsExt;
        use std::path::PathBuf;
        use std::time::{SystemTime, UNIX_EPOCH};

        struct Cleanup {
            old_path: Option<OsString>,
            old_wayland_display: Option<OsString>,
            old_display: Option<OsString>,
            temp_dir: PathBuf,
        }

        impl Drop for Cleanup {
            fn drop(&mut self) {
                unsafe {
                    match self.old_path.take() {
                        Some(value) => std::env::set_var("PATH", value),
                        None => std::env::remove_var("PATH"),
                    }
                    match self.old_wayland_display.take() {
                        Some(value) => std::env::set_var("WAYLAND_DISPLAY", value),
                        None => std::env::remove_var("WAYLAND_DISPLAY"),
                    }
                    match self.old_display.take() {
                        Some(value) => std::env::set_var("DISPLAY", value),
                        None => std::env::remove_var("DISPLAY"),
                    }
                    std::env::remove_var("HERDR_TEST_XCLIP_PAYLOAD");
                }
                let _ = std::fs::remove_dir_all(&self.temp_dir);
            }
        }

        let _guard = env_lock().lock().unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should follow unix epoch")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!(
            "herdr-failed-wl-copy-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should be created");
        let cleanup = Cleanup {
            old_path: std::env::var_os("PATH"),
            old_wayland_display: std::env::var_os("WAYLAND_DISPLAY"),
            old_display: std::env::var_os("DISPLAY"),
            temp_dir: temp_dir.clone(),
        };
        let payload = temp_dir.join("xclip-payload");
        let fake_wl_copy = temp_dir.join("wl-copy");
        let fake_xclip = temp_dir.join("xclip");
        std::fs::write(&fake_wl_copy, "#!/bin/sh\n/bin/cat >/dev/null\nexit 7\n")
            .expect("fake wl-copy should be written");
        std::fs::write(
            &fake_xclip,
            "#!/bin/sh\n/bin/cat > \"$HERDR_TEST_XCLIP_PAYLOAD\"\n",
        )
        .expect("fake xclip should be written");
        for command in [&fake_wl_copy, &fake_xclip] {
            let mut permissions = std::fs::metadata(command)
                .expect("fake clipboard command metadata")
                .permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(command, permissions)
                .expect("fake clipboard command should be executable");
        }

        unsafe {
            std::env::set_var("PATH", &temp_dir);
            std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
            std::env::set_var("DISPLAY", ":0");
            std::env::set_var("HERDR_TEST_XCLIP_PAYLOAD", &payload);
        }

        assert!(write_clipboard(b"clipboard fallback"));
        assert_eq!(
            std::fs::read(&payload).expect("xclip should record stdin"),
            b"clipboard fallback"
        );
        drop(cleanup);
    }

    #[test]
    fn finite_clipboard_commands_report_exit_status() {
        let success = ClipboardCommand {
            program: "sh",
            args: &["-c", "cat >/dev/null"],
        };
        let failure = ClipboardCommand {
            program: "sh",
            args: &["-c", "cat >/dev/null; exit 7"],
        };

        assert!(run_clipboard_command(&success, b"clipboard text"));
        assert!(!run_clipboard_command(&failure, b"clipboard text"));
    }

    #[test]
    fn clipboard_commands_include_x11_fallbacks() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
            std::env::set_var("DISPLAY", ":0");
        }
        let commands = clipboard_commands();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].program, "xclip");
        assert_eq!(commands[1].program, "xsel");
    }

    #[test]
    fn read_clipboard_text_commands_include_session_backends() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
            std::env::set_var("DISPLAY", ":0");
        }

        let commands = read_clipboard_text_commands();
        assert_eq!(commands[0].program, "wl-paste");
        assert_eq!(commands[1].program, "wl-paste");
        assert_eq!(commands[2].program, "xclip");
        assert_eq!(commands[3].program, "xsel");
    }

    #[test]
    fn read_clipboard_text_with_command_reads_utf8() {
        let command = ClipboardCommand {
            program: "printf",
            args: &["feature/linear-302"],
        };

        assert_eq!(
            read_clipboard_text_with_command(&command).as_deref(),
            Some("feature/linear-302")
        );
    }

    #[test]
    fn read_clipboard_text_with_command_rejects_oversized_output() {
        let command = ClipboardCommand {
            program: "sh",
            args: &["-c", "yes x | head -c 1048578"],
        };

        assert_eq!(read_clipboard_text_with_command(&command), None);
    }

    #[test]
    fn read_clipboard_image_with_spawned_command_reads_under_limit() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("printf image");

        assert_eq!(
            read_clipboard_image_with_spawned_command_max(command, 16),
            Some(b"image".to_vec())
        );
    }

    #[test]
    fn read_clipboard_image_with_spawned_command_rejects_over_limit() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("printf oversized");

        assert_eq!(
            read_clipboard_image_with_spawned_command_max(command, 4),
            None
        );
    }

    #[test]
    fn read_clipboard_image_rejects_xclip_text_served_for_image_target() {
        let _guard = env_lock().lock().unwrap();
        let temp_dir =
            std::env::temp_dir().join(format!("herdr-fake-xclip-{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should be created");
        let fake_xclip = temp_dir.join("xclip");
        std::fs::write(&fake_xclip, "#!/bin/sh\nprintf '# Tasks'\n")
            .expect("fake xclip should be written");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = std::fs::metadata(&fake_xclip)
                .expect("fake xclip metadata")
                .permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&fake_xclip, permissions)
                .expect("fake xclip should be executable");
        }

        let old_path = std::env::var_os("PATH");
        let test_path = match old_path.as_ref() {
            Some(path) => {
                let mut paths = vec![temp_dir.clone()];
                paths.extend(std::env::split_paths(path));
                std::env::join_paths(paths).expect("test path should be valid")
            }
            None => temp_dir.clone().into_os_string(),
        };

        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
            std::env::set_var("DISPLAY", ":0");
            std::env::set_var("PATH", test_path);
        }

        let result = read_clipboard_image();

        unsafe {
            match old_path {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
        let _ = std::fs::remove_file(fake_xclip);
        let _ = std::fs::remove_dir(temp_dir);

        assert_eq!(result, None);
    }

    #[test]
    fn read_clipboard_image_rejects_wayland_xclip_fallback_text_for_image_target() {
        let _guard = env_lock().lock().unwrap();
        let temp_dir =
            std::env::temp_dir().join(format!("herdr-fake-wayland-xclip-{}", std::process::id()));
        std::fs::create_dir_all(&temp_dir).expect("temp dir should be created");
        let fake_wl_paste = temp_dir.join("wl-paste");
        let fake_xclip = temp_dir.join("xclip");
        std::fs::write(&fake_wl_paste, "#!/bin/sh\nexit 1\n")
            .expect("fake wl-paste should be written");
        std::fs::write(&fake_xclip, "#!/bin/sh\nprintf '# Tasks'\n")
            .expect("fake xclip should be written");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            for command in [&fake_wl_paste, &fake_xclip] {
                let mut permissions = std::fs::metadata(command)
                    .expect("fake clipboard command metadata")
                    .permissions();
                permissions.set_mode(0o700);
                std::fs::set_permissions(command, permissions)
                    .expect("fake clipboard command should be executable");
            }
        }

        let old_path = std::env::var_os("PATH");
        let test_path = match old_path.as_ref() {
            Some(path) => {
                let mut paths = vec![temp_dir.clone()];
                paths.extend(std::env::split_paths(path));
                std::env::join_paths(paths).expect("test path should be valid")
            }
            None => temp_dir.clone().into_os_string(),
        };

        unsafe {
            std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
            std::env::set_var("DISPLAY", ":0");
            std::env::set_var("PATH", test_path);
        }

        let result = read_clipboard_image();

        unsafe {
            match old_path {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
        let _ = std::fs::remove_file(fake_wl_paste);
        let _ = std::fs::remove_file(fake_xclip);
        let _ = std::fs::remove_dir(temp_dir);

        assert_eq!(result, None);
    }

    #[test]
    fn read_validated_clipboard_image_accepts_real_png_payload() {
        assert_eq!(
            read_validated_clipboard_image(
                "sh",
                &["-c", "printf '\\211PNG\\r\\n\\032\\nrest-of-image'"],
                "png"
            ),
            Some(ClipboardImage {
                bytes: b"\x89PNG\r\n\x1a\nrest-of-image".to_vec(),
                extension: "png",
            })
        );
    }

    #[test]
    fn read_wsl_clipboard_image_accepts_png_from_windows_command() {
        assert_eq!(
            read_wsl_clipboard_image_with_command(|program| {
                assert_eq!(program, "powershell.exe");
                let mut command = Command::new("sh");
                command
                    .arg("-c")
                    .arg("printf '\\211PNG\\r\\n\\032\\nrest-of-image'");
                command
            }),
            Some(ClipboardImage {
                bytes: b"\x89PNG\r\n\x1a\nrest-of-image".to_vec(),
                extension: "png",
            })
        );
    }

    #[test]
    fn image_signatures_match_only_their_format() {
        assert!(bytes_match_image_signature("png", b"\x89PNG\r\n\x1a\n..."));
        assert!(bytes_match_image_signature(
            "jpg",
            &[0xFF, 0xD8, 0xFF, 0xE0]
        ));
        assert!(bytes_match_image_signature("gif", b"GIF87a..."));
        assert!(bytes_match_image_signature("gif", b"GIF89a..."));
        assert!(bytes_match_image_signature(
            "webp",
            b"RIFF\x10\x00\x00\x00WEBPVP8 "
        ));

        let mut bmp = vec![0u8; 26];
        bmp[..2].copy_from_slice(b"BM");
        bmp[10] = 26;
        assert!(bytes_match_image_signature("bmp", &bmp));

        assert!(!bytes_match_image_signature("png", b"# Tasks"));
        assert!(!bytes_match_image_signature("jpg", b"plain clipboard text"));
        assert!(!bytes_match_image_signature("gif", b""));
        assert!(!bytes_match_image_signature("webp", b"RIFF but not webp"));
        assert!(!bytes_match_image_signature("bmp", b"\x89PNG\r\n\x1a\n"));
        assert!(!bytes_match_image_signature(
            "bmp",
            b"BM text is not a bitmap"
        ));
        assert!(!bytes_match_image_signature("svg", b"<svg></svg>"));
    }

    #[test]
    fn desktop_notification_sets_app_name_and_separates_option_like_titles() {
        let _guard = env_lock().lock().unwrap();
        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
            std::env::set_var("DISPLAY", ":0");
        }

        let path =
            std::env::temp_dir().join(format!("herdr-notify-send-args-{}", std::process::id()));
        let script = "printf '%s\\n' \"$@\" > \"$HERDR_NOTIFY_ARGS\"";
        let shown = show_desktop_notification_with_command("-danger", Some("body"), |_| {
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg(script)
                .arg("notify-send")
                .env("HERDR_NOTIFY_ARGS", &path);
            cmd
        })
        .expect("notification command should run");

        assert!(shown);
        let args = std::fs::read_to_string(&path).expect("args file");
        let _ = std::fs::remove_file(&path);
        assert_eq!(args, "--app-name\nvqro\n--\n-danger\nbody\n");
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
