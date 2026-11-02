//! Explicit-invocation evidence. Never reused as a render/detection cache.
use super::*;
use crate::action_invocation::{
    InvocationAccount, InvocationForeground, InvocationPeer, InvocationProcess,
};
use std::{collections::HashMap, io, path::Path};
use windows_sys::Win32::{
    Foundation::{ERROR_NO_MORE_FILES, WAIT_TIMEOUT},
    System::{
        Console::{SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_C_EVENT},
        Threading::{GetProcessId, WaitForSingleObject, PROCESS_SYNCHRONIZE},
    },
};

#[cfg(test)]
mod tests;

const PROCESS_LIMIT: usize = 32768;

fn pipe_client_pid(stream: &crate::ipc::LocalStream) -> Option<u32> {
    use std::os::windows::io::AsHandle;
    use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
    let crate::ipc::LocalStream::NamedPipe(pipe) = stream;
    let mut pid = 0;
    // SAFETY: borrowed server-end pipe handle and valid writable PID output.
    (unsafe { GetNamedPipeClientProcessId(pipe.as_handle().as_raw_handle(), &mut pid) } != 0
        && pid != 0)
        .then_some(pid)
}

pub(crate) fn socket_peer_birth(stream: &crate::ipc::LocalStream) -> Option<(u32, u64)> {
    let pid = pipe_client_pid(stream)?;
    Some((pid, invocation_process_birth(pid)?))
}

pub(crate) fn invocation_peer(stream: &crate::ipc::LocalStream) -> Option<InvocationPeer> {
    invocation_peer_for_process(pipe_client_pid(stream)?)
}

fn live_process(pid: u32) -> Option<ProcessHandle> {
    if pid == 0 {
        return None;
    }
    let handle = ProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE)?;
    // SAFETY: the RAII handle is live and has query/synchronize rights.
    if unsafe { GetProcessId(handle.0) } != pid
        || unsafe { WaitForSingleObject(handle.0, 0) } != WAIT_TIMEOUT
    {
        return None;
    }
    Some(handle)
}

pub(crate) fn invocation_process_birth(pid: u32) -> Option<u64> {
    let handle = live_process(pid)?;
    process_creation_time(handle.0).filter(|birth| *birth != 0)
}

fn parents() -> Option<HashMap<u32, u32>> {
    Some(
        topology_snapshot()?
            .entries
            .iter()
            .map(|entry| (entry.pid, entry.parent_pid))
            .collect(),
    )
}

fn topology_snapshot() -> Option<ProcessSnapshot> {
    // SAFETY: snapshot is owned below and entry has its required structure size.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return None;
    }
    let _handle = ProcessHandle(raw);
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    let mut ok = unsafe { Process32FirstW(raw, &mut entry) } != 0;
    while ok {
        if result.len() >= PROCESS_LIMIT || !seen.insert(entry.th32ProcessID) {
            return None;
        }
        result.push(WindowsProcessEntry {
            pid: entry.th32ProcessID,
            parent_pid: entry.th32ParentProcessID,
            name: nul_terminated_utf16_to_string(&entry.szExeFile),
            command: OnceLock::new(),
        });
        ok = unsafe { Process32NextW(raw, &mut entry) } != 0;
    }
    // A failed enumeration is not an empty/partial topology proof.
    (io::Error::last_os_error().raw_os_error() == Some(ERROR_NO_MORE_FILES as i32))
        .then(|| ProcessSnapshot::new(result))
}

fn entry(
    pid: u32,
    topology: &HashMap<u32, u32>,
    sid: &str,
) -> Option<(InvocationProcess, ProcessHandle)> {
    let handle = live_process(pid)?;
    if account_sid_text(handle.0).ok()? != sid {
        return None;
    }
    let birth = process_creation_time(handle.0).filter(|birth| *birth != 0)?;
    let parent_pid = *topology.get(&pid)?;
    let image = process_executable_path(handle.0)?;
    let name = Path::new(&image).file_name()?.to_str()?.to_owned();
    let cmdline = read_process_command_line(handle.0)?;
    let argv = command_line_to_argv(&cmdline)?;
    if argv.is_empty()
        || argv[0].is_empty()
        || argv.iter().map(|arg| arg.len() + 1).sum::<usize>() > 16384
    {
        return None;
    }
    if account_sid_text(handle.0).ok()? != sid || process_creation_time(handle.0) != Some(birth) {
        return None;
    }
    Some((
        InvocationProcess {
            pid,
            parent_pid,
            birth,
            process: super::super::ForegroundProcess {
                pid,
                name,
                argv0: argv.first().cloned(),
                argv: Some(argv),
                cmdline: Some(cmdline),
            },
        },
        handle,
    ))
}

