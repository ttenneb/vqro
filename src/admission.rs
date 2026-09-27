//! Central resource admission policy.
//!
//! `App` is the single writer for live session state. Callers must perform these
//! checks while handling that actor turn and immediately before allocation or
//! mutation; a successful check is therefore the reservation for that turn.

use crate::delegation::{DelegationId, Delegations};

pub(crate) const MAX_SESSION_PANES: usize = 256;
pub(crate) const MAX_WORKSPACE_PANES: usize = 128;
pub(crate) const MAX_TAB_PANES: usize = 32;
pub(crate) const MAX_GROUPS_PER_TAB: usize = 32;
pub(crate) const MAX_GROUP_MEMBERS: usize = 64;
pub(crate) const MAX_RELATIONSHIPS: usize = 4096;
pub(crate) const MAX_RELATIONSHIP_DEPTH: usize = 32;
pub(crate) const MAX_RELATIONSHIP_FAN_OUT: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AdmissionError {
    pub resource: &'static str,
    pub limit: usize,
}

impl std::fmt::Display for AdmissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} limit reached (maximum {})",
            self.resource, self.limit
        )
    }
}

fn admit(
    current: usize,
    additional: usize,
    limit: usize,
    resource: &'static str,
) -> Result<(), AdmissionError> {
    current
        .checked_add(additional)
        .filter(|next| *next <= limit)
        .map(|_| ())
        .ok_or(AdmissionError { resource, limit })
}

fn admit_one(current: usize, limit: usize, resource: &'static str) -> Result<(), AdmissionError> {
    admit(current, 1, limit, resource)
}

pub(crate) fn panes(
    session_panes: usize,
    workspace_panes: usize,
    tab_panes: usize,
    additional: usize,
) -> Result<(), AdmissionError> {
    admit(session_panes, additional, MAX_SESSION_PANES, "session pane")?;
    admit(
        workspace_panes,
        additional,
        MAX_WORKSPACE_PANES,
        "workspace pane",
    )?;
    admit(tab_panes, additional, MAX_TAB_PANES, "tab pane")
}

pub(crate) fn workspace_pane(
    workspace_panes: usize,
    tab_panes: usize,
) -> Result<(), AdmissionError> {
    admit_one(workspace_panes, MAX_WORKSPACE_PANES, "workspace pane")?;
    admit_one(tab_panes, MAX_TAB_PANES, "tab pane")
}

pub(crate) fn group(groups_in_tab: usize) -> Result<(), AdmissionError> {
    admit_one(groups_in_tab, MAX_GROUPS_PER_TAB, "terminal group")
}

pub(crate) fn group_member(members: usize) -> Result<(), AdmissionError> {
    admit_one(members, MAX_GROUP_MEMBERS, "terminal group member")
}

pub(crate) fn relationships(relationships: &Delegations) -> Result<(), AdmissionError> {
    if relationships.records().len() > MAX_RELATIONSHIPS {
        return Err(AdmissionError {
            resource: "relationship",
            limit: MAX_RELATIONSHIPS,
        });
    }
    for id in relationships.records().keys().copied() {
        if ancestor_count(relationships, id)? > MAX_RELATIONSHIP_DEPTH {
            return Err(AdmissionError {
                resource: "relationship depth",
                limit: MAX_RELATIONSHIP_DEPTH,
            });
        }
        let children = relationships
            .records()
            .values()
            .filter(|record| record.parent_id == Some(id))
            .count();
        if children > MAX_RELATIONSHIP_FAN_OUT {
            return Err(AdmissionError {
                resource: "relationship child fan-out",
                limit: MAX_RELATIONSHIP_FAN_OUT,
            });
        }
    }
    Ok(())
}

pub(crate) fn relationship_create(
    relationships: &Delegations,
    parent: Option<DelegationId>,
) -> Result<(), AdmissionError> {
    admit_one(
        relationships.records().len(),
        MAX_RELATIONSHIPS,
        "relationship",
    )?;
    relationship_parent(relationships, None, parent)
}

