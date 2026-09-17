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

const CANDIDATE_COMMIT: &str = "b590657b23866e36b07e3bbbaed5554e1be5a4fc";
const CANDIDATE_REPOSITORY: &str = "https://github.com/ttenneb/vqro-extension-collections.git";
const CANDIDATE_GITATTRIBUTES: &[u8] = b"* text=auto eol=lf\n";
const CANDIDATE_PACKAGE_SHA256: &str =
    "36230691235fe70ef21b06e1d94c98d9888bce68ed4587d3c979bf03f9d2980b";
const CANDIDATE_MANIFEST_SHA256: &str =
    "4d270e2646cde5764260e5781eb837c7964078731fb5981578b1d08e80957034";
const CANDIDATE_COMPONENT_SHA256: &str =
    "819963c8aeee9d3c5c1ba05cc3b4f6fd73ace14b030ffd55fbd2e1ac95b0c1bc";
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
        "contracts/api/host-state-v1.schema.json",
        "76f18212c69780c5100594598a3aa388ddf70e4d275461ca1cbab9b2418f77bd",
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
        "50acbfdb201b1a56006f47a8c6a6cdb20761b0bd1d6d8c7b0dd65a0ddf5d3832",
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
        "61bb9dab61ac377c971151fea67a7da5b26bd19e44a62aeac67b4317ddbca128",
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
    assert_eq!(
        service.capabilities,
        ["host.state.read", "host.terminals.read"]
    );
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CandidateScenario {
    Accept,
    StateRaceAfterSnapshot,
    TopologyRaceAfterCapture,
    AuthorityRaceBeforeFinalize,
    PersistenceBlockBeforeFinalize,
}

struct CandidateFixture {
    collection_id: crate::layout::CollectionId,
    container_id: String,
    archived_terminal_id: crate::terminal::TerminalId,
}

fn run_authoritative_document_shadow(
    installed: &crate::extension_package::InstalledPackageV1,
    provenance: PackageProvenanceV1,
) {
    run_candidate_scenario(installed, provenance.clone(), CandidateScenario::Accept);
    for scenario in [
        CandidateScenario::StateRaceAfterSnapshot,
        CandidateScenario::TopologyRaceAfterCapture,
        CandidateScenario::AuthorityRaceBeforeFinalize,
        CandidateScenario::PersistenceBlockBeforeFinalize,
    ] {
        run_candidate_scenario(installed, provenance.clone(), scenario);
    }
}

