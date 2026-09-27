//! Bounded, read-only inspection of package-v1 artifacts pinned in Git.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::extension_package::{PackageRuntimeV1, ValidatedPackageV1, MAX_PACKAGE_BYTES};

const DEFAULT_PACKAGE_PATH: &str = "package.vqrox";
const GIT_TIMEOUT: Duration = Duration::from_secs(25);
const MAX_REPOSITORY_BYTES: u64 = 64 * 1024 * 1024;
static NEXT_INSPECTION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitPackageInspection {
    pub source_url: String,
    pub commit: String,
    pub package_path: String,
    pub package: ValidatedPackageV1,
    pub capabilities: Vec<String>,
    pub package_bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub(crate) enum GitPackageAction {
    Inspect,
    Install {
        expected_package_sha256: String,
        expected_manifest_sha256: String,
    },
    Update {
        expected_package_sha256: String,
        expected_manifest_sha256: String,
    },
}

#[derive(Debug)]
pub(crate) struct GitPackageWorkResult {
    pub id: String,
    pub action: GitPackageAction,
    pub inspection: Result<GitPackageInspection, String>,
    pub respond_to: std::sync::mpsc::Sender<String>,
}

/// Fetches and inspects one immutable package. Production callers may use only
/// credential-free HTTPS repositories.
pub(crate) fn inspect(
    source_url: &str,
    commit: &str,
    package_path: Option<&str>,
) -> Result<GitPackageInspection, String> {
    inspect_inner(source_url, commit, package_path, false)
}

#[cfg(test)]
fn inspect_test_source(
    source_url: &str,
    commit: &str,
    package_path: Option<&str>,
) -> Result<GitPackageInspection, String> {
    inspect_inner(source_url, commit, package_path, true)
}

fn inspect_inner(
    source_url: &str,
    commit: &str,
    package_path: Option<&str>,
    allow_file_url: bool,
) -> Result<GitPackageInspection, String> {
    validate_commit(commit)?;
    validate_source_url(source_url, allow_file_url)?;
    let package_path = package_path.unwrap_or(DEFAULT_PACKAGE_PATH);
    validate_package_path(package_path)?;

    let stage = InspectionStage::new()?;
    run_git(&stage.repository, ["init", "--quiet"], allow_file_url)?;
    run_git(
        &stage.repository,
        ["remote", "add", "origin", source_url],
        allow_file_url,
    )?;
    run_git(
        &stage.repository,
        ["fetch", "--quiet", "--depth=1", "origin", commit],
        allow_file_url,
    )?;
    let resolved = git_output(
        &stage.repository,
        ["rev-parse", "FETCH_HEAD"],
        allow_file_url,
    )?;
    if !resolved.eq_ignore_ascii_case(commit) {
        return Err("fetched commit does not exactly match the requested commit".into());
    }

    let tree = git_output(
        &stage.repository,
        ["ls-tree", "-r", "--full-tree", "FETCH_HEAD"],
        allow_file_url,
    )?;
    if tree.lines().any(|line| line.starts_with("160000 ")) {
        return Err("Git package repositories must not contain submodules".into());
    }
    let package_record = tree.lines().find(|line| {
        line.split_once('\t')
            .is_some_and(|(_, path)| path == package_path)
    });
    let Some(package_record) = package_record else {
        return Err(format!("pinned commit does not contain {package_path}"));
    };
    if package_record.starts_with("120000 ") {
        return Err("Git package path must not be a symlink".into());
    }
    if !package_record.starts_with("100644 ") && !package_record.starts_with("100755 ") {
        return Err("Git package path is not a regular file".into());
    }

    let bytes = git_bytes_bounded(
        &stage.repository,
        &format!("FETCH_HEAD:{package_path}"),
        allow_file_url,
    )?;
    let validated = crate::extension_package::validate_package_bytes(&bytes, None)
        .map_err(|error| format!("invalid extension package: {error}"))?;
    if validated
        .descriptor
        .manifest
        .services
        .iter()
        .any(|service| matches!(service.runtime, PackageRuntimeV1::Process { .. }))
    {
        return Err("Git extension packages must not declare process services".into());
    }
    let mut capabilities = validated
        .descriptor
        .manifest
        .services
        .iter()
        .flat_map(|service| service.capabilities.iter())
        .cloned()
        .collect::<Vec<_>>();
    capabilities.sort();
    capabilities.dedup();

    Ok(GitPackageInspection {
        source_url: source_url.into(),
        commit: resolved,
        package_path: package_path.into(),
        package: validated.descriptor,
        capabilities,
        package_bytes: bytes,
    })
}

fn validate_commit(commit: &str) -> Result<(), String> {
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Git package commit must be exactly 40 hexadecimal characters".into());
    }
    Ok(())
}

