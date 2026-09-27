//! Exact production pin and host-side validation for the sandboxed Spaces provider.

use serde::{Deserialize, Serialize};

pub(crate) const SPACES_PACKAGE_ID: &str = "vqro.spaces";
pub(crate) const SPACES_NAMESPACE: &str = "vqro.spaces";
pub(crate) const SPACES_SERVICE_ID: &str = "spaces";
pub(crate) const SPACES_SCOPE_ID: &str = "session";
pub(crate) const SPACES_DOCUMENT_RENDER_METHOD_V1: &str = "vqro.spaces.document.render.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SpacesProductionPinV1 {
    pub(crate) schema: String,
    pub(crate) schema_version: u32,
    pub(crate) package_id: String,
    pub(crate) version: String,
    pub(crate) source_url: String,
    pub(crate) source_commit: String,
    pub(crate) package_path: String,
    pub(crate) package_sha256: String,
    pub(crate) manifest_sha256: String,
    pub(crate) component_sha256: String,
    pub(crate) service_id: String,
    pub(crate) namespace: String,
    pub(crate) method: String,
}

impl SpacesProductionPinV1 {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema != "vqro.spaces.production-pin.v1"
            || self.schema_version != 1
            || self.package_id != SPACES_PACKAGE_ID
            || self.version != "1.0.0"
            || self.source_url != "https://github.com/ttenneb/vqro-extension-spaces.git"
            || self.source_commit.len() != 40
            || !lower_hex(&self.source_commit)
            || self.package_path != "dist/vqro-spaces.vqrox"
            || !digest(&self.package_sha256)
            || !digest(&self.manifest_sha256)
            || !digest(&self.component_sha256)
            || self.service_id != SPACES_SERVICE_ID
            || self.namespace != SPACES_NAMESPACE
            || self.method != SPACES_DOCUMENT_RENDER_METHOD_V1
        {
            return Err("invalid Spaces production pin".into());
        }
        Ok(())
    }
}

pub(crate) fn embedded_spaces_production_pin() -> Result<SpacesProductionPinV1, String> {
    let pin: SpacesProductionPinV1 = serde_json::from_str(include_str!(
        "../extensions/first-party/spaces/production-pin-v1.json"
    ))
    .map_err(|error| format!("decode Spaces production pin: {error}"))?;
    pin.validate()?;
    Ok(pin)
}

pub(crate) fn validate_spaces_document(
    snapshot: &crate::extension_contract::HostSpacesV1,
    document: &crate::extension_contract::HostDocumentV1,
) -> Result<(), String> {
    snapshot.validate().map_err(|error| error.to_string())?;
    document.validate().map_err(|error| error.to_string())?;
    if document.contract != crate::extension_contract::HOST_DOCUMENT_CONTRACT_V1
        || document.namespace != SPACES_NAMESPACE
        || document.generation != snapshot.generation
        || document.revision != snapshot.revision
        || document.surface != crate::extension_contract::HostSurface::Navigation
        || document.target != crate::extension_contract::HostTarget::Session
        || !document.actions.is_empty()
        || !document.required_capabilities.is_empty()
    {
        return Err("Spaces document header or authority is invalid".into());
    }

    type ExpectedItem = (
        String,
        Option<String>,
        crate::extension_contract::HostItemRole,
        String,
        Option<String>,
        bool,
    );
    fn append_repository(
        expected: &mut Vec<ExpectedItem>,
        repository: &crate::extension_contract::HostSpaceRepository,
    ) {
        let parent = format!("repository:{}", repository.repository_id);
        expected.push((
            parent.clone(),
            None,
            crate::extension_contract::HostItemRole::Group,
            repository.label.clone(),
            Some(format!(
                "{} workspace{}",
                repository.workspaces.len(),
                if repository.workspaces.len() == 1 {
                    ""
                } else {
                    "s"
                }
            )),
            true,
        ));
        for workspace in &repository.workspaces {
            expected.push((
                format!("workspace:{}", workspace.workspace_id),
                Some(parent.clone()),
                crate::extension_contract::HostItemRole::Section,
                workspace.label.clone(),
                None,
                false,
            ));
        }
    }
    fn append_workspace(
        expected: &mut Vec<ExpectedItem>,
        workspace: &crate::extension_contract::HostSpaceWorkspaceRef,
    ) {
        expected.push((
            format!("workspace:{}", workspace.workspace_id),
            None,
            crate::extension_contract::HostItemRole::Section,
            workspace.label.clone(),
            None,
            false,
        ));
    }

    let mut expected = Vec::new();
    if snapshot.order.is_empty() {
        for repository in &snapshot.repositories {
            append_repository(&mut expected, repository);
        }
        for workspace in &snapshot.standalone {
            append_workspace(&mut expected, workspace);
        }
    } else {
        for order in &snapshot.order {
            match order {
                crate::extension_contract::HostSpaceOrderRef::Repository { repository_id } => {
                    let repository = snapshot
                        .repositories
                        .iter()
                        .find(|repository| repository.repository_id == *repository_id)
                        .ok_or("Spaces order references a missing repository")?;
                    append_repository(&mut expected, repository);
                }
                crate::extension_contract::HostSpaceOrderRef::StandaloneWorkspace {
                    workspace_id,
                } => {
                    let workspace = snapshot
                        .standalone
                        .iter()
                        .find(|workspace| workspace.workspace_id == *workspace_id)
                        .ok_or("Spaces order references a missing workspace")?;
                    append_workspace(&mut expected, workspace);
                }
            }
        }
    }
    if document.items.len() != expected.len() {
        return Err("Spaces document does not cover the exact snapshot".into());
    }
    for (item, (id, parent_id, role, label, detail, expanded)) in
        document.items.iter().zip(expected)
    {
        let expected_flags = crate::extension_contract::HostDocumentItemFlags {
            expanded,
            ..Default::default()
        };
        if item.id != id
            || item.parent_id != parent_id
            || item.role != role
            || item.label != label
            || item.detail != detail
            || item.terminal.is_some()
            || !item.action_ids.is_empty()
            || item.flags != expected_flags
        {
            return Err("Spaces document diverges from the host snapshot".into());
        }
    }
    Ok(())
}

