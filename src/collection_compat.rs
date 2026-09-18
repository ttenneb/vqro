//! Temporary compatibility policy for the legacy Collections feature.
//!
//! This state is deliberately separate from the neutral terminal-container
//! primitive. It remains the legacy oracle until package-owned policy is the
//! sole reader and writer for APIs, persistence, restore, documents, and
//! compatibility projections. Delete it only after the tracked Collections
//! authority-transition gates are satisfied.

use std::collections::HashSet;

use crate::layout::PaneId;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CollectionCompatibilityPolicy {
    pub(crate) label: Option<String>,
    archived: HashSet<PaneId>,
    /// Preserves legacy invalidation semantics: membership, ordering, and
    /// archive changes advance this value; selection and label changes do not.
    revision: u64,
}

impl CollectionCompatibilityPolicy {
    pub(crate) fn from_legacy(label: Option<String>, archived: HashSet<PaneId>) -> Self {
        Self {
            label,
            archived,
            revision: 0,
        }
    }

    pub(crate) fn is_archived(&self, pane: PaneId) -> bool {
        self.archived.contains(&pane)
    }

    pub(crate) fn archived(&self) -> &HashSet<PaneId> {
        &self.archived
    }

    pub(crate) fn archived_members<'a>(
        &'a self,
        members: &'a [PaneId],
    ) -> impl Iterator<Item = PaneId> + 'a {
        members
            .iter()
            .copied()
            .filter(|pane| self.archived.contains(pane))
    }

    pub(crate) fn remove_member(&mut self, pane: PaneId) {
        self.archived.remove(&pane);
        self.bump_revision();
    }

    pub(crate) fn set_archived(&mut self, pane: PaneId, archived: bool) -> bool {
        let changed = if archived {
            self.archived.insert(pane)
        } else {
            self.archived.remove(&pane)
        };
        if changed {
            self.bump_revision();
        }
        changed
    }

    pub(crate) fn bump_revision(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }

    #[cfg(test)]
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
}