pub(crate) fn invocation_peer_for_process(pid: u32) -> Option<InvocationPeer> {
    let sid = current_account_sid_text().ok()?;
    let topology = parents()?;
    let server_pid = std::process::id();
    let server_birth = invocation_process_birth(server_pid)?;
    let mut held = Vec::new();
    let mut child_birth = u64::MAX;
    let ancestry = super::super::capture_invocation_ancestry(pid, |pid| {
        let process = live_process(pid)?;
        let birth = process_creation_time(process.0).filter(|birth| *birth != 0)?;
        if birth > child_birth {
            return None;
        } // A recycled parent is not this child's parent.
        child_birth = birth;
        if pid == server_pid {
            return (birth == server_birth).then_some(super::super::InvocationAncestor::Boundary);
        }
        let owner = account_sid_text(process.0).ok()?;
        if owner != sid {
            return Some(super::super::InvocationAncestor::Boundary);
        }
        let (entry, handle) = entry(pid, &topology, &sid)?;
        if entry.birth != birth {
            return None;
        }
        held.push(handle);
        Some(super::super::InvocationAncestor::Process(entry))
    })?;
    let mut peer = InvocationPeer {
        account: InvocationAccount::Windows { sid },
        boot_id: boot_id()?,
        ancestry,
        foreground_cli: false,
        foreground: None,
    };
    if !invocation_peer_is_current(&peer) {
        return None;
    }
    // A complete proof is an optional refinement; failure never broadens caller class.
    peer.foreground_cli = transparent_launcher(&peer);
    peer.foreground = Some(foreground_for_ancestry(&peer).unwrap_or_default());
    // Hold all captured incarnations through the final proof/currentness pass.
    invocation_peer_is_current(&peer).then_some(peer)
}

pub(crate) fn invocation_peer_is_current(peer: &InvocationPeer) -> bool {
    let InvocationAccount::Windows { sid } = &peer.account else {
        return false;
    };
    let Some(topology) = parents() else {
        return false;
    };
    let current = super::super::invocation_ancestry_is_current(&peer.ancestry, |pid| {
        let expected = peer.ancestry.iter().find(|entry| entry.pid == pid)?;
        let (now, _handle) = entry(pid, &topology, sid)?;
        (now.process == expected.process).then_some((now.birth, now.parent_pid))
    });
    if !current
        || peer
            .ancestry
            .windows(2)
            .any(|pair| pair[0].parent_pid != pair[1].pid || pair[0].birth < pair[1].birth)
    {
        return false;
    }
    if let Some(last) = peer.ancestry.last() {
        // A retained prefix can stop at the bound; never invent a missing root.
        // A positively matched server boundary additionally rejects PID reuse.
        if last.parent_pid == std::process::id()
            && invocation_process_birth(last.parent_pid).is_none_or(|birth| birth > last.birth)
        {
            return false;
        }
    }
    if peer.foreground.as_ref().is_some_and(|expected| {
        !expected.is_empty() && foreground_for_ancestry(peer).as_ref() != Some(expected)
    }) {
        return false;
    }
    !peer.foreground_cli || transparent_launcher(peer)
}

const FOREGROUND_QUERY_LIMIT: usize = 256;

fn foreground_for_ancestry(peer: &InvocationPeer) -> Option<Vec<InvocationForeground>> {
    let InvocationAccount::Windows { sid } = &peer.account else {
        return None;
    };
    let roots: Vec<_> = peer
        .ancestry
        .iter()
        .filter(|entry| {
            super::super::is_pane_shell_process_name(&entry.process.name)
                || crate::detect::identify_agent_in_job(&crate::platform::ForegroundJob {
                    process_group_id: entry.pid,
                    processes: vec![entry.process.clone()],
                })
                .is_some()
        })
        .map(|entry| (entry.pid, entry.birth))
        .collect();
    Some(
        foreground_selection(&roots, sid)?
            .into_iter()
            .filter(|proof| {
                peer.ancestry
                    .iter()
                    .any(|entry| entry.pid == proof.pid && entry.birth == proof.birth)
            })
            .collect(),
    )
}

/// An explicit off-actor API probe. The result includes the selected incarnation,
/// so a reused PID cannot pass the caller's before/after consistency check.
pub(crate) fn invocation_foreground_observation(root: u32) -> Option<(u32, u64)> {
    let sid = current_account_sid_text().ok()?;
    let birth = invocation_process_birth(root)?;
    let proof = foreground_selection(&[(root, birth)], &sid)?
        .into_iter()
        .next()?;
    Some((proof.pid, proof.birth))
}