fn validate_source_url(source_url: &str, allow_file_url: bool) -> Result<(), String> {
    if allow_file_url && source_url.starts_with("file://") {
        return Ok(());
    }
    let Some(rest) = source_url.strip_prefix("https://") else {
        return Err("Git package URL must use HTTPS".into());
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err("Git package URL must have a host and must not contain credentials".into());
    }
    if source_url
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte == b'\\')
    {
        return Err("Git package URL contains unsafe characters".into());
    }
    Ok(())
}

fn validate_package_path(path: &str) -> Result<(), String> {
    let path = Path::new(path);
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err("Git package path must be a safe relative path".into());
    }
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err("Git package path must be a safe relative path".into());
        };
        let Some(component) = component.to_str() else {
            return Err("Git package path must be UTF-8".into());
        };
        if component.is_empty()
            || !component
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err("Git package path contains unsafe characters".into());
        }
    }
    Ok(())
}

struct InspectionStage {
    root: PathBuf,
    repository: PathBuf,
}

impl InspectionStage {
    fn new() -> Result<Self, String> {
        let root = std::env::temp_dir().join(format!(
            "vqro-git-package-{}-{}",
            std::process::id(),
            NEXT_INSPECTION.fetch_add(1, Ordering::Relaxed)
        ));
        let repository = root.join("repository");
        fs::create_dir_all(&repository)
            .map_err(|error| format!("create Git package staging directory: {error}"))?;
        Ok(Self { root, repository })
    }
}

