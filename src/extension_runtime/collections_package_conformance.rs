use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::{ExtensionRuntimeDispatcher, RuntimeFactory, RuntimeStopReason};
use crate::extension_package::{
    install_local_file, validate_package_file, PackageProvenanceV1, PackageRuntimeV1,
};

const CANDIDATE_COMMIT: &str = "f3f4d2f8ce1dce8c319bf4c4d0e7224850afa868";
const CANDIDATE_REPOSITORY: &str = "https://github.com/ttenneb/vqro-extension-collections.git";
const CANDIDATE_PACKAGE_PATH: &str = "dist/vqro-collections.vqrox";
const CANDIDATE_PACKAGE_SHA256: &str =
    "067726c3ab83b44485a28926a1fd485a9ab05d1bd807382e6388fb5457299676";
const CANDIDATE_MANIFEST_SHA256: &str =
    "34751e197b31206ed2834a6243cd5ae27ecb8e1b44a57bb6594d55f724209385";
const CANDIDATE_COMPONENT_SHA256: &str =
    "a7f4c30b3d01298249a516f6d7f1d813e638c1383eff6435e70e72d6c55b65f7";
const ARTIFACT_GENERATION: u64 = 8;

#[test]
#[ignore = "requires the exact external candidate checkout; run just collections-package-conformance"]
fn collections_candidate_passes_authoritative_host_gate() {
    let checkout = std::env::var_os("VQRO_COLLECTIONS_CHECKOUT")
        .map(PathBuf::from)
        .expect("VQRO_COLLECTIONS_CHECKOUT must point to the exact Collections candidate checkout");
    assert_candidate_checkout(&checkout);

    let package_path = checkout.join(CANDIDATE_PACKAGE_PATH);
    let package_bytes = std::fs::read(&package_path).expect("read committed candidate artifact");
    assert_eq!(sha256(&package_bytes), CANDIDATE_PACKAGE_SHA256);
    let package = validate_package_file(&package_path, Some(CANDIDATE_PACKAGE_SHA256))
        .expect("host package validation must accept the committed artifact");
    assert_eq!(
        package.descriptor.manifest_sha256,
        CANDIDATE_MANIFEST_SHA256
    );
    assert_eq!(package.descriptor.manifest.id, "vqro.collections");
    assert_eq!(package.descriptor.manifest.namespaces, ["vqro.collections"]);
    assert_eq!(package.descriptor.manifest.services.len(), 1);
    let service = &package.descriptor.manifest.services[0];
    assert_eq!(service.id, "collections");
    assert_eq!(
        service.capabilities,
        ["host.state.read", "host.terminals.read"]
    );
    assert!(matches!(
        &service.runtime,
        PackageRuntimeV1::Component { component, world }
            if component == "services/collections.wasm"
                && world == "vqro:extension/service@1.0.0"
    ));
    assert_eq!(
        package
            .descriptor
            .files
            .iter()
            .find(|file| file.path == "services/collections.wasm")
            .map(|file| file.sha256.as_str()),
        Some(CANDIDATE_COMPONENT_SHA256)
    );

    let root = temporary_root();
    let provenance = exact_git_provenance();
    let installed = install_local_file(
        &package_path,
        &root.join("store"),
        Some(CANDIDATE_PACKAGE_SHA256),
        provenance.clone(),
        &std::collections::BTreeMap::new(),
        None,
    )
    .expect("the exact production pin must authorize its reserved namespace");

    let (mut app, expected_container, expected_archived) = staged_candidate_app(provenance);
    let state_before = app.extension_state.clone();
    let installed_for_plans = installed.clone();
    let slot = crate::extension_candidate::PackageCandidateSlot::spawn(
        installed,
        ARTIFACT_GENERATION,
        app.event_tx.clone(),
    )
    .expect("spawn bounded package candidate bootstrap");
    app.extension_package_services
        .insert("vqro.collections".into(), slot);

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut state_calls = 0;
    let mut terminal_calls = 0;
    while app.collections_candidate_audits.cached_len() != 1
        || app.collections_candidate_audits.action_cached_len() != 1
    {
        assert!(Instant::now() < deadline, "candidate audit timed out");
        match app.event_rx.try_recv() {
            Ok(crate::events::AppEvent::ExtensionPackageCandidateReady(result)) => {
                assert!(
                    result.result.is_ok(),
                    "candidate bootstrap failed: {:?}",
                    result.result.as_ref().err()
                );
                app.handle_extension_package_candidate_ready(*result);
                if app.collections_candidate_audits.pending_len() == 0
                    && app.collections_candidate_audits.cached_len() == 0
                    && app.collections_candidate_audits.action_cached_len() == 0
                {
                    let workspace_id = app.state.workspaces[0].id.clone();
                    let tab_id = app.state.workspaces[0].tabs[0].id.clone();
                    app.schedule_collections_candidate(workspace_id, tab_id, 999)
                        .expect("ready exact candidate must schedule");
                }
            }
            Ok(crate::events::AppEvent::ExtensionPackageCandidateFinished(result)) => {
                app.finalize_collections_candidate(result.attempt_id, result.result)
                    .expect("exact candidate completion must pass the profile audit");
            }
            Ok(event) => {
                match &event {
                    crate::events::AppEvent::ExtensionHostStateCall(_) => state_calls += 1,
                    crate::events::AppEvent::ExtensionHostTerminalsCall(_) => terminal_calls += 1,
                    _ => {}
                }
                app.handle_internal_event(event);
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                panic!("App event channel disconnected during candidate audit")
            }
        }
        assert!(
            app.collections_candidate_audits.pending_len() <= 1,
            "profile and action audits must be sequenced so actions cannot race the profile"
        );
    }

    assert_eq!(
        state_calls, 2,
        "profile and action declaration invocations make one state read each"
    );
    assert_eq!(
        terminal_calls, 4,
        "two terminal snapshots each use capture and validation App turns"
    );
    assert_eq!(app.collections_candidate_audits.pending_len(), 0);
    assert_eq!(app.extension_state, state_before);
    assert_eq!(
        app.host_document_services
            .source(crate::collections_extension_contract::COLLECTIONS_NAMESPACE),
        Some(crate::api::schema::HostDocumentSourceKind::TerminalGroups),
        "candidate admission must not publish package authority"
    );
    let durable = app.extension_state.snapshot("vqro.collections").unwrap();
    assert!(
        durable.values.is_empty(),
        "shadow import leaked into durable state"
    );
    let overlay = app
        .collections_import_overlay
        .state()
        .snapshot("vqro.collections")
        .unwrap();
    let value = overlay.values.get(&expected_container).unwrap();
    assert_eq!(value["label"], "Helpers");
    assert_eq!(value["archived_terminal_ids"][0], expected_archived);
    assert_shadow_action_plans(
        &mut app,
        &installed_for_plans,
        &expected_container,
        &expected_archived,
    );
    assert_eq!(app.extension_state, state_before);

    app.extension_package_services.remove("vqro.collections");
    app.revoke_host_invocation_authority_for_package("vqro.collections");
    assert_superseded_read_and_scope_pressure(&installed_for_plans);
    cleanup(root);
}

