//! Repair ambiguous persisted view references before terminal restore admission.
use std::collections::HashSet;

use super::snapshot::{LayoutSnapshot, SessionSnapshot};

pub(super) fn repair(snapshot: &mut SessionSnapshot) -> Result<bool, String> {
    fn collect(node: &LayoutSnapshot, occupied: &mut HashSet<u32>, terminals: &mut HashSet<u32>) {
        match node {
            LayoutSnapshot::Pane(id) => {
                occupied.insert(*id);
                terminals.insert(*id);
            }
            LayoutSnapshot::View { pane, .. } => {
                occupied.insert(*pane);
            }
            LayoutSnapshot::Split { first, second, .. } => {
                collect(first, occupied, terminals);
                collect(second, occupied, terminals);
            }
            LayoutSnapshot::Stack { children, .. } => {
                for child in children {
                    collect(child, occupied, terminals);
                }
            }
        }
    }
    let mut occupied = HashSet::new();
    let mut terminals = HashSet::new();
    for workspace in &snapshot.workspaces {
        for tab in &workspace.tabs {
            occupied.extend(tab.panes.keys().copied());
            terminals.extend(tab.panes.keys().copied());
            collect(&tab.layout, &mut occupied, &mut terminals);
        }
    }
    fn repair_node(
        node: &mut LayoutSnapshot,
        terminals: &HashSet<u32>,
        occupied: &mut HashSet<u32>,
        next: &mut u32,
    ) -> Result<bool, String> {
        match node {
            LayoutSnapshot::View { pane, .. } if terminals.contains(pane) => {
                // A real terminal record (even one no longer referenced by the
                // tree) outranks a malformed view alias. Give only the view a
                // fresh structural reference; never synthesize a terminal for it.
                while occupied.contains(next) {
                    *next = next.checked_add(1).ok_or_else(|| {
                        "pane identity space exhausted repairing view alias".to_owned()
                    })?;
                }
                *pane = *next;
                occupied.insert(*pane);
                Ok(true)
            }
            LayoutSnapshot::Split { first, second, .. } => {
                let first_changed = repair_node(first, terminals, occupied, next)?;
                let second_changed = repair_node(second, terminals, occupied, next)?;
                Ok(first_changed || second_changed)
            }
            LayoutSnapshot::Stack { children, .. } => {
                let mut changed = false;
                for child in children {
                    changed |= repair_node(child, terminals, occupied, next)?;
                }
                Ok(changed)
            }
            _ => Ok(false),
        }
    }
    let mut changed = false;
    let mut next = 1;
    for workspace in &mut snapshot.workspaces {
        for tab in &mut workspace.tabs {
            changed |= repair_node(&mut tab.layout, &terminals, &mut occupied, &mut next)?;
        }
    }
    Ok(changed)
}
