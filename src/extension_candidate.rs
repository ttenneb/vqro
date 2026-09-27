//! Bounded, non-publishing workers for installed package candidates.
//!
//! Package validation, component compilation/instantiation, invocation waits, and worker
//! shutdown all stay off the App actor. A ready candidate owns one worker per declared service;
//! dropping or replacing it revokes queued work without joining from the App actor.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::{Duration, Instant};

use crate::extension_host_invocation::HostReadInvocationAuthorization;
use crate::extension_host_state::HostProviderBinding;

pub(crate) const MAX_CANDIDATE_REQUESTS_PER_SERVICE: usize = 4;
const CANDIDATE_RUNTIME_GENERATION: u64 = 1;

pub(crate) struct PackageCandidateSlot {
    package_id: String,
    artifact_sha256: String,
    artifact_generation: u64,
    canceled: Arc<AtomicBool>,
    ready: Option<PackageCandidateSet>,
}

impl std::fmt::Debug for PackageCandidateSlot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PackageCandidateSlot")
            .field("package_id", &self.package_id)
            .field("artifact_sha256", &self.artifact_sha256)
            .field("artifact_generation", &self.artifact_generation)
            .field("ready", &self.ready.is_some())
            .finish()
    }
}

impl PackageCandidateSlot {
    #[cfg(test)]
    pub(crate) fn ready_for_test(binding: HostProviderBinding, artifact_generation: u64) -> Self {
        let package_id = binding.package_id().to_owned();
        let service_id = binding.service_id().to_owned();
        let artifact_sha256 = binding.artifact_sha256().to_owned();
        let canceled = Arc::new(AtomicBool::new(false));
        let worker_canceled = Arc::clone(&canceled);
        let (requests, request_rx) = mpsc::sync_channel::<CandidateRequest>(1);
        let join = std::thread::spawn(move || {
            while request_rx.recv().is_ok() {
                if worker_canceled.load(Ordering::Acquire) {
                    break;
                }
            }
        });
        let worker = CandidateWorker {
            binding,
            runtime_generation: CANDIDATE_RUNTIME_GENERATION,
            requests,
            canceled: Arc::clone(&canceled),
            active: Arc::new(Mutex::new(None)),
            _join: join,
        };
        Self {
            package_id: package_id.clone(),
            artifact_sha256,
            artifact_generation,
            canceled,
            ready: Some(PackageCandidateSet {
                package_id,
                artifact_generation,
                validated_package: None,
                workers: BTreeMap::from([(service_id, worker)]),
            }),
        }
    }

    pub(crate) fn spawn(
        installed: crate::extension_package::InstalledPackageV1,
        artifact_generation: u64,
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) -> Result<Self, String> {
        if artifact_generation == 0 {
            return Err("candidate artifact generation must be nonzero".into());
        }
        let package_id = installed.package_id.clone();
        let artifact_sha256 = installed.package_sha256.clone();
        let canceled = Arc::new(AtomicBool::new(false));
        let bootstrap_canceled = Arc::clone(&canceled);
        let bootstrap_package = package_id.clone();
        let bootstrap_sha = artifact_sha256.clone();
        std::thread::Builder::new()
            .name(format!("vqro-pkg-bootstrap-{package_id}"))
            .spawn(move || {
                let result = PackageCandidateSet::start(
                    installed,
                    artifact_generation,
                    event_tx.clone(),
                    bootstrap_canceled.clone(),
                );
                if bootstrap_canceled.load(Ordering::Acquire) {
                    return;
                }
                let completion = PackageCandidateBootstrapResult {
                    package_id: bootstrap_package,
                    artifact_sha256: bootstrap_sha,
                    artifact_generation,
                    result,
                };
                let _ = event_tx.blocking_send(
                    crate::events::AppEvent::ExtensionPackageCandidateReady(Box::new(completion)),
                );
            })
            .map_err(|error| format!("cannot spawn package candidate bootstrap: {error}"))?;
        Ok(Self {
            package_id,
            artifact_sha256,
            artifact_generation,
            canceled,
            ready: None,
        })
    }

