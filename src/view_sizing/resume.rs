//! Transient auto-size scheduling fences, never persisted sizing policy.
use std::collections::HashMap;

use super::{ViewMeasure, ViewSizing};
use crate::{kitty_graphics::HostCellSize, layout::PaneId, workspace::TabId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResumeFact {
    pub measure: ViewMeasure,
    pub source: Option<(u64, String)>,
    pub available: bool,
}

pub(crate) struct ResumeFence {
    pub tab: TabId,
    pub parent_members: Vec<PaneId>,
    pub views: HashMap<PaneId, ResumeFact>,
    pub issued: HashMap<(String, String), (u64, u64)>,
    pub viewport: Option<(u16, u16, HostCellSize)>,
}

#[derive(Clone, Copy)]
pub(crate) struct ResumeEvent<'a> {
    pub namespace: &'a str,
    pub scope: &'a str,
    pub stamp: (u64, u64),
    /// Proven by the host's current request watermark, not a provider timestamp.
    pub issued: bool,
}

impl ViewSizing {
    pub(crate) fn fenced(&self, pane: PaneId) -> bool {
        self.resume_fences
            .iter()
            .any(|fence| fence.views.contains_key(&pane))
    }

    pub(crate) fn observe_resume_fact(
        &mut self,
        pane: PaneId,
        fact: &ResumeFact,
        event: Option<ResumeEvent<'_>>,
        local_metric: bool,
    ) -> bool {
        let mut released = false;
        self.resume_fences.retain(|fence| {
            let fresh = local_metric
                || event.is_some_and(|event| {
                    event.issued
                        && fence
                            .issued
                            .get(&(event.namespace.into(), event.scope.into()))
                            .is_none_or(|watermark| event.stamp > *watermark)
                });
            let changed = fence.views.get(&pane).is_some_and(|before| before != fact);
            if fresh && changed {
                released = true;
                false
            } else {
                true
            }
        });
        released
    }

    pub(crate) fn resume_viewport(
        &mut self,
        tab: &TabId,
        width: u16,
        height: u16,
        cells: HostCellSize,
    ) {
        let current = (width, height, cells);
        self.resume_fences.retain_mut(|fence| {
            if &fence.tab != tab {
                return true;
            }
            match fence.viewport {
                Some(before) => before == current,
                None => {
                    fence.viewport = Some(current);
                    true
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (ViewSizing, PaneId, ResumeFact) {
        let pane = PaneId::from_raw(1);
        let fact = ResumeFact {
            measure: ViewMeasure { rows: 2, width: 32 },
            source: Some((7, "session".into())),
            available: true,
        };
        let mut sizing = ViewSizing::default();
        sizing.resume_fences.push(ResumeFence {
            tab: TabId::alloc().unwrap(),
            parent_members: vec![pane],
            views: HashMap::from([(pane, fact.clone())]),
            issued: HashMap::from([(("sample".into(), "session".into()), (7, 8))]),
            viewport: Some((100, 40, HostCellSize::default())),
        });
        (sizing, pane, fact)
    }
    #[test]
    fn pre_expand_completion_and_equal_facts_cannot_resume_but_new_issued_changed_fact_can() {
        let (mut sizing, pane, before) = fixture();
        let mut changed = before.clone();
        changed.measure.rows = 10;
        assert!(!sizing.observe_resume_fact(
            pane,
            &changed,
            Some(ResumeEvent {
                namespace: "sample",
                scope: "session",
                stamp: (7, 8),
                issued: true
            }),
            false
        ));
        assert!(sizing.fenced(pane));
        assert!(!sizing.observe_resume_fact(
            pane,
            &before,
            Some(ResumeEvent {
                namespace: "sample",
                scope: "session",
                stamp: (7, 9),
                issued: true
            }),
            false
        ));
        assert!(!sizing.observe_resume_fact(
            pane,
            &changed,
            Some(ResumeEvent {
                namespace: "sample",
                scope: "session",
                stamp: (7, 9),
                issued: false
            }),
            false
        ));
        assert!(sizing.fenced(pane));
        assert!(sizing.observe_resume_fact(
            pane,
            &changed,
            Some(ResumeEvent {
                namespace: "sample",
                scope: "session",
                stamp: (7, 9),
                issued: true
            }),
            false
        ));
        assert!(!sizing.fenced(pane));
    }
    #[test]
    fn same_viewport_and_first_attachment_preserve_fence_but_changed_controller_geometry_releases()
    {
        let (mut sizing, pane, _) = fixture();
        let tab = sizing.resume_fences[0].tab.clone();
        sizing.resume_viewport(&tab, 100, 40, HostCellSize::default());
        assert!(sizing.fenced(pane));
        sizing.resume_fences[0].viewport = None;
        sizing.resume_viewport(&tab, 80, 24, HostCellSize::default());
        assert!(sizing.fenced(pane));
        sizing.resume_viewport(&tab, 81, 24, HostCellSize::default());
        assert!(!sizing.fenced(pane));
    }
}
