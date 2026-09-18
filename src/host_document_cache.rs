//! Cached, validated generation-1 host-document publication.
//!
//! This cache is provider-neutral. It never renders a document or supplies a degraded fallback;
//! callers may publish only a document already produced and audited by its current authority.

use std::collections::HashSet;

use crate::extension_contract::{HostCapability, HostDocumentError, HostDocumentV1};

#[derive(Debug, Default)]
pub(crate) struct HostDocumentCache {
    document: Option<HostDocumentV1>,
    provider_priority: i32,
    grants: HashSet<HostCapability>,
    encoded_bytes: usize,
}

impl HostDocumentCache {
    pub(crate) fn publish_for_namespace(
        &mut self,
        expected_namespace: &str,
        expected_generation: u64,
        provider_priority: i32,
        granted_capabilities: &HashSet<HostCapability>,
        document: HostDocumentV1,
    ) -> Result<(), HostDocumentError> {
        document.validate()?;
        if document.namespace != expected_namespace || document.generation != expected_generation {
            return Err(HostDocumentError(
                "document provider or generation mismatch".into(),
            ));
        }
        if document
            .required_capabilities
            .iter()
            .any(|capability| !granted_capabilities.contains(capability))
        {
            return Err(HostDocumentError(
                "document capability was not granted".into(),
            ));
        }
        if self.document.as_ref().is_some_and(|current| {
            current.generation > document.generation
                || (current.generation == document.generation
                    && (current.revision > document.revision
                        || (current.revision == document.revision
                            && current.namespace != "host.fallback")))
        }) {
            return Err(HostDocumentError("stale document revision".into()));
        }
        self.encoded_bytes = serde_json::to_vec(&document)
            .map_err(|_| HostDocumentError("document is not serializable".into()))?
            .len();
        self.document = Some(document);
        self.provider_priority = provider_priority;
        self.grants = granted_capabilities.clone();
        Ok(())
    }

    pub(crate) fn disable(&mut self) {
        self.document = None;
        self.encoded_bytes = 0;
    }

    pub(crate) fn current(&self) -> Option<&HostDocumentV1> {
        self.document.as_ref()
    }

    pub(crate) fn provider_priority(&self) -> i32 {
        self.provider_priority
    }

    pub(crate) fn projected_encoded_bytes(&self) -> usize {
        let action_reserve = self
            .document
            .as_ref()
            .map_or(0, |document| document.actions.len().saturating_mul(256));
        self.encoded_bytes
            .saturating_add(action_reserve)
            .saturating_add(256)
    }

    pub(crate) fn grants(&self) -> &HashSet<HostCapability> {
        &self.grants
    }
}
