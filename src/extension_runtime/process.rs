use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{atomic::AtomicBool, mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{
    ExtensionRuntime, NegotiatedRuntimeDescriptor, RuntimeError, RuntimeGrants, RuntimeHostCallV1,
    RuntimeIdentityV1, RuntimeRequestV1, RuntimeStopReason, SharedRuntimeHostCalls,
    HANDSHAKE_TIMEOUT, MAX_RUNTIME_FRAME_BYTES, MAX_RUNTIME_QUEUE_DEPTH,
};
use RuntimeError as ExtensionServiceError;

pub(crate) const EXTENSION_SERVICE_PROTOCOL: u16 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct ServiceHello {
    #[serde(rename = "type")]
    pub kind: String,
    pub protocol: u16,
    pub service_id: String,
    #[serde(default)]
    pub requested_capabilities: Vec<crate::api::schema::ExtensionServiceCapability>,
    #[serde(default)]
    pub methods: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct ExtensionServiceWelcomeV1 {
    #[serde(rename = "type")]
    pub kind: String,
    pub protocol: u16,
    pub granted_capabilities: Vec<crate::api::schema::ExtensionServiceCapability>,
    pub methods: Vec<String>,
    pub trust: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct ExtensionServiceRequestV1 {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: u64,
    pub method: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct ExtensionServiceResponseV1 {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

const MAX_STDERR_LINE_BYTES: usize = 4 * 1024;
const MAX_STDERR_LINES_PER_MINUTE: usize = 20;

#[derive(Clone, Debug)]
enum ProcessSource {
    Legacy {
        command: Vec<String>,
        cwd: PathBuf,
    },
    // Constructed only for verified installed packages that explicitly select a process payload.
    #[allow(dead_code)]
    Installed {
        archive: PathBuf,
        payload_path: String,
        payload_sha256: String,
        args: Vec<String>,
        cwd: PathBuf,
        package_version: String,
        package_sha256: String,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct ProcessRuntimeFactory {
    package_id: String,
    namespace: String,
    service_id: String,
    grants: RuntimeGrants,
    source: ProcessSource,
}

impl ProcessRuntimeFactory {
    pub(crate) fn legacy(
        id: String,
        command: Vec<String>,
        cwd: PathBuf,
        granted_capabilities: Vec<crate::api::schema::ExtensionServiceCapability>,
    ) -> Self {
        let capabilities = granted_capabilities
            .iter()
            .filter_map(|capability| serde_json::to_value(capability).ok())
            .filter_map(|value| value.as_str().map(str::to_owned))
            .collect();
        Self {
            package_id: format!("legacy.{id}"),
            namespace: id.clone(),
            service_id: id,
            grants: RuntimeGrants {
                legacy: granted_capabilities,
                capabilities,
                installed_package: false,
            },
            source: ProcessSource::Legacy { command, cwd },
        }
    }

    // Constructed only for verified installed packages that explicitly select a process payload.
    #[allow(dead_code)]
    pub(crate) fn installed(
        installed: &crate::extension_package::InstalledPackageV1,
        service_id: &str,
        namespace: &str,
    ) -> Result<Self, ExtensionServiceError> {
        crate::extension_package::verify_installed_package(
            Path::new(&installed.immutable_root),
            &installed.package_sha256,
        )
        .map_err(ExtensionServiceError::Configuration)?;
        let archive = Path::new(&installed.immutable_root).join("package.vqrox");
        let package = crate::extension_package::validate_package_file(
            &archive,
            Some(&installed.package_sha256),
        )
        .map_err(ExtensionServiceError::Configuration)?;
        if package.descriptor.package_sha256 != installed.package_sha256
            || package.descriptor.manifest_sha256 != installed.manifest_sha256
            || package.descriptor.manifest.id != installed.package_id
            || package.descriptor.manifest.version != installed.version
        {
            return Err(ExtensionServiceError::Configuration(
                "installed package descriptor does not match its verified artifact".into(),
            ));
        }
        if !package
            .descriptor
            .manifest
            .namespaces
            .iter()
            .any(|owned| owned == namespace)
        {
            return Err(ExtensionServiceError::Configuration(
                "selected runtime namespace is not package-owned".into(),
            ));
        }
        let service = package
            .descriptor
            .manifest
            .services
            .iter()
            .find(|service| service.id == service_id)
            .ok_or_else(|| {
                ExtensionServiceError::Configuration("package service is not declared".into())
            })?;
        let target = current_target()?;
        let resolved =
            crate::extension_package::resolve_runtime(&package.descriptor, service_id, target)
                .map_err(ExtensionServiceError::Configuration)?;
        if resolved.kind != "process" {
            // Historical error text is retained for compatibility; component execution is
            // production-capable through the component runtime factory, not this process factory.
            return Err(ExtensionServiceError::Unsupported(
                "component execution is deferred to M1c".into(),
            ));
        }
        let root =
            std::fs::canonicalize(&installed.immutable_root).map_err(ExtensionServiceError::Io)?;
        let executable = root.join(&resolved.payload_path);
        let executable_metadata =
            std::fs::symlink_metadata(&executable).map_err(ExtensionServiceError::Io)?;
        let canonical_executable =
            std::fs::canonicalize(&executable).map_err(ExtensionServiceError::Io)?;
        if !executable_metadata.file_type().is_file()
            || executable_metadata.file_type().is_symlink()
            || !canonical_executable.starts_with(&root)
            || !canonical_executable.is_absolute()
        {
            return Err(ExtensionServiceError::Configuration(
                "process payload is not a verified package-relative regular file".into(),
            ));
        }
        let mut legacy = Vec::new();
        let mut capabilities = BTreeSet::new();
        for capability in &service.capabilities {
            capabilities.insert(capability.clone());
            if let Some(capability) = legacy_capability(capability) {
                legacy.push(capability);
            }
        }
        for provided in &service.provides {
            if let Some(capability) = legacy_capability(provided) {
                legacy.push(capability);
            }
        }
        legacy.sort_by_key(|capability| format!("{capability:?}"));
        legacy.dedup();
        Ok(Self {
            package_id: installed.package_id.clone(),
            namespace: namespace.into(),
            service_id: service.id.clone(),
            grants: RuntimeGrants {
                legacy,
                capabilities,
                installed_package: true,
            },
            source: ProcessSource::Installed {
                archive,
                payload_path: resolved.payload_path,
                payload_sha256: resolved.payload_sha256,
                args: resolved.args,
                cwd: root,
                package_version: installed.version.clone(),
                package_sha256: installed.package_sha256.clone(),
            },
        })
    }

    pub(crate) fn identity(&self, generation: u64) -> RuntimeIdentityV1 {
        RuntimeIdentityV1 {
            package_id: self.package_id.clone(),
            namespace: self.namespace.clone(),
            service_id: self.service_id.clone(),
            generation,
        }
    }

    pub(crate) fn grants(&self) -> RuntimeGrants {
        self.grants.clone()
    }

    pub(crate) fn start(
        &self,
        generation: u64,
        canceled: &AtomicBool,
    ) -> Result<ProcessRuntime, ExtensionServiceError> {
        ProcessRuntime::spawn(self, generation, canceled)
    }
}

pub(crate) fn legacy_capability(
    value: &str,
) -> Option<crate::api::schema::ExtensionServiceCapability> {
    use crate::api::schema::ExtensionServiceCapability;
    match value {
        "view_contribution" => Some(ExtensionServiceCapability::ViewContribution),
        "host_document" => Some(ExtensionServiceCapability::HostDocument),
        "action_choices" => Some(ExtensionServiceCapability::ActionChoices),
        "action_invoke" => Some(ExtensionServiceCapability::ActionInvoke),
        "focus_terminal" => Some(ExtensionServiceCapability::FocusTerminal),
        "terminal_interrupt" => Some(ExtensionServiceCapability::TerminalInterrupt),
        _ => None,
    }
}

#[allow(dead_code)]
fn current_target() -> Result<crate::extension_package::PackageTargetV1, ExtensionServiceError> {
    use crate::extension_package::PackageTargetV1;
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok(PackageTargetV1::LinuxX86_64),
        ("linux", "aarch64") => Ok(PackageTargetV1::LinuxAarch64),
        ("macos", "x86_64") => Ok(PackageTargetV1::MacosX86_64),
        ("macos", "aarch64") => Ok(PackageTargetV1::MacosAarch64),
        ("windows", "x86_64") => Ok(PackageTargetV1::WindowsX86_64),
        (os, arch) => Err(ExtensionServiceError::Unsupported(format!(
            "package process target {os}-{arch} is unsupported"
        ))),
    }
}

pub(crate) struct ProcessRuntime {
    child: Option<Child>,
    containment: Arc<Mutex<Option<crate::platform::ExtensionServiceContainment>>>,
    writes: Option<mpsc::SyncSender<Vec<u8>>>,
    frames: Option<mpsc::Receiver<Result<Vec<u8>, ExtensionServiceError>>>,
    descriptor: NegotiatedRuntimeDescriptor,
    next_transport_request_id: u64,
    _execution_staging: Option<crate::platform::SecurePackageStaging>,
}

impl ProcessRuntime {
    fn spawn(
        factory: &ProcessRuntimeFactory,
        generation: u64,
        canceled: &AtomicBool,
    ) -> Result<Self, ExtensionServiceError> {
        let identity = factory.identity(generation);
        let mut execution_staging = None;
        let mut command = match &factory.source {
            ProcessSource::Legacy { command, cwd } => {
                let program = command.first().ok_or_else(|| {
                    ExtensionServiceError::Configuration("service command must not be empty".into())
                })?;
                let args = command.iter().skip(1).cloned().collect::<Vec<_>>();
                let mut command =
                    crate::plugin_command::command_for_argv_in_dir(program, &args, cwd);
                remove_host_session_environment(&mut command);
                command
            }
            ProcessSource::Installed {
                archive,
                payload_path,
                payload_sha256,
                args,
                cwd,
                package_version,
                package_sha256,
            } => {
                let package =
                    crate::extension_package::validate_package_file(archive, Some(package_sha256))
                        .map_err(ExtensionServiceError::Configuration)?;
                let resolved = crate::extension_package::resolve_runtime(
                    &package.descriptor,
                    &identity.service_id,
                    current_target()?,
                )
                .map_err(ExtensionServiceError::Configuration)?;
                if resolved.kind != "process"
                    || resolved.payload_path != *payload_path
                    || resolved.payload_sha256 != *payload_sha256
                {
                    return Err(ExtensionServiceError::Configuration(
                        "installed process payload changed after verification".into(),
                    ));
                }
                let payload = package.file(payload_path).ok_or_else(|| {
                    ExtensionServiceError::Configuration(
                        "installed process payload is absent from the verified archive".into(),
                    )
                })?;
                let staging_parent = std::env::temp_dir().join("vqro-runtime-staging");
                let staging = crate::platform::SecurePackageStaging::create(&staging_parent)
                    .map_err(ExtensionServiceError::Io)?;
                let staged_name = Path::new(payload_path)
                    .file_name()
                    .ok_or_else(|| {
                        ExtensionServiceError::Configuration(
                            "installed process payload has no file name".into(),
                        )
                    })?
                    .to_owned();
                staging
                    .write_file(Path::new(&staged_name), payload, 0o755)
                    .map_err(ExtensionServiceError::Io)?;
                let executable = staging.path().join(staged_name);
                let mut command = Command::new(executable);
                command.args(args).current_dir(cwd).env_clear();
                execution_staging = Some(staging);
                command
                    .env(crate::product::ENV_MARKER, "1")
                    .env("VQRO_EXTENSION_PACKAGE_ID", &identity.package_id)
                    .env("VQRO_EXTENSION_PACKAGE_VERSION", package_version)
                    .env("VQRO_EXTENSION_PACKAGE_SHA256", package_sha256)
                    .env("VQRO_EXTENSION_NAMESPACE", &identity.namespace)
                    .env("VQRO_EXTENSION_GENERATION", generation.to_string());
                command
            }
        };
        command
            .env("VQRO_EXTENSION_SERVICE_ID", &identity.service_id)
            .env(
                "VQRO_EXTENSION_SERVICE_PROTOCOL",
                EXTENSION_SERVICE_PROTOCOL.to_string(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        crate::platform::configure_extension_service_command(&mut command);
        let mut child = command.spawn().map_err(ExtensionServiceError::Io)?;
        let containment = match crate::platform::ExtensionServiceContainment::new(&child) {
            Ok(containment) => Arc::new(Mutex::new(Some(containment))),
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
        spawn_bounded_stderr_reader(identity.clone(), stderr);
        let (write_tx, write_rx) = mpsc::sync_channel::<Vec<u8>>(2);
        std::thread::spawn(move || {
            let mut stdin = stdin;
            while let Ok(bytes) = write_rx.recv() {
                if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
                    break;
                }
            }
        });
        let (frame_tx, frames) = mpsc::sync_channel(MAX_RUNTIME_QUEUE_DEPTH);
        let reader_containment = Arc::clone(&containment);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let frame = read_bounded_frame(&mut reader);
                let eof = matches!(&frame, Ok(bytes) if bytes.is_empty());
                let framing_failure = frame.is_err();
                if eof || framing_failure {
                    if let Ok(mut containment) = reader_containment.lock() {
                        containment.take();
                    }
                }
                if frame_tx.send(frame).is_err() || eof || framing_failure {
                    break;
                }
            }
        });
        let mut runtime = Self {
            child: Some(child),
            containment,
            writes: Some(write_tx),
            frames: Some(frames),
            descriptor: NegotiatedRuntimeDescriptor {
                identity,
                methods: BTreeSet::new(),
            },
            next_transport_request_id: 1,
            _execution_staging: execution_staging,
        };
        runtime.handshake(&factory.grants.legacy, canceled)?;
        Ok(runtime)
    }

    fn handshake(
        &mut self,
        granted_capabilities: &[crate::api::schema::ExtensionServiceCapability],
        canceled: &AtomicBool,
    ) -> Result<(), ExtensionServiceError> {
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        let frame = self.recv_frame_cancellable(deadline, canceled)?;
        let hello: ServiceHello = serde_json::from_slice(&frame).map_err(|error| {
            ExtensionServiceError::Protocol(format!("invalid service hello: {error}"))
        })?;
        if hello.kind != "hello"
            || hello.protocol != EXTENSION_SERVICE_PROTOCOL
            || hello.service_id != self.descriptor.identity.service_id
        {
            return Err(ExtensionServiceError::Protocol(
                "service hello identity or protocol mismatch".into(),
            ));
        }
        validate_negotiation(&hello, granted_capabilities)?;
        self.descriptor.methods = hello.methods.iter().cloned().collect();
        let requires_choices = granted_capabilities
            .contains(&crate::api::schema::ExtensionServiceCapability::ActionChoices);
        let mut host_methods = vec![
            "view.render",
            "view.action",
            "host.document.render",
            "host.document.action",
        ];
        if granted_capabilities
            .contains(&crate::api::schema::ExtensionServiceCapability::ActionInvoke)
        {
            host_methods.push("action.invoke.v1");
        }
        if requires_choices {
            host_methods.push(crate::action_choices::ACTION_CHOICES_LIST_METHOD_V1);
            host_methods.push(crate::action_choices::ACTION_CHOICE_INVOKE_METHOD_V1);
        }
        let welcome = ExtensionServiceWelcomeV1 {
            kind: "welcome".into(),
            protocol: EXTENSION_SERVICE_PROTOCOL,
            granted_capabilities: granted_capabilities.to_vec(),
            methods: host_methods.into_iter().map(str::to_owned).collect(),
            trust: "trusted_same_user_not_sandboxed".into(),
        };
        self.send_json(&welcome)
    }

    fn send_json(&self, value: &impl serde::Serialize) -> Result<(), ExtensionServiceError> {
        let mut bytes = serde_json::to_vec(value).map_err(|error| {
            ExtensionServiceError::Protocol(format!("cannot encode process frame: {error}"))
        })?;
        if bytes.len() > MAX_RUNTIME_FRAME_BYTES {
            return Err(ExtensionServiceError::Protocol(
                "process frame exceeds limit".into(),
            ));
        }
        bytes.push(b'\n');
        self.writes
            .as_ref()
            .ok_or(ExtensionServiceError::Exited)?
            .try_send(bytes)
            .map_err(|error| {
                ExtensionServiceError::QueueFull(format!(
                    "service input queue unavailable: {error}"
                ))
            })
    }

    fn recv_frame_cancellable(
        &mut self,
        deadline: Instant,
        canceled: &AtomicBool,
    ) -> Result<Vec<u8>, ExtensionServiceError> {
        loop {
            if canceled.load(std::sync::atomic::Ordering::Acquire) {
                return Err(ExtensionServiceError::Canceled);
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(ExtensionServiceError::Timeout);
            };
            match self
                .frames
                .as_ref()
                .ok_or(ExtensionServiceError::Exited)?
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

impl ExtensionRuntime for ProcessRuntime {
    fn descriptor(&self) -> &NegotiatedRuntimeDescriptor {
        &self.descriptor
    }

    fn invoke(
        &mut self,
        request: &RuntimeRequestV1,
        deadline: Instant,
        canceled: &AtomicBool,
        host_calls: &SharedRuntimeHostCalls,
        response_limit: usize,
    ) -> Result<serde_json::Value, ExtensionServiceError> {
        if request.identity != self.descriptor.identity {
            return Err(ExtensionServiceError::StaleGeneration {
                expected: self.descriptor.identity.generation,
                actual: request.identity.generation,
            });
        }
        let request_id = self.next_transport_request_id;
        self.next_transport_request_id =
            self.next_transport_request_id
                .checked_add(1)
                .ok_or_else(|| {
                    ExtensionServiceError::Protocol("transport request ID exhausted".into())
                })?;
        self.send_json(&ExtensionServiceRequestV1 {
            kind: "request".into(),
            id: request_id,
            method: request.method.clone(),
            params: request.params.clone(),
        })?;
        loop {
            let frame = self.recv_frame_cancellable(deadline, canceled)?;
            // This conservative wire bound is checked before deserializing an envelope or
            // allocating its JSON result. The result bytes are a strict subset of the frame.
            if frame.len() > response_limit {
                return Err(ExtensionServiceError::Protocol(
                    "process response exceeds the method response limit".into(),
                ));
            }
            let envelope: ProcessInboundEnvelope =
                serde_json::from_slice(&frame).map_err(|error| {
                    ExtensionServiceError::Protocol(format!("invalid service response: {error}"))
                })?;
            match envelope {
                ProcessInboundEnvelope::Response(response) => {
                    if response.kind != "response" {
                        return Err(ExtensionServiceError::Protocol(
                            "service response has an invalid type".into(),
                        ));
                    }
                    if response.id != request_id {
                        return Err(ExtensionServiceError::Protocol(
                            "service response request id mismatch".into(),
                        ));
                    }
                    return match (response.result, response.error) {
                        (Some(result), None) => Ok(result),
                        (None, Some(error)) => Err(ExtensionServiceError::Remote(error)),
                        (None, None) => Err(ExtensionServiceError::Protocol(
                            "service response omitted result and error".into(),
                        )),
                        (Some(_), Some(_)) => Err(ExtensionServiceError::Protocol(
                            "service response cannot contain both result and error".into(),
                        )),
                    };
                }
                ProcessInboundEnvelope::HostCall(call) => {
                    let response = host_calls.handle(call);
                    self.send_json(&response)?;
                }
            }
        }
    }

    fn probe_exit(&mut self) -> Result<(), ExtensionServiceError> {
        let child = self.child.as_mut().ok_or(ExtensionServiceError::Exited)?;
        match child.try_wait().map_err(ExtensionServiceError::Io)? {
            Some(_) => Err(ExtensionServiceError::Exited),
            None => Ok(()),
        }
    }

    fn stop(&mut self, _reason: RuntimeStopReason) {
        self.writes.take();
        self.frames.take();
        if let Ok(mut containment) = self.containment.lock() {
            containment.take();
        }
        if let Some(mut child) = self.child.take() {
            crate::platform::terminate_extension_service_process_group(&child);
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for ProcessRuntime {
    fn drop(&mut self) {
        self.stop(RuntimeStopReason::Drop);
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ProcessInboundEnvelope {
    Response(ExtensionServiceResponseV1),
    HostCall(RuntimeHostCallV1),
}

fn validate_negotiation(
    hello: &ServiceHello,
    granted_capabilities: &[crate::api::schema::ExtensionServiceCapability],
) -> Result<(), ExtensionServiceError> {
    let requires_view = granted_capabilities
        .contains(&crate::api::schema::ExtensionServiceCapability::ViewContribution);
    let requires_document = granted_capabilities
        .contains(&crate::api::schema::ExtensionServiceCapability::HostDocument);
    let requires_action = requires_view
        && granted_capabilities
            .contains(&crate::api::schema::ExtensionServiceCapability::TerminalInterrupt);
    let requires_choices = granted_capabilities
        .contains(&crate::api::schema::ExtensionServiceCapability::ActionChoices);
    let requires_generic_action = granted_capabilities
        .contains(&crate::api::schema::ExtensionServiceCapability::ActionInvoke);
    if (requires_generic_action
        && !hello
            .methods
            .iter()
            .any(|method| method == "action.invoke.v1"))
        || (requires_view && !hello.methods.iter().any(|method| method == "view.render"))
        || (requires_document
            && !hello
                .methods
                .iter()
                .any(|method| method == "host.document.render"))
        || (requires_action && !hello.methods.iter().any(|method| method == "view.action"))
        || (requires_choices
            && (!hello
                .methods
                .iter()
                .any(|method| method == crate::action_choices::ACTION_CHOICES_LIST_METHOD_V1)
                || !hello
                    .methods
                    .iter()
                    .any(|method| method == crate::action_choices::ACTION_CHOICE_INVOKE_METHOD_V1)))
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
    Ok(())
}

fn remove_host_session_environment(command: &mut Command) {
    command.env(crate::product::ENV_MARKER, "1");
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
}

fn spawn_bounded_stderr_reader(identity: RuntimeIdentityV1, stderr: std::process::ChildStderr) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut window_started = Instant::now();
        let mut emitted = 0_usize;
        loop {
            let mut line = Vec::with_capacity(MAX_STDERR_LINE_BYTES.min(256));
            let mut truncated = false;
            let mut eof = false;
            loop {
                let available = match reader.fill_buf() {
                    Ok(available) => available,
                    Err(error) => {
                        tracing::warn!(
                            package_id = %identity.package_id,
                            service_id = %identity.service_id,
                            generation = identity.generation,
                            %error,
                            "extension service stderr read failed"
                        );
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
                tracing::warn!(
                    package_id = %identity.package_id,
                    service_id = %identity.service_id,
                    generation = identity.generation,
                    truncated,
                    message = %message,
                    "extension service stderr"
                );
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
    let mut limited = std::io::Read::take(&mut *reader, (MAX_RUNTIME_FRAME_BYTES + 1) as u64);
    let read = limited
        .read_until(b'\n', &mut frame)
        .map_err(ExtensionServiceError::Io)?;
    if read == 0 {
        return Ok(Vec::new());
    }
    if frame.len() > MAX_RUNTIME_FRAME_BYTES || !frame.ends_with(b"\n") {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use sha2::{Digest, Sha256};

    #[cfg(unix)]
    fn sha256(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    #[cfg(unix)]
    fn temporary_directory(label: &str) -> PathBuf {
        let mut random = [0_u8; 8];
        crate::platform::fill_secure_random(&mut random).unwrap();
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let root = std::env::temp_dir().join(format!(
            "vqro-runtime-{label}-{}-{suffix}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[cfg(unix)]
    fn process_package_with_namespaces(
        script: &str,
        provides: &[&str],
        capabilities: &[&str],
        args: &[&str],
        namespaces: &[&str],
    ) -> Vec<u8> {
        let list = |values: &[&str]| {
            values
                .iter()
                .map(|value| serde_json::to_string(value).unwrap())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let provides = list(provides);
        let capabilities = list(capabilities);
        let args = list(args);
        let namespaces = list(namespaces);
        let manifest = format!(
            "manifest_version = 2\npackage_contract = \"vqro.package.v1\"\nid = \"example.runtime\"\nversion = \"1.0.0\"\nmin_vqro_version = \"0.1.0\"\nnamespaces = [{namespaces}]\n\n[[services]]\nid = \"echo\"\nprovides = [{provides}]\ncapabilities = [{capabilities}]\n\n[services.runtime]\nkind = \"process\"\nexecutables = {{ linux-x86_64 = \"services/linux-x86_64/service\", linux-aarch64 = \"services/linux-aarch64/service\", macos-x86_64 = \"services/macos-x86_64/service\", macos-aarch64 = \"services/macos-aarch64/service\", windows-x86_64 = \"services/windows-x86_64/service.exe\" }}\nargs = [{args}]\n"
        );
        let mut files = vec![(
            crate::extension_package::PACKAGE_MANIFEST_FILE.to_string(),
            manifest.into_bytes(),
            0o644,
        )];
        for path in [
            "services/linux-x86_64/service",
            "services/linux-aarch64/service",
            "services/macos-x86_64/service",
            "services/macos-aarch64/service",
            "services/windows-x86_64/service.exe",
        ] {
            files.push((path.into(), script.as_bytes().to_vec(), 0o755));
        }
        files.sort_by(|left, right| left.0.cmp(&right.0));
        let checksums = files
            .iter()
            .map(|(path, bytes, _)| format!("{}  {path}\n", sha256(bytes)))
            .collect::<String>();
        files.push((
            crate::extension_package::PACKAGE_CHECKSUM_FILE.into(),
            checksums.into_bytes(),
            0o644,
        ));
        files.sort_by(|left, right| left.0.cmp(&right.0));
        let mut archive = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut archive);
            builder.mode(tar::HeaderMode::Deterministic);
            for (path, bytes, mode) in files {
                let mut header = tar::Header::new_ustar();
                header.set_path(path).unwrap();
                header.set_size(bytes.len() as u64);
                header.set_mode(mode);
                header.set_uid(0);
                header.set_gid(0);
                header.set_mtime(0);
                header.set_cksum();
                builder.append(&header, bytes.as_slice()).unwrap();
            }
            builder.finish().unwrap();
        }
        archive
    }

    #[cfg(unix)]
    fn process_package_with(
        script: &str,
        provides: &[&str],
        capabilities: &[&str],
        args: &[&str],
    ) -> Vec<u8> {
        process_package_with_namespaces(script, provides, capabilities, args, &["example.runtime"])
    }

    #[cfg(unix)]
    fn process_package(script: &str, capabilities: &[&str]) -> Vec<u8> {
        process_package_with(script, &[], capabilities, &[])
    }

    #[cfg(unix)]
    fn install_package_bytes(
        bytes: Vec<u8>,
    ) -> (PathBuf, crate::extension_package::InstalledPackageV1) {
        let root = temporary_directory("package");
        let source = root.join("runtime.vqrox");
        std::fs::write(&source, &bytes).unwrap();
        let digest = sha256(&bytes);
        let installed = crate::extension_package::install_local_file(
            &source,
            &root.join("store"),
            Some(&digest),
            crate::extension_package::PackageProvenanceV1::File {
                package_sha256: digest.clone(),
            },
            &std::collections::BTreeMap::new(),
            None,
        )
        .unwrap();
        (root, installed)
    }

    #[cfg(unix)]
    fn install_process_package(
        script: &str,
        capabilities: &[&str],
    ) -> (PathBuf, crate::extension_package::InstalledPackageV1) {
        install_package_bytes(process_package(script, capabilities))
    }

    #[cfg(unix)]
    fn cleanup_package_root(root: PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let mut pending = vec![root.clone()];
        while let Some(directory) = pending.pop() {
            let mut permissions = std::fs::metadata(&directory).unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&directory, permissions).unwrap();
            for entry in std::fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
                    permissions.set_mode(0o600);
                    std::fs::set_permissions(path, permissions).unwrap();
                }
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn explicit_namespace_selection_is_invariant_under_manifest_order() {
        let script = "#!/bin/sh\nexit 0\n";
        let mut identities = Vec::new();
        for namespaces in [
            ["example.alpha", "example.beta"],
            ["example.beta", "example.alpha"],
        ] {
            let (root, installed) = install_package_bytes(process_package_with_namespaces(
                script,
                &[],
                &[crate::extension_host_state::HOST_STATE_READ_CAPABILITY],
                &[],
                &namespaces,
            ));
            let factory =
                ProcessRuntimeFactory::installed(&installed, "echo", "example.beta").unwrap();
            identities.push(factory.identity(7));
            assert!(
                ProcessRuntimeFactory::installed(&installed, "echo", "example.missing",).is_err()
            );
            assert!(
                crate::extension_host_state::HostStateProviderBinding::from_installed(
                    &installed,
                    "missing-service",
                    "example.beta",
                )
                .is_err()
            );
            cleanup_package_root(root);
        }
        assert_eq!(identities[0].namespace, "example.beta");
        assert_eq!(identities[0], identities[1]);
    }

    #[test]
    fn bounded_reader_rejects_oversized_or_unterminated_frames() {
        let mut oversized = std::io::Cursor::new(vec![b'x'; MAX_RUNTIME_FRAME_BYTES + 2]);
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
    fn installed_process_passes_shared_conformance_vectors_with_sanitized_launch() {
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"echo","requested_capabilities":[],"methods":["conformance.echo","conformance.document","conformance.environment"]}'
IFS= read -r welcome
while IFS= read -r request; do
  id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$request" in
    *conformance.environment*) printf '{"type":"response","id":%s,"result":{"path":"%s","home":"%s","package":"%s","cwd":"%s"}}\n' "$id" "${PATH-unset}" "${HOME-unset}" "$VQRO_EXTENSION_PACKAGE_ID" "$PWD" ;;
    *conformance.document*) printf '{"type":"response","id":%s,"result":{"document_id":"fixture","nodes":[]}}\n' "$id" ;;
    *'"params":[]'*) printf '{"type":"response","id":%s,"result":[]}\n' "$id" ;;
    *) printf '{"type":"response","id":%s,"result":{"text":"vqro","count":2}}\n' "$id" ;;
  esac
done
"#;
        let (root, installed) = install_process_package(script, &[]);
        assert!(matches!(
            crate::extension_service::ExtensionServiceSupervisor::preflight_installed_process(
                &installed,
                "echo",
                "example.runtime",
            ),
            Err(ExtensionServiceError::Unsupported(message))
                if message.contains("constrained component runtime")
        ));
        assert!(matches!(
            super::super::RuntimeFactory::installed(&installed, "echo", "example.runtime"),
            Err(ExtensionServiceError::Unsupported(message))
                if message.contains("constrained component runtime")
        ));
        let factory =
            super::super::RuntimeFactory::installed_process(&installed, "echo", "example.runtime")
                .unwrap();
        assert!(matches!(factory, super::super::RuntimeFactory::Process(_)));
        let canceled = AtomicBool::new(false);
        let mut dispatcher =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 7, &canceled).unwrap();
        assert!(matches!(
            dispatcher.invoke(
                "conformance.echo",
                serde_json::json!({"oversized": "x".repeat(MAX_RUNTIME_FRAME_BYTES)}),
                7,
                Instant::now() + Duration::from_millis(750),
                &canceled,
            ),
            Err(ExtensionServiceError::Protocol(message)) if message.contains("frame limit")
        ));
        for vector in super::super::shared_conformance_vectors() {
            let result = dispatcher
                .invoke(
                    vector.method,
                    vector.params,
                    7,
                    Instant::now() + Duration::from_millis(750),
                    &canceled,
                )
                .unwrap();
            assert_eq!(result, vector.expected, "{}", vector.name);
        }
        let environment = dispatcher
            .invoke(
                "conformance.environment",
                serde_json::json!({}),
                7,
                Instant::now() + Duration::from_millis(750),
                &canceled,
            )
            .unwrap();
        assert_ne!(
            environment["path"].as_str(),
            std::env::var("PATH").ok().as_deref()
        );
        assert_eq!(environment["home"], "unset");
        assert_eq!(environment["package"], "example.runtime");
        assert_eq!(
            std::fs::canonicalize(environment["cwd"].as_str().unwrap()).unwrap(),
            std::fs::canonicalize(&installed.immutable_root).unwrap()
        );
        assert!(matches!(
            dispatcher.invoke_with_response_limit_for_test(
                "conformance.echo",
                serde_json::json!({}),
                7,
                Instant::now() + Duration::from_millis(750),
                &canceled,
                8,
            ),
            Err(ExtensionServiceError::Protocol(message)) if message.contains("method response limit")
        ));
        dispatcher.stop(RuntimeStopReason::Requested);
        cleanup_package_root(root);
    }

    #[cfg(unix)]
    #[test]
    fn installed_process_rejects_host_calls_until_m2_and_fences_generations() {
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"echo","requested_capabilities":["host_document"],"methods":["host.document.render","conformance.echo"]}'
IFS= read -r welcome
IFS= read -r request
printf '%s\n' '{"type":"host_call","call_id":"call-1","identity":{"package_id":"example.runtime","namespace":"example.runtime","service_id":"echo","generation":9},"depth":1,"capability":"state","method":"state.snapshot","namespace":"example.runtime","params":{}}'
IFS= read -r host_response
id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
case "$host_response" in
  *unsupported_before_m2*) printf '{"type":"response","id":%s,"result":"rejected"}\n' "$id" ;;
  *) exit 7 ;;
esac
"#;
        let (root, installed) = install_package_bytes(process_package_with(
            script,
            &["host_document"],
            &["state"],
            &[],
        ));
        let factory =
            super::super::RuntimeFactory::installed_process(&installed, "echo", "example.runtime")
                .unwrap();
        let canceled = AtomicBool::new(false);
        let mut dispatcher =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 9, &canceled).unwrap();
        assert!(matches!(
            dispatcher.invoke(
                "conformance.echo",
                serde_json::json!({}),
                8,
                Instant::now() + Duration::from_millis(750),
                &canceled,
            ),
            Err(ExtensionServiceError::StaleGeneration {
                expected: 9,
                actual: 8
            })
        ));
        assert_eq!(
            dispatcher
                .invoke(
                    "conformance.echo",
                    serde_json::json!({}),
                    9,
                    Instant::now() + Duration::from_millis(750),
                    &canceled,
                )
                .unwrap(),
            "rejected"
        );
        dispatcher.stop(RuntimeStopReason::Requested);
        cleanup_package_root(root);
    }

    #[cfg(unix)]
    #[test]
    fn terminal_wrapper_does_not_pre_reject_an_ungranted_no_call_invocation() {
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"echo","requested_capabilities":[],"methods":["conformance.echo"]}'
IFS= read -r welcome
IFS= read -r request
id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"type":"response","id":%s,"result":"ran"}\n' "$id"
"#;
        let (root, installed) = install_package_bytes(process_package_with(script, &[], &[], &[]));
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let authorization = crate::extension_host_terminals::install_test_package_and_authorize(
            &mut app,
            &installed,
            "echo",
            "example.runtime",
            9,
        );
        let factory =
            super::super::RuntimeFactory::installed_process(&installed, "echo", "example.runtime")
                .unwrap();
        let canceled = Arc::new(AtomicBool::new(false));
        let mut dispatcher =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 9, canceled.as_ref())
                .unwrap();
        let result = dispatcher
            .invoke_with_host_terminals(
                "conformance.echo",
                serde_json::json!({}),
                9,
                Instant::now() + Duration::from_millis(750),
                canceled,
                authorization,
            )
            .unwrap();
        assert_eq!(result, "ran");
        cleanup_package_root(root);
    }

    #[cfg(unix)]
    #[test]
    fn terminal_wrapper_reports_capability_denied_for_an_ungranted_attempt() {
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"echo","requested_capabilities":[],"methods":["conformance.echo"]}'
IFS= read -r welcome
IFS= read -r request
printf '%s\n' '{"type":"host_call","call_id":"terminal","identity":{"package_id":"example.runtime","namespace":"example.runtime","service_id":"echo","generation":9},"depth":1,"capability":"host.terminals.read","method":"terminals.snapshot","namespace":"example.runtime","params":{"contract":"host.terminals.v1"}}'
IFS= read -r host_response
id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"type":"response","id":%s,"result":%s}\n' "$id" "$host_response"
"#;
        let (root, installed) = install_package_bytes(process_package_with(script, &[], &[], &[]));
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let authorization = crate::extension_host_terminals::install_test_package_and_authorize(
            &mut app,
            &installed,
            "echo",
            "example.runtime",
            9,
        );
        let factory =
            super::super::RuntimeFactory::installed_process(&installed, "echo", "example.runtime")
                .unwrap();
        let canceled = Arc::new(AtomicBool::new(false));
        let mut dispatcher =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 9, canceled.as_ref())
                .unwrap();
        let result = dispatcher
            .invoke_with_host_terminals(
                "conformance.echo",
                serde_json::json!({}),
                9,
                Instant::now() + Duration::from_millis(750),
                canceled,
                authorization,
            )
            .unwrap();
        assert_eq!(result["error"]["code"], "capability_denied");
        assert!(app.event_rx.try_recv().is_err());
        cleanup_package_root(root);
    }

    #[cfg(unix)]
    #[test]
    fn installed_process_executes_staged_host_state_snapshot_through_actor() {
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"echo","requested_capabilities":[],"methods":["conformance.echo"]}'
IFS= read -r welcome
IFS= read -r request
printf '%s\n' '{"type":"host_call","call_id":"call-1","identity":{"package_id":"example.runtime","namespace":"example.runtime","service_id":"echo","generation":9},"depth":1,"capability":"host.state.read","method":"state.snapshot","namespace":"example.runtime","params":{"contract":"host.state.v1"}}'
IFS= read -r host_response
id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"type":"response","id":%s,"result":%s}\n' "$id" "$host_response"
"#;
        let (root, installed) = install_package_bytes(process_package_with(
            script,
            &[],
            &[crate::extension_host_state::HOST_STATE_READ_CAPABILITY],
            &[],
        ));
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let (authorization, _) = crate::extension_host_state::install_test_package_and_authorize(
            &mut app,
            &installed,
            "echo",
            "example.runtime",
            9,
        );
        let factory =
            super::super::RuntimeFactory::installed_process(&installed, "echo", "example.runtime")
                .unwrap();
        let canceled = Arc::new(AtomicBool::new(false));
        let join = std::thread::spawn(move || {
            let mut dispatcher =
                super::super::ExtensionRuntimeDispatcher::start(&factory, 9, canceled.as_ref())
                    .unwrap();
            dispatcher.invoke_with_host_state(
                "conformance.echo",
                serde_json::json!({}),
                9,
                Instant::now() + Duration::from_millis(750),
                canceled,
                authorization,
            )
        });
        let join = super::super::handle_app_events_until(
            &mut app,
            "process host-state snapshot",
            join,
            |event| {
                super::super::host_state_actor_request_matches(
                    event,
                    "1",
                    &super::super::RuntimeIdentityV1 {
                        package_id: "example.runtime".into(),
                        namespace: "example.runtime".into(),
                        service_id: "echo".into(),
                        generation: 9,
                    },
                    "call-1",
                    crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
                    crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
                )
            },
        );
        let response = join.join().unwrap().unwrap();
        assert_eq!(response["result"]["contract"], "host.state.v1");
        assert_eq!(response["result"]["namespace"], "example.runtime");
        cleanup_package_root(root);
    }

    #[cfg(unix)]
    #[test]
    fn installed_process_uses_one_composite_state_and_terminal_read_context() {
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"echo","requested_capabilities":[],"methods":["conformance.echo"]}'
IFS= read -r welcome
IFS= read -r request
printf '%s\n' '{"type":"host_call","call_id":"terminals","identity":{"package_id":"example.runtime","namespace":"example.runtime","service_id":"echo","generation":9},"depth":1,"capability":"host.terminals.read","method":"terminals.snapshot","namespace":"example.runtime","params":{"contract":"host.terminals.v1"}}'
IFS= read -r terminal_response
printf '%s\n' '{"type":"host_call","call_id":"state","identity":{"package_id":"example.runtime","namespace":"example.runtime","service_id":"echo","generation":9},"depth":1,"capability":"host.state.read","method":"state.snapshot","namespace":"example.runtime","params":{"contract":"host.state.v1"}}'
IFS= read -r state_response
id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"type":"response","id":%s,"result":{"terminals":%s,"state":%s}}\n' "$id" "$terminal_response" "$state_response"
"#;
        let (root, installed) = install_package_bytes(process_package_with(
            script,
            &[],
            &[
                crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
                crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY,
            ],
            &[],
        ));
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let _ = crate::extension_host_terminals::install_test_package_and_authorize(
            &mut app,
            &installed,
            "echo",
            "example.runtime",
            9,
        );
        let binding = crate::extension_host_state::HostProviderBinding::from_installed(
            &installed,
            "echo",
            "example.runtime",
        )
        .unwrap();
        let workspace_id = app.state.workspaces[0].id.clone();
        let tab_id = app.state.workspaces[0].tabs[0].id.clone();
        let authorization = app
            .stage_host_read_authorization(
                &binding,
                crate::extension_host_invocation::HostReadInvocationPurpose {
                    state: crate::extension_host_invocation::HostStateReadSource::Durable,
                    terminals: Some((workspace_id, tab_id)),
                    provisional_container: None,
                },
                5,
                7,
                9,
            )
            .unwrap();
        let observations = authorization.terminal_observations();
        let factory =
            super::super::RuntimeFactory::installed_process(&installed, "echo", "example.runtime")
                .unwrap();
        let canceled = Arc::new(AtomicBool::new(false));
        let join = std::thread::spawn(move || {
            let mut dispatcher =
                super::super::ExtensionRuntimeDispatcher::start(&factory, 9, canceled.as_ref())
                    .unwrap();
            dispatcher.invoke_with_host_reads(
                "conformance.echo",
                serde_json::json!({}),
                9,
                Instant::now() + Duration::from_millis(750),
                canceled,
                authorization,
            )
        });
        let join = super::super::handle_app_events_until(
            &mut app,
            "process composite reads",
            join,
            |event| {
                super::super::host_state_actor_request_matches(
                    event,
                    "1",
                    &super::super::RuntimeIdentityV1 {
                        package_id: "example.runtime".into(),
                        namespace: "example.runtime".into(),
                        service_id: "echo".into(),
                        generation: 9,
                    },
                    "state",
                    crate::extension_host_state::HOST_STATE_READ_CAPABILITY,
                    crate::extension_host_state::HOST_STATE_SNAPSHOT_METHOD,
                )
            },
        );
        let response = join.join().unwrap().unwrap();
        assert_eq!(
            response["terminals"]["result"]["contract"],
            "host.terminals.v1"
        );
        assert_eq!(response["state"]["result"]["contract"], "host.state.v1");
        let snapshot = observations.exactly_one().unwrap();
        let dependency = crate::host_document_v2::terminal_snapshot_dependency(
            snapshot.tab_id.as_str(),
            &snapshot.fingerprint_sha256,
            snapshot.lease_fence.provider_generation,
        )
        .unwrap();
        assert_eq!(dependency.contract, "host.terminals.v1");
        assert_eq!(dependency.generation, 7);
        cleanup_package_root(root);
    }

    #[cfg(unix)]
    #[test]
    fn terminal_harness_handles_unrelated_same_method_before_catalog_race() {
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"echo","requested_capabilities":[],"methods":["conformance.echo"]}'
IFS= read -r welcome
IFS= read -r request
printf '%s\n' '{"type":"host_call","call_id":"call-1","identity":{"package_id":"example.runtime","namespace":"example.runtime","service_id":"echo","generation":9},"depth":1,"capability":"host.terminals.read","method":"terminals.snapshot","namespace":"example.runtime","params":{"contract":"host.terminals.v1"}}'
IFS= read -r host_response
id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"type":"response","id":%s,"result":%s}\n' "$id" "$host_response"
"#;
        let (root, installed) = install_package_bytes(process_package_with(
            script,
            &[],
            &[crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY],
            &[],
        ));
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let authorization = crate::extension_host_terminals::install_test_package_and_authorize(
            &mut app,
            &installed,
            "echo",
            "example.runtime",
            9,
        );
        let expected_identity = super::super::RuntimeIdentityV1 {
            package_id: "example.runtime".into(),
            namespace: "example.runtime".into(),
            service_id: "echo".into(),
            generation: 9,
        };
        let decoy_lease = authorization.bind(
            "unrelated".into(),
            expected_identity.clone(),
            Instant::now() + Duration::from_secs(2),
            Arc::new(AtomicBool::new(false)),
        );
        let (decoy_respond_to, decoy_response) = std::sync::mpsc::sync_channel(1);
        app.event_tx
            .try_send(crate::events::AppEvent::ExtensionHostTerminalsCall(
                Box::new(crate::extension_host_terminals::HostTerminalsActorRequest {
                    lease: decoy_lease,
                    call_id: "call-1".into(),
                    capability: crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY
                        .into(),
                    method: crate::extension_host_terminals::HostTerminalsActorMethod::Capture,
                    respond_to: decoy_respond_to,
                }),
            ))
            .unwrap();
        let factory =
            super::super::RuntimeFactory::installed_process(&installed, "echo", "example.runtime")
                .unwrap();
        let canceled = Arc::new(AtomicBool::new(false));
        let join = std::thread::spawn(move || {
            let mut dispatcher =
                super::super::ExtensionRuntimeDispatcher::start(&factory, 9, canceled.as_ref())
                    .unwrap();
            dispatcher.invoke_with_host_terminals(
                "conformance.echo",
                serde_json::json!({}),
                9,
                Instant::now() + Duration::from_millis(750),
                canceled,
                authorization,
            )
        });
        let capture = super::super::take_host_terminals_actor_request(
            &mut app,
            "1",
            &expected_identity,
            "call-1",
            crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY,
            crate::extension_host_terminals::HostTerminalsActorMethod::Capture,
            "process terminal capture",
        );
        assert!(decoy_response.try_recv().unwrap().is_ok());
        app.handle_extension_host_terminals_call(*capture);
        app.marketplace_state.packages.revision += 1;
        let validate = super::super::take_host_terminals_actor_request(
            &mut app,
            "1",
            &expected_identity,
            "call-1",
            crate::extension_host_terminals::HOST_TERMINALS_READ_CAPABILITY,
            crate::extension_host_terminals::HostTerminalsActorMethod::Validate,
            "process terminal final authorization",
        );
        app.handle_extension_host_terminals_call(*validate);
        let response = join.join().unwrap().unwrap();
        assert_eq!(response["error"]["code"], "stale_host_call_lease");
        cleanup_package_root(root);
    }

    #[cfg(unix)]
    #[test]
    fn installed_process_transact_preserves_json_null_and_delete_semantics() {
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"echo","requested_capabilities":[],"methods":["conformance.echo"]}'
IFS= read -r welcome
count=0
while IFS= read -r request; do
  id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  if [ "$count" -eq 0 ]; then
    printf '%s\n' '{"type":"host_call","call_id":"set-null","identity":{"package_id":"example.runtime","namespace":"example.runtime","service_id":"echo","generation":19},"depth":1,"capability":"host.state.write","method":"state.transact","namespace":"example.runtime","params":{"contract":"host.state.v1","expected_store_generation":1,"expected_revision":0,"idempotency_key":"set-null","operations":[{"op":"set","key":"nullable","value":null}]}}'
  else
    printf '%s\n' '{"type":"host_call","call_id":"delete-null","identity":{"package_id":"example.runtime","namespace":"example.runtime","service_id":"echo","generation":19},"depth":1,"capability":"host.state.write","method":"state.transact","namespace":"example.runtime","params":{"contract":"host.state.v1","expected_store_generation":1,"expected_revision":1,"idempotency_key":"delete-null","operations":[{"op":"delete","key":"nullable"}]}}'
  fi
  IFS= read -r host_response
  printf '{"type":"response","id":%s,"result":%s}\n' "$id" "$host_response"
  count=$((count + 1))