    pub(crate) fn accepts(&self, result: &PackageCandidateBootstrapResult) -> bool {
        !self.canceled.load(Ordering::Acquire)
            && self.package_id == result.package_id
            && self.artifact_sha256 == result.artifact_sha256
            && self.artifact_generation == result.artifact_generation
    }

    pub(crate) fn install_ready(&mut self, candidate: PackageCandidateSet) {
        self.ready = Some(candidate);
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.ready.is_some()
    }

    pub(crate) fn artifact_sha256(&self) -> &str {
        &self.artifact_sha256
    }

    pub(crate) fn artifact_generation(&self) -> u64 {
        self.artifact_generation
    }

    pub(crate) fn binding(&self, service_id: &str) -> Option<&HostProviderBinding> {
        self.ready.as_ref()?.binding(service_id)
    }

    pub(crate) fn runtime_generation(&self, service_id: &str) -> Option<u64> {
        self.ready.as_ref()?.runtime_generation(service_id)
    }

    pub(crate) fn validated_package(
        &self,
    ) -> Option<&crate::extension_package::ValidatedPackageV1> {
        self.ready.as_ref()?.validated_package.as_deref()
    }

    pub(crate) fn service_read_endpoint(
        &self,
        service_id: &str,
    ) -> Option<ServiceReadProviderEndpoint> {
        self.ready.as_ref()?.service_read_endpoint(service_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn invoke(
        &self,
        service_id: &str,
        attempt_id: u64,
        method: String,
        params: serde_json::Value,
        authorization: HostReadInvocationAuthorization,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
    ) -> Result<(), String> {
        self.ready
            .as_ref()
            .ok_or("package candidate is not ready")?
            .invoke(
                service_id,
                attempt_id,
                method,
                params,
                CandidateAuthorization::HostReads(authorization),
                deadline,
                canceled,
            )
    }

    pub(crate) fn invoke_no_host_calls(
        &self,
        service_id: &str,
        attempt_id: u64,
        method: String,
        params: serde_json::Value,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
    ) -> Result<(), String> {
        self.ready
            .as_ref()
            .ok_or("package candidate is not ready")?
            .invoke(
                service_id,
                attempt_id,
                method,
                params,
                CandidateAuthorization::None,
                deadline,
                canceled,
            )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn invoke_service_read(
        &self,
        service_id: &str,
        attempt_id: u64,
        method: String,
        params: serde_json::Value,
        authorization: crate::service_fabric::ServiceReadInvocationAuthorization,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
    ) -> Result<(), String> {
        self.ready
            .as_ref()
            .ok_or("package candidate is not ready")?
            .invoke(
                service_id,
                attempt_id,
                method,
                params,
                CandidateAuthorization::ServiceRead(authorization),
                deadline,
                canceled,
            )
    }
}

impl Drop for PackageCandidateSlot {
    fn drop(&mut self) {
        self.canceled.store(true, Ordering::Release);
    }
}

pub(crate) struct PackageCandidateBootstrapResult {
    pub(crate) package_id: String,
    pub(crate) artifact_sha256: String,
    pub(crate) artifact_generation: u64,
    pub(crate) result: Result<PackageCandidateSet, String>,
}

impl std::fmt::Debug for PackageCandidateBootstrapResult {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PackageCandidateBootstrapResult")
            .field("package_id", &self.package_id)
            .field("artifact_sha256", &self.artifact_sha256)
            .field("artifact_generation", &self.artifact_generation)
            .field("ready", &self.result.is_ok())
            .finish()
    }
}

#[derive(Debug)]
pub(crate) struct PackageCandidateInvocationResult {
    pub(crate) package_id: String,
    pub(crate) service_id: String,
    pub(crate) artifact_sha256: String,
    pub(crate) artifact_generation: u64,
    pub(crate) runtime_generation: u64,
    pub(crate) attempt_id: u64,
    pub(crate) result: Result<serde_json::Value, String>,
}

#[allow(dead_code)] // ServiceRead is live through fabric conformance before R3 composition.
enum CandidateAuthorization {
    None,
    HostReads(HostReadInvocationAuthorization),
    ServiceRead(crate::service_fabric::ServiceReadInvocationAuthorization),
}

enum CandidateCompletion {
    AppEvent,
    ServiceRead(mpsc::SyncSender<Result<serde_json::Value, String>>),
}

struct CandidateRequest {
    attempt_id: u64,
    method: String,
    params: serde_json::Value,
    authorization: CandidateAuthorization,
    deadline: Instant,
    canceled: Arc<AtomicBool>,
    completion: CandidateCompletion,
}

#[derive(Clone)]
pub(crate) struct ServiceReadProviderEndpoint {
    package_id: String,
    service_id: String,
    artifact_sha256: String,
    artifact_generation: u64,
    runtime_generation: u64,
    requests: mpsc::SyncSender<CandidateRequest>,
    worker_canceled: Arc<AtomicBool>,
}

impl ServiceReadProviderEndpoint {
    #[cfg(test)]
    pub(crate) fn for_test(
        package_id: &str,
        service_id: &str,
        artifact_sha256: &str,
        artifact_generation: u64,
        runtime_generation: u64,
        respond: impl Fn(&str, serde_json::Value) -> Result<serde_json::Value, String> + Send + 'static,
    ) -> Self {
        Self::for_test_with_capacity(
            package_id,
            service_id,
            artifact_sha256,
            artifact_generation,
            runtime_generation,
            MAX_CANDIDATE_REQUESTS_PER_SERVICE,
            respond,
        )
    }

    #[cfg(test)]
    pub(crate) fn for_test_with_capacity(
        package_id: &str,
        service_id: &str,
        artifact_sha256: &str,
        artifact_generation: u64,
        runtime_generation: u64,
        queue_capacity: usize,
        respond: impl Fn(&str, serde_json::Value) -> Result<serde_json::Value, String> + Send + 'static,
    ) -> Self {
        let (requests, request_rx) = mpsc::sync_channel::<CandidateRequest>(queue_capacity);
        let worker_canceled = Arc::new(AtomicBool::new(false));
        let thread_canceled = Arc::clone(&worker_canceled);
        std::thread::spawn(move || {
            while let Ok(request) = request_rx.recv() {
                if thread_canceled.load(Ordering::Acquire) {
                    break;
                }
                let result = respond(&request.method, request.params);
                if let CandidateCompletion::ServiceRead(respond_to) = request.completion {
                    let _ = respond_to.send(result);
                }
            }
        });
        Self {
            package_id: package_id.into(),
            service_id: service_id.into(),
            artifact_sha256: artifact_sha256.into(),
            artifact_generation,
            runtime_generation,
            requests,
            worker_canceled,
        }
    }

    pub(crate) fn package_id(&self) -> &str {
        &self.package_id
    }

    pub(crate) fn service_id(&self) -> &str {
        &self.service_id
    }

    pub(crate) fn artifact_sha256(&self) -> &str {
        &self.artifact_sha256
    }

    pub(crate) fn artifact_generation(&self) -> u64 {
        self.artifact_generation
    }

    pub(crate) fn runtime_generation(&self) -> u64 {
        self.runtime_generation
    }

    pub(crate) fn is_current(&self) -> bool {
        !self.worker_canceled.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn retire_for_test(&self) {
        self.worker_canceled.store(true, Ordering::Release);
    }

    pub(crate) fn call(
        &self,
        attempt_id: u64,
        method: String,
        params: serde_json::Value,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
        outer_canceled: &AtomicBool,
        binding_revoked: &AtomicBool,
    ) -> Result<serde_json::Value, String> {
        if attempt_id == 0
            || Instant::now() >= deadline
            || canceled.load(Ordering::Acquire)
            || outer_canceled.load(Ordering::Acquire)
            || binding_revoked.load(Ordering::Acquire)
            || self.worker_canceled.load(Ordering::Acquire)
        {
            return Err("service provider is canceled or expired".into());
        }
        let (respond_to, response_rx) = mpsc::sync_channel(1);
        self.requests
            .try_send(CandidateRequest {
                attempt_id,
                method,
                params,
                authorization: CandidateAuthorization::None,
                deadline,
                canceled: Arc::clone(&canceled),
                completion: CandidateCompletion::ServiceRead(respond_to),
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => String::from("service provider queue is full"),
                mpsc::TrySendError::Disconnected(_) => {
                    String::from("service provider worker is unavailable")
                }
            })?;
        loop {
            if canceled.load(Ordering::Acquire)
                || outer_canceled.load(Ordering::Acquire)
                || binding_revoked.load(Ordering::Acquire)
                || self.worker_canceled.load(Ordering::Acquire)
            {
                canceled.store(true, Ordering::Release);
                return Err("service call was canceled".into());
            }
            let now = Instant::now();
            if now >= deadline {
                canceled.store(true, Ordering::Release);
                return Err("service call timed out".into());
            }
            let wait = deadline
                .saturating_duration_since(now)
                .min(Duration::from_millis(10));
            match response_rx.recv_timeout(wait) {
                Ok(result)
                    if !canceled.load(Ordering::Acquire)
                        && !outer_canceled.load(Ordering::Acquire)
                        && !binding_revoked.load(Ordering::Acquire)
                        && !self.worker_canceled.load(Ordering::Acquire) =>
                {
                    return result;
                }
                Ok(_) => {
                    canceled.store(true, Ordering::Release);
                    return Err("service call completed after cancellation".into());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("service provider response channel closed".into());
                }
            }
        }
    }
}

struct CandidateWorker {
    binding: HostProviderBinding,
    runtime_generation: u64,
    requests: mpsc::SyncSender<CandidateRequest>,
    canceled: Arc<AtomicBool>,
    active: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    // Deliberately detached on drop: the App actor must never join a runtime worker.
    _join: std::thread::JoinHandle<()>,
}

pub(crate) struct PackageCandidateSet {
    package_id: String,
    artifact_generation: u64,
    validated_package: Option<Arc<crate::extension_package::ValidatedPackageV1>>,
    workers: BTreeMap<String, CandidateWorker>,
}

impl std::fmt::Debug for PackageCandidateSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PackageCandidateSet")
            .field("package_id", &self.package_id)
            .field("artifact_generation", &self.artifact_generation)
            .field("worker_count", &self.workers.len())
            .finish()
    }
}

impl PackageCandidateSet {
    fn start(
        installed: crate::extension_package::InstalledPackageV1,
        artifact_generation: u64,
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
        bootstrap_canceled: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let archive = std::path::Path::new(&installed.immutable_root).join("package.vqrox");
        let package = crate::extension_package::validate_package_file(
            &archive,
            Some(&installed.package_sha256),
        )?;
        if bootstrap_canceled.load(Ordering::Acquire) {
            return Err("package candidate bootstrap was canceled".into());
        }
        let namespace = package
            .descriptor
            .manifest
            .namespaces
            .first()
            .map_or(package.descriptor.manifest.id.as_str(), String::as_str)
            .to_owned();
        let mut workers = BTreeMap::new();
        for service in &package.descriptor.manifest.services {
            if bootstrap_canceled.load(Ordering::Acquire) {
                return Err("package candidate bootstrap was canceled".into());
            }
            let exported_methods = service
                .exports
                .iter()
                .filter_map(|export| {
                    package
                        .descriptor
                        .contract_descriptors
                        .get(&export.descriptor_sha256)
                })
                .flat_map(|descriptor| descriptor.methods.iter().map(|method| method.id.clone()))
                .collect::<BTreeSet<_>>();
            let worker = CandidateWorker::start(
                installed.clone(),
                service.id.clone(),
                namespace.clone(),
                artifact_generation,
                exported_methods,
                event_tx.clone(),
            )?;
            workers.insert(service.id.clone(), worker);
        }
        if workers.len() != package.descriptor.manifest.services.len() {
            return Err("package candidate worker catalog is incomplete".into());
        }
        Ok(Self {
            package_id: installed.package_id,
            artifact_generation,
            validated_package: Some(Arc::new(package.descriptor)),
            workers,
        })
    }

