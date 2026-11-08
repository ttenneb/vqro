use super::*;

#[derive(Debug)]
pub(crate) struct ClientComposedSurfacePatch {
    pub(crate) rows: Vec<crate::protocol::PaneSurfacePatchRow>,
    pub(crate) cursor: Option<crate::protocol::CursorState>,
}

#[derive(Debug)]
pub(crate) enum ClientPaneSurfacePatchOutcome {
    Rejected,
    Applied(Option<ClientComposedSurfacePatch>),
}

fn row_fits_frame(row: &crate::protocol::PaneSurfacePatchRow, frame: &FrameData) -> bool {
    row.x
        .saturating_add(row.cells.len().min(u16::MAX as usize) as u16)
        <= frame.width
        && row.y < frame.height
}

fn apply_row(row: &crate::protocol::PaneSurfacePatchRow, frame: &mut FrameData) -> bool {
    if !row_fits_frame(row, frame) {
        return false;
    }
    let start = usize::from(row.y) * usize::from(frame.width) + usize::from(row.x);
    let end = start + row.cells.len();
    if end > frame.cells.len() {
        return false;
    }
    frame.cells[start..end].clone_from_slice(&row.cells);
    true
}

fn apply_patch_to_surface(
    surface: &mut crate::protocol::PaneSurfaceFrame,
    patch: &crate::protocol::PaneSurfacePatch,
) -> bool {
    for row in &patch.rows {
        if !apply_row(row, &mut surface.frame) {
            return false;
        }
    }
    for updated in &patch.panes {
        let Some(existing) = surface
            .panes
            .iter_mut()
            .find(|pane| pane.pane_id == updated.pane_id)
        else {
            return false;
        };
        *existing = updated.clone();
    }
    surface.frame.cursor = patch.cursor.clone();
    surface.surface_revision = patch.surface_revision;
    true
}

fn fast_path_blocker(
    state: &ClientShellState,
    patch: &crate::protocol::PaneSurfacePatch,
) -> Option<&'static str> {
    if state.mode != ClientShellMode::Terminal {
        Some("client_surface_patch.fallback.mode")
    } else if state.inspector.is_some() {
        Some("client_surface_patch.fallback.inspector")
    } else if state.overlay.is_some() {
        Some("client_surface_patch.fallback.overlay")
    } else if state.endpoint_error.is_some() {
        Some("client_surface_patch.fallback.endpoint_error")
    } else if state.config_diagnostic.is_some() {
        Some("client_surface_patch.fallback.config_diagnostic")
    } else if state.server_notice.is_some() {
        Some("client_surface_patch.fallback.server_notice")
    } else if state.visible_endpoint_notice.is_some() {
        Some("client_surface_patch.fallback.endpoint_notice")
    } else if state.visible_notification.is_some() {
        Some("client_surface_patch.fallback.notification")
    } else if state.copy_feedback.is_some() {
        Some("client_surface_patch.fallback.copy_feedback")
    } else if state.link_hover_blocks_patch(patch) {
        Some("client_surface_patch.fallback.link_hover")
    } else if state.selection.is_some() {
        Some("client_surface_patch.fallback.selection")
    } else if state.copy_mode.is_some() {
        Some("client_surface_patch.fallback.copy_mode")
    } else if state.selection_highlight_clear_deadline.is_some() {
        Some("client_surface_patch.fallback.selection_deadline")
    } else if patch.panes.iter().any(|pane| {
        !state
            .hits
            .panes
            .iter()
            .any(|hit| hit.pane_id == pane.pane_id)
    }) {
        Some("client_surface_patch.fallback.pane_hits")
    } else {
        None
    }
}

fn pane_geometry_matches(
    left: &crate::protocol::PaneSurfacePane,
    right: &crate::protocol::PaneSurfacePane,
) -> bool {
    left.pane_id == right.pane_id
        && left.rect == right.rect
        && left.inner_rect == right.inner_rect
        && left.focused == right.focused
        && left.pixel_width == right.pixel_width
        && left.pixel_height == right.pixel_height
}

