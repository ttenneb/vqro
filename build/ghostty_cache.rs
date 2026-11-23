//! Content-addressed native build cache. No Cargo, Zig, or checkout-specific paths in its identity.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Bump when the installed layout, manifest, or build semantics change.
pub const SCHEMA: u32 = 2;
pub const DEFAULT_KEEP: usize = 6;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inputs {
    pub schema: u32,
    pub source: String,
    pub target: String,
    pub optimize: String,
    pub zig_version: String,
    /// Main-approved reuse boundary, host/resolved Zig install and consumed native caller settings.
    pub native: BTreeMap<String, Option<String>>,
    /// The complete Zig argv, with output/cache paths replaced by stable placeholders.
    pub flags: Vec<String>,
}

impl Inputs {
    pub fn key(&self) -> String {
        // JSON provides unambiguous field/argument boundaries, in declaration order.
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(self).expect("serialize cache inputs"))
        )
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    inputs: Inputs,
    artifacts: BTreeMap<String, String>,
}

pub struct Entry {
    pub path: PathBuf,
    pub reused: bool,
    pub cached: bool,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn file_digest(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Enumerate exact installed files, including internal file symlinks, but never follow directory symlinks.
/// This also prevents a damaged entry from hashing/linking files outside the entry.
fn files(
    root: &Path,
    dir: &Path,
    source: bool,
    result: &mut BTreeMap<String, String>,
) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| invalid("file outside cache entry"))?;
        if source
            && ["zig-out", ".zig-cache", "zig-cache", "zig-pkg"]
                .iter()
                .any(|p| relative.starts_with(p))
        {
            continue;
        }
        if !source && relative == Path::new("manifest.json") {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            files(root, &path, source, result)?;
        } else {
            if kind.is_symlink() {
                let link = fs::read_link(&path)?;
                if !source {
                    // Canonical containment before rename is insufficient: ../../install/... can
                    // leave the entry and return only because its staging basename is `install`.
                    let mut depth = relative.parent().map_or(0, |p| p.components().count());
                    for component in link.components() {
                        match component {
                            std::path::Component::Normal(_) => depth += 1,
                            std::path::Component::CurDir => (),
                            std::path::Component::ParentDir if depth > 0 => depth -= 1,
                            _ => {
                                return Err(invalid(
                                    "nonrelocatable artifact symlink escapes entry",
                                ));
                            }
                        }
                    }
                }
                let resolved = path.canonicalize()?;
                if !resolved.starts_with(root) || !resolved.is_file() {
                    return Err(invalid("artifact symlink escapes entry or is not a file"));
                }
            } else if !kind.is_file() {
                return Err(invalid("artifact is not a file"));
            }
            let name = relative
                .components()
                .map(|part| {
                    part.as_os_str()
                        .to_str()
                        .ok_or_else(|| invalid("non-UTF-8 artifact path"))
                })
                .collect::<io::Result<Vec<_>>>()?
                .join("/");
            result.insert(name, file_digest(&path)?);
        }
    }
    Ok(())
}

fn inventory(root: &Path, source: bool) -> io::Result<BTreeMap<String, String>> {
    let root = root.canonicalize()?;
    let mut result = BTreeMap::new();
    files(&root, &root, source, &mut result)?;
    Ok(result)
}

