use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::{ExtensionRuntimeDispatcher, RuntimeFactory, RuntimeStopReason};
use crate::extension_package::{
    install_local_file, validate_package_file, PackageProvenanceV1, PackageRuntimeV1,
};

#[test]
#[ignore = "requires the exact external Spaces checkout; run just spaces-package-conformance"]
fn spaces_package_passes_authoritative_host_gate() {
    let checkout = std::env::var_os("VQRO_SPACES_CHECKOUT")
        .map(PathBuf::from)
        .expect("VQRO_SPACES_CHECKOUT must point to the exact Spaces package checkout");
    let pin = crate::spaces_extension_contract::embedded_spaces_production_pin().unwrap();
    assert_checkout(&checkout, &pin.source_commit);

    let package_path = checkout.join(&pin.package_path);
    let package_bytes = std::fs::read(&package_path).expect("read committed Spaces artifact");
    assert_eq!(sha256(&package_bytes), pin.package_sha256);
    let package = validate_package_file(&package_path, Some(&pin.package_sha256))
        .expect("host package validation must accept the exact Spaces artifact");
    assert_eq!(package.descriptor.manifest_sha256, pin.manifest_sha256);
    assert_eq!(package.descriptor.manifest.id, pin.package_id);
    assert_eq!(
        package.descriptor.manifest.namespaces,
        [pin.namespace.as_str()]
    );
    assert_eq!(package.descriptor.manifest.services.len(), 1);
    let service = &package.descriptor.manifest.services[0];
    assert_eq!(service.id, pin.service_id);
    assert!(service.capabilities.is_empty());
    assert!(matches!(
        &service.runtime,
        PackageRuntimeV1::Component { component, world }
            if component == "services/spaces.wasm"
                && world == "vqro:extension/service@1.0.0"
    ));
    assert_eq!(
        package
            .descriptor
            .files
            .iter()
            .find(|file| file.path == "services/spaces.wasm")
            .map(|file| file.sha256.as_str()),
        Some(pin.component_sha256.as_str())
    );

    let root = temporary_root();
    let installed = install_local_file(
        &package_path,
        &root.join("store"),
        Some(&pin.package_sha256),
        PackageProvenanceV1::Git {
            url: pin.source_url.clone(),
            source_commit: pin.source_commit.clone(),
            package_path: pin.package_path.clone(),
            package_id: pin.package_id.clone(),
            version: pin.version.clone(),
            package_sha256: pin.package_sha256.clone(),
            manifest_sha256: pin.manifest_sha256.clone(),
        },
        &std::collections::BTreeMap::new(),
        None,
    )
    .expect("the exact production pin authorizes its reserved namespace");
    let factory = RuntimeFactory::installed(&installed, &pin.service_id, &pin.namespace).unwrap();
    assert!(factory.grants().capabilities.is_empty());
    let canceled = AtomicBool::new(false);
    let mut runtime = ExtensionRuntimeDispatcher::start(&factory, 1, &canceled).unwrap();
    assert!(runtime.methods().contains(&pin.method));
    let snapshot = crate::extension_contract::HostSpacesV1 {
        contract: "host.spaces.v1".into(),
        generation: 3,
        revision: 7,
        scope_id: crate::spaces_extension_contract::SPACES_SCOPE_ID.into(),
        repositories: vec![crate::extension_contract::HostSpaceRepository {
            repository_id: "repo_vqro".into(),
            label: "Vqro".into(),
            workspaces: vec![crate::extension_contract::HostSpaceWorkspaceRef {
                workspace_id: "workspace_main".into(),
                label: "main".into(),
            }],
        }],
        standalone: vec![crate::extension_contract::HostSpaceWorkspaceRef {
            workspace_id: "workspace_notes".into(),
            label: "Notes".into(),
        }],
        order: vec![
            crate::extension_contract::HostSpaceOrderRef::StandaloneWorkspace {
                workspace_id: "workspace_notes".into(),
            },
            crate::extension_contract::HostSpaceOrderRef::Repository {
                repository_id: "repo_vqro".into(),
            },
        ],
    };
    let result = runtime
        .invoke(
            &pin.method,
            serde_json::to_value(&snapshot).unwrap(),
            1,
            Instant::now() + Duration::from_secs(2),
            &canceled,
        )
        .unwrap();
    let document: crate::extension_contract::HostDocumentV1 =
        serde_json::from_value(result).unwrap();
    crate::spaces_extension_contract::validate_spaces_document(&snapshot, &document).unwrap();
    runtime.stop(RuntimeStopReason::Requested);
    crate::app::App::assert_spaces_component_supersession_for_test(&installed);
    cleanup(root);
}

fn assert_checkout(checkout: &Path, expected_commit: &str) {
    let head = command_output(checkout, "git", &["rev-parse", "HEAD"]);
    assert_eq!(head.trim(), expected_commit, "Spaces checkout drifted");
    let status = command_output(checkout, "git", &["status", "--porcelain"]);
    assert!(status.trim().is_empty(), "Spaces checkout is dirty");
}

fn command_output(cwd: &Path, program: &str, args: &[&str]) -> String {
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|error| panic!("run {program}: {error}"));
    assert!(
        output.status.success(),
        "{program} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("command output is UTF-8")
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn temporary_root() -> PathBuf {
    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);
    std::env::temp_dir().join(format!(
        "vqro-spaces-package-{}-{}",
        std::process::id(),
        NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn cleanup(root: PathBuf) {
    fn make_writable(path: &Path) {
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            return;
        };
        if metadata.is_dir() {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    make_writable(&entry.path());
                }
            }
        }
        let mut permissions = metadata.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(if metadata.is_dir() { 0o700 } else { 0o600 });
        }
        // Non-Unix fixtures clear read-only attributes; Unix uses explicit modes above.
        #[cfg(not(unix))]
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        let _ = std::fs::set_permissions(path, permissions);
    }
    if root.exists() {
        make_writable(&root);
        let _ = std::fs::remove_dir_all(root);
    }
}
