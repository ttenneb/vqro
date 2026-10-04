//! Bounded Spaces package invocation, exact-pin fencing, and audited publication.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use super::App;

const INVOCATION_TIMEOUT: Duration = Duration::from_secs(2);
const PROVIDER_PRIORITY: i32 = 100;

struct PendingSpacesCandidate {
    attempt_id: u64,
    snapshot: crate::extension_contract::HostSpacesV1,
    artifact_generation: u64,
    runtime_generation: u64,
    deadline: Instant,
    canceled: Arc<AtomicBool>,
}

pub(crate) struct SpacesCandidateWork {
    attempt_id: u64,
    params: serde_json::Value,
    deadline: Instant,
    canceled: Arc<AtomicBool>,
}

#[derive(Default)]
pub(crate) struct SpacesCandidateCoordinator {
    pending: Option<PendingSpacesCandidate>,
    last_published: Option<crate::extension_contract::HostSpacesV1>,
    next_attempt_id: u64,
}

impl SpacesCandidateCoordinator {
    fn cancel_pending(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.canceled.store(true, Ordering::Release);
        }
    }

    fn begin(
        &mut self,
        app: &mut App,
        snapshot: crate::extension_contract::HostSpacesV1,
    ) -> Result<Option<SpacesCandidateWork>, String> {
        let (artifact_generation, runtime_generation) = exact_spaces_binding(app)?;
        if self
            .last_published
            .as_ref()
            .is_some_and(|published| same_spaces_semantics(published, &snapshot))
            && app
                .host_document_caches
                .get(&(
                    crate::spaces_extension_contract::SPACES_NAMESPACE.into(),
                    crate::spaces_extension_contract::SPACES_SCOPE_ID.into(),
                ))
                .and_then(|cache| cache.current())
                .is_some()
        {
            return Ok(None);
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| same_spaces_semantics(&pending.snapshot, &snapshot))
        {
            return Ok(None);
        }
        // Pure render supersession retires the ticket, not its component runtime.
        // finish() rejects the obsolete attempt; disable/error/deadline cancellation
        // remains unchanged. The service still has at most four queued + one active.
        self.pending = None;
        self.next_attempt_id = self.next_attempt_id.saturating_add(1).max(1);
        let attempt_id = self.next_attempt_id;
        let deadline = Instant::now() + INVOCATION_TIMEOUT;
        let canceled = Arc::new(AtomicBool::new(false));
        let params = serde_json::to_value(&snapshot)
            .map_err(|error| format!("encode Spaces snapshot: {error}"))?;
        self.pending = Some(PendingSpacesCandidate {
            attempt_id,
            snapshot,
            artifact_generation,
            runtime_generation,
            deadline,
            canceled: canceled.clone(),
        });
        Ok(Some(SpacesCandidateWork {
            attempt_id,
            params,
            deadline,
            canceled,
        }))
    }

    fn finish(
        &mut self,
        app: &mut App,
        attempt_id: u64,
        result: Result<serde_json::Value, String>,
    ) -> Result<bool, String> {
        let Some(pending) = self.pending.take() else {
            return Ok(false);
        };
        if pending.attempt_id != attempt_id {
            self.pending = Some(pending);
            return Ok(false);
        }
        if pending.canceled.load(Ordering::Acquire) || Instant::now() >= pending.deadline {
            return Err("Spaces candidate invocation was canceled or expired".into());
        }
        let (artifact_generation, runtime_generation) = exact_spaces_binding(app)?;
        if artifact_generation != pending.artifact_generation
            || runtime_generation != pending.runtime_generation
            || app
                .host_spaces_last_desired
                .as_ref()
                .is_none_or(|desired| desired != &pending.snapshot)
        {
            return Err("Spaces candidate authority is stale or mismatched".into());
        }
        let document: crate::extension_contract::HostDocumentV1 = serde_json::from_value(
            result.map_err(|error| format!("Spaces candidate failed: {error}"))?,
        )
        .map_err(|error| format!("decode Spaces document: {error}"))?;
        crate::spaces_extension_contract::validate_spaces_document(&pending.snapshot, &document)?;
        let key = (
            crate::spaces_extension_contract::SPACES_NAMESPACE.into(),
            crate::spaces_extension_contract::SPACES_SCOPE_ID.into(),
        );
        app.host_document_caches
            .entry(key.clone())
            .or_default()
            .publish_for_namespace(
                crate::spaces_extension_contract::SPACES_NAMESPACE,
                pending.snapshot.generation,
                PROVIDER_PRIORITY,
                &std::collections::HashSet::new(),
                document,
            )
            .map_err(|error| error.to_string())?;
        self.last_published = Some(pending.snapshot.clone());
        app.emit_host_document_updated(
            key.0,
            key.1,
            pending.snapshot.generation,
            pending.snapshot.revision,
            true,
        );
        Ok(true)
    }

    fn disable(&mut self) {
        self.cancel_pending();
        self.last_published = None;
    }
}

