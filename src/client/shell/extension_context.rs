//! Extension-owned relationship context, not runtime task or delegation authority.
//! The public convention uses existing document fields; generation-1 codecs stay frozen.
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::extension_contract::{HostDocumentV1, HostItemRole, HostSurface, HostTarget};
use crate::protocol::endpoint::HostDocumentsProjectionV1;
use crate::protocol::ClientShellSnapshot;

pub(super) type ContextNavigation = HashMap<String, Vec<String>>;

pub(super) fn navigation(
    snapshot: &ClientShellSnapshot,
    projection: Option<&HostDocumentsProjectionV1>,
) -> ContextNavigation {
    let Some(projection) = projection.filter(|projection| {
        projection.boot_id == snapshot.boot_id && projection.revision == snapshot.revision
    }) else {
        return HashMap::new();
    };
    // Choose once per scope, never once per pane. Invalid winners fail closed for that scope.
    let mut winners = BTreeMap::new();
    for entry in &projection.documents {
        let document = &entry.document;
        if document.surface != HostSurface::Navigation
            || document.items.is_empty()
            || !document
                .items
                .iter()
                .any(|item| item.id.starts_with("context:"))
        {
            continue;
        }
        let winner = winners.entry(&document.target).or_insert(entry);
        if entry.provider_priority > winner.provider_priority
            || (entry.provider_priority == winner.provider_priority
                && document.namespace < winner.document.namespace)
        {
            *winner = entry;
        }
    }
    let panes = snapshot
        .panes
        .iter()
        .map(|pane| (pane.pane_id.as_str(), pane))
        .collect::<HashMap<_, _>>();
    let mut result: ContextNavigation = HashMap::new();
    for entry in winners.values() {
        if let Some(rows) = join(&entry.document, &panes) {
            for (pane, labels) in rows {
                let output = result.entry(pane).or_default();
                // Bound row expansion even with several session/workspace/tab providers.
                output.extend(labels.into_iter().take(3usize.saturating_sub(output.len())));
            }
        }
    }
    result
}

