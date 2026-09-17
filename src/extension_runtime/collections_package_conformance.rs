use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::{
    ComponentRuntimeFactory, ExtensionRuntime, ExtensionRuntimeDispatcher, RuntimeError,
    RuntimeFactory, RuntimeHostCallHandler, RuntimeHostCallResponseV1, RuntimeHostCallRouter,
    RuntimeRequestV1, RuntimeStopReason, EXTENSION_SERVICE_CONTRACT_V1,
};
use crate::extension_package::{
    install_local_file, validate_package_bytes, validate_package_file, PackageProvenanceV1,
    PackageRuntimeV1, PACKAGE_CHECKSUM_FILE, PACKAGE_MANIFEST_FILE,
};

const CANDIDATE_COMMIT: &str = "9d0e3d50f826e1aa1e9b74f62fcfd8a4b27a8d47";
const CANDIDATE_REPOSITORY: &str = "https://github.com/ttenneb/vqro-extension-collections.git";
const CANDIDATE_GITATTRIBUTES: &[u8] = b"* text=auto eol=lf\n";
const HOST_WIT_COMMIT: &str = "74406386828e182043d48ecf3511d4c4288924ff";
const HOST_WIT_SHA256: &str = "625f909dcd26c714e94b0361e4c3fde969c792e0497aaf7477e08f4f73f22daf";
const CANDIDATE_PROVENANCE: &str = "# Contract provenance\n\nThis directory is an exact snapshot of Vqro's public component world.\n\n- Repository: `https://github.com/ttenneb/vqro`\n- Host commit: `74406386828e182043d48ecf3511d4c4288924ff`\n- Source path: `wit/vqro-extension-service/world.wit`\n- SHA-256: `625f909dcd26c714e94b0361e4c3fde969c792e0497aaf7477e08f4f73f22daf`\n\n`cargo run -p xtask -- contract-check` verifies the snapshot hash. Updating the\ncontract requires an explicit provenance update and host integration review.\n";

struct RejectingHostCalls {
    count: Arc<AtomicUsize>,
}

impl RuntimeHostCallHandler for RejectingHostCalls {
    fn handle_host_call(&self, call: super::RuntimeHostCallV1) -> RuntimeHostCallResponseV1 {
        self.count.fetch_add(1, Ordering::AcqRel);
        RuntimeHostCallResponseV1 {
            kind: "host_call_response".into(),
            call_id: call.call_id,
            identity: call.identity,
            result: None,
            error: Some(super::RuntimeErrorBodyV1 {
                code: "unexpected_host_call".into(),
                message: "zero-authority candidate attempted a host call".into(),
            }),
        }
    }
}

