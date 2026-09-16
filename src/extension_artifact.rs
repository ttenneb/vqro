//! Fetching, verification, and durable retention for curated extension sources.
//!
//! Network and archive work happens in staging directories. Callers commit a
//! verified stage only after reacquiring the extension-state mutation lock.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::extensions::{CuratedExtension, VerifiedExtensionArtifact};

const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_STAGING_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 10_000;
const GIT_TIMEOUT: Duration = Duration::from_secs(25);
static NEXT_STAGE_ID: AtomicU64 = AtomicU64::new(1);
static ACTIVE_STAGES: AtomicUsize = AtomicUsize::new(0);
const MAX_ACTIVE_STAGES: usize = 2;

pub(crate) struct ExtensionArtifactStageSlot;

impl ExtensionArtifactStageSlot {
    pub(crate) fn acquire() -> Option<Self> {
        ACTIVE_STAGES
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_ACTIVE_STAGES).then_some(active + 1)
            })
            .ok()
            .map(|_| Self)
    }
}

impl Drop for ExtensionArtifactStageSlot {
    fn drop(&mut self) {
        ACTIVE_STAGES.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ExtensionArtifactAction {
    Install,
    Update { version: String },
}

#[derive(Debug)]
pub(crate) struct ExtensionArtifactStageResult {
    pub(crate) id: String,
    pub(crate) repository: String,
    pub(crate) action: ExtensionArtifactAction,
    pub(crate) expected_state: crate::extensions::MarketplaceInstallState,
    pub(crate) staged: Result<StagedExtensionArtifact, String>,
    pub(crate) activation_asset: Result<Option<Vec<u8>>, String>,
    pub(crate) respond_to: std::sync::mpsc::Sender<String>,
}

#[derive(Debug)]
pub(crate) struct StagedExtensionArtifact {
    root: PathBuf,
    extracted: PathBuf,
    descriptor: VerifiedExtensionArtifact,
}

impl StagedExtensionArtifact {
    pub(crate) fn descriptor(&self) -> &VerifiedExtensionArtifact {
        &self.descriptor
    }

    pub(crate) fn read_file(&self, relative_path: &str) -> Result<Vec<u8>, String> {
        let relative = Path::new(relative_path);
        if !safe_archive_path(relative) {
            return Err("requested staged extension path is unsafe".into());
        }
        let path = self.extracted.join(relative);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("inspect staged extension asset: {error}"))?;
        if !metadata.file_type().is_file() {
            return Err("staged extension asset is not a regular file".into());
        }
        fs::read(path).map_err(|error| format!("read staged extension asset: {error}"))
    }
}

impl Drop for StagedExtensionArtifact {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub(crate) fn stage(extension: &CuratedExtension) -> Result<StagedExtensionArtifact, String> {
    let repository = github_repository(&extension.repository)?;
    let source_url = format!("https://github.com/{repository}.git");
    stage_from_source(extension, &source_url, false)
}

fn stage_from_source(
    extension: &CuratedExtension,
    source_url: &str,
    allow_file_protocol: bool,
) -> Result<StagedExtensionArtifact, String> {
    let commit = extension
        .source_commit
        .as_deref()
        .ok_or_else(|| "extension has no pinned source commit".to_string())?;
    let expected_source = extension
        .source_sha256
        .as_deref()
        .ok_or_else(|| "extension has no pinned source digest".to_string())?;
    let expected_manifest = extension
        .manifest_sha256
        .as_deref()
        .ok_or_else(|| "extension has no pinned manifest digest".to_string())?;

    let root = staging_root()?.join(format!(
        "{}-{}-{}",
        std::process::id(),
        NEXT_STAGE_ID.fetch_add(1, Ordering::Relaxed),
        safe_component(&extension.id)
    ));
    let repository_dir = root.join("repository");
    let extracted = root.join("artifact");
    fs::create_dir_all(&repository_dir)
        .map_err(|error| format!("create extension staging directory: {error}"))?;
    set_private_directory_permissions(&root)?;

    let result = (|| {
        run_git(&repository_dir, ["init", "--quiet"], allow_file_protocol)?;
        run_git(
            &repository_dir,
            ["remote", "add", "origin", source_url],
            allow_file_protocol,
        )?;
        run_git(
            &repository_dir,
            ["fetch", "--quiet", "--depth=1", "origin", commit],
            allow_file_protocol,
        )?;
        let resolved = git_output(
            &repository_dir,
            ["rev-parse", "FETCH_HEAD"],
            allow_file_protocol,
        )?;
        if !resolved.eq_ignore_ascii_case(commit) {
            return Err(format!(
                "pinned commit mismatch: expected {commit}, fetched {resolved}"
            ));
        }

        let archive_path = root.join("source.tar");
        write_bounded_git_archive(&repository_dir, allow_file_protocol, &archive_path)?;
        let metadata = fs::metadata(&archive_path)
            .map_err(|error| format!("inspect extension archive: {error}"))?;
        if metadata.len() > MAX_ARCHIVE_BYTES {
            return Err("pinned extension archive exceeds 8 MiB".into());
        }
        let source_digest = digest_file(&archive_path)?;
        if !source_digest.eq_ignore_ascii_case(expected_source) {
            return Err(format!(
                "source digest mismatch: expected {expected_source}, got {source_digest}"
            ));
        }

        fs::create_dir_all(&extracted)
            .map_err(|error| format!("create extracted extension directory: {error}"))?;
        extract_safe_archive(&archive_path, &extracted)?;
        let manifest_path = extracted.join(&extension.manifest_path);
        let manifest_metadata = fs::symlink_metadata(&manifest_path)
            .map_err(|error| format!("inspect pinned extension manifest: {error}"))?;
        if !manifest_metadata.file_type().is_file() {
            return Err("pinned extension manifest is not a regular file".into());
        }
        let manifest_digest = digest_file(&manifest_path)?;
        if !manifest_digest.eq_ignore_ascii_case(expected_manifest) {
            return Err(format!(
                "manifest digest mismatch: expected {expected_manifest}, got {manifest_digest}"
            ));
        }
        verify_manifest_identity(&manifest_path, extension)?;

        Ok(StagedExtensionArtifact {
            root: root.clone(),
            extracted: extracted.clone(),
            descriptor: VerifiedExtensionArtifact {
                extension_id: extension.id.clone(),
                version: extension.version.clone(),
                commit: commit.to_ascii_lowercase(),
                source_sha256: source_digest,
                manifest_sha256: manifest_digest,
                manifest_path: extension.manifest_path.clone(),
                path: String::new(),
            },
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(root);
    }
    result
}

pub(crate) fn commit(
    mut staged: StagedExtensionArtifact,
    data_id: &str,
) -> Result<VerifiedExtensionArtifact, String> {
    let retained_archive = staged.extracted.join(".vqro-source.tar");
    if retained_archive.exists() {
        return Err("extension source reserves .vqro-source.tar".into());
    }
    fs::rename(staged.root.join("source.tar"), &retained_archive)
        .map_err(|error| format!("retain verified extension archive: {error}"))?;
    crate::platform::sync_containing_directory(&retained_archive)
        .map_err(|error| format!("sync retained extension archive: {error}"))?;
    let destination = artifact_root()?
        .join(storage_component(data_id))
        .join(format!(
            "{}-{}",
            safe_component(&staged.descriptor.version),
            staged.descriptor.commit
        ));
    if destination.exists() {
        let mut descriptor = staged.descriptor.clone();
        descriptor.path = destination.display().to_string();
        if verify(&descriptor).is_ok() {
            return Ok(descriptor);
        }
        fs::remove_dir_all(&destination)
            .map_err(|error| format!("remove corrupt retained extension artifact: {error}"))?;
        crate::platform::sync_containing_directory(&destination)
            .map_err(|error| format!("sync corrupt artifact removal: {error}"))?;
    }
    let parent = destination
        .parent()
        .ok_or_else(|| "extension artifact destination has no parent".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("create extension artifact directory: {error}"))?;
    ensure_managed_path(parent)?;
    crate::platform::replace_file(&staged.extracted, &destination)
        .map_err(|error| format!("commit verified extension artifact: {error}"))?;
    crate::platform::sync_containing_directory(&destination)
        .map_err(|error| format!("sync extension artifact directory: {error}"))?;
    staged.descriptor.path = destination.display().to_string();
    Ok(staged.descriptor.clone())
}

pub(crate) fn read_verified_file(
    descriptor: &VerifiedExtensionArtifact,
    relative_path: &str,
) -> Result<Vec<u8>, String> {
    verify(descriptor)?;
    read_archive_file(descriptor, relative_path)
}

fn read_archive_file(
    descriptor: &VerifiedExtensionArtifact,
    relative_path: &str,
) -> Result<Vec<u8>, String> {
    let requested = Path::new(relative_path);
    if requested.is_absolute()
        || requested.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("requested extension artifact path is unsafe".into());
    }
    let archive_file = File::open(Path::new(&descriptor.path).join(".vqro-source.tar"))
        .map_err(|error| format!("open retained extension source: {error}"))?;
    let mut archive = tar::Archive::new(archive_file);
    for entry in archive
        .entries()
        .map_err(|error| format!("read retained extension source: {error}"))?
    {
        let mut entry = entry.map_err(|error| format!("read retained source entry: {error}"))?;
        let path = entry
            .path()
            .map_err(|error| format!("read retained source path: {error}"))?;
        if path.as_ref() != requested {
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err("requested extension artifact is not a regular file".into());
        }
        let size = entry
            .header()
            .size()
            .map_err(|error| format!("read retained source size: {error}"))?;
        if size > 4 * 1024 * 1024 {
            return Err("requested extension artifact exceeds 4 MiB".into());
        }
        let mut bytes = Vec::with_capacity(size as usize);
        entry
            .read_to_end(&mut bytes)
            .map_err(|error| format!("read retained extension artifact: {error}"))?;
        return Ok(bytes);
    }
    Err(format!(
        "verified extension source does not contain {relative_path}"
    ))
}

pub(crate) fn purge_artifact(descriptor: &VerifiedExtensionArtifact) -> Result<(), String> {
    let path = Path::new(&descriptor.path);
    ensure_managed_path(path)?;
    match fs::remove_dir_all(path) {
        Ok(()) => crate::platform::sync_containing_directory(path)
            .map_err(|error| format!("sync obsolete extension artifact deletion: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("delete obsolete extension artifact: {error}")),
    }
}

pub(crate) fn purge(data_id: &str) -> Result<(), String> {
    let path = artifact_root()?.join(storage_component(data_id));
    match fs::remove_dir_all(&path) {
        Ok(()) => crate::platform::sync_containing_directory(&path)
            .map_err(|error| format!("sync extension artifact deletion: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("delete extension artifacts: {error}")),
    }
}

pub(crate) fn retained(descriptor: &VerifiedExtensionArtifact) -> bool {
    let path = Path::new(&descriptor.path);
    artifact_root().is_ok_and(|root| path.starts_with(root))
        && ensure_managed_path(path).is_ok()
        && fs::symlink_metadata(path.join(".vqro-source.tar"))
            .is_ok_and(|metadata| metadata.file_type().is_file())
}

pub(crate) fn verify(descriptor: &VerifiedExtensionArtifact) -> Result<(), String> {
    let path = Path::new(&descriptor.path);
    let root = artifact_root()?;
    if !path.starts_with(&root) {
        return Err("extension artifact is outside managed storage".into());
    }
    ensure_managed_path(path)?;
    let retained_archive = path.join(".vqro-source.tar");
    let source_digest = digest_file(&retained_archive)?;
    if !source_digest.eq_ignore_ascii_case(&descriptor.source_sha256) {
        return Err("retained extension source digest mismatch".into());
    }
    let manifest = read_archive_file(descriptor, &descriptor.manifest_path)?;
    let digest = format!("{:x}", Sha256::digest(&manifest));
    if !digest.eq_ignore_ascii_case(&descriptor.manifest_sha256) {
        return Err("retained extension manifest digest mismatch".into());
    }
    verify_manifest_descriptor_bytes(&manifest, &descriptor.extension_id, &descriptor.version)
        .map_err(|error| format!("retained extension {error}"))
}

fn staging_root() -> Result<PathBuf, String> {
    let root = artifact_root()?.join(".staging");
    if let Ok(entries) = fs::read_dir(&root) {
        let cutoff = std::time::SystemTime::now()
            .checked_sub(Duration::from_secs(60 * 60))
            .unwrap_or(std::time::UNIX_EPOCH);
        for entry in entries.flatten() {
            let stale = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| modified < cutoff);
            if stale {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
    Ok(root)
}

fn artifact_root() -> Result<PathBuf, String> {
    let state_path = crate::extensions::state_storage_path()?;
    let parent = state_path
        .parent()
        .ok_or_else(|| "extension state path has no parent".to_string())?;
    Ok(parent.join("extension-artifacts"))
}

fn ensure_managed_path(path: &Path) -> Result<(), String> {
    let root = artifact_root()?;
    let canonical_root = fs::canonicalize(&root)
        .map_err(|error| format!("resolve extension artifact root: {error}"))?;
    let canonical_path = fs::canonicalize(path)
        .map_err(|error| format!("resolve extension artifact path: {error}"))?;
    if !canonical_path.starts_with(canonical_root) {
        return Err("extension artifact resolves outside managed storage".into());
    }
    Ok(())
}

fn github_repository(repository: &str) -> Result<&str, String> {
    let mut segments = repository.split('/');
    let owner = segments.next().unwrap_or_default();
    let name = segments.next().unwrap_or_default();
    if segments.next().is_some()
        || owner.is_empty()
        || name.is_empty()
        || repository.contains('#')
        || !owner.bytes().all(github_segment_byte)
        || !name.bytes().all(github_segment_byte)
    {
        return Err("extension repository is not an installable GitHub owner/repository".into());
    }
    Ok(repository)
}

fn github_segment_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

fn storage_component(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let suffix = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{}-{suffix}", safe_component(value))
}

fn safe_component(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
                byte as char
            } else {
                '_'
            }
        })
        .collect()
}

fn write_bounded_git_archive(
    repository: &Path,
    allow_file_protocol: bool,
    archive_path: &Path,
) -> Result<(), String> {
    let mut command = git_command(repository, allow_file_protocol);
    command
        .args(["archive", "--format=tar", "FETCH_HEAD"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    crate::platform::configure_extension_service_command(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("run git archive: {error}"))?;
    let _containment = match crate::platform::ExtensionServiceContainment::new(&child) {
        Ok(containment) => containment,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("contain git archive: {error}"));
        }
    };
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "git archive stdout was not captured".to_string())?;
    let path = archive_path.to_path_buf();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = (|| {
            let mut file =
                File::create(path).map_err(|error| format!("create extension archive: {error}"))?;
            let mut total = 0_u64;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = stdout
                    .read(&mut buffer)
                    .map_err(|error| format!("read git archive: {error}"))?;
                if read == 0 {
                    break;
                }
                total = total.saturating_add(read as u64);
                if total > MAX_ARCHIVE_BYTES {
                    return Err("pinned extension archive exceeds 8 MiB".into());
                }
                file.write_all(&buffer[..read])
                    .map_err(|error| format!("write extension archive: {error}"))?;
            }
            file.sync_all()
                .map_err(|error| format!("sync extension archive: {error}"))
        })();
        let _ = tx.send(result);
    });
    let deadline = Instant::now() + GIT_TIMEOUT;
    let archive_result = loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(result) => break result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                crate::platform::terminate_extension_service_process_group(&child);
                let _ = child.kill();
                let _ = child.wait();
                return Err("git archive exceeded its time limit".into());
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("git archive reader stopped unexpectedly".into());
            }
        }
    };
    if archive_result.is_err() {
        crate::platform::terminate_extension_service_process_group(&child);
        let _ = child.kill();
    }
    let status = child
        .wait()
        .map_err(|error| format!("wait for git archive: {error}"))?;
    archive_result?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| "git archive failed for pinned extension source".to_string())
}

