//! Bounded neutral presentation metrics and transient opt-in sizing policy.
//! No provider calls, clock text, persistence, or client-frame work lives here.
use crate::{
    extension_contract::{HostDocumentV1, HostViewWaiting},
    layout::PaneId,
};
use std::collections::HashMap;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ViewMeasure {
    pub rows: u16,
    pub width: u16,
}
impl Default for ViewMeasure {
    fn default() -> Self {
        Self { rows: 1, width: 32 }
    }
}
#[derive(Clone)]
pub(crate) struct AutoView {
    pub extension: String,
    pub view: String,
    pub terminal: PaneId,
    pub measure: ViewMeasure,
    pub incarnation: Option<(u64, String, u64)>,
}
#[derive(Default)]
pub(crate) struct ViewSizing {
    pub active: HashMap<PaneId, AutoView>,
    pub viewports: HashMap<
        crate::workspace::TabId,
        (ratatui::layout::Rect, crate::kitty_graphics::HostCellSize),
    >,
}
impl ViewSizing {
    pub fn activate(&mut self, pane: PaneId, extension: &str, view: &str, terminal: PaneId) {
        self.active.entry(pane).or_insert_with(|| AutoView {
            extension: extension.into(),
            view: view.into(),
            terminal,
            measure: ViewMeasure::default(),
            incarnation: None,
        });
    }
    pub fn manual(&mut self, panes: impl IntoIterator<Item = PaneId>) {
        for pane in panes {
            self.active.remove(&pane);
        }
    }
}

/// One bounded scan per accepted document/root, never per frame or client.
/// Fixed status budgets avoid age/status ticks changing terminal dimensions.
pub(crate) fn measure_document(
    document: &HostDocumentV1,
) -> HashMap<(String, String), ViewMeasure> {
    let mut children: HashMap<&str, Vec<&crate::extension_contract::HostDocumentItem>> =
        HashMap::new();
    for item in &document.items {
        if let Some(parent) = item.parent_id.as_deref() {
            children.entry(parent).or_default().push(item);
        }
    }
    let mut result = HashMap::new();
    for root in &document.items {
        let Some(view) = &root.view else {
            continue;
        };
        let (Some(pane), Some(view_id)) = (&view.pane_id, &view.view_id) else {
            continue;
        };
        if root.flags.disabled {
            continue;
        }
        let mut rows = 0u16;
        let mut width = root
            .label
            .width()
            .saturating_add(root.detail.as_deref().unwrap_or("").width())
            .saturating_add(22);
        let mut pending = vec![(root.id.as_str(), 0usize)];
        let mut budget = document.items.len().min(512);
        while let Some((id, depth)) = pending.pop() {
            if budget == 0 || depth > 8 {
                break;
            }
            budget -= 1;
            if let Some(items) = children.get(id) {
                for item in items.iter().rev() {
                    rows = rows.saturating_add(1);
                    let status = if item
                        .view
                        .as_ref()
                        .is_some_and(|view| view.waiting == Some(HostViewWaiting::Them))
                    {
                        96
                    } else {
                        26
                    };
                    width = width.max(item.label.width().saturating_add(depth * 2 + status + 8));
                    pending.push((item.id.as_str(), depth + 1));
                }
            }
        }
        // Four-cell buckets are stable under small wording changes. Revision
        // identity is tracked separately, so equal facts never dirty ratios.
        let width = width.min(4092).div_ceil(4) * 4;
        result.insert(
            (view_id.clone(), pane.clone()),
            ViewMeasure {
                rows: rows.max(1),
                width: width as u16,
            },
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepted_metrics_ignore_age_ticks_and_count_neutral_nested_rows() {
        let mut document: HostDocumentV1 = serde_json::from_value(serde_json::json!({"contract":"host.document.v1","namespace":"sample.views","generation":1,"revision":1,"surface":"pane","target":{"kind":"tab","tab_id":"w1:t1"},"items":[
            {"id":"root","role":"section","label":"Tasks","view":{"kind":"section","view_id":"rows","pane_id":"w1:p1"}},
            {"id":"a","parent_id":"root","role":"status","label":"stable row","view":{"kind":"row","since":1,"waiting":"you","action":"start"}},
            {"id":"b","parent_id":"a","role":"status","label":"nested row","view":{"kind":"row"}}
        ]})).unwrap();
        let before = measure_document(&document);
        document.revision = 999;
        document.items[1].view.as_mut().unwrap().since = Some(999_999);
        assert_eq!(measure_document(&document), before);
        let measure = before.get(&("rows".into(), "w1:p1".into())).unwrap();
        assert_eq!(measure.rows, 2);
        assert_eq!(measure.width % 4, 0);
        document.items[0].view.as_mut().unwrap().pane_id = Some("w1:p2".into());
        let after = measure_document(&document);
        assert!(!after.contains_key(&("rows".into(), "w1:p1".into())));
        assert!(after.contains_key(&("rows".into(), "w1:p2".into())));
    }

    #[test]
    fn manual_choices_are_not_rearmed_by_another_default_view() {
        let a = PaneId::from_raw(1);
        let b = PaneId::from_raw(2);
        let terminal = PaneId::from_raw(3);
        let mut state = ViewSizing::default();
        state.activate(a, "sample", "rows", terminal);
        state.manual([a]);
        state.activate(b, "sample", "rows", terminal);
        assert!(!state.active.contains_key(&a));
        assert!(state.active.contains_key(&b));
        assert!(
            ViewSizing::default().active.is_empty(),
            "restore starts without transient auto policies"
        );
    }
}
