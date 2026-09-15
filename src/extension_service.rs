//! Supervised, out-of-process extension services.
//!
//! Services use bounded newline-delimited JSON over stdio. This module deliberately exposes no
//! renderer callbacks or terminal escape surface; inspector providers return validated
//! [`crate::view_document::ViewDocument`] values.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Stdio};
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
    pub service_id: String,
    pub service_generation: u64,
    pub granted_capabilities: Vec<crate::api::schema::ExtensionServiceCapability>,
    pub identity: ExtensionRequestIdentity,
    pub kind: ExtensionAsyncResultKind,
}

struct ExtensionWorker {
    generation: Arc<std::sync::atomic::AtomicU64>,
    granted_capabilities: Vec<crate::api::schema::ExtensionServiceCapability>,
    requests: mpsc::SyncSender<ExtensionWorkerRequest>,
    result_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
}

pub struct ExtensionServiceManager {
    workers: std::collections::BTreeMap<String, ExtensionWorker>,
    provider: Option<String>,
    admissions: Arc<Mutex<std::collections::HashMap<ExtensionRequestIdentity, Arc<AtomicBool>>>>,
    cancellation_tombstones: Arc<Mutex<std::collections::VecDeque<ExtensionRequestIdentity>>>,
}

static NEXT_SERVICE_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

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
    let mut provider = None;
    let mut workers = std::collections::BTreeMap::new();
    let admissions = Arc::new(Mutex::new(std::collections::HashMap::new()));
    let cancellation_tombstones = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    for spec in specs {
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
        let thread_tombstones = cancellation_tombstones.clone();
        let thread_service_id = service_id.clone();
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
                                    remove_admission(&thread_admissions, &identity);
                                    remove_cancellation_tombstone(&thread_tombstones, &identity);
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
                                    remove_admission(&thread_admissions, &identity);
                                    remove_cancellation_tombstone(&thread_tombstones, &identity);
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
                        remove_admission(&thread_admissions, &identity);
                        remove_cancellation_tombstone(&thread_tombstones, &identity);
                        continue;
                    }
                    let result = ExtensionAsyncResult {
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
        if let Err(error) = spawn {
            tracing::warn!(%service_id, %error, "failed to start extension service worker");
            continue;
        }
        if provider.is_none() {
            provider = Some(service_id.clone());
        }
        workers.insert(
            service_id,
            ExtensionWorker {
                generation,
                granted_capabilities,
                requests: request_tx,
                result_tx: event_tx.clone(),
            },
        );
    }
    ExtensionServiceManager {
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
    pub fn complete(&self, identity: &ExtensionRequestIdentity) {
        remove_admission(&self.admissions, identity);
        remove_cancellation_tombstone(&self.cancellation_tombstones, identity);
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
        let result_permit = worker
            .result_tx
            .clone()
            .try_reserve_owned()
            .map_err(|error| format!("extension result queue unavailable: {error}"))?;
        let canceled = Arc::new(AtomicBool::new(false));
        {
            let mut admissions = self
                .admissions
                .lock()
                .map_err(|_| "extension admission registry is unavailable".to_owned())?;
            if admissions.contains_key(&identity) {
                return Err("duplicate extension request identity".into());
            }
            if admissions
                .keys()
                .any(|pending| pending.client_id == identity.client_id)
            {
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
        let result_permit = worker
            .result_tx
            .clone()
            .try_reserve_owned()
            .map_err(|error| format!("extension result queue unavailable: {error}"))?;
        let canceled = Arc::new(AtomicBool::new(false));
        {
            let mut admissions = self
                .admissions
                .lock()
                .map_err(|_| "extension admission registry is unavailable".to_owned())?;
            if admissions.contains_key(&identity) {
                return Err("duplicate extension request identity".into());
            }
            if admissions
                .keys()
                .any(|pending| pending.client_id == identity.client_id)
            {
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
        Ok(())
    }

    pub fn cancel(&self, identity: &ExtensionRequestIdentity) -> bool {
        let canceled = self
            .admissions
            .lock()
            .ok()
            .and_then(|mut admissions| admissions.remove(identity));
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

    pub fn start(&mut self) -> Result<(), ExtensionServiceError> {
        self.stop();
        self.status = ExtensionServiceStatus::Starting;
        match ExtensionServiceRuntime::spawn(&self.spec) {
            Ok(runtime) => {
                self.runtime = Some(runtime);
                self.failures = 0;
                self.status = ExtensionServiceStatus::Running;
                Ok(())
            }
            Err(error) => {
                self.record_failure(&error);
                Err(error)
            }
        }
    }

    pub fn ensure_running(&mut self, now: Instant) -> Result<(), ExtensionServiceError> {
        match &self.status {
            ExtensionServiceStatus::Running => return Ok(()),
            ExtensionServiceStatus::Backoff { retry_at, .. } if now < *retry_at => {
                return Err(ExtensionServiceError::Backoff)
            }
            ExtensionServiceStatus::Failed(message) => {
                return Err(ExtensionServiceError::RestartBudget(message.clone()))
            }
            ExtensionServiceStatus::Stopped
            | ExtensionServiceStatus::Starting
            | ExtensionServiceStatus::Backoff { .. } => {}
        }
        self.start()
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
        self.ensure_running(Instant::now())?;
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
                    self.record_failure(&error);
                }
                Err(error)
            }
        }
    }

    fn invoke_action_cancellable(
        &mut self,
        action_id: &str,
        target_pane_id: &str,
        canceled: &AtomicBool,
    ) -> Result<ExtensionHostEffect, ExtensionServiceError> {
        self.ensure_running(Instant::now())?;
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
                    self.record_failure(&error);
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
    _containment: crate::platform::ExtensionServiceContainment,
    stdin: ChildStdin,
    frames: mpsc::Receiver<Result<Vec<u8>, ExtensionServiceError>>,
    next_request_id: u64,
}

impl ExtensionServiceRuntime {
    fn spawn(spec: &ExtensionServiceSpec) -> Result<Self, ExtensionServiceError> {
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
            _containment: containment,
            stdin,
            frames,
            next_request_id: 1,
        };
        runtime.handshake(&spec.id, &spec.granted_capabilities)?;
        Ok(runtime)
    }

    fn handshake(
        &mut self,
        expected_id: &str,
        granted_capabilities: &[crate::api::schema::ExtensionServiceCapability],
    ) -> Result<(), ExtensionServiceError> {
        let frame = self.recv_frame(HANDSHAKE_TIMEOUT)?;
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
        if !hello.methods.iter().any(|method| method == "view.render")
            || (granted_capabilities
                .contains(&crate::api::schema::ExtensionServiceCapability::TerminalInterrupt)
                && !hello.methods.iter().any(|method| method == "view.action"))
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
        let welcome = ServiceWelcome {
            kind: "welcome",
            protocol: EXTENSION_SERVICE_PROTOCOL,
            granted_capabilities,
            methods: &["view.render", "view.action"],
            trust: "trusted_same_user_not_sandboxed",
        };
        let mut bytes = serde_json::to_vec(&welcome).map_err(|error| {
            ExtensionServiceError::Protocol(format!("cannot encode service welcome: {error}"))
        })?;
        bytes.push(b'\n');
        self.stdin
            .write_all(&bytes)
            .and_then(|_| self.stdin.flush())
            .map_err(ExtensionServiceError::Io)
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
        self.stdin
            .write_all(&bytes)
            .and_then(|_| self.stdin.flush())
            .map_err(ExtensionServiceError::Io)?;
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

    fn recv_frame(&mut self, timeout: Duration) -> Result<Vec<u8>, ExtensionServiceError> {
        match self.frames.recv_timeout(timeout) {
            Ok(Ok(frame)) if frame.is_empty() => Err(ExtensionServiceError::Exited),
            Ok(frame) => frame,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ExtensionServiceError::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ExtensionServiceError::Exited),
        }
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
        // Exact cancellation frees client admission synchronously even while the old jobs are
        // still queued/in flight; their separate tokens/tombstones fence late publication.
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