impl App {
    #[cfg(test)]
    pub(crate) fn assert_spaces_component_supersession_for_test(
        installed: &crate::extension_package::InstalledPackageV1,
    ) {
        tests::assert_real_component_supersession(installed);
    }

    pub(crate) fn spaces_package_provider_generation(&self) -> Option<u64> {
        exact_spaces_binding(self)
            .ok()
            .and_then(|_| {
                self.marketplace_state
                    .packages
                    .packages
                    .get(crate::spaces_extension_contract::SPACES_PACKAGE_ID)
                    .map(|record| record.revision)
            })
            .filter(|generation| *generation > 0)
    }

    pub(crate) fn schedule_spaces_candidate(
        &mut self,
        snapshot: crate::extension_contract::HostSpacesV1,
    ) -> Result<(), String> {
        let mut coordinator = std::mem::take(&mut self.spaces_candidate_audits);
        let work = coordinator.begin(self, snapshot);
        self.spaces_candidate_audits = coordinator;
        let Some(work) = work? else {
            return Ok(());
        };
        let slot = self
            .extension_package_services
            .get(crate::spaces_extension_contract::SPACES_PACKAGE_ID)
            .ok_or("Spaces package runtime is unavailable")?;
        let invoked = slot.invoke_no_host_calls(
            crate::spaces_extension_contract::SPACES_SERVICE_ID,
            work.attempt_id,
            crate::spaces_extension_contract::SPACES_DOCUMENT_RENDER_METHOD_V1.into(),
            work.params,
            work.deadline,
            work.canceled,
        );
        if invoked.is_err() {
            self.spaces_candidate_audits.cancel_pending();
            self.host_spaces_last_desired = None;
        }
        invoked
    }

    pub(crate) fn finalize_spaces_candidate(
        &mut self,
        attempt_id: u64,
        result: Result<serde_json::Value, String>,
    ) -> Result<bool, String> {
        let mut coordinator = std::mem::take(&mut self.spaces_candidate_audits);
        let finished = coordinator.finish(self, attempt_id, result);
        self.spaces_candidate_audits = coordinator;
        finished
    }

    pub(crate) fn disable_spaces_package_authority_runtime(&mut self) {
        self.spaces_candidate_audits.disable();
        self.host_spaces_last_desired = None;
        let key = (
            crate::spaces_extension_contract::SPACES_NAMESPACE.into(),
            crate::spaces_extension_contract::SPACES_SCOPE_ID.into(),
        );
        let previous = self
            .host_document_caches
            .get(&key)
            .and_then(|cache| cache.current())
            .map(|document| (document.generation, document.revision));
        if let Some(cache) = self.host_document_caches.get_mut(&key) {
            cache.disable();
        }
        self.host_document_request_watermarks.remove(&key);
        if let Some((generation, revision)) = previous {
            self.emit_host_document_updated(key.0, key.1, generation, revision, false);
        }
    }
}

