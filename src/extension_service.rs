//! Supervised, out-of-process extension services.
//!
//! Services use bounded newline-delimited JSON over stdio. This module deliberately exposes no
//! renderer callbacks or terminal escape surface; inspector providers return validated
//! [`crate::view_document::ViewDocument`] values.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub const EXTENSION_SERVICE_PROTOCOL: u16 = 2;
const MAX_FRAME_BYTES: usize = 256 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_millis(750);
const MAX_RESTARTS: u32 = 3;
const MAX_STDERR_LINE_BYTES: usize = 4 * 1024;
const MAX_STDERR_LINES_PER_MINUTE: usize = 20;
const MAX_CANCELLATION_TOMBSTONES: usize = 64;

#[derive(Debug, Clone)]
pub struct ExtensionServiceSpec {
    pub id: String,
    pub command: Vec<String>,
    pub cwd: PathBuf,
    pub priority: i32,
    pub granted_capabilities: Vec<crate::api::schema::ExtensionServiceCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionServiceStatus {
    Stopped,
    Starting,
    Running,
    Backoff { failures: u32, retry_at: Instant },
    Failed(String),
}

#[derive(Debug)]
pub struct ExtensionServiceSupervisor {
    spec: ExtensionServiceSpec,
    runtime: Option<ExtensionServiceRuntime>,
    status: ExtensionServiceStatus,
    failures: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExtensionRequestIdentity {
    pub client_id: u64,
    pub boot_id: String,
    pub projection_revision: u64,
    pub request_id: String,
    pub target_pane_id: String,
    pub contribution_revision: u64,
}

enum ExtensionWorkerRequest {
    Render {
        service_generation: u64,
        identity: ExtensionRequestIdentity,
        context: serde_json::Value,
        canceled: Arc<AtomicBool>,
        result_permit: tokio::sync::mpsc::OwnedPermit<crate::events::AppEvent>,
    },
    Action {
        service_generation: u64,
        identity: ExtensionRequestIdentity,
        action_id: String,
        canceled: Arc<AtomicBool>,
        result_permit: tokio::sync::mpsc::OwnedPermit<crate::events::AppEvent>,
    },
}

#[derive(Debug)]
pub enum ExtensionAsyncResultKind {
    Render(Result<ExtensionView, String>),
    Action(Result<ExtensionHostEffect, String>),
}

#[derive(Debug)]
pub struct ExtensionAsyncResult {
    pub manager_generation: u64,
    pub holds_work_slot: bool,
    pub service_id: String,
    pub service_generation: u64,
    pub granted_capabilities: Vec<crate::api::schema::ExtensionServiceCapability>,
    pub identity: ExtensionRequestIdentity,
    pub kind: ExtensionAsyncResultKind,
}

struct ExtensionWorker {
    _provider_slot: Option<PaneExtensionProviderSlotGuard>,
    join: Option<std::thread::JoinHandle<()>>,
    generation: Arc<std::sync::atomic::AtomicU64>,
    granted_capabilities: Vec<crate::api::schema::ExtensionServiceCapability>,
    requests: mpsc::SyncSender<ExtensionWorkerRequest>,
    result_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
}

struct PaneExtensionProviderSlotGuard;

impl PaneExtensionProviderSlotGuard {
    fn acquire() -> Result<Self, String> {
        PANE_EXTENSION_PROVIDER_SLOTS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < MAX_GLOBAL_PANE_EXTENSION_PROVIDERS).then_some(used + 1)
            })
            .map(|_| Self)
            .map_err(|_| "global pane extension provider limit reached".into())
    }
}

impl Drop for PaneExtensionProviderSlotGuard {
    fn drop(&mut self) {
        PANE_EXTENSION_PROVIDER_SLOTS.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
struct ExtensionWorkSlotGuard {
    armed: bool,
}

fn try_acquire_bounded_slot(
    slots: &std::sync::atomic::AtomicUsize,
    full_message: &str,
) -> Result<(), String> {
    slots
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
            (used < 32).then_some(used + 1)
        })
        .map(|_| ())
        .map_err(|_| full_message.to_string())
}

