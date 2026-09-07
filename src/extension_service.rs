//! Supervised, out-of-process extension services.
//!
//! Services use bounded newline-delimited JSON over stdio. This module deliberately exposes no
//! renderer callbacks or terminal escape surface; inspector providers return validated
//! [`crate::inspector_document::InspectorDocument`] values.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub const EXTENSION_SERVICE_PROTOCOL: u16 = 1;
const MAX_FRAME_BYTES: usize = 256 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_millis(750);
const MAX_RESTARTS: u32 = 3;

#[derive(Debug, Clone)]
pub struct ExtensionServiceSpec {
    pub id: String,
    pub command: Vec<String>,
    pub cwd: PathBuf,
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

pub fn supervisors_for_plugins(
    plugins: &crate::app::state::InstalledPluginRegistry,
) -> std::collections::BTreeMap<String, ExtensionServiceSupervisor> {
    let mut supervisors = std::collections::BTreeMap::new();
    for plugin in plugins.values().filter(|plugin| plugin.enabled) {
        for service in &plugin.services {
            if !service
                .provides
                .contains(&crate::api::schema::PluginServiceCapability::PaneInspector)
                || !supports_current_platform(service.platforms.as_deref())
            {
                continue;
            }
            let id = format!("{}.{}", plugin.plugin_id, service.id);
            let spec = ExtensionServiceSpec {
                id: id.clone(),
                command: service.command.clone(),
                cwd: std::path::PathBuf::from(&plugin.plugin_root),
            };
            match ExtensionServiceSupervisor::new(spec) {
                Ok(supervisor) => {
                    supervisors.insert(id, supervisor);
                }
                Err(error) => {
                    tracing::warn!(plugin_id = %plugin.plugin_id, service_id = %service.id, %error, "ignoring invalid extension service")
                }
            }
        }
    }
    supervisors
}

impl crate::app::App {
    pub(crate) fn render_extension_inspector(
        &mut self,
        telemetry: &crate::api::schema::PaneTelemetry,
    ) -> Option<crate::inspector_document::InspectorDocument> {
        if self.extension_services.is_empty() {
            return None;
        }
        let context = serde_json::to_value(telemetry).ok()?;
        for (service_id, supervisor) in &mut self.extension_services {
            match supervisor.inspect(context.clone()) {
                Ok(document) => return Some(document),
                Err(error) => {
                    tracing::warn!(%service_id, %error, "extension inspector unavailable")
                }
            }
        }
        None
    }
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

    pub fn inspect(
        &mut self,
        context: serde_json::Value,
    ) -> Result<crate::inspector_document::InspectorDocument, ExtensionServiceError> {
        self.ensure_running(Instant::now())?;
        let result = self
            .runtime
            .as_mut()
            .ok_or_else(|| ExtensionServiceError::Protocol("service is not running".into()))?
            .request("inspector.render", context)
            .and_then(|value| {
                let document: crate::inspector_document::InspectorDocument =
                    serde_json::from_value(value).map_err(|error| {
                        ExtensionServiceError::Protocol(format!(
                            "invalid inspector document: {error}"
                        ))
                    })?;
                document.validate().map_err(|error| {
                    ExtensionServiceError::Protocol(format!("invalid inspector document: {error}"))
                })?;
                Ok(document)
            });
        match result {
            Ok(document) => Ok(document),
            Err(error) => {
                self.runtime = None;
                self.record_failure(&error);
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
            .env(
                crate::api::SOCKET_PATH_ENV_VAR,
                crate::api::socket_path().as_os_str(),
            )
            .env(crate::product::ENV_MARKER, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Ok(binary) = std::env::current_exe() {
            command.env(crate::product::BIN_PATH_ENV, binary);
        }
        for key in [
            "HERDR_ENV",
            "HERDR_SOCKET_PATH",
            "HERDR_CLIENT_SOCKET_PATH",
            "HERDR_SESSION",
            "HERDR_WORKSPACE_ID",
            "HERDR_TAB_ID",
            "HERDR_PANE_ID",
            "HERDR_BIN_PATH",
        ] {
            command.env_remove(key);
        }
        let mut child = command.spawn().map_err(ExtensionServiceError::Io)?;
        let stdin = child.stdin.take().ok_or_else(|| {
            ExtensionServiceError::Protocol("service stdin was not captured".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ExtensionServiceError::Protocol("service stdout was not captured".into())
        })?;
        let (frame_tx, frames) = mpsc::sync_channel(8);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let frame = read_bounded_frame(&mut reader);
                let eof = matches!(&frame, Ok(bytes) if bytes.is_empty());
                if frame_tx.send(frame).is_err() || eof {
                    break;
                }
            }
        });
        let mut runtime = Self {
            child,
            stdin,
            frames,
            next_request_id: 1,
        };
        runtime.handshake(&spec.id)?;
        Ok(runtime)
    }

    fn handshake(&mut self, expected_id: &str) -> Result<(), ExtensionServiceError> {
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
        Ok(())
    }

    fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
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
        let frame = self.recv_frame(REQUEST_TIMEOUT)?;
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
}

impl Drop for ExtensionServiceRuntime {
    fn drop(&mut self) {
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
    Exited,
    Backoff,
    RestartBudget(String),
}

impl std::fmt::Display for ExtensionServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration(message) | Self::Protocol(message) | Self::Remote(message) => {
                formatter.write_str(message)
            }
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Timeout => formatter.write_str("extension service timed out"),
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

    #[test]
    fn rejects_invalid_specs_before_spawning() {
        let spec = ExtensionServiceSpec {
            id: "not valid".into(),
            command: vec!["unused".into()],
            cwd: PathBuf::from("."),
        };
        assert!(ExtensionServiceSupervisor::new(spec).is_err());
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
    fn invalid_document_retires_service_and_enters_backoff() {
        let script = r#"
printf '%s\n' '{"type":"hello","protocol":1,"service_id":"test.invalid"}'
IFS= read -r request
id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"type":"response","id":%s,"result":{"title":"bad\\u001b","sections":[],"actions":[]}}\n' "$id"
"#;
        let spec = ExtensionServiceSpec {
            id: "test.invalid".into(),
            command: vec!["sh".into(), "-c".into(), script.into()],
            cwd: PathBuf::from("."),
        };
        let mut supervisor = ExtensionServiceSupervisor::new(spec).unwrap();
        supervisor.start().unwrap();
        assert!(supervisor.inspect(serde_json::json!({})).is_err());
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
printf '%s\n' '{"type":"hello","protocol":1,"service_id":"test.telemetry"}'
IFS= read -r request
id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"type":"response","id":%s,"result":{"title":"Processes","sections":[],"actions":[]}}\n' "$id"
"#;
        let spec = ExtensionServiceSpec {
            id: "test.telemetry".into(),
            command: vec!["sh".into(), "-c".into(), script.into()],
            cwd: PathBuf::from("."),
        };
        let mut supervisor = ExtensionServiceSupervisor::new(spec).unwrap();
        supervisor.start().unwrap();
        let document = supervisor
            .inspect(serde_json::json!({"pane_id": "p1"}))
            .unwrap();
        assert_eq!(document.title, "Processes");
        assert_eq!(supervisor.status, ExtensionServiceStatus::Running);
        supervisor.stop();
        assert_eq!(supervisor.status, ExtensionServiceStatus::Stopped);
    }
}