/// Hash the same sorted path/content manifest as scripts/check_vendor.py. Dirty/additional vendored files
/// change the key too, even when the checked-in .sha256 manifest has not yet been regenerated.
pub fn source_digest(vendored: &Path) -> io::Result<String> {
    let mut digest = Sha256::new();
    let mut files = inventory(vendored, true)?.into_iter().collect::<Vec<_>>();
    // pathlib sorts path components, not the whole string (a/file precedes a.zig).
    files.sort_by(|(a, _), (b, _)| a.split('/').cmp(b.split('/')));
    for (name, hash) in files {
        digest.update(format!("{hash}  {name}\n").as_bytes());
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn require(artifacts: &BTreeMap<String, String>, required: &[&str]) -> io::Result<()> {
    if artifacts.is_empty()
        || required.is_empty()
        || required.iter().any(|p| !artifacts.contains_key(*p))
    {
        return Err(invalid(
            "native build did not produce the required artifacts",
        ));
    }
    Ok(())
}

/// Reject incomplete entries, changed inputs, missing/extra files and corrupt artifacts before linking.
pub fn verify(path: &Path, inputs: &Inputs, required: &[&str]) -> io::Result<()> {
    let entry_kind = fs::symlink_metadata(path)?.file_type();
    let manifest_path = path.join("manifest.json");
    let manifest_kind = fs::symlink_metadata(&manifest_path)?.file_type();
    if !entry_kind.is_dir() || !manifest_kind.is_file() {
        return Err(invalid(
            "cache entry or manifest is a symlink or wrong file type",
        ));
    }
    let manifest: Manifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
    if manifest.inputs != *inputs {
        return Err(invalid("cache inputs mismatch"));
    }
    require(&manifest.artifacts, required)?;
    if inventory(path, false)? != manifest.artifacts {
        return Err(invalid("cache artifact hashes mismatch"));
    }
    Ok(())
}

/// The lock file is never renamed/unlinked: every process locks the same inode. std File::lock uses flock
/// on Unix and LockFileEx on Windows, and is released even when a builder exits unsuccessfully.
/// Verification and publication happen under that lock. A build is private until the complete install tree
/// and manifest can be atomically renamed into place; the temporary Zig work tree is never cached.
#[cfg(test)]
pub fn ensure(
    root: &Path,
    inputs: &Inputs,
    required: &[&str],
    keep: usize,
    build: impl FnOnce(&Path, &Path) -> io::Result<()>,
    use_entry: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<Entry> {
    // Fake-artifact tests have an immutable literal source identity.
    ensure_checked(
        root,
        inputs,
        required,
        keep,
        true, // Fake-artifact maintenance tests opt in explicitly on disposable roots.
        || Ok(inputs.source.clone()),
        build,
        use_entry,
    )
}

pub fn ensure_checked(
    root: &Path,
    inputs: &Inputs,
    required: &[&str],
    keep: usize,
    gc: bool,
    read_source: impl Fn() -> io::Result<String>,
    build: impl FnOnce(&Path, &Path) -> io::Result<()>,
    use_entry: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<Entry> {
    if keep == 0 {
        return Err(invalid("cache retention must be positive"));
    }
    fs::create_dir_all(root)?;
    // Zig runs from the vendored directory, so caller-relative roots must become absolute first.
    let root = root.canonicalize()?;
    let key = inputs.key();
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(format!("{key}.lock")))?;
    lock.lock()?;
    if gc {
        cleanup_stages(&root, &key, std::time::SystemTime::now(), process_start)?;
    }
    // The pre-lock digest only routes us to a candidate lock. Accept its identity only after hashing
    // under that lock, then recheck immediately before accepting an existing entry (including waits).
    let locked_source = read_source()?;
    let mut stable = locked_source == inputs.source;
    let path = root.join(&key);
    let valid = verify(&path, inputs, required).is_ok();
    stable &= read_source()? == locked_source;
    if valid && stable {
        use_entry(&path)?;
        OpenOptions::new()
            .write(true)
            .open(path.join("manifest.json"))?
            .set_modified(std::time::SystemTime::now())?;
        if gc {
            prune(&root, keep)?;
        }
        return Ok(Entry {
            path,
            reused: true,
            cached: true,
        });
    }
    if !valid {
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() && !meta.is_symlink() => fs::remove_dir_all(&path)?,
            Ok(_) => fs::remove_file(&path)?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => (),
            Err(err) => return Err(err),
        }
    }
    let before_build = read_source()?;
    stable &= before_build == locked_source;
    {
        let stage = tempfile::Builder::new()
            .prefix(&format!(".{key}-"))
            .tempdir_in(&root)?;
        let owner = StageOwner {
            pid: std::process::id(),
            start: process_start(std::process::id()).ok().flatten(),
        };
        fs::write(stage.path().join("owner.json"), serde_json::to_vec(&owner)?)?;
        let prefix = stage.path().join("install");
        let work = stage.path().join("zig-cache");
        fs::create_dir(&prefix)?;
        fs::create_dir(&work)?;
        build(&prefix, &work)?;
        let artifacts = inventory(&prefix, false)?;
        require(&artifacts, required)?;
        let manifest = Manifest {
            inputs: inputs.clone(),
            artifacts,
        };
        fs::write(
            prefix.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        verify(&prefix, inputs, required)?;
        stable &= read_source()? == before_build;
        if !stable {
            // Keep this result local, as before shared caching: never publish it under an observed
            // stale source key. The caller copies/verifies it before this temporary tree is dropped.
            use_entry(&prefix)?;
            return Ok(Entry {
                path,
                reused: false,
                cached: false,
            });
        }
        fs::rename(&prefix, &path)?;
    }
    // Consumers copy their link inputs while this lock is still held. The shared entry may be evicted
    // immediately after return; a Cargo linker must never depend on that shared path's lifetime.
    use_entry(&path)?;
    OpenOptions::new()
        .write(true)
        .open(path.join("manifest.json"))?
        .set_modified(std::time::SystemTime::now())?;
    if gc {
        prune(&root, keep)?;
    }
    Ok(Entry {
        path,
        reused: false,
        cached: true,
    })
}

#[derive(Serialize, Deserialize)]
struct StageOwner {
    pid: u32,
    // Unknown creation time is recorded explicitly and never treated as a dead owner.
    start: Option<String>,
}

fn process_start(pid: u32) -> io::Result<Option<String>> {
    #[cfg(target_os = "linux")]
    {
        linux_process_start(Path::new("/proc"), pid)
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        unix_process_start(Path::new("ps"), pid)
    }
    #[cfg(windows)]
    {
        windows_process_start(Path::new("powershell.exe"), pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        Err(invalid("process identity unavailable on this host"))
    }
}

#[cfg(any(target_os = "linux", all(unix, test)))]
pub fn linux_process_start(proc_root: &Path, pid: u32) -> io::Result<Option<String>> {
    if pid == 0 {
        return Err(invalid("invalid owner PID"));
    }
    let directory = proc_root.join(pid.to_string());
    let stat = match fs::read_to_string(directory.join("stat")) {
        Ok(stat) => stat,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            // A missing stat file alone could be a broken/inaccessible proc view. Require a working
            // self query and a positively absent process directory; all other outcomes stay unknown.
            fs::read_to_string(proc_root.join("self/stat"))?;
            return match fs::symlink_metadata(directory) {
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
                _ => Err(invalid(
                    "process directory exists but identity is unavailable",
                )),
            };
        }
        Err(err) => return Err(err),
    };
    let ticks = stat
        .rsplit_once(')')
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .filter(|ticks| !ticks.is_empty() && ticks.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| invalid("process start time unavailable"))?;
    let boot = fs::read_to_string(proc_root.join("sys/kernel/random/boot_id"))?;
    if boot.trim().is_empty() {
        return Err(invalid("boot identity unavailable"));
    }
    Ok(Some(format!("{}:{ticks}", boot.trim())))
}

#[cfg(all(unix, any(not(target_os = "linux"), test)))]
pub fn unix_process_start(ps: &Path, pid: u32) -> io::Result<Option<String>> {
    if pid == 0 {
        return Err(invalid("invalid owner PID"));
    }
    let result = std::process::Command::new(ps)
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()?;
    let start = std::str::from_utf8(&result.stdout)
        .map_err(|_| invalid("invalid ps response"))?
        .trim();
    if result.status.success()
        && result.stderr.is_empty()
        && !start.is_empty()
        && start.lines().count() == 1
    {
        return Ok(Some(start.to_owned()));
    }
    // ps exit 1 plus empty output is not positive ESRCH evidence. Retain rather than guess absent.
    Err(invalid("process identity query failed"))
}

#[cfg(any(windows, all(unix, test)))]
pub fn windows_process_script(pid: u32) -> String {
    // Get-Process emits this specific ID/category for a missing PID. ProcessCommandException alone
    // is too broad: PowerShell uses it for other process errors too (see research note/source link).
    format!(
        r#"
try {{ $p = Get-Process -Id {pid} -ErrorAction Stop }}
catch {{
    if (($_.FullyQualifiedErrorId -split ',')[0] -eq 'NoProcessFoundForGivenId' -and
        $_.CategoryInfo.Category -eq [System.Management.Automation.ErrorCategory]::ObjectNotFound) {{
        [Console]::Out.WriteLine('absent'); exit 0
    }}
    exit 1
}}
if ($null -eq $p) {{ exit 1 }}
try {{
    $ticks = $p.StartTime.ToUniversalTime().Ticks
    if ($ticks -le 0) {{ exit 1 }}
    [Console]::Out.WriteLine($ticks)
}} catch {{ exit 1 }}
"#
    )
}

#[cfg(any(windows, all(unix, test)))]
pub fn windows_process_result(result: std::process::Output) -> io::Result<Option<String>> {
    let start = std::str::from_utf8(&result.stdout)
        .map_err(|_| invalid("invalid process response"))?
        .trim();
    if result.status.success() && result.stderr.is_empty() {
        if start == "absent" {
            return Ok(None);
        }
        if start.parse::<u64>().is_ok_and(|ticks| ticks > 0) {
            return Ok(Some(start.to_owned()));
        }
    }
    Err(invalid("process identity query failed"))
}

#[cfg(any(windows, all(unix, test)))]
pub fn windows_process_start(powershell: &Path, pid: u32) -> io::Result<Option<String>> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(invalid("invalid owner PID"));
    }
    let result = std::process::Command::new(powershell)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &windows_process_script(pid),
        ])
        .output()?;
    windows_process_result(result)
}

