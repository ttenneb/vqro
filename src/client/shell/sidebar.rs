use super::*;
use ratatui::{
    text::Line,
    widgets::{Paragraph, Widget},
};

pub(in crate::client::shell) fn collapsed_sidebar_sections(
    area: Rect,
) -> (Rect, Option<u16>, Rect) {
    let content = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
    if content.is_empty() {
        return (Rect::default(), None, Rect::default());
    }
    if content.height < 7 {
        return (content, None, Rect::default());
    }
    let workspace_height = content.height.div_ceil(2);
    let divider_y = content.y + workspace_height;
    let detail_height = content.height.saturating_sub(workspace_height + 1);
    (
        Rect::new(content.x, content.y, content.width, workspace_height),
        Some(divider_y),
        Rect::new(content.x, divider_y + 1, content.width, detail_height),
    )
}

pub(in crate::client::shell) fn workspace_projection_index(
    snapshot: &ClientShellSnapshot,
) -> HashMap<&str, &ClientShellWorkspace> {
    snapshot
        .workspaces
        .iter()
        .map(|workspace| (workspace.workspace_id.as_str(), workspace))
        .collect()
}

pub(crate) fn render_collapsed_sidebar(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    structure: Option<&crate::protocol::endpoint::ClientShellStructureProjection>,
    host_documents: Option<&crate::protocol::endpoint::HostDocumentsProjectionV1>,
    config: &ClientShellConfig,
    selected_workspace_id: Option<&str>,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    render_sidebar_background(buffer, area, palette);
    let (workspace_area, divider_y, detail_area) = collapsed_sidebar_sections(area);
    let collapsed_repositories = structure
        .filter(|structure| !structure.repositories.is_empty() || !structure.space_order.is_empty())
        .map(|structure| {
            structure
                .repositories
                .iter()
                .map(|repository| repository.repository_id.clone())
                .collect::<HashSet<_>>()
        })
        .unwrap_or_else(|| {
            snapshot
                .workspaces
                .iter()
                .enumerate()
                .filter_map(|(index, _)| parent_group_key(snapshot, index))
                .collect()
        });
    let spaces = workspace_entries_with_structure(
        snapshot,
        structure,
        host_documents,
        &collapsed_repositories,
    );
    let workspace_index = workspace_projection_index(snapshot);
    for (index, entry) in spaces
        .iter()
        .filter(|entry| !entry.indented)
        .take(workspace_area.height as usize)
        .enumerate()
    {
        let Some(workspace) = snapshot.workspaces.get(entry.index) else {
            continue;
        };
        let rect = Rect::new(
            workspace_area.x,
            workspace_area.y + index as u16,
            workspace_area.width,
            1,
        );
        let repository = entry
            .repository_index
            .and_then(|index| structure?.repositories.get(index));
        let selected = selected_workspace_id == Some(workspace.workspace_id.as_str())
            || repository.is_some_and(|repository| {
                repository
                    .checkouts
                    .iter()
                    .any(|checkout| selected_workspace_id == Some(checkout.workspace_id.as_str()))
            });
        let repository_workspace = |workspace_id: &str| workspace_index.get(workspace_id).copied();
        let focused = repository.map_or(workspace.focused, |repository| {
            repository.checkouts.iter().any(|checkout| {
                repository_workspace(&checkout.workspace_id)
                    .is_some_and(|workspace| workspace.focused)
            })
        });
        let target_workspace_id = repository
            .and_then(|repository| repository.last_focused_workspace_id.as_deref())
            .filter(|workspace_id| repository_workspace(workspace_id).is_some())
            .or_else(|| {
                repository.and_then(|repository| {
                    repository.checkouts.iter().find_map(|checkout| {
                        repository_workspace(&checkout.workspace_id)
                            .filter(|workspace| workspace.focused)
                            .map(|workspace| workspace.workspace_id.as_str())
                    })
                })
            })
            .unwrap_or(&workspace.workspace_id);
        let selection_background =
            if focused && palette.selection_bg == ratatui::style::Color::Reset {
                palette.active_row_bg
            } else {
                palette.selection_bg
            };
        if selected {
            buffer.set_style(rect, Style::default().bg(selection_background));
        } else if focused {
            buffer.set_style(rect, Style::default().bg(palette.active_row_bg));
        }
        let number_style = if selected {
            Style::default()
                .fg(palette.overlay1)
                .bg(selection_background)
        } else if focused {
            Style::default().fg(palette.text).bg(palette.active_row_bg)
        } else {
            Style::default().fg(palette.overlay0)
        };
        put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width.min(2),
            &format!("{:<2}", index + 1),
            number_style,
        );
        let status = projected_workspace_status(
            snapshot,
            workspace,
            entry,
            structure,
            &collapsed_repositories,
            projected_repository_is_collapsed(entry, structure, &collapsed_repositories),
            Some(&workspace_index),
        );
        put_text(
            buffer,
            rect.x.saturating_add(2),
            rect.y,
            rect.width.saturating_sub(2),
            status_icon(status, config.status_indicators),
            Style::default().fg(status_color(status, palette)),
        );
        hits.workspaces.push(WorkspaceHit {
            rect,
            endpoint_id: ClientEndpointId::Local,
            workspace_id: target_workspace_id.to_owned(),
            move_workspace_id: workspace.workspace_id.clone(),
            indented: false,
            group_toggle: None,
        });
    }

    if let Some(divider_y) = divider_y {
        put_text(
            buffer,
            workspace_area.x,
            divider_y,
            workspace_area.width,
            &"─".repeat(workspace_area.width as usize),
            Style::default().fg(palette.surface_dim),
        );
    }

    let detail_content = Rect::new(
        detail_area.x,
        detail_area.y,
        detail_area.width,
        detail_area.height.saturating_sub(1),
    );
    for (index, pane_id) in super::ordered_agent_pane_ids(snapshot, config.agent_panel_sort)
        .into_iter()
        .take(detail_content.height as usize)
        .enumerate()
    {
        let Some(agent) = snapshot
            .agents
            .iter()
            .find(|agent| agent.pane_id == pane_id)
        else {
            continue;
        };
        let rect = Rect::new(
            detail_content.x,
            detail_content.y + index as u16,
            detail_content.width,
            1,
        );
        if agent.focused {
            buffer.set_style(rect, Style::default().bg(palette.active_row_bg));
        }
        put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width.min(2),
            &format!("{:<2}", index + 1),
            Style::default().fg(if agent.focused {
                palette.text
            } else {
                palette.overlay0
            }),
        );
        put_text(
            buffer,
            rect.x.saturating_add(2),
            rect.y,
            rect.width.saturating_sub(2),
            status_icon(agent.agent_status, config.status_indicators),
            Style::default().fg(status_color(agent.agent_status, palette)),
        );
        hits.agents.push((rect, pane_id));
    }
    hits.sidebar_toggle = if area.is_empty() || workspace_area.width == 0 {
        Rect::default()
    } else {
        Rect::new(
            workspace_area.x + workspace_area.width / 2,
            area.bottom().saturating_sub(1),
            1,
            1,
        )
    };
    put_text(
        buffer,
        hits.sidebar_toggle.x,
        hits.sidebar_toggle.y,
        hits.sidebar_toggle.width,
        "»",
        if super::super::global_menu::global_menu_attention(snapshot) {
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.overlay0)
        },
    );
}

