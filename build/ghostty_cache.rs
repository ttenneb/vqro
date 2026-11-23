//! Content-addressed native build cache. No Cargo, Zig, or checkout-specific paths in its identity.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Bump when the installed layout, manifest, or build semantics change.
pub const SCHEMA: u32 = 1;
pub const DEFAULT_KEEP: usize = 6;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inputs {
    pub schema: u32,
    pub source: String,
    pub target: String,
    pub optimize: String,
    pub zig_version: String,
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
pub fn ensure(
    root: &Path,
    inputs: &Inputs,
    required: &[&str],
    keep: usize,
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
    // Only a live builder holding this key's lock may create a stage. Abruptly exited builders cannot
    // run TempDir's destructor, so reclaim their directories before either rebuilding or reusing.
    let stage_prefix = format!(".{key}-");
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(&stage_prefix))
            && entry.file_type()?.is_dir()
        {
            // file_type does not follow symlinks: never traverse a similarly named symlink target.
            fs::remove_dir_all(entry.path())?;
        }
    }
    let path = root.join(&key);
    let reused = verify(&path, inputs, required).is_ok();
    if !reused {
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() && !meta.is_symlink() => fs::remove_dir_all(&path)?,
            Ok(_) => fs::remove_file(&path)?,
            Err(err) if err.kind() == io::ErrorKind::NotFound => (),
            Err(err) => return Err(err),
        }
        let stage = tempfile::Builder::new()
            .prefix(&format!(".{key}-"))
            .tempdir_in(&root)?;
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
        fs::rename(&prefix, &path)?;
    }
    // Consumers copy their link inputs while this lock is still held. The shared entry may be evicted
    // immediately after return; a Cargo linker must never depend on that shared path's lifetime.
    use_entry(&path)?;
    OpenOptions::new()
        .write(true)
        .open(path.join("manifest.json"))?
        .set_modified(std::time::SystemTime::now())?;
    prune(&root, keep)?;
    Ok(Entry { path, reused })
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