fn assert_superseded_read_and_scope_pressure(
    installed: &crate::extension_package::InstalledPackageV1,
) {
    let (mut app, _, _) = staged_candidate_app(exact_git_provenance());
    for _ in 1..8 {
        app.state.workspaces[0].test_add_tab(None);
    }
    app.state.ensure_test_terminals();
    // This is a non-publishing read characterization, not a cutover/migration.
    app.collections_mutations_blocked = true;
    let state_before = app.extension_state.clone();
    let workspace_id = app.state.workspaces[0].id.clone();
    let tab_id = app.state.workspaces[0].tabs[0].id.clone();
    app.extension_package_services.insert(
        "vqro.collections".into(),
        crate::extension_candidate::PackageCandidateSlot::spawn(
            installed.clone(),
            ARTIFACT_GENERATION,
            app.event_tx.clone(),
        )
        .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut held_read = false;
    let mut discarded_old = false;
    while app.collections_candidate_audits.cached_len() != 8
        || app.collections_candidate_audits.action_cached_len() != 8
    {
        assert!(
            Instant::now() < deadline,
            "supersession/queue pressure did not drain"
        );
        match app.event_rx.try_recv() {
            Ok(crate::events::AppEvent::ExtensionHostStateCall(request)) if !held_read => {
                // The real component is waiting for its first host read. Supersede its
                // audit while it is active, with four other requests already queued.
                held_read = true;
                let _ =
                    app.schedule_collections_candidate(workspace_id.clone(), tab_id.clone(), 999);
                app.handle_internal_event(crate::events::AppEvent::ExtensionHostStateCall(request));
            }
            Ok(crate::events::AppEvent::ExtensionPackageCandidateFinished(result)) => {
                if held_read && !discarded_old {
                    assert!(
                        result.result.is_ok(),
                        "superseding a read killed its component: {:?}",
                        result.result
                    );
                    app.handle_extension_package_candidate_finished(*result);
                    assert_eq!(
                        app.collections_candidate_audits.cached_len(),
                        0,
                        "obsolete component result was published"
                    );
                    discarded_old = true;
                } else {
                    app.handle_extension_package_candidate_finished(*result);
                }
            }
            Ok(event) => {
                app.handle_internal_event(event);
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("candidate event channel failed: {error}"),
        }
    }
    assert!(held_read && discarded_old);
    let current = app
        .collections_candidate_audits
        .profiles()
        .find(|profile| profile.document.scope.scope_id == tab_id.to_string())
        .unwrap();
    assert_eq!(current.document.revision, 999);
    assert_eq!(app.collections_candidate_audits.pending_len(), 0);
    assert_eq!(
        app.extension_state, state_before,
        "read characterization wrote policy"
    );
    app.extension_package_services.remove("vqro.collections");
    app.revoke_host_invocation_authority_for_package("vqro.collections");
}

fn assert_shadow_action_plans(
    app: &mut crate::app::App,
    installed: &crate::extension_package::InstalledPackageV1,
    container_id: &str,
    terminal_id: &str,
) {
    use crate::collections_extension_contract::{
        CollectionsActionDependencyV1, CollectionsActionInputV1, CollectionsActionInvocationV1,
        CollectionsActionKindV1, COLLECTIONS_ACTION_INVOCATION_CONTRACT_V1,
        COLLECTIONS_ACTION_PLAN_METHOD_V1,
    };

    let profile = app
        .collections_candidate_audits
        .first_profile()
        .expect("audited profile exists")
        .clone();
    let actions = app
        .collections_candidate_audits
        .first_actions()
        .expect("audited action declarations exist")
        .clone();
    let state = app
        .collections_import_overlay
        .state()
        .snapshot("vqro.collections")
        .expect("shadow state snapshot");
    let terminal_dependency = profile
        .document
        .dependencies
        .iter()
        .find(|dependency| dependency.contract == "host.terminals.v1")
        .expect("profile has terminal dependency");
    let fingerprint = terminal_dependency
        .scope_id
        .rsplit_once('/')
        .map(|(_, fingerprint)| fingerprint.to_string())
        .expect("terminal dependency carries a fingerprint");

    let label_action = actions
        .actions
        .iter()
        .find(|action| action.action == CollectionsActionKindV1::SetLabel)
        .expect("label action is declared");
    let label_invocation = CollectionsActionInvocationV1 {
        contract: COLLECTIONS_ACTION_INVOCATION_CONTRACT_V1.into(),
        ticket: "ticket:r2-label".into(),
        idempotency_key: "r2-set-label".into(),
        document_revision: actions.document_revision,
        actions_revision: actions.revision,
        action_id: label_action.id.clone(),
        expected_store_generation: state.store_generation,
        expected_namespace_revision: state.revision,
        dependency: CollectionsActionDependencyV1::Container {
            container_id: container_id.into(),
        },
        input: CollectionsActionInputV1::SetLabel {
            container_id: container_id.into(),
            label: Some("Renamed by shadow".into()),
        },
    };
    actions
        .validate_invocation(&label_invocation)
        .expect("label shadow invocation matches declarations");
    let label_plan = invoke_shadow_plan(app, installed, &label_invocation, 31);
    label_plan
        .validate_for(&label_invocation)
        .expect("label plan is one exact state CAS");

    let archive_action = actions
        .actions
        .iter()
        .find(|action| {
            action.action == CollectionsActionKindV1::SetArchived
                && action.id.ends_with(terminal_id)
        })
        .expect("archive action is declared");
    let archive_invocation = CollectionsActionInvocationV1 {
        contract: COLLECTIONS_ACTION_INVOCATION_CONTRACT_V1.into(),
        ticket: "ticket:r2-archive".into(),
        idempotency_key: "r2-set-archived".into(),
        document_revision: actions.document_revision,
        actions_revision: actions.revision,
        action_id: archive_action.id.clone(),
        expected_store_generation: state.store_generation,
        expected_namespace_revision: state.revision,
        dependency: CollectionsActionDependencyV1::TerminalMembership {
            container_id: container_id.into(),
            terminal_id: terminal_id.into(),
            terminal_snapshot_fingerprint_sha256: fingerprint,
            provider_generation: terminal_dependency.generation,
        },
        input: CollectionsActionInputV1::SetArchived {
            container_id: container_id.into(),
            terminal_id: terminal_id.into(),
            archived: false,
        },
    };
    actions
        .validate_invocation(&archive_invocation)
        .expect("archive shadow invocation matches declarations");
    let archive_plan = invoke_shadow_plan(app, installed, &archive_invocation, 32);
    archive_plan
        .validate_for(&archive_invocation)
        .expect("archive plan is one exact state CAS");

    assert_eq!(label_plan.state.key, container_id);
    assert_eq!(archive_plan.state.key, container_id);
    assert_ne!(label_plan.ticket, archive_plan.ticket);
    assert_eq!(
        COLLECTIONS_ACTION_PLAN_METHOD_V1,
        "vqro.collections.action.plan.v1"
    );
}

fn invoke_shadow_plan(
    app: &mut crate::app::App,
    installed: &crate::extension_package::InstalledPackageV1,
    invocation: &crate::collections_extension_contract::CollectionsActionInvocationV1,
    runtime_generation: u64,
) -> crate::collections_extension_contract::CollectionsEffectPlanV1 {
    let binding = crate::extension_host_state::HostProviderBinding::from_installed(
        installed,
        "collections",
        "vqro.collections",
    )
    .expect("exact action-plan binding");
    let record = &app.marketplace_state.packages.packages["vqro.collections"];
    let authorization = app
        .stage_host_read_authorization(
            &binding,
            crate::extension_host_invocation::HostReadInvocationPurpose {
                state: crate::extension_host_invocation::HostStateReadSource::CollectionsShadow,
                terminals: None,
                provisional_container: None,
            },
            ARTIFACT_GENERATION,
            record.revision,
            runtime_generation,
        )
        .expect("stage read-only shadow action authority");
    let factory = RuntimeFactory::installed(installed, "collections", "vqro.collections")
        .expect("action candidate runtime factory");
    let params = serde_json::to_value(invocation).expect("encode action invocation");
    let canceled = Arc::new(AtomicBool::new(false));
    let worker_canceled = Arc::clone(&canceled);
    let worker = std::thread::spawn(move || {
        let mut dispatcher = ExtensionRuntimeDispatcher::start(
            &factory,
            runtime_generation,
            worker_canceled.as_ref(),
        )
        .expect("start action candidate runtime");
        let result = dispatcher.invoke_with_host_reads(
            crate::collections_extension_contract::COLLECTIONS_ACTION_PLAN_METHOD_V1,
            params,
            runtime_generation,
            Instant::now() + Duration::from_secs(10),
            worker_canceled,
            authorization,
        );
        dispatcher.stop(RuntimeStopReason::Requested);
        result
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut state_calls = 0;
    while !worker.is_finished() {
        assert!(Instant::now() < deadline, "action plan worker timed out");
        match app.event_rx.try_recv() {
            Ok(event @ crate::events::AppEvent::ExtensionHostStateCall(_)) => {
                state_calls += 1;
                app.handle_internal_event(event);
            }
            Ok(event) => {
                app.handle_internal_event(event);
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                panic!("App event channel disconnected during action plan")
            }
        }
    }
    assert_eq!(
        state_calls, 1,
        "action planning must make exactly one state read"
    );
    let value = worker
        .join()
        .expect("action worker must not panic")
        .expect("action candidate must return a plan");
    serde_json::from_value(value).expect("decode typed action plan")
}

fn staged_candidate_app(provenance: PackageProvenanceV1) -> (crate::app::App, String, String) {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = crate::app::App::new(
        &crate::config::Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    let mut workspace = crate::workspace::Workspace::test_new("collections-r1-candidate");
    let root = workspace.tabs[0].layout.pane_ids()[0];
    let first_member = workspace.test_split(ratatui::layout::Direction::Vertical);
    let second_member = workspace.test_split(ratatui::layout::Direction::Horizontal);
    let collection_id = workspace.tabs[0]
        .create_collection_near(
            crate::layout::LayoutLeaf::Pane(root),
            ratatui::layout::Direction::Vertical,
            0.5,
            Some("Helpers".into()),
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
        .expect("select candidate terminal");
    workspace.tabs[0]
        .set_collection_member_archived(collection_id, first_member, true)
        .expect("archive candidate terminal in legacy policy");
    let archived_terminal = workspace.tabs[0]
        .terminal_id(first_member)
        .expect("candidate member has terminal")
        .to_string();
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
                version: "1.0.0".into(),
                runtime_generation: ARTIFACT_GENERATION,
            }),
            previous: None,
            activation_error: None,
            pending_activation: None,
            retained_data: false,
        },
    );
    (app, container_id, archived_terminal)
}

fn exact_git_provenance() -> PackageProvenanceV1 {
    PackageProvenanceV1::Git {
        url: CANDIDATE_REPOSITORY.into(),
        source_commit: CANDIDATE_COMMIT.into(),
        package_path: CANDIDATE_PACKAGE_PATH.into(),
        package_id: "vqro.collections".into(),
        version: "1.0.0".into(),
        package_sha256: CANDIDATE_PACKAGE_SHA256.into(),
        manifest_sha256: CANDIDATE_MANIFEST_SHA256.into(),
    }
}

fn assert_candidate_checkout(checkout: &Path) {
    let head = command_output(checkout, "git", &["rev-parse", "HEAD"]);
    assert_eq!(head.trim(), CANDIDATE_COMMIT, "candidate checkout drifted");
    let status = command_output(checkout, "git", &["status", "--porcelain"]);
    assert!(status.trim().is_empty(), "candidate checkout is dirty");
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
        "vqro-collections-r1-{}-{}",
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
        #[cfg(not(unix))]
        permissions.set_readonly(false);
        let _ = std::fs::set_permissions(path, permissions);
    }
    if root.exists() {
        make_writable(&root);
        let _ = std::fs::remove_dir_all(root);
    }
}