pub(crate) fn render_sidebar(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    state: &mut ShellRenderState<'_>,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    render_sidebar_background(buffer, area, palette);
    hits.sidebar_divider = if area.is_empty() {
        Rect::default()
    } else {
        Rect::new(area.right().saturating_sub(1), area.y, 1, area.height)
    };
    let (workspace_area, detail_area) =
        crate::ui::expanded_sidebar_sections(area, state.sidebar_section_split);
    hits.sidebar_section_divider =
        crate::ui::sidebar_section_divider_rect(area, state.sidebar_section_split);
    put_text(
        buffer,
        workspace_area.x,
        workspace_area.y,
        workspace_area.width,
        " spaces",
        Style::default()
            .fg(palette.overlay0)
            .add_modifier(Modifier::BOLD),
    );

    let endpoint = state
        .endpoints
        .iter()
        .find(|endpoint| &endpoint.endpoint_id == state.active_endpoint_id);
    let structure = endpoint.and_then(|endpoint| endpoint.current_structure());
    let host_documents = endpoint.and_then(|endpoint| endpoint.current_host_documents());
    let entries = workspace_entries_with_structure(
        snapshot,
        structure,
        host_documents,
        state.collapsed_groups,
    );
    let workspace_index = workspace_projection_index(snapshot);
    let body = Rect::new(
        workspace_area.x,
        workspace_area.y.saturating_add(WORKSPACE_HEADER_ROWS),
        workspace_area.width,
        workspace_area
            .height
            .saturating_sub(WORKSPACE_HEADER_ROWS + 1),
    );
    hits.workspace_body = body;
    let row_heights = entries
        .iter()
        .map(|entry| {
            snapshot
                .workspaces
                .get(entry.index)
                .map(|workspace| {
                    projected_workspace_rows(
                        workspace,
                        projected_workspace_status(
                            snapshot,
                            workspace,
                            entry,
                            structure,
                            state.collapsed_groups,
                            projected_repository_is_collapsed(
                                entry,
                                structure,
                                state.collapsed_groups,
                            ),
                            Some(&workspace_index),
                        ),
                        entry,
                        structure,
                        &config.spaces,
                    )
                    .len()
                    .max(1)
                    .min(u16::MAX as usize) as u16
                })
                .unwrap_or(1)
        })
        .collect::<Vec<_>>();
    let gaps = entries
        .iter()
        .enumerate()
        .map(|(index, _)| {
            entries
                .get(index + 1)
                .map_or(0, |next| u16::from(!next.indented) * config.spaces.row_gap)
        })
        .collect::<Vec<_>>();
    let mut metrics = super::scroll::list_scroll_metrics(
        &row_heights,
        &gaps,
        body.height,
        *state.workspace_scroll,
    );
    if !body.is_empty() && std::mem::take(state.reveal_focused_workspace) {
        if let Some(target) = entries
            .iter()
            .position(|entry| snapshot.workspaces[entry.index].focused)
        {
            *state.workspace_scroll = super::scroll::list_scroll_start_to_reveal(
                &row_heights,
                &gaps,
                body.height,
                *state.workspace_scroll,
                target,
            );
            metrics = super::scroll::list_scroll_metrics(
                &row_heights,
                &gaps,
                body.height,
                *state.workspace_scroll,
            );
        }
    }
    hits.workspace_max_scroll = metrics.max_offset_from_bottom;
    hits.workspace_scroll_metrics = Some(metrics);
    *state.workspace_scroll = metrics
        .max_offset_from_bottom
        .saturating_sub(metrics.offset_from_bottom);
    let show_scrollbar = metrics.max_offset_from_bottom > 0 && body.width > 1;
    let content_width = body.width.saturating_sub(u16::from(show_scrollbar));
    let mut y = body.y;
    for (entry_position, entry) in entries.iter().enumerate().skip(*state.workspace_scroll) {
        let Some(workspace) = snapshot.workspaces.get(entry.index) else {
            continue;
        };
        let repository_collapsed =
            projected_repository_is_collapsed(entry, structure, state.collapsed_groups);
        let navigation_target =
            projected_workspace_target(snapshot, workspace, entry, structure, repository_collapsed);
        let status = projected_workspace_status(
            snapshot,
            workspace,
            entry,
            structure,
            state.collapsed_groups,
            repository_collapsed,
            Some(&workspace_index),
        );
        let rows = projected_workspace_rows(workspace, status, entry, structure, &config.spaces);
        let row_height = (rows.len().max(1).min(u16::MAX as usize) as u16).min(body.height);
        if y.saturating_add(row_height) > body.bottom() {
            break;
        }
        let rect = Rect::new(body.x, y, content_width, row_height);
        let selected = state
            .selected_workspace_id
            .is_some_and(|target| target.matches(state.active_endpoint_id, &navigation_target));
        let dragged = state.dragged_workspace_id == Some(workspace.workspace_id.as_str());
        let focused = if repository_collapsed {
            entry
                .repository_index
                .and_then(|index| structure?.repositories.get(index))
                .is_some_and(|repository| {
                    repository.checkouts.iter().any(|checkout| {
                        workspace_index
                            .get(checkout.workspace_id.as_str())
                            .is_some_and(|candidate| candidate.focused)
                    })
                })
        } else {
            workspace.focused
        };
        if selected {
            buffer.set_style(rect, Style::default().bg(palette.selection_bg));
        } else if dragged {
            buffer.set_style(rect, Style::default().bg(palette.surface1));
        } else if focused {
            buffer.set_style(rect, Style::default().bg(palette.active_row_bg));
        }
        let repository = entry
            .repository_index
            .and_then(|index| structure?.repositories.get(index));
        let checkout = repository.and_then(|repository| {
            repository
                .checkouts
                .iter()
                .find(|checkout| checkout.workspace_id == workspace.workspace_id)
        });
        let rendered_links = render_workspace_rows(
            buffer,
            rect,
            status,
            config.status_indicators,
            entry,
            rows,
            focused,
            selected,
            dragged,
            palette,
            WorkspaceLinkLabels {
                repository: (!entry.indented)
                    .then_some(entry.label.as_deref())
                    .flatten()
                    .or_else(|| repository.map(|repository| repository.label.as_str())),
                branch: entry
                    .indented
                    .then_some(entry.label.as_deref())
                    .flatten()
                    .or(workspace.branch.as_deref()),
                upstream: checkout.and_then(|checkout| checkout.upstream.as_deref()),
            },
        );
        if let Some((link_rect, path)) = rendered_links
            .repository
            .zip(repository.and_then(|repository| repository.repository_path.as_ref()))
        {
            hits.space_links.push(SpaceLinkHit {
                rect: link_rect,
                target: SpaceLinkTarget::LocalDirectory(path.clone()),
            });
        }
        if let Some((link_rect, path)) = rendered_links.branch.zip(
            checkout
                .and_then(|checkout| checkout.checkout_path.as_ref())
                .or_else(|| repository.and_then(|repository| repository.repository_path.as_ref())),
        ) {
            hits.space_links.push(SpaceLinkHit {
                rect: link_rect,
                target: SpaceLinkTarget::LocalDirectory(path.clone()),
            });
        }
        if let Some((link_rect, url)) = rendered_links
            .upstream
            .zip(checkout.and_then(|checkout| checkout.upstream_web_url.as_ref()))
        {
            hits.space_links.push(SpaceLinkHit {
                rect: link_rect,
                target: SpaceLinkTarget::WebUrl(url.clone()),
            });
        }
        let projected_group = (!entry.indented && entry.repository_has_children)
            .then_some(entry.repository_index)
            .flatten()
            .and_then(|repository_index| structure?.repositories.get(repository_index))
            .map(|repository| repository.repository_id.clone());
        let group_toggle = projected_group
            .or_else(|| parent_group_key(snapshot, entry.index))
            .map(|key| {
                let rect = Rect::new(rect.right().saturating_sub(1), rect.y, 1, 1);
                put_text(
                    buffer,
                    rect.x,
                    rect.y,
                    rect.width,
                    if state.collapsed_groups.contains(&key) {
                        "▸"
                    } else {
                        "▾"
                    },
                    Style::default().fg(palette.accent),
                );
                (rect, key)
            });
        hits.workspaces.push(WorkspaceHit {
            rect,
            endpoint_id: ClientEndpointId::Local,
            workspace_id: navigation_target,
            move_workspace_id: workspace.workspace_id.clone(),
            indented: entry.indented,
            group_toggle,
        });
        let gap = entries
            .get(entry_position + 1)
            .map_or(0, |next| u16::from(!next.indented) * config.spaces.row_gap);
        y = y.saturating_add(row_height + gap);
    }

    if show_scrollbar {
        let track = Rect::new(body.right().saturating_sub(1), body.y, 1, body.height);
        hits.workspace_scrollbar = track;
        super::scroll::render_list_scrollbar(buffer, track, metrics, palette);
    }

    if let Some(row) = state.workspace_drop_indicator_row.filter(|row| {
        *row >= workspace_area.y.saturating_add(1)
            && *row < workspace_area.bottom().saturating_sub(1)
    }) {
        put_text(
            buffer,
            body.x,
            row,
            body.width,
            &"─".repeat(body.width as usize),
            Style::default().fg(palette.accent),
        );
    }

    let footer_y = workspace_area.bottom().saturating_sub(1);
    if config.mouse_capture {
        hits.new_workspace = Rect::new(
            workspace_area.x,
            footer_y,
            5.min(workspace_area.width),
            u16::from(workspace_area.height > 0),
        );
        put_text(
            buffer,
            workspace_area.x,
            footer_y,
            workspace_area.width,
            " new",
            Style::default().fg(palette.overlay0),
        );
        let attention = super::super::global_menu::global_menu_attention(snapshot);
        let launcher_width = if attention { 8 } else { 6 }.min(workspace_area.width);
        hits.global_launcher = Rect::new(
            workspace_area.right().saturating_sub(launcher_width),
            footer_y,
            launcher_width,
            1,
        );
        if attention {
            let start_x = workspace_area.right().saturating_sub(6);
            put_text(
                buffer,
                start_x,
                footer_y,
                2,
                "● ",
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD),
            );
            put_text(
                buffer,
                start_x.saturating_add(2),
                footer_y,
                4,
                "menu",
                Style::default().fg(palette.overlay0),
            );
        } else {
            put_right_text(
                buffer,
                workspace_area,
                footer_y,
                "menu",
                Style::default().fg(palette.overlay0),
            );
        }
    }

    super::render_agent_panel(
        buffer,
        detail_area,
        snapshot,
        config,
        state.agent_scroll,
        hits,
    );

    hits.sidebar_toggle = Rect::new(
        area.right().saturating_sub(2),
        area.bottom().saturating_sub(1),
        u16::from(area.width > 1),
        u16::from(area.height > 0),
    );
    put_text(
        buffer,
        hits.sidebar_toggle.x,
        hits.sidebar_toggle.y,
        hits.sidebar_toggle.width,
        "«",
        Style::default().fg(palette.overlay0),
    );
}