#[test]
#[ignore = "requires the exact external candidate checkout; run just collections-package-conformance"]
fn collections_candidate_passes_authoritative_host_gate() {
    let checkout = std::env::var_os("VQRO_COLLECTIONS_CHECKOUT")
        .map(PathBuf::from)
        .expect("VQRO_COLLECTIONS_CHECKOUT must point to the exact Collections candidate checkout");
    assert_candidate_checkout(&checkout);
    assert_contract_provenance(&checkout);

    let root = temporary_root();
    std::fs::create_dir_all(&root).expect("create conformance root");
    let first_path = root.join("candidate-first.vqrox");
    let second_path = root.join("candidate-second.vqrox");
    build_candidate(&checkout, &first_path);
    build_candidate(&checkout, &second_path);
    let package_bytes = std::fs::read(&first_path).expect("read first candidate package");
    assert_eq!(
        package_bytes,
        std::fs::read(&second_path).expect("read second candidate package"),
        "candidate package construction is not deterministic"
    );

    let package_sha256 = sha256(&package_bytes);
    let validated = validate_package_file(&first_path, Some(&package_sha256))
        .expect("host package-v1 file validation must accept the candidate");
    let manifest = &validated.descriptor.manifest;
    assert_eq!(manifest.id, "vqro.collections");
    assert_eq!(manifest.version, "0.0.0");
    assert_eq!(manifest.min_vqro_version, "0.9.0");
    assert_eq!(manifest.namespaces, ["vqro.collections"]);
    assert_eq!(manifest.services.len(), 1);
    let service = &manifest.services[0];
    assert_eq!(service.id, "collections");
    assert!(service.provides.is_empty());
    assert!(service.capabilities.is_empty());
    assert!(matches!(
        &service.runtime,
        PackageRuntimeV1::Component { component, world }
            if component == "services/collections.wasm"
                && world == "vqro:extension/service@1.0.0"
    ));

    let ordinary = PackageProvenanceV1::File {
        package_sha256: package_sha256.clone(),
    };
    let ordinary_error = install_local_file(
        &first_path,
        &root.join("ordinary-store"),
        Some(&package_sha256),
        ordinary,
        &BTreeMap::new(),
        None,
    )
    .expect_err("ordinary file provenance must not claim vqro.*");
    assert_eq!(
        ordinary_error,
        "local files cannot claim vqro.* without a trusted link approval"
    );

    let curated = PackageProvenanceV1::Curated {
        catalog: "collections-candidate-conformance".into(),
        publisher: "vqro".into(),
        repository: CANDIDATE_REPOSITORY.into(),
        package_id: manifest.id.clone(),
        version: manifest.version.clone(),
        source_commit: CANDIDATE_COMMIT.into(),
        package_sha256: package_sha256.clone(),
        manifest_sha256: validated.descriptor.manifest_sha256.clone(),
        authorized_namespaces: vec!["vqro.collections".into()],
    };
    let trusted_curated = curated.clone();
    let mut forged_curated = curated.clone();
    if let PackageProvenanceV1::Curated { source_commit, .. } = &mut forged_curated {
        *source_commit = "0000000000000000000000000000000000000000".into();
    }
    let forged_error = install_local_file(
        &first_path,
        &root.join("forged-curated-store"),
        Some(&package_sha256),
        forged_curated,
        &BTreeMap::new(),
        Some(&trusted_curated),
    )
    .expect_err("curated provenance must exactly match the independent trust decision");
    assert_eq!(
        forged_error,
        "curated provenance is not exactly authorized by the trusted catalog"
    );
    let installed = install_local_file(
        &first_path,
        &root.join("curated-store"),
        Some(&package_sha256),
        curated,
        &BTreeMap::new(),
        Some(&trusted_curated),
    )
    .expect("exact trusted curated provenance must install");

    let canceled = AtomicBool::new(false);
    let production_factory =
        RuntimeFactory::installed(&installed, "collections", "vqro.collections")
            .expect("installed candidate must enter the runtime factory trust boundary");
    let mut dispatcher = ExtensionRuntimeDispatcher::start(&production_factory, 7, &canceled)
        .expect("candidate must start through the production dispatcher");
    assert!(dispatcher.methods().is_empty());
    dispatcher.stop(RuntimeStopReason::Requested);
    drop(dispatcher);

    let host_call_count = Arc::new(AtomicUsize::new(0));
    let factory = ComponentRuntimeFactory::installed(&installed, "collections", "vqro.collections")
        .expect("verified installed component must compile");
    let mut runtime = factory
        .start(8, &canceled)
        .expect("candidate must instantiate in the host Wasmtime runtime");
    assert_eq!(runtime.descriptor().identity.package_id, "vqro.collections");
    assert_eq!(runtime.descriptor().identity.namespace, "vqro.collections");
    assert_eq!(runtime.descriptor().identity.service_id, "collections");
    assert_eq!(runtime.descriptor().identity.generation, 8);
    assert!(runtime.descriptor().methods.is_empty());
    // Descriptor negotiation has no host-call router. A startup host call would have
    // failed instantiation; the counter is established before all directly observed calls.
    assert_eq!(host_call_count.load(Ordering::Acquire), 0);

    let host_calls = Arc::new(RuntimeHostCallRouter::new(RejectingHostCalls {
        count: Arc::clone(&host_call_count),
    }));
    let invocation = RuntimeRequestV1 {
        contract: EXTENSION_SERVICE_CONTRACT_V1.into(),
        request_id: "collections-candidate-gate".into(),
        identity: runtime.descriptor().identity.clone(),
        method: "collections.unavailable".into(),
        params: serde_json::json!({}),
    };
    let invocation_error = runtime
        .invoke(
            &invocation,
            Instant::now() + Duration::from_millis(750),
            &canceled,
            &host_calls,
        )
        .expect_err("zero-authority candidate invocation must be rejected");
    assert!(matches!(&invocation_error, RuntimeError::Remote(_)));
    assert_eq!(
        invocation_error.to_string(),
        "component service error [no_authority]: Collections candidate has no callable methods"
    );
    runtime.stop(RuntimeStopReason::Requested);
    assert_eq!(host_call_count.load(Ordering::Acquire), 0);

    assert_package_mutations_are_rejected(&package_bytes, &package_sha256);
    overwrite(
        &Path::new(&installed.immutable_root).join("services/collections.wasm"),
        b"mutated installed component",
    );
    assert!(matches!(
        ComponentRuntimeFactory::installed(&installed, "collections", "vqro.collections"),
        Err(RuntimeError::Configuration(message)) if message.contains("installed package file changed")
    ));

    drop(runtime);
    cleanup(root);
}