done
"#;
        let (root, installed) = install_package_bytes(process_package_with(
            script,
            &[],
            &[crate::extension_host_state::HOST_STATE_WRITE_CAPABILITY],
            &[],
        ));
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let (authorization, _) = crate::extension_host_state::install_test_package_and_authorize(
            &mut app,
            &installed,
            "echo",
            "example.runtime",
            19,
        );
        let factory =
            super::super::RuntimeFactory::installed_process(&installed, "echo", "example.runtime")
                .unwrap();
        let canceled = Arc::new(AtomicBool::new(false));
        let mut join = std::thread::spawn(move || {
            let mut dispatcher =
                super::super::ExtensionRuntimeDispatcher::start(&factory, 19, canceled.as_ref())
                    .unwrap();
            let set = dispatcher
                .invoke_with_host_state(
                    "conformance.echo",
                    serde_json::json!({}),
                    19,
                    Instant::now() + Duration::from_millis(750),
                    canceled.clone(),
                    authorization.clone(),
                )
                .unwrap();
            let delete = dispatcher
                .invoke_with_host_state(
                    "conformance.echo",
                    serde_json::json!({}),
                    19,
                    Instant::now() + Duration::from_millis(750),
                    canceled,
                    authorization,
                )
                .unwrap();
            (set, delete)
        });
        for expected_revision in [1, 2] {
            join = super::super::handle_app_events_until(
                &mut app,
                "process host-state transaction",
                join,
                |event| {
                    let outer_request_id = if expected_revision == 1 { "1" } else { "2" };
                    let call_id = if expected_revision == 1 {
                        "set-null"
                    } else {
                        "delete-null"
                    };
                    super::super::host_state_actor_request_matches(
                        event,
                        outer_request_id,
                        &super::super::RuntimeIdentityV1 {
                            package_id: "example.runtime".into(),
                            namespace: "example.runtime".into(),
                            service_id: "echo".into(),
                            generation: 19,
                        },
                        call_id,
                        crate::extension_host_state::HOST_STATE_WRITE_CAPABILITY,
                        crate::extension_host_state::HOST_STATE_TRANSACT_METHOD,
                    ) && matches!(
                        event,
                        crate::events::AppEvent::ExtensionHostStateCall(request)
                            if matches!(
                                &request.method,
                                crate::extension_host_state::HostStateActorMethod::Transact(transaction)
                                    if transaction.expected_revision == expected_revision - 1
                                        && transaction.idempotency_key == call_id
                            )
                    )
                },
            );
            assert_eq!(
                app.extension_state
                    .snapshot("example.runtime")
                    .unwrap()
                    .revision,
                expected_revision
            );
            if expected_revision == 1 {
                assert_eq!(
                    app.extension_state
                        .snapshot("example.runtime")
                        .unwrap()
                        .values["nullable"],
                    serde_json::Value::Null
                );
            }
        }
        let (set, delete) = join.join().unwrap();
        assert_eq!(set["result"]["revision"], 1);
        assert_eq!(delete["result"]["revision"], 2);
        assert!(!app
            .extension_state
            .snapshot("example.runtime")
            .unwrap()
            .values
            .contains_key("nullable"));
        cleanup_package_root(root);
    }

    #[cfg(unix)]
    #[test]
    fn process_transport_normalizes_denial_stale_cancellation_and_oversize_errors() {
        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"echo","requested_capabilities":[],"methods":["conformance.echo"]}'
