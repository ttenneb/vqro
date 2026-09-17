pub mod client;
mod event_hub;
pub mod schema;
mod server;
mod status;
mod subscriptions;
mod wait;

pub use event_hub::EventHub;
pub use server::ServerHandle;
pub(crate) use server::{api_method_name, start_server_with_stop_control};
pub use status::{read_runtime_status_at, RuntimeStatus};

use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::api::schema::{Method, Request};

pub const SOCKET_PATH_ENV_VAR: &str = crate::product::SOCKET_PATH_ENV;

pub(crate) fn request_changes_ui(request: &Request) -> bool {
    matches!(
        &request.method,
        Method::ServerReloadConfig(_)
            | Method::ServerReloadAgentManifests(_)
            | Method::NotificationShow(_)
            | Method::ProductAnnouncementDismiss(_)
            | Method::ReleaseNotesDismiss(_)
            | Method::CommandInvoke(_)
            | Method::WorkspaceCreate(_)
            | Method::WorkspaceFocus(_)
            | Method::WorkspaceRename(_)
            | Method::WorkspaceMove(_)
            | Method::WorkspaceMoveBlock(_)
            | Method::WorkspaceReportMetadata(_)
            | Method::WorkspaceClose(_)
            | Method::WorktreeCreate(_)
            | Method::WorktreeOpen(_)
            | Method::WorktreeRemove(_)
            | Method::TabCreate(_)
            | Method::TabFocus(_)
            | Method::TabRename(_)
            | Method::TabMove(_)
            | Method::TabClose(_)
            | Method::LayoutApply(_)
            | Method::LayoutSetSplitRatio(_)
            | Method::AgentRename(_)
            | Method::AgentViewSet(_)
            | Method::AgentViewClear(_)
            | Method::AgentFocus(_)
            | Method::AgentStart(_)
            | Method::AgentPrompt(_)
            | Method::AgentSendKeys(_)
            | Method::CollectionCreate(_)
            | Method::CollectionAdd(_)
            | Method::CollectionPromote(_)
            | Method::CollectionSelect(_)
            | Method::CollectionReorder(_)
            | Method::CollectionArchive(_)
            | Method::CollectionRestore(_)
            | Method::CollectionClose(_)
            | Method::DelegationCreate(_)
            | Method::DelegationReparent(_)
            | Method::DelegationReorder(_)
            | Method::PaneSplit(_)
            | Method::PaneSwap(_)
            | Method::PaneMove(_)
            | Method::PaneZoom(_)
            | Method::PaneFocusDirection(_)
            | Method::PaneResize(_)
            | Method::PaneScroll(_)
            | Method::PaneEditScrollback(_)
            | Method::PaneFocus(_)
            | Method::PaneInputSet(_)
            | Method::PaneRename(_)
            | Method::PaneGraphicsSet(_)
            | Method::PaneGraphicsClear(_)
            | Method::PaneGraphicsStream(_)
            | Method::PaneGraphicsStreamSet(_)
            | Method::PaneGraphicsStreamDirect(_)
            | Method::PaneGraphicsStreamOpen(_)
            | Method::PaneGraphicsStreamClose(_)
            | Method::PaneReportAgent(_)
            | Method::PaneReportAgentSession(_)
            | Method::PaneReportMetadata(_)
            | Method::PaneClearAgentAuthority(_)
            | Method::PaneReleaseAgent(_)
            | Method::PaneClose(_)
            | Method::PopupClose(_)
            | Method::PluginUnlink(_)
            | Method::PluginDisable(_)
            | Method::PluginActionInvoke(_)
            | Method::PluginPaneOpen(_)
            | Method::PluginPaneFocus(_)
            | Method::PluginPaneClose(_)
            | Method::ExtensionInstall(_)
            | Method::ExtensionEnable(_)
            | Method::ExtensionRemove(_)
            | Method::ExtensionDataDelete(_)
            | Method::ExtensionReorder(_)
            | Method::ExtensionRepositoryOverrideSet(_)
            | Method::ExtensionUpdate(_)
            | Method::ExtensionRollback(_)
            | Method::ExtensionPackageInstall(_)
            | Method::ExtensionPackageUpdate(_)
            | Method::ExtensionPackageEnable(_)
            | Method::ExtensionPackageDisable(_)
            | Method::ExtensionPackageRemove(_)
            | Method::ExtensionPackageRollback(_)
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiOrigin {
    LocalSocket,
    NativeEndpoint { client_id: u64, boot_id: String },
    LegacyEndpointV1 { client_id: u64, boot_id: String },
}

pub struct ApiRequestMessage {
    pub origin: ApiOrigin,
    pub request: Request,
    pub respond_to: std::sync::mpsc::Sender<String>,
    pub response_write_complete: Option<std::sync::mpsc::Receiver<()>>,
    pub stream_active: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

pub type ApiRequestSender = mpsc::UnboundedSender<ApiRequestMessage>;

pub fn socket_path() -> PathBuf {
    crate::session::active_api_socket_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_layout_mutations_request_a_client_redraw() {
        let request = Request {
            id: "test".into(),
            method: Method::CollectionAdd(crate::api::schema::CollectionAddParams {
                collection_id: "collection_1".into(),
                pane_id: "w1:p1".into(),
            }),
        };
        assert!(request_changes_ui(&request));

        let query = Request {
            id: "test".into(),
            method: Method::CollectionList(Default::default()),
        };
        assert!(!request_changes_ui(&query));
    }

    #[test]
    fn every_extension_mutation_requests_a_client_redraw() {
        let cases = [
            r#"{"id":"1","method":"extension.install","params":{"repository":"r","confirmed":true}}"#,
            r#"{"id":"1","method":"extension.enable","params":{"repository":"r","enabled":false}}"#,
            r#"{"id":"1","method":"extension.remove","params":{"repository":"r","confirmed":true}}"#,
            r#"{"id":"1","method":"extension.data.delete","params":{"repository":"r","confirmed":true}}"#,
            r#"{"id":"1","method":"extension.reorder","params":{"repository":"r","index":0}}"#,
            r#"{"id":"1","method":"extension.repository_override.set","params":{"repository":"r","repository_id":"repo","enabled":false}}"#,
            r#"{"id":"1","method":"extension.update","params":{"repository":"r","version":"2","approved":true}}"#,
            r#"{"id":"1","method":"extension.rollback","params":{"repository":"r","confirmed":true}}"#,
        ];
        for encoded in cases {
            let request: Request = serde_json::from_str(encoded).unwrap();
            assert!(request_changes_ui(&request), "{encoded}");
        }
    }
}
