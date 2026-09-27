//! Read-only label/archive snapshot decoded from pre-package session formats.
//!
//! Production code may inspect this only while building the one-way package-state import. It is
//! never a post-R4 policy authority or writer. Test-only mutation builds historical fixtures.

use std::collections::HashSet;

use crate::layout::PaneId;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LegacyCollectionSnapshot {
    pub(crate) label: Option<String>,
    archived: HashSet<PaneId>,
}

impl LegacyCollectionSnapshot {
    pub(crate) fn from_legacy(label: Option<String>, archived: HashSet<PaneId>) -> Self {
        Self { label, archived }
    }

    pub(crate) fn is_archived(&self, pane: PaneId) -> bool {
        self.archived.contains(&pane)
    }

    pub(crate) fn archived(&self) -> &HashSet<PaneId> {
        &self.archived
    }

    #[cfg(test)]
    pub(crate) fn set_archived(&mut self, pane: PaneId, archived: bool) -> bool {
        if archived {
            self.archived.insert(pane)
        } else {
            self.archived.remove(&pane)
        }
    }
}
