//! Pure, unwired planning for mirroring legacy Collections policy into extension state.
//!
//! The planner operates only on persisted snapshots and a clone of durable extension state. It
//! does not mutate its inputs or confer production authority on the extension-state projection.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::collection_policy::{
    decode_policy_value, encode_policy_value, policy_key, validate_policy_namespace_capacity,
    CollectionsPolicyError, CollectionsPolicyRecordV1, LegacyCollectionsPolicyInput,
};
use crate::extension_state::{
    DurableExtensionStateV1, ExtensionStateError, ExtensionStateOperationV1,
    ExtensionStateTransactionV1, EXTENSION_STATE_CONTRACT_V1, MAX_STATE_OPERATIONS,
};
use crate::persist::{terminal_id_for_pane_snapshot, LayoutSnapshot, SessionSnapshot};
use crate::terminal::TerminalId;
use crate::terminal_container::TerminalContainerId;

const MIRROR_DIGEST_DOMAIN: &[u8] = b"vqro.collections.legacy-mirror.v1\0";
const IDEMPOTENCY_DOMAIN: &[u8] = b"vqro.collections.legacy-mirror.transaction.v1\0";
const IDEMPOTENCY_PREFIX: &str = "collections-mirror-v1-";

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CollectionsMirrorPlanV1 {
    pub(crate) transactions: Vec<ExtensionStateTransactionV1>,
    pub(crate) expected_final_projection: BTreeMap<String, Value>,
    pub(crate) expected_final_digest: String,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CollectionsMirrorError {
    DuplicateContainerIdentity,
    OrphanCollectionRecord,
    RepeatedCollectionLeaf,
    UnrecordedCollectionLeaf,
    GroupedMemberIsStandalone,
    DuplicateCollectionMember,
    DuplicateArchivedMember,
    ArchivedMemberNotInCollection,
    MissingPane,
    MissingTerminalIdentity,
    DuplicateTerminalIdentity,
    Policy(CollectionsPolicyError),
    State(ExtensionStateError),
    Serialization,
    SimulationMismatch,
}

impl From<CollectionsPolicyError> for CollectionsMirrorError {
    fn from(error: CollectionsPolicyError) -> Self {
        Self::Policy(error)
    }
}

impl From<ExtensionStateError> for CollectionsMirrorError {
    fn from(error: ExtensionStateError) -> Self {
        Self::State(error)
    }
}

#[derive(Debug)]
struct LegacyRecordInput {
    container_id: TerminalContainerId,
    label: Option<String>,
    archived_terminal_ids: Vec<TerminalId>,
    live_terminal_ids: HashSet<TerminalId>,
}

/// Plans an atomic, deterministic mirror without mutating either input.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn plan_collections_mirror(
    session: &SessionSnapshot,
    extension_state: &DurableExtensionStateV1,
) -> Result<CollectionsMirrorPlanV1, CollectionsMirrorError> {
    let legacy = collect_legacy_records(session)?;
    let namespace = crate::first_party::collections::NAMESPACE;
    let initial = extension_state.snapshot(namespace)?;

    let mut desired_records = BTreeMap::new();
    for (key, input) in legacy {
        let mut archived_terminal_ids = input.archived_terminal_ids;
        if let Some(existing) = initial.values.get(&key) {
            // Existing policy is never an archive source for live terminals. It contributes only
            // stale references, which cannot be reconstructed from the legacy pane projection.
            if let Ok(record) = decode_policy_value(&key, existing) {
                archived_terminal_ids.extend(
                    record
                        .archived_terminal_ids()
                        .iter()
                        .filter(|terminal_id| !input.live_terminal_ids.contains(*terminal_id))
                        .cloned(),
                );
            }
        }
        let record = CollectionsPolicyRecordV1::from_legacy(
            input.container_id,
            LegacyCollectionsPolicyInput {
                label: input.label,
                archived_terminal_ids,
            },
        )?;
        desired_records.insert(key, record);
    }
    validate_policy_namespace_capacity(desired_records.values())?;

    // Preserve every unrelated or malformed entry. Only a strictly decodable policy record for a
    // container absent from the complete legacy snapshot is managed stale state and may be deleted.
    let mut expected = initial.values.clone();
    for (key, value) in &initial.values {
        if !desired_records.contains_key(key) && decode_policy_value(key, value).is_ok() {
            expected.remove(key);
        }
    }
    for (key, record) in &desired_records {
        expected.insert(key.clone(), encode_policy_value(record)?);
    }

    let mut deletions = Vec::new();
    let mut non_growing_replacements = Vec::new();
    let mut growth_and_creations = Vec::new();
    for key in initial.values.keys() {
        if !expected.contains_key(key) {
            deletions.push(ExtensionStateOperationV1 {
                key: key.clone(),
                value: None,
            });
        }
    }
    for (key, value) in &expected {
        let Some(existing) = initial.values.get(key) else {
            growth_and_creations.push(ExtensionStateOperationV1 {
                key: key.clone(),
                value: Some(value.clone()),
            });
            continue;
        };
        if existing == value {
            continue;
        }
        let replacement = ExtensionStateOperationV1 {
            key: key.clone(),
            value: Some(value.clone()),
        };
        if encoded_value_len(value)? <= encoded_value_len(existing)? {
            non_growing_replacements.push(replacement);
        } else {
            growth_and_creations.push(replacement);
        }
    }
    let mut operations = deletions;
    operations.extend(non_growing_replacements);
    operations.extend(growth_and_creations);

    let expected_final_digest = projection_digest(&expected)?;
    let exact_plan_digest = exact_operations_digest(&operations)?;
    let chunks = operations.chunks(MAX_STATE_OPERATIONS).collect::<Vec<_>>();
    let mut simulation = extension_state.clone();
    let mut transactions = Vec::with_capacity(chunks.len());
    for (index, chunk) in chunks.iter().enumerate() {
        let actual = simulation.snapshot(namespace)?;
        let chunk = chunk.to_vec();
        let transaction = ExtensionStateTransactionV1 {
            contract: EXTENSION_STATE_CONTRACT_V1.into(),
            namespace: namespace.into(),
            expected_store_generation: actual.store_generation,
            expected_revision: actual.revision,
            idempotency_key: idempotency_key(
                &exact_plan_digest,
                index,
                chunks.len(),
                &actual.store_id,
                actual.store_generation,
                actual.revision,
                &chunk,
            )?,
            operations: chunk,
        };
        simulation.transact(&transaction)?;
        transactions.push(transaction);
    }

    if simulation.snapshot(namespace)?.values != expected {
        return Err(CollectionsMirrorError::SimulationMismatch);
    }

    Ok(CollectionsMirrorPlanV1 {
        transactions,
        expected_final_projection: expected,
        expected_final_digest,
    })
}

