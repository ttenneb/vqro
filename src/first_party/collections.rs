//! Shadow implementation of the bundled Collections document provider.
//!
//! The provider consumes and emits only public extension contracts. It does not
//! receive pane runtimes, PTYs, layout coordinates, focus, input, or styling.

use std::collections::HashSet;

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
                    disabled: member.lifecycle == HostTerminalLifecycle::Archived,
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

/// Generic host fallback used when Collections is disabled, stale, or failed.
/// It intentionally drops product labels while retaining every terminal.
pub(crate) fn fallback(
    snapshot: &HostTerminalGroupsV1,
) -> Result<HostDocumentV1, HostDocumentError> {
    snapshot.validate()?;
    let mut items = Vec::new();
    let mut actions = Vec::new();
    for member in snapshot
        .groups
        .iter()
        .flat_map(|group| &group.members)
        .chain(&snapshot.ungrouped)
    {
        let item_id = format!("terminal:{}", member.terminal.terminal_id);
        let action_id = format!("focus:{}", member.terminal.terminal_id);
        items.push(HostDocumentItem {
            id: item_id.clone(),
            parent_id: None,
            role: HostItemRole::Terminal,
            label: member.label.clone(),
            detail: None,
            terminal: Some(member.terminal.clone()),
            action_ids: vec![action_id.clone()],
            flags: HostDocumentItemFlags {
                disabled: member.lifecycle == HostTerminalLifecycle::Archived,
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
    let document = HostDocumentV1 {
        contract: HOST_DOCUMENT_CONTRACT_V1.into(),
        namespace: "host.fallback".into(),
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

#[derive(Debug, Default)]
pub(crate) struct ShadowDocumentCache {
    document: Option<HostDocumentV1>,
}

impl ShadowDocumentCache {
    pub(crate) fn publish(
        &mut self,
        expected_generation: u64,
        granted_capabilities: &HashSet<HostCapability>,
        document: HostDocumentV1,
    ) -> Result<(), HostDocumentError> {
        document.validate()?;
        if document.namespace != NAMESPACE || document.generation != expected_generation {
            return Err(HostDocumentError(
                "document provider or generation mismatch".into(),
            ));
        }
        if document
            .required_capabilities
            .iter()
            .any(|capability| !granted_capabilities.contains(capability))
        {
            return Err(HostDocumentError(
                "document capability was not granted".into(),
            ));
        }
        if self.document.as_ref().is_some_and(|current| {
            current.generation > document.generation
                || (current.generation == document.generation
                    && current.revision >= document.revision)
        }) {
            return Err(HostDocumentError("stale document revision".into()));
        }
        self.document = Some(document);
        Ok(())
    }

    pub(crate) fn disable(&mut self) {
        self.document = None;
    }

    pub(crate) fn document_or_fallback(
        &self,
        snapshot: &HostTerminalGroupsV1,
    ) -> Result<HostDocumentV1, HostDocumentError> {
        match self.document.as_ref() {
            Some(document)
                if document.generation == snapshot.generation
                    && document.revision == snapshot.revision =>
            {
                Ok(document.clone())
            }
            _ => fallback(snapshot),
        }
    }
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
    fn collections_projection_matches_normalized_parity_fixture() {
        let document = project(&snapshot(11)).unwrap();
        let actual = serde_json::to_value(document).unwrap();
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/collections-document-v1.json"
        ))
        .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn stale_crashed_or_disabled_provider_falls_back_without_losing_terminals() {
        let grants = HashSet::from([HostCapability::FocusTerminal]);
        let mut cache = ShadowDocumentCache::default();
        cache
            .publish(3, &grants, project(&snapshot(4)).unwrap())
            .unwrap();
        assert_eq!(
            cache.document_or_fallback(&snapshot(4)).unwrap().namespace,
            NAMESPACE
        );
        assert!(cache
            .publish(3, &grants, project(&snapshot(4)).unwrap())
            .is_err());
        let stale_fallback = cache.document_or_fallback(&snapshot(5)).unwrap();
        assert_eq!(stale_fallback.namespace, "host.fallback");
        assert_eq!(stale_fallback.items.len(), 3);
        assert_eq!(
            stale_fallback
                .items
                .iter()
                .filter_map(|item| item.terminal.as_ref())
                .map(|terminal| terminal.terminal_id.as_str())
                .collect::<Vec<_>>(),
            ["term_a", "term_b", "term_c"]
        );
        cache.disable();
        assert_eq!(
            cache
                .document_or_fallback(&snapshot(4))
                .unwrap()
                .items
                .len(),
            3
        );
    }

    #[test]
    fn publication_requires_the_granted_host_effect_capability() {
        let mut cache = ShadowDocumentCache::default();
        let error = cache
            .publish(3, &HashSet::new(), project(&snapshot(1)).unwrap())
            .unwrap_err();
        assert!(error.to_string().contains("not granted"));
    }
}