impl ExtensionWorkSlotGuard {
    fn acquire() -> Result<Self, String> {
        try_acquire_bounded_slot(
            &EXTENSION_WORK_SLOTS,
            "global extension service work queue is full",
        )?;
        Ok(Self { armed: true })
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ExtensionWorkSlotGuard {
    fn drop(&mut self) {
        if self.armed {
            EXTENSION_WORK_SLOTS.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

pub struct ExtensionServiceManager {
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    complete_catalog: bool,
    generation: u64,
    workers: std::collections::BTreeMap<String, ExtensionWorker>,
    provider: Option<String>,
    admissions: Arc<Mutex<std::collections::HashMap<ExtensionRequestIdentity, Arc<AtomicBool>>>>,
    cancellation_tombstones: Arc<Mutex<std::collections::VecDeque<ExtensionRequestIdentity>>>,
}

static NEXT_SERVICE_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
static NEXT_MANAGER_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
static EXTENSION_WORK_SLOTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
static PANE_EXTENSION_PROVIDER_SLOTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
const MAX_PANE_EXTENSION_PROVIDERS: usize = 16;
const MAX_GLOBAL_PANE_EXTENSION_PROVIDERS: usize = MAX_PANE_EXTENSION_PROVIDERS * 2;
static HOST_DOCUMENT_ACTION_SLOTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
static HOST_DOCUMENT_RENDER_SLOTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
static HOST_DOCUMENT_PROVIDER_SLOTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
#[cfg_attr(test, allow(dead_code))]
const MAX_GLOBAL_HOST_DOCUMENT_PROVIDERS: usize =
    crate::extension_contract::MAX_DOCUMENT_PROVIDERS * 2;

#[cfg_attr(test, allow(dead_code))]
fn limit_host_document_specs(
    mut specs: Vec<(
        String,
        ExtensionServiceSpec,
        std::collections::HashSet<crate::extension_contract::HostCapability>,
    )>,
    reserve_bundled: bool,
) -> Vec<(
    String,
    ExtensionServiceSpec,
    std::collections::HashSet<crate::extension_contract::HostCapability>,
)> {
    specs.retain(|(namespace, _, _)| {
        namespace != crate::first_party::collections::NAMESPACE && namespace != "host.fallback"
    });
    let mut namespaces = std::collections::HashSet::new();
    specs.retain(|(namespace, _, _)| namespaces.insert(namespace.clone()));
    specs.truncate(
        crate::extension_contract::MAX_DOCUMENT_PROVIDERS
            .saturating_sub(usize::from(reserve_bundled)),
    );
    specs
}

// Production catalog workers use this cancellation-aware event delivery loop.
#[cfg_attr(test, allow(dead_code))]
fn send_host_document_event(
    pending: &Arc<(Mutex<HostDocumentEventPending>, std::sync::Condvar)>,
    scope_id: String,
    event: crate::events::AppEvent,
) {
    let (lock, ready) = &**pending;
    let mut pending = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    pending.push(scope_id, event);
    ready.notify_one();
}

pub struct HostDocumentServiceCatalog {
    #[cfg_attr(not(test), allow(dead_code))]
    test_render_slots: Vec<Arc<HostDocumentRenderSlotGuard>>,
    complete_catalog: bool,
    generations: std::collections::BTreeMap<String, u64>,
    requests: std::collections::BTreeMap<String, HostDocumentWorkerHandle>,
    event_tx: Option<tokio::sync::mpsc::Sender<crate::events::AppEvent>>,
}

type ActiveRenderSlots =
    Arc<Mutex<std::collections::HashMap<String, (u64, u64, Arc<HostDocumentRenderSlotGuard>)>>>;

struct HostDocumentWorkerHandle {
    _provider_slot: Option<HostDocumentProviderSlotGuard>,
    pending: Arc<(Mutex<HostDocumentPending>, std::sync::Condvar)>,
    desired_revisions: Arc<Mutex<std::collections::HashMap<String, (u64, u64)>>>,
    active_actions: Arc<Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>>,
    active_renders: Arc<Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>>,
    active_render_slots: ActiveRenderSlots,
    event_pending: Option<Arc<(Mutex<HostDocumentEventPending>, std::sync::Condvar)>>,
    revoked: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    event_join: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct HostDocumentEventPending {
    by_scope: std::collections::HashMap<String, crate::events::AppEvent>,
    order: std::collections::VecDeque<String>,
}

impl HostDocumentEventPending {
    fn push(&mut self, scope_id: String, event: crate::events::AppEvent) {
        if !self.by_scope.contains_key(&scope_id) {
            self.order.push_back(scope_id.clone());
        }
        self.by_scope.insert(scope_id, event);
    }

    #[cfg_attr(test, allow(dead_code))]
    fn pop(&mut self) -> Option<(String, crate::events::AppEvent)> {
        while let Some(scope_id) = self.order.pop_front() {
            if let Some(event) = self.by_scope.remove(&scope_id) {
                return Some((scope_id, event));
            }
        }
        None
    }

    fn retry_if_not_superseded(&mut self, scope_id: String, event: crate::events::AppEvent) {
        if !self.by_scope.contains_key(&scope_id) {
            self.order.push_back(scope_id.clone());
            self.by_scope.insert(scope_id, event);
        }
    }
}

#[derive(Default)]
struct HostDocumentPending {
    render_by_scope: std::collections::HashMap<String, HostDocumentRequest>,
    render_order: std::collections::VecDeque<String>,
    actions: std::collections::VecDeque<HostDocumentRequest>,
}

impl HostDocumentPending {
    fn push(&mut self, request: HostDocumentRequest) -> Result<(), String> {
        if request.action.is_some() {
            if self.actions.len() >= 64 {
                return Err("document provider action queue is full".into());
            }
            self.actions.push_back(request);
            return Ok(());
        }
        let scope_id = request.snapshot.scope_id.clone();
        if !self.render_by_scope.contains_key(&scope_id) {
            if self.render_by_scope.len() >= crate::extension_contract::MAX_DOCUMENT_ITEMS {
                return Err("document provider scope queue is full".into());
            }
            self.render_order.push_back(scope_id.clone());
        }
        self.render_by_scope.insert(scope_id, request);
        Ok(())
    }

    fn pop(&mut self) -> Option<HostDocumentRequest> {
        if let Some(action) = self.actions.pop_front() {
            return Some(action);
        }
        while let Some(scope_id) = self.render_order.pop_front() {
            if let Some(request) = self.render_by_scope.remove(&scope_id) {
                return Some(request);
            }
        }
        None
    }

    // Production workers wait on this state; unit tests inspect queued scopes directly.
    #[cfg_attr(test, allow(dead_code))]
    fn is_empty(&self) -> bool {
        self.actions.is_empty() && self.render_by_scope.is_empty()
    }
}

// Unit-test executables cannot recursively host the bundled service binary.
#[cfg_attr(test, allow(dead_code))]
struct HostDocumentProvider {
    priority: i32,
    generation: u64,
    supervisor: ExtensionServiceSupervisor,
    grants: std::collections::HashSet<crate::extension_contract::HostCapability>,
}

// Fields are consumed by the production worker, omitted from unit-test catalogs.
#[cfg_attr(test, allow(dead_code))]
struct HostDocumentRequest {
    namespace: String,
    render_slot: Option<Arc<HostDocumentRenderSlotGuard>>,
    snapshot: crate::extension_contract::HostTerminalGroupsV1,
    action: Option<crate::extension_contract::HostDocumentExtensionActionV1>,
    action_response: Option<mpsc::SyncSender<Result<(), String>>>,
    action_completion: Option<HostDocumentActionCompletion>,
    action_deadline: Option<Instant>,
}

struct HostDocumentProviderSlotGuard;

impl HostDocumentProviderSlotGuard {
    #[cfg_attr(test, allow(dead_code))]
    fn acquire() -> Result<Self, String> {
        HOST_DOCUMENT_PROVIDER_SLOTS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < MAX_GLOBAL_HOST_DOCUMENT_PROVIDERS).then_some(used + 1)
            })
            .map(|_| Self)
            .map_err(|_| "global document provider limit reached".into())
    }
}

impl Drop for HostDocumentProviderSlotGuard {
    fn drop(&mut self) {
        HOST_DOCUMENT_PROVIDER_SLOTS.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
pub(crate) struct HostDocumentRenderSlotGuard;

impl HostDocumentRenderSlotGuard {
    fn acquire() -> Result<Self, String> {
        try_acquire_bounded_slot(
            &HOST_DOCUMENT_RENDER_SLOTS,
            "global document render queue is full",
        )?;
        Ok(Self)
    }
}

impl Drop for HostDocumentRenderSlotGuard {
    fn drop(&mut self) {
        HOST_DOCUMENT_RENDER_SLOTS.fetch_sub(1, Ordering::AcqRel);
    }
}

struct HostDocumentActionCompletion {
    request_id: String,
    namespace: String,
    scope_id: String,
    generation: u64,
    revision: u64,
    respond_to: std::sync::mpsc::Sender<String>,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
}

impl HostDocumentRequest {
    fn action_expired(&self, now: Instant) -> bool {
        self.action_deadline.is_some_and(|deadline| now >= deadline)
    }
}

fn reject_queued_host_document_action(
    message: &str,
    response: Option<mpsc::SyncSender<Result<(), String>>>,
    completion: Option<HostDocumentActionCompletion>,
) {
    if let Some(response) = response {
        let _ = response.send(Err(message.into()));
    }
    if let Some(completion) = completion {
        HOST_DOCUMENT_ACTION_SLOTS.fetch_sub(1, Ordering::AcqRel);
        let response = serde_json::to_string(&crate::api::schema::ErrorResponse {
            id: completion.request_id,
            error: crate::api::schema::ErrorBody {
                code: "host_document_action_rejected".into(),
                message: message.into(),
            },
        })
        .unwrap_or_else(|_| "{}".into());
        let _ = completion.respond_to.send(response);
    }
}

fn host_document_action_is_current(
    desired_revisions: &Mutex<std::collections::HashMap<String, (u64, u64)>>,
    snapshot: &crate::extension_contract::HostTerminalGroupsV1,
) -> bool {
    desired_revisions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&snapshot.scope_id)
        .is_some_and(|desired| *desired == (snapshot.generation, snapshot.revision))
}

fn try_acquire_host_document_action_slot() -> Result<(), String> {
    try_acquire_bounded_slot(
        &HOST_DOCUMENT_ACTION_SLOTS,
        "global document provider action queue is full",
    )
}

fn complete_host_document_action(
    result: Result<(), String>,
    response: Option<mpsc::SyncSender<Result<(), String>>>,
    completion: Option<HostDocumentActionCompletion>,
) {
    if let Some(response) = response {
        let _ = response.send(result.clone());
    }
    if let Some(completion) = completion {
        let event = crate::events::AppEvent::HostDocumentProviderActionFinished {
            request_id: completion.request_id,
            namespace: completion.namespace,
            scope_id: completion.scope_id,
            generation: completion.generation,
            revision: completion.revision,
            holds_action_slot: true,
            result,
            respond_to: completion.respond_to,
        };
        if let Err(error) = completion.event_tx.blocking_send(event) {
            let crate::events::AppEvent::HostDocumentProviderActionFinished {
                request_id,
                holds_action_slot,
                respond_to,
                ..
            } = error.0
            else {
                return;
            };
            let response = serde_json::to_string(&crate::api::schema::ErrorResponse {
                id: request_id,
                error: crate::api::schema::ErrorBody {
                    code: "server_unavailable".into(),
                    message: "document action completion channel closed".into(),
                },
            })
            .unwrap_or_else(|_| "{}".into());
            let _ = respond_to.send(response);
            if holds_action_slot {
                HOST_DOCUMENT_ACTION_SLOTS.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }
}

pub(crate) fn complete_host_document_provider_action() {
    HOST_DOCUMENT_ACTION_SLOTS.fetch_sub(1, Ordering::AcqRel);
}

#[derive(Debug)]
pub struct HostDocumentAsyncResult {
    pub(crate) _render_slot: Option<Arc<HostDocumentRenderSlotGuard>>,
    pub namespace: String,
    pub provider_priority: i32,
    pub snapshot: crate::extension_contract::HostTerminalGroupsV1,
    pub result: Result<crate::extension_contract::HostDocumentV1, String>,
    pub grants: std::collections::HashSet<crate::extension_contract::HostCapability>,
}

impl HostDocumentServiceCatalog {
    pub fn is_complete(&self) -> bool {
        self.complete_catalog
    }

    #[cfg(test)]
    pub fn bundled(
        disabled: &std::collections::BTreeSet<String>,
        plugins: &crate::app::state::InstalledPluginRegistry,
        _event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) -> Self {
        let mut generations = host_document_specs(plugins)
            .into_iter()
            .map(|(namespace, _, _)| {
                (
                    namespace,
                    NEXT_SERVICE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        if !disabled.contains(crate::first_party::collections::NAMESPACE) {
            generations.insert(
                crate::first_party::collections::NAMESPACE.into(),
                NEXT_SERVICE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            );
        }
        Self {
            test_render_slots: Vec::new(),
            complete_catalog: true,
            generations,
            requests: std::collections::BTreeMap::new(),
            event_tx: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_provider(namespace: &str, generation: u64) -> Self {
        Self::with_test_providers(&[(namespace, generation)])
    }

    #[cfg(test)]
    pub(crate) fn with_test_providers(providers: &[(&str, u64)]) -> Self {
        Self {
            test_render_slots: Vec::new(),
            complete_catalog: true,
            generations: providers
                .iter()
                .map(|(namespace, generation)| ((*namespace).into(), *generation))
                .collect(),
            requests: std::collections::BTreeMap::new(),
            event_tx: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_saturated_render_provider(namespace: &str, generation: u64) -> Self {
        let active_render_slots = Arc::new(Mutex::new(std::collections::HashMap::new()));
        Self {
            test_render_slots: (0..32)
                .map(|_| Arc::new(HostDocumentRenderSlotGuard::acquire().unwrap()))
                .collect(),
            complete_catalog: true,
            generations: std::collections::BTreeMap::from([(namespace.into(), generation)]),
            requests: std::collections::BTreeMap::from([(
                namespace.into(),
                HostDocumentWorkerHandle {
                    _provider_slot: None,
                    pending: Arc::new((
                        Mutex::new(HostDocumentPending::default()),
                        std::sync::Condvar::new(),
                    )),
                    desired_revisions: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_actions: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_renders: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_render_slots,
                    event_pending: None,
                    revoked: Arc::new(AtomicBool::new(false)),
                    join: None,
                    event_join: None,
                },
            )]),
            event_tx: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn release_test_render_slot(&mut self) {
        self.test_render_slots.pop();
    }

    #[cfg(test)]
    pub(crate) fn pending_render_count_for_test(&self, namespace: &str) -> usize {
        self.requests
            .get(namespace)
            .map(|handle| handle.pending.0.lock().unwrap().render_by_scope.len())
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn with_test_action_sink(namespace: &str, generation: u64) -> Self {
        Self::with_test_action_result(namespace, generation, Ok(()))
    }

    #[cfg(test)]
    pub(crate) fn with_test_action_result(
        namespace: &str,
        generation: u64,
        action_result: Result<(), String>,
    ) -> Self {
        let pending = Arc::new((
            Mutex::new(HostDocumentPending::default()),
            std::sync::Condvar::new(),
        ));
        let revoked = Arc::new(AtomicBool::new(false));
        let worker_pending = pending.clone();
        let worker_revoked = revoked.clone();
        let join = std::thread::spawn(move || {
            let (lock, ready) = &*worker_pending;
            let mut queue = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            while queue.is_empty() && !worker_revoked.load(Ordering::Acquire) {
                queue = ready
                    .wait(queue)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            if let Some(request) = queue.pop() {
                if let Some(response) = request.action_response {
                    let _ = response.send(action_result);
                }
            }
        });
        Self {
            test_render_slots: Vec::new(),
            complete_catalog: true,
            generations: std::collections::BTreeMap::from([(namespace.into(), generation)]),
            requests: std::collections::BTreeMap::from([(
                namespace.into(),
                HostDocumentWorkerHandle {
                    _provider_slot: None,
                    pending,
                    desired_revisions: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_actions: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_renders: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_render_slots: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    event_pending: None,
                    revoked,
                    join: Some(join),
                    event_join: None,
                },
            )]),
            event_tx: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_delayed_action(
        namespace: &str,
        generation: u64,
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
        delay: Duration,
    ) -> Self {
        let pending = Arc::new((
            Mutex::new(HostDocumentPending::default()),
            std::sync::Condvar::new(),
        ));
        let revoked = Arc::new(AtomicBool::new(false));
        let worker_pending = pending.clone();
        let worker_revoked = revoked.clone();
        let join = std::thread::spawn(move || {
            let (lock, ready) = &*worker_pending;
            let mut queue = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            while queue.is_empty() && !worker_revoked.load(Ordering::Acquire) {
                queue = ready
                    .wait(queue)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            let request = queue.pop();
            drop(queue);
            if let Some(request) = request {
                std::thread::sleep(delay);
                complete_host_document_action(
                    Ok(()),
                    request.action_response,
                    request.action_completion,
                );
            }
        });
        Self {
            test_render_slots: Vec::new(),
            complete_catalog: true,
            generations: std::collections::BTreeMap::from([(namespace.into(), generation)]),
            requests: std::collections::BTreeMap::from([(
                namespace.into(),
                HostDocumentWorkerHandle {
                    _provider_slot: None,
                    pending,
                    desired_revisions: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_actions: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_renders: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_render_slots: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    event_pending: None,
                    revoked,
                    join: Some(join),
                    event_join: None,
                },
            )]),
            event_tx: Some(event_tx),
        }
    }

    #[cfg(not(test))]
    pub fn bundled(
        disabled: &std::collections::BTreeSet<String>,
        plugins: &crate::app::state::InstalledPluginRegistry,
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) -> Self {
        let mut providers = std::collections::BTreeMap::new();
        // Reserve one public namespace for the host's generic fallback even when the
        // product-specific Collections provider is disabled.
        let reserve_host_fallback = true;
        for (namespace, candidate, grants) in
            limit_host_document_specs(host_document_specs(plugins), reserve_host_fallback)
        {
            if providers.contains_key(&namespace) {
                continue;
            }
            let priority = candidate.priority;
            if let Ok(supervisor) = ExtensionServiceSupervisor::new(candidate) {
                providers.insert(
                    namespace,
                    HostDocumentProvider {
                        priority,
                        generation: NEXT_SERVICE_GENERATION
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                        supervisor,
                        grants,
                    },
                );
            }
        }
        let namespace = crate::first_party::collections::NAMESPACE;
        if !disabled.contains(namespace) {
            if let Ok(exe) = std::env::current_exe() {
                let spec = ExtensionServiceSpec {
                    id: namespace.into(),
                    command: vec![
                        exe.display().to_string(),
                        "_extension-service".into(),
                        "collections".into(),
                    ],
                    cwd: std::env::current_dir().unwrap_or_default(),
                    priority: 100,
                    granted_capabilities: vec![
                        crate::api::schema::ExtensionServiceCapability::HostDocument,
                        crate::api::schema::ExtensionServiceCapability::FocusTerminal,
                    ],
                };
                if let Ok(supervisor) = ExtensionServiceSupervisor::new(spec) {
                    providers.insert(
                        namespace.into(),
                        HostDocumentProvider {
                            priority: 100,
                            generation: NEXT_SERVICE_GENERATION
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                            supervisor,
                            grants: std::collections::HashSet::from([
                                crate::extension_contract::HostCapability::FocusTerminal,
                            ]),
                        },
                    );
                }
            }
        }
        let mut generations: std::collections::BTreeMap<String, u64> = providers
            .iter()
            .map(|(namespace, provider)| (namespace.clone(), provider.generation))
            .collect();
        let expected_provider_count = generations.len();
        let mut requests = std::collections::BTreeMap::new();
        for (namespace, mut provider) in providers {
            let Ok(provider_slot) = HostDocumentProviderSlotGuard::acquire() else {
                generations.remove(&namespace);
                tracing::warn!(%namespace, "global document provider limit reached; provider omitted");
                continue;
            };
            let pending = Arc::new((
                Mutex::new(HostDocumentPending::default()),
                std::sync::Condvar::new(),
            ));
            let desired_revisions = Arc::new(Mutex::new(std::collections::HashMap::new()));
            let active_actions = Arc::new(Mutex::new(std::collections::HashMap::new()));
            let active_renders = Arc::new(Mutex::new(std::collections::HashMap::new()));
            let active_render_slots = Arc::new(Mutex::new(std::collections::HashMap::new()));
            let revoked = Arc::new(AtomicBool::new(false));
            let event_pending = Arc::new((
                Mutex::new(HostDocumentEventPending::default()),
                std::sync::Condvar::new(),
            ));
            let publisher_pending = event_pending.clone();
            let publisher_revoked = revoked.clone();
            let publisher_tx = event_tx.clone();
            let event_join = std::thread::Builder::new()
                .name(format!("vqro-host-document-events-{namespace}"))
                .spawn(move || loop {
                    let item = {
                        let (lock, ready) = &*publisher_pending;
                        let mut queue =
                            lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                        while queue.by_scope.is_empty()
                            && !publisher_revoked.load(Ordering::Acquire)
                        {
                            queue = ready
                                .wait(queue)
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                        }
                        if publisher_revoked.load(Ordering::Acquire) {
                            return;
                        }
                        queue.pop()
                    };
                    let Some((scope_id, event)) = item else {
                        continue;
                    };
                    match publisher_tx.try_send(event) {
                        Ok(()) => {}
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return,
                        Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => {
                            let (lock, ready) = &*publisher_pending;
                            lock.lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .retry_if_not_superseded(scope_id, event);
                            ready.notify_one();
                            std::thread::sleep(Duration::from_millis(1));
                        }
                    }
                })
                .ok();
            let Some(event_join) = event_join else {
                continue;
            };
            let worker_pending = pending.clone();
            let worker_desired_revisions = desired_revisions.clone();
            let worker_active_actions = active_actions.clone();
            let worker_active_renders = active_renders.clone();
            let worker_active_render_slots = active_render_slots.clone();
            let worker_revoked = revoked.clone();
            let worker_events = event_pending.clone();
            let thread_name = format!("vqro-host-document-{namespace}");
            if let Ok(join) = std::thread::Builder::new()
                .name(thread_name)
                .spawn(move || loop {
                    let request = {
                        let (lock, ready) = &*worker_pending;
                        let mut queue =
                            lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                        while queue.is_empty() && !worker_revoked.load(Ordering::Acquire) {
                            queue = ready
                                .wait(queue)
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                        }
                        if worker_revoked.load(Ordering::Acquire) {
                            return;
                        }
                        queue.pop()
                    };
                    let Some(request) = request else {
                        continue;
                    };
                    if worker_revoked.load(Ordering::Acquire) {
                        if request.action.is_some() {
                            complete_host_document_action(
                                Err("document provider was revoked".into()),
                                request.action_response,
                                request.action_completion,
                            );
                        }
                        return;
                    }
                    let action_expired = request.action_expired(Instant::now());
                    if let Some(action) = request.action {
                        let action_response = request.action_response;
                        let action_completion = request.action_completion;
                        if action_expired {
                            complete_host_document_action(
                                Err("document provider action expired before execution".into()),
                                action_response,
                                action_completion,
                            );
                            continue;
                        }
                        if worker_revoked.load(Ordering::Acquire) {
                            complete_host_document_action(
                                Err("document provider was revoked".into()),
                                action_response,
                                action_completion,
                            );
                            return;
                        }
                        let desired_is_current = host_document_action_is_current(
                            &worker_desired_revisions,
                            &request.snapshot,
                        );
                        let result = if request.snapshot.generation == provider.generation
                            && desired_is_current
                        {
                            let action_canceled = Arc::new(AtomicBool::new(false));
                            worker_active_actions
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .insert(request.snapshot.scope_id.clone(), action_canceled.clone());
                            let result = if !worker_revoked.load(Ordering::Acquire)
                                && host_document_action_is_current(
                                    &worker_desired_revisions,
                                    &request.snapshot,
                                ) {
                                provider
                                    .supervisor
                                    .invoke_host_document_action_cancellable(
                                        &action,
                                        &action_canceled,
                                    )
                                    .map_err(|error| error.to_string())
                            } else {
                                Err("document action revision was superseded".into())
                            };
                            if worker_revoked.load(Ordering::Acquire) {
                                Err("document provider was retired".into())
                            } else if host_document_action_is_current(
                                &worker_desired_revisions,
                                &request.snapshot,
                            ) {
                                result
                            } else {
                                Err("document action revision was superseded".into())
                            }
                        } else {
                            Err("document action generation or revision is stale".into())
                        };
                        let action_scope_id = request.snapshot.scope_id.clone();
                        complete_host_document_action(result, action_response, action_completion);
                        worker_active_actions
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .remove(&action_scope_id);
                        continue;
                    }
                    let result = if request.snapshot.generation != provider.generation
                        || !host_document_action_is_current(
                            &worker_desired_revisions,
                            &request.snapshot,
                        ) {
                        Err("document snapshot generation or revision is stale".into())
                    } else {
                        let render_canceled = Arc::new(AtomicBool::new(false));
                        worker_active_renders
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .insert(request.snapshot.scope_id.clone(), render_canceled.clone());
                        if let Some(slot) = request.render_slot.as_ref() {
                            worker_active_render_slots
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .insert(
                                    request.snapshot.scope_id.clone(),
                                    (
                                        request.snapshot.generation,
                                        request.snapshot.revision,
                                        slot.clone(),
                                    ),
                                );
                        }
                        let result =
                            if worker_revoked.load(Ordering::Acquire)
                                || !host_document_action_is_current(
                                    &worker_desired_revisions,
                                    &request.snapshot,
                                )
                            {
                                Err("document snapshot revision was superseded".into())
                            } else {
                                provider
                            .supervisor
                            .render_host_document_cancellable(&request.snapshot, &render_canceled)
                            .map_err(|error| error.to_string())
                            .and_then(|document| {
                                if document.namespace == request.namespace
                                    && document.generation == provider.generation
                                    && document.revision == request.snapshot.revision
                                    && matches!(
                                        &document.target,
                                        crate::extension_contract::HostTarget::Tab { tab_id }
                                            if tab_id == &request.snapshot.scope_id
                                    )
                                {
                                    Ok(document)
                                } else {
                                    Err("document provider identity or generation mismatch".into())
                                }
                            })
                            };
                        worker_active_renders
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .remove(&request.snapshot.scope_id);
                        if host_document_action_is_current(
                            &worker_desired_revisions,
                            &request.snapshot,
                        ) {
                            result
                        } else {
                            Err("document snapshot revision was superseded".into())
                        }
                    };
                    let scope_id = request.snapshot.scope_id.clone();
                    send_host_document_event(
                        &worker_events,
                        scope_id.clone(),
                        crate::events::AppEvent::HostDocumentFinished(Box::new(
                            HostDocumentAsyncResult {
                                _render_slot: request.render_slot,
                                namespace: request.namespace,
                                provider_priority: provider.priority,
                                snapshot: request.snapshot,
                                result,
                                grants: provider.grants.clone(),
                            },
                        )),
                    );
                })
            {
                requests.insert(
                    namespace,
                    HostDocumentWorkerHandle {
                        _provider_slot: Some(provider_slot),
                        pending,
                        desired_revisions,
                        active_actions,
                        active_renders,
                        active_render_slots,
                        event_pending: Some(event_pending),
                        revoked,
                        join: Some(join),
                        event_join: Some(event_join),
                    },
                );
            } else {
                revoked.store(true, Ordering::Release);
                let (_, ready) = &*event_pending;
                ready.notify_all();
                let _ = event_join.join();
            }
        }
        generations.retain(|namespace, _| requests.contains_key(namespace));
        Self {
            test_render_slots: Vec::new(),
            complete_catalog: requests.len() == expected_provider_count,
            generations,
            requests,
            event_tx: Some(event_tx),
        }
    }

    pub fn namespaces(&self) -> impl Iterator<Item = &str> {
        self.generations.keys().map(String::as_str)
    }

    pub fn generation(&self, namespace: &str) -> Option<u64> {
        self.generations.get(namespace).copied()
    }

    #[cfg(test)]
    pub(crate) fn set_desired_revision_for_test(
        &self,
        namespace: &str,
        scope_id: String,
        generation: u64,
        revision: u64,
    ) {
        if let Some(handle) = self.requests.get(namespace) {
            handle
                .desired_revisions
                .lock()
                .unwrap()
                .insert(scope_id, (generation, revision));
        }
    }

    #[cfg(test)]
    pub fn request_action(
        &self,
        snapshot: crate::extension_contract::HostTerminalGroupsV1,
        action: crate::extension_contract::HostDocumentExtensionActionV1,
    ) -> Result<mpsc::Receiver<Result<(), String>>, String> {
        let namespace = action.namespace.clone();
        let generation = self
            .generation(&namespace)
            .ok_or_else(|| format!("document provider {namespace} is disabled or unavailable"))?;
        if snapshot.generation != generation || action.generation != generation {
            return Err("document action generation is stale".into());
        }
        let (response_tx, response_rx) = mpsc::sync_channel(1);
        self.enqueue(
            &namespace,
            HostDocumentRequest {
                namespace: namespace.clone(),
                render_slot: None,
                snapshot,
                action: Some(action),
                action_response: Some(response_tx),
                action_completion: None,
                action_deadline: Some(Instant::now() + HANDSHAKE_TIMEOUT + REQUEST_TIMEOUT),
            },
        )?;
        Ok(response_rx)
    }

    pub fn request_action_deferred(
        &self,
        snapshot: crate::extension_contract::HostTerminalGroupsV1,
        action: crate::extension_contract::HostDocumentExtensionActionV1,
        request_id: String,
        respond_to: std::sync::mpsc::Sender<String>,
    ) -> Result<(), String> {
        let namespace = action.namespace.clone();
        let generation = self
            .generation(&namespace)
            .ok_or_else(|| format!("document provider {namespace} is disabled or unavailable"))?;
        if snapshot.generation != generation || action.generation != generation {
            return Err("document action generation is stale".into());
        }
        let event_tx = self
            .event_tx
            .as_ref()
            .ok_or_else(|| "document provider completion channel is unavailable".to_string())?;
        let handle = self
            .requests
            .get(&namespace)
            .ok_or_else(|| "document provider worker is unavailable".to_string())?;
        if !host_document_action_is_current(&handle.desired_revisions, &snapshot) {
            return Err("document action revision is stale".into());
        }
        try_acquire_host_document_action_slot()?;
        let scope_id = snapshot.scope_id.clone();
        let revision = snapshot.revision;
        let enqueue = self.enqueue(
            &namespace,
            HostDocumentRequest {
                namespace: namespace.clone(),
                render_slot: None,
                snapshot,
                action: Some(action),
                action_response: None,
                action_completion: Some(HostDocumentActionCompletion {
                    request_id,
                    namespace: namespace.clone(),
                    scope_id,
                    generation,
                    revision,
                    respond_to,
                    event_tx: event_tx.clone(),
                }),
                action_deadline: Some(Instant::now() + HANDSHAKE_TIMEOUT + REQUEST_TIMEOUT),
            },
        );
        if enqueue.is_err() {
            HOST_DOCUMENT_ACTION_SLOTS.fetch_sub(1, Ordering::AcqRel);
        }
        enqueue
    }

    pub fn retain_scopes(&self, live_scope_ids: &std::collections::HashSet<String>) {
        for handle in self.requests.values() {
            handle
                .desired_revisions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .retain(|scope_id, _| live_scope_ids.contains(scope_id));
            for active in [&handle.active_actions, &handle.active_renders] {
                for (scope_id, canceled) in active
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .iter()
                {
                    if !live_scope_ids.contains(scope_id) {
                        canceled.store(true, Ordering::Release);
                    }
                }
            }
            handle
                .active_render_slots
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .retain(|scope_id, _| live_scope_ids.contains(scope_id));
            let mut pending = handle
                .pending
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut retained = std::collections::VecDeque::new();
            while let Some(request) = pending.actions.pop_front() {
                if live_scope_ids.contains(&request.snapshot.scope_id) {
                    retained.push_back(request);
                } else {
                    reject_queued_host_document_action(
                        "document action scope was retired",
                        request.action_response,
                        request.action_completion,
                    );
                }
            }
            pending.actions = retained;
            pending
                .render_by_scope
                .retain(|scope_id, _| live_scope_ids.contains(scope_id));
            pending
                .render_order
                .retain(|scope_id| live_scope_ids.contains(scope_id));
        }
    }

    pub fn complete_render(&self, namespace: &str, scope_id: &str, generation: u64, revision: u64) {
        let Some(handle) = self.requests.get(namespace) else {
            return;
        };
        let mut slots = handle
            .active_render_slots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slots
            .get(scope_id)
            .is_some_and(|(slot_generation, slot_revision, _)| {
                (*slot_generation, *slot_revision) == (generation, revision)
            })
        {
            slots.remove(scope_id);
        }
    }

    pub fn request(
        &self,
        namespace: &str,
        snapshot: crate::extension_contract::HostTerminalGroupsV1,
    ) -> Result<(), String> {
        let generation = self
            .generation(namespace)
            .ok_or_else(|| format!("document provider {namespace} is disabled or unavailable"))?;
        if snapshot.generation != generation {
            return Err("document snapshot generation is stale".into());
        }
        let handle = self
            .requests
            .get(namespace)
            .ok_or_else(|| "document provider worker is unavailable".to_string())?;
        let render_slot = match HostDocumentRenderSlotGuard::acquire() {
            Ok(slot) => Arc::new(slot),
            Err(error) => handle
                .pending
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .render_by_scope
                .get(&snapshot.scope_id)
                .and_then(|request| request.render_slot.clone())
                .or_else(|| {
                    handle
                        .active_render_slots
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .get(&snapshot.scope_id)
                        .map(|(_, _, slot)| slot.clone())
                })
                .ok_or(error)?,
        };
        let desired = (snapshot.generation, snapshot.revision);
        handle
            .desired_revisions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(snapshot.scope_id.clone(), desired);
        for active in [&handle.active_actions, &handle.active_renders] {
            if let Some(canceled) = active
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&snapshot.scope_id)
            {
                canceled.store(true, Ordering::Release);
            }
        }
        {
            let mut pending = handle
                .pending
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut retained = std::collections::VecDeque::new();
            while let Some(request) = pending.actions.pop_front() {
                if request.snapshot.scope_id != snapshot.scope_id
                    || (request.snapshot.generation, request.snapshot.revision) == desired
                {
                    retained.push_back(request);
                } else {
                    reject_queued_host_document_action(
                        "document action revision was superseded",
                        request.action_response,
                        request.action_completion,
                    );
                }
            }
            pending.actions = retained;
        }
        self.enqueue(
            namespace,
            HostDocumentRequest {
                namespace: namespace.into(),
                render_slot: Some(render_slot),
                snapshot,
                action: None,
                action_response: None,
                action_completion: None,
                action_deadline: None,
            },
        )
    }

    fn enqueue(&self, namespace: &str, request: HostDocumentRequest) -> Result<(), String> {
        let handle = self
            .requests
            .get(namespace)
            .ok_or_else(|| "document provider worker is unavailable".to_string())?;
        if handle.revoked.load(Ordering::Acquire) {
            return Err("document provider was revoked".into());
        }
        let (lock, ready) = &*handle.pending;
        let mut pending = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        pending.push(request)?;
        ready.notify_one();
        Ok(())
    }
}

impl Drop for HostDocumentServiceCatalog {
    fn drop(&mut self) {
        for handle in self.requests.values() {
            handle.revoked.store(true, Ordering::Release);
            for active in [&handle.active_actions, &handle.active_renders] {
                for canceled in active
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .values()
                {
                    canceled.store(true, Ordering::Release);
                }
            }
            let (lock, ready) = &*handle.pending;
            let mut pending = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for request in pending.actions.drain(..) {
                reject_queued_host_document_action(
                    "document provider was revoked",
                    request.action_response,
                    request.action_completion,
                );
            }
            pending.render_by_scope.clear();
            pending.render_order.clear();
            ready.notify_all();
            if let Some(event_pending) = &handle.event_pending {
                let (event_lock, event_ready) = &**event_pending;
                event_lock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .by_scope
                    .clear();
                event_ready.notify_all();
            }
        }
        let mut joins = Vec::new();
        let mut provider_slots = Vec::new();
        for handle in self.requests.values_mut() {
            joins.extend(handle.join.take());
            joins.extend(handle.event_join.take());
            provider_slots.extend(handle._provider_slot.take());
        }
        if !joins.is_empty() || !provider_slots.is_empty() {
            // Teardown has already been revoked and queued effects have been rejected. Reap
            // workers away from the App/server actor so process termination cannot freeze PTYs.
            let payload = Arc::new(Mutex::new(Some((
                joins,
                provider_slots,
                self.event_tx.clone(),
            ))));
            let thread_payload = payload.clone();
            let spawn = std::thread::Builder::new()
                .name("vqro-host-document-reaper".into())
                .spawn(move || {
                    let Some((joins, provider_slots, event_tx)) = thread_payload
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .take()
                    else {
                        return;
                    };
                    for join in joins {
                        let _ = join.join();
                    }
                    drop(provider_slots);
                    if let Some(event_tx) = event_tx {
                        let _ = event_tx.blocking_send(
                            crate::events::AppEvent::ExtensionCatalogCapacityAvailable,
                        );
                    }
                });
            if spawn.is_err() {
                // Fail closed: leaking the guards is preferable to claiming capacity while
                // detached provider processes may still be alive.
                std::mem::forget(payload);
            }
        }
    }
}

fn host_document_specs(
    plugins: &crate::app::state::InstalledPluginRegistry,
) -> Vec<(
    String,
    ExtensionServiceSpec,
    std::collections::HashSet<crate::extension_contract::HostCapability>,
)> {
    let mut specs = Vec::new();
    for plugin in plugins.values().filter(|plugin| plugin.enabled) {
        if !supports_current_platform(plugin.platforms.as_deref()) {
            continue;
        }
        for service in &plugin.services {
            if service
                .provides
                .contains(&crate::api::schema::PluginServiceCapability::HostDocument)
                && supports_current_platform(service.platforms.as_deref())
            {
                specs.push((
                    plugin.plugin_id.clone(),
                    ExtensionServiceSpec {
                        id: format!("{}.{}", plugin.plugin_id, service.id),
                        command: service.command.clone(),
                        cwd: std::path::PathBuf::from(&plugin.plugin_root),
                        priority: service.priority,
                        granted_capabilities: service.granted_capabilities.clone(),
                    },
                    service
                        .granted_capabilities
                        .iter()
                        .filter_map(protocol_grant_to_host_capability)
                        .collect(),
                ));
            }
        }
    }
    specs.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| right.1.priority.cmp(&left.1.priority))
            .then_with(|| left.1.id.cmp(&right.1.id))
    });
    specs
}

fn protocol_grant_to_host_capability(
    capability: &crate::api::schema::ExtensionServiceCapability,
) -> Option<crate::extension_contract::HostCapability> {
    match capability {
        crate::api::schema::ExtensionServiceCapability::FocusTerminal => {
            Some(crate::extension_contract::HostCapability::FocusTerminal)
        }
        crate::api::schema::ExtensionServiceCapability::TerminalInterrupt => {
            Some(crate::extension_contract::HostCapability::TerminalInterrupt)
        }
        crate::api::schema::ExtensionServiceCapability::ViewContribution
        | crate::api::schema::ExtensionServiceCapability::HostDocument
        | crate::api::schema::ExtensionServiceCapability::Unknown => None,
    }
}

pub fn manager_for_plugins(
    plugins: &crate::app::state::InstalledPluginRegistry,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
) -> ExtensionServiceManager {
    let mut specs = Vec::new();
    for plugin in plugins.values().filter(|plugin| plugin.enabled) {
        if !supports_current_platform(plugin.platforms.as_deref()) {
            continue;
        }
        for service in &plugin.services {
            if !service
                .provides
                .contains(&crate::api::schema::PluginServiceCapability::PaneInspector)
                || !supports_current_platform(service.platforms.as_deref())
            {
                continue;
            }
            specs.push(ExtensionServiceSpec {
                id: format!("{}.{}", plugin.plugin_id, service.id),
                command: service.command.clone(),
                cwd: std::path::PathBuf::from(&plugin.plugin_root),
                priority: service.priority,
                granted_capabilities: service.granted_capabilities.clone(),
            });
        }
    }
    manager_for_specs(specs, event_tx)
}

fn manager_for_specs(
    mut specs: Vec<ExtensionServiceSpec>,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
) -> ExtensionServiceManager {
    specs.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.id.cmp(&right.id))
    });
    specs.truncate(MAX_PANE_EXTENSION_PROVIDERS);
    let expected_provider_count = specs.len();
    let manager_generation =
        NEXT_MANAGER_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut provider = None;
    let mut workers = std::collections::BTreeMap::new();
    let admissions = Arc::new(Mutex::new(std::collections::HashMap::new()));
    let cancellation_tombstones = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    for spec in specs {
        let Ok(provider_slot) = PaneExtensionProviderSlotGuard::acquire() else {
            tracing::warn!(service_id = %spec.id, "global pane extension provider limit reached; provider omitted");
            continue;
        };
        let Ok(mut supervisor) = ExtensionServiceSupervisor::new(spec) else {
            tracing::warn!("ignoring invalid extension service specification");
            continue;
        };
        let generation = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(
            NEXT_SERVICE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        let thread_generation = generation.clone();
        let granted_capabilities = supervisor.spec.granted_capabilities.clone();
        let service_id = supervisor.spec.id.clone();
        let (request_tx, request_rx) = mpsc::sync_channel(8);
        let thread_admissions = admissions.clone();
        let thread_service_id = service_id.clone();
        let thread_manager_generation = manager_generation;
        let thread_grants = granted_capabilities.clone();
        let spawn = std::thread::Builder::new()
            .name(format!("vqro-ext-{}", service_id))
            .spawn(move || {
                while let Ok(request) = request_rx.recv() {
                    let (service_generation, identity, canceled, result_permit, kind) =
                        match request {
                            ExtensionWorkerRequest::Render {
                                service_generation,
                                identity,
                                context,
                                canceled,
                                result_permit,
                            } => {
                                if canceled.load(Ordering::Acquire) {
                                    EXTENSION_WORK_SLOTS.fetch_sub(1, Ordering::AcqRel);
                                    remove_admission(&thread_admissions, &identity);
                                    continue;
                                }
                                let result = supervisor
                                    .render_view_cancellable(context, &canceled)
                                    .map_err(|error| error.to_string());
                                (
                                    service_generation,
                                    identity,
                                    canceled,
                                    result_permit,
                                    ExtensionAsyncResultKind::Render(result),
                                )
                            }
                            ExtensionWorkerRequest::Action {
                                service_generation,
                                identity,
                                action_id,
                                canceled,
                                result_permit,
                            } => {
                                if canceled.load(Ordering::Acquire) {
                                    EXTENSION_WORK_SLOTS.fetch_sub(1, Ordering::AcqRel);
                                    remove_admission(&thread_admissions, &identity);
                                    continue;
                                }
                                let result = supervisor
                                    .invoke_action_cancellable(
                                        &action_id,
                                        &identity.target_pane_id,
                                        &canceled,
                                    )
                                    .map_err(|error| error.to_string());
                                (
                                    service_generation,
                                    identity,
                                    canceled,
                                    result_permit,
                                    ExtensionAsyncResultKind::Action(result),
                                )
                            }
                        };
                    let retired_runtime = supervisor.runtime.is_none()
                        && matches!(
                            &kind,
                            ExtensionAsyncResultKind::Render(Err(_))
                                | ExtensionAsyncResultKind::Action(Err(_))
                        );
                    if retired_runtime {
                        let next = NEXT_SERVICE_GENERATION
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let _ = thread_generation.compare_exchange(
                            service_generation,
                            next,
                            std::sync::atomic::Ordering::AcqRel,
                            std::sync::atomic::Ordering::Acquire,
                        );
                    }
                    if canceled.load(Ordering::Acquire) {
                        EXTENSION_WORK_SLOTS.fetch_sub(1, Ordering::AcqRel);
                        remove_admission(&thread_admissions, &identity);
                        continue;
                    }
                    let result = ExtensionAsyncResult {
                        manager_generation: thread_manager_generation,
                        holds_work_slot: true,
                        service_id: thread_service_id.clone(),
                        service_generation,
                        granted_capabilities: thread_grants.clone(),
                        identity,
                        kind,
                    };
                    result_permit.send(crate::events::AppEvent::ExtensionServiceFinished(
                        Box::new(result),
                    ));
                }
            });
        let join = match spawn {
            Ok(join) => join,
            Err(error) => {
                tracing::warn!(%service_id, %error, "failed to start extension service worker");
                continue;
            }
        };
        if provider.is_none() {
            provider = Some(service_id.clone());
        }
        workers.insert(
            service_id,
            ExtensionWorker {
                _provider_slot: Some(provider_slot),
                join: Some(join),
                generation,
                granted_capabilities,
                requests: request_tx,
                result_tx: event_tx.clone(),
            },
        );
    }
    ExtensionServiceManager {
        event_tx,
        complete_catalog: workers.len() == expected_provider_count,
        generation: manager_generation,
        workers,
        provider,
        admissions,
        cancellation_tombstones,
    }
}

fn remove_admission(
    admissions: &Mutex<std::collections::HashMap<ExtensionRequestIdentity, Arc<AtomicBool>>>,
    identity: &ExtensionRequestIdentity,
) {
    if let Ok(mut admissions) = admissions.lock() {
        admissions.remove(identity);
    }
}

fn remove_cancellation_tombstone(
    tombstones: &Mutex<std::collections::VecDeque<ExtensionRequestIdentity>>,
    identity: &ExtensionRequestIdentity,
) {
    if let Ok(mut tombstones) = tombstones.lock() {
        tombstones.retain(|tombstone| tombstone != identity);
    }
}

#[cfg(test)]
pub(crate) fn manager_for_test_spec(
    spec: ExtensionServiceSpec,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
) -> ExtensionServiceManager {
    manager_for_specs(vec![spec], event_tx)
}

impl ExtensionServiceManager {
    pub fn is_complete(&self) -> bool {
        self.complete_catalog
    }

    pub fn complete(&self, identity: &ExtensionRequestIdentity) {
        remove_admission(&self.admissions, identity);
        remove_cancellation_tombstone(&self.cancellation_tombstones, identity);
    }

    pub fn consume_completion_slot() {
        EXTENSION_WORK_SLOTS.fetch_sub(1, Ordering::AcqRel);
    }

    pub fn manager_generation(&self) -> u64 {
        self.generation
    }

    #[cfg(test)]
    pub(crate) fn admit_for_test(&self, identity: ExtensionRequestIdentity) {
        EXTENSION_WORK_SLOTS.fetch_add(1, Ordering::AcqRel);
        self.admissions
            .lock()
            .unwrap()
            .insert(identity, Arc::new(AtomicBool::new(false)));
    }

    #[cfg(test)]
    pub(crate) fn is_admitted_for_test(&self, identity: &ExtensionRequestIdentity) -> bool {
        self.admissions.lock().unwrap().contains_key(identity)
    }

    pub fn matches_manager_generation(&self, generation: u64) -> bool {
        self.manager_generation() == generation
    }

    pub fn provider_identity(&self) -> Option<(String, u64)> {
        let service_id = self.provider.as_ref()?;
        let worker = self.workers.get(service_id)?;
        Some((
            service_id.clone(),
            worker.generation.load(Ordering::Acquire),
        ))
    }

    pub fn request_view(
        &self,
        identity: ExtensionRequestIdentity,
        context: serde_json::Value,
    ) -> Result<(String, u64), String> {
        let service_id = self
            .provider
            .as_ref()
            .ok_or_else(|| "no pane contribution provider is enabled".to_owned())?;
        let worker = self
            .workers
            .get(service_id)
            .ok_or_else(|| "pane contribution provider is unavailable".to_owned())?;
        if !worker
            .granted_capabilities
            .contains(&crate::api::schema::ExtensionServiceCapability::ViewContribution)
        {
            return Err("view_contribution was not granted".into());
        }
        let service_generation = worker.generation.load(std::sync::atomic::Ordering::Acquire);
        let mut work_slot = ExtensionWorkSlotGuard::acquire()?;
        let result_permit = worker
            .result_tx
            .clone()
            .try_reserve_owned()
            .map_err(|error| format!("extension result queue unavailable: {error}"))?;
        let canceled = Arc::new(AtomicBool::new(false));
        {
            if self
                .cancellation_tombstones
                .lock()
                .map_err(|_| "extension cancellation registry is unavailable".to_owned())?
                .iter()
                .any(|tombstone| tombstone == &identity)
            {
                return Err("duplicate extension request identity".into());
            }
            let mut admissions = self
                .admissions
                .lock()
                .map_err(|_| "extension admission registry is unavailable".to_owned())?;
            if admissions.contains_key(&identity) {
                return Err("duplicate extension request identity".into());
            }
            if admissions.iter().any(|(pending, canceled)| {
                pending.client_id == identity.client_id && !canceled.load(Ordering::Acquire)
            }) {
                return Err("client already has extension work pending".into());
            }
            admissions.insert(identity.clone(), canceled.clone());
        }
        if let Err(error) = worker.requests.try_send(ExtensionWorkerRequest::Render {
            service_generation,
            identity: identity.clone(),
            context,
            canceled,
            result_permit,
        }) {
            remove_admission(&self.admissions, &identity);
            return Err(format!("extension service queue unavailable: {error}"));
        }
        work_slot.disarm();
        Ok((service_id.clone(), service_generation))
    }

    pub fn request_action(
        &self,
        service_id: &str,
        service_generation: u64,
        identity: ExtensionRequestIdentity,
        action_id: String,
    ) -> Result<(), String> {
        let worker = self
            .workers
            .get(service_id)
            .filter(|worker| {
                worker.generation.load(std::sync::atomic::Ordering::Acquire) == service_generation
            })
            .ok_or_else(|| "extension service generation is stale".to_owned())?;
        let mut work_slot = ExtensionWorkSlotGuard::acquire()?;
        let result_permit = worker
            .result_tx
            .clone()
            .try_reserve_owned()
            .map_err(|error| format!("extension result queue unavailable: {error}"))?;
        let canceled = Arc::new(AtomicBool::new(false));
        {
            if self
                .cancellation_tombstones
                .lock()
                .map_err(|_| "extension cancellation registry is unavailable".to_owned())?
                .iter()
                .any(|tombstone| tombstone == &identity)
            {
                return Err("duplicate extension request identity".into());
            }
            let mut admissions = self
                .admissions
                .lock()
                .map_err(|_| "extension admission registry is unavailable".to_owned())?;
            if admissions.contains_key(&identity) {
                return Err("duplicate extension request identity".into());
            }
            if admissions.iter().any(|(pending, canceled)| {
                pending.client_id == identity.client_id && !canceled.load(Ordering::Acquire)
            }) {
                return Err("client already has extension work pending".into());
            }
            admissions.insert(identity.clone(), canceled.clone());
        }
        if let Err(error) = worker.requests.try_send(ExtensionWorkerRequest::Action {
            service_generation,
            identity: identity.clone(),
            action_id,
            canceled,
            result_permit,
        }) {
            remove_admission(&self.admissions, &identity);
            return Err(format!("extension service queue unavailable: {error}"));
        }
        work_slot.disarm();
        Ok(())
    }

    pub fn cancel(&self, identity: &ExtensionRequestIdentity) -> bool {
        let canceled = self
            .admissions
            .lock()
            .ok()
            .and_then(|admissions| admissions.get(identity).cloned());
        let Some(canceled) = canceled else {
            return false;
        };
        canceled.store(true, Ordering::Release);
        if let Ok(mut tombstones) = self.cancellation_tombstones.lock() {
            if tombstones.len() >= MAX_CANCELLATION_TOMBSTONES {
                tombstones.pop_front();
            }
            tombstones.push_back(identity.clone());
        }
        true
    }

    pub fn matches_generation(&self, service_id: &str, generation: u64) -> bool {
        self.workers.get(service_id).is_some_and(|worker| {
            worker.generation.load(std::sync::atomic::Ordering::Acquire) == generation
        })
    }

    #[cfg(test)]
    pub(crate) fn generation_for_test(&self, service_id: &str) -> Option<u64> {
        self.workers
            .get(service_id)
            .map(|worker| worker.generation.load(std::sync::atomic::Ordering::Acquire))
    }

    pub fn has_grant(
        &self,
        service_id: &str,
        generation: u64,
        capability: crate::api::schema::ExtensionServiceCapability,
    ) -> bool {
        self.workers.get(service_id).is_some_and(|worker| {
            worker.generation.load(std::sync::atomic::Ordering::Acquire) == generation
                && worker.granted_capabilities.contains(&capability)
        })
    }
}

impl Drop for ExtensionServiceManager {
    fn drop(&mut self) {
        if let Ok(admissions) = self.admissions.lock() {
            for canceled in admissions.values() {
                canceled.store(true, Ordering::Release);
            }
        }
        let mut joins = Vec::new();
        let mut provider_slots = Vec::new();
        for worker in self.workers.values_mut() {
            joins.extend(worker.join.take());
            provider_slots.extend(worker._provider_slot.take());
        }
        // Dropping every sender disconnects the bounded queues. Workers observe cancellation,
        // discard queued work, terminate their supervised process, and exit off the App actor.
        self.workers.clear();
        if !joins.is_empty() || !provider_slots.is_empty() {
            let payload = Arc::new(Mutex::new(Some((
                joins,
                provider_slots,
                self.event_tx.clone(),
            ))));
            let thread_payload = payload.clone();
            let spawn = std::thread::Builder::new()
                .name("vqro-pane-extension-reaper".into())
                .spawn(move || {
                    let Some((joins, provider_slots, event_tx)) = thread_payload
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .take()
                    else {
                        return;
                    };
                    for join in joins {
                        let _ = join.join();
                    }
                    drop(provider_slots);
                    let _ = event_tx
                        .blocking_send(crate::events::AppEvent::ExtensionCatalogCapacityAvailable);
                });
            if spawn.is_err() {
                // Fail closed rather than release capacity for a detached live provider.
                std::mem::forget(payload);
            }
        }
    }
}

pub(crate) fn declared_actions_allowed(
    actions: &[crate::view_document::ViewAction],
    grants: &[crate::api::schema::ExtensionServiceCapability],
) -> bool {
    actions.iter().all(|action| match action.id.as_str() {
        "terminal.interrupt" => {
            action.destructive
                && grants
                    .contains(&crate::api::schema::ExtensionServiceCapability::TerminalInterrupt)
        }
        _ => false,
    })
}

fn supports_current_platform(platforms: Option<&[crate::api::schema::PluginPlatform]>) -> bool {
    let Some(platforms) = platforms else {
        return true;
    };
    #[cfg(target_os = "linux")]
    let current = crate::api::schema::PluginPlatform::Linux;
    #[cfg(target_os = "macos")]
    let current = crate::api::schema::PluginPlatform::Macos;
    #[cfg(windows)]
    let current = crate::api::schema::PluginPlatform::Windows;
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    return false;
    platforms.contains(&current)
}

impl ExtensionServiceSupervisor {
    pub fn new(spec: ExtensionServiceSpec) -> Result<Self, ExtensionServiceError> {
        validate_service_id(&spec.id)?;
        if spec.command.is_empty() {
            return Err(ExtensionServiceError::Configuration(
                "service command must not be empty".into(),
            ));
        }
        Ok(Self {
            spec,
            runtime: None,
            status: ExtensionServiceStatus::Stopped,
            failures: 0,
        })
    }

    #[cfg(test)]
    pub fn start(&mut self) -> Result<(), ExtensionServiceError> {
        self.start_cancellable(&AtomicBool::new(false))
    }

    fn start_cancellable(&mut self, canceled: &AtomicBool) -> Result<(), ExtensionServiceError> {
        self.stop();
        self.status = ExtensionServiceStatus::Starting;
        match ExtensionServiceRuntime::spawn(&self.spec, canceled) {
            Ok(runtime) => {
                self.runtime = Some(runtime);
                self.failures = 0;
                self.status = ExtensionServiceStatus::Running;
                Ok(())
            }
            Err(error) => {
                if error.counts_as_failure() {
                    self.record_failure(&error);
                } else {
                    self.status = ExtensionServiceStatus::Stopped;
                }
                Err(error)
            }
        }
    }

    fn ensure_running_cancellable(
        &mut self,
        now: Instant,
        canceled: &AtomicBool,
    ) -> Result<(), ExtensionServiceError> {
        if canceled.load(Ordering::Acquire) {
            return Err(ExtensionServiceError::Canceled);
        }
        match &self.status {
            ExtensionServiceStatus::Running => Ok(()),
            ExtensionServiceStatus::Backoff { retry_at, .. } if now < *retry_at => {
                Err(ExtensionServiceError::Backoff)
            }
            ExtensionServiceStatus::Failed(message) => {
                Err(ExtensionServiceError::RestartBudget(message.clone()))
            }
            ExtensionServiceStatus::Stopped
            | ExtensionServiceStatus::Starting
            | ExtensionServiceStatus::Backoff { .. } => self.start_cancellable(canceled),
        }
    }

    #[cfg(test)]
    pub fn render_view(
        &mut self,
        context: serde_json::Value,
    ) -> Result<ExtensionView, ExtensionServiceError> {
        self.render_view_cancellable(context, &AtomicBool::new(false))
    }

    fn render_view_cancellable(
        &mut self,
        context: serde_json::Value,
        canceled: &AtomicBool,
    ) -> Result<ExtensionView, ExtensionServiceError> {
        self.ensure_running_cancellable(Instant::now(), canceled)?;
        let result = self
            .runtime
            .as_mut()
            .ok_or_else(|| ExtensionServiceError::Protocol("service is not running".into()))?
            .request_cancellable("view.render", context, canceled)
            .and_then(|value| {
                let view: ExtensionView = serde_json::from_value(value).map_err(|error| {
                    ExtensionServiceError::Protocol(format!("invalid view contribution: {error}"))
                })?;
                view.document.validate().map_err(|error| {
                    ExtensionServiceError::Protocol(format!("invalid view document: {error}"))
                })?;
                Ok(view)
            });
        match result {
            Ok(view) => Ok(view),
            Err(error) => {
                if error.requires_restart() {
                    self.runtime = None;
                    if error.counts_as_failure() {
                        self.record_failure(&error);
                    }
                }
                Err(error)
            }
        }
    }

    fn render_host_document_cancellable(
        &mut self,
        snapshot: &crate::extension_contract::HostTerminalGroupsV1,
        canceled: &AtomicBool,
    ) -> Result<crate::extension_contract::HostDocumentV1, ExtensionServiceError> {
        self.ensure_running_cancellable(Instant::now(), canceled)?;
        let params = serde_json::to_value(snapshot).map_err(|error| {
            ExtensionServiceError::Protocol(format!("cannot encode host snapshot: {error}"))
        })?;
        let result =
            self.runtime
                .as_mut()
                .ok_or_else(|| ExtensionServiceError::Protocol("service is not running".into()))?
                .request_cancellable("host.document.render", params, canceled)
                .and_then(|value| {
                    let document = serde_json::from_value::<
                        crate::extension_contract::HostDocumentV1,
                    >(value)
                    .map_err(|error| {
                        ExtensionServiceError::Protocol(format!("invalid host document: {error}"))
                    })?;
                    document.validate().map_err(|error| {
                        ExtensionServiceError::Protocol(format!("invalid host document: {error}"))
                    })?;
                    Ok(document)
                });
        match result {
            Ok(document) => Ok(document),
            Err(error) => {
                if error.requires_restart() {
                    self.runtime = None;
                    if error.counts_as_failure() {
                        self.record_failure(&error);
                    }
                }
                Err(error)
            }
        }
    }

    #[cfg(test)]
    pub fn invoke_host_document_action(
        &mut self,
        action: &crate::extension_contract::HostDocumentExtensionActionV1,
    ) -> Result<(), ExtensionServiceError> {
        self.invoke_host_document_action_cancellable(action, &AtomicBool::new(false))
    }

    fn invoke_host_document_action_cancellable(
        &mut self,
        action: &crate::extension_contract::HostDocumentExtensionActionV1,
        canceled: &AtomicBool,
    ) -> Result<(), ExtensionServiceError> {
        self.ensure_running_cancellable(Instant::now(), canceled)?;
        let params = serde_json::to_value(action).map_err(|error| {
            ExtensionServiceError::Protocol(format!("cannot encode host document action: {error}"))
        })?;
        let runtime = self
            .runtime
            .as_mut()
            .ok_or_else(|| ExtensionServiceError::Protocol("service is not running".into()))?;
        if !runtime.methods.contains("host.document.action") {
            return Err(ExtensionServiceError::Protocol(
                "service did not advertise host.document.action".into(),
            ));
        }
        let result = runtime
            .request_cancellable("host.document.action", params, canceled)
            .map(|_| ());
        if let Err(error) = &result {
            if error.requires_restart() {
                self.runtime = None;
                if error.counts_as_failure() {
                    self.record_failure(error);
                }
            }
        }
        result
    }

    fn invoke_action_cancellable(
        &mut self,
        action_id: &str,
        target_pane_id: &str,
        canceled: &AtomicBool,
    ) -> Result<ExtensionHostEffect, ExtensionServiceError> {
        self.ensure_running_cancellable(Instant::now(), canceled)?;
        let result = self
            .runtime
            .as_mut()
            .ok_or_else(|| ExtensionServiceError::Protocol("service is not running".into()))?
            .request_cancellable(
                "view.action",
                serde_json::json!({
                    "action_id": action_id,
                    "target": { "type": "pane", "pane_id": target_pane_id },
                }),
                canceled,
            )
            .and_then(|value| {
                serde_json::from_value::<ExtensionActionResponse>(value).map_err(|error| {
                    ExtensionServiceError::Protocol(format!("invalid action response: {error}"))
                })
            });
        match result {
            Ok(response) => Ok(response.effect),
            Err(error) => {
                if error.requires_restart() {
                    self.runtime = None;
                    if error.counts_as_failure() {
                        self.record_failure(&error);
                    }
                }
                Err(error)
            }
        }
    }

    pub fn stop(&mut self) {
        self.runtime = None;
        self.status = ExtensionServiceStatus::Stopped;
    }

    fn record_failure(&mut self, error: &ExtensionServiceError) {
        self.failures = self.failures.saturating_add(1);
        self.runtime = None;
        if self.failures > MAX_RESTARTS {
            self.status = ExtensionServiceStatus::Failed(error.to_string());
            return;
        }
        let delay = Duration::from_millis(250).saturating_mul(1 << (self.failures - 1));
        self.status = ExtensionServiceStatus::Backoff {
            failures: self.failures,
            retry_at: Instant::now() + delay,
        };
    }
}

#[derive(Debug)]
struct ExtensionServiceRuntime {
    child: Child,
    containment: Option<crate::platform::ExtensionServiceContainment>,
    writes: mpsc::SyncSender<Vec<u8>>,
    frames: mpsc::Receiver<Result<Vec<u8>, ExtensionServiceError>>,
    methods: std::collections::HashSet<String>,
    next_request_id: u64,
}

impl ExtensionServiceRuntime {
    fn spawn(
        spec: &ExtensionServiceSpec,
        canceled: &AtomicBool,
    ) -> Result<Self, ExtensionServiceError> {
        let program = spec.command.first().ok_or_else(|| {
            ExtensionServiceError::Configuration("service command must not be empty".into())
        })?;
        let args = spec.command.iter().skip(1).cloned().collect::<Vec<_>>();
        let mut command = crate::plugin_command::command_for_argv_in_dir(program, &args, &spec.cwd);
        command
            .env("VQRO_EXTENSION_SERVICE_ID", &spec.id)
            .env(
                "VQRO_EXTENSION_SERVICE_PROTOCOL",
                EXTENSION_SERVICE_PROTOCOL.to_string(),
            )
            .env(crate::product::ENV_MARKER, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for key in [
            "HERDR_ENV",
            "HERDR_SOCKET_PATH",
            "HERDR_CLIENT_SOCKET_PATH",
            "HERDR_SESSION",
            "HERDR_WORKSPACE_ID",
            "HERDR_TAB_ID",
            "HERDR_PANE_ID",
            "HERDR_BIN_PATH",
            "HERDR_STARTUP_CWD",
            "VQRO_SOCKET_PATH",
            "VQRO_CLIENT_SOCKET_PATH",
            "VQRO_SESSION",
            "VQRO_WORKSPACE_ID",
            "VQRO_TAB_ID",
            "VQRO_PANE_ID",
            "VQRO_BIN_PATH",
            "VQRO_STARTUP_CWD",
        ] {
            command.env_remove(key);
        }
        crate::platform::configure_extension_service_command(&mut command);
        let mut child = command.spawn().map_err(ExtensionServiceError::Io)?;
        let containment = match crate::platform::ExtensionServiceContainment::new(&child) {
            Ok(containment) => containment,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ExtensionServiceError::Io(error));
            }
        };
        let stdin = child.stdin.take().ok_or_else(|| {
            ExtensionServiceError::Protocol("service stdin was not captured".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ExtensionServiceError::Protocol("service stdout was not captured".into())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            ExtensionServiceError::Protocol("service stderr was not captured".into())
        })?;
        spawn_bounded_stderr_reader(spec.id.clone(), stderr);
        // Welcome and the first request may be queued before the writer thread is scheduled.
        // Requests are otherwise strictly sequential, so two slots are sufficient and bounded.
        let (write_tx, write_rx) = mpsc::sync_channel::<Vec<u8>>(2);
        std::thread::spawn(move || {
            let mut stdin = stdin;
            while let Ok(bytes) = write_rx.recv() {
                if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
                    break;
                }
            }
        });
        let (frame_tx, frames) = mpsc::sync_channel(8);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let frame = read_bounded_frame(&mut reader);
                let eof = matches!(&frame, Ok(bytes) if bytes.is_empty());
                let framing_failure = frame.is_err();
                if frame_tx.send(frame).is_err() || eof || framing_failure {
                    break;
                }
            }
        });
        let mut runtime = Self {
            child,
            containment: Some(containment),
            writes: write_tx,
            frames,
            methods: std::collections::HashSet::new(),
            next_request_id: 1,
        };
        runtime.handshake(&spec.id, &spec.granted_capabilities, canceled)?;
        Ok(runtime)
    }

    fn handshake(
        &mut self,
        expected_id: &str,
        granted_capabilities: &[crate::api::schema::ExtensionServiceCapability],
        canceled: &AtomicBool,
    ) -> Result<(), ExtensionServiceError> {
        let frame = self.recv_frame_cancellable(HANDSHAKE_TIMEOUT, canceled)?;
        let hello: ServiceHello = serde_json::from_slice(&frame).map_err(|error| {
            ExtensionServiceError::Protocol(format!("invalid service hello: {error}"))
        })?;
        if hello.kind != "hello"
            || hello.protocol != EXTENSION_SERVICE_PROTOCOL
            || hello.service_id != expected_id
        {
            return Err(ExtensionServiceError::Protocol(
                "service hello identity or protocol mismatch".into(),
            ));
        }
        let requires_view = granted_capabilities
            .contains(&crate::api::schema::ExtensionServiceCapability::ViewContribution);
        let requires_document = granted_capabilities
            .contains(&crate::api::schema::ExtensionServiceCapability::HostDocument);
        let requires_action = requires_view
            && granted_capabilities
                .contains(&crate::api::schema::ExtensionServiceCapability::TerminalInterrupt);
        if (requires_view && !hello.methods.iter().any(|method| method == "view.render"))
            || (requires_document
                && !hello
                    .methods
                    .iter()
                    .any(|method| method == "host.document.render"))
            || (requires_action && !hello.methods.iter().any(|method| method == "view.action"))
        {
            return Err(ExtensionServiceError::Protocol(
                "service does not support the granted methods".into(),
            ));
        }
        if hello
            .requested_capabilities
            .iter()
            .any(|requested| !granted_capabilities.contains(requested))
            || granted_capabilities
                .iter()
                .any(|granted| !hello.requested_capabilities.contains(granted))
        {
            return Err(ExtensionServiceError::Protocol(
                "service capability request does not match host grants".into(),
            ));
        }
        self.methods = hello.methods.iter().cloned().collect();
        let welcome = ServiceWelcome {
            kind: "welcome",
            protocol: EXTENSION_SERVICE_PROTOCOL,
            granted_capabilities,
            methods: &[
                "view.render",
                "view.action",
                "host.document.render",
                "host.document.action",
            ],
            trust: "trusted_same_user_not_sandboxed",
        };
        let mut bytes = serde_json::to_vec(&welcome).map_err(|error| {
            ExtensionServiceError::Protocol(format!("cannot encode service welcome: {error}"))
        })?;
        bytes.push(b'\n');
        self.writes.try_send(bytes).map_err(|error| {
            ExtensionServiceError::Protocol(format!("service input queue unavailable: {error}"))
        })
    }

    fn request_cancellable(
        &mut self,
        method: &str,
        params: serde_json::Value,
        canceled: &AtomicBool,
    ) -> Result<serde_json::Value, ExtensionServiceError> {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);
        let request = ServiceRequest {
            kind: "request",
            id: request_id,
            method,
            params,
        };
        let mut bytes = serde_json::to_vec(&request).map_err(|error| {
            ExtensionServiceError::Protocol(format!("cannot encode service request: {error}"))
        })?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(ExtensionServiceError::Protocol(
                "service request exceeds frame limit".into(),
            ));
        }
        bytes.push(b'\n');
        if canceled.load(Ordering::Acquire) {
            return Err(ExtensionServiceError::Canceled);
        }
        self.writes.try_send(bytes).map_err(|error| {
            ExtensionServiceError::Protocol(format!("service input queue unavailable: {error}"))
        })?;
        let frame = self.recv_frame_cancellable(REQUEST_TIMEOUT, canceled)?;
        let response: ServiceResponse = serde_json::from_slice(&frame).map_err(|error| {
            ExtensionServiceError::Protocol(format!("invalid service response: {error}"))
        })?;
        if response.kind != "response" || response.id != request_id {
            return Err(ExtensionServiceError::Protocol(
                "service response request id mismatch".into(),
            ));
        }
        if let Some(error) = response.error {
            return Err(ExtensionServiceError::Remote(error));
        }
        response.result.ok_or_else(|| {
            ExtensionServiceError::Protocol("service response omitted result".into())
        })
    }

    fn recv_frame_cancellable(
        &mut self,
        timeout: Duration,
        canceled: &AtomicBool,
    ) -> Result<Vec<u8>, ExtensionServiceError> {
        let deadline = Instant::now() + timeout;
        loop {
            if canceled.load(Ordering::Acquire) {
                return Err(ExtensionServiceError::Canceled);
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(ExtensionServiceError::Timeout);
            };
            match self
                .frames
                .recv_timeout(remaining.min(Duration::from_millis(25)))
            {
                Ok(Ok(frame)) if frame.is_empty() => return Err(ExtensionServiceError::Exited),
                Ok(frame) => return frame,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(ExtensionServiceError::Exited);
                }
            }
        }
    }
}

impl Drop for ExtensionServiceRuntime {
    fn drop(&mut self) {
        // Close kill-on-close containment before waiting so descendants cannot
        // retain inherited stdio and keep teardown blocked.
        self.containment.take();
        crate::platform::terminate_extension_service_process_group(&self.child);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Serialize)]
struct ServiceRequest<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    id: u64,
    method: &'a str,
    params: serde_json::Value,
}

#[derive(Deserialize)]
struct ServiceHello {
    #[serde(rename = "type")]
    kind: String,
    protocol: u16,
    service_id: String,
    #[serde(default)]
    requested_capabilities: Vec<crate::api::schema::ExtensionServiceCapability>,
    #[serde(default)]
    methods: Vec<String>,
}

#[derive(Serialize)]
struct ServiceWelcome<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    protocol: u16,
    granted_capabilities: &'a [crate::api::schema::ExtensionServiceCapability],
    methods: &'a [&'a str],
    trust: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtensionView {
    pub document: crate::view_document::ViewDocument,
    #[serde(default)]
    pub actions: Vec<crate::view_document::ViewAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtensionHostEffect {
    TerminalInterrupt,
}

#[derive(Deserialize)]
struct ExtensionActionResponse {
    effect: ExtensionHostEffect,
}

#[derive(Deserialize)]
struct ServiceResponse {
    #[serde(rename = "type")]
    kind: String,
    id: u64,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug)]
pub enum ExtensionServiceError {
    Configuration(String),
    Io(std::io::Error),
    Protocol(String),
    Remote(String),
    Timeout,
    Canceled,
    Exited,
    Backoff,
    RestartBudget(String),
}

impl ExtensionServiceError {
    fn requires_restart(&self) -> bool {
        matches!(
            self,
            Self::Io(_) | Self::Protocol(_) | Self::Timeout | Self::Canceled | Self::Exited
        )
    }

    fn counts_as_failure(&self) -> bool {
        !matches!(self, Self::Canceled)
    }
}

impl std::fmt::Display for ExtensionServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration(message) | Self::Protocol(message) | Self::Remote(message) => {
                formatter.write_str(message)
            }
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Timeout => formatter.write_str("extension service timed out"),
            Self::Canceled => formatter.write_str("extension service request was canceled"),
            Self::Exited => formatter.write_str("extension service exited"),
            Self::Backoff => formatter.write_str("extension service is restarting"),
            Self::RestartBudget(message) => {
                write!(
                    formatter,
                    "extension service restart budget exhausted: {message}"
                )
            }
        }
    }
}

impl std::error::Error for ExtensionServiceError {}

fn spawn_bounded_stderr_reader(service_id: String, stderr: std::process::ChildStderr) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut window_started = Instant::now();
        let mut emitted = 0usize;
        loop {
            let mut line = Vec::with_capacity(MAX_STDERR_LINE_BYTES.min(256));
            let mut truncated = false;
            let mut eof = false;
            loop {
                let available = match reader.fill_buf() {
                    Ok(available) => available,
                    Err(error) => {
                        tracing::warn!(%service_id, %error, "extension service stderr read failed");
                        return;
                    }
                };
                if available.is_empty() {
                    eof = true;
                    break;
                }
                let newline = available.iter().position(|byte| *byte == b'\n');
                let consumed = newline.map_or(available.len(), |index| index + 1);
                let content = newline.map_or(available, |index| &available[..index]);
                let remaining = MAX_STDERR_LINE_BYTES.saturating_sub(line.len());
                line.extend_from_slice(&content[..content.len().min(remaining)]);
                truncated |= content.len() > remaining;
                reader.consume(consumed);
                if newline.is_some() {
                    break;
                }
            }
            if line.is_empty() && eof {
                return;
            }
            if window_started.elapsed() >= Duration::from_secs(60) {
                window_started = Instant::now();
                emitted = 0;
            }
            if emitted < MAX_STDERR_LINES_PER_MINUTE {
                let message = String::from_utf8_lossy(&line);
                tracing::warn!(%service_id, truncated, message = %message, "extension service stderr");
                emitted += 1;
            }
            if eof {
                return;
            }
        }
    });
}

