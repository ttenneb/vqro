//! Stable box identity; membership and selection live in the layout tree.

#[cfg(test)]
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
        let root = crate::layout::TypedNode::empty_stack(id);
        let mut layout =
            crate::layout::TileLayout::from_tree(root, crate::layout::LayoutLeaf::Stack(id))
                .unwrap();
        assert_eq!(layout.collection(id).unwrap().selected(), None);

        assert!(layout.add_collection_member(id, first));
        assert!(layout.add_collection_member(id, second));
        assert_eq!(layout.collection(id).unwrap().selected(), Some(first));
        assert!(layout.select_collection_member(id, second));
        assert!(layout.remove_collection_member(id, second));
        assert_eq!(layout.collection(id).unwrap().selected(), Some(first));
        assert!(layout.remove_collection_member(id, first));
        assert_eq!(layout.collection(id).unwrap().selected(), None);
    }
}
