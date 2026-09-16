//! Strict client-side joins from opaque host documents to the current shell snapshot.
//!
//! Documents only contribute presentation order, labels, and hierarchy. Runtime identity and
//! agent status always come from the current endpoint snapshot.

use std::collections::{HashMap, HashSet};

use crate::extension_contract::{HostItemRole, HostSurface, HostTarget};
use crate::protocol::endpoint::{
    ClientShellStructureProjection, HostDocumentProjectionEntryV1, HostDocumentsProjectionV1,
};
use crate::protocol::ClientShellSnapshot;

const DELEGATION_NAMESPACE: &str = "vqro.delegation";
const AGENT_STATUS_NAMESPACE: &str = "vqro.agent-status";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DocumentedDelegation {
    pub(super) relationship_id: String,
    pub(super) parent_id: Option<String>,
    pub(super) parent_pane_id: Option<String>,
    pub(super) label: String,
    pub(super) depth: usize,
    pub(super) disabled: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct DelegationNavigation {
    by_pane: HashMap<String, DocumentedDelegation>,
}

impl DelegationNavigation {
    pub(super) fn for_pane(&self, pane_id: &str) -> Option<&DocumentedDelegation> {
        self.by_pane.get(pane_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DocumentedAgent {
    pub(super) pane_id: String,
    pub(super) label: String,
    pub(super) group_label: String,
    pub(super) starts_group: bool,
}

#[cfg(test)]
thread_local! {
    static DELEGATION_NAVIGATION_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_delegation_navigation_call_count() {
    DELEGATION_NAVIGATION_CALLS.with(|count| count.set(0));
}

#[cfg(test)]
pub(super) fn delegation_navigation_call_count() -> usize {
    DELEGATION_NAVIGATION_CALLS.with(std::cell::Cell::get)
}

pub(super) fn delegation_navigation(
    snapshot: &ClientShellSnapshot,
    structure: &ClientShellStructureProjection,
    projection: Option<&HostDocumentsProjectionV1>,
) -> Option<DelegationNavigation> {
    #[cfg(test)]
    DELEGATION_NAVIGATION_CALLS.with(|count| count.set(count.get().saturating_add(1)));
    let document = session_navigation_document(snapshot, projection, DELEGATION_NAMESPACE)?;
    let item_indexes = document
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| (item.id.as_str(), index))
        .collect::<HashMap<_, _>>();
    if item_indexes.len() != document.items.len()
        || document
            .items
            .iter()
            .any(|item| item.id.strip_prefix("relationship:").is_none())
    {
        return None;
    }

    let panes = snapshot
        .panes
        .iter()
        .map(|pane| (pane.pane_id.as_str(), pane))
        .collect::<HashMap<_, _>>();
    let authoritative = structure
        .delegations
        .iter()
        .map(|delegation| (delegation.delegation_id.as_str(), delegation))
        .collect::<HashMap<_, _>>();
    if authoritative.len() != structure.delegations.len()
        || item_indexes.len() != authoritative.len()
    {
        return None;
    }

    let mut pane_ids = HashSet::new();
    let mut terminal_ids = HashSet::new();
    let mut live_relationships = HashSet::new();
    let mut by_pane = HashMap::new();
    for item in &document.items {
        if !matches!(item.role, HostItemRole::Terminal | HostItemRole::Section)
            || (item.role == HostItemRole::Section && item.terminal.is_some())
        {
            return None;
        }
        let relationship_id = item.id.strip_prefix("relationship:")?;
        let baked = authoritative.get(relationship_id)?;
        let parent_id = match item.parent_id.as_deref() {
            Some(parent) => Some(parent.strip_prefix("relationship:")?),
            None => None,
        };
        if relationship_id.is_empty()
            || parent_id != baked.parent_id.as_deref()
            || item.parent_id.as_deref().is_some_and(|parent| {
                !parent.starts_with("relationship:") || !item_indexes.contains_key(parent)
            })
        {
            return None;
        }
        let depth = baked.depth;
        let Some(expected_pane_id) = baked.pane_id.as_deref() else {
            if item.terminal.is_some() || item.role != HostItemRole::Section {
                return None;
            }
            continue;
        };
        let terminal = item.terminal.as_ref()?;
        if item.flags.disabled || item.role != HostItemRole::Terminal {
            return None;
        }
        let placement = terminal.placement.as_ref()?;
        if !terminal_ids.insert(terminal.terminal_id.as_str())
            || !pane_ids.insert(placement.pane_id.as_str())
        {
            return None;
        }

        let live_join = placement.pane_id == expected_pane_id
            && panes.get(placement.pane_id.as_str()).is_some_and(|pane| {
                pane.workspace_id == placement.workspace_id && pane.tab_id == placement.tab_id
            });
        if !live_join || !live_relationships.insert((relationship_id, placement.pane_id.as_str())) {
            return None;
        }

        let parent_pane_id = item.parent_id.as_deref().and_then(|parent_id| {
            let parent = &document.items[*item_indexes.get(parent_id)?];
            parent
                .terminal
                .as_ref()?
                .placement
                .as_ref()
                .map(|placement| placement.pane_id.clone())
        });
        by_pane.insert(
            placement.pane_id.clone(),
            DocumentedDelegation {
                relationship_id: relationship_id.to_owned(),
                parent_id: item
                    .parent_id
                    .as_deref()
                    .and_then(|parent| parent.strip_prefix("relationship:"))
                    .map(str::to_owned),
                parent_pane_id,
                label: item.label.clone(),
                depth,
                disabled: item.flags.disabled,
            },
        );
    }

    let baked_live_relationships = authoritative
        .values()
        .filter_map(|delegation| {
            let pane_id = delegation.pane_id.as_deref()?;
            panes
                .contains_key(pane_id)
                .then_some((delegation.delegation_id.as_str(), pane_id))
        })
        .collect::<HashSet<_>>();
    // Completeness is all-or-nothing: a sidecar may not silently omit or invent a live
    // relationship while replacing the baked projection.
    if live_relationships != baked_live_relationships {
        return None;
    }

    // Every enabled terminal is required to resolve to a current pane and agent. Disabled retained
    // nodes may resolve only to an archived collection member so they can preserve ancestry.
    if document.items.iter().any(|item| {
        !item.flags.disabled
            && item.role == HostItemRole::Terminal
            && item
                .terminal
                .as_ref()
                .and_then(|terminal| terminal.placement.as_ref())
                .is_none_or(|placement| !panes.contains_key(placement.pane_id.as_str()))
    }) {
        return None;
    }

    Some(DelegationNavigation { by_pane })
}

#[cfg(test)]
thread_local! {
    static AGENT_NAVIGATION_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_agent_navigation_call_count() {
    AGENT_NAVIGATION_CALLS.with(|count| count.set(0));
}

#[cfg(test)]
pub(super) fn agent_navigation_call_count() -> usize {
    AGENT_NAVIGATION_CALLS.with(std::cell::Cell::get)
}

pub(super) fn agent_navigation(
    snapshot: &ClientShellSnapshot,
    projection: Option<&HostDocumentsProjectionV1>,
) -> Option<Vec<DocumentedAgent>> {
    #[cfg(test)]
    AGENT_NAVIGATION_CALLS.with(|count| count.set(count.get().saturating_add(1)));
    let document = session_navigation_document(snapshot, projection, AGENT_STATUS_NAMESPACE)?;
    let agents = snapshot
        .agents
        .iter()
        .map(|agent| (agent.pane_id.as_str(), agent))
        .collect::<HashMap<_, _>>();
    if agents.len() != snapshot.agents.len() {
        return None;
    }
    let panes = snapshot
        .panes
        .iter()
        .map(|pane| (pane.pane_id.as_str(), pane))
        .collect::<HashMap<_, _>>();
    let groups = document
        .items
        .iter()
        .filter(|item| item.role == HostItemRole::Group)
        .map(|item| (item.id.as_str(), item))
        .collect::<HashMap<_, _>>();
    if groups.len()
        != document
            .items
            .iter()
            .filter(|item| item.role == HostItemRole::Group)
            .count()
        || groups.values().any(|group| {
            group.parent_id.is_some()
                || group.terminal.is_some()
                || group.flags.disabled
                || !group.flags.expanded
        })
    {
        return None;
    }

    let mut seen_panes = HashSet::new();
    let mut seen_terminals = HashSet::new();
    let mut seen_groups = HashSet::new();
    let mut rows = Vec::with_capacity(snapshot.agents.len());
    for item in &document.items {
        match item.role {
            HostItemRole::Group => continue,
            HostItemRole::Terminal => {}
            _ => return None,
        }
        if item.flags.disabled {
            return None;
        }
        let group = groups.get(item.parent_id.as_deref()?)?;
        let terminal = item.terminal.as_ref()?;
        let placement = terminal.placement.as_ref()?;
        let agent = agents.get(placement.pane_id.as_str())?;
        let pane = panes.get(placement.pane_id.as_str())?;
        if !seen_panes.insert(placement.pane_id.as_str())
            || !seen_terminals.insert(terminal.terminal_id.as_str())
            || item.id.strip_prefix("terminal:") != Some(terminal.terminal_id.as_str())
            || agent.workspace_id != placement.workspace_id
            || agent.tab_id != placement.tab_id
            || pane.workspace_id != placement.workspace_id
            || pane.tab_id != placement.tab_id
        {
            return None;
        }
        rows.push(DocumentedAgent {
            pane_id: placement.pane_id.clone(),
            label: item.label.clone(),
            group_label: group.label.clone(),
            starts_group: seen_groups.insert(group.id.as_str()),
        });
    }
    if rows.len() != snapshot.agents.len() || seen_groups.len() != groups.len() {
        return None;
    }

    // Done is acknowledgement state owned by this client, never shared provider evidence.
    // Lift acknowledged rows out of the neutral Idle group while preserving provider order
    // within every other group and within Done itself.
    let mut done = Vec::new();
    rows.retain_mut(|row| {
        if agents
            .get(row.pane_id.as_str())
            .is_some_and(|agent| agent.agent_status == crate::api::schema::AgentStatus::Done)
        {
            row.group_label = "Done".into();
            done.push(row.clone());
            false
        } else {
            true
        }
    });
    if !done.is_empty() {
        let insertion = rows
            .iter()
            .position(|row| {
                agents.get(row.pane_id.as_str()).is_none_or(|agent| {
                    agent.agent_status != crate::api::schema::AgentStatus::Blocked
                })
            })
            .unwrap_or(rows.len());
        rows.splice(insertion..insertion, done);
    }
    let mut previous_group: Option<String> = None;
    for row in &mut rows {
        row.starts_group = previous_group.as_deref() != Some(row.group_label.as_str());
        previous_group = Some(row.group_label.clone());
    }
    Some(rows)
}

fn session_navigation_document<'a>(
    snapshot: &ClientShellSnapshot,
    projection: Option<&'a HostDocumentsProjectionV1>,
    namespace: &str,
) -> Option<&'a crate::extension_contract::HostDocumentV1> {
    let projection = projection.filter(|projection| {
        projection.boot_id == snapshot.boot_id && projection.revision == snapshot.revision
    })?;
    let mut entries = projection.documents.iter().filter(|entry| {
        entry.document.namespace == namespace
            && entry.document.surface == HostSurface::Navigation
            && entry.document.target == HostTarget::Session
    });
    let HostDocumentProjectionEntryV1 { document, .. } = entries.next()?;
    if entries.next().is_some() {
        return None;
    }
    // Endpoint projections are fully validated once at the client cache boundary. Render paths
    // perform only the authoritative snapshot join, avoiding repeated hierarchy validation.
    Some(document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::AgentStatus;
    use crate::extension_contract::{
        HostDocumentItem, HostDocumentItemFlags, HostDocumentV1, HostTerminalPlacement,
        HostTerminalRef, HOST_DOCUMENT_CONTRACT_V1,
    };
    use crate::protocol::endpoint::{
        ClientShellCollection, ClientShellCollectionMember, ClientShellDelegation,
    };
    use crate::protocol::ClientShellAgent;

    fn agent(pane_id: &str, status: AgentStatus) -> ClientShellAgent {
        ClientShellAgent {
            pane_id: pane_id.into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some(format!("baked-{pane_id}")),
            display_agent: None,
            agent: None,
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: status,
            state_change_seq: 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: pane_id == "pane_1",
        }
    }

    fn snapshot() -> ClientShellSnapshot {
        let mut snapshot = crate::client::shell::tests::snapshot();
        let mut second = snapshot.panes[0].clone();
        second.pane_id = "pane_2".into();
        second.focused = false;
        snapshot.panes.push(second);
        snapshot.agents = vec![
            agent("pane_1", AgentStatus::Done),
            agent("pane_2", AgentStatus::Working),
        ];
        snapshot
    }

    fn terminal_item(
        id: &str,
        pane_id: &str,
        label: &str,
        parent: Option<&str>,
        disabled: bool,
    ) -> HostDocumentItem {
        HostDocumentItem {
            id: id.into(),
            parent_id: parent.map(str::to_owned),
            role: HostItemRole::Terminal,
            label: label.into(),
            detail: None,
            terminal: Some(HostTerminalRef {
                terminal_id: id.split_once(':').map_or(id, |(_, id)| id).into(),
                placement: Some(HostTerminalPlacement {
                    pane_id: pane_id.into(),
                    workspace_id: "ws_1".into(),
                    tab_id: "tab_1".into(),
                }),
            }),
            action_ids: Vec::new(),
            flags: HostDocumentItemFlags {
                disabled,
                ..Default::default()
            },
        }
    }

    fn group(id: &str, label: &str) -> HostDocumentItem {
        HostDocumentItem {
            id: id.into(),
            parent_id: None,
            role: HostItemRole::Group,
            label: label.into(),
            detail: None,
            terminal: None,
            action_ids: Vec::new(),
            flags: HostDocumentItemFlags {
                expanded: true,
                ..Default::default()
            },
        }
    }

    fn projection(namespace: &str, items: Vec<HostDocumentItem>) -> HostDocumentsProjectionV1 {
        HostDocumentsProjectionV1 {
            boot_id: "boot-1".into(),
            revision: 1,
            documents: vec![HostDocumentProjectionEntryV1 {
                provider_priority: 0,
                document: HostDocumentV1 {
                    contract: HOST_DOCUMENT_CONTRACT_V1.into(),
                    namespace: namespace.into(),
                    generation: 1,
                    revision: 1,
                    surface: HostSurface::Navigation,
                    target: HostTarget::Session,
                    items,
                    actions: Vec::new(),
                    required_capabilities: Vec::new(),
                },
                confirmation_tokens: Default::default(),
            }],
        }
    }

    fn structure() -> ClientShellStructureProjection {
        ClientShellStructureProjection {
            session_id: "session".into(),
            boot_id: "boot-1".into(),
            revision: 1,
            collections: vec![ClientShellCollection {
                collection_id: "collection".into(),
                workspace_id: "ws_1".into(),
                tab_id: "tab_1".into(),
                label: None,
                selected_pane_id: Some("pane_1".into()),
                members: vec![
                    ClientShellCollectionMember {
                        pane_id: "pane_1".into(),
                        index: 0,
                        archived: false,
                    },
                    ClientShellCollectionMember {
                        pane_id: "pane_2".into(),
                        index: 1,
                        archived: true,
                    },
                ],
            }],
            delegations: vec![
                ClientShellDelegation {
                    delegation_id: "root".into(),
                    pane_id: None,
                    parent_id: None,
                    purpose: Some("baked root".into()),
                    depth: 0,
                    external_parent_id: None,
                },
                ClientShellDelegation {
                    delegation_id: "child".into(),
                    pane_id: Some("pane_1".into()),
                    parent_id: Some("root".into()),
                    purpose: Some("baked purpose".into()),
                    depth: 1,
                    external_parent_id: None,
                },
            ],
            repositories: Vec::new(),
            space_order: Vec::new(),
        }
    }

    #[test]
    fn complete_agent_document_controls_order_and_labels_without_status_authority() {
        let snapshot = snapshot();
        let projection = projection(
            AGENT_STATUS_NAMESPACE,
            vec![
                group("activity:working", "Working now"),
                terminal_item(
                    "terminal:t2",
                    "pane_2",
                    "Build",
                    Some("activity:working"),
                    false,
                ),
                group("activity:idle", "Idle now"),
                terminal_item(
                    "terminal:t1",
                    "pane_1",
                    "Review",
                    Some("activity:idle"),
                    false,
                ),
            ],
        );

        let rows = agent_navigation(&snapshot, Some(&projection)).unwrap();
        assert_eq!(
            rows.iter()
                .map(|row| row.pane_id.as_str())
                .collect::<Vec<_>>(),
            ["pane_1", "pane_2"]
        );
        assert_eq!(rows[0].label, "Review");
        assert_eq!(rows[0].group_label, "Done");
        assert!(rows[0].starts_group);
        assert_eq!(rows[1].label, "Build");
        assert_eq!(rows[1].group_label, "Working now");
        assert_eq!(snapshot.agents[0].agent_status, AgentStatus::Done);
        assert_eq!(snapshot.agents[1].agent_status, AgentStatus::Working);

        let mut unacknowledged = snapshot.clone();
        unacknowledged.agents[0].agent_status = AgentStatus::Idle;
        let rows = agent_navigation(&unacknowledged, Some(&projection)).unwrap();
        assert_eq!(rows[1].pane_id, "pane_1");
        assert_eq!(rows[1].group_label, "Idle now");

        let mut custom_view = snapshot.clone();
        custom_view.agent_view_label = Some("custom".into());
        let documented = agent_navigation(&custom_view, Some(&projection)).unwrap();
        let config =
            super::super::ClientShellConfig::from_config(&crate::config::Config::default());
        let row = super::super::agent_sidebar::agent_row_with_document(
            &custom_view,
            "pane_1",
            &config,
            None,
            documented.iter().find(|row| row.pane_id == "pane_1"),
        )
        .unwrap();
        assert!(row.group_label.is_none());
    }

    #[test]
    fn incomplete_disabled_or_stale_agent_document_falls_back_whole() {
        let snapshot = snapshot();
        let missing = projection(
            AGENT_STATUS_NAMESPACE,
            vec![
                group("activity:idle", "Idle"),
                terminal_item("terminal:t1", "pane_1", "One", Some("activity:idle"), false),
            ],
        );
        assert!(agent_navigation(&snapshot, Some(&missing)).is_none());

        let mut disabled = missing.clone();
        disabled.documents[0].document.items[1].flags.disabled = true;
        assert!(agent_navigation(&snapshot, Some(&disabled)).is_none());

        let mut stale = missing;
        stale.revision = 2;
        assert!(agent_navigation(&snapshot, Some(&stale)).is_none());
    }

    #[test]
    fn delegation_document_preserves_retained_ancestry_and_rejects_missing_live_terminal() {
        let snapshot = snapshot();
        let structure = structure();
        let mut retained_root =
            terminal_item("relationship:root", "pane_2", "Retained root", None, true);
        retained_root.role = HostItemRole::Section;
        retained_root.terminal = None;
        let projection = projection(
            DELEGATION_NAMESPACE,
            vec![
                retained_root,
                terminal_item(
                    "relationship:child",
                    "pane_1",
                    "Document purpose",
                    Some("relationship:root"),
                    false,
                ),
            ],
        );
        let navigation = delegation_navigation(&snapshot, &structure, Some(&projection)).unwrap();
        let child = navigation.for_pane("pane_1").unwrap();
        assert_eq!(child.label, "Document purpose");
        assert_eq!(child.depth, 1);
        assert_eq!(child.parent_pane_id, None);
        assert!(navigation.for_pane("pane_2").is_none());

        let mut missing = projection;
        missing.documents[0].document.items[1].terminal = None;
        assert!(delegation_navigation(&snapshot, &structure, Some(&missing)).is_none());
    }
}