fn foreground_selection(roots: &[(u32, u64)], sid: &str) -> Option<Vec<InvocationForeground>> {
    if roots.len() > 64 {
        return None;
    }
    let snapshot = topology_snapshot()?; // Complete, fresh, bounded; neither foreground cache.
    let topology: HashMap<_, _> = snapshot
        .entries
        .iter()
        .map(|entry| (entry.pid, entry.parent_pid))
        .collect();
    let mut required = HashSet::new();
    for &(root, _) in roots {
        required.insert(root);
        for descendant in descendant_entries(root, &snapshot) {
            required.insert(descendant.pid);
            if required.len() > FOREGROUND_QUERY_LIMIT {
                return None;
            }
        }
    }
    if required.len() > FOREGROUND_QUERY_LIMIT {
        return None;
    }
    let mut captured = Vec::new();
    for pid in required {
        let (process, handle) = entry(pid, &topology, sid)?;
        let target = snapshot.entry(pid)?;
        if !target.name.eq_ignore_ascii_case(&process.process.name) {
            return None;
        }
        target
            .command
            .set(WindowsProcessCommand {
                creation_time: Some(process.birth),
                argv0: process.process.argv0.clone(),
                argv: process.process.argv.clone(),
                cmdline: process.process.cmdline.clone(),
            })
            .ok()?;
        captured.push((process, handle));
    }
    for (process, _) in &captured {
        if captured
            .iter()
            .any(|(parent, _)| parent.pid == process.parent_pid && parent.birth > process.birth)
        {
            return None;
        }
    }
    let mut result = Vec::new();
    for &(root_pid, root_birth) in roots {
        if snapshot.entry(root_pid)?.command().creation_time != Some(root_birth) {
            return None;
        }
        // Use the existing uncached local-tree selector. Escaped Git Bash jobs
        // cannot prove ancestry and are deliberately not searched via runtime
        // environment markers. Every local candidate's argv is prevalidated.
        let selected = select_pane_foreground_job_from_snapshot_with_runtime_inspection(
            root_pid,
            &snapshot,
            |_| false,
            |_| None,
        )?;
        let birth = snapshot
            .entry(selected.process_group_id)?
            .command()
            .creation_time?;
        result.push(InvocationForeground {
            root_pid,
            root_birth,
            pid: selected.process_group_id,
            birth,
        });
    }
    // Bound complete-query work, and reject changes/partial reads rather than
    // selecting from an incomplete tree. Handles retain all queried lifetimes.
    let fresh_topology = parents()?;
    for (expected, _) in &captured {
        let (current, _handle) = entry(expected.pid, &fresh_topology, sid)?;
        if &current != expected {
            return None;
        }
    }
    Some(result)
}

#[repr(C)]
#[derive(Default)]
struct BootEnvironment {
    identifier: [u8; 16],
    firmware_type: u32,
    boot_flags: u64,
}

fn boot_id() -> Option<String> {
    use windows_sys::Wdk::System::SystemInformation::NtQuerySystemInformation;
    let mut information = BootEnvironment::default();
    let mut returned = 0;
    // SystemBootEnvironmentInformation (90) returns the kernel boot GUID. No
    // wall-clock subtraction, machine UUID or random per-server fallback.
    // SAFETY: exact native structure size and valid writable output pointers.
    let status = unsafe {
        NtQuerySystemInformation(
            90,
            (&mut information as *mut BootEnvironment).cast(),
            size_of::<BootEnvironment>() as u32,
            &mut returned,
        )
    };
    if status < 0
        || returned as usize != size_of::<BootEnvironment>()
        || information.identifier == [0; 16]
    {
        return None;
    }
    Some(
        information
            .identifier
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

#[derive(Debug, PartialEq, Eq)]
struct ImageIdentity {
    volume: u32,
    index: u64,
}

fn image_identity(path: &Path) -> Option<(ImageIdentity, OwnedHandle)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, OPEN_EXISTING,
    };
    let mut path: Vec<u16> = path.as_os_str().encode_wide().collect();
    if path.contains(&0) {
        return None;
    }
    path.push(0);
    // Keep the executable file from being rewritten/renamed through this proof.
    // Compare file identity, not Windows spelling/case, argv0 or a basename.
    let raw = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ,
            null_mut(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return None;
    }
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    if unsafe { GetFileInformationByHandle(raw, information.as_mut_ptr()) } == 0 {
        return None;
    }
    let information = unsafe { information.assume_init() };
    Some((
        ImageIdentity {
            volume: information.dwVolumeSerialNumber,
            index: (u64::from(information.nFileIndexHigh) << 32)
                | u64::from(information.nFileIndexLow),
        },
        handle,
    ))
}

struct LauncherEvidence {
    child: InvocationProcess,
    launcher: InvocationProcess,
    child_sid: String,
    launcher_sid: String,
    // Host, launcher, declaration program, actual child; kernel file identities.
    images: [ImageIdentity; 4],
    declaration: crate::extension_cli::ForegroundDeclaration,
}