fn git_command(repository: &Path, allow_file_protocol: bool) -> Command {
    let mut command = Command::new("git");
    for variable in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_INDEX_FILE",
        "GIT_CONFIG",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_NOSYSTEM",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_TEMPLATE_DIR",
        "GIT_SSH",
        "GIT_SSH_COMMAND",
        "GIT_PROXY_COMMAND",
        "GIT_ASKPASS",
        "SSH_ASKPASS",
    ] {
        command.env_remove(variable);
    }
    command
        .args([
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.https.allow=always",
        ])
        .arg("-c")
        .arg(if allow_file_protocol {
            "protocol.file.allow=always"
        } else {
            "protocol.file.allow=never"
        })
        .args(["-c", "core.hooksPath=", "-c", "init.templateDir="])
        .arg("-C")
        .arg(repository)
        .env("GIT_TERMINAL_PROMPT", "0");
    command
}

fn run_git<const N: usize>(
    repository: &Path,
    args: [&str; N],
    allow_file_protocol: bool,
) -> Result<(), String> {
    let mut command = git_command(repository, allow_file_protocol);
    command
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::platform::configure_extension_service_command(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("run git: {error}"))?;
    let status = wait_for_child(&mut child, GIT_TIMEOUT, || {
        directory_size(repository)
            .map(|size| size <= MAX_STAGING_BYTES)
            .unwrap_or(false)
    })?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| "git command failed".to_string())
}