fn collect_legacy_records(
    session: &SessionSnapshot,
) -> Result<BTreeMap<String, LegacyRecordInput>, CollectionsMirrorError> {
    let mut records = BTreeMap::new();
    let mut all_terminal_ids = HashSet::new();
    for workspace in &session.workspaces {
        for tab in &workspace.tabs {
            validate_tab_collection_layout(tab)?;
            for collection in &tab.collections {
                let container_id = TerminalContainerId::from(collection.id);
                let key = policy_key(container_id);
                if records.contains_key(&key) {
                    return Err(CollectionsMirrorError::DuplicateContainerIdentity);
                }

                let members = collection.members.iter().copied().collect::<HashSet<_>>();
                if members.len() != collection.members.len() {
                    return Err(CollectionsMirrorError::DuplicateCollectionMember);
                }
                let archived = collection.archived.iter().copied().collect::<HashSet<_>>();
                if archived.len() != collection.archived.len() {
                    return Err(CollectionsMirrorError::DuplicateArchivedMember);
                }
                if !archived.is_subset(&members) {
                    return Err(CollectionsMirrorError::ArchivedMemberNotInCollection);
                }

                let mut terminal_by_pane = HashMap::new();
                let mut live_terminal_ids = HashSet::new();
                for pane_id in &collection.members {
                    let pane = tab
                        .panes
                        .get(pane_id)
                        .ok_or(CollectionsMirrorError::MissingPane)?;
                    let terminal_id = terminal_id_for_pane_snapshot(pane)
                        .map_err(|_| CollectionsMirrorError::MissingTerminalIdentity)?;
                    if !live_terminal_ids.insert(terminal_id.clone())
                        || !all_terminal_ids.insert(terminal_id.clone())
                    {
                        return Err(CollectionsMirrorError::DuplicateTerminalIdentity);
                    }
                    terminal_by_pane.insert(*pane_id, terminal_id);
                }
                let archived_terminal_ids = collection
                    .archived
                    .iter()
                    .map(|pane_id| terminal_by_pane[pane_id].clone())
                    .collect();
                records.insert(
                    key,
                    LegacyRecordInput {
                        container_id,
                        label: collection.label.clone(),
                        archived_terminal_ids,
                        live_terminal_ids,
                    },
                );
            }
        }
    }
    Ok(records)
}

fn validate_tab_collection_layout(
    tab: &crate::persist::TabSnapshot,
) -> Result<(), CollectionsMirrorError> {
    let record_ids = tab
        .collections
        .iter()
        .map(|collection| collection.id.raw())
        .collect::<HashSet<_>>();
    if record_ids.len() != tab.collections.len() {
        return Err(CollectionsMirrorError::DuplicateContainerIdentity);
    }
    let mut leaf_counts = HashMap::new();
    let mut standalone_panes = HashSet::new();
    collect_layout_representations(&tab.layout, &mut leaf_counts, &mut standalone_panes);
    for (id, count) in &leaf_counts {
        if *count > 1 {
            return Err(CollectionsMirrorError::RepeatedCollectionLeaf);
        }
        if !record_ids.contains(id) {
            return Err(CollectionsMirrorError::UnrecordedCollectionLeaf);
        }
    }
    if record_ids
        .iter()
        .any(|id| leaf_counts.get(id).copied() != Some(1))
    {
        return Err(CollectionsMirrorError::OrphanCollectionRecord);
    }
    if tab.collections.iter().any(|collection| {
        collection
            .members
            .iter()
            .any(|pane_id| standalone_panes.contains(pane_id))
    }) {
        return Err(CollectionsMirrorError::GroupedMemberIsStandalone);
    }
    Ok(())
}

