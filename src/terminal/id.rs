use std::fmt;

const TERMINAL_ID_PREFIX: &str = "term_";
const TERMINAL_ID_RANDOM_BYTES: usize = 16;
const TERMINAL_ID_ALLOCATION_ATTEMPTS: usize = 32;
pub(crate) const MAX_TERMINAL_ID_BYTES: usize =
    TERMINAL_ID_PREFIX.len() + TERMINAL_ID_RANDOM_BYTES * 2;

/// Stable opaque identity for a server-owned logical terminal.
///
/// A terminal may move between panes or survive a server handoff without
/// changing this identity. Pane and layout identities must never be used as a
/// substitute at public runtime boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct TerminalId(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InvalidTerminalId;

impl TerminalId {
    pub(crate) fn alloc() -> std::io::Result<Self> {
        let mut random = [0_u8; TERMINAL_ID_RANDOM_BYTES];
        crate::platform::fill_secure_random(&mut random)?;
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(MAX_TERMINAL_ID_BYTES);
        encoded.push_str(TERMINAL_ID_PREFIX);
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
        for _ in 0..TERMINAL_ID_ALLOCATION_ATTEMPTS {
            let candidate = generate()?;
            if !occupied.contains(&candidate) {
                return Ok(candidate);
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "failed to allocate a unique terminal identity after bounded retries",
        ))
    }

    pub(crate) fn parse(raw: &str) -> Result<Self, InvalidTerminalId> {
        let Some(hex) = raw.strip_prefix(TERMINAL_ID_PREFIX) else {
            return Err(InvalidTerminalId);
        };
        if raw.len() != MAX_TERMINAL_ID_BYTES
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(InvalidTerminalId);
        }
        Ok(Self(raw.to_owned()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> serde::Deserialize<'de> for TerminalId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        let raw = String::deserialize(deserializer)?;
        TerminalId::parse(raw.as_str()).map_err(D::Error::custom)
    }
}

impl fmt::Display for TerminalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for InvalidTerminalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid terminal identity")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocated_terminal_ids_are_canonical_and_unique() {
        let first = TerminalId::alloc().unwrap();
        let second = TerminalId::alloc().unwrap();
        assert_ne!(first, second);
        assert_eq!(TerminalId::parse(first.as_str()), Ok(first));
    }

    #[test]
    fn unique_allocation_retries_collisions_and_propagates_failures() {
        let occupied_id = TerminalId::parse("term_00000000000000000000000000000001").unwrap();
        let replacement = TerminalId::parse("term_00000000000000000000000000000002").unwrap();
        let occupied = std::collections::HashSet::from([occupied_id.clone()]);
        let mut candidates = [occupied_id, replacement.clone()].into_iter();
        assert_eq!(
            TerminalId::alloc_unique_with(&occupied, || Ok(candidates.next().unwrap())).unwrap(),
            replacement
        );

        let error = TerminalId::alloc_unique_with(&occupied, || {
            Err(std::io::Error::other("entropy unavailable"))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "entropy unavailable");
    }

    #[test]
    fn unique_allocation_has_a_bounded_collision_retry_limit() {
        let occupied_id = TerminalId::parse("term_00000000000000000000000000000001").unwrap();
        let occupied = std::collections::HashSet::from([occupied_id.clone()]);
        let mut attempts = 0;
        let error = TerminalId::alloc_unique_with(&occupied, || {
            attempts += 1;
            Ok(occupied_id.clone())
        })
        .unwrap_err();
        assert_eq!(attempts, TERMINAL_ID_ALLOCATION_ATTEMPTS);
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn parsing_rejects_unbounded_or_noncanonical_terminal_ids() {
        for invalid in [
            "",
            "term_1",
            "pane_00000000000000000000000000000000",
            "term_0000000000000000000000000000000G",
            "term_000000000000000000000000000000000",
        ] {
            assert!(TerminalId::parse(invalid).is_err(), "accepted {invalid}");
        }
    }
}