pub(crate) fn relationship_parent(
    relationships: &Delegations,
    moving: Option<DelegationId>,
    parent: Option<DelegationId>,
) -> Result<(), AdmissionError> {
    let Some(parent) = parent else {
        return Ok(());
    };
    let child_count = relationships
        .records()
        .iter()
        .filter(|(id, record)| Some(**id) != moving && record.parent_id == Some(parent))
        .count();
    admit_one(
        child_count,
        MAX_RELATIONSHIP_FAN_OUT,
        "relationship child fan-out",
    )?;

    let parent_depth = ancestor_count(relationships, parent)?;
    let subtree_depth = moving.map_or(0, |id| descendant_depth(relationships, id));
    let proposed_depth = parent_depth.saturating_add(1).saturating_add(subtree_depth);
    if proposed_depth > MAX_RELATIONSHIP_DEPTH {
        return Err(AdmissionError {
            resource: "relationship depth",
            limit: MAX_RELATIONSHIP_DEPTH,
        });
    }
    Ok(())
}

fn ancestor_count(
    relationships: &Delegations,
    mut id: DelegationId,
) -> Result<usize, AdmissionError> {
    let mut depth = 0usize;
    for _ in 0..=MAX_RELATIONSHIPS {
        let Some(record) = relationships.get(id) else {
            return Ok(depth);
        };
        let Some(parent) = record.parent_id else {
            return Ok(depth);
        };
        depth = depth.saturating_add(1);
        id = parent;
    }
    Err(AdmissionError {
        resource: "relationship depth",
        limit: MAX_RELATIONSHIP_DEPTH,
    })
}

fn descendant_depth(relationships: &Delegations, root: DelegationId) -> usize {
    let mut frontier = vec![(root, 0usize)];
    let mut maximum = 0usize;
    let mut visited = std::collections::HashSet::new();
    while let Some((id, depth)) = frontier.pop() {
        if !visited.insert(id) {
            continue;
        }
        maximum = maximum.max(depth);
        if depth >= MAX_RELATIONSHIP_DEPTH {
            continue;
        }
        frontier.extend(
            relationships
                .records()
                .iter()
                .filter_map(|(child, record)| {
                    (record.parent_id == Some(id)).then_some((*child, depth + 1))
                }),
        );
    }
    maximum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_admission_rejects_each_scope_at_the_boundary() {
        assert!(panes(
            MAX_SESSION_PANES - 1,
            MAX_WORKSPACE_PANES - 1,
            MAX_TAB_PANES - 1,
            1,
        )
        .is_ok());
        assert_eq!(
            panes(MAX_SESSION_PANES, 0, 0, 1).unwrap_err().resource,
            "session pane"
        );
        assert_eq!(
            panes(0, MAX_WORKSPACE_PANES, 0, 1).unwrap_err().resource,
            "workspace pane"
        );
        assert_eq!(
            panes(0, 0, MAX_TAB_PANES, 1).unwrap_err().resource,
            "tab pane"
        );
    }

    #[test]
    fn group_admission_rejects_before_id_or_member_allocation() {
        assert!(group(MAX_GROUPS_PER_TAB - 1).is_ok());
        assert_eq!(
            group(MAX_GROUPS_PER_TAB).unwrap_err().resource,
            "terminal group"
        );
        assert!(group_member(MAX_GROUP_MEMBERS - 1).is_ok());
        assert_eq!(
            group_member(MAX_GROUP_MEMBERS).unwrap_err().resource,
            "terminal group member"
        );
    }

    #[test]
    fn relationship_admission_bounds_depth_and_fan_out() {
        let mut relationships = Delegations::new();
        let root = relationships.create(None, None, None).unwrap();
        for _ in 0..MAX_RELATIONSHIP_FAN_OUT {
            relationships.create(None, Some(root), None).unwrap();
        }
        assert_eq!(
            relationship_create(&relationships, Some(root))
                .unwrap_err()
                .resource,
            "relationship child fan-out"
        );

        let mut relationships = Delegations::new();
        let mut parent = relationships.create(None, None, None).unwrap();
        for _ in 0..MAX_RELATIONSHIP_DEPTH {
            parent = relationships.create(None, Some(parent), None).unwrap();
        }
        assert_eq!(
            relationship_create(&relationships, Some(parent))
                .unwrap_err()
                .resource,
            "relationship depth"
        );
    }
}