fn collect_layout_representations(
    layout: &LayoutSnapshot,
    collection_counts: &mut HashMap<u64, usize>,
    standalone_panes: &mut HashSet<u32>,
) {
    match layout {
        LayoutSnapshot::Pane(pane_id) => {
            standalone_panes.insert(*pane_id);
        }
        LayoutSnapshot::Collection(id) => {
            *collection_counts.entry(id.raw()).or_default() += 1;
        }
        LayoutSnapshot::Split { first, second, .. } => {
            collect_layout_representations(first, collection_counts, standalone_panes);
            collect_layout_representations(second, collection_counts, standalone_panes);
        }
    }
}

fn encoded_value_len(value: &Value) -> Result<usize, CollectionsMirrorError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|_| CollectionsMirrorError::Serialization)
}

fn projection_digest(
    projection: &BTreeMap<String, Value>,
) -> Result<String, CollectionsMirrorError> {
    let bytes =
        serde_json::to_vec(projection).map_err(|_| CollectionsMirrorError::Serialization)?;
    let mut digest = Sha256::new();
    digest.update(MIRROR_DIGEST_DOMAIN);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    Ok(format!("{:x}", digest.finalize()))
}

fn exact_operations_digest(
    operations: &[ExtensionStateOperationV1],
) -> Result<String, CollectionsMirrorError> {
    let bytes =
        serde_json::to_vec(operations).map_err(|_| CollectionsMirrorError::Serialization)?;
    let mut digest = Sha256::new();
    digest.update(IDEMPOTENCY_DOMAIN);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    Ok(format!("{:x}", digest.finalize()))
}