fn assert_candidate_checkout(checkout: &Path) {
    let head = command_output(checkout, "git", &["rev-parse", "HEAD"]);
    assert_eq!(
        head.trim(),
        CANDIDATE_COMMIT,
        "candidate checkout is not pinned"
    );
    let tracked_diff = Command::new("git")
        .current_dir(checkout)
        .args(["diff", "--quiet", "HEAD", "--"])
        .status()
        .expect("inspect candidate tracked files");
    assert!(
        tracked_diff.success(),
        "candidate checkout has tracked modifications"
    );
}

fn assert_contract_provenance(checkout: &Path) {
    let attributes = std::fs::read(checkout.join(".gitattributes"))
        .expect("read candidate checkout normalization policy");
    assert_eq!(attributes, CANDIDATE_GITATTRIBUTES);
    let candidate_wit = std::fs::read(checkout.join("wit/vqro-extension-service/world.wit"))
        .expect("read candidate WIT snapshot");
    let host_wit = include_bytes!("../../wit/vqro-extension-service/world.wit");
    assert_eq!(
        candidate_wit, host_wit,
        "candidate WIT differs byte-for-byte from host"
    );
    assert_eq!(sha256(host_wit), HOST_WIT_SHA256);
    let provenance = std::fs::read(checkout.join("wit/vqro-extension-service/PROVENANCE.md"))
        .expect("read candidate WIT provenance");
    assert_eq!(provenance, CANDIDATE_PROVENANCE.as_bytes());
    assert!(CANDIDATE_PROVENANCE.contains(HOST_WIT_COMMIT));
}

fn build_candidate(checkout: &Path, output: &Path) {
    let status = Command::new("cargo")
        .current_dir(checkout)
        .args(["run", "--locked", "-p", "xtask", "--", "package"])
        .arg(output)
        .status()
        .expect("run candidate deterministic package builder");
    assert!(status.success(), "candidate package construction failed");
}

fn assert_package_mutations_are_rejected(original: &[u8], digest: &str) {
    let mut package_mutation = original.to_vec();
    package_mutation[0] ^= 1;
    assert!(validate_package_bytes(&package_mutation, Some(digest)).is_err());

    let checksum_mutation = mutate_archive_entry(original, PACKAGE_CHECKSUM_FILE, |bytes| {
        bytes[0] = if bytes[0] == b'0' { b'1' } else { b'0' };
    });
    assert!(validate_package_bytes(&checksum_mutation, None).is_err());

    let manifest_mutation = mutate_archive_entry(original, PACKAGE_MANIFEST_FILE, |bytes| {
        let text = std::str::from_utf8(bytes).expect("candidate manifest is UTF-8");
        let changed = text.replacen("version = \"0.0.0\"", "version = \"0.0.1\"", 1);
        assert_ne!(changed, text);
        *bytes = changed.into_bytes();
    });
    assert!(validate_package_bytes(&manifest_mutation, None).is_err());

    let component_mutation = mutate_archive_entry(original, "services/collections.wasm", |bytes| {
        let last = bytes.last_mut().expect("candidate component is not empty");
        *last ^= 1;
    });
    assert!(validate_package_bytes(&component_mutation, None).is_err());
}