IFS= read -r welcome
IFS= read -r request
printf '%s\n' '{"type":"host_call","call_id":"denied","identity":{"package_id":"example.runtime","namespace":"example.runtime","service_id":"echo","generation":29},"depth":1,"capability":"host.state.read","method":"state.snapshot","namespace":"example.runtime","params":{"contract":"host.state.v1"}}'
IFS= read -r host_response
id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"type":"response","id":%s,"result":%s}\n' "$id" "$host_response"
"#;
        let (root, installed) = install_process_package(script, &[]);
        let factory =
            super::super::RuntimeFactory::installed_process(&installed, "echo", "example.runtime")
                .unwrap();
        let canceled = AtomicBool::new(false);
        let mut dispatcher =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 29, &canceled).unwrap();
        assert_eq!(
            dispatcher
                .invoke(
                    "conformance.echo",
                    serde_json::json!({}),
                    29,
                    Instant::now() + Duration::from_millis(750),
                    &canceled,
                )
                .unwrap()["error"]["code"],
            "capability_denied"
        );
        assert!(matches!(
            dispatcher.invoke(
                "conformance.echo",
                serde_json::json!({}),
                28,
                Instant::now() + Duration::from_millis(750),
                &canceled,
            ),
            Err(RuntimeError::StaleGeneration { .. })
        ));
        let canceled = AtomicBool::new(true);
        assert!(matches!(
            dispatcher.invoke(
                "conformance.echo",
                serde_json::json!({}),
                29,
                Instant::now() + Duration::from_millis(750),
                &canceled,
            ),
            Err(RuntimeError::Canceled)
        ));
        let canceled = AtomicBool::new(false);
        assert!(matches!(
            dispatcher.invoke(
                "conformance.echo",
                serde_json::json!({"oversized": "x".repeat(MAX_RUNTIME_FRAME_BYTES)}),
                29,
                Instant::now() + Duration::from_millis(750),
                &canceled,
            ),
            Err(RuntimeError::Protocol(message)) if message.contains("frame limit")
        ));
        cleanup_package_root(root);
    }

    #[cfg(unix)]
    #[test]
    fn installed_process_rejects_wrong_response_types_and_changed_archives() {
        use std::os::unix::fs::PermissionsExt;

        let script = r#"#!/bin/sh
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"echo","requested_capabilities":[],"methods":["conformance.echo"]}'
IFS= read -r welcome
IFS= read -r request
id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"type":"garbage","id":%s,"result":{}}\n' "$id"
"#;
        let (root, installed) = install_process_package(script, &[]);
        let factory =
            super::super::RuntimeFactory::installed_process(&installed, "echo", "example.runtime")
                .unwrap();
        let canceled = AtomicBool::new(false);
        let mut dispatcher =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 11, &canceled).unwrap();
        assert!(matches!(
            dispatcher.invoke(
                "conformance.echo",
                serde_json::json!({}),
                11,
                Instant::now() + Duration::from_millis(750),
                &canceled,
            ),
            Err(ExtensionServiceError::Protocol(message))
                if message.contains("invalid type")
        ));
        dispatcher.stop(RuntimeStopReason::Protocol);

        let factory =
            super::super::RuntimeFactory::installed_process(&installed, "echo", "example.runtime")
                .unwrap();
        let archive = Path::new(&installed.immutable_root).join("package.vqrox");
        let mut permissions = std::fs::metadata(&archive).unwrap().permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&archive, permissions).unwrap();
        std::fs::write(&archive, b"changed after factory construction").unwrap();
        assert!(matches!(
            super::super::ExtensionRuntimeDispatcher::start(&factory, 12, &canceled),
            Err(ExtensionServiceError::Configuration(_))
        ));
        cleanup_package_root(root);
    }

    #[cfg(unix)]
    #[test]
    fn installed_process_preserves_fixed_argv_and_reaps_idle_descendants() {
        let argv_script = r#"#!/bin/sh
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"echo","requested_capabilities":[],"methods":["conformance.echo"]}'
IFS= read -r welcome
IFS= read -r request
id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
if [ "$#" -eq 4 ] && [ "$1" = "space value" ] && [ "$2" = "" ] && [ "$3" = "quote'value" ] && [ "$4" = "雪" ]; then
  printf '{"type":"response","id":%s,"result":"argv-ok"}\n' "$id"
