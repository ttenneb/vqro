//! Neutral placement state for terminals that share one layout leaf.
//!
//! The canonical textual container identity is exposed by the public `host.terminals.v1`
//! snapshot contract. Container members remain pane-backed private state because the current
//! persisted session format attaches one terminal to one pane; pane identities are not stable
//! terminal references and must not cross public extension host boundaries.

use std::collections::HashSet;

use crate::layout::PaneId;

static NEXT_TERMINAL_CONTAINER_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

const TERMINAL_CONTAINER_ID_PREFIX: &str = "container_";
pub(crate) const TERMINAL_CONTAINER_ID_TEXT_BYTES: usize = TERMINAL_CONTAINER_ID_PREFIX.len() + 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TerminalContainerId(u64);

pub(crate) fn format_terminal_container_id(id: TerminalContainerId) -> String {
    format!("{TERMINAL_CONTAINER_ID_PREFIX}{:016x}", id.raw())
}

/// Parses only the frozen public wire shape without allocating or advancing the ID allocator.
///
/// Compatibility validation intentionally accepts the reserved all-zero and all-ones encodings
/// that the original `host.terminals.v1` syntax validator accepted. New internal authorities must
/// use `parse_terminal_container_id_text` instead.
pub(crate) fn parse_terminal_container_wire_text(value: &str) -> Option<u64> {
    let encoded = value.strip_prefix(TERMINAL_CONTAINER_ID_PREFIX)?;
    if value.len() != TERMINAL_CONTAINER_ID_TEXT_BYTES
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    u64::from_str_radix(encoded, 16).ok()
}

/// Parses a canonical textual identity and rejects values reserved outside the valid ID domain.
pub(crate) fn parse_terminal_container_id_text(value: &str) -> Option<TerminalContainerId> {
    TerminalContainerId::parse(parse_terminal_container_wire_text(value)?)
}

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
    fn canonical_text_identity_round_trips_without_advancing_allocation() {
        let id = TerminalContainerId::parse(42).unwrap();
        let encoded = format_terminal_container_id(id);
        assert_eq!(encoded, "container_000000000000002a");
        assert_eq!(parse_terminal_container_wire_text(&encoded), Some(42));
        assert_eq!(parse_terminal_container_id_text(&encoded), Some(id));
        for invalid in [
            "container_2a",
            "container_000000000000002A",
            "collection_000000000000002a",
            "container_00000000000000001",
        ] {
            assert_eq!(parse_terminal_container_wire_text(invalid), None);
            assert_eq!(parse_terminal_container_id_text(invalid), None);
        }
    }

    #[test]
    fn wire_shape_retains_reserved_values_but_valid_identity_parser_rejects_them() {
        for (encoded, raw) in [
            ("container_0000000000000000", 0),
            ("container_ffffffffffffffff", u64::MAX),
        ] {
            assert_eq!(parse_terminal_container_wire_text(encoded), Some(raw));
            assert_eq!(parse_terminal_container_id_text(encoded), None);
        }
    }

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