fn mutate_archive_entry(
    archive_bytes: &[u8],
    selected: &str,
    mutation: impl FnOnce(&mut Vec<u8>),
) -> Vec<u8> {
    let mut archive = tar::Archive::new(archive_bytes);
    let mut entries = Vec::new();
    let mut mutation = Some(mutation);
    for entry in archive.entries().expect("read candidate archive") {
        let mut entry = entry.expect("read candidate entry");
        let path = entry
            .path()
            .expect("read candidate path")
            .to_str()
            .expect("candidate path is UTF-8")
            .to_owned();
        let mode = entry.header().mode().expect("read candidate mode");
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes).expect("read candidate entry bytes");
        if path == selected {
            mutation.take().expect("selected entry is unique")(&mut bytes);
        }
        entries.push((path, bytes, mode));
    }
    assert!(mutation.is_none(), "selected archive entry was absent");
    let mut output = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut output);
        builder.mode(tar::HeaderMode::Deterministic);
        for (path, bytes, mode) in entries {
            let mut header = tar::Header::new_ustar();
            header.set_path(path).expect("encode candidate path");
            header.set_size(bytes.len() as u64);
            header.set_mode(mode);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            header.set_cksum();
            builder
                .append(&header, bytes.as_slice())
                .expect("encode mutated candidate archive");
        }
        builder.finish().expect("finish mutated candidate archive");
    }
    output
}

fn command_output(cwd: &Path, program: &str, args: &[&str]) -> String {
    let output = Command::new(program)
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run candidate checkout command");
    assert!(output.status.success(), "candidate checkout command failed");
    String::from_utf8(output.stdout).expect("candidate command output is UTF-8")
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn temporary_root() -> PathBuf {
    let mut random = [0_u8; 8];
    crate::platform::fill_secure_random(&mut random).expect("generate conformance path suffix");
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    std::env::temp_dir().join(format!(
        "vqro-collections-conformance-{}-{suffix}",
        std::process::id()
    ))
}

#[cfg(unix)]
fn overwrite(path: &Path, bytes: &[u8]) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)
        .expect("inspect installed component")
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions).expect("make installed component writable");
    std::fs::write(path, bytes).expect("mutate installed component");
}

#[cfg(windows)]
fn overwrite(path: &Path, bytes: &[u8]) {
    let mut permissions = std::fs::metadata(path)
        .expect("inspect installed component")
        .permissions();
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions).expect("make installed component writable");
    std::fs::write(path, bytes).expect("mutate installed component");
}

#[cfg(unix)]
fn cleanup(root: PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let mut pending = vec![root.clone()];
    while let Some(directory) = pending.pop() {
        let mut permissions = std::fs::metadata(&directory)
            .expect("inspect cleanup directory")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&directory, permissions).expect("make cleanup directory writable");
        for entry in std::fs::read_dir(&directory).expect("read cleanup directory") {
            let path = entry.expect("read cleanup entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let mut permissions = std::fs::metadata(&path)
                    .expect("inspect cleanup file")
                    .permissions();
                permissions.set_mode(0o600);
                std::fs::set_permissions(&path, permissions).expect("make cleanup file writable");
            }
        }
    }
    std::fs::remove_dir_all(root).expect("remove conformance root");
}

#[cfg(windows)]
fn cleanup(root: PathBuf) {
    let mut pending = vec![root.clone()];
    while let Some(directory) = pending.pop() {
        let mut permissions = std::fs::metadata(&directory)
            .expect("inspect cleanup directory")
            .permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&directory, permissions).expect("make cleanup directory writable");
        for entry in std::fs::read_dir(&directory).expect("read cleanup directory") {
            let path = entry.expect("read cleanup entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let mut permissions = std::fs::metadata(&path)
                    .expect("inspect cleanup file")
                    .permissions();
                permissions.set_readonly(false);
                std::fs::set_permissions(&path, permissions).expect("make cleanup file writable");
            }
        }
    }
    std::fs::remove_dir_all(root).expect("remove conformance root");
}
