//! Cached actionless attention presentation. Category text/counts belong to the publisher.
use super::*;
use crate::extension_contract::{HostItemRole, HostSurface, HostTarget};
use crate::protocol::endpoint::HostDocumentsProjectionV1;

#[derive(Clone, Debug, Default)]
pub(super) struct AttentionNavigation {
    pub(super) plugin: String,
    pub(super) entrypoint: String,
    pub(super) badges: HashMap<(String, String), String>,
}

pub(super) fn navigation(projection: &HostDocumentsProjectionV1) -> Option<AttentionNavigation> {
    let document = &projection
        .documents
        .iter()
        .filter(|entry| {
            let doc = &entry.document;
            doc.surface == HostSurface::Navigation
                && doc.target == HostTarget::Session
                && doc.actions.is_empty()
                && doc.required_capabilities.is_empty()
                && doc.items.iter().all(|item| {
                    item.role == HostItemRole::Section
                        && item.id.starts_with("context:attention.")
                        && !item.flags.disabled
                })
                && doc
                    .items
                    .iter()
                    .any(|item| item.id.starts_with("context:attention.view:"))
        })
        .min_by(|left, right| {
            right
                .provider_priority
                .cmp(&left.provider_priority)
                .then_with(|| left.document.namespace.cmp(&right.document.namespace))
        })?
        .document;
    let launch = document
        .items
        .iter()
        .find_map(|item| item.id.strip_prefix("context:attention.view:"))?;
    let (plugin, entrypoint) = launch.split_once(':')?;
    if plugin.is_empty() || entrypoint.is_empty() || entrypoint.contains(':') {
        return None;
    }
    let mut result = AttentionNavigation {
        plugin: plugin.into(),
        entrypoint: entrypoint.into(),
        ..Default::default()
    };
    for item in &document.items {
        let Some(id) = item.id.strip_prefix("context:attention.badge:") else {
            continue;
        };
        let mut parts = id.rsplitn(4, ':');
        // Counts are opaque bounded publisher measures, not host-classified task facts.
        for _ in 0..3 {
            parts.next()?.parse::<u64>().ok()?;
        }
        let (kind, id) = parts.next()?.split_once(':')?;
        if !matches!(kind, "workspace" | "tab" | "session") {
            return None;
        }
        if result
            .badges
            .insert((kind.into(), id.into()), item.label.clone())
            .is_some()
        {
            return None;
        }
    }
    Some(result)
}

impl ClientShellState {
    pub(super) fn open_attention(&mut self, launch: AttentionHit, outcome: &mut ClientShellInput) {
        let current = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .filter(|endpoint| endpoint.current_host_documents().is_some())
            .and_then(|endpoint| endpoint.attention.as_ref());
        if !current
            .is_some_and(|nav| nav.plugin == launch.plugin && nav.entrypoint == launch.entrypoint)
        {
            return;
        }
        self.push_endpoint_method(
            crate::api::schema::Method::PluginPaneOpen(crate::api::schema::PluginPaneOpenParams {
                plugin_id: launch.plugin,
                entrypoint: launch.entrypoint,
                placement: Some(crate::api::schema::PluginPanePlacement::Tab),
                width: None,
                height: None,
                workspace_id: self
                    .snapshot
                    .as_deref()
                    .and_then(|snapshot| snapshot.focused_workspace_id.clone()),
                target_pane_id: None,
                direction: None,
                cwd: None,
                focus: true,
                env: HashMap::from([(
                    "VQRO_ATTENTION_SCOPE".into(),
                    serde_json::json!({"kind": launch.kind, "ids": launch.ids}).to_string(),
                )]),
            }),
            outcome,
        );
    }
}

#[derive(Clone, Debug)]
pub(super) struct AttentionHit {
    pub(super) rect: Rect,
    pub(super) plugin: String,
    pub(super) entrypoint: String,
    pub(super) kind: String,
    pub(super) ids: Vec<String>,
}
