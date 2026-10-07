//! Explicit one-way v6/v7 layout conversion. This is the only reader of the
//! retired Collection-leaf plus membership-record representation.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

use super::snapshot::{CollectionSnapshot, DirectionSnapshot, LayoutSnapshot};
use crate::layout::CollectionId;

#[derive(Deserialize)]
enum LegacyLayout {
    Pane(u32),
    Collection(CollectionId),
    Split {
        direction: DirectionSnapshot,
        ratio: f32,
        first: Box<LegacyLayout>,
        second: Box<LegacyLayout>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyCollection {
    id: CollectionId,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    members: Vec<u32>,
    #[serde(default)]
    selected: Option<u32>,
    #[serde(default)]
    archived: Vec<u32>,
}

/// Translates legacy shape before current DTO decoding. Semantic faults use the
/// existing typed-layout recovery rule, so repairable files retain every pane.
/// Identity and public-number normalization remain independently fenced.
pub(super) fn convert_workspace(workspace: &mut Value) -> Result<(), String> {
    if let Some(tabs) = workspace.get_mut("tabs").and_then(Value::as_array_mut) {
        for (index, tab) in tabs.iter_mut().enumerate() {
            convert_tab(tab).map_err(|error| format!("tab {index}: {error}"))?;
        }
    } else if workspace.get("layout").is_some() {
        convert_tab(workspace)?;
    }
    Ok(())
}

fn convert_tab(tab: &mut Value) -> Result<(), String> {
    let layout: LegacyLayout = serde_json::from_value(
        tab.get("layout")
            .cloned()
            .ok_or("legacy tab has no layout")?,
    )
    .map_err(|error| format!("invalid legacy layout: {error}"))?;
    let saved: Vec<LegacyCollection> = serde_json::from_value(
        tab.get("collections")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([])),
    )
    .map_err(|error| format!("invalid legacy Collections: {error}"))?;
    let mut records = saved
        .into_iter()
        .map(|record| (record.id, record))
        .collect();
    let mut policies = Vec::new();
    let converted = convert_node(layout, &mut records, &mut policies);
    // Unplaced policies carry no host placement. Pane records are untouched and
    // typed recovery restores every surviving, unplaced pane.
    policies.sort_by_key(|policy| policy.id.raw());
    tab["layout"] = serde_json::to_value(converted).map_err(|error| error.to_string())?;
    tab["collections"] = serde_json::to_value(policies).map_err(|error| error.to_string())?;
    if let Some(focus) = tab.get_mut("focused_leaf") {
        if focus.get("type").and_then(Value::as_str) == Some("collection") {
            focus["type"] = Value::String("stack".into());
        }
    }
    Ok(())
}

fn convert_node(
    node: LegacyLayout,
    records: &mut HashMap<CollectionId, LegacyCollection>,
    policies: &mut Vec<CollectionSnapshot>,
) -> LayoutSnapshot {
    match node {
        LegacyLayout::Pane(pane) => LayoutSnapshot::Pane(pane),
        LegacyLayout::Collection(id) => {
            let record = records.remove(&id).unwrap_or(LegacyCollection {
                id,
                label: None,
                members: Vec::new(),
                selected: None,
                archived: Vec::new(),
            });
            // A missing/nonmember legacy selection meant the first member.
            let selected = record
                .selected
                .and_then(|pane| record.members.iter().position(|member| *member == pane))
                .unwrap_or(0);
            policies.push(CollectionSnapshot {
                id,
                label: record.label,
                archived: record.archived,
            });
            LayoutSnapshot::Stack {
                id,
                selected,
                children: record
                    .members
                    .into_iter()
                    .map(LayoutSnapshot::Pane)
                    .collect(),
            }
        }
        LegacyLayout::Split {
            direction,
            ratio,
            first,
            second,
        } => LayoutSnapshot::Split {
            direction,
            ratio,
            first: Box::new(convert_node(*first, records, policies)),
            second: Box::new(convert_node(*second, records, policies)),
        },
    }
}