fn newest_modified(path: &Path) -> io::Result<std::time::SystemTime> {
    let metadata = fs::symlink_metadata(path)?;
    let mut newest = metadata.modified()?;
    if metadata.file_type().is_dir() {
        for child in fs::read_dir(path)? {
            newest = newest.max(newest_modified(&child?.path())?);
        }
    }
    Ok(newest)
}

/// Called only under this key's lock. A dead builder does NOT prove its Zig descendants stopped:
/// retain the stage unless the owner identity is gone AND the entire tree has been quiet for an hour.
/// Missing/unknown owner records, probe failures, and unreadable/newer timestamps are all retained.
pub fn cleanup_stages(
    root: &Path,
    key: &str,
    now: std::time::SystemTime,
    process_start: impl Fn(u32) -> io::Result<Option<String>>,
) -> io::Result<()> {
    let prefix = format!(".{key}-");
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with(&prefix))
            || !entry.file_type()?.is_dir()
        {
            continue;
        }
        let path = entry.path();
        let Some(owner) = fs::read(path.join("owner.json"))
            .ok()
            .and_then(|v| serde_json::from_slice::<StageOwner>(&v).ok())
        else {
            continue;
        };
        let Some(start) = owner.start else { continue };
        if owner.pid == 0 || start.trim().is_empty() {
            continue;
        }
        let gone = match process_start(owner.pid) {
            Ok(None) => true,
            Ok(Some(current)) => current != start,
            Err(_) => false,
        };
        if gone
            && newest_modified(&path)
                .ok()
                .and_then(|mtime| now.duration_since(mtime).ok())
                .is_some_and(|age| age >= std::time::Duration::from_secs(3600))
        {
            fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

/// Copy just the link inputs into private output, checking bytes against the manifest while the caller
/// holds the shared entry lock. Identical snapshots retain their mtime to avoid repeated Cargo rebuilds.
pub fn materialize(entry: &Path, required: &[&str], out: &Path) -> io::Result<()> {
    let manifest: Manifest = serde_json::from_slice(&fs::read(entry.join("manifest.json"))?)?;
    require(&manifest.artifacts, required)?;
    for name in required {
        let destination = out.join(name);
        let expected = &manifest.artifacts[*name];
        if fs::symlink_metadata(&destination).is_ok_and(|m| m.file_type().is_file())
            && file_digest(&destination)? == *expected
        {
            continue;
        }
        let parent = destination
            .parent()
            .ok_or_else(|| invalid("snapshot parent missing"))?;
        fs::create_dir_all(parent)?;
        let copy = tempfile::NamedTempFile::new_in(parent)?;
        fs::copy(entry.join(name), copy.path())?;
        if file_digest(copy.path())? != *expected {
            return Err(invalid("snapshot hash mismatch"));
        }
        copy.persist(&destination).map_err(|err| err.error)?;
    }
    Ok(())
}

/// Busy keys may temporarily exceed the limit. Never wait while holding a different key's lock, and
/// recheck use time under the candidate lock so a recent use after our directory scan is not evicted.
fn prune(root: &Path, keep: usize) -> io::Result<()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(key) = name.to_str() else { continue };
        if key.len() != 64
            || !key.bytes().all(|c| c.is_ascii_hexdigit())
            || !entry.file_type()?.is_dir()
        {
            continue;
        }
        match fs::symlink_metadata(entry.path().join("manifest.json")) {
            Ok(meta) if meta.file_type().is_file() => {
                entries.push((meta.modified()?, key.to_owned()))
            }
            Ok(_) => (),
            Err(err) if err.kind() == io::ErrorKind::NotFound => (),
            Err(err) => return Err(err),
        }
    }
    entries.sort_by(|a, b| b.cmp(a));
    for (used, key) in entries.into_iter().skip(keep) {
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join(format!("{key}.lock")))?;
        match lock.try_lock() {
            Ok(()) => (),
            Err(std::fs::TryLockError::WouldBlock) => continue,
            Err(std::fs::TryLockError::Error(err)) => return Err(err),
        }
        let path = root.join(key);
        match fs::symlink_metadata(path.join("manifest.json")) {
            Ok(meta) if meta.file_type().is_file() && meta.modified()? == used => {
                fs::remove_dir_all(path)?
            }
            Ok(_) => (),
            Err(err) if err.kind() == io::ErrorKind::NotFound => (),
            Err(err) => return Err(err),
        }
    }
    Ok(())
}
