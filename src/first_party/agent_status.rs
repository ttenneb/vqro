//! Bundled Agent Status provider over neutral, attributed activity records.

use crate::extension_contract::{
    HostAgentActivityRecord, HostAgentActivityState, HostAgentActivityV1, HostDocumentError,
    HostDocumentItem, HostDocumentItemFlags, HostDocumentV1, HostItemRole, HostSurface, HostTarget,
    HOST_DOCUMENT_CONTRACT_V1,
};

pub(crate) const NAMESPACE: &str = "vqro.agent-status";
const DISPLAY_LABEL_KEYS: [&str; 3] = ["display", "name", "agent"];

pub(crate) fn project(snapshot: &HostAgentActivityV1) -> Result<HostDocumentV1, HostDocumentError> {
    snapshot.validate()?;
    let mut items = Vec::with_capacity(snapshot.activities.len() + 4);

    for state in [
        HostAgentActivityState::Blocked,
        HostAgentActivityState::Working,
        HostAgentActivityState::Idle,
        HostAgentActivityState::Unknown,
    ] {
        let mut activities = snapshot
            .activities
            .iter()
            .filter(|activity| activity.activity == state)
            .collect::<Vec<_>>();
        if activities.is_empty() {
            continue;
        }
        activities.sort_by(|left, right| {
            right
                .state_change_seq
                .cmp(&left.state_change_seq)
                .then_with(|| left.terminal.terminal_id.cmp(&right.terminal.terminal_id))
        });
        let state_id = state_id(state);
        items.push(HostDocumentItem {
            id: format!("activity:{state_id}"),
            parent_id: None,
            role: HostItemRole::Group,
            label: state_label(state).into(),
            detail: Some(format!(
                "{} agent{}",
                activities.len(),
                if activities.len() == 1 { "" } else { "s" }
            )),
            terminal: None,
            action_ids: Vec::new(),
            flags: HostDocumentItemFlags {
                expanded: true,
                attention: state == HostAgentActivityState::Blocked,
                ..Default::default()
            },
        });
        for activity in activities {
            items.push(activity_item(activity, state_id));
        }
    }

    let document = HostDocumentV1 {
        contract: HOST_DOCUMENT_CONTRACT_V1.into(),
        namespace: NAMESPACE.into(),
        generation: snapshot.generation,
        revision: snapshot.revision,
        surface: HostSurface::Navigation,
        target: HostTarget::Session,
        items,
        actions: Vec::new(),
        required_capabilities: Vec::new(),
    };
    document.validate()?;
    Ok(document)
}

fn activity_item(activity: &HostAgentActivityRecord, state_id: &str) -> HostDocumentItem {
    let label = DISPLAY_LABEL_KEYS
        .iter()
        .find_map(|key| activity.labels.get(*key))
        .filter(|label| !label.is_empty())
        .cloned()
        .unwrap_or_else(|| activity.terminal.terminal_id.clone());
    HostDocumentItem {
        id: format!("terminal:{}", activity.terminal.terminal_id),
        parent_id: Some(format!("activity:{state_id}")),
        role: HostItemRole::Terminal,
        label,
        detail: Some(state_label(activity.activity).into()),
        terminal: Some(activity.terminal.clone()),
        action_ids: Vec::new(),
        flags: HostDocumentItemFlags {
            attention: activity.activity == HostAgentActivityState::Blocked,
            ..Default::default()
        },
    }
}

fn state_id(state: HostAgentActivityState) -> &'static str {
    match state {
        HostAgentActivityState::Idle => "idle",
        HostAgentActivityState::Working => "working",
        HostAgentActivityState::Blocked => "blocked",
        HostAgentActivityState::Unknown => "unknown",
    }
}

fn state_label(state: HostAgentActivityState) -> &'static str {
    match state {
        HostAgentActivityState::Idle => "Idle",
        HostAgentActivityState::Working => "Working",
        HostAgentActivityState::Blocked => "Blocked",
        HostAgentActivityState::Unknown => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension_contract::{HostTerminalPlacement, HostTerminalRef};
    use std::collections::BTreeMap;

    fn snapshot() -> HostAgentActivityV1 {
        HostAgentActivityV1 {
            contract: "host.agent-activity.v1".into(),
            generation: 8,
            revision: 21,
            scope_id: "session_alpha".into(),
            activities: vec![
                activity("term_idle", HostAgentActivityState::Idle, 3, "Research"),
                activity("term_blocked", HostAgentActivityState::Blocked, 9, "Review"),
                activity("term_working", HostAgentActivityState::Working, 7, "Build"),
            ],
        }
    }

    fn activity(
        terminal_id: &str,
        state: HostAgentActivityState,
        state_change_seq: u64,
        label: &str,
    ) -> HostAgentActivityRecord {
        HostAgentActivityRecord {
            terminal: HostTerminalRef {
                terminal_id: terminal_id.into(),
                placement: Some(HostTerminalPlacement {
                    pane_id: format!("w1:p_{terminal_id}"),
                    workspace_id: "w1".into(),
                    tab_id: "w1:t1".into(),
                }),
            },
            activity: state,
            source: "vqro.host-agent-detection".into(),
            evidence_kind: "vqro.screen-state".into(),
            state_change_seq,
            labels: BTreeMap::from([("display".into(), label.into())]),
        }
    }

    #[test]
    fn agent_status_projection_matches_fixture() {
        let actual = serde_json::to_value(project(&snapshot()).unwrap()).unwrap();
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/agent-status-document-v1.json"
        ))
        .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn statuses_are_neutral_and_grouped_by_attention_order() {
        let document = project(&snapshot()).unwrap();
        assert_eq!(document.items[0].label, "Blocked");
        assert_eq!(document.items[2].label, "Working");
        assert_eq!(document.items[4].label, "Idle");
        assert!(!serde_json::to_string(&document).unwrap().contains("Done"));
    }
}
