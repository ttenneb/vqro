//! Shadow implementation of the bundled Collections document provider.
//!
//! The provider consumes and emits only public extension contracts. It does not
//! receive pane runtimes, PTYs, layout coordinates, focus, input, or styling.

use crate::extension_contract::{
    HostCapability, HostConfirmation, HostDocumentAction, HostDocumentError, HostDocumentItem,
    HostDocumentItemFlags, HostDocumentOperation, HostDocumentV1, HostEffect, HostItemRole,
    HostSurface, HostTarget, HostTerminalGroupsV1, HostTerminalLifecycle,
    HOST_DOCUMENT_CONTRACT_V1,
};

pub(crate) const NAMESPACE: &str = "vqro.collections";

pub(crate) fn project(
    snapshot: &HostTerminalGroupsV1,
) -> Result<HostDocumentV1, HostDocumentError> {
    snapshot.validate()?;
    let mut items = Vec::new();
    let mut actions = Vec::new();
    for group in &snapshot.groups {
        let group_item_id = format!("group:{}", group.group_id);
        items.push(HostDocumentItem {
            id: group_item_id.clone(),
            parent_id: None,
            role: HostItemRole::Group,
            label: group.label.clone().unwrap_or_else(|| "Collection".into()),
            detail: Some(format!("{} terminals", group.members.len())),
            terminal: None,
            action_ids: Vec::new(),
            flags: HostDocumentItemFlags {
                expanded: true,
                ..Default::default()
            },
        });
        for member in &group.members {
            let item_id = format!("terminal:{}", member.terminal.terminal_id);
            let action_id = format!("focus:{}", member.terminal.terminal_id);
            items.push(HostDocumentItem {
                id: item_id.clone(),
                parent_id: Some(group_item_id.clone()),
                role: HostItemRole::Terminal,
                label: member.label.clone(),
                detail: Some(
                    match member.lifecycle {
                        HostTerminalLifecycle::Live => "live",
                        HostTerminalLifecycle::Exited => "exited",
                        HostTerminalLifecycle::Archived => "archived",
                    }
                    .into(),
                ),
                terminal: Some(member.terminal.clone()),
                action_ids: vec![action_id.clone()],
                flags: HostDocumentItemFlags {
                    selected: group.selected_terminal_id.as_deref()
                        == Some(member.terminal.terminal_id.as_str()),
                    disabled: false,
                    ..Default::default()
                },
            });
            actions.push(HostDocumentAction {
                id: action_id,
                label: "Focus terminal".into(),
                operation: HostDocumentOperation::Host {
                    effect: HostEffect::FocusTerminal {
                        terminal_id: member.terminal.terminal_id.clone(),
                    },
                },
                subject_id: Some(item_id),
                confirmation: HostConfirmation::None,
            });
        }
    }
    for member in &snapshot.ungrouped {
        let item_id = format!("terminal:{}", member.terminal.terminal_id);
        let action_id = format!("focus:{}", member.terminal.terminal_id);
        items.push(HostDocumentItem {
            id: item_id.clone(),
            parent_id: None,
            role: HostItemRole::Terminal,
            label: member.label.clone(),
            detail: Some(
                match member.lifecycle {
                    HostTerminalLifecycle::Live => "live",
                    HostTerminalLifecycle::Exited => "exited",
                    HostTerminalLifecycle::Archived => "archived",
                }
                .into(),
            ),
            terminal: Some(member.terminal.clone()),
            action_ids: vec![action_id.clone()],
            flags: HostDocumentItemFlags::default(),
        });
        actions.push(HostDocumentAction {
            id: action_id,
            label: "Focus terminal".into(),
            operation: HostDocumentOperation::Host {
                effect: HostEffect::FocusTerminal {
                    terminal_id: member.terminal.terminal_id.clone(),
                },
            },
            subject_id: Some(item_id),
            confirmation: HostConfirmation::None,
        });
    }
    let document = HostDocumentV1 {
        contract: HOST_DOCUMENT_CONTRACT_V1.into(),
        namespace: NAMESPACE.into(),
        generation: snapshot.generation,
        revision: snapshot.revision,
        surface: HostSurface::Navigation,
        target: HostTarget::Tab {
            tab_id: snapshot.scope_id.clone(),
        },
        items,
        actions,
        required_capabilities: vec![HostCapability::FocusTerminal],
    };
    document.validate()?;
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_contract::{
        HostTerminalGroup, HostTerminalGroupMember, HostTerminalPlacement, HostTerminalRef,
    };

    fn snapshot(revision: u64) -> HostTerminalGroupsV1 {
        HostTerminalGroupsV1 {
            contract: "host.terminal-groups.v1".into(),
            generation: 3,
            revision,
            scope_id: "w1:t2".into(),
            groups: vec![HostTerminalGroup {
                group_id: "collection_7".into(),
                label: Some("Review".into()),
                selected_terminal_id: Some("term_b".into()),
                members: vec![
                    member("term_a", "w1:p4", "API", HostTerminalLifecycle::Live),
                    member("term_b", "w1:p7", "UX", HostTerminalLifecycle::Exited),
                ],
            }],
            ungrouped: vec![member(
                "term_c",
                "w1:p9",
                "Shell",
                HostTerminalLifecycle::Live,
            )],
        }
    }

    fn member(
        terminal_id: &str,
        pane_id: &str,
        label: &str,
        lifecycle: HostTerminalLifecycle,
    ) -> HostTerminalGroupMember {
        HostTerminalGroupMember {
            terminal: HostTerminalRef {
                terminal_id: terminal_id.into(),
                placement: Some(HostTerminalPlacement {
                    pane_id: pane_id.into(),
                    workspace_id: "w1".into(),
                    tab_id: "w1:t2".into(),
                }),
            },
            label: label.into(),
            lifecycle,
        }
    }

    #[test]
    fn collection_document_and_actions_match_normalized_parity_vectors() {
        let document = project(&snapshot(11)).unwrap();
        assert!(!document.items.is_empty());
        let actual = serde_json::to_value(document).unwrap();
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/collections-document-v1.json"
        ))
        .unwrap();
        assert_eq!(actual, expected);
    }
}