fn wait_for_child(
    child: &mut Child,
    timeout: Duration,
    within_quota: impl Fn() -> bool,
) -> Result<ExitStatus, String> {
    let _containment = match crate::platform::ExtensionServiceContainment::new(child) {
        Ok(containment) => containment,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("contain git operation: {error}"));
        }
    };
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline || !within_quota() {
            crate::platform::terminate_extension_service_process_group(child);
            let _ = child.kill();
            let _ = child.wait();
            return Err("git operation exceeded its time or size limit".into());
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("wait for git: {error}"))?
        {
            return Ok(status);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn directory_size(path: &Path) -> Result<u64, String> {
    let mut total = 0_u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .map_err(|error| format!("inspect extension staging directory: {error}"))?
        {
            let entry =
                entry.map_err(|error| format!("inspect extension staging entry: {error}"))?;
            let metadata = entry
                .metadata()
                .map_err(|error| format!("inspect extension staging metadata: {error}"))?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                total = total.saturating_add(metadata.len());
                if total > MAX_STAGING_BYTES {
                    return Ok(total);
                }
            }
        }
    }
    Ok(total)
}

fn git_output<const N: usize>(
    repository: &Path,
    args: [&str; N],
    allow_file_protocol: bool,
) -> Result<String, String> {
    let output = git_command(repository, allow_file_protocol)
        .args(args)
        .output()
        .map_err(|error| format!("run git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|error| format!("git output is not UTF-8: {error}"))
}

fn digest_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn extract_safe_archive(archive_path: &Path, destination: &Path) -> Result<(), String> {
    let file =
        File::open(archive_path).map_err(|error| format!("open extension archive: {error}"))?;
    let mut archive = tar::Archive::new(file);
    let entries = archive
        .entries()
        .map_err(|error| format!("read extension archive: {error}"))?;
    let mut extracted_paths = std::collections::HashSet::new();
    for (entry_index, entry) in entries.enumerate() {
        if entry_index >= MAX_ARCHIVE_ENTRIES {
            return Err("extension archive contains too many entries".into());
        }
        let mut entry = entry.map_err(|error| format!("read extension archive entry: {error}"))?;
        let path = entry
            .path()
            .map_err(|error| format!("read extension archive path: {error}"))?
            .into_owned();
        if !safe_archive_path(&path) {
            return Err("extension archive contains an unsafe path".into());
        }
        let canonical_key = path
            .components()
            .filter_map(|component| match component {
                Component::Normal(component) => component.to_str(),
                _ => None,
            })
            .map(|component| component.to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join("/");
        if !extracted_paths.insert(canonical_key) {
            return Err("extension archive contains a colliding path".into());
        }
        let entry_type = entry.header().entry_type();
        let output = destination.join(&path);
        if entry_type.is_pax_global_extensions()
            || entry_type.is_pax_local_extensions()
            || entry_type.is_gnu_longname()
            || entry_type.is_gnu_longlink()
        {
            continue;
        }
        if entry_type.is_dir() {
            fs::create_dir_all(&output)
                .map_err(|error| format!("create extension archive directory: {error}"))?;
        } else if entry_type.is_file() {
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("create extension archive parent: {error}"))?;
            }
            let mut target = File::create(&output)
                .map_err(|error| format!("create extension archive file: {error}"))?;
            std::io::copy(&mut entry, &mut target)
                .map_err(|error| format!("extract extension archive file: {error}"))?;
            target
                .sync_all()
                .map_err(|error| format!("sync extension archive file: {error}"))?;
        } else {
            return Err("extension archive contains links or special files".into());
        }
    }
    Ok(())
}

fn safe_archive_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path.components().all(|component| {
            let Component::Normal(component) = component else {
                return false;
            };
            let Some(component) = component.to_str() else {
                return false;
            };
            let normalized = component.to_ascii_uppercase();
            let stem = normalized.split('.').next().unwrap_or(&normalized);
            !component.is_empty()
                && component != "."
                && component != ".."
                && !component.ends_with(['.', ' '])
                && component
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
                && !matches!(
                    stem,
                    "CON"
                        | "PRN"
                        | "AUX"
                        | "NUL"
                        | "COM1"
                        | "COM2"
                        | "COM3"
                        | "COM4"
                        | "COM5"
                        | "COM6"
                        | "COM7"
                        | "COM8"
                        | "COM9"
                        | "LPT1"
                        | "LPT2"
                        | "LPT3"
                        | "LPT4"
                        | "LPT5"
                        | "LPT6"
                        | "LPT7"
                        | "LPT8"
                        | "LPT9"
                )
        })
}