fn run_candidate_scenario(
    installed: &crate::extension_package::InstalledPackageV1,
    provenance: PackageProvenanceV1,
    scenario: CandidateScenario,
) {
    const SERVICE_GENERATION: u64 = 7;
    const PROVIDER_GENERATION: u64 = 9;
    const RUNTIME_GENERATION: u64 = 8;

    let (mut app, binding, fixture) = staged_candidate_app(installed, provenance);
    let workspace_id = app.state.workspaces[0].id.clone();
    let tab_id = app.state.workspaces[0].tabs[0].id.clone();
    let seeded_state = app.extension_state.clone();
    let legacy_policy = app.state.workspaces[0].tabs[0]
        .collection(fixture.collection_id)
        .map(|collection| {
            (
                collection.label().map(str::to_owned),
                collection.archived_members().collect::<Vec<_>>(),
            )
        })
        .expect("candidate fixture collection exists");
    assert_eq!(legacy_policy, (None, Vec::new()));
    let legacy_document_namespaces = app
        .host_document_services
        .namespaces()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let render_was_pending = app.render_dirty.is_pending();
    let last_render_at = app.last_render_at;
    let last_presentation_at = app.last_presentation_at;
    let full_redraw_pending = app.full_redraw_pending;
    let document_watermarks = app.host_document_request_watermarks.clone();
    let mut document_retry_keys = app
        .host_document_retry_snapshots
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    document_retry_keys.sort();
    assert!(app.extension_state_path.is_none());
    assert!(!app.extension_state_persistence_blocked);
    assert!(!app.extension_state_transaction_pending);
    assert!(app.collections_shadow_documents.is_empty());

    let canceled = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_secs(30);
    let work = app
        .begin_collections_composite_shadow_audit_for_test(
            &binding,
            workspace_id.clone(),
            tab_id.clone(),
            SERVICE_GENERATION,
            PROVIDER_GENERATION,
            RUNTIME_GENERATION,
            1,
            1,
            deadline,
            Arc::clone(&canceled),
        )
        .expect("App must stage exact candidate composite authority and audit ticket");
    let params: crate::host_document_v2::HostDocumentRenderParamsV2 =
        serde_json::from_value(work.request.params.clone()).expect("decode host-authored params");
    let attempt_id = work.attempt_id;
    let observer = work.observer.clone();
    let staged_authority = Arc::clone(&work.authorization.authority);
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

    let actor_deadline = Instant::now() + Duration::from_secs(30);
    let mut actor_requests = Vec::new();
    while !worker.is_finished() {
        assert!(Instant::now() < actor_deadline, "candidate worker stalled");
        match app.event_rx.try_recv() {
            Ok(event) => {
                let actor = match &event {
                    crate::events::AppEvent::ExtensionHostStateCall(_) => Some("state.snapshot"),
                    crate::events::AppEvent::ExtensionHostTerminalsCall(request) => {
                        Some(match &request.method {
                            crate::extension_host_terminals::HostTerminalsActorMethod::Capture => {
                                "terminals.capture"
                            }
                            crate::extension_host_terminals::HostTerminalsActorMethod::Validate => {
                                "terminals.validate"
                            }
                        })
                    }
                    _ => None,
                };
                if let Some(actor) = actor {
                    app.handle_internal_event(event);
                    actor_requests.push(actor);
                    if scenario == CandidateScenario::StateRaceAfterSnapshot
                        && actor == "state.snapshot"
                    {
                        mutate_candidate_policy_state(&mut app, "raced label");
                    }
                    if scenario == CandidateScenario::TopologyRaceAfterCapture
                        && actor == "terminals.capture"
                    {
                        mutate_candidate_topology(&mut app);
                    }
                }
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
    let value = result.expect("actual candidate invocation must return a document");
    assert_eq!(
        actor_requests,
        ["state.snapshot", "terminals.capture", "terminals.validate"],
        "the candidate must use exactly three ordered App actor requests"
    );
    let observed = observer
        .exact_state_then_terminals()
        .expect("candidate must make exactly state.snapshot then terminals.snapshot");
    let returned: crate::host_document_v2::HostDocumentV2 = serde_json::from_value(value.clone())
        .expect("candidate result must be canonical host.document.v2");
    let state_before_finalization = app.extension_state.clone();

    if scenario == CandidateScenario::Accept {
        assert_candidate_document(&returned, &params, &observed, &fixture);
    }
    if scenario == CandidateScenario::AuthorityRaceBeforeFinalize {
        app.revoke_host_invocation_authority("vqro.collections", "collections");
    }
    if scenario == CandidateScenario::PersistenceBlockBeforeFinalize {
        app.extension_state_persistence_blocked = true;
    }

    let finalization = app.finalize_document_shadow_audit(attempt_id, Ok(value));
    if scenario == CandidateScenario::Accept {
        finalization.expect("final App turn must atomically accept the audited document");
        assert_eq!(
            app.document_shadow_cached_for_test(&params),
            Some(&returned),
            "only the exact audited document may enter quarantine"
        );
        assert_eq!(app.document_shadow_audits.pending_len(), 0);
        assert_eq!(app.document_shadow_audits.cached_len(), 1);
        assert_eq!(app.extension_state, seeded_state);
    } else {
        assert!(
            finalization.is_err(),
            "{scenario:?} must reject final acceptance"
        );
        assert_eq!(app.document_shadow_audits.pending_len(), 0);
        assert_eq!(app.document_shadow_audits.cached_len(), 0);
    }
    assert_eq!(
        app.extension_state, state_before_finalization,
        "candidate finalization mutated durable state"
    );
    if scenario == CandidateScenario::AuthorityRaceBeforeFinalize {
        assert!(!app
            .host_invocation_authorities
            .contains_key(&("vqro.collections".into(), "collections".into())));
    } else {
        assert!(app
            .host_invocation_authorities
            .get(&("vqro.collections".into(), "collections".into()))
            .is_some_and(|authority| Arc::ptr_eq(authority, &staged_authority)));
    }

    let policy_after = app.state.workspaces[0].tabs[0]
        .collection(fixture.collection_id)
        .map(|collection| {
            (
                collection.label().map(str::to_owned),
                collection.archived_members().collect::<Vec<_>>(),
            )
        })
        .expect("candidate fixture collection remains present");
    assert_eq!(
        policy_after, legacy_policy,
        "legacy collection policy changed"
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
    assert_eq!(app.host_document_request_watermarks, document_watermarks);
    let mut current_retry_keys = app
        .host_document_retry_snapshots
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    current_retry_keys.sort();
    assert_eq!(current_retry_keys, document_retry_keys);
    assert_eq!(app.render_dirty.is_pending(), render_was_pending);
    assert_eq!(app.last_render_at, last_render_at);
    assert_eq!(app.last_presentation_at, last_presentation_at);
    assert_eq!(app.full_redraw_pending, full_redraw_pending);
    assert!(app.extension_state_path.is_none());
    assert_eq!(
        app.extension_state_persistence_blocked,
        scenario == CandidateScenario::PersistenceBlockBeforeFinalize
    );
}

fn staged_candidate_app(
    installed: &crate::extension_package::InstalledPackageV1,
    provenance: PackageProvenanceV1,
) -> (
    crate::app::App,
    crate::extension_host_state::HostProviderBinding,
    CandidateFixture,
) {
    const RUNTIME_GENERATION: u64 = 8;
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = crate::app::App::new(
        &crate::config::Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    let mut workspace = crate::workspace::Workspace::test_new("collections-shadow-gate");
    let root = workspace.tabs[0].layout.pane_ids()[0];
    let first_member = workspace.test_split(ratatui::layout::Direction::Vertical);
    let second_member = workspace.test_split(ratatui::layout::Direction::Horizontal);
    let collection_id = workspace.tabs[0]
        .create_collection_near(
            crate::layout::LayoutLeaf::Pane(root),
            ratatui::layout::Direction::Vertical,
            0.5,
            None,
        )
        .expect("create neutral candidate collection");
    workspace.tabs[0]
        .collect_tiled_pane(first_member, collection_id)
        .expect("collect first candidate terminal");
    workspace.tabs[0]
        .collect_tiled_pane(second_member, collection_id)
        .expect("collect second candidate terminal");
    workspace.tabs[0]
        .select_collection_member(collection_id, second_member)
        .expect("select second candidate terminal");
    let archived_terminal_id = workspace.tabs[0]
        .terminal_id(first_member)
        .expect("first candidate member has terminal")
        .clone();
    let container_id = crate::terminal_container::format_terminal_container_id(
        crate::terminal_container::TerminalContainerId::from(collection_id),
    );
    app.state.workspaces = vec![workspace];
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
    let store_generation = app.extension_state.store_generation;
    app.extension_state
        .transact(&crate::extension_state::ExtensionStateTransactionV1 {
            contract: crate::extension_state::EXTENSION_STATE_CONTRACT_V1.into(),
            namespace: "vqro.collections".into(),
            expected_store_generation: store_generation,
            expected_revision: 0,
            idempotency_key: "seed-collections-policy".into(),
            operations: vec![crate::extension_state::ExtensionStateOperationV1 {
                key: container_id.clone(),
                value: Some(serde_json::json!({
                    "archived_terminal_ids": [archived_terminal_id.to_string()],
                    "container_id": container_id,
                    "label": "Helpers",
                    "schema": "vqro.collections.policy",
                    "schema_version": 1
                })),
            }],
        })
        .expect("seed canonical Collections policy directly in durable test state");
    let binding = crate::extension_host_state::HostProviderBinding::from_installed(
        installed,
        "collections",
        "vqro.collections",
    )
    .expect("installed candidate must create the exact provider binding");
    (
        app,
        binding,
        CandidateFixture {
            collection_id,
            container_id,
            archived_terminal_id,
        },
    )
}

fn mutate_candidate_policy_state(app: &mut crate::app::App, label: &str) {
    let snapshot = app.extension_state.snapshot("vqro.collections").unwrap();
    let (key, mut value) = snapshot.values.into_iter().next().unwrap();
    value["label"] = serde_json::Value::String(label.into());
    app.extension_state
        .transact(&crate::extension_state::ExtensionStateTransactionV1 {
            contract: crate::extension_state::EXTENSION_STATE_CONTRACT_V1.into(),
            namespace: "vqro.collections".into(),
            expected_store_generation: snapshot.store_generation,
            expected_revision: snapshot.revision,
            idempotency_key: "race-collections-policy".into(),
            operations: vec![crate::extension_state::ExtensionStateOperationV1 {
                key,
                value: Some(value),
            }],
        })
        .unwrap();
}

fn mutate_candidate_topology(app: &mut crate::app::App) {
    let pane = app.state.workspaces[0].test_split(ratatui::layout::Direction::Vertical);
    let terminal = app.state.workspaces[0].tabs[0]
        .terminal_id(pane)
        .unwrap()
        .clone();
    app.state.terminals.insert(
        terminal.clone(),
        crate::terminal::TerminalState::new(terminal, "/".into()),
    );
}

fn assert_candidate_document(
    returned: &crate::host_document_v2::HostDocumentV2,
    params: &crate::host_document_v2::HostDocumentRenderParamsV2,
    observed: &crate::extension_host_invocation::CompositeSnapshotObservation,
    fixture: &CandidateFixture,
) {
    let mut dependencies = vec![
        crate::host_document_v2::host_state_snapshot_dependency(
            &observed.state.contract,
            &observed.state.store_id,
            observed.state.store_generation,
            &observed.state.namespace,
            observed.state.revision,
        )
        .unwrap(),
        crate::host_document_v2::terminal_snapshot_dependency(
            observed.terminals.tab_id.as_str(),
            &observed.terminals.fingerprint_sha256,
            observed.terminals.lease_fence.provider_generation,
        )
        .unwrap(),
    ];
    dependencies.sort();
    let mut roots = Vec::new();
    let mut nodes = Vec::new();
    let mut next_node = 0_usize;
    for item in &observed.terminals.layout {
        match item {
            crate::extension_host_terminals::HostTerminalLayoutItemV1::Tiled { terminal_id } => {
                let id = format!("node-{next_node:03}");
                next_node += 1;
                roots.push(id.clone());
                nodes.push(crate::host_document_v2::DocumentNodeV2::TerminalSlot {
                    id,
                    accessibility: crate::host_document_v2::DocumentAccessibilityV2 {
                        name: "Terminal".into(),
                        description: None,
                    },
                    terminal_id: terminal_id.to_string(),
                });
            }
            crate::extension_host_terminals::HostTerminalLayoutItemV1::Container {
                container_id,
                terminal_ids,
                ..
            } => {
                assert_eq!(container_id, &fixture.container_id);
                let group_id = format!("node-{next_node:03}");
                next_node += 1;
                roots.push(group_id.clone());
                let mut children = Vec::new();
                let label_id = format!("node-{next_node:03}");
                next_node += 1;
                children.push(label_id.clone());
                let mut child_nodes = vec![crate::host_document_v2::DocumentNodeV2::Text {
                    id: label_id,
                    accessibility: crate::host_document_v2::DocumentAccessibilityV2 {
                        name: "Collection label".into(),
                        description: None,
                    },
                    text: "Helpers".into(),
                }];
                for terminal_id in terminal_ids {
                    let id = format!("node-{next_node:03}");
                    next_node += 1;
                    children.push(id.clone());
                    child_nodes.push(crate::host_document_v2::DocumentNodeV2::TerminalSlot {
                        id,
                        accessibility: crate::host_document_v2::DocumentAccessibilityV2 {
                            name: if terminal_id == &fixture.archived_terminal_id {
                                "Archived terminal".into()
                            } else {
                                "Terminal".into()
                            },
                            description: None,
                        },
                        terminal_id: terminal_id.to_string(),
                    });
                }
                nodes.push(crate::host_document_v2::DocumentNodeV2::Group {
                    id: group_id,
                    accessibility: crate::host_document_v2::DocumentAccessibilityV2 {
                        name: "Collection".into(),
                        description: None,
                    },
                    children,
                });
                nodes.extend(child_nodes);
            }
        }
    }
    let expected = crate::host_document_v2::HostDocumentV2 {
        contract: crate::host_document_v2::HOST_DOCUMENT_CONTRACT_V2.into(),
        producer: params.producer.clone(),
        scope: params.scope.clone(),
        revision: params.requested_revision,
        dependencies,
        roots,
        nodes,
    };
    assert_eq!(returned, &expected, "candidate document semantics changed");

    let group_index = returned
        .nodes
        .iter()
        .position(|node| {
            matches!(node,
            crate::host_document_v2::DocumentNodeV2::Group { accessibility, .. }
                if accessibility.name == "Collection")
        })
        .expect("candidate document contains collection group");
    let crate::host_document_v2::DocumentNodeV2::Group { children, .. } =
        &returned.nodes[group_index]
    else {
        unreachable!()
    };
    assert_eq!(children.len(), 3);
    assert!(matches!(
        &returned.nodes[group_index + 1],
        crate::host_document_v2::DocumentNodeV2::Text { accessibility, text, .. }
            if accessibility.name == "Collection label" && text == "Helpers"
    ));
    let member_nodes = children
        .iter()
        .skip(1)
        .map(|id| {
            returned
                .nodes
                .iter()
                .find(|node| match node {
                    crate::host_document_v2::DocumentNodeV2::Group { id: node_id, .. }
                    | crate::host_document_v2::DocumentNodeV2::Text { id: node_id, .. }
                    | crate::host_document_v2::DocumentNodeV2::TerminalSlot {
                        id: node_id, ..
                    } => node_id == id,
                })
                .expect("group child node exists")
        })
        .collect::<Vec<_>>();
    assert!(member_nodes.iter().any(|node| matches!(node,
        crate::host_document_v2::DocumentNodeV2::TerminalSlot { accessibility, terminal_id, .. }
            if terminal_id == fixture.archived_terminal_id.as_str()
                && accessibility.name == "Archived terminal")));
    assert!(member_nodes.iter().any(|node| matches!(node,
        crate::host_document_v2::DocumentNodeV2::TerminalSlot { accessibility, terminal_id, .. }
            if terminal_id != fixture.archived_terminal_id.as_str()
                && accessibility.name == "Terminal")));
    let projected_terminals = returned
        .nodes
        .iter()
        .filter_map(|node| match node {
            crate::host_document_v2::DocumentNodeV2::TerminalSlot { terminal_id, .. } => {
                Some(terminal_id.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let expected_terminals = observed
        .terminals
        .layout
        .iter()
        .flat_map(|item| match item {
            crate::extension_host_terminals::HostTerminalLayoutItemV1::Tiled { terminal_id } => {
                vec![terminal_id.to_string()]
            }
            crate::extension_host_terminals::HostTerminalLayoutItemV1::Container {
                container_id,
                terminal_ids,
                ..
            } => {
                assert_eq!(container_id, &fixture.container_id);
                terminal_ids.iter().map(ToString::to_string).collect()
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(projected_terminals, expected_terminals);
    let encoded = serde_json::to_string(returned).unwrap();
    for forbidden in [
        "pane_id",
        "geometry",
        "selected_terminal_id",
        "action",
        "effect",
    ] {
        assert!(!encoded.contains(forbidden), "candidate leaked {forbidden}");
    }
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