#[allow(clippy::too_many_arguments)]
fn idempotency_key(
    plan_digest: &str,
    index: usize,
    total: usize,
    store_id: &str,
    store_generation: u64,
    revision: u64,
    operations: &[ExtensionStateOperationV1],
) -> Result<String, CollectionsMirrorError> {
    let bytes = serde_json::to_vec(&(
        plan_digest,
        index,
        total,
        store_id,
        store_generation,
        revision,
        operations,
    ))
    .map_err(|_| CollectionsMirrorError::Serialization)?;
    let mut digest = Sha256::new();
    digest.update(IDEMPOTENCY_DOMAIN);
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    Ok(format!("{IDEMPOTENCY_PREFIX}{:x}", digest.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::CollectionId;
    use crate::persist::SNAPSHOT_VERSION;

    fn terminal(raw: u64) -> String {
        format!("term_{raw:032x}")
    }

    fn pane(terminal_id: Option<&str>) -> Value {
        serde_json::json!({
            "terminal_id": terminal_id,
            "cwd": "/tmp"
        })
    }

    fn collection(raw: u64, members: Vec<u32>, archived: Vec<u32>) -> Value {
        collection_with_label(raw, members, archived, format!("Collection {raw}"))
    }

    fn collection_with_label(
        raw: u64,
        members: Vec<u32>,
        archived: Vec<u32>,
        label: String,
    ) -> Value {
        let id = CollectionId::parse(&format!("collection_{raw}")).unwrap();
        serde_json::json!({
            "id": id,
            "label": label,
            "members": members,
            "archived": archived
        })
    }

    fn collection_layout(collections: &[Value]) -> Value {
        let mut leaves = collections
            .iter()
            .map(|collection| serde_json::json!({"Collection": collection["id"].clone()}));
        let Some(first) = leaves.next() else {
            return serde_json::json!({"Pane": 999_999});
        };
        leaves.fold(first, |first, second| {
            serde_json::json!({
                "Split": {
                    "direction": "Horizontal",
                    "ratio": 0.5,
                    "first": first,
                    "second": second
                }
            })
        })
    }

    fn session_with_layout(
        collections: Vec<Value>,
        panes: HashMap<u32, Value>,
        layout: Value,
    ) -> SessionSnapshot {
        serde_json::from_value(serde_json::json!({
            "version": SNAPSHOT_VERSION,
            "workspaces": [{
                "id": "w1",
                "identity_cwd": "/tmp",
                "tabs": [{
                    "layout": layout,
                    "collections": collections,
                    "panes": panes,
                    "zoomed": false,
                    "focused": 1,
                    "root_pane": 1
                }],
                "active_tab": 0
            }],
            "repositories": [],
            "space_order": [],
            "active": 0,
            "selected": 0,
            "collapsed_space_keys": [],
            "delegations": []
        }))
        .unwrap()
    }

    fn session(collections: Vec<Value>, panes: HashMap<u32, Value>) -> SessionSnapshot {
        let layout = collection_layout(&collections);
        session_with_layout(collections, panes, layout)
    }

    fn state() -> DurableExtensionStateV1 {
        DurableExtensionStateV1::new("session:test".into(), 7).unwrap()
    }

    fn apply_values(state: &mut DurableExtensionStateV1, values: BTreeMap<String, Value>) {
        for (index, chunk) in values
            .into_iter()
            .collect::<Vec<_>>()
            .chunks(MAX_STATE_OPERATIONS)
            .enumerate()
        {
            let snapshot = state
                .snapshot(crate::first_party::collections::NAMESPACE)
                .unwrap();
            state
                .transact(&ExtensionStateTransactionV1 {
                    contract: EXTENSION_STATE_CONTRACT_V1.into(),
                    namespace: crate::first_party::collections::NAMESPACE.into(),
                    expected_store_generation: snapshot.store_generation,
                    expected_revision: snapshot.revision,
                    idempotency_key: format!("seed-{index}"),
                    operations: chunk
                        .iter()
                        .map(|(key, value)| ExtensionStateOperationV1 {
                            key: key.clone(),
                            value: Some(value.clone()),
                        })
                        .collect(),
                })
                .unwrap();
        }
    }

    fn encoded_record(container: u64, archived: &[u64]) -> (String, Value) {
        encoded_record_with_label(container, archived, format!("Collection {container}"))
    }

    fn encoded_record_with_label(
        container: u64,
        archived: &[u64],
        label: String,
    ) -> (String, Value) {
        let container_id = TerminalContainerId::parse(container).unwrap();
        let record = CollectionsPolicyRecordV1::new(
            container_id,
            Some(label),
            archived
                .iter()
                .map(|raw| TerminalId::parse(&terminal(*raw)).unwrap())
                .collect(),
        )
        .unwrap();
        (
            policy_key(container_id),
            encode_policy_value(&record).unwrap(),
        )
    }

    fn basic_session() -> SessionSnapshot {
        session(
            vec![collection(1, vec![10, 20], vec![20])],
            HashMap::from([
                (10, pane(Some(&terminal(10)))),
                (20, pane(Some(&terminal(20)))),
            ]),
        )
    }

    #[test]
    fn mirrors_only_legacy_live_archive_policy() {
        let plan = plan_collections_mirror(&basic_session(), &state()).unwrap();
        let key = policy_key(TerminalContainerId::parse(1).unwrap());
        let record = decode_policy_value(&key, &plan.expected_final_projection[&key]).unwrap();
        assert_eq!(record.label(), Some("Collection 1"));
        assert_eq!(
            record.archived_terminal_ids(),
            &[TerminalId::parse(&terminal(20)).unwrap()]
        );
    }

    #[test]
    fn maps_panes_to_stable_terminal_identities() {
        let plan = plan_collections_mirror(&basic_session(), &state()).unwrap();
        let encoded = serde_json::to_string(&plan.expected_final_projection).unwrap();
        assert!(encoded.contains(&terminal(20)));
        assert!(!encoded.contains("\"20\""));
    }

    #[test]
    fn rejects_missing_pane_and_terminal_identities() {
        let missing_pane = session(vec![collection(1, vec![10], vec![])], HashMap::new());
        assert_eq!(
            plan_collections_mirror(&missing_pane, &state()),
            Err(CollectionsMirrorError::MissingPane)
        );

        let missing_terminal = session(
            vec![collection(1, vec![10], vec![])],
            HashMap::from([(10, pane(None))]),
        );
        assert_eq!(
            plan_collections_mirror(&missing_terminal, &state()),
            Err(CollectionsMirrorError::MissingTerminalIdentity)
        );
    }

    #[test]
    fn rejects_duplicate_terminal_identities() {
        let id = terminal(1);
        let snapshot = session(
            vec![collection(1, vec![10, 20], vec![])],
            HashMap::from([(10, pane(Some(&id))), (20, pane(Some(&id)))]),
        );
        assert_eq!(
            plan_collections_mirror(&snapshot, &state()),
            Err(CollectionsMirrorError::DuplicateTerminalIdentity)
        );
    }

    #[test]
    fn rejects_duplicate_container_identities() {
        let snapshot = session(
            vec![collection(1, vec![], vec![]), collection(1, vec![], vec![])],
            HashMap::new(),
        );
        assert_eq!(
            plan_collections_mirror(&snapshot, &state()),
            Err(CollectionsMirrorError::DuplicateContainerIdentity)
        );
    }

    #[test]
    fn rejects_duplicate_members_and_archives() {
        let duplicate_member = session(
            vec![collection(1, vec![10, 10], vec![])],
            HashMap::from([(10, pane(Some(&terminal(10))))]),
        );
        assert_eq!(
            plan_collections_mirror(&duplicate_member, &state()),
            Err(CollectionsMirrorError::DuplicateCollectionMember)
        );
        let duplicate_archive = session(
            vec![collection(1, vec![10], vec![10, 10])],
            HashMap::from([(10, pane(Some(&terminal(10))))]),
        );
        assert_eq!(
            plan_collections_mirror(&duplicate_archive, &state()),
            Err(CollectionsMirrorError::DuplicateArchivedMember)
        );
    }

    #[test]
    fn rejects_archives_outside_membership() {
        let snapshot = session(
            vec![collection(1, vec![10], vec![20])],
            HashMap::from([(10, pane(Some(&terminal(10))))]),
        );
        assert_eq!(
            plan_collections_mirror(&snapshot, &state()),
            Err(CollectionsMirrorError::ArchivedMemberNotInCollection)
        );
    }

    #[test]
    fn validates_exact_same_tab_collection_layout_semantics() {
        let record = collection(1, vec![10], vec![]);
        let panes = HashMap::from([(10, pane(Some(&terminal(10))))]);

        let orphan = session_with_layout(
            vec![record.clone()],
            panes.clone(),
            serde_json::json!({"Pane": 999}),
        );
        assert_eq!(
            plan_collections_mirror(&orphan, &state()),
            Err(CollectionsMirrorError::OrphanCollectionRecord)
        );

        let leaf = serde_json::json!({"Collection": "collection_1"});
        let repeated = session_with_layout(
            vec![record.clone()],
            panes.clone(),
            serde_json::json!({
                "Split": {
                    "direction": "Horizontal",
                    "ratio": 0.5,
                    "first": leaf.clone(),
                    "second": leaf
                }
            }),
        );
        assert_eq!(
            plan_collections_mirror(&repeated, &state()),
            Err(CollectionsMirrorError::RepeatedCollectionLeaf)
        );

        let unrecorded = session_with_layout(
            Vec::new(),
            HashMap::new(),
            serde_json::json!({"Collection": "collection_1"}),
        );
        assert_eq!(
            plan_collections_mirror(&unrecorded, &state()),
            Err(CollectionsMirrorError::UnrecordedCollectionLeaf)
        );

        let standalone_member = session_with_layout(
            vec![record],
            panes,
            serde_json::json!({
                "Split": {
                    "direction": "Horizontal",
                    "ratio": 0.5,
                    "first": {"Collection": "collection_1"},
                    "second": {"Pane": 10}
                }
            }),
        );
        assert_eq!(
            plan_collections_mirror(&standalone_member, &state()),
            Err(CollectionsMirrorError::GroupedMemberIsStandalone)
        );
    }

    #[test]
    fn validates_collection_identity_scope_across_tabs_and_workspaces() {
        let mut combined = basic_session();
        let second = session(vec![collection(2, vec![], vec![])], HashMap::new());
        combined.workspaces[0]
            .tabs
            .push(second.workspaces[0].tabs[0].clone());
        let third = session(vec![collection(3, vec![], vec![])], HashMap::new());
        combined.workspaces.push(third.workspaces[0].clone());
        let plan = plan_collections_mirror(&combined, &state()).unwrap();
        for raw in 1..=3 {
            assert!(plan
                .expected_final_projection
                .contains_key(&policy_key(TerminalContainerId::parse(raw).unwrap())));
        }

        let mut wrong_tab_scope = combined.clone();
        let first_layout = wrong_tab_scope.workspaces[0].tabs[0].layout.clone();
        wrong_tab_scope.workspaces[0].tabs[0].layout =
            wrong_tab_scope.workspaces[0].tabs[1].layout.clone();
        wrong_tab_scope.workspaces[0].tabs[1].layout = first_layout;
        assert_eq!(
            plan_collections_mirror(&wrong_tab_scope, &state()),
            Err(CollectionsMirrorError::UnrecordedCollectionLeaf),
            "a matching collection elsewhere in the session cannot satisfy this tab"
        );

        let duplicate = session(vec![collection(1, vec![], vec![])], HashMap::new());
        combined.workspaces.push(duplicate.workspaces[0].clone());
        assert_eq!(
            plan_collections_mirror(&combined, &state()),
            Err(CollectionsMirrorError::DuplicateContainerIdentity)
        );
    }

    #[test]
    fn identical_pane_numbers_in_distinct_tabs_map_to_each_tabs_terminal() {
        let first_terminal = terminal(101);
        let second_terminal = terminal(202);
        let mut snapshot = session(
            vec![collection(1, vec![10], vec![10])],
            HashMap::from([(10, pane(Some(&first_terminal)))]),
        );
        let second = session(
            vec![collection(2, vec![10], vec![10])],
            HashMap::from([(10, pane(Some(&second_terminal)))]),
        );
        snapshot.workspaces[0]
            .tabs
            .push(second.workspaces[0].tabs[0].clone());

        let plan = plan_collections_mirror(&snapshot, &state()).unwrap();
        let first_key = policy_key(TerminalContainerId::parse(1).unwrap());
        let second_key = policy_key(TerminalContainerId::parse(2).unwrap());
        assert_eq!(
            decode_policy_value(&first_key, &plan.expected_final_projection[&first_key])
                .unwrap()
                .archived_terminal_ids(),
            &[TerminalId::parse(&first_terminal).unwrap()]
        );
        assert_eq!(
            decode_policy_value(&second_key, &plan.expected_final_projection[&second_key])
                .unwrap()
                .archived_terminal_ids(),
            &[TerminalId::parse(&second_terminal).unwrap()]
        );
    }

    #[test]
    fn duplicate_terminal_identity_across_tabs_and_workspaces_rejects() {
        let shared = terminal(303);
        let mut across_tabs = session(
            vec![collection(1, vec![10], vec![])],
            HashMap::from([(10, pane(Some(&shared)))]),
        );
        let second = session(
            vec![collection(2, vec![20], vec![])],
            HashMap::from([(20, pane(Some(&shared)))]),
        );
        across_tabs.workspaces[0]
            .tabs
            .push(second.workspaces[0].tabs[0].clone());
        assert_eq!(
            plan_collections_mirror(&across_tabs, &state()),
            Err(CollectionsMirrorError::DuplicateTerminalIdentity)
        );

        let mut across_workspaces = session(
            vec![collection(1, vec![10], vec![])],
            HashMap::from([(10, pane(Some(&shared)))]),
        );
        across_workspaces
            .workspaces
            .push(second.workspaces[0].clone());
        assert_eq!(
            plan_collections_mirror(&across_workspaces, &state()),
            Err(CollectionsMirrorError::DuplicateTerminalIdentity)
        );
    }

    #[test]
    fn preserves_stale_archived_ids_for_live_records() {
        let mut durable = state();
        apply_values(&mut durable, BTreeMap::from([encoded_record(1, &[10, 99])]));
        let plan = plan_collections_mirror(&basic_session(), &durable).unwrap();
        let key = policy_key(TerminalContainerId::parse(1).unwrap());
        let record = decode_policy_value(&key, &plan.expected_final_projection[&key]).unwrap();
        assert_eq!(
            record.archived_terminal_ids(),
            &[
                TerminalId::parse(&terminal(20)).unwrap(),
                TerminalId::parse(&terminal(99)).unwrap(),
            ]
        );
    }

    #[test]
    fn existing_live_archive_flags_never_override_legacy() {
        let mut durable = state();
        apply_values(&mut durable, BTreeMap::from([encoded_record(1, &[10])]));
        let plan = plan_collections_mirror(&basic_session(), &durable).unwrap();
        let key = policy_key(TerminalContainerId::parse(1).unwrap());
        let record = decode_policy_value(&key, &plan.expected_final_projection[&key]).unwrap();
        assert_eq!(
            record.archived_terminal_ids(),
            &[TerminalId::parse(&terminal(20)).unwrap()]
        );
    }

    #[test]
    fn preserves_unknown_and_malformed_unrelated_keys() {
        let mut durable = state();
        let malformed = serde_json::json!({"not": "policy"});
        let values = BTreeMap::from([
            ("unrelated".into(), serde_json::json!([1, 2, 3])),
            ("container_0000000000000063".into(), malformed.clone()),
        ]);
        apply_values(&mut durable, values.clone());
        let plan = plan_collections_mirror(&basic_session(), &durable).unwrap();
        for (key, value) in values {
            assert_eq!(plan.expected_final_projection.get(&key), Some(&value));
        }
    }

    #[test]
    fn deletes_only_decodable_managed_stale_records() {
        let mut durable = state();
        let stale = encoded_record(2, &[]);
        let malformed_key = "container_0000000000000003".to_string();
        apply_values(
            &mut durable,
            BTreeMap::from([
                stale.clone(),
                (malformed_key.clone(), serde_json::json!({"broken": true})),
            ]),
        );
        let plan = plan_collections_mirror(&basic_session(), &durable).unwrap();
        assert!(!plan.expected_final_projection.contains_key(&stale.0));
        assert!(plan.expected_final_projection.contains_key(&malformed_key));
        assert_eq!(plan.transactions[0].operations[0].key, stale.0);
        assert!(plan.transactions[0].operations[0].value.is_none());
    }

    #[test]
    fn overwrites_malformed_required_live_keys() {
        let mut durable = state();
        let key = policy_key(TerminalContainerId::parse(1).unwrap());
        apply_values(
            &mut durable,
            BTreeMap::from([(key.clone(), serde_json::json!({"broken": true}))]),
        );
        let plan = plan_collections_mirror(&basic_session(), &durable).unwrap();
        assert!(decode_policy_value(&key, &plan.expected_final_projection[&key]).is_ok());
        assert_eq!(plan.transactions.len(), 1);
    }

    #[test]
    fn diff_is_deterministic_delete_then_set() {
        let mut durable = state();
        apply_values(
            &mut durable,
            BTreeMap::from([encoded_record(2, &[]), encoded_record(3, &[])]),
        );
        let first = plan_collections_mirror(&basic_session(), &durable).unwrap();
        let second = plan_collections_mirror(&basic_session(), &durable).unwrap();
        assert_eq!(first, second);
        let operations = &first.transactions[0].operations;
        assert!(operations[..2]
            .iter()
            .all(|operation| operation.value.is_none()));
        assert!(operations[2..]
            .iter()
            .all(|operation| operation.value.is_some()));
        assert!(operations[..2]
            .windows(2)
            .all(|pair| pair[0].key <= pair[1].key));
        assert!(operations[2..]
            .windows(2)
            .all(|pair| pair[0].key <= pair[1].key));
    }

    #[test]
    fn digest_changes_directly_with_legacy_label() {
        let first = basic_session();
        let mut second = first.clone();
        second.workspaces[0].tabs[0].collections[0].label = Some("Renamed".into());
        assert_ne!(
            plan_collections_mirror(&first, &state())
                .unwrap()
                .expected_final_digest,
            plan_collections_mirror(&second, &state())
                .unwrap()
                .expected_final_digest
        );
    }

    #[test]
    fn digest_changes_directly_with_preserved_stale_archive() {
        let baseline = plan_collections_mirror(&basic_session(), &state())
            .unwrap()
            .expected_final_digest;
        let mut durable = state();
        apply_values(&mut durable, BTreeMap::from([encoded_record(1, &[99])]));
        assert_ne!(
            baseline,
            plan_collections_mirror(&basic_session(), &durable)
                .unwrap()
                .expected_final_digest
        );
    }

    #[test]
    fn managed_stale_deletion_changes_plan_and_idempotency_with_constant_projection() {
        let mut deleting_second = state();
        apply_values(
            &mut deleting_second,
            BTreeMap::from([encoded_record(2, &[])]),
        );
        let mut deleting_third = state();
        apply_values(
            &mut deleting_third,
            BTreeMap::from([encoded_record(3, &[])]),
        );

        let second_plan = plan_collections_mirror(&basic_session(), &deleting_second).unwrap();
        let third_plan = plan_collections_mirror(&basic_session(), &deleting_third).unwrap();
        assert_eq!(
            second_plan.expected_final_projection,
            third_plan.expected_final_projection
        );
        assert_eq!(
            second_plan.expected_final_digest,
            third_plan.expected_final_digest
        );
        assert_ne!(
            second_plan.transactions[0].operations,
            third_plan.transactions[0].operations
        );
        assert_ne!(
            second_plan.transactions[0].idempotency_key,
            third_plan.transactions[0].idempotency_key
        );
    }

    #[test]
    fn digest_changes_directly_when_managed_stale_record_is_deleted() {
        let stale = encoded_record(2, &[]);
        let before = BTreeMap::from([stale]);
        let after = BTreeMap::new();
        assert_ne!(
            projection_digest(&before).unwrap(),
            projection_digest(&after).unwrap()
        );
    }

    #[test]
    fn digest_changes_directly_with_preserved_unknown_and_malformed_values() {
        let baseline = plan_collections_mirror(&basic_session(), &state())
            .unwrap()
            .expected_final_digest;
        let mut durable = state();
        apply_values(
            &mut durable,
            BTreeMap::from([
                ("unknown".into(), serde_json::json!("preserved")),
                (
                    "container_0000000000000063".into(),
                    serde_json::json!({"malformed": true}),
                ),
            ]),
        );
        assert_ne!(
            baseline,
            plan_collections_mirror(&basic_session(), &durable)
                .unwrap()
                .expected_final_digest
        );
    }

    #[test]
    fn validates_capacity_against_the_complete_preserved_namespace() {
        let mut durable = state();
        let unrelated = (0..4)
            .map(|index| {
                (
                    format!("unrelated-{index}"),
                    Value::String("x".repeat(31_800)),
                )
            })
            .collect();
        apply_values(&mut durable, unrelated);
        let mut snapshot = basic_session();
        snapshot.workspaces[0].tabs[0].collections[0].label = Some("y".repeat(4 * 1024));
        assert!(matches!(
            plan_collections_mirror(&snapshot, &durable),
            Err(CollectionsMirrorError::State(
                ExtensionStateError::LimitExceeded("namespace bytes")
            ))
        ));
    }

    #[test]
    fn more_than_128_size_swaps_shrink_before_growth_and_remain_valid() {
        let large = "L".repeat(1_400);
        let collections = (1..=130)
            .map(|raw| {
                let label = if raw <= 65 { large.clone() } else { "s".into() };
                collection_with_label(raw, Vec::new(), Vec::new(), label)
            })
            .collect();
        let snapshot = session(collections, HashMap::new());
        let existing = (1..=130)
            .map(|raw| {
                let label = if raw <= 65 { "s".into() } else { large.clone() };
                encoded_record_with_label(raw, &[], label)
            })
            .collect();
        let mut durable = state();
        apply_values(&mut durable, existing);

        let plan = plan_collections_mirror(&snapshot, &durable).unwrap();
        assert_eq!(plan.transactions.len(), 2);
        assert_eq!(plan.transactions[0].operations.len(), MAX_STATE_OPERATIONS);
        assert_eq!(plan.transactions[1].operations.len(), 2);
        let operations = plan
            .transactions
            .iter()
            .flat_map(|transaction| transaction.operations.iter())
            .collect::<Vec<_>>();
        assert_eq!(
            operations[..65]
                .iter()
                .map(|operation| operation.key.clone())
                .collect::<Vec<_>>(),
            (66..=130)
                .map(|raw| policy_key(TerminalContainerId::parse(raw).unwrap()))
                .collect::<Vec<_>>(),
            "all shrinking replacements must be key-sorted before growth"
        );
        assert_eq!(
            operations[65..]
                .iter()
                .map(|operation| operation.key.clone())
                .collect::<Vec<_>>(),
            (1..=65)
                .map(|raw| policy_key(TerminalContainerId::parse(raw).unwrap()))
                .collect::<Vec<_>>(),
            "growth replacements must retain stable key order"
        );
        let mut applied = durable;
        for transaction in &plan.transactions {
            applied.transact(transaction).unwrap();
        }
        assert_eq!(
            applied
                .snapshot(crate::first_party::collections::NAMESPACE)
                .unwrap()
                .values,
            plan.expected_final_projection
        );
    }

    #[test]
    fn chunks_more_than_128_operations_with_actual_revisions() {
        let mut durable = state();
        let stale = (2..=131).map(|raw| encoded_record(raw, &[])).collect();
        apply_values(&mut durable, stale);
        let initial = durable
            .snapshot(crate::first_party::collections::NAMESPACE)
            .unwrap();
        let plan = plan_collections_mirror(&basic_session(), &durable).unwrap();
        assert_eq!(plan.transactions.len(), 2);
        assert_eq!(plan.transactions[0].operations.len(), MAX_STATE_OPERATIONS);
        assert_eq!(plan.transactions[1].operations.len(), 3);
        assert_eq!(plan.transactions[0].expected_revision, initial.revision);
        assert_eq!(plan.transactions[1].expected_revision, initial.revision + 1);
        assert!(plan.transactions.iter().all(|transaction| {
            transaction.expected_store_generation == initial.store_generation
                && transaction.idempotency_key.len() <= 128
        }));
    }

    #[test]
    fn namespace_creation_tracks_store_generation_transition_between_chunks() {
        let mut durable = state();
        for index in 0..crate::extension_state::MAX_STATE_NAMESPACES {
            let namespace = format!("seed.namespace-{index}");
            let snapshot = durable.snapshot(&namespace).unwrap();
            durable
                .transact(&ExtensionStateTransactionV1 {
                    contract: EXTENSION_STATE_CONTRACT_V1.into(),
                    namespace,
                    expected_store_generation: snapshot.store_generation,
                    expected_revision: snapshot.revision,
                    idempotency_key: format!("seed-namespace-{index}"),
                    operations: vec![ExtensionStateOperationV1 {
                        key: "value".into(),
                        value: Some(serde_json::json!(index)),
                    }],
                })
                .unwrap();
        }
        durable
            .clear_namespace("seed.namespace-0", "clear-seed-namespace".into())
            .unwrap();
        // Retirement requires an empty namespace whose last event has aged out of the bounded
        // event window.
        for index in 0..=crate::extension_state::MAX_STATE_EVENTS {
            let namespace = "seed.namespace-1";
            let snapshot = durable.snapshot(namespace).unwrap();
            durable
                .transact(&ExtensionStateTransactionV1 {
                    contract: EXTENSION_STATE_CONTRACT_V1.into(),
                    namespace: namespace.into(),
                    expected_store_generation: snapshot.store_generation,
                    expected_revision: snapshot.revision,
                    idempotency_key: format!("age-clear-event-{index}"),
                    operations: vec![ExtensionStateOperationV1 {
                        key: "value".into(),
                        value: Some(serde_json::json!(index)),
                    }],
                })
                .unwrap();
        }
        let generation = durable.store_generation;
        let collections = (1..=129)
            .map(|raw| collection(raw, Vec::new(), Vec::new()))
            .collect();
        let snapshot = session(collections, HashMap::new());
        let plan = plan_collections_mirror(&snapshot, &durable).unwrap();
        assert_eq!(plan.transactions.len(), 2);
        assert_eq!(plan.transactions[0].expected_store_generation, generation);
        assert_eq!(
            plan.transactions[1].expected_store_generation,
            generation + 1,
            "the simulation must observe namespace retirement during creation"
        );
        let mut applied = durable;
        for transaction in &plan.transactions {
            applied.transact(transaction).unwrap();
        }
        assert_eq!(applied.store_generation, generation + 1);
    }

    #[test]
    fn full_multi_chunk_sequence_replays_and_replans_as_noop() {
        let mut durable = state();
        let stale = (2..=131).map(|raw| encoded_record(raw, &[])).collect();
        apply_values(&mut durable, stale);
        let plan = plan_collections_mirror(&basic_session(), &durable).unwrap();
        assert!(plan.transactions.len() > 1);
        let mut applied = durable.clone();
        for transaction in &plan.transactions {
            let first = applied.transact(transaction).unwrap();
            assert!(!first.replayed);
        }
        for transaction in &plan.transactions {
            let replay = applied.transact(transaction).unwrap();
            assert!(replay.replayed);
        }
        let final_snapshot = applied
            .snapshot(crate::first_party::collections::NAMESPACE)
            .unwrap();
        assert_eq!(final_snapshot.values, plan.expected_final_projection);
        assert_eq!(
            projection_digest(&final_snapshot.values).unwrap(),
            plan.expected_final_digest
        );
        let noop = plan_collections_mirror(&basic_session(), &applied).unwrap();
        assert!(noop.transactions.is_empty());
        assert_eq!(
            noop.expected_final_projection,
            plan.expected_final_projection
        );
        assert_eq!(noop.expected_final_digest, plan.expected_final_digest);
    }
}
