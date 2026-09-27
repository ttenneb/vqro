use std::fmt;

const TAB_ID_PREFIX: &str = "tab_";
const TAB_ID_RANDOM_BYTES: usize = 16;
const TAB_ID_ALLOCATION_ATTEMPTS: usize = 32;
pub(crate) const MAX_TAB_ID_BYTES: usize = TAB_ID_PREFIX.len() + TAB_ID_RANDOM_BYTES * 2;

/// Stable opaque identity for a server-owned tab.
///
/// This identity is independent from the generation-1 public
/// `workspace:t<number>` handle, display order, and label.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct TabId(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InvalidTabId;

impl TabId {
    pub(crate) fn alloc() -> std::io::Result<Self> {
        let mut random = [0_u8; TAB_ID_RANDOM_BYTES];
        crate::platform::fill_secure_random(&mut random)?;
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(MAX_TAB_ID_BYTES);
        encoded.push_str(TAB_ID_PREFIX);
        for byte in random {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        Ok(Self(encoded))
    }

    pub(crate) fn alloc_unique_with<F>(
        occupied: &std::collections::HashSet<Self>,
        mut generate: F,
    ) -> std::io::Result<Self>
    where
        F: FnMut() -> std::io::Result<Self>,
    {
        for _ in 0..TAB_ID_ALLOCATION_ATTEMPTS {
            let candidate = generate()?;
            if !occupied.contains(&candidate) {
                return Ok(candidate);
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "failed to allocate a unique tab identity after bounded retries",
        ))
    }

    pub(crate) fn parse(raw: &str) -> Result<Self, InvalidTabId> {
        let Some(hex) = raw.strip_prefix(TAB_ID_PREFIX) else {
            return Err(InvalidTabId);
        };
        if raw.len() != MAX_TAB_ID_BYTES
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(InvalidTabId);
        }
        Ok(Self(raw.to_owned()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> serde::Deserialize<'de> for TabId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        let raw = <String as serde::Deserialize>::deserialize(deserializer)?;
        TabId::parse(raw.as_str()).map_err(D::Error::custom)
    }
}

impl fmt::Display for TabId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl fmt::Display for InvalidTabId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid tab identity")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocated_tab_ids_are_canonical_and_unique() {
        let first = TabId::alloc().unwrap();
        let second = TabId::alloc().unwrap();
        assert_ne!(first, second);
        assert_eq!(TabId::parse(first.as_str()), Ok(first));
    }

    #[test]
    fn unique_allocation_retries_and_is_bounded() {
        let occupied_id = TabId::parse("tab_00000000000000000000000000000001").unwrap();
        let replacement = TabId::parse("tab_00000000000000000000000000000002").unwrap();
        let occupied = std::collections::HashSet::from([occupied_id.clone()]);
        let mut candidates = [occupied_id.clone(), replacement.clone()].into_iter();
        assert_eq!(
            TabId::alloc_unique_with(&occupied, || Ok(candidates.next().unwrap())).unwrap(),
            replacement
        );

        let mut attempts = 0;
        let error = TabId::alloc_unique_with(&occupied, || {
            attempts += 1;
            Ok(occupied_id.clone())
        })
        .unwrap_err();
        assert_eq!(attempts, TAB_ID_ALLOCATION_ATTEMPTS);
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn parsing_rejects_noncanonical_tab_ids() {
        for invalid in [
            "",
            "tab_1",
            "term_00000000000000000000000000000000",
            "tab_0000000000000000000000000000000G",
            "tab_000000000000000000000000000000000",
        ] {
            assert!(TabId::parse(invalid).is_err(), "accepted {invalid}");
        }
    }
}
