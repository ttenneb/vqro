//! Pure internal model for prospective Collections label/archive policy.
//!
//! This staged module has no application, persistence, API, provider, or rendering integration.
//! Neutral terminal containers remain authoritative for identity, membership, order, selection,
//! and placement.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::terminal::TerminalId;
use crate::terminal_container::TerminalContainerId;

const POLICY_SCHEMA: &str = "vqro.collections.policy";
const POLICY_SCHEMA_VERSION: u32 = 1;
const MAX_POLICY_LABEL_BYTES: usize = 4 * 1024;
const MAX_ARCHIVED_TERMINALS: usize = crate::admission::MAX_GROUP_MEMBERS;
const MAX_NEUTRAL_TERMINALS: usize = crate::admission::MAX_GROUP_MEMBERS;
const MEMBERSHIP_ARCHIVE_INVALIDATION_DOMAIN: &[u8] = b"vqro.collections.membership-archive.v1\0";

// Crate-private policy types staged before a production package-authority path exists.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CollectionsPolicyRecordV1 {
    schema: String,
    schema_version: u32,
    container_id: TerminalContainerId,
    label: Option<String>,
    archived_terminal_ids: Vec<TerminalId>,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LegacyCollectionsPolicyInput {
    pub(crate) label: Option<String>,
    pub(crate) archived_terminal_ids: Vec<TerminalId>,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReconciledCollectionsPolicyV1 {
    pub(crate) label: Option<String>,
    pub(crate) terminals: Vec<ReconciledTerminalPolicyV1>,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReconciledTerminalPolicyV1 {
    pub(crate) terminal_id: TerminalId,
    pub(crate) archived: bool,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CollectionsPolicyError {
    InvalidValue,
    InvalidSchema,
    InvalidContainerKey,
    InvalidContainerValue,
    KeyValueContainerMismatch,
    ContainerMismatch,
    LabelTooLarge,
    TooManyArchivedTerminals,
    DuplicateArchivedTerminal,
    TooManyNeutralTerminals,
    DuplicateNeutralTerminal,
    ValueTooLarge,
    TooManyNamespaceRecords,
    NamespaceTooLarge,
}

#[derive(Serialize)]
struct PolicyValueRef<'a> {
    schema: &'a str,
    schema_version: u32,
    container_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<&'a str>,
    archived_terminal_ids: &'a [TerminalId],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyValueOwned {
    schema: String,
    schema_version: u32,
    container_id: String,
    #[serde(default)]
    label: Option<String>,
    archived_terminal_ids: Vec<TerminalId>,
}

// These methods remain unused until the package-owned policy authority is wired.
#[cfg_attr(not(test), allow(dead_code))]
impl CollectionsPolicyRecordV1 {
    pub(crate) fn new(
        container_id: TerminalContainerId,
        label: Option<String>,
        mut archived_terminal_ids: Vec<TerminalId>,
    ) -> Result<Self, CollectionsPolicyError> {
        archived_terminal_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        let record = Self {
            schema: POLICY_SCHEMA.into(),
            schema_version: POLICY_SCHEMA_VERSION,
            container_id,
            label,
            archived_terminal_ids,
        };
        record.validate()?;
        Ok(record)
    }

    pub(crate) fn from_legacy(
        container_id: TerminalContainerId,
        legacy: LegacyCollectionsPolicyInput,
    ) -> Result<Self, CollectionsPolicyError> {
        Self::new(container_id, legacy.label, legacy.archived_terminal_ids)
    }

    pub(crate) fn validate(&self) -> Result<(), CollectionsPolicyError> {
        if self.schema != POLICY_SCHEMA || self.schema_version != POLICY_SCHEMA_VERSION {
            return Err(CollectionsPolicyError::InvalidSchema);
        }
        if self
            .label
            .as_ref()
            .is_some_and(|label| label.len() > MAX_POLICY_LABEL_BYTES)
        {
            return Err(CollectionsPolicyError::LabelTooLarge);
        }
        if self.archived_terminal_ids.len() > MAX_ARCHIVED_TERMINALS {
            return Err(CollectionsPolicyError::TooManyArchivedTerminals);
        }
        if self
            .archived_terminal_ids
            .windows(2)
            .any(|pair| pair[0].as_str() >= pair[1].as_str())
        {
            return Err(CollectionsPolicyError::DuplicateArchivedTerminal);
        }
        Ok(())
    }

    pub(crate) fn schema(&self) -> &str {
        &self.schema
    }

    pub(crate) fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub(crate) fn container_id(&self) -> TerminalContainerId {
        self.container_id
    }

    pub(crate) fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Returns the complete canonical policy, including references stale against current neutral
    /// membership. Reconciliation never deletes those references.
    pub(crate) fn archived_terminal_ids(&self) -> &[TerminalId] {
        &self.archived_terminal_ids
    }
}

// Staged codec entry points; no production package-state caller is connected yet.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn policy_key(container_id: TerminalContainerId) -> String {
    crate::terminal_container::format_terminal_container_id(container_id)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn container_id_from_policy_key(
    key: &str,
) -> Result<TerminalContainerId, CollectionsPolicyError> {
    crate::terminal_container::parse_terminal_container_id_text(key)
        .ok_or(CollectionsPolicyError::InvalidContainerKey)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn encode_policy_value(
    record: &CollectionsPolicyRecordV1,
) -> Result<serde_json::Value, CollectionsPolicyError> {
    record.validate()?;
    let value = serde_json::to_value(PolicyValueRef {
        schema: &record.schema,
        schema_version: record.schema_version,
        container_id: policy_key(record.container_id),
        label: record.label.as_deref(),
        archived_terminal_ids: &record.archived_terminal_ids,
    })
    .map_err(|_| CollectionsPolicyError::InvalidValue)?;
    ensure_value_size(&value)?;
    Ok(value)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn decode_policy_value(
    key: &str,
    value: &serde_json::Value,
) -> Result<CollectionsPolicyRecordV1, CollectionsPolicyError> {
    ensure_value_size(value)?;
    let key_id = container_id_from_policy_key(key)?;
    let decoded: PolicyValueOwned =
        serde_json::from_value(value.clone()).map_err(|_| CollectionsPolicyError::InvalidValue)?;
    if decoded.schema != POLICY_SCHEMA || decoded.schema_version != POLICY_SCHEMA_VERSION {
        return Err(CollectionsPolicyError::InvalidSchema);
    }
    let value_id =
        crate::terminal_container::parse_terminal_container_id_text(&decoded.container_id)
            .ok_or(CollectionsPolicyError::InvalidContainerValue)?;
    if value_id != key_id {
        return Err(CollectionsPolicyError::KeyValueContainerMismatch);
    }
    let record =
        CollectionsPolicyRecordV1::new(value_id, decoded.label, decoded.archived_terminal_ids)?;
    if &encode_policy_value(&record)? != value {
        return Err(CollectionsPolicyError::InvalidValue);
    }
    Ok(record)
}

/// Validates the exact compact JSON footprint that this namespace would contribute to generic
/// state. Capacity is aggregate and byte-based rather than an unsafe promise that every possible
/// live neutral container can simultaneously carry maximum policy.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn validate_policy_namespace_capacity<'a>(
    records: impl IntoIterator<Item = &'a CollectionsPolicyRecordV1>,
) -> Result<usize, CollectionsPolicyError> {
    let mut values = BTreeMap::new();
    for record in records {
        let key = policy_key(record.container_id);
        if values.len() >= crate::extension_state::MAX_STATE_KEYS_PER_NAMESPACE {
            return Err(CollectionsPolicyError::TooManyNamespaceRecords);
        }
        if values.insert(key, encode_policy_value(record)?).is_some() {
            return Err(CollectionsPolicyError::KeyValueContainerMismatch);
        }
    }
    let encoded = serde_json::to_vec(&values).map_err(|_| CollectionsPolicyError::InvalidValue)?;
    if encoded.len() > crate::extension_state::MAX_STATE_NAMESPACE_BYTES {
        return Err(CollectionsPolicyError::NamespaceTooLarge);
    }
    Ok(encoded.len())
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn reconcile_policy(
    container_id: TerminalContainerId,
    neutral_order: &[TerminalId],
    record: Option<&CollectionsPolicyRecordV1>,
) -> Result<ReconciledCollectionsPolicyV1, CollectionsPolicyError> {
    validate_neutral_order(neutral_order)?;
    if let Some(record) = record {
        record.validate()?;
        if record.container_id != container_id {
            return Err(CollectionsPolicyError::ContainerMismatch);
        }
    }

    let archived = record
        .map(|record| record.archived_terminal_ids.iter().collect::<HashSet<_>>())
        .unwrap_or_default();
    Ok(ReconciledCollectionsPolicyV1 {
        label: record.and_then(|record| record.label.clone()),
        terminals: neutral_order
            .iter()
            .cloned()
            .map(|terminal_id| ReconciledTerminalPolicyV1 {
                archived: archived.contains(&terminal_id),
                terminal_id,
            })
            .collect(),
    })
}

/// Fingerprints only canonical neutral membership/order and effective archive policy.
///
/// Labels, selection, placement, stale archive references, and archive storage order are excluded.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn membership_archive_invalidation_fingerprint(
    container_id: TerminalContainerId,
    neutral_order: &[TerminalId],
    record: Option<&CollectionsPolicyRecordV1>,
) -> Result<String, CollectionsPolicyError> {
    let projection = reconcile_policy(container_id, neutral_order, record)?;
    let mut digest = Sha256::new();
    digest.update(MEMBERSHIP_ARCHIVE_INVALIDATION_DOMAIN);
    digest.update(container_id.raw().to_be_bytes());
    digest.update((projection.terminals.len() as u64).to_be_bytes());
    for terminal in projection.terminals {
        let encoded = terminal.terminal_id.as_str().as_bytes();
        digest.update((encoded.len() as u64).to_be_bytes());
        digest.update(encoded);
        digest.update([u8::from(terminal.archived)]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn ensure_value_size(value: &serde_json::Value) -> Result<usize, CollectionsPolicyError> {
    let encoded = serde_json::to_vec(value).map_err(|_| CollectionsPolicyError::InvalidValue)?;
    if encoded.len() > crate::extension_state::MAX_STATE_VALUE_BYTES {
        return Err(CollectionsPolicyError::ValueTooLarge);
    }
    Ok(encoded.len())
}

fn validate_neutral_order(neutral_order: &[TerminalId]) -> Result<(), CollectionsPolicyError> {
    if neutral_order.len() > MAX_NEUTRAL_TERMINALS {
        return Err(CollectionsPolicyError::TooManyNeutralTerminals);
    }
    let unique = neutral_order.iter().collect::<HashSet<_>>();
    if unique.len() != neutral_order.len() {
        return Err(CollectionsPolicyError::DuplicateNeutralTerminal);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(raw: u64) -> TerminalContainerId {
        TerminalContainerId::parse(raw).expect("valid container ID")
    }

    fn terminal(raw: u64) -> TerminalId {
        TerminalId::parse(&format!("term_{raw:032x}")).expect("valid terminal ID")
    }

    fn record(
        container_raw: u64,
        label: Option<String>,
        archived: impl IntoIterator<Item = u64>,
    ) -> CollectionsPolicyRecordV1 {
        CollectionsPolicyRecordV1::new(
            container(container_raw),
            label,
            archived.into_iter().map(terminal).collect(),
        )
        .unwrap()
    }

    #[test]
    fn record_owns_only_label_and_canonical_archived_stable_terminal_ids() {
        let record = record(41, Some("Review".into()), [3, 1, 2]);
        assert_eq!(record.schema(), POLICY_SCHEMA);
        assert_eq!(record.schema_version(), 1);
        assert_eq!(record.container_id(), container(41));
        assert_eq!(record.label(), Some("Review"));
        assert_eq!(
            record.archived_terminal_ids(),
            &[terminal(1), terminal(2), terminal(3)]
        );
    }

    #[test]
    fn state_key_is_exact_public_host_terminals_container_identity() {
        let id = container(0x2a);
        assert_eq!(policy_key(id), "container_000000000000002a");
        assert_eq!(container_id_from_policy_key(&policy_key(id)), Ok(id));
        for invalid in [
            "",
            "container:000000000000002a",
            "container_2a",
            "container_000000000000002A",
            "container_0000000000000000",
            "container_ffffffffffffffff",
            "container_00000000000000001",
        ] {
            assert_eq!(
                container_id_from_policy_key(invalid),
                Err(CollectionsPolicyError::InvalidContainerKey),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn codec_is_canonical_strict_and_binds_key_to_value_identity() {
        let first = record(7, Some("Legacy".into()), [3, 1, 2]);
        let permuted = record(7, Some("Legacy".into()), [2, 3, 1]);
        let first_value = encode_policy_value(&first).unwrap();
        let permuted_value = encode_policy_value(&permuted).unwrap();
        assert_eq!(first_value, permuted_value);
        assert_eq!(
            serde_json::to_vec(&first_value).unwrap(),
            serde_json::to_vec(&permuted_value).unwrap()
        );
        assert_eq!(
            decode_policy_value(&policy_key(container(7)), &first_value).unwrap(),
            first
        );

        let mut unsorted = first_value.clone();
        unsorted["archived_terminal_ids"] =
            serde_json::json!([terminal(3), terminal(1), terminal(2),]);
        let before = unsorted.clone();
        assert_eq!(
            decode_policy_value(&policy_key(container(7)), &unsorted),
            Err(CollectionsPolicyError::InvalidValue)
        );
        assert_eq!(unsorted, before);

        let mut duplicate = first_value.clone();
        duplicate["archived_terminal_ids"] = serde_json::json!([terminal(1), terminal(1),]);
        let before = duplicate.clone();
        assert_eq!(
            decode_policy_value(&policy_key(container(7)), &duplicate),
            Err(CollectionsPolicyError::DuplicateArchivedTerminal)
        );
        assert_eq!(duplicate, before);

        let without_label = encode_policy_value(&record(7, None, [1])).unwrap();
        let mut explicit_null = without_label.clone();
        explicit_null["label"] = serde_json::Value::Null;
        let before = explicit_null.clone();
        assert_eq!(
            decode_policy_value(&policy_key(container(7)), &explicit_null),
            Err(CollectionsPolicyError::InvalidValue)
        );
        assert_eq!(explicit_null, before);

        let mut unknown = first_value.clone();
        unknown["membership"] = serde_json::json!([]);
        assert_eq!(
            decode_policy_value(&policy_key(container(7)), &unknown),
            Err(CollectionsPolicyError::InvalidValue)
        );

        let mut wrong_schema = first_value.clone();
        wrong_schema["schema"] = serde_json::json!("vqro.collections.policy.v2");
        assert_eq!(
            decode_policy_value(&policy_key(container(7)), &wrong_schema),
            Err(CollectionsPolicyError::InvalidSchema)
        );
        let mut wrong_version = first_value.clone();
        wrong_version["schema_version"] = serde_json::json!(2);
        assert_eq!(
            decode_policy_value(&policy_key(container(7)), &wrong_version),
            Err(CollectionsPolicyError::InvalidSchema)
        );
        assert_eq!(
            decode_policy_value(&policy_key(container(8)), &first_value),
            Err(CollectionsPolicyError::KeyValueContainerMismatch)
        );
        let mut malformed_id = first_value.clone();
        malformed_id["container_id"] = serde_json::json!("container_7");
        assert_eq!(
            decode_policy_value(&policy_key(container(7)), &malformed_id),
            Err(CollectionsPolicyError::InvalidContainerValue)
        );
    }

    #[test]
    fn rejected_values_are_unchanged_and_enforce_exact_generic_value_size() {
        let exact_empty_len = serde_json::to_vec(&serde_json::json!({"x": ""}))
            .unwrap()
            .len();
        let exact = serde_json::json!({
            "x": "x".repeat(crate::extension_state::MAX_STATE_VALUE_BYTES - exact_empty_len)
        });
        assert_eq!(
            serde_json::to_vec(&exact).unwrap().len(),
            crate::extension_state::MAX_STATE_VALUE_BYTES
        );
        let before = exact.clone();
        assert_eq!(
            decode_policy_value(&policy_key(container(1)), &exact),
            Err(CollectionsPolicyError::InvalidValue),
            "a value exactly at the generic limit must pass size admission before shape rejection"
        );
        assert_eq!(exact, before);

        let oversized = serde_json::json!({
            "x": "x".repeat(
                crate::extension_state::MAX_STATE_VALUE_BYTES - exact_empty_len + 1
            )
        });
        let before = oversized.clone();
        assert_eq!(
            decode_policy_value(&policy_key(container(1)), &oversized),
            Err(CollectionsPolicyError::ValueTooLarge)
        );
        assert_eq!(oversized, before);
    }

    #[test]
    fn validation_bounds_schema_label_archive_count_and_duplicates() {
        let mut wrong_schema = record(1, None, []);
        wrong_schema.schema_version += 1;
        assert_eq!(
            wrong_schema.validate(),
            Err(CollectionsPolicyError::InvalidSchema)
        );
        assert_eq!(
            CollectionsPolicyRecordV1::new(
                container(1),
                Some("x".repeat(MAX_POLICY_LABEL_BYTES + 1)),
                Vec::new(),
            ),
            Err(CollectionsPolicyError::LabelTooLarge)
        );
        assert_eq!(
            CollectionsPolicyRecordV1::new(
                container(1),
                None,
                (1..=(MAX_ARCHIVED_TERMINALS + 1) as u64)
                    .map(terminal)
                    .collect(),
            ),
            Err(CollectionsPolicyError::TooManyArchivedTerminals)
        );
        let duplicate = terminal(1);
        assert_eq!(
            CollectionsPolicyRecordV1::new(container(1), None, vec![duplicate.clone(), duplicate],),
            Err(CollectionsPolicyError::DuplicateArchivedTerminal)
        );
    }

    #[test]
    fn legacy_conversion_has_only_label_and_stable_archive_inputs() {
        let archived = terminal(3);
        let record = CollectionsPolicyRecordV1::from_legacy(
            container(7),
            LegacyCollectionsPolicyInput {
                label: Some("Legacy".into()),
                archived_terminal_ids: vec![archived.clone(), terminal(1), terminal(2)],
            },
        )
        .unwrap();
        let permuted = CollectionsPolicyRecordV1::from_legacy(
            container(7),
            LegacyCollectionsPolicyInput {
                label: Some("Legacy".into()),
                archived_terminal_ids: vec![terminal(2), archived, terminal(1)],
            },
        )
        .unwrap();
        assert_eq!(record.container_id(), container(7));
        assert_eq!(record.label(), Some("Legacy"));
        assert_eq!(
            record.archived_terminal_ids(),
            &[terminal(1), terminal(2), terminal(3)]
        );
        assert_eq!(record, permuted);
        assert_eq!(
            encode_policy_value(&record).unwrap(),
            encode_policy_value(&permuted).unwrap()
        );
    }

    #[test]
    fn reconciliation_preserves_neutral_order_and_retains_stale_archives() {
        let second = terminal(2);
        let first = terminal(1);
        let stale = terminal(3);
        let record = CollectionsPolicyRecordV1::new(
            container(9),
            Some("Helpers".into()),
            vec![second.clone(), stale.clone()],
        )
        .unwrap();
        let before = record.clone();
        let projection = reconcile_policy(
            container(9),
            &[second.clone(), first.clone()],
            Some(&record),
        )
        .unwrap();
        assert_eq!(projection.label.as_deref(), Some("Helpers"));
        assert_eq!(
            projection.terminals,
            vec![
                ReconciledTerminalPolicyV1 {
                    terminal_id: second,
                    archived: true,
                },
                ReconciledTerminalPolicyV1 {
                    terminal_id: first,
                    archived: false,
                },
            ]
        );
        assert_eq!(record, before);
        assert!(record.archived_terminal_ids().contains(&stale));
    }

    #[test]
    fn reconciliation_defaults_and_bounds_neutral_membership() {
        let order = vec![terminal(2), terminal(1)];
        let projection = reconcile_policy(container(10), &order, None).unwrap();
        assert_eq!(projection.label, None);
        assert_eq!(
            projection
                .terminals
                .iter()
                .map(|terminal| (&terminal.terminal_id, terminal.archived))
                .collect::<Vec<_>>(),
            vec![(&order[0], false), (&order[1], false)]
        );

        let duplicate = terminal(1);
        assert_eq!(
            reconcile_policy(container(10), &[duplicate.clone(), duplicate], None,),
            Err(CollectionsPolicyError::DuplicateNeutralTerminal)
        );
        let over_limit = (1..=(MAX_NEUTRAL_TERMINALS + 1) as u64)
            .map(terminal)
            .collect::<Vec<_>>();
        assert_eq!(
            reconcile_policy(container(10), &over_limit, None),
            Err(CollectionsPolicyError::TooManyNeutralTerminals)
        );
        assert_eq!(
            reconcile_policy(container(11), &[], Some(&record(10, None, []))),
            Err(CollectionsPolicyError::ContainerMismatch)
        );
    }

    #[test]
    fn namespace_capacity_is_exact_and_rejects_aggregate_overflow() {
        let maximum_record = |raw| {
            record(
                raw,
                Some("x".repeat(MAX_POLICY_LABEL_BYTES)),
                1..=MAX_ARCHIVED_TERMINALS as u64,
            )
        };
        let mut accepted = Vec::new();
        for raw in 1..=crate::extension_state::MAX_STATE_KEYS_PER_NAMESPACE as u64 {
            let candidate = maximum_record(raw);
            let mut next = accepted.clone();
            next.push(candidate);
            match validate_policy_namespace_capacity(&next) {
                Ok(bytes) => {
                    assert!(bytes <= crate::extension_state::MAX_STATE_NAMESPACE_BYTES);
                    accepted = next;
                }
                Err(CollectionsPolicyError::NamespaceTooLarge) => break,
                other => panic!("unexpected capacity result: {other:?}"),
            }
        }
        assert!(!accepted.is_empty());
        let accepted_bytes = validate_policy_namespace_capacity(&accepted).unwrap();
        assert!(accepted_bytes <= crate::extension_state::MAX_STATE_NAMESPACE_BYTES);
        let mut overflow = accepted;
        overflow.push(maximum_record(overflow.len() as u64 + 1));
        assert_eq!(
            validate_policy_namespace_capacity(&overflow),
            Err(CollectionsPolicyError::NamespaceTooLarge)
        );
    }

    #[test]
    fn membership_archive_fingerprint_has_pinned_vectors_and_exact_inputs() {
        let id = container(11);
        let first = terminal(1);
        let second = terminal(2);
        let order = vec![first.clone(), second.clone()];
        let archived_first = record(11, Some("ignored label".into()), [1]);
        let archived_first_permuted_storage = CollectionsPolicyRecordV1::new(
            id,
            Some("other ignored label".into()),
            vec![terminal(99), first.clone()],
        )
        .unwrap();

        let fingerprint =
            membership_archive_invalidation_fingerprint(id, &order, Some(&archived_first)).unwrap();
        assert_eq!(
            fingerprint,
            "ac59e6088e844f5e61bb7e5e051a5447d80f9c0360e75043097143fba59bef08"
        );
        assert_eq!(
            membership_archive_invalidation_fingerprint(id, &order, None).unwrap(),
            "43fb7e566ba50559fc38cc635ad2aca95487208e2e94fe0094e4c546d0bd3dbc"
        );
        assert_eq!(
            membership_archive_invalidation_fingerprint(
                id,
                &[second.clone(), first.clone()],
                Some(&archived_first),
            )
            .unwrap(),
            "125005b58a5350902b6d6087844d13ad3b4aa47663910155ea66ab81beea171f"
        );
        assert_eq!(
            fingerprint,
            membership_archive_invalidation_fingerprint(
                id,
                &order,
                Some(&archived_first_permuted_storage),
            )
            .unwrap(),
            "labels and stale archive IDs are excluded"
        );
        let both_archived =
            CollectionsPolicyRecordV1::new(id, None, vec![first.clone(), second.clone()]).unwrap();
        let both_archived_permuted =
            CollectionsPolicyRecordV1::new(id, None, vec![second.clone(), first.clone()]).unwrap();
        assert_eq!(
            membership_archive_invalidation_fingerprint(id, &order, Some(&both_archived)).unwrap(),
            membership_archive_invalidation_fingerprint(id, &order, Some(&both_archived_permuted),)
                .unwrap(),
            "archive input permutations canonicalize identically"
        );

        assert_ne!(
            fingerprint,
            membership_archive_invalidation_fingerprint(
                id,
                &[first.clone(), second.clone(), terminal(3)],
                Some(&archived_first),
            )
            .unwrap(),
            "add changes membership"
        );
        assert_ne!(
            fingerprint,
            membership_archive_invalidation_fingerprint(
                id,
                std::slice::from_ref(&first),
                Some(&archived_first),
            )
            .unwrap(),
            "remove changes membership"
        );
        assert_ne!(
            fingerprint,
            membership_archive_invalidation_fingerprint(
                id,
                &[second.clone(), first.clone()],
                Some(&archived_first),
            )
            .unwrap(),
            "order is canonical neutral structure"
        );
        assert_ne!(
            fingerprint,
            membership_archive_invalidation_fingerprint(id, &order, Some(&record(11, None, [2])),)
                .unwrap(),
            "effective archive changes"
        );
        assert_ne!(
            fingerprint,
            membership_archive_invalidation_fingerprint(container(12), &order, None).unwrap(),
            "container identity changes"
        );
    }
}