impl Drop for InspectionStage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn git_command(repository: &Path, allow_file_url: bool) -> Command {
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
        "GIT_CONFIG_COUNT",
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
        .arg(if allow_file_url {
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
    allow_file_url: bool,
) -> Result<(), String> {
    let mut command = git_command(repository, allow_file_url);
    command
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::platform::configure_extension_service_command(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("run git: {error}"))?;
    let status = wait_bounded(&mut child, repository)?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| "Git package fetch failed".into())
}

fn git_output<const N: usize>(
    repository: &Path,
    args: [&str; N],
    allow_file_url: bool,
) -> Result<String, String> {
    let bytes = git_output_bounded(repository, args, allow_file_url, 2 * 1024 * 1024)?;
    String::from_utf8(bytes)
        .map(|value| value.trim().into())
        .map_err(|_| "git output is not UTF-8".into())
}

fn git_bytes_bounded(
    repository: &Path,
    object: &str,
    allow_file_url: bool,
) -> Result<Vec<u8>, String> {
    git_output_bounded(
        repository,
        ["show", object],
        allow_file_url,
        MAX_PACKAGE_BYTES,
    )
}

fn git_output_bounded<const N: usize>(
    repository: &Path,
    args: [&str; N],
    allow_file_url: bool,
    limit: u64,
) -> Result<Vec<u8>, String> {
    let output_path = repository.join(".git-package-output");
    let output_file =
        fs::File::create(&output_path).map_err(|error| format!("create Git output: {error}"))?;
    let mut command = git_command(repository, allow_file_url);
    command
        .args(args)
        .stdout(Stdio::from(output_file))
        .stderr(Stdio::null());
    crate::platform::configure_extension_service_command(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("run git: {error}"))?;
    let status = wait_with_limit(&mut child, repository, &output_path, limit)?;
    if !status.success() {
        return Err("Git package command failed".into());
    }
    let bytes = fs::read(&output_path).map_err(|error| format!("read Git output: {error}"))?;
    let _ = fs::remove_file(output_path);
    Ok(bytes)
}

fn wait_bounded(child: &mut Child, repository: &Path) -> Result<ExitStatus, String> {
    wait_with_limit(child, repository, Path::new(""), 0)
}

fn wait_with_limit(
    child: &mut Child,
    repository: &Path,
    output: &Path,
    output_limit: u64,
) -> Result<ExitStatus, String> {
    let _containment = match crate::platform::ExtensionServiceContainment::new(child) {
        Ok(containment) => containment,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("contain git operation: {error}"));
        }
    };
    let deadline = Instant::now() + GIT_TIMEOUT;
    loop {
        let output_ok = output_limit == 0
            || fs::metadata(output)
                .map(|m| m.len() <= output_limit)
                .unwrap_or(true);
        if Instant::now() >= deadline
            || !output_ok
            || directory_size(repository)? > MAX_REPOSITORY_BYTES
        {
            crate::platform::terminate_extension_service_process_group(child);
            let _ = child.kill();
            let _ = child.wait();
            return Err("Git package operation exceeded its time or size limit".into());
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("wait for git: {error}"))?
        {
            return Ok(status);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn directory_size(path: &Path) -> Result<u64, String> {
    let mut total = 0_u64;
    let mut pending = vec![path.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in
            fs::read_dir(directory).map_err(|error| format!("inspect Git staging: {error}"))?
        {
            let entry = entry.map_err(|error| format!("inspect Git staging: {error}"))?;
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|error| format!("inspect Git staging: {error}"))?;
            if metadata.file_type().is_dir() {
                pending.push(entry.path());
            } else {
                total = total.saturating_add(metadata.len());
            }
            if total > MAX_REPOSITORY_BYTES {
                return Ok(total);
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;

    struct Repo {
        root: PathBuf,
    }
    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn package(process: bool) -> Vec<u8> {
        let runtime = if process {
            "kind = \"process\"\nexecutables = { linux-x86_64 = \"services/linux-x86_64/service\" }"
        } else {
            "kind = \"component\"\ncomponent = \"services/service.wasm\"\nworld = \"vqro:extension/service@1.0.0\""
        };
        let manifest = format!("manifest_version = 2\npackage_contract = \"vqro.package.v1\"\nid = \"example.git\"\nversion = \"1.0.0\"\nmin_vqro_version = \"0.1.0\"\nnamespaces = [\"example.git\"]\n\n[[services]]\nid = \"main\"\nprovides = [\"zeta\"]\ncapabilities = [\"beta\", \"alpha\"]\n[services.runtime]\n{runtime}\n");
        let payload_name = if process {
            "services/linux-x86_64/service"
        } else {
            "services/service.wasm"
        };
        let payload = if process {
            b"#!/bin/sh\n".to_vec()
        } else {
            b"\0asm\x0d\0\x01\0".to_vec()
        };
        let mut files = BTreeMap::from([
            (
                payload_name.to_string(),
                (payload, if process { 0o755 } else { 0o644 }),
            ),
            ("vqro-extension.toml".into(), (manifest.into_bytes(), 0o644)),
        ]);
        let checksums = files
            .iter()
            .map(|(path, (bytes, _))| format!("{:x}  {path}\n", Sha256::digest(bytes)))
            .collect::<String>();
        files.insert("checksums.sha256".into(), (checksums.into_bytes(), 0o644));
        let mut bytes = Vec::new();
        {
            let mut archive = tar::Builder::new(&mut bytes);
            archive.mode(tar::HeaderMode::Deterministic);
            for (path, (contents, mode)) in files {
                let mut header = tar::Header::new_ustar();
                header.set_path(path).unwrap();
                header.set_size(contents.len() as u64);
                header.set_mode(mode);
                header.set_uid(0);
                header.set_gid(0);
                header.set_mtime(0);
                header.set_cksum();
                archive.append(&header, contents.as_slice()).unwrap();
            }
            archive.finish().unwrap();
        }
        bytes
    }

    fn repo(path: &str, bytes: &[u8]) -> (Repo, String, String) {
        let root = std::env::temp_dir().join(format!(
            "vqro-git-inspect-fixture-{}-{}",
            std::process::id(),
            NEXT_INSPECTION.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join(Path::new(path).parent().unwrap_or(Path::new("")))).unwrap();
        fs::write(root.join(path), bytes).unwrap();
        for args in [
            &["init", "--quiet"][..],
            &["config", "user.name", "Test"][..],
            &["config", "user.email", "test@example.invalid"][..],
            &["add", "."][..],
            &["commit", "--quiet", "-m", "fixture"][..],
        ] {
            assert!(Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .status()
                .unwrap()
                .success());
        }
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
        .into();
        let url = format!("file://{}", root.display());
        (Repo { root }, commit, url)
    }

    #[test]
    fn validates_commit_url_and_path_contracts() {
        for commit in ["abc", &"g".repeat(40), &"a".repeat(41)] {
            assert!(validate_commit(commit).is_err());
        }
        assert!(validate_commit(&"A".repeat(40)).is_ok());
        for url in [
            "http://example.test/a",
            "ssh://example.test/a",
            "https://user@example.test/a",
            "https:///a",
        ] {
            assert!(validate_source_url(url, false).is_err(), "{url}");
        }
        assert!(validate_source_url("https://example.test/repo.git", false).is_ok());
        assert!(validate_source_url("file:///tmp/repo", false).is_err());
        assert!(validate_source_url("file:///tmp/repo", true).is_ok());
        for path in [
            "",
            "/package.vqrox",
            "../package.vqrox",
            "a/../../package.vqrox",
            "a b/package.vqrox",
            "a\\package.vqrox",
        ] {
            assert!(validate_package_path(path).is_err(), "{path}");
        }
        assert!(validate_package_path("nested/package.vqrox").is_ok());
    }

    #[test]
    fn inspects_default_and_nested_packages_and_sorts_capabilities() {
        let (_repo, commit, url) = repo(DEFAULT_PACKAGE_PATH, &package(false));
        let result = inspect_test_source(&url, &commit, None).unwrap();
        assert_eq!(result.commit, commit);
        assert_eq!(result.package_path, DEFAULT_PACKAGE_PATH);
        assert_eq!(result.package.manifest.id, "example.git");
        assert_eq!(result.capabilities, ["alpha", "beta"]);

        let (_repo, commit, url) = repo("nested/custom.vqrox", &package(false));
        assert!(inspect_test_source(&url, &commit, Some("nested/custom.vqrox")).is_ok());
    }

    #[test]
    fn rejects_invalid_package_and_process_service() {
        let (_repo, commit, url) = repo(DEFAULT_PACKAGE_PATH, b"not a package");
        assert!(inspect_test_source(&url, &commit, None)
            .unwrap_err()
            .contains("invalid extension package"));
        let (_repo, commit, url) = repo(DEFAULT_PACKAGE_PATH, &package(true));
        let error = inspect_test_source(&url, &commit, None).unwrap_err();
        assert!(error.contains("process services"), "{error}");
    }

    #[test]
    fn rejects_missing_package_and_submodule_entry() {
        let (repo, commit, url) = repo("other", b"x");
        assert!(inspect_test_source(&url, &commit, None)
            .unwrap_err()
            .contains("does not contain"));
        assert!(Command::new("git")
            .arg("-C")
            .arg(&repo.root)
            .args([
                "update-index",
                "--add",
                "--cacheinfo",
                "160000,0000000000000000000000000000000000000001,submodule"
            ])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .arg("-C")
            .arg(&repo.root)
            .args(["commit", "--quiet", "-m", "submodule"])
            .status()
            .unwrap()
            .success());
        let new_commit = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(&repo.root)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        assert!(inspect_test_source(&url, &new_commit, None)
            .unwrap_err()
            .contains("submodules"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_package_path() {
        use std::os::unix::fs::symlink;
        let (repo, _, url) = repo("real", &package(false));
        symlink("real", repo.root.join(DEFAULT_PACKAGE_PATH)).unwrap();
        assert!(Command::new("git")
            .arg("-C")
            .arg(&repo.root)
            .args(["add", "."])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .arg("-C")
            .arg(&repo.root)
            .args(["commit", "--quiet", "-m", "symlink"])
            .status()
            .unwrap()
            .success());
        let commit = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(&repo.root)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        assert!(inspect_test_source(&url, &commit, None)
            .unwrap_err()
            .contains("symlink"));
    }
}