else
  exit 9
fi
"#;
        let bytes = process_package_with(
            argv_script,
            &[],
            &[],
            &["space value", "", "quote'value", "雪"],
        );
        let (argv_root, installed) = install_package_bytes(bytes);
        let factory =
            super::super::RuntimeFactory::installed_process(&installed, "echo", "example.runtime")
                .unwrap();
        let canceled = AtomicBool::new(false);
        let mut dispatcher =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 13, &canceled).unwrap();
        assert_eq!(
            dispatcher
                .invoke(
                    "conformance.echo",
                    serde_json::json!({}),
                    13,
                    Instant::now() + Duration::from_millis(750),
                    &canceled,
                )
                .unwrap(),
            "argv-ok"
        );
        dispatcher.stop(RuntimeStopReason::Requested);
        cleanup_package_root(argv_root);

        let idle_script = r#"#!/bin/sh
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"echo","requested_capabilities":[],"methods":[]}'
IFS= read -r welcome
sleep 30 </dev/null >/dev/null 2>&1 &
printf '%s\n' "$!"
exit 0
"#;
        let (idle_root, installed) = install_process_package(idle_script, &[]);
        let factory =
            ProcessRuntimeFactory::installed(&installed, "echo", "example.runtime").unwrap();
        let mut runtime = factory.start(14, &canceled).unwrap();
        let pid_frame = runtime
            .frames
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_millis(750))
            .unwrap()
            .unwrap();
        let pid = std::str::from_utf8(&pid_frame)
            .unwrap()
            .parse::<i32>()
            .unwrap();
        let started = Instant::now();
        loop {
            // SAFETY: signal 0 only probes process existence.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(10));
        }
        runtime.stop(RuntimeStopReason::Drop);
        cleanup_package_root(idle_root);
    }

    #[cfg(unix)]
    #[test]
    fn installed_process_timeout_crash_and_descendants_retire_the_generation() {
        let timeout_script = r#"#!/bin/sh
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"echo","requested_capabilities":[],"methods":["conformance.echo"]}'
IFS= read -r welcome
IFS= read -r request
sleep 2
"#;
        let (timeout_root, timeout_package) = install_process_package(timeout_script, &[]);
        let factory = super::super::RuntimeFactory::installed_process(
            &timeout_package,
            "echo",
            "example.runtime",
        )
        .unwrap();
        let canceled = AtomicBool::new(false);
        let mut dispatcher =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 1, &canceled).unwrap();
        canceled.store(true, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            dispatcher.invoke(
                "conformance.echo",
                serde_json::json!({}),
                1,
                Instant::now() + Duration::from_millis(50),
                &canceled,
            ),
            Err(ExtensionServiceError::Canceled)
        ));
        canceled.store(false, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            dispatcher.invoke(
                "conformance.echo",
                serde_json::json!({}),
                1,
                Instant::now() + Duration::from_millis(50),
                &canceled,
            ),
            Err(ExtensionServiceError::Timeout)
        ));
        dispatcher.stop(RuntimeStopReason::Deadline);
        cleanup_package_root(timeout_root);

        let crash_script = r#"#!/bin/sh
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"echo","requested_capabilities":[],"methods":["conformance.echo"]}'
IFS= read -r welcome
IFS= read -r request
exit 3
"#;
        let (crash_root, crash_package) = install_process_package(crash_script, &[]);
        let factory = super::super::RuntimeFactory::installed_process(
            &crash_package,
            "echo",
            "example.runtime",
        )
        .unwrap();
        let mut dispatcher =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 2, &canceled).unwrap();
        assert!(matches!(
            dispatcher.invoke(
                "conformance.echo",
                serde_json::json!({}),
                2,
                Instant::now() + Duration::from_millis(750),
                &canceled,
            ),
            Err(ExtensionServiceError::Exited)
        ));
        dispatcher.stop(RuntimeStopReason::Protocol);
        cleanup_package_root(crash_root);

        let descendant_script = r#"#!/bin/sh
printf '%s\n' '{"type":"hello","protocol":2,"service_id":"echo","requested_capabilities":[],"methods":["conformance.echo"]}'
IFS= read -r welcome
IFS= read -r request
sleep 30 &
descendant=$!
id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"type":"response","id":%s,"result":%s}\n' "$id" "$descendant"
wait
"#;
        let (descendant_root, descendant_package) = install_process_package(descendant_script, &[]);
        let factory = super::super::RuntimeFactory::installed_process(
            &descendant_package,
            "echo",
            "example.runtime",
        )
        .unwrap();
        let mut dispatcher =
            super::super::ExtensionRuntimeDispatcher::start(&factory, 3, &canceled).unwrap();
        let pid = dispatcher
            .invoke(
                "conformance.echo",
                serde_json::json!({}),
                3,
                Instant::now() + Duration::from_millis(750),
                &canceled,
            )
            .unwrap()
            .as_u64()
            .unwrap() as i32;
        dispatcher.stop(RuntimeStopReason::Requested);
        let started = Instant::now();
        loop {
            // SAFETY: signal 0 only probes process existence.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(10));
        }
        cleanup_package_root(descendant_root);
    }
}