fn verify_manifest_identity(path: &Path, extension: &CuratedExtension) -> Result<(), String> {
    verify_manifest_descriptor(path, &extension.id, &extension.version)
}

fn verify_manifest_descriptor(
    path: &Path,
    expected_id: &str,
    expected_version: &str,
) -> Result<(), String> {
    let bytes =
        fs::read(path).map_err(|error| format!("read pinned extension manifest: {error}"))?;
    verify_manifest_descriptor_bytes(&bytes, expected_id, expected_version)
}

fn verify_manifest_descriptor_bytes(
    bytes: &[u8],
    expected_id: &str,
    expected_version: &str,
) -> Result<(), String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("pinned extension manifest is not UTF-8: {error}"))?;
    let manifest: toml::Value = toml::from_str(text)
        .map_err(|error| format!("parse pinned extension manifest: {error}"))?;
    let id = manifest
        .get("id")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| "pinned extension manifest has no id".to_string())?;
    let version = manifest
        .get("version")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| "pinned extension manifest has no version".to_string())?;
    if id != expected_id || version != expected_version {
        return Err(format!(
            "manifest identity mismatch: expected {expected_id} {expected_version}, got {id} {version}"
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("secure extension staging directory: {error}"))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extensions::ExtensionPackage;

    fn fixture() -> (PathBuf, CuratedExtension) {
        let root = std::env::temp_dir().join(format!(
            "vqro-extension-artifact-test-{}-{}",
            std::process::id(),
            NEXT_STAGE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        for args in [
            vec!["init", "--quiet"],
            vec!["config", "user.name", "Vqro Test"],
            vec!["config", "user.email", "vqro@example.invalid"],
        ] {
            assert!(Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .status()
                .unwrap()
                .success());
        }
        let manifest = b"id = \"test.artifact\"\nname = \"Artifact\"\nversion = \"1.2.3\"\n";
        fs::write(root.join("herdr-plugin.toml"), manifest).unwrap();
        fs::write(root.join("payload.txt"), b"verified payload\n").unwrap();
        assert!(Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["add", "."])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["commit", "--quiet", "-m", "fixture"])
            .status()
            .unwrap()
            .success());
        let commit = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        let archive = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["archive", "--format=tar", "HEAD"])
            .output()
            .unwrap()
            .stdout;
        let extension = CuratedExtension {
            id: "test.artifact".into(),
            repository: "example/artifact".into(),
            source_commit: Some(commit),
            source_sha256: Some(format!("{:x}", Sha256::digest(&archive))),
            manifest_path: "herdr-plugin.toml".into(),
            manifest_sha256: Some(format!("{:x}", Sha256::digest(manifest))),
            name: "Artifact".into(),
            summary: String::new(),
            details: String::new(),
            version: "1.2.3".into(),
            categories: vec!["Workflow".into()],
            package: ExtensionPackage::Bundled {
                provider: "artifact".into(),
            },
            default_installed: false,
            default_enabled: false,
        };
        (root, extension)
    }

    #[test]
    fn archive_paths_are_portable_to_windows() {
        assert!(safe_archive_path(Path::new("assets/herdr-agent-state.ts")));
        for unsafe_path in [
            "../escape",
            "assets/Foo ",
            "assets/name.",
            "assets/CON.txt",
            "assets/file:name",
            "assets/back\\slash",
        ] {
            assert!(!safe_archive_path(Path::new(unsafe_path)), "{unsafe_path}");
        }
    }

    #[test]
    fn stages_exact_commit_and_verifies_source_manifest_and_identity() {
        let (repository, extension) = fixture();
        let staged = stage_from_source(&extension, repository.to_str().unwrap(), true).unwrap();

        assert_eq!(staged.descriptor.version, "1.2.3");
        assert!(staged.extracted.join("payload.txt").is_file());
        drop(staged);
        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn rejects_a_manifest_identity_mismatch() {
        let (repository, mut extension) = fixture();
        extension.id = "test.other".into();

        let error = stage_from_source(&extension, repository.to_str().unwrap(), true).unwrap_err();

        assert!(error.contains("manifest identity mismatch"));
        fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn rejects_a_source_digest_mismatch() {
        let (repository, mut extension) = fixture();
        extension.source_sha256 = Some("00".repeat(32));

        let error = stage_from_source(&extension, repository.to_str().unwrap(), true).unwrap_err();

        assert!(error.contains("source digest mismatch"));
        fs::remove_dir_all(repository).unwrap();
    }
}