impl ClientShellState {
    /// Apply a terminal-cell patch.
    ///
    /// A patch carries no view descriptors: a placed view's identity and scope
    /// change only with a topology change, which forces a new full surface, so
    /// `pane_surface_views` is left exactly as the last full surface set it.
    pub(crate) fn apply_pane_surface_patch(
        &mut self,
        patch: crate::protocol::PaneSurfacePatch,
    ) -> ClientPaneSurfacePatchOutcome {
        let Some(current) = self.pane_surface.as_ref() else {
            return ClientPaneSurfacePatchOutcome::Rejected;
        };
        if patch.boot_id != current.boot_id
            || patch.projection_revision != current.projection_revision
            || patch.base_surface_revision != current.surface_revision
            || patch.surface_revision != current.surface_revision.saturating_add(1)
            || current.popup.is_some()
            || !current.graphics.placements.is_empty()
            || !current.graphics.retained_assets.is_empty()
        {
            return ClientPaneSurfacePatchOutcome::Rejected;
        }

        for updated in &patch.panes {
            let Some(existing) = current
                .panes
                .iter()
                .find(|pane| pane.pane_id == updated.pane_id)
            else {
                return ClientPaneSurfacePatchOutcome::Rejected;
            };
            if !pane_geometry_matches(existing, updated) {
                return ClientPaneSurfacePatchOutcome::Rejected;
            }
        }
        for row in &patch.rows {
            if !row_fits_frame(row, &current.frame)
                || row.cells.is_empty()
                || !patch.panes.iter().any(|pane| {
                    let terminal_row = row.x >= pane.inner_rect.x
                        && row.y >= pane.inner_rect.y
                        && row.y < pane.inner_rect.y.saturating_add(pane.inner_rect.height)
                        && row
                            .x
                            .saturating_add(row.cells.len().min(u16::MAX as usize) as u16)
                            <= pane.inner_rect.x.saturating_add(pane.inner_rect.width);
                    let scrollbar_rect = pane.scrollbar_rect.or_else(|| {
                        current
                            .panes
                            .iter()
                            .find(|existing| existing.pane_id == pane.pane_id)
                            .and_then(|existing| existing.scrollbar_rect)
                    });
                    let scrollbar_row = scrollbar_rect.is_some_and(|rect| {
                        row.x == rect.x
                            && row.y >= rect.y
                            && row.y < rect.y.saturating_add(rect.height)
                            && row.cells.len() == usize::from(rect.width)
                    });
                    terminal_row || scrollbar_row
                })
            {
                return ClientPaneSurfacePatchOutcome::Rejected;
            }
        }

        // Collection lists own their cells: a patch row that lands in one is
        // dropped, so chrome is never repainted over by a retained surface patch.
        let panel_rows = self.collection_list_areas.clone();
        // The list areas also decide the cursor: a full compose suppresses the terminal
        // cursor under a list, so the direct path must not paint one there either.
        let list_areas = self.collection_list_areas.clone();
        let fast_path_blocker = fast_path_blocker(self, &patch);
        if let Some(reason) = fast_path_blocker {
            crate::render_prof::event(reason);
        }
        let fast_path_area = fast_path_blocker.is_none().then(|| {
            let (cols, rows) = self.last_composed_size.unwrap_or_default();
            self.layout(cols, rows).pane_surface
        });
        // The cursor's band and the composed row its pane ends at, from the pane the
        // surface says is focused: a patch can carry only the panes it changed, so
        // the focused pane is often absent from it. A cursor past that row is not
        // visible, which is what a full compose decides too.
        let focused = self
            .pane_surface
            .as_ref()
            .and_then(|surface| surface.panes.iter().find(|pane| pane.focused));
        let composed_patch = fast_path_area.map(|area| {
            // The composed row the focused pane ends at: a cursor at or past it is
            // not visible, which is what a full compose decides too.
            let cursor_bottom = focused.map(|pane| {
                area.y
                    .saturating_add(pane.inner_rect.y)
                    .saturating_add(pane.inner_rect.height)
            });
            ClientComposedSurfacePatch {
                rows: patch
                    .rows
                    .iter()
                    .map(|row| crate::protocol::PaneSurfacePatchRow {
                        x: area.x.saturating_add(row.x),
                        y: area.y.saturating_add(row.y),
                        cells: row.cells.clone(),
                    })
                    // Only the panel's own cells are filtered: a row with no
                    // destination never reaches here, because the predicate above
                    // sends such a patch to the compose path.
                    .filter(|row| {
                        !panel_rows.iter().any(|panel| {
                            row.y >= panel.y
                                && row.y < panel.bottom()
                                && row.x < panel.right()
                                && row.x.saturating_add(row.cells.len() as u16) > panel.x
                        })
                    })
                    .collect(),
                cursor: patch.cursor.clone().map(|cursor| {
                    let y = area.y.saturating_add(cursor.y);
                    let x = area.x.saturating_add(cursor.x);
                    let under_a_list = list_areas.iter().any(|list| super::contains(*list, (x, y)));
                    let visible = cursor.visible
                        && cursor_bottom.is_none_or(|bottom| y < bottom)
                        && !under_a_list;
                    crate::protocol::CursorState {
                        x,
                        y,
                        visible,
                        shape: cursor.shape,
                    }
                }),
            }
        });
        if let Some(area) = fast_path_area {
            let applied = self
                .pane_surface
                .as_mut()
                .is_some_and(|surface| apply_patch_to_surface(surface, &patch));
            if !applied {
                return ClientPaneSurfacePatchOutcome::Rejected;
            }
            self.advance_collapse_patch(
                &patch.boot_id,
                patch.projection_revision,
                patch.surface_revision,
            );
            for updated in &patch.panes {
                let Some(hit) = self
                    .hits
                    .panes
                    .iter_mut()
                    .find(|hit| hit.pane_id == updated.pane_id)
                else {
                    continue;
                };
                hit.scrollbar_rect = updated.scrollbar_rect.map(|rect| {
                    Rect::new(
                        area.x.saturating_add(rect.x),
                        area.y.saturating_add(rect.y),
                        rect.width,
                        rect.height,
                    )
                });
                hit.scroll = updated.scroll.map(|metrics| crate::pane::ScrollMetrics {
                    offset_from_bottom: usize::try_from(metrics.offset_from_bottom)
                        .unwrap_or(usize::MAX),
                    max_offset_from_bottom: usize::try_from(metrics.max_offset_from_bottom)
                        .unwrap_or(usize::MAX),
                    viewport_rows: usize::try_from(metrics.viewport_rows).unwrap_or(usize::MAX),
                });
                hit.mouse_reporting = updated.mouse_reporting;
                hit.sgr_pixel_mouse = updated.sgr_pixel_mouse;
                hit.pixel_width = updated.pixel_width;
                hit.pixel_height = updated.pixel_height;
                if let (Some(target), Some(scroll)) = (
                    self.pane_scroll_targets.get(&updated.pane_id).copied(),
                    updated.scroll,
                ) {
                    let target = target
                        .min(usize::try_from(scroll.max_offset_from_bottom).unwrap_or(usize::MAX));
                    if usize::try_from(scroll.offset_from_bottom).unwrap_or(usize::MAX) == target {
                        self.pane_scroll_targets.remove(&updated.pane_id);
                    }
                }
            }
            self.invalidate_link_hover();
            self.reconcile_input_source();
        } else {
            let mut next = current.clone();
            if !apply_patch_to_surface(&mut next, &patch) {
                return ClientPaneSurfacePatchOutcome::Rejected;
            }
            self.reinstall_patched_surface(next);
        }
        ClientPaneSurfacePatchOutcome::Applied(composed_patch)
    }
}

#[cfg(test)]
pub(crate) fn apply_composed_surface_patch(
    frame: &FrameData,
    patch: ClientComposedSurfacePatch,
) -> Option<FrameData> {
    let mut next = frame.clone();
    for row in &patch.rows {
        if !apply_row(row, &mut next) {
            return None;
        }
    }
    next.cursor = patch.cursor;
    Some(next)
}
