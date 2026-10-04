//! Bundled Delegation document provider over the generic relationship contract.

use std::collections::{HashMap, HashSet};

use crate::extension_contract::{
    HostDocumentError, HostDocumentItem, HostDocumentItemFlags, HostDocumentV1, HostItemRole,
    HostRelationship, HostRelationshipLifecycle, HostRelationshipsV1, HostSurface, HostTarget,
    HOST_DOCUMENT_CONTRACT_V1,
};

pub(crate) const NAMESPACE: &str = "vqro.delegation";
const PURPOSE_METADATA_KEY: &str = "vqro.delegation.purpose";

pub(crate) fn project(snapshot: &HostRelationshipsV1) -> Result<HostDocumentV1, HostDocumentError> {
    snapshot.validate()?;

    let included = snapshot
        .relationships
        .iter()
        .filter(|relationship| relationship.kind == NAMESPACE)
        .map(|relationship| relationship.relationship_id.as_str())
        .collect::<HashSet<_>>();
    let mut children: HashMap<Option<&str>, Vec<&HostRelationship>> = HashMap::new();
    for relationship in snapshot
        .relationships
        .iter()
        .filter(|relationship| relationship.kind == NAMESPACE)
    {
        let parent = relationship
            .parent_id
            .as_deref()
            .filter(|parent| included.contains(parent));
        children.entry(parent).or_default().push(relationship);
    }
    for siblings in children.values_mut() {
        siblings.sort_by(|left, right| {
            left.sibling_rank
                .cmp(&right.sibling_rank)
                .then_with(|| left.relationship_id.cmp(&right.relationship_id))
        });
    }

    let mut items = Vec::with_capacity(included.len());
    append_children(None, &children, &mut items);
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

fn append_children(
    parent: Option<&str>,
    children: &HashMap<Option<&str>, Vec<&HostRelationship>>,
    items: &mut Vec<HostDocumentItem>,
) {
    let Some(siblings) = children.get(&parent) else {
        return;
    };
    for relationship in siblings {
        let item_id = format!("relationship:{}", relationship.relationship_id);
        let parent_id = parent.map(|id| format!("relationship:{id}"));
        let label = relationship
            .metadata
            .get(PURPOSE_METADATA_KEY)
            .and_then(serde_json::Value::as_str)
            .filter(|purpose| !purpose.is_empty())
            .unwrap_or(&relationship.relationship_id)
            .to_string();
        items.push(HostDocumentItem {
            id: item_id,
            parent_id,
            role: if relationship.terminal.is_some() {
                HostItemRole::Terminal
            } else {
                HostItemRole::Section
            },
            label,
            detail: None,
            terminal: relationship.terminal.clone(),
            action_ids: Vec::new(),
            flags: HostDocumentItemFlags {
                expanded: children.contains_key(&Some(relationship.relationship_id.as_str())),
                disabled: relationship.lifecycle == HostRelationshipLifecycle::Retained,
                ..Default::default()
            },
            view: None,
        });
        append_children(Some(relationship.relationship_id.as_str()), children, items);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_contract::{HostTerminalPlacement, HostTerminalRef};
    use std::collections::BTreeMap;

    fn snapshot() -> HostRelationshipsV1 {
        HostRelationshipsV1 {
            contract: "host.relationships.v1".into(),
            generation: 6,
            revision: 13,
            scope_id: "session_alpha".into(),
            relationships: vec![
                relationship(
                    "child",
                    Some("root"),
                    0,
                    None,
                    HostRelationshipLifecycle::Retained,
                ),
                relationship(
                    "root",
                    None,
                    0,
                    Some("Coordinate release"),
                    HostRelationshipLifecycle::Active,
                ),
            ],
        }
    }

    fn relationship(
        id: &str,
        parent_id: Option<&str>,
        sibling_rank: u64,
        purpose: Option<&str>,
        lifecycle: HostRelationshipLifecycle,
    ) -> HostRelationship {
        let mut metadata = BTreeMap::new();
        if let Some(purpose) = purpose {
            metadata.insert(PURPOSE_METADATA_KEY.into(), serde_json::json!(purpose));
        }
        HostRelationship {
            relationship_id: id.into(),
            kind: NAMESPACE.into(),
            terminal: Some(HostTerminalRef {
                terminal_id: format!("term_{id}"),
                placement: Some(HostTerminalPlacement {
                    pane_id: format!("w1:p_{id}"),
                    workspace_id: "w1".into(),
                    tab_id: "w1:t1".into(),
                }),
            }),
            parent_id: parent_id.map(str::to_string),
            sibling_rank,
            lifecycle,
            metadata,
        }
    }

    #[test]
    fn delegation_projection_matches_fixture() {
        let document = project(&snapshot()).unwrap();
        assert!(!document.items.is_empty());
        let actual = serde_json::to_value(document).unwrap();
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/delegation-document-v1.json"
        ))
        .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn generation_one_document_limit_falls_back_before_provider_dispatch() {
        let count = 513;
        let snapshot = HostRelationshipsV1 {
            contract: crate::extension_contract::HOST_RELATIONSHIPS_CONTRACT_V1.into(),
            generation: 1,
            revision: 1,
            scope_id: "session".into(),
            relationships: (0..count)
                .map(|index| HostRelationship {
                    relationship_id: format!("r{index}"),
                    kind: NAMESPACE.into(),
                    terminal: None,
                    parent_id: None,
                    sibling_rank: index,
                    lifecycle: HostRelationshipLifecycle::Active,
                    metadata: BTreeMap::new(),
                })
                .collect(),
        };
        snapshot.validate().unwrap();
        assert!(project(&snapshot).is_err());
    }

    #[test]
    fn retained_relationships_are_present_but_disabled() {
        let document = project(&snapshot()).unwrap();
        assert_eq!(document.items[0].label, "Coordinate release");
        assert_eq!(document.items[1].label, "child");
        assert!(document.items[1].flags.disabled);
    }
}
