use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PaneContributionGeometry {
    pub(super) terminal: Option<Rect>,
    pub(super) divider: Option<Rect>,
    pub(super) contribution: Rect,
}

pub(super) fn contribution_geometry(
    area: Rect,
    placement: crate::pane_contribution::PaneContributionPlacement,
) -> PaneContributionGeometry {
    if placement == crate::pane_contribution::PaneContributionPlacement::Replacement
        || area.width < 24
        || area.height < 15
    {
        return PaneContributionGeometry {
            terminal: None,
            divider: None,
            contribution: area,
        };
    }
    if area.width >= 72 {
        let contribution_width = (area.width / 2).clamp(28, 48);
        let terminal_width = area.width.saturating_sub(contribution_width + 1);
        return PaneContributionGeometry {
            terminal: Some(Rect::new(area.x, area.y, terminal_width, area.height)),
            divider: Some(Rect::new(
                area.x.saturating_add(terminal_width),
                area.y,
                1,
                area.height,
            )),
            contribution: Rect::new(
                area.x.saturating_add(terminal_width + 1),
                area.y,
                contribution_width,
                area.height,
            ),
        };
    }
    let contribution_height = (area.height / 2).clamp(10, 16);
    let terminal_height = area.height.saturating_sub(contribution_height + 1);
    PaneContributionGeometry {
        terminal: Some(Rect::new(area.x, area.y, area.width, terminal_height)),
        divider: Some(Rect::new(
            area.x,
            area.y.saturating_add(terminal_height),
            area.width,
            1,
        )),
        contribution: Rect::new(
            area.x,
            area.y.saturating_add(terminal_height + 1),
            area.width,
            contribution_height,
        ),
    }
}

pub(super) fn viewport_request(
    boot_id: &str,
    projection_revision: u64,
    pane_id: &str,
    terminal: Option<Rect>,
) -> crate::protocol::endpoint::ClientPaneViewportRequest {
    crate::protocol::endpoint::ClientPaneViewportRequest {
        boot_id: boot_id.into(),
        projection_revision,
        pane_id: pane_id.into(),
        size: terminal.map(|rect| crate::protocol::ClientSurfaceSize {
            cols: rect.width.max(1),
            rows: rect.height.max(1),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessory_preserves_terminal_across_wide_and_narrow_panes() {
        let wide = contribution_geometry(
            Rect::new(2, 3, 100, 30),
            crate::pane_contribution::PaneContributionPlacement::Accessory,
        );
        assert_eq!(wide.terminal, Some(Rect::new(2, 3, 51, 30)));
        assert_eq!(wide.divider, Some(Rect::new(53, 3, 1, 30)));
        assert_eq!(wide.contribution, Rect::new(54, 3, 48, 30));

        let narrow = contribution_geometry(
            Rect::new(2, 3, 40, 30),
            crate::pane_contribution::PaneContributionPlacement::Accessory,
        );
        assert_eq!(narrow.terminal, Some(Rect::new(2, 3, 40, 14)));
        assert_eq!(narrow.divider, Some(Rect::new(2, 17, 40, 1)));
        assert_eq!(narrow.contribution, Rect::new(2, 18, 40, 15));
    }

    #[test]
    fn replacement_owns_the_target_area_and_has_no_terminal_viewport() {
        let geometry = contribution_geometry(
            Rect::new(2, 3, 80, 24),
            crate::pane_contribution::PaneContributionPlacement::Replacement,
        );
        assert_eq!(geometry.terminal, None);
        assert_eq!(geometry.divider, None);
        assert_eq!(geometry.contribution, Rect::new(2, 3, 80, 24));
    }
}
