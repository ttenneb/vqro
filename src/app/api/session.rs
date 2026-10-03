use crate::api::schema::{ResponseResult, SessionSnapshot};
use crate::app::App;

use super::responses::encode_success;

impl App {
    pub(super) fn handle_session_snapshot(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::SessionSnapshot {
                snapshot: Box::new(self.session_snapshot()),
            },
        )
    }

    pub(crate) fn session_snapshot(&self) -> SessionSnapshot {
        self.session_snapshot_with_structure(true)
    }

    /// Build the source used by the frozen endpoint snapshot v1 projection.
    /// Collection and delegation data are available through the API snapshot,
    /// but must not be folded into the immutable v1 endpoint codec.
    pub(crate) fn session_snapshot_v1_compat(&self) -> SessionSnapshot {
        self.session_snapshot_with_structure(false)
    }

    fn session_snapshot_with_structure(&self, include_structure: bool) -> SessionSnapshot {
        let focused_workspace_id = self
            .state
            .active
            .map(|ws_idx| self.public_workspace_id(ws_idx));
        let focused_tab_id = self.state.active.and_then(|ws_idx| {
            let ws = self.state.workspaces.get(ws_idx)?;
            self.public_tab_id(ws_idx, ws.active_tab)
        });
        let focused_pane_id = self.state.active.and_then(|ws_idx| {
            let ws = self.state.workspaces.get(ws_idx)?;
            self.public_pane_id(ws_idx, ws.focused_pane_id()?)
        });

        let mut workspaces = Vec::new();
        let mut tabs = Vec::new();
        let mut layouts = Vec::new();
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            workspaces.push(self.workspace_info(ws_idx));
            for tab_idx in 0..ws.tabs.len() {
                if let Some(tab) = self.tab_info(ws_idx, tab_idx) {
                    tabs.push(tab);
                }
                if let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) {
                    layouts.push(layout);
                }
            }
        }

        let repositories = self
            .state
            .repositories
            .iter()
            .map(|repository| crate::api::schema::RepositoryInfo {
                repository_id: repository.id.clone(),
                label: repository.display_label().to_owned(),
                git_common_dir: repository.git_common_dir.display().to_string(),
                checkout_workspace_ids: repository.checkout_workspace_ids.clone(),
                last_focused_workspace_id: repository.last_focused_workspace_id.clone(),
                focused: focused_workspace_id
                    .as_ref()
                    .is_some_and(|focused| repository.checkout_workspace_ids.contains(focused)),
            })
            .collect();
        let space_order = self
            .host_document_caches
            .get(&(
                crate::spaces_extension_contract::SPACES_NAMESPACE.into(),
                crate::spaces_extension_contract::SPACES_SCOPE_ID.into(),
            ))
            .and_then(crate::host_document_cache::HostDocumentCache::current)
            .into_iter()
            .flat_map(|document| document.items.iter())
            .filter(|item| item.parent_id.is_none())
            .filter_map(|item| {
                item.id
                    .strip_prefix("repository:")
                    .map(|id| crate::api::schema::SpaceInfo::Repository(id.into()))
                    .or_else(|| {
                        item.id
                            .strip_prefix("workspace:")
                            .map(|id| crate::api::schema::SpaceInfo::StandaloneWorkspace(id.into()))
                    })
            })
            .collect();

        SessionSnapshot {
            version: crate::build_info::version(),
            protocol: crate::protocol::PROTOCOL_VERSION,
            focused_workspace_id,
            focused_tab_id,
            focused_pane_id,
            repositories,
            space_order,
            workspaces,
            tabs,
            panes: self.collect_panes_for_workspace(None).unwrap_or_default(),
            layouts,
            // Runtime facts must remain available to any admitted document provider/client;
            // disabling a presentation extension revokes its document, not these identities.
            agents: self.collect_agent_infos(),
            collections: if include_structure && self.collections_package_runtime_available() {
                self.collection_infos(None, None)
            } else {
                Vec::new()
            },
            delegations: if include_structure {
                self.delegation_tree()
            } else {
                Vec::new()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::schema::{EmptyParams, Method, ResponseResult, SuccessResponse};
    use crate::layout::LayoutLeaf;
    use crate::{config::Config, workspace::Workspace};

    fn app_with_two_tabs() -> crate::app::App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("snapshot");
        workspace.test_add_tab(None);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app
    }

    #[test]
    fn session_snapshot_projects_optional_repository_identity_and_order() {
        let mut app = app_with_two_tabs();
        let workspace_id = app.state.workspaces[0].id.clone();
        app.state.workspaces[0].identity_cwd = "/repos/app".into();
        app.state.workspaces[0].cached_git_space = Some(crate::workspace::GitSpaceMetadata {
            key: "/repos/app/.git".into(),
            checkout_key: "/repos/app".into(),
            repo_name: "app".into(),
            repo_root: "/repos/app".into(),
            is_linked_worktree: false,
        });
        app.state.reconcile_repositories();
        let repository_id = app.state.repositories[0].id.clone();
        app.host_document_caches
            .entry((
                crate::spaces_extension_contract::SPACES_NAMESPACE.into(),
                crate::spaces_extension_contract::SPACES_SCOPE_ID.into(),
            ))
            .or_default()
            .publish_for_namespace(
                crate::spaces_extension_contract::SPACES_NAMESPACE,
                1,
                100,
                &std::collections::HashSet::new(),
                crate::extension_contract::HostDocumentV1 {
                    contract: crate::extension_contract::HOST_DOCUMENT_CONTRACT_V1.into(),
                    namespace: crate::spaces_extension_contract::SPACES_NAMESPACE.into(),
                    generation: 1,
                    revision: 1,
                    surface: crate::extension_contract::HostSurface::Navigation,
                    target: crate::extension_contract::HostTarget::Session,
                    items: vec![
                        crate::extension_contract::HostDocumentItem {
                            id: format!("repository:{repository_id}"),
                            parent_id: None,
                            role: crate::extension_contract::HostItemRole::Group,
                            label: "app".into(),
                            detail: Some("1 workspace".into()),
                            terminal: None,
                            action_ids: Vec::new(),
                            flags: crate::extension_contract::HostDocumentItemFlags {
                                expanded: true,
                                ..Default::default()
                            },
                            view: None,
                        },
                        crate::extension_contract::HostDocumentItem {
                            id: format!("workspace:{workspace_id}"),
                            parent_id: Some(format!("repository:{repository_id}")),
                            role: crate::extension_contract::HostItemRole::Section,
                            label: "snapshot".into(),
                            detail: None,
                            terminal: None,
                            action_ids: Vec::new(),
                            flags: crate::extension_contract::HostDocumentItemFlags::default(),
                            view: None,
                        },
                    ],
                    actions: Vec::new(),
                    required_capabilities: Vec::new(),
                },
            )
            .unwrap();

        let snapshot = app.session_snapshot();

        assert_eq!(snapshot.repositories.len(), 1);
        assert_eq!(
            snapshot.repositories[0].checkout_workspace_ids,
            vec![workspace_id.clone()]
        );
        assert_eq!(
            snapshot.space_order,
            vec![crate::api::schema::SpaceInfo::Repository(
                snapshot.repositories[0].repository_id.clone()
            )]
        );
        assert_eq!(
            snapshot.workspaces[0].repository_id,
            Some(snapshot.repositories[0].repository_id.clone())
        );
        assert_eq!(
            snapshot.workspaces[0]
                .checkout
                .as_ref()
                .map(|checkout| checkout.workspace_id.as_str()),
            Some(workspace_id.as_str())
        );
    }

    #[test]
    fn session_snapshot_hides_unavailable_collections_and_projects_delegation_preorder() {
        let mut app = app_with_two_tabs();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let member = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        let collection = app.state.workspaces[0].tabs[0]
            .create_collection_near(
                LayoutLeaf::Pane(root),
                ratatui::layout::Direction::Vertical,
                0.5,
                Some("reviewers".into()),
            )
            .expect("create collection");
        app.state.workspaces[0].tabs[0]
            .collect_tiled_pane(member, collection)
            .expect("collect member");
        let root_delegation = app
            .state
            .delegations
            .create(Some(root), None, Some("coordinate".into()))
            .expect("create root delegation");
        app.state
            .delegations
            .create(Some(member), Some(root_delegation), Some("review".into()))
            .expect("create child delegation");

        let snapshot = app.session_snapshot();

        assert!(snapshot.collections.is_empty());
        assert_eq!(snapshot.delegations.len(), 2);
        assert_eq!(snapshot.delegations[0].depth, 0);
        assert_eq!(snapshot.delegations[1].depth, 1);
        assert_eq!(
            snapshot.delegations[1].delegation.parent_id,
            Some(root_delegation.to_string())
        );

        let endpoint_v1_source = app.session_snapshot_v1_compat();
        assert!(endpoint_v1_source.collections.is_empty());
        assert!(endpoint_v1_source.delegations.is_empty());
    }

    #[test]
    fn neutral_snapshot_facts_survive_bundled_presentation_disablement() {
        let mut app = app_with_two_tabs();
        app.state.ensure_test_terminals();
        let pane = app.state.workspaces[0].focused_pane_id().unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(pane)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(
                Some(crate::detect::Agent::Pi),
                crate::detect::AgentState::Idle,
            );
        app.state
            .delegations
            .create(Some(pane), None, None)
            .unwrap();
        let before = app.session_snapshot();
        assert!(!before.agents.is_empty());
        app.disabled_extensions.insert("vqro.agent-status".into());
        app.disabled_extensions.insert("vqro.delegation".into());
        let after = app.session_snapshot();
        assert_eq!(after.agents, before.agents);
        assert_eq!(after.delegations, before.delegations);
        // Do not widen the gen-1 base snapshot's optional structure projection.
        assert!(app.session_snapshot_v1_compat().delegations.is_empty());
    }

    #[test]
    fn session_snapshot_space_order_requires_package_document_authority() {
        let mut app = app_with_two_tabs();
        let workspace_id = app.state.workspaces[0].id.clone();
        app.state
            .space_order
            .push(crate::repository::SpaceRef::StandaloneWorkspace(
                workspace_id.clone(),
            ));
        assert!(app.session_snapshot().space_order.is_empty());
        app.host_document_caches
            .entry((
                crate::spaces_extension_contract::SPACES_NAMESPACE.into(),
                crate::spaces_extension_contract::SPACES_SCOPE_ID.into(),
            ))
            .or_default()
            .publish_for_namespace(
                crate::spaces_extension_contract::SPACES_NAMESPACE,
                1,
                100,
                &std::collections::HashSet::new(),
                crate::extension_contract::HostDocumentV1 {
                    contract: crate::extension_contract::HOST_DOCUMENT_CONTRACT_V1.into(),
                    namespace: crate::spaces_extension_contract::SPACES_NAMESPACE.into(),
                    generation: 1,
                    revision: 1,
                    surface: crate::extension_contract::HostSurface::Navigation,
                    target: crate::extension_contract::HostTarget::Session,
                    items: vec![crate::extension_contract::HostDocumentItem {
                        id: format!("workspace:{workspace_id}"),
                        parent_id: None,
                        role: crate::extension_contract::HostItemRole::Section,
                        label: "main".into(),
                        detail: None,
                        terminal: None,
                        action_ids: Vec::new(),
                        flags: crate::extension_contract::HostDocumentItemFlags::default(),
                        view: None,
                    }],
                    actions: Vec::new(),
                    required_capabilities: Vec::new(),
                },
            )
            .unwrap();
        assert_eq!(app.session_snapshot().space_order.len(), 1);
    }

    #[test]
    fn session_snapshot_structure_projection_is_optional_when_decoding() {
        let mut value = serde_json::to_value(app_with_two_tabs().session_snapshot()).unwrap();
        value.as_object_mut().unwrap().remove("collections");
        value.as_object_mut().unwrap().remove("delegations");

        let decoded: crate::api::schema::SessionSnapshot = serde_json::from_value(value).unwrap();

        assert!(decoded.collections.is_empty());
        assert!(decoded.delegations.is_empty());
    }

    #[test]
    fn session_snapshot_bootstraps_runtime_resources() {
        let mut app = app_with_two_tabs();
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_snapshot".into(),
            method: Method::SessionSnapshot(EmptyParams::default()),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::SessionSnapshot { snapshot } = success.result else {
            panic!("expected session snapshot response");
        };
        assert_eq!(success.id, "req_snapshot");
        assert_eq!(snapshot.workspaces.len(), 1);
        assert_eq!(snapshot.tabs.len(), 2);
        assert_eq!(snapshot.panes.len(), 2);
        assert_eq!(snapshot.layouts.len(), 2);
        assert_eq!(
            snapshot.focused_workspace_id.as_deref(),
            Some(snapshot.workspaces[0].workspace_id.as_str())
        );
        assert_eq!(
            snapshot.focused_tab_id.as_deref(),
            Some(snapshot.tabs[0].tab_id.as_str())
        );
        assert_eq!(
            snapshot.focused_pane_id.as_deref(),
            Some(snapshot.panes[0].pane_id.as_str())
        );
    }
}