fn launcher_evidence_matches(
    peer: &InvocationPeer,
    proof: &LauncherEvidence,
    fresh: &crate::extension_cli::ForegroundDeclaration,
) -> bool {
    let [child, launcher, ..] = peer.ancestry.as_slice() else {
        return false;
    };
    let InvocationAccount::Windows { sid } = &peer.account else {
        return false;
    };
    proof.child == *child
        && proof.launcher == *launcher
        && child.parent_pid == launcher.pid
        && child.birth >= launcher.birth
        && &proof.child_sid == sid
        && &proof.launcher_sid == sid
        && proof.images[0] == proof.images[1]
        && proof.images[2] == proof.images[3]
        && child.process.argv.as_ref() == Some(&proof.declaration.argv)
        && &proof.declaration == fresh
}

fn transparent_launcher(peer: &InvocationPeer) -> bool {
    transparent_launcher_proof(peer).is_some()
}

fn transparent_launcher_proof(peer: &InvocationPeer) -> Option<()> {
    let [child, launcher, ..] = peer.ancestry.as_slice() else {
        return None;
    };
    if child.parent_pid != launcher.pid || child.birth < launcher.birth {
        return None;
    }
    let child_handle = live_process(child.pid)?;
    let launcher_handle = live_process(launcher.pid)?;
    if process_creation_time(child_handle.0) != Some(child.birth)
        || process_creation_time(launcher_handle.0) != Some(launcher.birth)
    {
        return None;
    }
    let invocation = launcher.process.argv.as_ref()?;
    let declaration = crate::extension_cli::foreground_declaration(invocation)?;
    if child.process.argv.as_ref()? != &declaration.argv {
        return None;
    }
    let (host_image, _host_file) = image_identity(&std::env::current_exe().ok()?)?;
    let (launcher_image, _launcher_file) =
        image_identity(Path::new(&process_executable_path(launcher_handle.0)?))?;
    if launcher_image != host_image {
        return None;
    }
    let (declared_image, _declared_file) = image_identity(Path::new(declaration.argv.first()?))?;
    let (child_image, _child_file) =
        image_identity(Path::new(&process_executable_path(child_handle.0)?))?;
    if child_image != declared_image {
        return None;
    }
    let topology = parents()?;
    if topology.get(&child.pid) != Some(&launcher.pid)
        || topology.get(&launcher.pid) != Some(&launcher.parent_pid)
    {
        return None;
    }
    let InvocationAccount::Windows { sid } = &peer.account else {
        return None;
    };
    let (fresh_child, _) = entry(child.pid, &topology, sid)?;
    let (fresh_launcher, _) = entry(launcher.pid, &topology, sid)?;
    let proof = LauncherEvidence {
        child: fresh_child,
        launcher: fresh_launcher,
        child_sid: account_sid_text(child_handle.0).ok()?,
        launcher_sid: account_sid_text(launcher_handle.0).ok()?,
        images: [host_image, launcher_image, declared_image, child_image],
        declaration,
    };
    // Fresh full parse + digest + enabled/unambiguous registration, not a static
    // prefix or a process name. No environment marker is used as proof.
    let fresh = crate::extension_cli::foreground_declaration(invocation)?;
    launcher_evidence_matches(peer, &proof, &fresh).then_some(())
}

unsafe extern "system" fn foreground_control(kind: u32) -> i32 {
    // Do not use SetConsoleCtrlHandler(NULL, TRUE): its ignore flag would be
    // inherited by the child. This parent's scoped handler lets the ordinary
    // child handle Ctrl-C/Break while the parent remains to reap its exit.
    i32::from(matches!(kind, CTRL_C_EVENT | CTRL_BREAK_EVENT))
}

struct ForegroundControl;
impl Drop for ForegroundControl {
    fn drop(&mut self) {
        // SAFETY: removes only the handler this invocation installed.
        unsafe {
            SetConsoleCtrlHandler(Some(foreground_control), 0);
        }
    }
}

pub(crate) fn foreground_cli_command(mut command: std::process::Command) -> io::Result<i32> {
    if !Path::new(command.get_program())
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Windows extension CLI programs must be explicit .exe executables; batch/shell execution is not supported"));
    }
    // SAFETY: static function pointer; guard unregisters on every return path.
    if unsafe { SetConsoleCtrlHandler(Some(foreground_control), 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _control = ForegroundControl;
    let mut child = command.spawn()?;
    loop {
        match child.wait() {
            Ok(status) => {
                return status.code().ok_or_else(|| {
                    io::Error::other("extension CLI exited without a Windows exit code")
                })
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                // Never abandon an executing child after a launcher wait error.
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
    }
}
