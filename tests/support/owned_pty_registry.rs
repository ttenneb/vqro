//! Test-owned incarnation cleanup after forced exits (which cannot run drops).
//! Never delete an unproved incarnation or records outside this fixture's scope.
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub(super) fn birth(pid: u32) -> io::Result<Option<u64>> {
    #[cfg(target_os = "linux")]
    {
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
            Ok(text) => text
                .rsplit_once(") ")
                .and_then(|(_, fields)| fields.split_whitespace().nth(19))
                .and_then(|value| value.parse().ok())
                .map(Some)
                .ok_or_else(|| io::Error::other("invalid kernel birth")),
        }
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: fixed writable BSD-info buffer, read-only kernel query.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info) as libc::c_int;
        let count = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        if count == size && info.pbi_pid == pid {
            info.pbi_start_tvsec
                .checked_mul(1_000_000)
                .and_then(|value| value.checked_add(info.pbi_start_tvusec))
                .map(Some)
                .ok_or_else(|| io::Error::other("birth overflow"))
        } else {
            let error = io::Error::last_os_error();
            if count == 0 && error.raw_os_error() == Some(libc::ESRCH) {
                Ok(None)
            } else {
                Err(error)
            }
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        Err(io::Error::other("no native incarnation proof"))
    }
}

fn fixed_root() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        Some(
            PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() }))
                .join("vqro/pty-owners-v1"),
        )
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStringExt;
        let mut buffer = vec![0u8; 4096];
        // SAFETY: writable bounded buffer; confstr returns required byte count.
        let size = unsafe {
            libc::confstr(
                libc::_CS_DARWIN_USER_TEMP_DIR,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        if size < 2 || size > buffer.len() || buffer[size - 1] != 0 {
            return None;
        }
        buffer.truncate(size - 1);
        Some(PathBuf::from(std::ffi::OsString::from_vec(buffer)).join("vqro/pty-owners-v1"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

fn current_boot() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
    }
    #[cfg(target_os = "macos")]
    {
        let mut bytes = [0u8; 37];
        let mut size = bytes.len();
        // SAFETY: bounded writable buffer and read-only, NUL-terminated query.
        let result = unsafe {
            libc::sysctlbyname(
                c"kern.bootsessionuuid".as_ptr(),
                bytes.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 || size != 37 || bytes[36] != 0 {
            return None;
        }
        std::str::from_utf8(&bytes[..36])
            .ok()
            .map(str::to_ascii_lowercase)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

pub(super) fn cleanup(roots: &HashSet<PathBuf>, children: &HashMap<u32, Option<u64>>) {
    let (Some(root), Some(boot)) = (fixed_root(), current_boot()) else {
        return;
    };
    if let Err(error) = cleanup_at(&root, &boot, roots, children, birth) {
        eprintln!("test-owned PTY record cleanup unavailable: {error}");
    }
}

fn cleanup_at(
    root: &Path,
    current_boot: &str,
    roots: &HashSet<PathBuf>,
    children: &HashMap<u32, Option<u64>>,
    observe: impl Fn(u32) -> io::Result<Option<u64>>,
) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        result => result?,
    };
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::other("untrusted registry directory"));
    }
    for (index, entry) in fs::read_dir(root)?.enumerate() {
        if index >= 4096 {
            return Err(io::Error::other("test cleanup enumeration bound"));
        }
        let path = entry?.path();
        if path.extension().is_none_or(|value| value != "json") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file()
            || metadata.uid() != uid
            || metadata.mode() & 0o077 != 0
            || metadata.len() > 131072
        {
            continue;
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        let opened = file.metadata()?;
        if opened.ino() != metadata.ino() || opened.dev() != metadata.dev() {
            continue;
        }
        let mut bytes = Vec::new();
        file.take(131073).read_to_end(&mut bytes)?;
        if bytes.len() > 131072 {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let key = &value["ownership"]["server"];
        let (Some(pid), Some(expected), Some(boot), Some(endpoint)) = (
            key["pid"].as_u64().and_then(|pid| u32::try_from(pid).ok()),
            key["birth"].as_u64(),
            key["boot_id"].as_str(),
            value["endpoint"].as_str(),
        ) else {
            continue;
        };
        if boot != current_boot {
            continue;
        }
        if path
            .file_name()
            .is_none_or(|name| name != format!("{boot}-{pid}-{expected}.json").as_str())
        {
            continue;
        }
        let own_child = children
            .get(&pid)
            .is_some_and(|birth| *birth == Some(expected));
        let own_runtime = Path::new(endpoint)
            .parent()
            .is_some_and(|parent| roots.contains(parent));
        if !own_child && !own_runtime {
            continue;
        }
        // A live same-birth process, permission failure or malformed kernel
        // evidence never authorizes deletion. Recheck immediately before unlink.
        if !matches!(observe(pid), Ok(None))
            && !matches!(observe(pid), Ok(Some(current)) if current != expected)
        {
            continue;
        }
        let current = fs::symlink_metadata(&path)?;
        if current.ino() != metadata.ino()
            || current.dev() != metadata.dev()
            || fs::read(&path)? != bytes
        {
            return Err(io::Error::other("own record changed during cleanup"));
        }
        if matches!(observe(pid), Ok(None))
            || matches!(observe(pid), Ok(Some(current)) if current != expected)
        {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn forced_exit_cleanup_preserves_live_unknown_and_other_incarnations() {
        let root = std::env::temp_dir().join(format!("owned-pty-cleanup-{}", std::process::id()));
        fs::create_dir(&root).expect("private root");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("private");
        for pid in 10..14 {
            let path = root.join(format!("boot-{pid}-99.json"));
            fs::write(&path, serde_json::json!({"endpoint":"/own/runtime/api.sock", "ownership":{"server":{"boot_id":"boot", "pid":pid, "birth":99}}}).to_string()).expect("fixture");
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("private file");
        }
        let children = HashMap::from([(10, Some(99)), (11, Some(99)), (12, Some(99))]);
        cleanup_at(&root, "boot", &HashSet::new(), &children, |pid| match pid {
            10 => Ok(None),
            11 => Ok(Some(99)),
            _ => Err(io::Error::other("unprovable")),
        })
        .expect("own cleanup");
        assert!(!root.join("boot-10-99.json").exists());
        for pid in 11..14 {
            assert!(root.join(format!("boot-{pid}-99.json")).exists());
        }
        fs::remove_dir_all(root).expect("own fixtures");
    }
}