fn join(
    document: &HostDocumentV1,
    panes: &HashMap<&str, &crate::protocol::ClientShellPane>,
) -> Option<ContextNavigation> {
    if document.items.len() > 256
        || !document.actions.is_empty()
        || !document.required_capabilities.is_empty()
    {
        return None;
    }
    let items = document
        .items
        .iter()
        .map(|item| (item.id.as_str(), item))
        .collect::<HashMap<_, _>>();
    if items.len() != document.items.len() {
        return None;
    }
    let mut terminals = HashMap::new();
    let mut placements = HashMap::new();
    let mut rows: ContextNavigation = HashMap::new();
    let mut checked_parents = HashSet::new();
    for item in &document.items {
        // Open, namespaced edge types: task.owner, task.parent, thread.link, etc.
        // A type is a publisher's context claim, never a host-authorized operation.
        let (kind, key) = item.id.strip_prefix("context:")?.split_once(':')?;
        if key.is_empty()
            || kind.len() > 64
            || !kind.contains('.')
            || kind.split('.').any(str::is_empty)
            || !kind.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'-' | b'_')
            })
            || item.label.len() > 240
            || !item.action_ids.is_empty()
        {
            return None;
        }
        let mut seen = HashSet::new();
        let mut ancestor = Some(item.id.as_str());
        while let Some(id) = ancestor {
            if checked_parents.contains(id) {
                break;
            }
            if !seen.insert(id) {
                return None;
            }
            ancestor = items.get(id)?.parent_id.as_deref();
        }
        checked_parents.extend(seen);
        match item.role {
            HostItemRole::Section if item.terminal.is_none() => continue,
            HostItemRole::Terminal => {}
            _ => return None,
        }
        let terminal = item.terminal.as_ref()?;
        if terminal.terminal_id.is_empty() {
            return None;
        }
        let placement = terminal.placement.as_ref()?;
        let pane = panes.get(placement.pane_id.as_str())?;
        let scoped = match &document.target {
            HostTarget::Session => true,
            HostTarget::Workspace { workspace_id } => workspace_id == &pane.workspace_id,
            HostTarget::Tab { tab_id } => tab_id == &pane.tab_id,
            HostTarget::Pane { pane_id } => pane_id == &pane.pane_id,
        };
        if !scoped || pane.workspace_id != placement.workspace_id || pane.tab_id != placement.tab_id
        {
            return None;
        }
        if terminals
            .insert(&terminal.terminal_id, &placement.pane_id)
            .is_some_and(|previous| previous != &placement.pane_id)
            || placements
                .insert(&placement.pane_id, &terminal.terminal_id)
                .is_some_and(|previous| previous != &terminal.terminal_id)
        {
            return None;
        }
        if !item.flags.disabled {
            let labels = rows.entry(placement.pane_id.clone()).or_default();
            if labels.len() < 3 {
                labels.push(item.label.clone());
            }
        }
    }
    Some(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_contract::{
        HostDocumentItem, HostDocumentItemFlags, HostTerminalPlacement, HostTerminalRef,
    };
    use crate::protocol::endpoint::HostDocumentProjectionEntryV1;

    fn fixture() -> (ClientShellSnapshot, HostDocumentsProjectionV1) {
        let mut snapshot = super::super::tests::snapshot();
        snapshot.agents.push(crate::protocol::ClientShellAgent {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: Some("native owner".into()),
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: crate::api::schema::AgentStatus::Idle,
            state_change_seq: 1,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: true,
        });
        let item = HostDocumentItem {
            id: "context:task.owner:task-one".into(),
            parent_id: None,
            role: HostItemRole::Terminal,
            label: "Owner context".into(),
            detail: None,
            terminal: Some(HostTerminalRef {
                terminal_id: "term-one".into(),
                placement: Some(HostTerminalPlacement {
                    pane_id: "pane_1".into(),
                    workspace_id: "ws_1".into(),
                    tab_id: "tab_1".into(),
                }),
            }),
            action_ids: Vec::new(),
            flags: HostDocumentItemFlags::default(),
            view: None,
        };
        let projection = HostDocumentsProjectionV1 {
            boot_id: snapshot.boot_id.clone(),
            revision: snapshot.revision,
            documents: vec![HostDocumentProjectionEntryV1 {
                provider_priority: 0,
                confirmation_tokens: Default::default(),
                document: HostDocumentV1 {
                    contract: "host.document.v1".into(),
                    namespace: "external.work".into(),
                    generation: 1,
                    revision: 1,
                    surface: HostSurface::Navigation,
                    target: HostTarget::Tab {
                        tab_id: "tab_1".into(),
                    },
                    items: vec![item],
                    actions: Vec::new(),
                    required_capabilities: Vec::new(),
                },
            }],
        };
        (snapshot, projection)
    }

    #[test]
    fn extension_context_visible_without_activity_or_legacy_delegation() {
        let (snapshot, projection) = fixture();
        assert_eq!(
            navigation(&snapshot, Some(&projection))["pane_1"],
            ["Owner context"]
        );
        let config =
            super::super::ClientShellConfig::from_config(&crate::config::Config::default());
        let area = ratatui::layout::Rect::new(0, 0, 40, 20);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        super::super::agent_sidebar::render_agent_panel(
            &mut buffer,
            area,
            &snapshot,
            &config,
            Some(&projection),
            &mut 0,
            &mut super::super::ShellHitMap::default(),
        );
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Owner context"), "{text}");
        assert_eq!(
            snapshot.agents[0].agent_status,
            crate::api::schema::AgentStatus::Idle
        );
    }

    #[test]
    fn extension_context_rejects_stale_foreign_ambiguous_and_cyclic_documents() {
        let (snapshot, original) = fixture();
        for mutation in 0..8 {
            let mut projection = original.clone();
            let item = &mut projection.documents[0].document.items[0];
            match mutation {
                0 => projection.boot_id = "old-boot".into(),
                1 => projection.revision += 1,
                2 => {
                    item.terminal
                        .as_mut()
                        .unwrap()
                        .placement
                        .as_mut()
                        .unwrap()
                        .pane_id = "missing".into()
                }
                3 => {
                    item.terminal
                        .as_mut()
                        .unwrap()
                        .placement
                        .as_mut()
                        .unwrap()
                        .workspace_id = "foreign".into()
                }
                4 => item.parent_id = Some(item.id.clone()),
                5 => item.parent_id = Some("context:task.parent:missing".into()),
                6 => {
                    let duplicate = item.clone();
                    projection.documents[0].document.items.push(duplicate);
                }
                _ => item.id = "context:untyped:key".into(),
            }
            assert!(
                navigation(&snapshot, Some(&projection)).is_empty(),
                "mutation {mutation}"
            );
        }
        assert!(navigation(&snapshot, None).is_empty());
    }

    #[test]
    #[ignore = "supporting local performance evidence, not a CI timing gate"]
    fn public_extension_context_render_scale_profile() {
        use std::time::Instant;
        for pane_count in [1, 15] {
            for provider_count in [1, 15] {
                let (mut snapshot, mut projection) = fixture();
                let pane = snapshot.panes[0].clone();
                let agent = snapshot.agents[0].clone();
                let item = projection.documents[0].document.items[0].clone();
                snapshot.panes.clear();
                snapshot.agents.clear();
                projection.documents[0].document.items.clear();
                for index in 0..pane_count {
                    let pane_id = format!("pane_{index}");
                    let mut pane = pane.clone();
                    pane.pane_id = pane_id.clone();
                    let mut agent = agent.clone();
                    agent.pane_id = pane_id.clone();
                    let mut item = item.clone();
                    item.id = format!("context:task.owner:{index}");
                    let terminal = item.terminal.as_mut().unwrap();
                    terminal.terminal_id = format!("term-{index}");
                    terminal.placement.as_mut().unwrap().pane_id = pane_id;
                    snapshot.panes.push(pane);
                    snapshot.agents.push(agent);
                    projection.documents[0].document.items.push(item);
                }
                let template = projection.documents[0].clone();
                for index in 1..provider_count {
                    let mut provider = template.clone();
                    provider.document.namespace = format!("example.context-{index}");
                    projection.documents.push(provider);
                }
                let config =
                    super::super::ClientShellConfig::from_config(&crate::config::Config::default());
                let area = ratatui::layout::Rect::new(0, 0, 40, 120);
                let mut buffer = ratatui::buffer::Buffer::empty(area);
                let mut samples = Vec::new();
                for _ in 0..1000 {
                    let start = Instant::now();
                    super::super::agent_sidebar::render_agent_panel(
                        &mut buffer,
                        area,
                        &snapshot,
                        &config,
                        Some(&projection),
                        &mut 0,
                        &mut super::super::ShellHitMap::default(),
                    );
                    std::hint::black_box(&buffer);
                    samples.push(start.elapsed().as_nanos());
                }
                samples.sort_unstable();
                println!("PUBLIC_CONTEXT panes={pane_count} providers={provider_count} median_ns={} p95_ns={}", samples[500], samples[950]);
            }
        }
    }

    #[test]
    fn extension_context_priority_bounds_and_open_types() {
        let (snapshot, mut projection) = fixture();
        let mut high = projection.documents[0].clone();
        high.provider_priority = 2;
        high.document.namespace = "external.other-work".into();
        high.document.items[0].id = "context:custom.link:thread-one".into();
        high.document.items[0].label = "Thread context".into();
        projection.documents.push(high);
        assert_eq!(
            navigation(&snapshot, Some(&projection))["pane_1"],
            ["Thread context"]
        );
        let item = projection.documents[1].document.items[0].clone();
        for index in 0..6 {
            let mut extra = item.clone();
            extra.id = format!("context:thread.link:{index}");
            projection.documents[1].document.items.push(extra);
        }
        assert_eq!(navigation(&snapshot, Some(&projection))["pane_1"].len(), 3);
        projection.documents[1].document.items[0].parent_id = Some("missing".into());
        // Never fall back to a lower-priority provider after an invalid winning response.
        assert!(navigation(&snapshot, Some(&projection)).is_empty());
    }
}
