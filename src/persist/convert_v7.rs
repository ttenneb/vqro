//! Explicit one-way v6/v7 layout conversion. This is the only reader of the
//! retired Collection-leaf plus membership-record representation.

use std::collections::{HashMap, HashSet};

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

/// Converts the admitted legacy schema before current DTO decoding. Identity
/// and public-number normalization is a separate, independently fenced pass.
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
    if saved.len() > crate::admission::MAX_GROUPS_PER_TAB {
        return Err("legacy conversion refused: Collections exceed the per-tab limit".into());
    }
    let mut records = HashMap::new();
    for record in saved {
        let id = record.id;
        if records.insert(id, record).is_some() {
            return Err(format!(
                "legacy conversion refused: duplicate Collection {id:?}"
            ));
        }
    }
    let mut placed = HashSet::new();
    let mut policies = Vec::new();
    let converted = convert_node(layout, &mut records, &mut placed, &mut policies)?;
    if let Some(id) = records.keys().min_by_key(|id| id.raw()) {
        return Err(format!(
            "legacy conversion refused: Collection {id:?} has no layout placement"
        ));
    }
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
    placed: &mut HashSet<u32>,
    policies: &mut Vec<CollectionSnapshot>,
) -> Result<LayoutSnapshot, String> {
    match node {
        LegacyLayout::Pane(pane) => {
            if !placed.insert(pane) {
                return Err(format!(
                    "legacy conversion refused: duplicate pane {pane} placement"
                ));
            }
            Ok(LayoutSnapshot::Pane(pane))
        }
        LegacyLayout::Collection(id) => {
            let record = records.remove(&id).ok_or_else(|| {
                format!("legacy conversion refused: missing or multiply placed Collection {id:?}",)
            })?;
            if record.members.len() > crate::admission::MAX_GROUP_MEMBERS {
                return Err(format!(
                    "legacy conversion refused: Collection {id:?} exceeds member limit"
                ));
            }
            for pane in &record.members {
                if !placed.insert(*pane) {
                    return Err(format!(
                        "legacy conversion refused: Collection {id:?} duplicates pane {pane}"
                    ));
                }
            }
            for pane in &record.archived {
                if !record.members.contains(pane) {
                    return Err(format!("legacy conversion refused: Collection {id:?} archived pane {pane} is not a member"));
                }
            }
            let selected = match record.selected {
                Some(pane) => record.members.iter().position(|member| *member == pane)
                    .ok_or_else(|| format!("legacy conversion refused: Collection {id:?} selected pane {pane} is not a member"))?,
                // Legacy restore selected the first member when selection was absent.
                None => 0,
            };
            policies.push(CollectionSnapshot {
                id,
                label: record.label,
                archived: record.archived,
            });
            Ok(LayoutSnapshot::Stack {
                id,
                selected,
                children: record
                    .members
                    .into_iter()
                    .map(LayoutSnapshot::Pane)
                    .collect(),
            })
        }
        LegacyLayout::Split {
            direction,
            ratio,
            first,
            second,
        } => Ok(LayoutSnapshot::Split {
            direction,
            ratio,
            first: Box::new(convert_node(*first, records, placed, policies)?),
            second: Box::new(convert_node(*second, records, placed, policies)?),
        }),
    }
}
