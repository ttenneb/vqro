use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::{
    ComponentRuntimeFactory, ExtensionRuntimeDispatcher, RuntimeError, RuntimeFactory,
    RuntimeStopReason,
};
use crate::extension_package::{
    install_local_file, validate_package_bytes, validate_package_file, PackageProvenanceV1,
    PackageRuntimeV1, PACKAGE_CHECKSUM_FILE, PACKAGE_MANIFEST_FILE,
};

const CANDIDATE_COMMIT: &str = "32b489994443e4bca96ecd844769344d225d3bc4";
const CANDIDATE_REPOSITORY: &str = "https://github.com/ttenneb/vqro-extension-collections.git";
const CANDIDATE_GITATTRIBUTES: &[u8] = b"* text=auto eol=lf\n";
const CANDIDATE_PACKAGE_SHA256: &str =
    "42745bb5af54b0dced06cf0e92e8c95383e42641c105f6256249ede67af2c634";
const CANDIDATE_MANIFEST_SHA256: &str =
    "73c6accc4e35f9030731973e84ee3b1cce252c1034ab99e78ca6425cf40e9295";
const CANDIDATE_COMPONENT_SHA256: &str =
    "44e48062488a7ed66a213d84e4f46b7e19cce1bf8b64c08ea83292cb757b49e0";
const CONTRACT_DIGESTS: &[(&str, &str)] = &[
    (
        "contracts/vqro-extension-service/world.wit",
        "625f909dcd26c714e94b0361e4c3fde969c792e0497aaf7477e08f4f73f22daf",
    ),
    (
        "contracts/api/vqro-service-v1.schema.json",
        "3c2712d8b92b4908a40b92360e7fe089e914a88b1cb07640dc8d5c5d7129bdbc",
    ),
    (
        "contracts/api/host-terminals-v1.schema.json",
        "3b0a9fff414d1f2b0ca57b8e26910d4da9ec11a5fe30a7704a4a3acc74680449",
    ),
    (
        "contracts/api/host-document-render-v2.schema.json",
        "a500fef482259ce90772f9e677c3b9a943d006cfd0a75abe7915cf1bc2994776",
    ),
    (
        "contracts/api/host-document-v2.schema.json",
        "1c433b3b25ca1703204ec51f3f65f02541f2fcd02568105d34da53731d8bef52",
    ),
    (
        "contracts/api/host-document-v2.md",
        "cd7079f829f9af4b5e4e943c3f34ad77911f8dfc43fb65855e5a07c8c5aa55b1",
    ),
    (
        "contracts/fixtures/host-document-v2/valid.json",
        "95018a287650b5de06691b200a091643eb5130b0387918812fddadb07e016163",
    ),
    (
        "contracts/fixtures/host-document-v2/invalid-action.json",
        "10d6a80150e18d0b18d39e6c1f76ea0ed38a4d76f8d961f653a94d18520a637f",
    ),
    (
        "contracts/fixtures/host-document-v2/invalid-unknown-field.json",
        "2cf6c0d971e06925c553f893a30af0c33049d79b929204a5990199de0a15197f",
    ),
    (
        "contracts/fixtures/host-document-v2/invalid-unreachable.json",
        "020e74c6f94c63f9f6399d2deaf71956877edebb06dd943a18a98c0ba1a8e100",
    ),
    (
        "contracts/fixtures/host-terminals-v1/fingerprint-vector.json",
        "ace75ef459e96136563ee3cc747d0e2537a8f5cadd0f8af41a95f4ccf3597105",
    ),
    (
        "contracts/PROVENANCE.md",
        "96aca18d936ea2ad44361d6eccd4fa9d331711bed1f75349fdf21b99606231f2",
    ),
];

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
    assert_eq!(package_sha256, CANDIDATE_PACKAGE_SHA256);
    assert_eq!(
        sha256(&archive_entry(&package_bytes, PACKAGE_MANIFEST_FILE)),
        CANDIDATE_MANIFEST_SHA256
    );
    assert_eq!(
        sha256(&archive_entry(&package_bytes, "services/collections.wasm")),
        CANDIDATE_COMPONENT_SHA256
    );
    for (path, digest) in CONTRACT_DIGESTS {
        assert_eq!(
            sha256(&archive_entry(&package_bytes, path)),
            *digest,
            "packaged contract changed: {path}"
        );
    }
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
    assert_eq!(service.provides, ["host_document"]);
    assert_eq!(service.capabilities, ["host.terminals.read"]);
    assert_eq!(
        validated.descriptor.manifest_sha256,
        CANDIDATE_MANIFEST_SHA256
    );
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
    assert_eq!(dispatcher.methods().len(), 1);
    assert!(dispatcher.methods().contains("host.document.render"));
    dispatcher.stop(RuntimeStopReason::Requested);
    drop(dispatcher);

    run_authoritative_document_shadow(&installed, trusted_curated.clone());

    assert_package_mutations_are_rejected(&package_bytes, &package_sha256);
    overwrite(
        &Path::new(&installed.immutable_root).join("services/collections.wasm"),
        b"mutated installed component",
    );
    assert!(matches!(
        ComponentRuntimeFactory::installed(&installed, "collections", "vqro.collections"),
        Err(RuntimeError::Configuration(message)) if message.contains("installed package file changed")
    ));

    cleanup(root);
}