    pub(crate) fn binding(&self, service_id: &str) -> Option<&HostProviderBinding> {
        self.workers.get(service_id).map(|worker| &worker.binding)
    }

    pub(crate) fn runtime_generation(&self, service_id: &str) -> Option<u64> {
        self.workers
            .get(service_id)
            .map(|worker| worker.runtime_generation)
    }

    #[allow(dead_code)]
    pub(crate) fn service_read_endpoint(
        &self,
        service_id: &str,
    ) -> Option<ServiceReadProviderEndpoint> {
        let worker = self.workers.get(service_id)?;
        Some(ServiceReadProviderEndpoint {
            package_id: self.package_id.clone(),
            service_id: service_id.into(),
            artifact_sha256: worker.binding.artifact_sha256().into(),
            artifact_generation: self.artifact_generation,
            runtime_generation: worker.runtime_generation,
            requests: worker.requests.clone(),
            worker_canceled: Arc::clone(&worker.canceled),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn invoke(
        &self,
        service_id: &str,
        attempt_id: u64,
        method: String,
        params: serde_json::Value,
        authorization: CandidateAuthorization,
        deadline: Instant,
        canceled: Arc<AtomicBool>,
    ) -> Result<(), String> {
        if attempt_id == 0 || Instant::now() >= deadline || canceled.load(Ordering::Acquire) {
            return Err("candidate invocation is canceled or expired".into());
        }
        let worker = self
            .workers
            .get(service_id)
            .ok_or("package candidate service is absent")?;
        worker
            .requests
            .try_send(CandidateRequest {
                attempt_id,
                method,
                params,
                authorization,
                deadline,
                canceled,
                completion: CandidateCompletion::AppEvent,
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => "package candidate queue is full".into(),
                mpsc::TrySendError::Disconnected(_) => {
                    "package candidate worker is unavailable".into()
                }
            })
    }
}

impl Drop for PackageCandidateSet {
    fn drop(&mut self) {
        for worker in self.workers.values() {
            worker.canceled.store(true, Ordering::Release);
            if let Some(active) = worker
                .active
                .lock()
                .expect("candidate active-call lock")
                .as_ref()
            {
                active.store(true, Ordering::Release);
            }
        }
    }
}

impl CandidateWorker {
    fn start(
        installed: crate::extension_package::InstalledPackageV1,
        service_id: String,
        namespace: String,
        artifact_generation: u64,
        exported_methods: BTreeSet<String>,
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) -> Result<Self, String> {
        let binding = HostProviderBinding::from_installed(&installed, &service_id, &namespace)?;
        let (requests, request_rx) =
            mpsc::sync_channel::<CandidateRequest>(MAX_CANDIDATE_REQUESTS_PER_SERVICE);
        let canceled = Arc::new(AtomicBool::new(false));
        let worker_canceled = Arc::clone(&canceled);
        let active = Arc::new(Mutex::new(None::<Arc<AtomicBool>>));
        let thread_active = Arc::clone(&active);
        let thread_package_id = installed.package_id.clone();
        let thread_artifact_sha256 = installed.package_sha256.clone();
        let thread_service_id = service_id.clone();
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let join = std::thread::Builder::new()
            .name(format!("vqro-candidate-{thread_package_id}-{service_id}"))
            .spawn(move || {
                let supervisor = crate::extension_service::ExtensionServiceSupervisor::from_installed_service_at_generation(
                    &installed,
                    &thread_service_id,
                    &namespace,
                    0,
                    CANDIDATE_RUNTIME_GENERATION,
                )
                .and_then(|mut supervisor| {
                    supervisor.start()?;
                    if !supervisor.package_candidate_supports_methods(&exported_methods) {
                        return Err(crate::extension_runtime::RuntimeError::Protocol(
                            "component handshake omitted a manifest-v3 exported contract method"
                                .into(),
                        ));
                    }
                    Ok(supervisor)
                });
                let mut supervisor = match supervisor {
                    Ok(supervisor) => {
                        let _ = started_tx.send(Ok(()));
                        supervisor
                    }
                    Err(error) => {
                        let _ = started_tx.send(Err(error.to_string()));
                        return;
                    }
                };
                while let Ok(request) = request_rx.recv() {
                    if worker_canceled.load(Ordering::Acquire) {
                        request.canceled.store(true, Ordering::Release);
                        if let CandidateCompletion::ServiceRead(respond_to) = request.completion {
                            let _ = respond_to.send(Err("service provider was retired".into()));
                        }
                        break;
                    }
                    *thread_active
                        .lock()
                        .expect("candidate active-call lock") = Some(Arc::clone(&request.canceled));
                    if worker_canceled.load(Ordering::Acquire) {
                        request.canceled.store(true, Ordering::Release);
                    }
                    tracing::debug!(
                        package_id = %thread_package_id,
                        service_id = %thread_service_id,
                        method = %request.method,
                        attempt_id = request.attempt_id,
                        "package candidate invocation started"
                    );
                    let result = match request.authorization {
                        CandidateAuthorization::HostReads(authorization) => {
                            supervisor.invoke_package_read_candidate(
                                &request.method,
                                request.params,
                                request.deadline,
                                Arc::clone(&request.canceled),
                                authorization,
                            )
                        }
                        CandidateAuthorization::ServiceRead(authorization) => {
                            supervisor.invoke_package_service_read_candidate(
                                &request.method,
                                request.params,
                                request.deadline,
                                Arc::clone(&request.canceled),
                                authorization,
                            )
                        }
                        CandidateAuthorization::None => {
                            supervisor.invoke_package_candidate_no_host_calls(
                                &request.method,
                                request.params,
                                request.deadline,
                                Arc::clone(&request.canceled),
                            )
                        }
                    }
                    .map_err(|error| error.to_string());
                    *thread_active
                        .lock()
                        .expect("candidate active-call lock") = None;
                    tracing::debug!(
                        package_id = %thread_package_id,
                        service_id = %thread_service_id,
                        method = %request.method,
                        attempt_id = request.attempt_id,
                        succeeded = result.is_ok(),
                        "package candidate invocation finished"
                    );
                    match request.completion {
                        CandidateCompletion::ServiceRead(respond_to) => {
                            let _ = respond_to.send(result);
                        }
                        CandidateCompletion::AppEvent => {
                            let completion = PackageCandidateInvocationResult {
                                package_id: thread_package_id.clone(),
                                service_id: thread_service_id.clone(),
                                artifact_sha256: thread_artifact_sha256.clone(),
                                artifact_generation,
                                runtime_generation: CANDIDATE_RUNTIME_GENERATION,
                                attempt_id: request.attempt_id,
                                result,
                            };
                            if event_tx
                                .blocking_send(
                                    crate::events::AppEvent::ExtensionPackageCandidateFinished(
                                        Box::new(completion),
                                    ),
                                )
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
                supervisor.stop();
            })
            .map_err(|error| format!("cannot spawn package candidate worker: {error}"))?;
        match started_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                binding,
                runtime_generation: CANDIDATE_RUNTIME_GENERATION,
                requests,
                canceled,
                active,
                _join: join,
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err("package candidate worker exited during startup".into()),
        }
    }
}
