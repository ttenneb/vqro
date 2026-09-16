//! Bundled Spaces document provider over the public session-space contract.

use crate::extension_contract::{
    HostDocumentError, HostDocumentItem, HostDocumentItemFlags, HostDocumentV1, HostItemRole,
    HostSpaceOrderRef, HostSpacesV1, HostSurface, HostTarget, HOST_DOCUMENT_CONTRACT_V1,
};

pub(crate) const NAMESPACE: &str = "vqro.spaces";
pub(crate) const SESSION_SCOPE_ID: &str = "session";

pub(crate) fn project(snapshot: &HostSpacesV1) -> Result<HostDocumentV1, HostDocumentError> {
    snapshot.validate()?;
    let mut items = Vec::with_capacity(
        snapshot.repositories.len()
            + snapshot
                .repositories
                .iter()
                .map(|repository| repository.workspaces.len())
                .sum::<usize>()
            + snapshot.standalone.len(),
    );

    if snapshot.order.is_empty() {
        for repository in &snapshot.repositories {
            append_repository(&mut items, repository);
        }
        for workspace in &snapshot.standalone {
            items.push(workspace_item(workspace, None));
        }
    } else {
        for entry in &snapshot.order {
            match entry {
                HostSpaceOrderRef::Repository { repository_id } => {
                    if let Some(repository) = snapshot
                        .repositories
                        .iter()
                        .find(|repository| repository.repository_id == *repository_id)
                    {
                        append_repository(&mut items, repository);
                    }
                }
                HostSpaceOrderRef::StandaloneWorkspace { workspace_id } => {
                    if let Some(workspace) = snapshot
                        .standalone
                        .iter()
                        .find(|workspace| workspace.workspace_id == *workspace_id)
                    {
                        items.push(workspace_item(workspace, None));
                    }
                }
            }
        }
    }

    let document = HostDocumentV1 {
        contract: HOST_DOCUMENT_CONTRACT_V1.into(),
        namespace: NAMESPACE.into(),
        generation: snapshot.generation,
        revision: snapshot.revision,
        surface: HostSurface::Navigation,
        target: HostTarget::Session,
        items,
        actions: Vec::new(),
        required_capabilities: Vec::new(),
    };
    document.validate()?;
    Ok(document)
}

fn append_repository(
    items: &mut Vec<HostDocumentItem>,
    repository: &crate::extension_contract::HostSpaceRepository,
) {
    let repository_item_id = format!("repository:{}", repository.repository_id);
    items.push(HostDocumentItem {
        id: repository_item_id.clone(),
        parent_id: None,
        role: HostItemRole::Group,
        label: repository.label.clone(),
        detail: Some(format!(
            "{} workspace{}",
            repository.workspaces.len(),
            if repository.workspaces.len() == 1 {
                ""
            } else {
                "s"
            }
        )),
        terminal: None,
        action_ids: Vec::new(),
        flags: HostDocumentItemFlags {
            expanded: true,
            ..Default::default()
        },
    });
    for workspace in &repository.workspaces {
        items.push(workspace_item(workspace, Some(repository_item_id.clone())));
    }
}

fn workspace_item(
    workspace: &crate::extension_contract::HostSpaceWorkspaceRef,
    parent_id: Option<String>,
) -> HostDocumentItem {
    HostDocumentItem {
        id: format!("workspace:{}", workspace.workspace_id),
        parent_id,
        role: HostItemRole::Section,
        label: workspace.label.clone(),
        detail: None,
        terminal: None,
        action_ids: Vec::new(),
        flags: HostDocumentItemFlags::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_contract::{
        HostSpaceOrderRef, HostSpaceRepository, HostSpaceWorkspaceRef,
    };

    fn snapshot() -> HostSpacesV1 {
        HostSpacesV1 {
            contract: "host.spaces.v1".into(),
            generation: 5,
            revision: 12,
            scope_id: "session_alpha".into(),
            repositories: vec![HostSpaceRepository {
                repository_id: "repo_vqro".into(),
                label: "vqro".into(),
                workspaces: vec![
                    HostSpaceWorkspaceRef {
                        workspace_id: "workspace_main".into(),
                        label: "main".into(),
                    },
                    HostSpaceWorkspaceRef {
                        workspace_id: "workspace_feature".into(),
                        label: "feature/spaces".into(),
                    },
                ],
            }],
            standalone: vec![HostSpaceWorkspaceRef {
                workspace_id: "workspace_scratch".into(),
                label: "Scratch".into(),
            }],
            order: Vec::new(),
        }
    }

    #[test]
    fn spaces_projection_matches_normalized_fixture() {
        let actual = serde_json::to_value(project(&snapshot()).unwrap()).unwrap();
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/spaces-document-v1.json"))
                .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn spaces_projection_preserves_interleaved_top_level_order() {
        let mut snapshot = snapshot();
        snapshot.order = vec![
            HostSpaceOrderRef::StandaloneWorkspace {
                workspace_id: "workspace_scratch".into(),
            },
            HostSpaceOrderRef::Repository {
                repository_id: "repo_vqro".into(),
            },
        ];
        let document = project(&snapshot).unwrap();
        assert_eq!(document.items[0].id, "workspace:workspace_scratch");
        assert_eq!(document.items[1].id, "repository:repo_vqro");
    }

    #[test]
    fn spaces_projection_rejects_an_invalid_snapshot() {
        let mut invalid = snapshot();
        invalid.repositories[0].workspaces[0].workspace_id = "workspace_scratch".into();
        assert!(project(&invalid).is_err());
    }
}