#[cfg(test)]
pub(crate) fn workspace_entries(
    snapshot: &ClientShellSnapshot,
    collapsed_groups: &HashSet<String>,
) -> Vec<WorkspaceEntry> {
    workspace_entries_with_structure(snapshot, None, None, collapsed_groups)
}

pub(crate) fn workspace_entries_with_structure(
    snapshot: &ClientShellSnapshot,
    structure: Option<&crate::protocol::endpoint::ClientShellStructureProjection>,
    host_documents: Option<&crate::protocol::endpoint::HostDocumentsProjectionV1>,
    collapsed_groups: &HashSet<String>,
) -> Vec<WorkspaceEntry> {
    let Some(structure) = structure.filter(|structure| {
        structure.boot_id == snapshot.boot_id
            && structure.revision == snapshot.revision
            && (!structure.repositories.is_empty() || !structure.space_order.is_empty())
    }) else {
        return compatibility_workspace_entries(snapshot, collapsed_groups);
    };
    if let Some(entries) =
        documented_workspace_entries(snapshot, structure, host_documents, collapsed_groups)
    {
        return entries;
    }
    let workspace_indexes = snapshot
        .workspaces
        .iter()
        .enumerate()
        .map(|(index, workspace)| (workspace.workspace_id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let repository_indexes = structure
        .repositories
        .iter()
        .enumerate()
        .map(|(index, repository)| (repository.repository_id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut emitted_repositories = HashSet::new();
    let mut emitted_workspaces = HashSet::new();
    let mut entries = Vec::new();

    for space in &structure.space_order {
        match space.kind {
            crate::protocol::endpoint::ClientShellSpaceKind::Repository => {
                if let Some(index) = repository_indexes.get(space.id.as_str()).copied() {
                    append_repository_entries(
                        snapshot,
                        structure,
                        index,
                        &workspace_indexes,
                        collapsed_groups,
                        &mut emitted_repositories,
                        &mut emitted_workspaces,
                        &mut entries,
                    );
                }
            }
            crate::protocol::endpoint::ClientShellSpaceKind::StandaloneWorkspace => {
                if let Some(index) = workspace_indexes.get(space.id.as_str()).copied() {
                    if emitted_workspaces.insert(index) {
                        entries.push(plain_workspace_entry(index));
                    }
                }
            }
            crate::protocol::endpoint::ClientShellSpaceKind::Unknown => {}
        }
    }
    for repository_index in 0..structure.repositories.len() {
        append_repository_entries(
            snapshot,
            structure,
            repository_index,
            &workspace_indexes,
            collapsed_groups,
            &mut emitted_repositories,
            &mut emitted_workspaces,
            &mut entries,
        );
    }
    for index in 0..snapshot.workspaces.len() {
        if emitted_workspaces.insert(index) {
            entries.push(plain_workspace_entry(index));
        }
    }
    entries
}

fn documented_workspace_entries(
    snapshot: &ClientShellSnapshot,
    structure: &crate::protocol::endpoint::ClientShellStructureProjection,
    host_documents: Option<&crate::protocol::endpoint::HostDocumentsProjectionV1>,
    collapsed_groups: &HashSet<String>,
) -> Option<Vec<WorkspaceEntry>> {
    use crate::extension_contract::{HostItemRole, HostSurface, HostTarget};

    let projection = host_documents.filter(|projection| {
        projection.boot_id == snapshot.boot_id && projection.revision == snapshot.revision
    })?;
    let (_, document_entry) = projection
        .documents
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            let document = &entry.document;
            document.namespace == "vqro.spaces"
                && document.surface == HostSurface::Navigation
                && document.target == HostTarget::Session
        })
        .max_by(|(left_index, left), (right_index, right)| {
            left.provider_priority
                .cmp(&right.provider_priority)
                // On equal priority, the earlier projection entry wins.
                .then_with(|| right_index.cmp(left_index))
        })?;
    let document = &document_entry.document;
    if document.validate().is_err() {
        return None;
    }

    let workspace_indexes = snapshot
        .workspaces
        .iter()
        .enumerate()
        .map(|(index, workspace)| (workspace.workspace_id.as_str(), index))
        .collect::<HashMap<_, _>>();
    if workspace_indexes.len() != snapshot.workspaces.len() {
        return None;
    }
    let repository_indexes = structure
        .repositories
        .iter()
        .enumerate()
        .map(|(index, repository)| (repository.repository_id.as_str(), index))
        .collect::<HashMap<_, _>>();
    if repository_indexes.len() != structure.repositories.len() {
        return None;
    }

    let mut workspace_membership = HashMap::new();
    for (repository_index, repository) in structure.repositories.iter().enumerate() {
        if repository.checkouts.is_empty() {
            return None;
        }
        for (checkout_index, checkout) in repository.checkouts.iter().enumerate() {
            if !workspace_indexes.contains_key(checkout.workspace_id.as_str())
                || workspace_membership
                    .insert(
                        checkout.workspace_id.as_str(),
                        (repository_index, checkout_index),
                    )
                    .is_some()
            {
                return None;
            }
        }
    }

    let mut repository_items = HashMap::new();
    let mut workspace_items = HashMap::new();
    for (item_index, item) in document.items.iter().enumerate() {
        if let Some(repository_id) = item.id.strip_prefix("repository:") {
            if repository_id.is_empty()
                || item.role != HostItemRole::Group
                || item.parent_id.is_some()
                || !repository_indexes.contains_key(repository_id)
                || repository_items.insert(repository_id, item_index).is_some()
            {
                return None;
            }
        } else if let Some(workspace_id) = item.id.strip_prefix("workspace:") {
            let &workspace_index = workspace_indexes.get(workspace_id)?;
            if workspace_id.is_empty()
                || item.role != HostItemRole::Section
                || workspace_items
                    .insert(workspace_id, (item_index, workspace_index))
                    .is_some()
            {
                return None;
            }
            match (
                workspace_membership.get(workspace_id),
                item.parent_id.as_deref(),
            ) {
                (Some((repository_index, _)), Some(parent))
                    if parent.strip_prefix("repository:")
                        == Some(
                            structure.repositories[*repository_index]
                                .repository_id
                                .as_str(),
                        ) => {}
                (None, None) => {}
                _ => return None,
            }
        } else {
            return None;
        }
    }
    if repository_items.len() != structure.repositories.len()
        || workspace_items.len() != snapshot.workspaces.len()
    {
        return None;
    }

    let mut entries = Vec::with_capacity(snapshot.workspaces.len());
    for item in document
        .items
        .iter()
        .filter(|item| item.parent_id.is_none())
    {
        if let Some(repository_id) = item.id.strip_prefix("repository:") {
            let repository_index = *repository_indexes.get(repository_id)?;
            let repository = structure.repositories.get(repository_index)?;
            let ordered_checkouts = document
                .items
                .iter()
                .filter(|candidate| candidate.parent_id.as_deref() == Some(item.id.as_str()))
                .map(|candidate| {
                    let workspace_id = candidate.id.strip_prefix("workspace:")?;
                    let (_, workspace_index) = *workspace_items.get(workspace_id)?;
                    let (_, checkout_index) = *workspace_membership.get(workspace_id)?;
                    Some((checkout_index, workspace_index, candidate.label.clone()))
                })
                .collect::<Option<Vec<_>>>()?;
            if ordered_checkouts.len() != repository.checkouts.len() {
                return None;
            }
            let (root_checkout_index, root_workspace_index) = ordered_checkouts
                .iter()
                .map(|(checkout, workspace, _)| (*checkout, *workspace))
                .find(|(checkout, _)| {
                    repository.checkouts[*checkout].kind
                        == crate::protocol::endpoint::ClientShellCheckoutKind::Primary
                })
                .or_else(|| {
                    ordered_checkouts
                        .first()
                        .map(|(checkout, workspace, _)| (*checkout, *workspace))
                })?;
            let has_children = ordered_checkouts.len() > 1;
            entries.push(WorkspaceEntry {
                index: root_workspace_index,
                indented: false,
                last_child: false,
                repository_index: Some(repository_index),
                checkout_index: Some(root_checkout_index),
                repository_has_children: has_children,
                label: Some(item.label.clone()),
            });
            let mut children = ordered_checkouts
                .into_iter()
                .filter(|(_, workspace_index, _)| *workspace_index != root_workspace_index)
                .collect::<Vec<_>>();
            if collapsed_groups.contains(&repository.repository_id) {
                children.retain(|(_, workspace_index, _)| {
                    snapshot.workspaces[*workspace_index].focused
                });
                children.truncate(1);
            }
            let child_count = children.len();
            for (position, (checkout_index, workspace_index, label)) in
                children.into_iter().enumerate()
            {
                entries.push(WorkspaceEntry {
                    index: workspace_index,
                    indented: true,
                    last_child: position + 1 == child_count,
                    repository_index: Some(repository_index),
                    checkout_index: Some(checkout_index),
                    repository_has_children: has_children,
                    label: Some(label),
                });
            }
        } else {
            let workspace_id = item.id.strip_prefix("workspace:")?;
            let (_, workspace_index) = *workspace_items.get(workspace_id)?;
            entries.push(WorkspaceEntry {
                index: workspace_index,
                indented: false,
                last_child: false,
                repository_index: None,
                checkout_index: None,
                repository_has_children: false,
                label: Some(item.label.clone()),
            });
        }
    }
    Some(entries)
}

fn append_repository_entries(
    snapshot: &ClientShellSnapshot,
    structure: &crate::protocol::endpoint::ClientShellStructureProjection,
    repository_index: usize,
    workspace_indexes: &HashMap<&str, usize>,
    collapsed_groups: &HashSet<String>,
    emitted_repositories: &mut HashSet<usize>,
    emitted_workspaces: &mut HashSet<usize>,
    entries: &mut Vec<WorkspaceEntry>,
) {
    if !emitted_repositories.insert(repository_index) {
        return;
    }
    let Some(repository) = structure.repositories.get(repository_index) else {
        return;
    };
    let checkouts = repository
        .checkouts
        .iter()
        .enumerate()
        .filter_map(|(checkout_index, checkout)| {
            workspace_indexes
                .get(checkout.workspace_id.as_str())
                .copied()
                .map(|workspace_index| (checkout_index, workspace_index))
        })
        .collect::<Vec<_>>();
    let Some((root_checkout_index, root_workspace_index)) = checkouts
        .iter()
        .copied()
        .find(|(checkout_index, _)| {
            repository.checkouts[*checkout_index].kind
                == crate::protocol::endpoint::ClientShellCheckoutKind::Primary
        })
        .or_else(|| checkouts.first().copied())
    else {
        return;
    };
    let has_children = checkouts.len() > 1;
    // Membership is authoritative even when a collapsed group hides most checkout rows.
    for (_, workspace_index) in &checkouts {
        emitted_workspaces.insert(*workspace_index);
    }
    entries.push(WorkspaceEntry {
        index: root_workspace_index,
        indented: false,
        last_child: false,
        repository_index: Some(repository_index),
        checkout_index: Some(root_checkout_index),
        repository_has_children: has_children,
        label: None,
    });
    if collapsed_groups.contains(&repository.repository_id) {
        if let Some((checkout_index, workspace_index)) =
            checkouts.iter().copied().find(|(_, workspace_index)| {
                *workspace_index != root_workspace_index
                    && snapshot.workspaces[*workspace_index].focused
            })
        {
            emitted_workspaces.insert(workspace_index);
            entries.push(WorkspaceEntry {
                index: workspace_index,
                indented: true,
                last_child: true,
                repository_index: Some(repository_index),
                checkout_index: Some(checkout_index),
                repository_has_children: has_children,
                label: None,
            });
        }
        return;
    }
    let children = checkouts
        .into_iter()
        .filter(|(_, workspace_index)| *workspace_index != root_workspace_index)
        .collect::<Vec<_>>();
    for (position, (checkout_index, workspace_index)) in children.iter().copied().enumerate() {
        emitted_workspaces.insert(workspace_index);
        entries.push(WorkspaceEntry {
            index: workspace_index,
            indented: true,
            last_child: position + 1 == children.len(),
            repository_index: Some(repository_index),
            checkout_index: Some(checkout_index),
            repository_has_children: has_children,
            label: None,
        });
    }
}

fn plain_workspace_entry(index: usize) -> WorkspaceEntry {
    WorkspaceEntry {
        index,
        indented: false,
        last_child: false,
        repository_index: None,
        checkout_index: None,
        repository_has_children: false,
        label: None,
    }
}

fn compatibility_workspace_entries(
    snapshot: &ClientShellSnapshot,
    collapsed_groups: &HashSet<String>,
) -> Vec<WorkspaceEntry> {
    let mut members = HashMap::<&str, Vec<usize>>::new();
    for (index, workspace) in snapshot.workspaces.iter().enumerate() {
        if let Some(worktree) = &workspace.worktree {
            members.entry(&worktree.key).or_default().push(index);
        }
    }
    let grouped = members
        .iter()
        .filter(|(_, indices)| indices.len() >= 2)
        .map(|(key, _)| *key)
        .collect::<HashSet<_>>();
    let mut emitted = HashSet::<&str>::new();
    let mut entries = Vec::new();
    for (index, workspace) in snapshot.workspaces.iter().enumerate() {
        let Some(worktree) = workspace
            .worktree
            .as_ref()
            .filter(|worktree| grouped.contains(worktree.key.as_str()))
        else {
            entries.push(plain_workspace_entry(index));
            continue;
        };
        if !emitted.insert(&worktree.key) {
            continue;
        }
        let Some(group_members) = members.get(worktree.key.as_str()) else {
            continue;
        };
        let parent = group_members
            .iter()
            .copied()
            .find(|member| {
                snapshot.workspaces[*member]
                    .worktree
                    .as_ref()
                    .is_some_and(|worktree| !worktree.is_linked_worktree)
            })
            .unwrap_or(index);
        entries.push(WorkspaceEntry {
            index: parent,
            indented: false,
            last_child: false,
            repository_index: None,
            checkout_index: None,
            repository_has_children: true,
            label: None,
        });
        if collapsed_groups.contains(&worktree.key) {
            if let Some(active) = group_members
                .iter()
                .copied()
                .find(|member| *member != parent && snapshot.workspaces[*member].focused)
            {
                entries.push(WorkspaceEntry {
                    index: active,
                    indented: true,
                    last_child: true,
                    repository_index: None,
                    checkout_index: None,
                    repository_has_children: true,
                    label: None,
                });
            }
            continue;
        }
        let children = group_members
            .iter()
            .copied()
            .filter(|member| *member != parent)
            .collect::<Vec<_>>();
        for (child_index, child) in children.iter().enumerate() {
            entries.push(WorkspaceEntry {
                index: *child,
                indented: true,
                last_child: child_index + 1 == children.len(),
                repository_index: None,
                checkout_index: None,
                repository_has_children: true,
                label: None,
            });
        }
    }
    entries
}

pub(in crate::client::shell) fn parent_group_key(
    snapshot: &ClientShellSnapshot,
    index: usize,
) -> Option<String> {
    let workspace = snapshot.workspaces.get(index)?;
    let worktree = workspace.worktree.as_ref()?;
    let mut count = 0;
    let mut first = None;
    let mut primary = None;
    for (candidate_index, candidate) in snapshot.workspaces.iter().enumerate() {
        let Some(candidate) = candidate
            .worktree
            .as_ref()
            .filter(|candidate| candidate.key == worktree.key)
        else {
            continue;
        };
        count += 1;
        first.get_or_insert(candidate_index);
        if !candidate.is_linked_worktree {
            primary.get_or_insert(candidate_index);
        }
    }
    (count >= 2 && primary.or(first) == Some(index)).then(|| worktree.key.clone())
}

pub(in crate::client::shell) fn repository_group_key(
    endpoint_id: &ClientEndpointId,
    repository_id: &str,
) -> String {
    if endpoint_id.is_local() {
        repository_id.to_owned()
    } else {
        format!("{}\0{repository_id}", endpoint_id.storage_key())
    }
}

fn projected_repository_is_collapsed(
    entry: &WorkspaceEntry,
    structure: Option<&crate::protocol::endpoint::ClientShellStructureProjection>,
    collapsed_groups: &HashSet<String>,
) -> bool {
    entry
        .repository_index
        .and_then(|index| structure?.repositories.get(index))
        .is_some_and(|repository| collapsed_groups.contains(&repository.repository_id))
}

pub(in crate::client::shell) fn projected_workspace_target(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
    entry: &WorkspaceEntry,
    structure: Option<&crate::protocol::endpoint::ClientShellStructureProjection>,
    repository_collapsed: bool,
) -> String {
    if !repository_collapsed {
        return workspace.workspace_id.clone();
    }
    let Some(repository) = entry
        .repository_index
        .and_then(|index| structure?.repositories.get(index))
    else {
        return workspace.workspace_id.clone();
    };
    repository
        .last_focused_workspace_id
        .as_deref()
        .filter(|workspace_id| {
            snapshot
                .workspaces
                .iter()
                .any(|candidate| candidate.workspace_id == *workspace_id)
        })
        .or_else(|| {
            repository.checkouts.iter().find_map(|checkout| {
                snapshot
                    .workspaces
                    .iter()
                    .find(|candidate| {
                        candidate.workspace_id == checkout.workspace_id && candidate.focused
                    })
                    .map(|candidate| candidate.workspace_id.as_str())
            })
        })
        .unwrap_or(&workspace.workspace_id)
        .to_owned()
}

pub(in crate::client::shell) fn projected_workspace_status(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
    entry: &WorkspaceEntry,
    structure: Option<&crate::protocol::endpoint::ClientShellStructureProjection>,
    collapsed_groups: &HashSet<String>,
    repository_collapsed: bool,
    workspace_index: Option<&HashMap<&str, &ClientShellWorkspace>>,
) -> crate::api::schema::AgentStatus {
    if let Some(repository) = entry
        .repository_index
        .and_then(|index| structure?.repositories.get(index))
        .filter(|_| repository_collapsed)
    {
        return repository
            .checkouts
            .iter()
            .filter_map(|checkout| {
                workspace_index
                    .and_then(|index| index.get(checkout.workspace_id.as_str()).copied())
                    .or_else(|| {
                        snapshot
                            .workspaces
                            .iter()
                            .find(|candidate| candidate.workspace_id == checkout.workspace_id)
                    })
            })
            .map(|candidate| candidate.agent_status)
            .max_by_key(|status| status_priority(*status))
            .unwrap_or(workspace.agent_status);
    }
    displayed_workspace_status(snapshot, workspace, collapsed_groups)
}

pub(in crate::client::shell) fn displayed_workspace_status(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
    collapsed_groups: &HashSet<String>,
) -> crate::api::schema::AgentStatus {
    let Some(worktree) = workspace
        .worktree
        .as_ref()
        .filter(|worktree| !worktree.is_linked_worktree)
    else {
        return workspace.agent_status;
    };
    if !collapsed_groups.contains(&worktree.key) {
        return workspace.agent_status;
    }
    snapshot
        .workspaces
        .iter()
        .filter(|candidate| {
            candidate
                .worktree
                .as_ref()
                .is_some_and(|candidate| candidate.key == worktree.key)
        })
        .map(|candidate| candidate.agent_status)
        .max_by_key(|status| status_priority(*status))
        .unwrap_or(workspace.agent_status)
}

pub(in crate::client::shell) fn projected_workspace_rows(
    workspace: &ClientShellWorkspace,
    status: crate::api::schema::AgentStatus,
    entry: &WorkspaceEntry,
    structure: Option<&crate::protocol::endpoint::ClientShellStructureProjection>,
    config: &SpacesSidebarConfig,
) -> Vec<Vec<crate::ui::ResolvedToken>> {
    let repository = entry
        .repository_index
        .and_then(|index| structure?.repositories.get(index));
    let checkout = entry
        .checkout_index
        .and_then(|index| repository?.checkouts.get(index));
    let label = if let Some(label) = entry.label.as_ref() {
        label.clone()
    } else if entry.indented && !workspace.custom_label {
        workspace.branch.as_deref().map_or_else(
            || {
                checkout
                    .and_then(|checkout| checkout.detached_head.as_deref())
                    .map_or_else(
                        || workspace.label.clone(),
                        |head| {
                            format!(
                                "detached@{}",
                                head.strip_prefix("detached@").unwrap_or(head)
                            )
                        },
                    )
            },
            |branch| {
                branch
                    .strip_prefix("worktree/")
                    .unwrap_or(branch)
                    .to_owned()
            },
        )
    } else if !entry.indented {
        repository.map_or_else(
            || workspace.label.clone(),
            |repository| repository.label.clone(),
        )
    } else {
        workspace.label.clone()
    };
    let branch_with_upstream = (!entry.indented)
        .then(|| {
            if let Some(branch) = workspace.branch.as_deref() {
                return Some(
                    match checkout.and_then(|checkout| checkout.upstream.as_deref()) {
                        Some(upstream) if upstream != branch => format!("{branch} → {upstream}"),
                        _ => branch.to_owned(),
                    },
                );
            }
            checkout.and_then(|checkout| {
                checkout.detached_head.as_deref().map(|head| {
                    format!(
                        "detached@{}",
                        head.strip_prefix("detached@").unwrap_or(head)
                    )
                })
            })
        })
        .flatten();
    let token_values = workspace.tokens.iter().cloned().collect::<HashMap<_, _>>();
    crate::ui::sidebar_space_rows(
        config,
        crate::ui::SpaceTokenContext {
            workspace: &label,
            branch: branch_with_upstream
                .as_deref()
                .or(workspace.branch.as_deref()),
            state_text: status_text(status),
            ahead_behind: workspace.git_ahead_behind,
            tokens: &token_values,
            suppress_branch: entry.indented,
        },
    )
}

pub(in crate::client::shell) struct WorkspaceLinkLabels<'a> {
    pub(in crate::client::shell) repository: Option<&'a str>,
    pub(in crate::client::shell) branch: Option<&'a str>,
    pub(in crate::client::shell) upstream: Option<&'a str>,
}

#[derive(Default)]
pub(in crate::client::shell) struct WorkspaceRenderedLinks {
    pub(in crate::client::shell) repository: Option<Rect>,
    pub(in crate::client::shell) branch: Option<Rect>,
    pub(in crate::client::shell) upstream: Option<Rect>,
}

pub(in crate::client::shell) fn render_workspace_rows(
    buffer: &mut Buffer,
    area: Rect,
    status: crate::api::schema::AgentStatus,
    indicators: crate::config::StatusIndicatorStyle,
    entry: &WorkspaceEntry,
    rows: Vec<Vec<crate::ui::ResolvedToken>>,
    focused: bool,
    selected: bool,
    dragged: bool,
    palette: &Palette,
    link_labels: WorkspaceLinkLabels<'_>,
) -> WorkspaceRenderedLinks {
    let mut links = WorkspaceRenderedLinks::default();
    for (row_index, row) in rows.iter().enumerate() {
        let y = area.y + row_index as u16;
        if y >= area.bottom() {
            break;
        }
        let mut x = area.x;
        if entry.indented {
            if row_index == 0 {
                put_text(
                    buffer,
                    area.x.saturating_add(1),
                    y,
                    area.width.saturating_sub(1).min(1),
                    status_icon(status, indicators),
                    Style::default().fg(status_color(status, palette)),
                );
                x = put_segment(
                    buffer,
                    area.x.saturating_add(3),
                    y,
                    area.right(),
                    if entry.last_child {
                        "└─ "
                    } else {
                        "├─ "
                    },
                    Style::default().fg(palette.overlay0),
                );
            } else {
                x = put_segment(
                    buffer,
                    x,
                    y,
                    area.right(),
                    if entry.last_child {
                        "        "
                    } else {
                        "   │    "
                    },
                    Style::default().fg(palette.overlay0),
                );
            }
        } else if row_index == 0 {
            x = x.saturating_add(1);
        } else {
            x = x.saturating_add(3);
        }
        let highlighted = focused || dragged;
        let workspace_style = Style::default()
            .fg(if highlighted {
                palette.text
            } else {
                palette.subtext0
            })
            .add_modifier(if highlighted {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        let secondary_style = Style::default().fg(if focused {
            palette.mauve
        } else {
            palette.overlay0
        });
        let state_icon = if entry.indented {
            (
                "",
                Style::default().fg(if focused || selected {
                    palette.mauve
                } else {
                    palette.overlay0
                }),
            )
        } else {
            (
                status_icon(status, indicators),
                Style::default().fg(status_color(status, palette)),
            )
        };
        let spans = crate::ui::resolved_token_spans(
            row,
            state_icon,
            Style::default().fg(status_color(status, palette)),
            workspace_style,
            secondary_style,
            Style::default().fg(palette.overlay1),
            palette,
            area.right().saturating_sub(2).saturating_sub(x) as usize,
        );
        let link_rect = |needle: &str| {
            let mut span_offset = 0;
            for span in &spans {
                let text = span.content.as_ref();
                if let Some(byte_offset) = text.find(needle) {
                    let column_offset =
                        span_offset + super::render::display_width(&text[..byte_offset]);
                    let width = super::render::display_width(needle);
                    return (width > 0)
                        .then(|| Rect::new(x.saturating_add(column_offset), y, width, 1));
                }
                span_offset += super::render::display_width(text);
            }
            None
        };
        if links.repository.is_none() {
            links.repository = link_labels.repository.and_then(link_rect);
        }
        if links.branch.is_none() {
            links.branch = link_labels.branch.and_then(link_rect);
        }
        if links.upstream.is_none() {
            links.upstream = link_labels.upstream.and_then(|upstream| {
                link_rect(upstream).or_else(|| {
                    let marker = " → ";
                    let mut span_offset = 0;
                    for span in &spans {
                        let text = span.content.as_ref();
                        if let Some(byte_offset) = text.rfind(marker) {
                            let byte_offset = byte_offset + marker.len();
                            let column_offset =
                                span_offset + super::render::display_width(&text[..byte_offset]);
                            let width = super::render::display_width(&text[byte_offset..]);
                            return (width > 0)
                                .then(|| Rect::new(x.saturating_add(column_offset), y, width, 1));
                        }
                        span_offset += super::render::display_width(text);
                    }
                    None
                })
            });
        }
        Paragraph::new(Line::from(spans)).render(
            Rect::new(x, y, area.right().saturating_sub(2).saturating_sub(x), 1),
            buffer,
        );
    }

    let background = if selected {
        Some(palette.selection_bg)
    } else if dragged {
        Some(palette.surface1)
    } else if focused {
        Some(palette.active_row_bg)
    } else {
        None
    };
    if let Some(background) = background {
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                buffer[(x, y)].set_bg(background);
            }
        }
    }
    links
}
