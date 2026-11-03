//! Repair ambiguous persisted view references before terminal restore admission.
use std::collections::HashSet;

use super::snapshot::{LayoutSnapshot, SessionSnapshot};

pub(super) fn repair(snapshot: &mut SessionSnapshot) -> bool {
    fn repair_node(node: &mut LayoutSnapshot, terminals: &HashSet<u32>) -> bool {
        match node {
            LayoutSnapshot::View { pane, .. } if terminals.contains(pane) => {
                // A saved terminal row outranks an ambiguous view alias. Refer
                // to that SAME terminal, letting ordinary duplicate pruning
                // keep its placement. Do not allocate a new logical pane: even
                // malformed aliases must not push admitted terminals over caps.
                *node = LayoutSnapshot::Pane(*pane);
                true
            }
            LayoutSnapshot::Split { first, second, .. } => {
                let first_changed = repair_node(first, terminals);
                let second_changed = repair_node(second, terminals);
                first_changed || second_changed
            }
            LayoutSnapshot::Stack { children, .. } => {
                let mut changed = false;
                for child in children {
                    changed |= repair_node(child, terminals);
                }
                changed
            }
            _ => false,
        }
    }
    let mut changed = false;
    for workspace in &mut snapshot.workspaces {
        for tab in &mut workspace.tabs {
            let terminals = tab.panes.keys().copied().collect();
            changed |= repair_node(&mut tab.layout, &terminals);
        }
    }
    changed
}

#[cfg(test)]
mod tests;
