//! Stable endpoint compatibility contract for client-owned shells.
//!
//! The endpoint generation is intentionally independent from the private
//! binary protocol used by same-install CLI, direct-terminal, and handoff
//! paths. Generation 1 is the compatibility floor for Local, SSH, and Cloud
//! shell endpoints and must remain available indefinitely unless retired for a
//! security reason. New JSON fields must be optional or have serde defaults;
//! new enum values need an `Unknown` fallback. Unknown named controls are
//! optional and ignored unless negotiated as part of the core.

use serde::{Deserialize, Serialize};

use super::{ClientShellSnapshot, ClientSurfaceSize, ServerMessage};

pub const ENDPOINT_PROTOCOL_GENERATION: u32 = 1;
pub const ENDPOINT_HELLO_KIND: &str = "endpoint.hello.v1";
pub const ENDPOINT_WELCOME_KIND: &str = "endpoint.welcome.v1";
pub const SNAPSHOT_CODEC_V1: &str = "shell.snapshot.v1";
pub const ENDPOINT_SNAPSHOT_KIND: &str = SNAPSHOT_CODEC_V1;
pub const SURFACE_CODEC_V1: &str = "shell.surface.v1";
pub const INPUT_CODEC_V1: &str = "shell.input.semantic.v1";
pub const BLOB_CODEC_V1: &str = "shell.blob.v1";
pub const SURFACE_INTEREST_CAPABILITY: &str = "surface_interest";
pub const PRESENTATION_EFFECTS_FENCE_CAPABILITY: &str = "presentation_effects_fence";
pub const PRESENTATION_EFFECTS_SYNC_KIND: &str = "endpoint.presentation.sync.v1";
pub const PRESENTATION_EFFECTS_READY_KIND: &str = "endpoint.presentation.ready.v1";
pub const HEALTH_CHECK_CAPABILITY: &str = "health_check";
pub const HEALTH_PING_KIND: &str = "endpoint.health.ping.v1";
pub const HEALTH_PONG_KIND: &str = "endpoint.health.pong.v1";
pub const AGENT_VIEW_PROJECTION_CAPABILITY: &str = "agent_view_projection";
pub const AGENT_VIEW_PROJECTION_KIND: &str = "endpoint.agent-view.v1";
pub const SESSION_STRUCTURE_CAPABILITY: &str = "session_structure_v1";
pub const SESSION_STRUCTURE_KIND: &str = "session.structure.v1";
pub const HOST_DOCUMENTS_KIND: &str = "host.documents.v1";
pub const PANE_VIEWPORT_CAPABILITY: &str = "pane_viewport_v1";
pub const PANE_VIEW_CAPABILITY: &str = "pane_view_v1";
pub const PANE_VIEWPORT_KIND: &str = "pane.viewport.v1";
pub const PANE_VIEW_REQUEST_KIND: &str = "pane.view.request.v1";
pub const PANE_VIEW_CANCEL_KIND: &str = "pane.view.cancel.v1";
pub const PANE_VIEW_RESULT_KIND: &str = "pane.view.result.v1";
pub const PANE_VIEW_ACTION_KIND: &str = "pane.view.action.v1";
pub const PANE_VIEW_ACTION_RESULT_KIND: &str = "pane.view.action-result.v1";
pub const PANE_VIEW_CONFIRM_KIND: &str = "pane.view.confirm.v1";
pub const TERMINAL_VIEWS_CAPABILITY: &str = "terminal_views_v1";
pub const TERMINAL_VIEWS_INTEREST_KIND: &str = "terminal.views.interest.v1";
pub const TERMINAL_VIEWS_SURFACE_KIND: &str = "terminal.views.surface.v1";
pub const MAX_TERMINAL_VIEWS: usize = 16;
pub const MAX_TERMINAL_VIEW_CELLS: usize = 32_768;
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointClientHello {
    pub generation: u32,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    pub surface_size: ClientSurfaceSize,
    pub pixel_mouse: bool,
    pub direct_graphics: bool,
    pub endpoint_keybindings: bool,
    pub mouse_capture: bool,
    #[serde(default = "default_true")]
    pub surface_active: bool,
    #[serde(default)]
    pub snapshot_codecs: Vec<String>,
    #[serde(default)]
    pub surface_codecs: Vec<String>,
    #[serde(default)]
    pub input_codecs: Vec<String>,
    #[serde(default)]
    pub blob_codecs: Vec<String>,
    /// Optional controls the client understands; absent for generation-1 clients.
    #[serde(default)]
    pub control_kinds: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostDocumentsProjectionV1 {
    pub boot_id: String,
    pub revision: u64,
    #[serde(default)]
    pub documents: Vec<HostDocumentProjectionEntryV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostDocumentProjectionEntryV1 {
    pub provider_priority: i32,
    pub document: crate::extension_contract::HostDocumentV1,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub confirmation_tokens: std::collections::BTreeMap<String, String>,
}

impl HostDocumentsProjectionV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.boot_id.is_empty() || self.boot_id.len() > 128 || self.revision == 0 {
            return Err("invalid host document projection identity".into());
        }
        let max_documents =
            crate::admission::MAX_SESSION_PANES * crate::extension_contract::MAX_DOCUMENT_PROVIDERS;
        if self.documents.len() > max_documents {
            return Err("host document projection has too many entries".into());
        }
        let mut identities = std::collections::BTreeSet::new();
        for entry in &self.documents {
            entry
                .document
                .validate()
                .map_err(|error| error.to_string())?;
            if !identities.insert((
                entry.document.namespace.clone(),
                entry.document.target.clone(),
            )) {
                return Err("host document projection identities must be unique".into());
            }
            for (action_id, token) in &entry.confirmation_tokens {
                if token.is_empty()
                    || token.len() > 256
                    || !token.bytes().all(|byte| byte.is_ascii_alphanumeric())
                    || !entry
                        .document
                        .actions
                        .iter()
                        .any(|action| action.id == *action_id)
                {
                    return Err("invalid host document confirmation token".into());
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointHandshakeError {
    pub code: String,
    pub message: String,
}

/// Optional companion to a V1 snapshot. The view payload is intentionally opaque to this
/// stable envelope so clients can ignore view language additions they do not understand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointAgentViewProjection {
    pub boot_id: String,
    pub revision: u64,
    #[serde(default)]
    pub view: Option<serde_json::Value>,
}

/// Optional, revision-bound runtime structure projected alongside the frozen shell snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellStructureProjection {
    pub session_id: String,
    pub boot_id: String,
    pub revision: u64,
    pub collections: Vec<ClientShellCollection>,
    pub delegations: Vec<ClientShellDelegation>,
    /// Additive repository-space data. Older generation-1 servers omit this field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repositories: Vec<ClientShellRepository>,
    /// Authoritative ordering of repository and standalone-workspace Spaces.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub space_order: Vec<ClientShellSpaceRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellRepository {
    pub repository_id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_url: Option<String>,
    /// Durable extension-scope repository identity shared by linked worktrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_identity: Option<String>,
    pub checkouts: Vec<ClientShellRepositoryCheckout>,
    pub last_focused_workspace_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellRepositoryCheckout {
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout_path: Option<String>,
    pub kind: ClientShellCheckoutKind,
    pub upstream: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_web_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detached_head: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientShellCheckoutKind {
    Primary,
    Linked,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellSpaceRef {
    pub kind: ClientShellSpaceKind,
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientShellSpaceKind {
    Repository,
    StandaloneWorkspace,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellCollection {
    pub collection_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub label: Option<String>,
    pub selected_pane_id: Option<String>,
    pub members: Vec<ClientShellCollectionMember>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellCollectionMember {
    pub pane_id: String,
    pub index: usize,
    pub archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientShellDelegation {
    pub delegation_id: String,
    pub pane_id: Option<String>,
    pub parent_id: Option<String>,
    pub purpose: Option<String>,
    pub depth: usize,
    pub external_parent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientTerminalViewsInterest {
    pub boot_id: String,
    pub projection_revision: u64,
    pub views: Vec<ClientTerminalViewInterest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientTerminalViewInterest {
    pub pane_id: String,
    pub size: ClientSurfaceSize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerTerminalViewsSurface {
    pub boot_id: String,
    pub projection_revision: u64,
    /// Complete interest replacement versus a retained update for only the listed views.
    #[serde(default = "default_true")]
    pub replace: bool,
    pub views: Vec<ServerTerminalViewSurface>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerTerminalViewSurface {
    pub pane_id: String,
    pub frame: crate::protocol::FrameData,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scroll: Option<crate::protocol::PaneSurfaceScrollMetrics>,
    pub mouse_reporting: bool,
    pub sgr_pixel_mouse: bool,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientPaneViewportRequest {
    pub boot_id: String,
    pub projection_revision: u64,
    pub pane_id: String,
    /// Effective PTY viewport requested by this connection; `None` restores layout geometry.
    pub size: Option<ClientSurfaceSize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientPaneViewRequest {
    pub boot_id: String,
    pub projection_revision: u64,
    pub request_id: String,
    pub pane_id: String,
    pub contribution_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientPaneViewCancel {
    pub boot_id: String,
    pub request_id: String,
    pub pane_id: String,
    pub contribution_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerPaneViewResult {
    pub boot_id: String,
    pub projection_revision: u64,
    pub request_id: String,
    pub pane_id: String,
    pub contribution_revision: u64,
    pub owner_service_id: String,
    pub service_generation: u64,
    pub document: Option<crate::view_document::ViewDocument>,
    #[serde(default)]
    pub actions: Vec<crate::view_document::ViewAction>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientPaneViewActionRequest {
    pub boot_id: String,
    pub projection_revision: u64,
    pub request_id: String,
    pub pane_id: String,
    pub contribution_revision: u64,
    pub owner_service_id: String,
    pub service_generation: u64,
    pub action_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerPaneViewActionResult {
    pub request_id: String,
    pub pane_id: String,
    pub contribution_revision: u64,
    pub action_id: String,
    pub confirmation_token: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientPaneViewConfirm {
    pub boot_id: String,
    pub confirmation_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointServerWelcome {
    pub generation: u32,
    pub server_version: String,
    pub snapshot_codec: String,
    pub surface_codec: String,
    pub input_codec: String,
    pub blob_codec: String,
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<EndpointHandshakeError>,
}

pub fn snapshot_message(snapshot: &ClientShellSnapshot) -> serde_json::Result<ServerMessage> {
    Ok(ServerMessage::EndpointControl {
        kind: ENDPOINT_SNAPSHOT_KIND.into(),
        data: serde_json::to_string(snapshot)?,
    })
}

pub fn agent_view_projection_message(
    boot_id: &str,
    revision: u64,
    view: Option<&crate::api::schema::AgentViewSetParams>,
) -> serde_json::Result<ServerMessage> {
    let projection = EndpointAgentViewProjection {
        boot_id: boot_id.to_owned(),
        revision,
        view: view.map(serde_json::to_value).transpose()?,
    };
    Ok(ServerMessage::EndpointControl {
        kind: AGENT_VIEW_PROJECTION_KIND.into(),
        data: serde_json::to_string(&projection)?,
    })
}

pub fn structure_message(
    projection: &ClientShellStructureProjection,
) -> serde_json::Result<ServerMessage> {
    Ok(ServerMessage::EndpointControl {
        kind: SESSION_STRUCTURE_KIND.into(),
        data: serde_json::to_string(projection)?,
    })
}

pub fn host_documents_message(
    projection: &HostDocumentsProjectionV1,
) -> serde_json::Result<ServerMessage> {
    Ok(ServerMessage::EndpointControl {
        kind: HOST_DOCUMENTS_KIND.into(),
        data: serde_json::to_string(projection)?,
    })
}

impl EndpointClientHello {
    pub fn supports_required_codecs(&self) -> bool {
        self.snapshot_codecs
            .iter()
            .any(|codec| codec == SNAPSHOT_CODEC_V1)
            && self
                .surface_codecs
                .iter()
                .any(|codec| codec == SURFACE_CODEC_V1)
            && self
                .input_codecs
                .iter()
                .any(|codec| codec == INPUT_CODEC_V1)
            && self.blob_codecs.iter().any(|codec| codec == BLOB_CODEC_V1)
    }
}

impl EndpointServerWelcome {
    pub fn compatible(methods: Vec<String>) -> Self {
        Self {
            generation: ENDPOINT_PROTOCOL_GENERATION,
            server_version: crate::build_info::version(),
            snapshot_codec: SNAPSHOT_CODEC_V1.into(),
            surface_codec: SURFACE_CODEC_V1.into(),
            input_codec: INPUT_CODEC_V1.into(),
            blob_codec: BLOB_CODEC_V1.into(),
            methods,
            capabilities: vec![
                SURFACE_INTEREST_CAPABILITY.into(),
                PRESENTATION_EFFECTS_FENCE_CAPABILITY.into(),
                HEALTH_CHECK_CAPABILITY.into(),
                AGENT_VIEW_PROJECTION_CAPABILITY.into(),
            ],
            error: None,
        }
    }

    pub fn incompatible(code: &str, message: impl Into<String>) -> Self {
        Self {
            generation: ENDPOINT_PROTOCOL_GENERATION,
            server_version: crate::build_info::version(),
            snapshot_codec: SNAPSHOT_CODEC_V1.into(),
            surface_codec: SURFACE_CODEC_V1.into(),
            input_codec: INPUT_CODEC_V1.into(),
            blob_codec: BLOB_CODEC_V1.into(),
            methods: Vec::new(),
            capabilities: Vec::new(),
            error: Some(EndpointHandshakeError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> EndpointClientHello {
        EndpointClientHello {
            generation: ENDPOINT_PROTOCOL_GENERATION,
            cell_width_px: 8,
            cell_height_px: 16,
            surface_size: ClientSurfaceSize { cols: 80, rows: 24 },
            pixel_mouse: true,
            direct_graphics: false,
            endpoint_keybindings: false,
            mouse_capture: true,
            surface_active: true,
            snapshot_codecs: vec![SNAPSHOT_CODEC_V1.into()],
            surface_codecs: vec![SURFACE_CODEC_V1.into()],
            input_codecs: vec![INPUT_CODEC_V1.into()],
            blob_codecs: vec![BLOB_CODEC_V1.into()],
            control_kinds: Vec::new(),
        }
    }

    fn snapshot() -> ClientShellSnapshot {
        ClientShellSnapshot {
            boot_id: "boot".into(),
            revision: 1,
            config_diagnostic: None,
            product_announcement: None,
            update_available: None,
            update_install_command: "herdr update".into(),
            server_keybindings_toml: None,
            latest_release_notes_available: false,
            integration_updates_available: false,
            worktree_directory: String::new(),
            release_notes: None,
            focused_workspace_id: None,
            focused_tab_id: None,
            focused_pane_id: None,
            tab_bar_right: Vec::new(),
            tab_bar_right_separator: String::new(),
            agent_view_label: None,
            agent_order: Vec::new(),
            workspaces: Vec::new(),
            tabs: Vec::new(),
            panes: Vec::new(),
            agents: Vec::new(),
            commands: Vec::new(),
        }
    }

    #[test]
    fn hello_ignores_future_named_fields() {
        let mut value = serde_json::to_value(hello()).unwrap();
        value["future_feature"] = serde_json::json!({"enabled": true});
        let decoded: EndpointClientHello = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, hello());
    }

    #[test]
    fn frozen_generation_one_handshake_decodes() {
        let hello: EndpointClientHello = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/endpoint-hello-v1.json"
        )))
        .unwrap();
        assert!(hello.supports_required_codecs());

        let welcome: EndpointServerWelcome = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/endpoint-welcome-v1.json"
        )))
        .unwrap();
        assert_eq!(welcome.generation, ENDPOINT_PROTOCOL_GENERATION);
        assert_eq!(welcome.snapshot_codec, SNAPSHOT_CODEC_V1);
        assert_eq!(welcome.surface_codec, SURFACE_CODEC_V1);
        assert_eq!(welcome.input_codec, INPUT_CODEC_V1);
        assert_eq!(welcome.blob_codec, BLOB_CODEC_V1);
        assert!(welcome.capabilities.is_empty());
    }

    #[test]
    fn frozen_generation_one_snapshot_decodes() {
        let snapshot: ClientShellSnapshot = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/endpoint-snapshot-v1.json"
        )))
        .unwrap();
        assert_eq!(snapshot.boot_id, "boot-v1");
        assert_eq!(
            snapshot.workspaces[0].agent_status,
            crate::api::schema::AgentStatus::Unknown
        );
    }

    #[test]
    fn snapshot_message_uses_named_json_control() {
        let snapshot = snapshot();
        let ServerMessage::EndpointControl { kind, data } = snapshot_message(&snapshot).unwrap()
        else {
            panic!("snapshot should use endpoint control");
        };
        assert_eq!(kind, ENDPOINT_SNAPSHOT_KIND);
        let decoded: ClientShellSnapshot = serde_json::from_str(&data).unwrap();
        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn agent_view_projection_is_an_optional_revision_bound_control() {
        let view = crate::api::schema::AgentViewSetParams {
            source: "example.views".into(),
            label: Some("focus".into()),
            filter: None,
            sort: Vec::new(),
        };
        let ServerMessage::EndpointControl { kind, data } =
            agent_view_projection_message("boot", 7, Some(&view)).unwrap()
        else {
            panic!("projection should use endpoint control");
        };
        assert_eq!(kind, AGENT_VIEW_PROJECTION_KIND);
        let projection: EndpointAgentViewProjection = serde_json::from_str(&data).unwrap();
        assert_eq!(projection.boot_id, "boot");
        assert_eq!(projection.revision, 7);
        assert_eq!(
            projection
                .view
                .map(serde_json::from_value)
                .transpose()
                .unwrap(),
            Some(view)
        );
    }

    #[test]
    fn snapshot_json_tolerates_future_fields_and_command_actions() {
        let mut snapshot = match snapshot_message(&snapshot()).unwrap() {
            ServerMessage::EndpointControl { data, .. } => {
                serde_json::from_str::<serde_json::Value>(&data).unwrap()
            }
            _ => unreachable!(),
        };
        snapshot["future_projection"] = serde_json::json!({"enabled": true});
        snapshot["commands"] = serde_json::json!([{
            "command_id": "future",
            "binding_label": "x",
            "binding_labels": ["x"],
            "action": "FutureAction",
            "description": null
        }]);

        let decoded: ClientShellSnapshot = serde_json::from_value(snapshot).unwrap();
        assert_eq!(
            decoded.commands[0].action,
            crate::protocol::ClientShellCommandAction::Unknown
        );
    }

    #[test]
    fn optional_structure_control_round_trips_revision_and_identity() {
        let projection = ClientShellStructureProjection {
            session_id: "alpha".into(),
            boot_id: "boot-7".into(),
            revision: 42,
            collections: vec![ClientShellCollection {
                collection_id: "collection_3".into(),
                workspace_id: "w1".into(),
                tab_id: "w1:t1".into(),
                label: Some("review".into()),
                selected_pane_id: Some("w1:p2".into()),
                members: vec![ClientShellCollectionMember {
                    pane_id: "w1:p2".into(),
                    index: 0,
                    archived: false,
                }],
            }],
            delegations: vec![ClientShellDelegation {
                delegation_id: "d1".into(),
                pane_id: Some("w1:p2".into()),
                parent_id: None,
                purpose: Some("test".into()),
                depth: 0,
                external_parent_id: None,
            }],
            repositories: vec![ClientShellRepository {
                repository_id: "repo_1".into(),
                label: "vqro".into(),
                repository_path: Some("/repo".into()),
                web_url: Some("https://github.com/example/vqro".into()),
                durable_identity: Some("https://github.com/example/vqro".into()),
                checkouts: vec![ClientShellRepositoryCheckout {
                    workspace_id: "w1".into(),
                    checkout_path: Some("/repo".into()),
                    kind: ClientShellCheckoutKind::Primary,
                    upstream: Some("origin/main".into()),
                    upstream_web_url: Some("https://github.com/example/vqro/tree/main".into()),
                    detached_head: None,
                }],
                last_focused_workspace_id: Some("w1".into()),
            }],
            space_order: vec![ClientShellSpaceRef {
                kind: ClientShellSpaceKind::Repository,
                id: "repo_1".into(),
            }],
        };
        let ServerMessage::EndpointControl { kind, data } =
            structure_message(&projection).expect("structure control")
        else {
            panic!("endpoint control");
        };
        assert_eq!(kind, SESSION_STRUCTURE_KIND);
        assert_eq!(
            serde_json::from_str::<ClientShellStructureProjection>(&data).unwrap(),
            projection
        );

        let mut legacy = serde_json::to_value(&projection).unwrap();
        let repository = legacy["repositories"][0].as_object_mut().unwrap();
        repository.remove("repository_path");
        repository.remove("web_url");
        repository["checkouts"][0]
            .as_object_mut()
            .unwrap()
            .remove("upstream_web_url");
        repository["checkouts"][0]
            .as_object_mut()
            .unwrap()
            .remove("checkout_path");
        let legacy: ClientShellStructureProjection = serde_json::from_value(legacy).unwrap();
        assert!(legacy.repositories[0].repository_path.is_none());
        assert!(legacy.repositories[0].web_url.is_none());
        assert!(legacy.repositories[0].checkouts[0]
            .upstream_web_url
            .is_none());
        assert!(legacy.repositories[0].checkouts[0].checkout_path.is_none());
    }

    #[test]
    fn legacy_hello_defaults_to_an_active_surface() {
        let mut value = serde_json::to_value(hello()).unwrap();
        value.as_object_mut().unwrap().remove("surface_active");
        let decoded: EndpointClientHello = serde_json::from_value(value).unwrap();
        assert!(decoded.surface_active);
    }

    #[test]
    fn compatible_server_advertises_endpoint_lifecycle_capabilities() {
        let welcome = EndpointServerWelcome::compatible(Vec::new());
        assert_eq!(
            welcome.capabilities,
            vec![
                SURFACE_INTEREST_CAPABILITY.to_string(),
                PRESENTATION_EFFECTS_FENCE_CAPABILITY.to_string(),
                HEALTH_CHECK_CAPABILITY.to_string(),
                AGENT_VIEW_PROJECTION_CAPABILITY.to_string(),
            ]
        );
    }

    #[test]
    fn required_codecs_are_explicit() {
        let mut value = hello();
        assert!(value.supports_required_codecs());
        value.snapshot_codecs.clear();
        assert!(!value.supports_required_codecs());

        let mut value = hello();
        value.surface_codecs.clear();
        assert!(!value.supports_required_codecs());

        let mut value = hello();
        value.input_codecs.clear();
        assert!(!value.supports_required_codecs());

        let mut value = hello();
        value.blob_codecs.clear();
        assert!(!value.supports_required_codecs());
    }

    #[test]
    fn welcome_ignores_future_named_fields() {
        let welcome = EndpointServerWelcome::compatible(vec!["pane.close".into()]);
        let mut value = serde_json::to_value(&welcome).unwrap();
        value["future_service"] = serde_json::json!("v2");
        let decoded: EndpointServerWelcome = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, welcome);
    }
}