fn read_bounded_frame(reader: &mut impl BufRead) -> Result<Vec<u8>, ExtensionServiceError> {
    let mut frame = Vec::new();
    let mut limited = std::io::Read::take(&mut *reader, (MAX_FRAME_BYTES + 1) as u64);
    let read = limited
        .read_until(b'\n', &mut frame)
        .map_err(ExtensionServiceError::Io)?;
    if read == 0 {
        return Ok(Vec::new());
    }
    if frame.len() > MAX_FRAME_BYTES || !frame.ends_with(b"\n") {
        return Err(ExtensionServiceError::Protocol(
            "extension service frame exceeds limit".into(),
        ));
    }
    frame.pop();
    if frame.ends_with(b"\r") {
        frame.pop();
    }
    Ok(frame)
}

fn validate_service_id(id: &str) -> Result<(), ExtensionServiceError> {
    if id.is_empty()
        || id.len() > 120
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(ExtensionServiceError::Configuration(
            "invalid extension service id".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_for(
        client_id: u64,
        request_id: &str,
        target_pane_id: &str,
        contribution_revision: u64,
    ) -> ExtensionRequestIdentity {
        ExtensionRequestIdentity {
            client_id,
            boot_id: "boot".into(),
            projection_revision: 1,
            request_id: request_id.into(),
            target_pane_id: target_pane_id.into(),
            contribution_revision,
        }
    }

    #[test]
    fn rejects_invalid_specs_before_spawning() {
        let spec = ExtensionServiceSpec {
            id: "not valid".into(),
            command: vec!["unused".into()],
            cwd: PathBuf::from("."),
            priority: 0,
            granted_capabilities: Vec::new(),
        };
        assert!(ExtensionServiceSupervisor::new(spec).is_err());
    }

    #[test]
    fn manager_never_selects_an_invalid_or_unstarted_provider() {
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(1);
        let manager = manager_for_specs(
            vec![ExtensionServiceSpec {
                id: "not valid".into(),
                command: vec!["unused".into()],
                cwd: PathBuf::from("."),
                priority: 100,
                granted_capabilities: vec![
                    crate::api::schema::ExtensionServiceCapability::ViewContribution,
                ],
            }],
            event_tx,
        );
        assert!(manager.provider_identity().is_none());
        assert!(manager
            .request_view(identity_for(1, "view", "w1:p1", 1), serde_json::json!({}))
            .is_err());
    }

    #[test]
    fn terminal_interrupt_action_requires_explicit_grant_and_destructive_label() {
        let action = crate::view_document::ViewAction {
            id: "terminal.interrupt".into(),
            label: "Interrupt process".into(),
            destructive: true,
        };
        assert!(!declared_actions_allowed(
            std::slice::from_ref(&action),
            &[]
        ));
        assert!(declared_actions_allowed(
            &[action],
            &[crate::api::schema::ExtensionServiceCapability::TerminalInterrupt]
        ));
        let unknown = crate::view_document::ViewAction {
            id: "pane.send_keys".into(),
            label: "Send".into(),
            destructive: true,
        };
        assert!(!declared_actions_allowed(
            &[unknown],
            &[crate::api::schema::ExtensionServiceCapability::TerminalInterrupt]
        ));
    }

    #[test]
    fn bounded_reader_rejects_oversized_or_unterminated_frames() {
        let mut oversized = std::io::Cursor::new(vec![b'x'; MAX_FRAME_BYTES + 2]);
        assert!(matches!(
            read_bounded_frame(&mut oversized),
            Err(ExtensionServiceError::Protocol(_))
        ));
        let mut normal = std::io::Cursor::new(b"{\"type\":\"hello\"}\n".to_vec());
        assert_eq!(
            read_bounded_frame(&mut normal).unwrap(),
            b"{\"type\":\"hello\"}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn manager_dispatches_off_thread_and_fences_service_generations() {
        let script = r#"
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"test.async","requested_capabilities":["view_contribution"],"methods":["view.render"]}'
IFS= read -r welcome
for index in 1 2; do
  IFS= read -r request
  sleep 0.05
  id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  printf '{"type":"response","id":%s,"result":{"document":{"title":"Async","sections":[]},"actions":[]}}\n' "$id"
done
"#;
        let spec = || ExtensionServiceSpec {
            id: "test.async".into(),
            command: vec!["sh".into(), "-c".into(), script.into()],
            cwd: PathBuf::from("."),
            priority: 10,
            granted_capabilities: vec![
                crate::api::schema::ExtensionServiceCapability::ViewContribution,
            ],
        };
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
        let manager = manager_for_specs(vec![spec()], event_tx);
        let identity = ExtensionRequestIdentity {
            client_id: 9,
            boot_id: "boot".into(),
            projection_revision: 3,
            request_id: "request-1".into(),
            target_pane_id: "w1:p2".into(),
            contribution_revision: 4,
        };
        let started = Instant::now();
        let (_, generation) = manager
            .request_view(identity, serde_json::json!({}))
            .unwrap();
        assert!(started.elapsed() < Duration::from_millis(50));
        let second_identity = ExtensionRequestIdentity {
            client_id: 10,
            boot_id: "boot".into(),
            projection_revision: 8,
            request_id: "request-2".into(),
            target_pane_id: "w2:p4".into(),
            contribution_revision: 2,
        };
        let (_, second_generation) = manager
            .request_view(second_identity, serde_json::json!({}))
            .unwrap();
        assert_eq!(second_generation, generation);
        let event = event_rx.blocking_recv().expect("async result");
        let crate::events::AppEvent::ExtensionServiceFinished(result) = event else {
            panic!("extension result");
        };
        assert_eq!(result.service_generation, generation);
        assert_eq!(result.identity.request_id, "request-1");
        assert!(matches!(
            result.kind,
            ExtensionAsyncResultKind::Render(Ok(_))
        ));
        let event = event_rx.blocking_recv().expect("second async result");
        let crate::events::AppEvent::ExtensionServiceFinished(result) = event else {
            panic!("extension result");
        };
        assert_eq!(result.service_generation, generation);
        assert_eq!(result.identity.client_id, 10);
        assert_eq!(result.identity.target_pane_id, "w2:p4");
        assert!(matches!(
            result.kind,
            ExtensionAsyncResultKind::Render(Ok(_))
        ));

        let (replacement_tx, _replacement_rx) = tokio::sync::mpsc::channel(4);
        let replacement = manager_for_specs(vec![spec()], replacement_tx);
        assert!(!replacement.matches_generation("test.async", generation));
    }

    #[cfg(unix)]
    #[test]
    fn request_admission_reserves_result_capacity_before_work() {
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(1);
        let _occupied = event_tx.clone().try_reserve_owned().unwrap();
        let manager = manager_for_test_spec(
            ExtensionServiceSpec {
                id: "test.capacity".into(),
                command: vec!["sh".into(), "-c".into(), "exit 0".into()],
                cwd: PathBuf::from("."),
                priority: 1,
                granted_capabilities: vec![
                    crate::api::schema::ExtensionServiceCapability::ViewContribution,
                ],
            },
            event_tx,
        );
        let error = manager
            .request_view(identity_for(1, "view", "w1:p1", 1), serde_json::json!({}))
            .unwrap_err();
        assert!(error.contains("result queue unavailable"));
    }

    #[test]
    fn cancel_and_replace_refresh_is_admitted_for_queued_and_inflight_work() {
        let script = r#"
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"test.cancel","requested_capabilities":["view_contribution"],"methods":["view.render"]}'
IFS= read -r welcome
while IFS= read -r request; do sleep 5; done
"#;
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(16);
        let manager = manager_for_test_spec(
            ExtensionServiceSpec {
                id: "test.cancel".into(),
                command: vec!["sh".into(), "-c".into(), script.into()],
                cwd: PathBuf::from("."),
                priority: 1,
                granted_capabilities: vec![
                    crate::api::schema::ExtensionServiceCapability::ViewContribution,
                ],
            },
            event_tx,
        );
        let inflight = identity_for(1, "inflight", "w1:p1", 1);
        let queued = identity_for(2, "queued", "w1:p2", 1);
        let fair = identity_for(3, "fair", "w1:p3", 1);
        manager
            .request_view(inflight.clone(), serde_json::json!({}))
            .unwrap();
        manager
            .request_view(queued.clone(), serde_json::json!({}))
            .unwrap();
        manager
            .request_view(fair.clone(), serde_json::json!({}))
            .unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert!(manager.cancel(&queued));
        assert!(manager.cancel(&inflight));
        assert!(manager
            .request_view(inflight.clone(), serde_json::json!({}))
            .unwrap_err()
            .contains("duplicate extension request identity"));
        // Cancellation permits a newer identity for the client while retaining ownership of the
        // exact old identity until its worker observes cancellation; tokens and tombstones fence
        // late publication from the old queued/in-flight jobs.
        manager
            .request_view(
                identity_for(1, "inflight-replacement", "w1:p1", 2),
                serde_json::json!({}),
            )
            .expect("in-flight replacement admitted immediately");
        manager
            .request_view(
                identity_for(2, "queued-replacement", "w1:p2", 2),
                serde_json::json!({}),
            )
            .expect("queued replacement admitted immediately");

        let started = Instant::now();
        let event = loop {
            if let Ok(event) = event_rx.try_recv() {
                break event;
            }
            assert!(started.elapsed() < Duration::from_millis(600));
            std::thread::sleep(Duration::from_millis(5));
        };
        let crate::events::AppEvent::ExtensionServiceFinished(result) = event else {
            panic!("expected extension result");
        };
        assert_eq!(result.identity, fair);
        assert!(matches!(
            result.kind,
            ExtensionAsyncResultKind::Render(Err(_))
        ));
        while let Ok(event) = event_rx.try_recv() {
            let crate::events::AppEvent::ExtensionServiceFinished(result) = event else {
                continue;
            };
            assert_ne!(result.identity, inflight, "in-flight cancel published");
            assert_ne!(result.identity, queued, "queued cancel published");
        }
    }

    #[test]
    fn retired_runtime_immediately_rejects_old_lease_before_next_view() {
        let script = r#"
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"test.restart","requested_capabilities":["view_contribution"],"methods":["view.render"]}'
IFS= read -r welcome
IFS= read -r request
id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"type":"response","id":%s,"result":{"document":{"title":"bad\\u001b","sections":[]},"actions":[]}}\n' "$id"
"#;
        let spec = ExtensionServiceSpec {
            id: "test.restart".into(),
            command: vec!["sh".into(), "-c".into(), script.into()],
            cwd: PathBuf::from("."),
            priority: 0,
            granted_capabilities: vec![
                crate::api::schema::ExtensionServiceCapability::ViewContribution,
            ],
        };
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(4);
        let manager = manager_for_specs(vec![spec], event_tx);
        let identity = |request: &str| ExtensionRequestIdentity {
            client_id: 1,
            boot_id: "boot".into(),
            projection_revision: 1,
            request_id: request.into(),
            target_pane_id: "w1:p1".into(),
            contribution_revision: 1,
        };
        let (_, first_generation) = manager
            .request_view(identity("first"), serde_json::json!({}))
            .unwrap();
        let event = event_rx.blocking_recv().unwrap();
        let crate::events::AppEvent::ExtensionServiceFinished(first) = event else {
            panic!("extension result");
        };
        assert_eq!(first.service_generation, first_generation);
        manager.complete(&first.identity);
        assert!(!manager.matches_generation("test.restart", first_generation));
        assert!(manager
            .request_action(
                "test.restart",
                first_generation,
                identity("old-action"),
                "terminal.interrupt".into(),
            )
            .is_err());
        std::thread::sleep(Duration::from_millis(300));
        let (_, second_generation) = manager
            .request_view(identity("second"), serde_json::json!({}))
            .unwrap();
        assert_ne!(second_generation, first_generation);
        assert!(manager.matches_generation("test.restart", second_generation));
    }

    #[cfg(unix)]
    #[test]
    fn invalid_document_retires_service_and_enters_backoff() {
        let script = r#"
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"test.invalid","methods":["view.render"]}'
IFS= read -r welcome
IFS= read -r request
id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"type":"response","id":%s,"result":{"document":{"title":"bad\\u001b","sections":[]},"actions":[]}}\n' "$id"
"#;
        let spec = ExtensionServiceSpec {
            id: "test.invalid".into(),
            command: vec!["sh".into(), "-c".into(), script.into()],
            cwd: PathBuf::from("."),
            priority: 0,
            granted_capabilities: Vec::new(),
        };
        let mut supervisor = ExtensionServiceSupervisor::new(spec).unwrap();
        supervisor.start().unwrap();
        assert!(supervisor.render_view(serde_json::json!({})).is_err());
        assert!(supervisor.runtime.is_none());
        assert!(matches!(
            supervisor.status,
            ExtensionServiceStatus::Backoff { failures: 1, .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn noisy_stderr_is_continuously_drained_without_blocking_handshake() {
        let script = r#"
i=0
while [ "$i" -lt 2000 ]; do echo 'bounded diagnostic output' >&2; i=$((i + 1)); done
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"test.stderr","methods":["view.render"]}'
IFS= read -r welcome
"#;
        let spec = ExtensionServiceSpec {
            id: "test.stderr".into(),
            command: vec!["sh".into(), "-c".into(), script.into()],
            cwd: PathBuf::from("."),
            priority: 0,
            granted_capabilities: Vec::new(),
        };
        let mut supervisor = ExtensionServiceSupervisor::new(spec).unwrap();
        supervisor.start().unwrap();
        assert_eq!(supervisor.status, ExtensionServiceStatus::Running);
    }

    #[cfg(unix)]
    #[test]
    fn python_reference_service_completes_real_handshake_and_render_when_available() {
        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let cwd = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("extensions/terminal-inspector");
        let mut supervisor = ExtensionServiceSupervisor::new(ExtensionServiceSpec {
            id: "vqro.terminal-inspector.telemetry".into(),
            command: vec!["python3".into(), "service.py".into()],
            cwd,
            priority: 100,
            granted_capabilities: vec![
                crate::api::schema::ExtensionServiceCapability::ViewContribution,
                crate::api::schema::ExtensionServiceCapability::TerminalInterrupt,
            ],
        })
        .unwrap();
        let view = supervisor
            .render_view(serde_json::json!({
                "pane_id": "w1:p1",
                "process": { "pid": 42, "name": "python3", "memory_bytes": 4096 },
                "agent": null
            }))
            .unwrap();
        assert_eq!(view.document.title, "Terminal intelligence");
        assert_eq!(view.actions.len(), 1);
        assert_eq!(view.actions[0].id, "terminal.interrupt");
    }

    #[test]
    fn handshake_rejects_capabilities_outside_manifest_grants() {
        let script = r#"
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"test.denied","requested_capabilities":["terminal_interrupt"],"methods":["view.render","view.action"]}'
sleep 1
"#;
        let spec = ExtensionServiceSpec {
            id: "test.denied".into(),
            command: vec!["sh".into(), "-c".into(), script.into()],
            cwd: PathBuf::from("."),
            priority: 0,
            granted_capabilities: vec![
                crate::api::schema::ExtensionServiceCapability::ViewContribution,
            ],
        };
        let mut supervisor = ExtensionServiceSupervisor::new(spec).unwrap();
        assert!(matches!(
            supervisor.start(),
            Err(ExtensionServiceError::Protocol(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn remote_rejection_does_not_consume_restart_budget() {
        let script = r#"
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"test.remote","methods":["view.render"]}'
IFS= read -r welcome
while IFS= read -r request; do
  id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  printf '{"type":"response","id":%s,"error":"domain rejected"}\n' "$id"
done
"#;
        let spec = ExtensionServiceSpec {
            id: "test.remote".into(),
            command: vec!["sh".into(), "-c".into(), script.into()],
            cwd: PathBuf::from("."),
            priority: 0,
            granted_capabilities: Vec::new(),
        };
        let mut supervisor = ExtensionServiceSupervisor::new(spec).unwrap();
        supervisor.start().unwrap();
        assert!(matches!(
            supervisor.render_view(serde_json::json!({})),
            Err(ExtensionServiceError::Remote(_))
        ));
        assert_eq!(supervisor.failures, 0);
        assert_eq!(supervisor.status, ExtensionServiceStatus::Running);
        assert!(supervisor.runtime.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn timeout_retires_runtime_and_enters_backoff() {
        let script = r#"
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"test.timeout","methods":["view.render"]}'
IFS= read -r welcome
IFS= read -r request
sleep 2
"#;
        let spec = ExtensionServiceSpec {
            id: "test.timeout".into(),
            command: vec!["sh".into(), "-c".into(), script.into()],
            cwd: PathBuf::from("."),
            priority: 0,
            granted_capabilities: Vec::new(),
        };
        let mut supervisor = ExtensionServiceSupervisor::new(spec).unwrap();
        supervisor.start().unwrap();
        assert!(matches!(
            supervisor.render_view(serde_json::json!({})),
            Err(ExtensionServiceError::Timeout)
        ));
        assert!(supervisor.runtime.is_none());
        assert!(matches!(
            supervisor.status,
            ExtensionServiceStatus::Backoff { failures: 1, .. }
        ));
    }

    #[test]
    fn service_supervisor_construction_is_lazy() {
        let supervisor = ExtensionServiceSupervisor::new(ExtensionServiceSpec {
            id: "test.lazy".into(),
            command: vec!["command-that-must-not-run".into()],
            cwd: PathBuf::from("."),
            priority: 0,
            granted_capabilities: Vec::new(),
        })
        .unwrap();
        assert!(supervisor.runtime.is_none());
        assert_eq!(supervisor.status, ExtensionServiceStatus::Stopped);
    }

    #[test]
    fn host_document_request_submission_coalesces_to_latest_revision() {
        let pending = Arc::new((
            Mutex::new(HostDocumentPending::default()),
            std::sync::Condvar::new(),
        ));
        let catalog = HostDocumentServiceCatalog {
            test_render_slots: Vec::new(),
            complete_catalog: true,
            generations: std::collections::BTreeMap::from([("example.docs".into(), 7)]),
            requests: std::collections::BTreeMap::from([(
                "example.docs".into(),
                HostDocumentWorkerHandle {
                    _provider_slot: None,
                    pending: pending.clone(),
                    desired_revisions: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_actions: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_renders: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_render_slots: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    event_pending: None,
                    revoked: Arc::new(AtomicBool::new(false)),
                    join: None,
                    event_join: None,
                },
            )]),
            event_tx: None,
        };
        let snapshot = crate::extension_contract::HostTerminalGroupsV1 {
            contract: "host.terminal-groups.v1".into(),
            generation: 7,
            revision: 1,
            scope_id: "tab-1".into(),
            groups: Vec::new(),
            ungrouped: Vec::new(),
        };
        catalog.request("example.docs", snapshot.clone()).unwrap();
        let mut newer = snapshot;
        newer.revision = 2;
        catalog.request("example.docs", newer).unwrap();
        let queued_revision = pending
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .render_by_scope
            .get("tab-1")
            .map(|request| request.snapshot.revision);
        assert_eq!(queued_revision, Some(2));
    }

    #[test]
    fn superseding_or_retiring_scope_rejects_queued_provider_actions() {
        let pending = Arc::new((
            Mutex::new(HostDocumentPending::default()),
            std::sync::Condvar::new(),
        ));
        let catalog = HostDocumentServiceCatalog {
            test_render_slots: Vec::new(),
            complete_catalog: true,
            generations: std::collections::BTreeMap::from([("example.docs".into(), 7)]),
            requests: std::collections::BTreeMap::from([(
                "example.docs".into(),
                HostDocumentWorkerHandle {
                    _provider_slot: None,
                    pending,
                    desired_revisions: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_actions: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_renders: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_render_slots: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    event_pending: None,
                    revoked: Arc::new(AtomicBool::new(false)),
                    join: None,
                    event_join: None,
                },
            )]),
            event_tx: None,
        };
        let snapshot = crate::extension_contract::HostTerminalGroupsV1 {
            contract: "host.terminal-groups.v1".into(),
            generation: 7,
            revision: 1,
            scope_id: "tab-1".into(),
            groups: Vec::new(),
            ungrouped: Vec::new(),
        };
        catalog.set_desired_revision_for_test("example.docs", "tab-1".into(), 7, 1);
        let action = crate::extension_contract::HostDocumentExtensionActionV1 {
            contract: crate::extension_contract::HOST_DOCUMENT_CONTRACT_V1.into(),
            namespace: "example.docs".into(),
            generation: 7,
            revision: 1,
            scope_id: "tab-1".into(),
            action_id: "action".into(),
            name: "action".into(),
            subject_id: None,
        };
        let superseded = catalog
            .request_action(snapshot.clone(), action.clone())
            .unwrap();
        let active_canceled = Arc::new(AtomicBool::new(false));
        catalog.requests["example.docs"]
            .active_actions
            .lock()
            .unwrap()
            .insert("tab-1".into(), active_canceled.clone());
        let mut newer = snapshot.clone();
        newer.revision = 2;
        catalog.request("example.docs", newer).unwrap();
        assert!(active_canceled.load(Ordering::Acquire));
        assert!(superseded
            .recv()
            .unwrap()
            .unwrap_err()
            .contains("superseded"));

        catalog.set_desired_revision_for_test("example.docs", "tab-1".into(), 7, 1);
        let retired = catalog.request_action(snapshot, action).unwrap();
        let render_canceled = Arc::new(AtomicBool::new(false));
        catalog.requests["example.docs"]
            .active_renders
            .lock()
            .unwrap()
            .insert("tab-1".into(), render_canceled.clone());
        catalog.retain_scopes(&std::collections::HashSet::new());
        assert!(retired.recv().unwrap().unwrap_err().contains("retired"));
        assert!(render_canceled.load(Ordering::Acquire));
        let pending = catalog.requests["example.docs"].pending.0.lock().unwrap();
        assert!(pending.render_by_scope.is_empty());
        assert!(pending.render_order.is_empty());
    }

    #[test]
    fn catalog_drop_revokes_without_blocking_on_worker_reap() {
        let pending = Arc::new((
            Mutex::new(HostDocumentPending::default()),
            std::sync::Condvar::new(),
        ));
        let revoked = Arc::new(AtomicBool::new(false));
        let observed = Arc::new(AtomicBool::new(false));
        let worker_revoked = revoked.clone();
        let worker_observed = observed.clone();
        let join = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            if !worker_revoked.load(Ordering::Acquire) {
                worker_observed.store(true, Ordering::Release);
            }
        });
        let catalog = HostDocumentServiceCatalog {
            test_render_slots: Vec::new(),
            complete_catalog: true,
            generations: std::collections::BTreeMap::from([("example.docs".into(), 1)]),
            requests: std::collections::BTreeMap::from([(
                "example.docs".into(),
                HostDocumentWorkerHandle {
                    _provider_slot: None,
                    pending,
                    desired_revisions: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_actions: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_renders: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_render_slots: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    event_pending: None,
                    revoked,
                    join: Some(join),
                    event_join: None,
                },
            )]),
            event_tx: None,
        };
        let started = Instant::now();
        drop(catalog);
        assert!(started.elapsed() < Duration::from_millis(100));
        std::thread::sleep(Duration::from_millis(225));
        assert!(!observed.load(Ordering::Acquire));
    }

    #[test]
    fn catalog_revocation_completes_an_action_already_popped_by_worker() {
        let pending = Arc::new((
            Mutex::new(HostDocumentPending::default()),
            std::sync::Condvar::new(),
        ));
        let revoked = Arc::new(AtomicBool::new(false));
        let worker_pending = pending.clone();
        let worker_revoked = revoked.clone();
        let (response_tx, response_rx) = mpsc::sync_channel(1);
        pending
            .0
            .lock()
            .unwrap()
            .push(HostDocumentRequest {
                namespace: "example.docs".into(),
                render_slot: None,
                snapshot: crate::extension_contract::HostTerminalGroupsV1 {
                    contract: "host.terminal-groups.v1".into(),
                    generation: 1,
                    revision: 1,
                    scope_id: "tab-1".into(),
                    groups: Vec::new(),
                    ungrouped: Vec::new(),
                },
                action: Some(crate::extension_contract::HostDocumentExtensionActionV1 {
                    contract: crate::extension_contract::HOST_DOCUMENT_CONTRACT_V1.into(),
                    namespace: "example.docs".into(),
                    generation: 1,
                    revision: 1,
                    scope_id: "tab-1".into(),
                    action_id: "action".into(),
                    name: "action".into(),
                    subject_id: None,
                }),
                action_response: Some(response_tx),
                action_completion: None,
                action_deadline: None,
            })
            .unwrap();
        let join = std::thread::spawn(move || {
            let request = worker_pending.0.lock().unwrap().pop().unwrap();
            while !worker_revoked.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            complete_host_document_action(
                Err("document provider was revoked".into()),
                request.action_response,
                request.action_completion,
            );
        });
        let catalog = HostDocumentServiceCatalog {
            test_render_slots: Vec::new(),
            complete_catalog: true,
            generations: std::collections::BTreeMap::from([("example.docs".into(), 1)]),
            requests: std::collections::BTreeMap::from([(
                "example.docs".into(),
                HostDocumentWorkerHandle {
                    _provider_slot: None,
                    pending,
                    desired_revisions: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_actions: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_renders: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_render_slots: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    event_pending: None,
                    revoked,
                    join: Some(join),
                    event_join: None,
                },
            )]),
            event_tx: None,
        };
        std::thread::sleep(Duration::from_millis(10));
        drop(catalog);
        assert!(response_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err()
            .contains("revoked"));
    }

    #[test]
    fn saturated_document_event_queue_retains_latest_per_scope_for_retry() {
        fn event(log_id: &str) -> crate::events::AppEvent {
            crate::events::AppEvent::PluginCommandFinished {
                log_id: log_id.into(),
                finished_unix_ms: 0,
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
                error: None,
            }
        }
        let mut pending = HostDocumentEventPending::default();
        pending.push("tab-1".into(), event("old"));
        pending.push("tab-2".into(), event("other"));
        pending.push("tab-1".into(), event("new"));
        let (_, first) = pending.pop().unwrap();
        let (_, second) = pending.pop().unwrap();
        let ids = [first, second].map(|event| match event {
            crate::events::AppEvent::PluginCommandFinished { log_id, .. } => log_id,
            _ => unreachable!(),
        });
        assert_eq!(ids, ["new", "other"]);
        assert!(pending.pop().is_none());

        pending.push("tab-1".into(), event("old-in-flight"));
        let (scope_id, old) = pending.pop().unwrap();
        pending.push("tab-1".into(), event("new-while-full"));
        pending.retry_if_not_superseded(scope_id, old);
        let (_, retained) = pending.pop().unwrap();
        assert!(matches!(
            retained,
            crate::events::AppEvent::PluginCommandFinished { ref log_id, .. }
                if log_id == "new-while-full"
        ));
    }

    #[test]
    fn host_document_pending_is_latest_per_scope_and_fair_beyond_eight_scopes() {
        let mut pending = HostDocumentPending::default();
        for revision in [1, 2] {
            for index in 0..16 {
                pending
                    .push(HostDocumentRequest {
                        namespace: "example.docs".into(),
                        render_slot: None,
                        snapshot: crate::extension_contract::HostTerminalGroupsV1 {
                            contract: "host.terminal-groups.v1".into(),
                            generation: 7,
                            revision,
                            scope_id: format!("tab-{index:02}"),
                            groups: Vec::new(),
                            ungrouped: Vec::new(),
                        },
                        action: None,
                        action_response: None,
                        action_completion: None,
                        action_deadline: None,
                    })
                    .unwrap();
            }
        }
        let mut scopes = Vec::new();
        while let Some(request) = pending.pop() {
            assert_eq!(request.snapshot.revision, 2);
            scopes.push(request.snapshot.scope_id);
        }
        assert_eq!(scopes.len(), 16);
        assert_eq!(scopes[0], "tab-00");
        assert_eq!(scopes[15], "tab-15");
    }

    #[test]
    fn provider_action_execution_is_fenced_by_latest_scope_revision() {
        let desired = Mutex::new(std::collections::HashMap::from([("tab-1".into(), (7, 2))]));
        let mut snapshot = crate::extension_contract::HostTerminalGroupsV1 {
            contract: "host.terminal-groups.v1".into(),
            generation: 7,
            revision: 1,
            scope_id: "tab-1".into(),
            groups: Vec::new(),
            ungrouped: Vec::new(),
        };
        assert!(!host_document_action_is_current(&desired, &snapshot));
        snapshot.revision = 2;
        assert!(host_document_action_is_current(&desired, &snapshot));
    }

    #[test]
    fn saturated_render_admission_still_coalesces_latest_active_scope() {
        let slots = (0..32)
            .map(|_| Arc::new(HostDocumentRenderSlotGuard::acquire().unwrap()))
            .collect::<Vec<_>>();
        let active_render_slots = Arc::new(Mutex::new(std::collections::HashMap::from([(
            "tab-1".into(),
            (7, 1, slots[0].clone()),
        )])));
        let pending = Arc::new((
            Mutex::new(HostDocumentPending::default()),
            std::sync::Condvar::new(),
        ));
        let catalog = HostDocumentServiceCatalog {
            test_render_slots: Vec::new(),
            complete_catalog: true,
            generations: std::collections::BTreeMap::from([("example.docs".into(), 7)]),
            requests: std::collections::BTreeMap::from([(
                "example.docs".into(),
                HostDocumentWorkerHandle {
                    _provider_slot: None,
                    pending: pending.clone(),
                    desired_revisions: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_actions: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_renders: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_render_slots,
                    event_pending: None,
                    revoked: Arc::new(AtomicBool::new(false)),
                    join: None,
                    event_join: None,
                },
            )]),
            event_tx: None,
        };
        let snapshot = crate::extension_contract::HostTerminalGroupsV1 {
            scope_id: "tab-1".into(),
            contract: crate::extension_contract::HOST_DOCUMENT_CONTRACT_V1.into(),
            generation: 7,
            revision: 2,
            groups: Vec::new(),
            ungrouped: Vec::new(),
        };
        catalog.request("example.docs", snapshot).unwrap();
        assert!(pending
            .0
            .lock()
            .unwrap()
            .render_by_scope
            .contains_key("tab-1"));
        let unrelated = crate::extension_contract::HostTerminalGroupsV1 {
            scope_id: "tab-2".into(),
            contract: crate::extension_contract::HOST_DOCUMENT_CONTRACT_V1.into(),
            generation: 7,
            revision: 2,
            groups: Vec::new(),
            ungrouped: Vec::new(),
        };
        assert!(catalog
            .request("example.docs", unrelated)
            .unwrap_err()
            .contains("global"));
        drop(catalog);
        drop(slots);
        assert_eq!(HOST_DOCUMENT_RENDER_SLOTS.load(Ordering::Acquire), 0);
    }

    #[test]
    fn host_document_provider_limit_deduplicates_namespaces_before_truncation() {
        let make = |index: usize, namespace: &str| {
            (
                namespace.to_string(),
                ExtensionServiceSpec {
                    id: format!("service-{index}"),
                    command: vec!["false".into()],
                    cwd: PathBuf::from("."),
                    priority: 100 - index as i32,
                    granted_capabilities: Vec::new(),
                },
                std::collections::HashSet::new(),
            )
        };
        let mut specs = vec![
            make(100, crate::first_party::collections::NAMESPACE),
            make(101, "host.fallback"),
        ];
        specs.extend((0..20).map(|index| make(index, "duplicate.namespace")));
        specs.extend((0..20).map(|index| make(index + 20, &format!("unique.{index}"))));
        let limited = limit_host_document_specs(specs, true);
        assert_eq!(
            limited.len(),
            crate::extension_contract::MAX_DOCUMENT_PROVIDERS - 1
        );
        assert_eq!(limited[0].0, "duplicate.namespace");
        assert!(limited.iter().all(|(namespace, _, _)| {
            namespace != crate::first_party::collections::NAMESPACE && namespace != "host.fallback"
        }));
        assert_eq!(
            limited
                .iter()
                .map(|(namespace, _, _)| namespace)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            limited.len()
        );
    }

    #[test]
    fn pane_extension_work_cannot_reserve_the_entire_actor_queue() {
        let slots = std::sync::atomic::AtomicUsize::new(0);
        for _ in 0..32 {
            try_acquire_bounded_slot(&slots, "global extension queue full").unwrap();
        }
        assert!(
            try_acquire_bounded_slot(&slots, "global extension queue full")
                .unwrap_err()
                .contains("global")
        );
    }

    #[test]
    fn provider_action_admission_is_globally_bounded() {
        let slots = std::sync::atomic::AtomicUsize::new(0);
        for _ in 0..32 {
            try_acquire_bounded_slot(&slots, "global document queue full").unwrap();
        }
        assert!(
            try_acquire_bounded_slot(&slots, "global document queue full")
                .unwrap_err()
                .contains("global")
        );
    }

    #[test]
    fn queued_provider_action_deadline_prevents_late_execution() {
        let request = HostDocumentRequest {
            namespace: "example.docs".into(),
            render_slot: None,
            snapshot: crate::extension_contract::HostTerminalGroupsV1 {
                contract: "host.terminal-groups.v1".into(),
                generation: 7,
                revision: 9,
                scope_id: "tab-1".into(),
                groups: Vec::new(),
                ungrouped: Vec::new(),
            },
            action: Some(crate::extension_contract::HostDocumentExtensionActionV1 {
                contract: crate::extension_contract::HOST_DOCUMENT_CONTRACT_V1.into(),
                namespace: "example.docs".into(),
                generation: 7,
                revision: 9,
                scope_id: "tab-1".into(),
                action_id: "retry".into(),
                name: "retry".into(),
                subject_id: None,
            }),
            action_response: None,
            action_completion: None,
            action_deadline: Some(Instant::now() - Duration::from_millis(1)),
        };
        assert!(request.action_expired(Instant::now()));
    }

    #[test]
    fn host_document_only_plugin_enters_deterministic_catalog_selection() {
        let plugin: crate::api::schema::InstalledPluginInfo =
            serde_json::from_value(serde_json::json!({
                "plugin_id": "example.docs",
                "name": "Documents",
                "version": "1.0.0",
                "manifest_path": "/tmp/example/herdr-plugin.toml",
                "plugin_root": "/tmp/example",
                "enabled": true,
                "services": [
                    {
                        "id": "low",
                        "command": ["docs-low"],
                        "priority": 1,
                        "provides": ["host_document"],
                        "granted_capabilities": ["host_document"]
                    },
                    {
                        "id": "high",
                        "command": ["docs-high"],
                        "priority": 9,
                        "provides": ["host_document"],
                        "granted_capabilities": ["host_document", "focus_terminal"]
                    },
                    {
                        "id": "inspector",
                        "command": ["inspect"],
                        "priority": 99,
                        "provides": ["pane_inspector"]
                    }
                ]
            }))
            .unwrap();
        let plugins =
            crate::app::state::InstalledPluginRegistry::from([(plugin.plugin_id.clone(), plugin)]);
        let specs = host_document_specs(&plugins);
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].0, "example.docs");
        assert_eq!(specs[0].1.id, "example.docs.high");
        assert_eq!(specs[1].1.id, "example.docs.low");
        assert!(specs[0]
            .2
            .contains(&crate::extension_contract::HostCapability::FocusTerminal));
    }

    #[test]
    fn future_service_capabilities_decode_as_unknown() {
        assert_eq!(
            serde_json::from_str::<crate::api::schema::PluginServiceCapability>(
                "\"future_provider\""
            )
            .unwrap(),
            crate::api::schema::PluginServiceCapability::Unknown
        );
        assert_eq!(
            serde_json::from_str::<crate::api::schema::ExtensionServiceCapability>(
                "\"future_effect\""
            )
            .unwrap(),
            crate::api::schema::ExtensionServiceCapability::Unknown
        );
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_interrupts_handshake_without_consuming_restart_budget() {
        let mut supervisor = ExtensionServiceSupervisor::new(ExtensionServiceSpec {
            id: "test.handshake-cancel".into(),
            command: vec!["sh".into(), "-c".into(), "sleep 5".into()],
            cwd: PathBuf::from("."),
            priority: 0,
            granted_capabilities: vec![
                crate::api::schema::ExtensionServiceCapability::HostDocument,
            ],
        })
        .unwrap();
        let canceled = Arc::new(AtomicBool::new(false));
        let worker_canceled = canceled.clone();
        let worker = std::thread::spawn(move || {
            let result = supervisor.render_host_document_cancellable(
                &crate::extension_contract::HostTerminalGroupsV1 {
                    contract: "host.terminal-groups.v1".into(),
                    generation: 1,
                    revision: 1,
                    scope_id: "tab-1".into(),
                    groups: Vec::new(),
                    ungrouped: Vec::new(),
                },
                &worker_canceled,
            );
            (result, supervisor.failures, supervisor.status)
        });
        std::thread::sleep(Duration::from_millis(50));
        canceled.store(true, Ordering::Release);
        let (result, failures, status) = worker.join().unwrap();
        assert!(matches!(result, Err(ExtensionServiceError::Canceled)));
        assert_eq!(failures, 0);
        assert_eq!(status, ExtensionServiceStatus::Stopped);
    }

    #[cfg(unix)]
    #[test]
    fn host_document_protocol_io_observes_catalog_revocation() {
        let script = r#"
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"test.document-cancel","requested_capabilities":["host_document"],"methods":["host.document.render"]}'
IFS= read -r welcome
IFS= read -r request
sleep 5
"#;
        let supervisor = ExtensionServiceSupervisor::new(ExtensionServiceSpec {
            id: "test.document-cancel".into(),
            command: vec!["sh".into(), "-c".into(), script.into()],
            cwd: PathBuf::from("."),
            priority: 0,
            granted_capabilities: vec![
                crate::api::schema::ExtensionServiceCapability::HostDocument,
            ],
        })
        .unwrap();
        let canceled = Arc::new(AtomicBool::new(false));
        let worker_canceled = canceled.clone();
        let started = Instant::now();
        let worker = std::thread::spawn(move || {
            let mut supervisor = supervisor;
            supervisor.render_host_document_cancellable(
                &crate::extension_contract::HostTerminalGroupsV1 {
                    contract: "host.terminal-groups.v1".into(),
                    generation: 1,
                    revision: 1,
                    scope_id: "tab-1".into(),
                    groups: Vec::new(),
                    ungrouped: Vec::new(),
                },
                &worker_canceled,
            )
        });
        std::thread::sleep(Duration::from_millis(50));
        canceled.store(true, Ordering::Release);
        assert!(matches!(
            worker.join().unwrap(),
            Err(ExtensionServiceError::Canceled)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn revocation_interrupts_request_even_when_provider_stops_reading_stdin() {
        let script = r#"
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"test.document-blocked-stdin","requested_capabilities":["host_document"],"methods":["host.document.render","host.document.action"]}'
IFS= read -r welcome
sleep 5
"#;
        let supervisor = ExtensionServiceSupervisor::new(ExtensionServiceSpec {
            id: "test.document-blocked-stdin".into(),
            command: vec!["sh".into(), "-c".into(), script.into()],
            cwd: PathBuf::from("."),
            priority: 0,
            granted_capabilities: vec![
                crate::api::schema::ExtensionServiceCapability::HostDocument,
            ],
        })
        .unwrap();
        let canceled = Arc::new(AtomicBool::new(false));
        let worker_canceled = canceled.clone();
        let started = Instant::now();
        let worker = std::thread::spawn(move || {
            let mut supervisor = supervisor;
            supervisor.invoke_host_document_action_cancellable(
                &crate::extension_contract::HostDocumentExtensionActionV1 {
                    contract: crate::extension_contract::HOST_DOCUMENT_CONTRACT_V1.into(),
                    namespace: "test.document-blocked-stdin".into(),
                    generation: 1,
                    revision: 1,
                    scope_id: "tab-1".into(),
                    action_id: "blocked".into(),
                    name: "x".repeat(200_000),
                    subject_id: None,
                },
                &worker_canceled,
            )
        });
        std::thread::sleep(Duration::from_millis(50));
        canceled.store(true, Ordering::Release);
        assert!(matches!(
            worker.join().unwrap(),
            Err(ExtensionServiceError::Canceled)
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn host_document_terminal_interrupt_does_not_require_view_action() {
        let script = r#"
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"test.document-interrupt","requested_capabilities":["host_document","terminal_interrupt"],"methods":["host.document.render"]}'
IFS= read -r welcome
sleep 1
"#;
        let mut supervisor = ExtensionServiceSupervisor::new(ExtensionServiceSpec {
            id: "test.document-interrupt".into(),
            command: vec!["sh".into(), "-c".into(), script.into()],
            cwd: PathBuf::from("."),
            priority: 0,
            granted_capabilities: vec![
                crate::api::schema::ExtensionServiceCapability::HostDocument,
                crate::api::schema::ExtensionServiceCapability::TerminalInterrupt,
            ],
        })
        .unwrap();
        supervisor.start().unwrap();
        assert_eq!(supervisor.status, ExtensionServiceStatus::Running);
    }

    #[cfg(unix)]
    #[test]
    fn provider_owned_document_action_uses_fenced_service_result_path() {
        let script = r#"
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"test.document-action","methods":["host.document.action"]}'
IFS= read -r welcome
IFS= read -r request
id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"type":"response","id":%s,"result":{"accepted":true}}\n' "$id"
"#;
        let mut supervisor = ExtensionServiceSupervisor::new(ExtensionServiceSpec {
            id: "test.document-action".into(),
            command: vec!["sh".into(), "-c".into(), script.into()],
            cwd: PathBuf::from("."),
            priority: 0,
            granted_capabilities: Vec::new(),
        })
        .unwrap();
        supervisor
            .invoke_host_document_action(
                &crate::extension_contract::HostDocumentExtensionActionV1 {
                    contract: crate::extension_contract::HOST_DOCUMENT_CONTRACT_V1.into(),
                    namespace: "example.documents".into(),
                    generation: 7,
                    revision: 9,
                    scope_id: "w1:t1".into(),
                    action_id: "refresh".into(),
                    name: "refresh".into(),
                    subject_id: None,
                },
            )
            .unwrap();
        assert_eq!(supervisor.status, ExtensionServiceStatus::Running);
    }

    #[cfg(unix)]
    #[test]
    fn service_handshake_and_document_request_are_supervised() {
        let script = r#"
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"test.telemetry","methods":["view.render"]}'
IFS= read -r welcome
IFS= read -r request
id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"type":"response","id":%s,"result":{"document":{"title":"Processes","sections":[]},"actions":[]}}\n' "$id"
"#;
        let spec = ExtensionServiceSpec {
            id: "test.telemetry".into(),
            command: vec!["sh".into(), "-c".into(), script.into()],
            cwd: PathBuf::from("."),
            priority: 0,
            granted_capabilities: Vec::new(),
        };
        let mut supervisor = ExtensionServiceSupervisor::new(spec).unwrap();
        supervisor.start().unwrap();
        let document = supervisor
            .render_view(serde_json::json!({"pane_id": "p1"}))
            .unwrap();
        assert_eq!(document.document.title, "Processes");
        assert_eq!(supervisor.status, ExtensionServiceStatus::Running);
        supervisor.stop();
        assert_eq!(supervisor.status, ExtensionServiceStatus::Stopped);
    }
}