fn exact_spaces_binding(app: &App) -> Result<(u64, u64), String> {
    let pin = crate::spaces_extension_contract::embedded_spaces_production_pin()?;
    let record = app
        .marketplace_state
        .packages
        .packages
        .get(&pin.package_id)
        .ok_or("exact Spaces package is not installed")?;
    let active = record
        .active
        .as_ref()
        .filter(|_| record.status == crate::extension_package::PackageLifecycleStatusV1::Enabled)
        .ok_or("exact Spaces package is not enabled")?;
    let source_matches = matches!(
        &record.source,
        crate::extension_package::PackageProvenanceV1::Git {
            url,
            source_commit,
            package_path,
            package_id,
            version,
            package_sha256,
            manifest_sha256,
        } if url == &pin.source_url
            && source_commit == &pin.source_commit
            && package_path == &pin.package_path
            && package_id == &pin.package_id
            && version == &pin.version
            && package_sha256 == &pin.package_sha256
            && manifest_sha256 == &pin.manifest_sha256
    );
    if !source_matches
        || active.package_sha256 != pin.package_sha256
        || active.version != pin.version
        || !record.namespaces.contains(&pin.namespace)
    {
        return Err("installed Spaces package does not match the exact production pin".into());
    }
    let slot = app
        .extension_package_services
        .get(&pin.package_id)
        .filter(|slot| slot.is_ready())
        .ok_or("exact Spaces package candidate is not ready")?;
    let _binding = slot
        .binding(&pin.service_id)
        .filter(|binding| {
            binding.package_id() == pin.package_id
                && binding.namespace() == pin.namespace
                && binding.artifact_sha256() == pin.package_sha256
                && binding.artifact_version() == pin.version
                && binding.capabilities().is_empty()
        })
        .ok_or("Spaces package runtime binding does not match the production pin")?
        .clone();
    let runtime_generation = slot
        .runtime_generation(&pin.service_id)
        .filter(|generation| *generation > 0)
        .ok_or("Spaces package runtime generation is unavailable")?;
    Ok((active.runtime_generation, runtime_generation))
}