fn lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn digest(value: &str) -> bool {
    value.len() == 64 && lower_hex(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_pin_is_exact() {
        embedded_spaces_production_pin().unwrap();
    }

    #[test]
    fn document_validation_requires_exact_snapshot_parity() {
        let snapshot: crate::extension_contract::HostSpacesV1 =
            serde_json::from_value(serde_json::json!({
                "contract": "host.spaces.v1",
                "generation": 4,
                "revision": 9,
                "scope_id": "session",
                "repositories": [{
                    "repository_id": "repo_main",
                    "label": "Repository",
                    "workspaces": [{"workspace_id": "workspace_main", "label": "main"}]
                }],
                "standalone": [{"workspace_id": "workspace_notes", "label": "Notes"}],
                "order": [
                    {"kind": "standalone_workspace", "workspace_id": "workspace_notes"},
                    {"kind": "repository", "repository_id": "repo_main"}
                ]
            }))
            .unwrap();
        let document: crate::extension_contract::HostDocumentV1 =
            serde_json::from_value(serde_json::json!({
                "contract": "host.document.v1",
                "namespace": "vqro.spaces",
                "generation": 4,
                "revision": 9,
                "surface": "navigation",
                "target": {"kind": "session"},
                "items": [
                    {
                        "id": "workspace:workspace_notes",
                        "role": "section",
                        "label": "Notes",
                        "action_ids": [],
                        "flags": {}
                    },
                    {
                        "id": "repository:repo_main",
                        "role": "group",
                        "label": "Repository",
                        "detail": "1 workspace",
                        "action_ids": [],
                        "flags": {"expanded": true}
                    },
                    {
                        "id": "workspace:workspace_main",
                        "parent_id": "repository:repo_main",
                        "role": "section",
                        "label": "main",
                        "action_ids": [],
                        "flags": {}
                    }
                ],
                "actions": [],
                "required_capabilities": []
            }))
            .unwrap();
        validate_spaces_document(&snapshot, &document).unwrap();

        let mut wrong_label = document.clone();
        wrong_label.items[2].label = "other".into();
        assert!(validate_spaces_document(&snapshot, &wrong_label)
            .unwrap_err()
            .contains("diverges"));
        let mut missing = document.clone();
        missing.items.pop();
        assert!(validate_spaces_document(&snapshot, &missing)
            .unwrap_err()
            .contains("exact snapshot"));
        let mut stale = document;
        stale.revision = 8;
        assert!(validate_spaces_document(&snapshot, &stale)
            .unwrap_err()
            .contains("header"));
    }
}
