//! Private neutral placement state for terminals that share one layout leaf.
//!
//! Members are pane identities only because the current persisted session format
//! attaches one terminal to one pane. They are an internal migration aid and are
//! explicitly not stable persisted terminal identities or suitable for public
//! extension host calls. A later M2 gate must introduce stable terminal identity
//! before exposing container contracts outside the host.

use std::collections::HashSet;

use crate::layout::PaneId;

static NEXT_TERMINAL_CONTAINER_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TerminalContainerId(u64);

impl TerminalContainerId {
    pub(crate) fn alloc() -> Option<Self> {
        NEXT_TERMINAL_CONTAINER_ID
            .fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |current| (current > 0 && current < u64::MAX).then_some(current + 1),
            )
            .ok()
            .map(Self)
    }

    pub(crate) fn from_persisted(raw: u64) -> Option<Self> {
        let id = Self::parse(raw)?;
        let next = raw.checked_add(1)?;
        let mut current = NEXT_TERMINAL_CONTAINER_ID.load(std::sync::atomic::Ordering::Relaxed);
        while current <= raw {
            match NEXT_TERMINAL_CONTAINER_ID.compare_exchange_weak(
                current,
                next,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
        Some(id)
    }

    pub(crate) fn parse(raw: u64) -> Option<Self> {
        (raw > 0 && raw < u64::MAX).then_some(Self(raw))
    }

    pub(crate) fn from_validated(raw: u64) -> Self {
        debug_assert!(raw > 0 && raw < u64::MAX);
        Self(raw)
    }

    pub(crate) fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalContainer {
    pub(crate) id: TerminalContainerId,
    members: Vec<PaneId>,
    selected: Option<PaneId>,
}

impl TerminalContainer {
    pub(crate) fn empty(id: TerminalContainerId) -> Self {
        Self {
            id,
            members: Vec::new(),
            selected: None,
        }
    }

    pub(crate) fn from_legacy(
        id: TerminalContainerId,
        members: Vec<PaneId>,
        selected: Option<PaneId>,
    ) -> Option<Self> {
        let unique: HashSet<_> = members.iter().copied().collect();
        if unique.len() != members.len()
            || selected.is_some_and(|pane| !unique.contains(&pane))
            || (selected.is_none() && !members.is_empty())
        {
            return None;
        }
        Some(Self {
            id,
            members,
            selected,
        })
    }

    pub(crate) fn members(&self) -> &[PaneId] {
        &self.members
    }

    pub(crate) fn selected(&self) -> Option<PaneId> {
        self.selected
    }

    pub(crate) fn add(&mut self, pane: PaneId) {
        self.members.push(pane);
        self.selected.get_or_insert(pane);
    }

    pub(crate) fn remove(&mut self, pane: PaneId) -> bool {
        let Some(index) = self.members.iter().position(|member| *member == pane) else {
            return false;
        };
        self.members.remove(index);
        if self.selected == Some(pane) {
            self.selected = self
                .members
                .get(index)
                .or_else(|| index.checked_sub(1).and_then(|i| self.members.get(i)))
                .copied();
        }
        true
    }

    pub(crate) fn select(&mut self, pane: PaneId) -> bool {
        if !self.members.contains(&pane) {
            return false;
        }
        self.selected = Some(pane);
        true
    }

    pub(crate) fn reorder(&mut self, pane: PaneId, index: usize) -> bool {
        let Some(current) = self.members.iter().position(|member| *member == pane) else {
            return false;
        };
        if index >= self.members.len() {
            return false;
        }
        let pane = self.members.remove(current);
        self.members.insert(index, pane);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_container_selection_is_none_only_when_empty() {
        let id = TerminalContainerId::from_persisted(8_800_001).expect("container id");
        let first = PaneId::from_raw(1);
        let second = PaneId::from_raw(2);
        let mut container = TerminalContainer::empty(id);
        assert_eq!(container.selected(), None);

        container.add(first);
        container.add(second);
        assert_eq!(container.selected(), Some(first));
        assert!(container.select(second));
        assert!(container.remove(second));
        assert_eq!(container.selected(), Some(first));
        assert!(container.remove(first));
        assert_eq!(container.selected(), None);
    }

    #[test]
    fn legacy_container_requires_selection_if_and_only_if_non_empty() {
        let id = TerminalContainerId::from_persisted(8_800_003).expect("container id");
        let pane = PaneId::from_raw(1);

        assert!(TerminalContainer::from_legacy(id, vec![pane], Some(pane)).is_some());
        assert!(TerminalContainer::from_legacy(id, Vec::new(), None).is_some());
        assert!(TerminalContainer::from_legacy(id, vec![pane], None).is_none());
        assert!(TerminalContainer::from_legacy(id, Vec::new(), Some(pane)).is_none());
    }

    #[test]
    fn terminal_container_has_no_collection_policy_state() {
        let container = TerminalContainer::empty(
            TerminalContainerId::from_persisted(8_800_002).expect("container id"),
        );
        let TerminalContainer {
            id,
            members,
            selected,
        } = container;
        assert_eq!(id.raw(), 8_800_002);
        assert!(members.is_empty());
        assert_eq!(selected, None);
    }
}
