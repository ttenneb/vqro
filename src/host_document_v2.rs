//! Canonical, producer-fenced `host.document.v2` data contract.
//!
//! This module is deliberately runtime-neutral. It validates producer output but does not
//! discover or invoke producers, grant actions, perform effects, or project presentation state.

use std::collections::{HashMap, HashSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::terminal::TerminalId;

pub const HOST_DOCUMENT_CONTRACT_V2: &str = "host.document.v2";
pub const HOST_DOCUMENT_SCHEMA_ID_V2: &str = "https://vqro.dev/schema/host-document/v2";
pub const MAX_DOCUMENT_V2_BYTES: usize = 128 * 1024;
pub const MAX_DOCUMENT_V2_NODES: usize = 512;
pub const MAX_DOCUMENT_V2_DEPTH: usize = 16;
pub const MAX_DOCUMENT_V2_DEPENDENCIES: usize = 64;
pub const MAX_DOCUMENT_V2_TEXT_CHARS: usize = 4 * 1024;
pub const MAX_DOCUMENT_V2_TOTAL_TEXT_CHARS: usize = 64 * 1024;
pub const MAX_DOCUMENT_V2_ACCESSIBLE_NAME_CHARS: usize = 256;
pub const MAX_DOCUMENT_V2_ACCESSIBLE_DESCRIPTION_CHARS: usize = 1024;
pub const MAX_DOCUMENT_V2_IDENTIFIER_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CappedSerializationError {
    LimitExceeded,
    Serialization,
}

struct CountingWriter {
    written: usize,
    limit: usize,
    exceeded: bool,
}

impl std::io::Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let Some(next) = self.written.checked_add(bytes.len()) else {
            self.exceeded = true;
            return Err(std::io::Error::other("encoded value exceeds byte limit"));
        };
        if next > self.limit {
            self.exceeded = true;
            return Err(std::io::Error::other("encoded value exceeds byte limit"));
        }
        self.written = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn serialized_len_capped<T: Serialize + ?Sized>(
    value: &T,
    limit: usize,
) -> Result<usize, CappedSerializationError> {
    let mut writer = CountingWriter {
        written: 0,
        limit,
        exceeded: false,
    };
    match serde_json::to_writer(&mut writer, value) {
        Ok(()) => Ok(writer.written),
        Err(_) if writer.exceeded => Err(CappedSerializationError::LimitExceeded),
        Err(_) => Err(CappedSerializationError::Serialization),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HostDocumentV2 {
    pub contract: String,
    pub producer: DocumentProducerFenceV2,
    pub scope: DocumentScopeV2,
    pub revision: u64,
    #[serde(default)]
    pub dependencies: Vec<DocumentDependencyV2>,
    #[serde(default)]
    pub roots: Vec<String>,
    #[serde(default)]
    pub nodes: Vec<DocumentNodeV2>,
}

#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct DocumentProducerV2 {
    pub package_id: String,
    pub service_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocumentProducerFenceV2 {
    pub package_id: String,
    pub service_id: String,
    pub artifact_sha256: String,
    pub runtime_generation: u64,
    pub provider_generation: u64,
    pub scope_generation: u64,
}

impl DocumentProducerFenceV2 {
    pub fn producer(&self) -> DocumentProducerV2 {
        DocumentProducerV2 {
            package_id: self.package_id.clone(),
            service_id: self.service_id.clone(),
        }
    }
}

#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct DocumentScopeV2 {
    pub contract: String,
    pub scope_id: String,
}

#[derive(
    Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct DocumentDependencyV2 {
    pub contract: String,
    pub scope_id: String,
    pub revision: u64,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DocumentAccessibilityV2 {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DocumentNodeV2 {
    Group {
        id: String,
        accessibility: DocumentAccessibilityV2,
        #[serde(default)]
        children: Vec<String>,
    },
    Text {
        id: String,
        accessibility: DocumentAccessibilityV2,
        text: String,
    },
    TerminalSlot {
        id: String,
        accessibility: DocumentAccessibilityV2,
        terminal_id: String,
    },
}

impl DocumentNodeV2 {
    pub fn id(&self) -> &str {
        match self {
            Self::Group { id, .. } | Self::Text { id, .. } | Self::TerminalSlot { id, .. } => id,
        }
    }

    fn accessibility(&self) -> &DocumentAccessibilityV2 {
        match self {
            Self::Group { accessibility, .. }
            | Self::Text { accessibility, .. }
            | Self::TerminalSlot { accessibility, .. } => accessibility,
        }
    }

    fn children(&self) -> &[String] {
        match self {
            Self::Group { children, .. } => children,
            Self::Text { .. } | Self::TerminalSlot { .. } => &[],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TerminalScopePolicy {
    #[default]
    ReferencedSubset,
    ExactCoverage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostDocumentV2Error(pub String);

impl std::fmt::Display for HostDocumentV2Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for HostDocumentV2Error {}

impl HostDocumentV2 {
    /// Strictly decodes and validates producer bytes. Unknown fields are denied by every DTO.
    pub fn validate_json(
        encoded: &[u8],
        allowed_terminals: &HashSet<TerminalId>,
        terminal_policy: TerminalScopePolicy,
    ) -> Result<Self, HostDocumentV2Error> {
        if encoded.len() > MAX_DOCUMENT_V2_BYTES {
            return invalid("document exceeds the 128 KiB encoded limit");
        }
        let document: Self = serde_json::from_slice(encoded).map_err(|error| {
            HostDocumentV2Error(format!("invalid host.document.v2 JSON: {error}"))
        })?;
        document.validate(allowed_terminals, terminal_policy)?;
        Ok(document)
    }

    pub fn validate(
        &self,
        allowed_terminals: &HashSet<TerminalId>,
        terminal_policy: TerminalScopePolicy,
    ) -> Result<(), HostDocumentV2Error> {
        match serialized_len_capped(self, MAX_DOCUMENT_V2_BYTES) {
            Ok(_) => {}
            Err(CappedSerializationError::LimitExceeded) => {
                return invalid("document exceeds the 128 KiB encoded limit");
            }
            Err(CappedSerializationError::Serialization) => {
                return invalid("cannot encode document");
            }
        }
        if self.contract != HOST_DOCUMENT_CONTRACT_V2 {
            return invalid("unsupported host document contract");
        }
        validate_producer_fence(&self.producer)?;
        validate_scope(&self.scope)?;
        if self.revision == 0 {
            return invalid("document revision must be non-zero");
        }
        validate_dependencies(&self.dependencies)?;
        if self.nodes.len() > MAX_DOCUMENT_V2_NODES {
            return invalid("document has more than 512 nodes");
        }
        if self.roots.len() > MAX_DOCUMENT_V2_NODES {
            return invalid("document has more than 512 roots");
        }

        let mut total_text = 0_usize;
        let mut node_indices = HashMap::with_capacity(self.nodes.len());
        let mut terminal_ids = HashSet::new();
        for (index, node) in self.nodes.iter().enumerate() {
            validate_opaque_id(node.id(), "node id")?;
            if node_indices.insert(node.id(), index).is_some() {
                return invalid("document node ids must be unique");
            }
            validate_accessibility(node.accessibility(), &mut total_text)?;
            if let DocumentNodeV2::Text { text, .. } = node {
                validate_text(text, MAX_DOCUMENT_V2_TEXT_CHARS, "text", false)?;
                add_text_budget(&mut total_text, text)?;
            }
            if let DocumentNodeV2::TerminalSlot { terminal_id, .. } = node {
                let parsed = TerminalId::parse(terminal_id)
                    .map_err(|_| HostDocumentV2Error("invalid terminal id".into()))?;
                if !allowed_terminals.contains(&parsed) {
                    return invalid("terminal slot is outside the caller-supplied scope");
                }
                if !terminal_ids.insert(parsed) {
                    return invalid("terminal slots must reference unique terminals");
                }
            }
        }

        let mut referenced = HashSet::with_capacity(self.nodes.len());
        for root in &self.roots {
            validate_opaque_id(root, "root node id")?;
            if !node_indices.contains_key(root.as_str()) {
                return invalid("document root references an unknown node");
            }
            if !referenced.insert(root.as_str()) {
                return invalid("a node may be referenced only once");
            }
        }
        for node in &self.nodes {
            if node.children().len() > MAX_DOCUMENT_V2_NODES {
                return invalid("document group has more than 512 children");
            }
            for child in node.children() {
                validate_opaque_id(child, "child node id")?;
                if !node_indices.contains_key(child.as_str()) {
                    return invalid("document child references an unknown node");
                }
                if !referenced.insert(child.as_str()) {
                    return invalid("a node may be referenced only once");
                }
            }
        }
        if referenced.len() != self.nodes.len() {
            return invalid("every document node must be reachable from exactly one root");
        }
        validate_tree_depth_and_cycles(self, &node_indices)?;

        if terminal_policy == TerminalScopePolicy::ExactCoverage
            && terminal_ids.len() != allowed_terminals.len()
        {
            return invalid("terminal slots do not exactly cover the caller-supplied scope");
        }
        Ok(())
    }
}

pub(crate) fn validate_producer_fence(
    fence: &DocumentProducerFenceV2,
) -> Result<(), HostDocumentV2Error> {
    validate_namespaced_id(&fence.package_id, "package id")?;
    validate_opaque_id(&fence.service_id, "service id")?;
    if fence.artifact_sha256.len() != 64
        || !fence
            .artifact_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return invalid("artifact_sha256 must be 64 lowercase hexadecimal characters");
    }
    if fence.runtime_generation == 0
        || fence.provider_generation == 0
        || fence.scope_generation == 0
    {
        return invalid("producer and scope generations must be non-zero");
    }
    Ok(())
}

pub(crate) fn validate_scope(scope: &DocumentScopeV2) -> Result<(), HostDocumentV2Error> {
    validate_contract_name(&scope.contract)?;
    validate_opaque_id(&scope.scope_id, "scope id")
}

pub(crate) fn validate_dependencies(
    dependencies: &[DocumentDependencyV2],
) -> Result<(), HostDocumentV2Error> {
    if dependencies.len() > MAX_DOCUMENT_V2_DEPENDENCIES {
        return invalid("document has too many dependencies");
    }
    let mut previous: Option<&DocumentDependencyV2> = None;
    for dependency in dependencies {
        validate_contract_name(&dependency.contract)?;
        validate_opaque_id(&dependency.scope_id, "dependency scope id")?;
        if dependency.revision == 0 || dependency.generation == 0 {
            return invalid("dependency revision and generation must be non-zero");
        }
        if previous.is_some_and(|value| value >= dependency) {
            return invalid("dependency vector must be strictly sorted and unique");
        }
        previous = Some(dependency);
    }
    Ok(())
}

fn validate_accessibility(
    accessibility: &DocumentAccessibilityV2,
    total_text: &mut usize,
) -> Result<(), HostDocumentV2Error> {
    validate_text(
        &accessibility.name,
        MAX_DOCUMENT_V2_ACCESSIBLE_NAME_CHARS,
        "accessibility name",
        true,
    )?;
    add_text_budget(total_text, &accessibility.name)?;
    if let Some(description) = &accessibility.description {
        validate_text(
            description,
            MAX_DOCUMENT_V2_ACCESSIBLE_DESCRIPTION_CHARS,
            "accessibility description",
            false,
        )?;
        add_text_budget(total_text, description)?;
    }
    Ok(())
}

fn add_text_budget(total: &mut usize, value: &str) -> Result<(), HostDocumentV2Error> {
    *total = total
        .checked_add(value.chars().count())
        .ok_or_else(|| HostDocumentV2Error("document text budget overflowed".into()))?;
    if *total > MAX_DOCUMENT_V2_TOTAL_TEXT_CHARS {
        return invalid("document exceeds the aggregate text budget");
    }
    Ok(())
}

fn validate_text(
    value: &str,
    max_chars: usize,
    label: &str,
    require_nonempty: bool,
) -> Result<(), HostDocumentV2Error> {
    let chars = value.chars().count();
    if (require_nonempty && chars == 0) || chars > max_chars {
        return invalid(&format!("invalid {label} length"));
    }
    if value.chars().any(char::is_control) {
        return invalid(&format!("{label} contains a control or escape character"));
    }
    Ok(())
}

fn validate_tree_depth_and_cycles(
    document: &HostDocumentV2,
    indices: &HashMap<&str, usize>,
) -> Result<(), HostDocumentV2Error> {
    let mut complete = HashSet::with_capacity(document.nodes.len());
    for root in &document.roots {
        let mut stack = vec![(root.as_str(), 1_usize, false)];
        let mut path = HashSet::new();
        while let Some((id, depth, leaving)) = stack.pop() {
            if leaving {
                path.remove(id);
                complete.insert(id);
                continue;
            }
            if depth > MAX_DOCUMENT_V2_DEPTH {
                return invalid("document tree exceeds depth 16");
            }
            if !path.insert(id) {
                return invalid("document tree contains a cycle");
            }
            if complete.contains(id) {
                path.remove(id);
                continue;
            }
            stack.push((id, depth, true));
            let node = &document.nodes[indices[id]];
            for child in node.children().iter().rev() {
                stack.push((child.as_str(), depth + 1, false));
            }
        }
    }
    if complete.len() != document.nodes.len() {
        return invalid("every document node must be reachable from a root");
    }
    Ok(())
}

fn validate_contract_name(value: &str) -> Result<(), HostDocumentV2Error> {
    if value.is_empty()
        || value.len() > MAX_DOCUMENT_V2_IDENTIFIER_BYTES
        || value.split('.').count() < 2
        || value.split('.').any(|segment| {
            segment.is_empty()
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
    {
        return invalid("invalid named contract");
    }
    Ok(())
}

fn validate_namespaced_id(value: &str, label: &str) -> Result<(), HostDocumentV2Error> {
    validate_contract_name(value).map_err(|_| HostDocumentV2Error(format!("invalid {label}")))
}

fn validate_opaque_id(value: &str, label: &str) -> Result<(), HostDocumentV2Error> {
    if value.is_empty()
        || value.len() > MAX_DOCUMENT_V2_IDENTIFIER_BYTES
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'/' | b'_' | b'-')
        })
    {
        return invalid(&format!("invalid {label}"));
    }
    Ok(())
}

fn invalid<T>(message: &str) -> Result<T, HostDocumentV2Error> {
    Err(HostDocumentV2Error(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TERMINAL: &str = "term_00000000000000000000000000000001";

    fn allowed() -> HashSet<TerminalId> {
        HashSet::from([TerminalId::parse(TERMINAL).unwrap()])
    }

    fn valid_bytes() -> Vec<u8> {
        include_bytes!("../docs/next/api/host-document-v2.fixtures/valid.json").to_vec()
    }

    #[test]
    fn golden_document_is_strict_and_valid() {
        let document = HostDocumentV2::validate_json(
            &valid_bytes(),
            &allowed(),
            TerminalScopePolicy::ExactCoverage,
        )
        .unwrap();
        assert_eq!(document.contract, HOST_DOCUMENT_CONTRACT_V2);
        assert_eq!(document.nodes.len(), 3);
    }

    #[test]
    fn golden_invalid_documents_are_rejected() {
        for bytes in [
            include_bytes!("../docs/next/api/host-document-v2.fixtures/invalid-unknown-field.json")
                .as_slice(),
            include_bytes!("../docs/next/api/host-document-v2.fixtures/invalid-action.json")
                .as_slice(),
            include_bytes!("../docs/next/api/host-document-v2.fixtures/invalid-unreachable.json")
                .as_slice(),
        ] {
            assert!(HostDocumentV2::validate_json(
                bytes,
                &allowed(),
                TerminalScopePolicy::ReferencedSubset,
            )
            .is_err());
        }
    }

    #[test]
    fn terminal_scope_and_tree_rules_are_enforced() {
        let mut document = HostDocumentV2::validate_json(
            &valid_bytes(),
            &allowed(),
            TerminalScopePolicy::ReferencedSubset,
        )
        .unwrap();
        let DocumentNodeV2::Group { children, .. } = &mut document.nodes[0] else {
            panic!("group fixture")
        };
        children.push("root".into());
        assert!(document
            .validate(&allowed(), TerminalScopePolicy::ReferencedSubset)
            .is_err());

        assert!(HostDocumentV2::validate_json(
            &valid_bytes(),
            &HashSet::new(),
            TerminalScopePolicy::ReferencedSubset,
        )
        .unwrap_err()
        .to_string()
        .contains("outside"));
    }

    #[test]
    fn tree_depth_node_identity_and_terminal_uniqueness_are_enforced() {
        let mut document = HostDocumentV2::validate_json(
            &valid_bytes(),
            &allowed(),
            TerminalScopePolicy::ReferencedSubset,
        )
        .unwrap();
        document.nodes[2] = document.nodes[1].clone();
        assert!(document
            .validate(&allowed(), TerminalScopePolicy::ReferencedSubset)
            .unwrap_err()
            .to_string()
            .contains("unique"));

        let accessibility = DocumentAccessibilityV2 {
            name: "group".into(),
            description: None,
        };
        document.roots = vec!["n0".into()];
        document.nodes = (0..=MAX_DOCUMENT_V2_DEPTH)
            .map(|index| DocumentNodeV2::Group {
                id: format!("n{index}"),
                accessibility: accessibility.clone(),
                children: if index < MAX_DOCUMENT_V2_DEPTH {
                    vec![format!("n{}", index + 1)]
                } else {
                    Vec::new()
                },
            })
            .collect();
        assert!(document
            .validate(&allowed(), TerminalScopePolicy::ReferencedSubset)
            .unwrap_err()
            .to_string()
            .contains("depth"));

        let mut value: serde_json::Value = serde_json::from_slice(&valid_bytes()).unwrap();
        value["nodes"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "kind": "terminal_slot",
                "id": "terminal-slot-2",
                "accessibility": {"name": "Duplicate terminal"},
                "terminal_id": TERMINAL
            }));
        value["nodes"][0]["children"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("terminal-slot-2"));
        assert!(HostDocumentV2::validate_json(
            &serde_json::to_vec(&value).unwrap(),
            &allowed(),
            TerminalScopePolicy::ReferencedSubset,
        )
        .unwrap_err()
        .to_string()
        .contains("unique terminals"));
    }

    #[test]
    fn rootless_cycles_and_inexact_terminal_coverage_are_rejected() {
        let mut document = HostDocumentV2::validate_json(
            &valid_bytes(),
            &allowed(),
            TerminalScopePolicy::ReferencedSubset,
        )
        .unwrap();
        document.roots.clear();
        document.nodes = vec![
            DocumentNodeV2::Group {
                id: "a".into(),
                accessibility: DocumentAccessibilityV2 {
                    name: "A".into(),
                    description: None,
                },
                children: vec!["b".into()],
            },
            DocumentNodeV2::Group {
                id: "b".into(),
                accessibility: DocumentAccessibilityV2 {
                    name: "B".into(),
                    description: None,
                },
                children: vec!["a".into()],
            },
        ];
        assert!(document
            .validate(&allowed(), TerminalScopePolicy::ReferencedSubset)
            .unwrap_err()
            .to_string()
            .contains("reachable"));

        let extra = TerminalId::parse("term_00000000000000000000000000000002").unwrap();
        let mut wider_scope = allowed();
        wider_scope.insert(extra);
        assert!(HostDocumentV2::validate_json(
            &valid_bytes(),
            &wider_scope,
            TerminalScopePolicy::ExactCoverage,
        )
        .unwrap_err()
        .to_string()
        .contains("exactly cover"));
    }

    #[test]
    fn encoded_and_text_budgets_are_enforced() {
        let oversized = vec![b' '; MAX_DOCUMENT_V2_BYTES + 1];
        assert!(HostDocumentV2::validate_json(
            &oversized,
            &allowed(),
            TerminalScopePolicy::ReferencedSubset,
        )
        .unwrap_err()
        .to_string()
        .contains("128 KiB"));

        let mut value: serde_json::Value = serde_json::from_slice(&valid_bytes()).unwrap();
        value["nodes"][1]["text"] =
            serde_json::Value::String("x".repeat(MAX_DOCUMENT_V2_TEXT_CHARS + 1));
        assert!(HostDocumentV2::validate_json(
            &serde_json::to_vec(&value).unwrap(),
            &allowed(),
            TerminalScopePolicy::ReferencedSubset,
        )
        .is_err());

        value = serde_json::from_slice(&valid_bytes()).unwrap();
        value["nodes"][0]["accessibility"]["name"] = serde_json::json!("");
        assert!(HostDocumentV2::validate_json(
            &serde_json::to_vec(&value).unwrap(),
            &allowed(),
            TerminalScopePolicy::ReferencedSubset,
        )
        .is_err());

        value = serde_json::from_slice(&valid_bytes()).unwrap();
        value["nodes"][0]["accessibility"]["description"] =
            serde_json::json!("x".repeat(MAX_DOCUMENT_V2_ACCESSIBLE_DESCRIPTION_CHARS + 1));
        assert!(HostDocumentV2::validate_json(
            &serde_json::to_vec(&value).unwrap(),
            &allowed(),
            TerminalScopePolicy::ReferencedSubset,
        )
        .is_err());
    }

    #[test]
    fn empty_ordered_forest_is_valid() {
        let mut value: serde_json::Value = serde_json::from_slice(&valid_bytes()).unwrap();
        value["roots"] = serde_json::json!([]);
        value["nodes"] = serde_json::json!([]);
        HostDocumentV2::validate_json(
            &serde_json::to_vec(&value).unwrap(),
            &HashSet::new(),
            TerminalScopePolicy::ExactCoverage,
        )
        .unwrap();
    }

    #[test]
    fn canonical_identifier_terminal_artifact_and_integer_forms_are_enforced() {
        let mutations = [
            ("contract", serde_json::json!("host..document")),
            (
                "producer.package_id",
                serde_json::json!("example..document"),
            ),
            ("producer.service_id", serde_json::json!("bad service")),
            (
                "producer.artifact_sha256",
                serde_json::json!("A".repeat(64)),
            ),
            ("scope.contract", serde_json::json!("host..session")),
            ("scope.scope_id", serde_json::json!("bad scope")),
            ("nodes.0.id", serde_json::json!("bad id")),
            ("nodes.0.children.0", serde_json::json!("bad child")),
            (
                "nodes.2.terminal_id",
                serde_json::json!("term_0000000000000000000000000000000A"),
            ),
        ];
        for (path, replacement) in mutations {
            let mut value: serde_json::Value = serde_json::from_slice(&valid_bytes()).unwrap();
            let mut current = &mut value;
            for segment in path.split('.') {
                current = if let Ok(index) = segment.parse::<usize>() {
                    &mut current[index]
                } else {
                    &mut current[segment]
                };
            }
            *current = replacement;
            assert!(
                HostDocumentV2::validate_json(
                    &serde_json::to_vec(&value).unwrap(),
                    &allowed(),
                    TerminalScopePolicy::ReferencedSubset,
                )
                .is_err(),
                "accepted noncanonical {path}"
            );
        }

        let oversized_integer = String::from_utf8(valid_bytes())
            .unwrap()
            .replace("\"revision\": 3", "\"revision\": 18446744073709551616");
        assert!(HostDocumentV2::validate_json(
            oversized_integer.as_bytes(),
            &allowed(),
            TerminalScopePolicy::ReferencedSubset,
        )
        .is_err());
    }

    #[test]
    fn collection_and_aggregate_budgets_are_enforced() {
        let mut document = HostDocumentV2::validate_json(
            &valid_bytes(),
            &allowed(),
            TerminalScopePolicy::ReferencedSubset,
        )
        .unwrap();
        document.dependencies = (0..=MAX_DOCUMENT_V2_DEPENDENCIES)
            .map(|index| DocumentDependencyV2 {
                contract: "host.scope.v1".into(),
                scope_id: format!("s{index:03}"),
                revision: 1,
                generation: 1,
            })
            .collect();
        assert!(document
            .validate(&allowed(), TerminalScopePolicy::ReferencedSubset)
            .unwrap_err()
            .to_string()
            .contains("dependencies"));

        document.dependencies.clear();
        document.roots = (0..=MAX_DOCUMENT_V2_NODES)
            .map(|index| format!("n{index}"))
            .collect();
        document.nodes = (0..=MAX_DOCUMENT_V2_NODES)
            .map(|index| DocumentNodeV2::Text {
                id: format!("n{index}"),
                accessibility: DocumentAccessibilityV2 {
                    name: "N".into(),
                    description: None,
                },
                text: String::new(),
            })
            .collect();
        assert!(document
            .validate(&allowed(), TerminalScopePolicy::ReferencedSubset)
            .unwrap_err()
            .to_string()
            .contains("nodes"));

        document.nodes.truncate(1);
        document.roots = (0..=MAX_DOCUMENT_V2_NODES)
            .map(|index| format!("n{index}"))
            .collect();
        assert!(document
            .validate(&allowed(), TerminalScopePolicy::ReferencedSubset)
            .unwrap_err()
            .to_string()
            .contains("roots"));

        document.roots = vec!["root".into()];
        document.nodes = vec![DocumentNodeV2::Group {
            id: "root".into(),
            accessibility: DocumentAccessibilityV2 {
                name: "Root".into(),
                description: None,
            },
            children: (0..=MAX_DOCUMENT_V2_NODES)
                .map(|index| format!("n{index}"))
                .collect(),
        }];
        assert!(document
            .validate(&allowed(), TerminalScopePolicy::ReferencedSubset)
            .unwrap_err()
            .to_string()
            .contains("children"));

        document.roots = (0..17).map(|index| format!("n{index}")).collect();
        document.nodes = (0..17)
            .map(|index| DocumentNodeV2::Text {
                id: format!("n{index}"),
                accessibility: DocumentAccessibilityV2 {
                    name: "N".into(),
                    description: None,
                },
                text: "x".repeat(MAX_DOCUMENT_V2_TEXT_CHARS),
            })
            .collect();
        assert!(document
            .validate(&allowed(), TerminalScopePolicy::ReferencedSubset)
            .unwrap_err()
            .to_string()
            .contains("aggregate text"));
    }

    #[test]
    fn dependency_order_controls_and_zero_action_budget_are_enforced() {
        let mut value: serde_json::Value = serde_json::from_slice(&valid_bytes()).unwrap();
        value["dependencies"] = serde_json::json!([
            {"contract":"host.z.v1","scope_id":"s","revision":1,"generation":1},
            {"contract":"host.a.v1","scope_id":"s","revision":1,"generation":1}
        ]);
        assert!(HostDocumentV2::validate_json(
            &serde_json::to_vec(&value).unwrap(),
            &allowed(),
            TerminalScopePolicy::ReferencedSubset,
        )
        .is_err());

        value = serde_json::from_slice(&valid_bytes()).unwrap();
        value["nodes"][1]["text"] = serde_json::Value::String("unsafe\u{1b}[31m".into());
        assert!(HostDocumentV2::validate_json(
            &serde_json::to_vec(&value).unwrap(),
            &allowed(),
            TerminalScopePolicy::ReferencedSubset,
        )
        .is_err());

        value = serde_json::from_slice(&valid_bytes()).unwrap();
        value["actions"] = serde_json::json!([]);
        assert!(HostDocumentV2::validate_json(
            &serde_json::to_vec(&value).unwrap(),
            &allowed(),
            TerminalScopePolicy::ReferencedSubset,
        )
        .is_err());
    }
}
