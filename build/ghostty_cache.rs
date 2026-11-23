//! Content-addressed native build cache. No Cargo, Zig, or checkout-specific paths in its identity.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Bump when the installed layout, manifest, or build semantics change.
pub const SCHEMA: u32 = 1;

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
    for (name, hash) in inventory(vendored, true)? {
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
    build: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<Entry> {
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
    let path = root.join(&key);
    if verify(&path, inputs, required).is_ok() {
        return Ok(Entry { path, reused: true });
    }
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
    Ok(Entry {
        path,
        reused: false,
    })
}