fn same_spaces_semantics(
    left: &crate::extension_contract::HostSpacesV1,
    right: &crate::extension_contract::HostSpacesV1,
) -> bool {
    let mut normalized = left.clone();
    normalized.revision = right.revision;
    normalized == *right
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_exact_package() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let pin = crate::spaces_extension_contract::embedded_spaces_production_pin().unwrap();
        app.marketplace_state.packages.revision = 1;
        app.marketplace_state.packages.load_order = vec![pin.package_id.clone()];
        app.marketplace_state.packages.packages.insert(
            pin.package_id.clone(),
            crate::extension_package::PackageLifecycleRecordV1 {
                package_id: pin.package_id.clone(),
                source: crate::extension_package::PackageProvenanceV1::Git {
                    url: pin.source_url.clone(),
                    source_commit: pin.source_commit.clone(),
                    package_path: pin.package_path.clone(),
                    package_id: pin.package_id.clone(),
                    version: pin.version.clone(),
                    package_sha256: pin.package_sha256.clone(),
                    manifest_sha256: pin.manifest_sha256.clone(),
                },
                previous_source: None,
                namespaces: vec![pin.namespace.clone()],
                revision: 1,
                status: crate::extension_package::PackageLifecycleStatusV1::Enabled,
                active: Some(crate::extension_package::PackageArtifactPointerV1 {
                    package_sha256: pin.package_sha256.clone(),
                    version: pin.version.clone(),
                    runtime_generation: 1,
                }),
                previous: None,
                activation_error: None,
                pending_activation: None,
                retained_data: false,
            },
        );
        let binding = crate::extension_host_state::HostProviderBinding::for_test(
            pin.package_id.clone(),
            pin.service_id.clone(),
            pin.namespace,
            pin.package_sha256,
            pin.version,
            std::collections::BTreeSet::new(),
        );
        app.extension_package_services.insert(
            pin.package_id,
            crate::extension_candidate::PackageCandidateSlot::ready_for_test(binding, 1),
        );
        app
    }

    fn snapshot(label: &str, revision: u64) -> crate::extension_contract::HostSpacesV1 {
        crate::extension_contract::HostSpacesV1 {
            contract: "host.spaces.v1".into(),
            generation: 1,
            revision,
            scope_id: crate::spaces_extension_contract::SPACES_SCOPE_ID.into(),
            repositories: vec![crate::extension_contract::HostSpaceRepository {
                repository_id: "repo_main".into(),
                label: label.into(),
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
                    repository_id: "repo_main".into(),
                },
            ],
        }
    }

    fn document(
        snapshot: &crate::extension_contract::HostSpacesV1,
    ) -> crate::extension_contract::HostDocumentV1 {
        use crate::extension_contract::{
            HostDocumentItem, HostDocumentItemFlags, HostDocumentV1, HostItemRole, HostSurface,
            HostTarget,
        };
        HostDocumentV1 {
            contract: crate::extension_contract::HOST_DOCUMENT_CONTRACT_V1.into(),
            namespace: crate::spaces_extension_contract::SPACES_NAMESPACE.into(),
            generation: snapshot.generation,
            revision: snapshot.revision,
            surface: HostSurface::Navigation,
            target: HostTarget::Session,
            items: vec![
                HostDocumentItem {
                    id: "workspace:workspace_notes".into(),
                    parent_id: None,
                    role: HostItemRole::Section,
                    label: "Notes".into(),
                    detail: None,
                    terminal: None,
                    action_ids: Vec::new(),
                    flags: HostDocumentItemFlags::default(),
                },
                HostDocumentItem {
                    id: "repository:repo_main".into(),
                    parent_id: None,
                    role: HostItemRole::Group,
                    label: snapshot.repositories[0].label.clone(),
                    detail: Some("1 workspace".into()),
                    terminal: None,
                    action_ids: Vec::new(),
                    flags: HostDocumentItemFlags {
                        expanded: true,
                        ..Default::default()
                    },
                },
                HostDocumentItem {
                    id: "workspace:workspace_main".into(),
                    parent_id: Some("repository:repo_main".into()),
                    role: HostItemRole::Section,
                    label: "main".into(),
                    detail: None,
                    terminal: None,
                    action_ids: Vec::new(),
                    flags: HostDocumentItemFlags::default(),
                },
            ],
            actions: Vec::new(),
            required_capabilities: Vec::new(),
        }
    }

    fn begin(app: &mut App, snapshot: crate::extension_contract::HostSpacesV1) -> u64 {
        app.host_spaces_last_desired = Some(snapshot.clone());
        let mut coordinator = std::mem::take(&mut app.spaces_candidate_audits);
        let attempt = coordinator
            .begin(app, snapshot)
            .unwrap()
            .expect("changed snapshot schedules one invocation")
            .attempt_id;
        app.spaces_candidate_audits = coordinator;
        attempt
    }

    fn completion(app: &mut App, attempt_id: u64, result: Result<serde_json::Value, String>) {
        let pin = crate::spaces_extension_contract::embedded_spaces_production_pin().unwrap();
        app.handle_extension_package_candidate_finished(
            crate::extension_candidate::PackageCandidateInvocationResult {
                package_id: pin.package_id,
                service_id: pin.service_id,
                artifact_sha256: pin.package_sha256,
                artifact_generation: 1,
                runtime_generation: 1,
                attempt_id,
                result,
            },
        );
    }

    #[test]
    fn exact_package_publication_is_atomic_and_failure_fences_only_spaces() {
        let mut app = app_with_exact_package();
        let first = snapshot("Repository", 1);
        let first_attempt = begin(&mut app, first.clone());
        completion(
            &mut app,
            first_attempt,
            Ok(serde_json::to_value(document(&first)).unwrap()),
        );
        let key = (
            crate::spaces_extension_contract::SPACES_NAMESPACE.into(),
            crate::spaces_extension_contract::SPACES_SCOPE_ID.into(),
        );
        assert_eq!(
            app.host_document_caches
                .get(&key)
                .and_then(|cache| cache.current())
                .map(|document| document.revision),
            Some(1)
        );

        let replacement = snapshot("Renamed repository", 2);
        let replacement_attempt = begin(&mut app, replacement);
        assert_eq!(
            app.host_document_caches
                .get(&key)
                .and_then(|cache| cache.current())
                .map(|document| document.revision),
            Some(1),
            "the last audited document remains visible until atomic replacement"
        );
        completion(
            &mut app,
            replacement_attempt,
            Err("component trapped".into()),
        );
        assert!(app
            .host_document_caches
            .get(&key)
            .and_then(|cache| cache.current())
            .is_none());

        let generic = app.handle_api_request(crate::api::schema::Request {
            id: "workspaces".into(),
            method: crate::api::schema::Method::WorkspaceList(
                crate::api::schema::EmptyParams::default(),
            ),
        });
        serde_json::from_str::<crate::api::schema::SuccessResponse>(&generic)
            .expect("generic workspace access remains available");
    }

    #[test]
    fn provider_generation_change_requires_fresh_audit_for_unchanged_spaces() {
        let mut app = app_with_exact_package();
        let first = snapshot("Repository", 1);
        let first_attempt = begin(&mut app, first.clone());
        completion(
            &mut app,
            first_attempt,
            Ok(serde_json::to_value(document(&first)).unwrap()),
        );
        app.marketplace_state
            .packages
            .packages
            .get_mut(crate::spaces_extension_contract::SPACES_PACKAGE_ID)
            .unwrap()
            .revision = 2;
        let mut rebound = first;
        rebound.generation = 2;
        rebound.revision = 2;
        assert_ne!(begin(&mut app, rebound), first_attempt);
    }

    pub(super) fn assert_real_component_supersession(
        installed: &crate::extension_package::InstalledPackageV1,
    ) {
        let pin = crate::spaces_extension_contract::embedded_spaces_production_pin().unwrap();
        let factory = crate::extension_runtime::RuntimeFactory::installed(
            installed,
            &pin.service_id,
            &pin.namespace,
        )
        .unwrap();
        let mut app = app_with_exact_package();
        let first = snapshot("first", 1);
        let old_attempt = begin(&mut app, first.clone());
        let old_canceled = app
            .spaces_candidate_audits
            .pending
            .as_ref()
            .unwrap()
            .canceled
            .clone();
        let mut runtime =
            crate::extension_runtime::ExtensionRuntimeDispatcher::start(&factory, 1, &old_canceled)
                .unwrap();
        let old_result = runtime
            .invoke(
                &pin.method,
                serde_json::to_value(&first).unwrap(),
                1,
                Instant::now() + INVOCATION_TIMEOUT,
                &old_canceled,
            )
            .unwrap();
        // Hold the real component's result outside the App until supersession.
        // Spaces has no host calls; acceptance is its host snapshot/attempt fence.
        let second = snapshot("second", 2);
        let current_attempt = begin(&mut app, second.clone());
        assert!(!old_canceled.load(Ordering::Acquire));
        completion(&mut app, old_attempt, Ok(old_result));
        let key = (
            pin.namespace.clone(),
            crate::spaces_extension_contract::SPACES_SCOPE_ID.into(),
        );
        assert!(app
            .host_document_caches
            .get(&key)
            .and_then(|cache| cache.current())
            .is_none());
        let current_canceled = app
            .spaces_candidate_audits
            .pending
            .as_ref()
            .unwrap()
            .canceled
            .clone();
        let result = runtime
            .invoke(
                &pin.method,
                serde_json::to_value(&second).unwrap(),
                1,
                Instant::now() + INVOCATION_TIMEOUT,
                &current_canceled,
            )
            .unwrap();
        completion(&mut app, current_attempt, Ok(result));
        assert_eq!(
            app.host_document_caches
                .get(&key)
                .and_then(|cache| cache.current())
                .map(|document| document.revision),
            Some(2)
        );
        runtime.stop(crate::extension_runtime::RuntimeStopReason::Requested);
    }

    #[test]
    fn superseded_completion_cannot_replace_the_current_spaces_attempt() {
        let mut app = app_with_exact_package();
        let first = snapshot("first", 1);
        let first_attempt = begin(&mut app, first.clone());
        let old_canceled = app
            .spaces_candidate_audits
            .pending
            .as_ref()
            .unwrap()
            .canceled
            .clone();
        let second = snapshot("second", 2);
        let second_attempt = begin(&mut app, second.clone());
        assert!(
            !old_canceled.load(Ordering::Acquire),
            "read supersession aborted the component"
        );
        completion(
            &mut app,
            first_attempt,
            Ok(serde_json::to_value(document(&first)).unwrap()),
        );
        assert!(app
            .host_spaces_last_desired
            .as_ref()
            .is_some_and(|desired| { desired.repositories[0].label == "second" }));
        completion(
            &mut app,
            second_attempt,
            Ok(serde_json::to_value(document(&second)).unwrap()),
        );
        assert_eq!(
            app.host_document_caches
                .get(&(
                    crate::spaces_extension_contract::SPACES_NAMESPACE.into(),
                    crate::spaces_extension_contract::SPACES_SCOPE_ID.into(),
                ))
                .and_then(|cache| cache.current())
                .map(|document| document.revision),
            Some(2)
        );
    }
}