fn run_authoritative_document_shadow(
    installed: &crate::extension_package::InstalledPackageV1,
    provenance: PackageProvenanceV1,
) {
    const SERVICE_GENERATION: u64 = 7;
    const PROVIDER_GENERATION: u64 = 9;
    const RUNTIME_GENERATION: u64 = 8;

    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = crate::app::App::new(
        &crate::config::Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    app.state.workspaces = vec![crate::workspace::Workspace::test_new(
        "collections-shadow-gate",
    )];
    app.state.ensure_test_terminals();
    app.marketplace_state.packages.revision = 17;
    app.marketplace_state.packages.packages.insert(
        "vqro.collections".into(),
        crate::extension_package::PackageLifecycleRecordV1 {
            package_id: "vqro.collections".into(),
            source: provenance,
            previous_source: None,
            namespaces: vec!["vqro.collections".into()],
            revision: 19,
            status: crate::extension_package::PackageLifecycleStatusV1::Enabled,
            active: Some(crate::extension_package::PackageArtifactPointerV1 {
                package_sha256: CANDIDATE_PACKAGE_SHA256.into(),
                version: "0.0.0".into(),
                runtime_generation: RUNTIME_GENERATION,
            }),
            previous: None,
            activation_error: None,
            pending_activation: None,
            retained_data: false,
        },
    );
    let binding = crate::extension_host_state::HostProviderBinding::from_installed(
        installed,
        "collections",
        "vqro.collections",
    )
    .expect("installed candidate must create the exact provider binding");
    let workspace_id = app.state.workspaces[0].id.clone();
    let tab_id = app.state.workspaces[0].tabs[0].id.clone();
    let terminal_id = app.state.workspaces[0].tabs[0]
        .panes
        .values()
        .next()
        .expect("gate tab has one pane")
        .attached_terminal_id
        .clone();
    let legacy_state = app.extension_state.clone();
    let legacy_document_namespaces = app
        .host_document_services
        .namespaces()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(!app.extension_state_transaction_pending);
    assert!(app.collections_shadow_documents.is_empty());

    let canceled = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(5);
    let work = app
        .begin_document_shadow_audit(
            &binding,
            workspace_id,
            tab_id,
            SERVICE_GENERATION,
            PROVIDER_GENERATION,
            RUNTIME_GENERATION,
            1,
            1,
            deadline,
            Arc::clone(&canceled),
        )
        .expect("App must atomically stage exact candidate authority and audit ticket");
    let params: crate::host_document_v2::HostDocumentRenderParamsV2 =
        serde_json::from_value(work.request.params.clone()).expect("decode host-authored params");
    let attempt_id = work.attempt_id;
    let observer = work.observer.clone();
    let factory = RuntimeFactory::installed(installed, "collections", "vqro.collections")
        .expect("installed candidate must enter the runtime factory");
    let worker = std::thread::spawn(move || {
        let mut dispatcher =
            ExtensionRuntimeDispatcher::start(&factory, RUNTIME_GENERATION, canceled.as_ref())
                .expect("start exact candidate dispatcher");
        let result = dispatcher.invoke_with_host_reads(
            "host.document.render",
            work.request.params,
            RUNTIME_GENERATION,
            work.deadline,
            canceled,
            work.authorization,
        );
        dispatcher.stop(RuntimeStopReason::Requested);
        result
    });

    let actor_deadline = Instant::now() + Duration::from_secs(5);
    let mut actor_turns = 0_usize;
    while !worker.is_finished() {
        assert!(Instant::now() < actor_deadline, "candidate worker stalled");
        match app.event_rx.try_recv() {
            Ok(event) => {
                let is_terminal_call = matches!(
                    &event,
                    crate::events::AppEvent::ExtensionHostTerminalsCall(_)
                );
                app.handle_internal_event(event);
                actor_turns += usize::from(is_terminal_call);
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                panic!("App event channel disconnected during candidate invocation");
            }
        }
    }
    let result = worker.join().expect("candidate worker must not panic");
    assert_eq!(
        actor_turns, 2,
        "capture and validation must each use one App turn"
    );
    let observed = observer
        .exactly_one()
        .expect("candidate must make exactly one accepted terminal snapshot call");
    let value = result.expect("actual candidate invocation must return a document");
    let returned: crate::host_document_v2::HostDocumentV2 = serde_json::from_value(value.clone())
        .expect("candidate result must be canonical host.document.v2");
    assert_eq!(returned.roots, ["node-000"]);
    assert_eq!(returned.nodes.len(), 1);
    assert!(matches!(
        &returned.nodes[0],
        crate::host_document_v2::DocumentNodeV2::TerminalSlot { terminal_id: returned_id, .. }
            if returned_id == terminal_id.as_str()
    ));
    assert_eq!(
        returned.dependencies,
        [crate::host_document_v2::terminal_snapshot_dependency(
            observed.tab_id.as_str(),
            &observed.fingerprint_sha256,
            observed.lease_fence.provider_generation,
        )
        .expect("construct exact observed dependency")]
    );

    app.finalize_document_shadow_audit(attempt_id, Ok(value))
        .expect("final App turn must atomically accept the audited document");
    assert_eq!(
        app.document_shadow_cached_for_test(&params),
        Some(&returned),
        "only the exact audited document may enter quarantine"
    );
    assert_eq!(
        app.extension_state, legacy_state,
        "legacy state authority changed"
    );
    assert_eq!(
        app.host_document_services
            .namespaces()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        legacy_document_namespaces
    );
    assert!(!app.extension_state_transaction_pending);
    assert!(app.collections_shadow_documents.is_empty());
}

fn assert_candidate_checkout(checkout: &Path) {
    let head = command_output(checkout, "git", &["rev-parse", "HEAD"]);
    assert_eq!(
        head.trim(),
        CANDIDATE_COMMIT,
        "candidate checkout is not pinned"
    );
    let status = command_output(
        checkout,
        "git",
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    assert!(status.is_empty(), "candidate checkout is dirty: {status}");
}

fn assert_contract_provenance(checkout: &Path) {
    let attributes = std::fs::read(checkout.join(".gitattributes"))
        .expect("read candidate checkout normalization policy");
    assert_eq!(attributes, CANDIDATE_GITATTRIBUTES);
    for (path, digest) in CONTRACT_DIGESTS {
        let bytes = std::fs::read(checkout.join(path)).expect("read pinned candidate contract");
        assert_eq!(
            sha256(&bytes),
            *digest,
            "candidate contract digest changed: {path}"
        );
    }
    let candidate_wit = std::fs::read(checkout.join("contracts/vqro-extension-service/world.wit"))
        .expect("read candidate WIT snapshot");
    assert_eq!(
        candidate_wit,
        include_bytes!("../../wit/vqro-extension-service/world.wit")
    );
}

fn build_candidate(checkout: &Path, output: &Path) {
    let status = Command::new("cargo")
        .current_dir(checkout)
        .args(["run", "--locked", "-p", "xtask", "--", "package"])
        .arg(output)
        .status()
        .expect("run candidate deterministic package builder");
    assert!(status.success(), "candidate package construction failed");
    let verify = Command::new("cargo")
        .current_dir(checkout)
        .args(["run", "--locked", "-p", "xtask", "--", "verify"])
        .arg(output)
        .status()
        .expect("run candidate package verifier");
    assert!(verify.success(), "candidate package verification failed");
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

    for selected in [
        "contracts/PROVENANCE.md",
        "contracts/api/host-document-v2.schema.json",
    ] {
        let contract_mutation = mutate_archive_entry(original, selected, |bytes| bytes[0] ^= 1);
        assert!(validate_package_bytes(&contract_mutation, None).is_err());
    }

    let forged_paths = std::iter::once(PACKAGE_MANIFEST_FILE)
        .chain(std::iter::once("services/collections.wasm"))
        .chain(CONTRACT_DIGESTS.iter().map(|(path, _)| *path));
    for selected in forged_paths {
        let forge = || mutate_archive_entry_rechecksummed(original, selected);
        let first = forge();
        assert_eq!(
            first,
            forge(),
            "forged archive must be deterministic: {selected}"
        );
        let unpinned = validate_package_bytes(&first, None).unwrap_or_else(|error| {
            panic!("valid rechecksummed forge was internally rejected for {selected}: {error}")
        });
        assert_ne!(unpinned.descriptor.package_sha256, digest);
        assert_eq!(unpinned.descriptor.package_sha256, sha256(&first));
        assert_eq!(
            validate_package_bytes(&first, Some(digest)).unwrap_err(),
            "package digest mismatch",
            "rejection must come from the hard outer package pin: {selected}"
        );
    }
}

fn archive_entry(archive_bytes: &[u8], selected: &str) -> Vec<u8> {
    let mut archive = tar::Archive::new(archive_bytes);
    archive
        .entries()
        .expect("read candidate archive")
        .map(|entry| entry.expect("read candidate entry"))
        .find_map(|mut entry| {
            let matches = entry.path().ok()?.to_str()? == selected;
            matches.then(|| {
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut bytes).expect("read archive entry");
                bytes
            })
        })
        .unwrap_or_else(|| panic!("missing candidate archive entry: {selected}"))
}

fn mutate_archive_entry_rechecksummed(archive_bytes: &[u8], selected: &str) -> Vec<u8> {
    let mut archive = tar::Archive::new(archive_bytes);
    let mut entries = archive
        .entries()
        .expect("read candidate archive")
        .map(|entry| {
            let mut entry = entry.expect("read candidate entry");
            let path = entry.path().unwrap().to_str().unwrap().to_owned();
            let mode = entry.header().mode().unwrap();
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut bytes).unwrap();
            (path, bytes, mode)
        })
        .collect::<Vec<_>>();
    let selected_entry = entries
        .iter_mut()
        .find(|(path, _, _)| path == selected)
        .unwrap_or_else(|| panic!("missing forged archive entry: {selected}"));
    match selected {
        PACKAGE_MANIFEST_FILE => {
            let manifest = std::str::from_utf8(&selected_entry.1).expect("manifest is UTF-8");
            let changed = manifest.replacen("version = \"0.0.0\"", "version = \"0.0.1\"", 1);
            assert_ne!(changed, manifest, "manifest version fixture changed");
            selected_entry.1 = changed.into_bytes();
        }
        "services/collections.wasm" => {
            // Append a valid custom section: id=0, payload length=9, name length=8.
            // Custom sections preserve component structure and execution semantics.
            selected_entry.1.extend_from_slice(&[0x00, 0x09, 0x08]);
            selected_entry.1.extend_from_slice(b"m3-forge");
        }
        _ => {
            // JSON, Markdown, and WIT contract/provenance assets all admit trailing LF bytes.
            selected_entry.1.push(b'\n');
        }
    }
    let mut inventory_entries = entries
        .iter()
        .filter(|(path, _, _)| path != PACKAGE_CHECKSUM_FILE)
        .map(|(path, bytes, _)| (path.as_str(), sha256(bytes)))
        .collect::<Vec<_>>();
    inventory_entries.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    let inventory = inventory_entries
        .into_iter()
        .map(|(path, digest)| format!("{digest}  {path}\n"))
        .collect::<String>();
    entries
        .iter_mut()
        .find(|(path, _, _)| path == PACKAGE_CHECKSUM_FILE)
        .expect("checksum inventory exists")
        .1 = inventory.into_bytes();
    encode_archive(entries)
}

fn encode_archive(entries: Vec<(String, Vec<u8>, u32)>) -> Vec<u8> {
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
                .expect("encode archive");
        }
        builder.finish().expect("finish archive");
    }
    output
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
