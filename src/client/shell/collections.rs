use super::*;

fn member_name(
    pane_id: &str,
    delegation: Option<&crate::protocol::endpoint::ClientShellDelegation>,
    agent: Option<&crate::protocol::ClientShellAgent>,
    pane: Option<&crate::protocol::ClientShellPane>,
) -> String {
    delegation
        .and_then(|entry| entry.purpose.as_deref())
        .or_else(|| agent.and_then(|agent| agent.name.as_deref()))
        .or_else(|| agent.and_then(|agent| agent.title.as_deref()))
        .or_else(|| agent.and_then(|agent| agent.terminal_title_stripped.as_deref()))
        .or_else(|| agent.and_then(|agent| agent.display_agent.as_deref()))
        .or_else(|| pane.and_then(|pane| pane.label.as_deref()))
        .unwrap_or(pane_id)
        .to_owned()
}

fn status_style(status: crate::api::schema::AgentStatus, palette: &Palette) -> Style {
    let color = match status {
        crate::api::schema::AgentStatus::Working => palette.green,
        crate::api::schema::AgentStatus::Blocked => palette.red,
        crate::api::schema::AgentStatus::Done => palette.teal,
        crate::api::schema::AgentStatus::Idle => palette.yellow,
        crate::api::schema::AgentStatus::Unknown => palette.overlay0,
    };
    Style::default().fg(color).bg(palette.panel_bg)
}

fn put(buffer: &mut Buffer, rect: Rect, text: &str, style: Style) {
    if rect.is_empty() {
        return;
    }
    let text = crate::ui::truncate_end(text, rect.width as usize);
    buffer.set_style(rect, style);
    for (offset, symbol) in text.chars().enumerate() {
        let Ok(offset) = u16::try_from(offset) else {
            break;
        };
        if offset >= rect.width {
            break;
        }
        buffer[(rect.x + offset, rect.y)]
            .set_symbol(&symbol.to_string())
            .set_style(style);
    }
}

pub(super) fn render_collection_chrome(
    buffer: &mut Buffer,
    snapshot: &crate::protocol::ClientShellSnapshot,
    structure: Option<&crate::protocol::endpoint::ClientShellStructureProjection>,
    panes: &[PaneHit],
    list_views: &HashSet<String>,
    palette: &Palette,
) -> Vec<CollectionChromeHit> {
    let Some(structure) = structure.filter(|structure| {
        structure.boot_id == snapshot.boot_id && structure.revision == snapshot.revision
    }) else {
        return Vec::new();
    };
    let agents = snapshot
        .agents
        .iter()
        .map(|agent| (agent.pane_id.as_str(), agent))
        .collect::<std::collections::HashMap<_, _>>();
    let projected_panes = snapshot
        .panes
        .iter()
        .map(|pane| (pane.pane_id.as_str(), pane))
        .collect::<std::collections::HashMap<_, _>>();
    let delegations = structure
        .delegations
        .iter()
        .filter_map(|entry| entry.pane_id.as_deref().map(|pane_id| (pane_id, entry)))
        .collect::<std::collections::HashMap<_, _>>();
    let delegations_by_id = structure
        .delegations
        .iter()
        .map(|entry| (entry.delegation_id.as_str(), entry))
        .collect::<std::collections::HashMap<_, _>>();
    let collections = structure
        .collections
        .iter()
        .filter_map(|collection| {
            collection
                .selected_pane_id
                .as_deref()
                .map(|pane_id| (pane_id, collection))
        })
        .collect::<std::collections::HashMap<_, _>>();
    let mut hits = Vec::new();

    for pane in panes {
        let Some(collection) = collections.get(pane.pane_id.as_str()).copied() else {
            continue;
        };
        let mut members = collection.members.iter().collect::<Vec<_>>();
        members.sort_by_key(|member| member.index);
        let Some(selected_index) = members
            .iter()
            .position(|member| member.pane_id == pane.pane_id)
        else {
            continue;
        };
        let selected = members[selected_index];
        let delegation = delegations.get(selected.pane_id.as_str()).copied();
        let header_y = pane.inner_rect.y.saturating_sub(1);
        if header_y < pane.rect.y || pane.inner_rect.width < 12 {
            continue;
        }
        let rect = Rect::new(pane.inner_rect.x, header_y, pane.inner_rect.width, 1);
        let base = Style::default().fg(palette.text).bg(palette.panel_bg);
        buffer.set_style(rect, base);
        for x in rect.x..rect.right() {
            buffer[(x, rect.y)]
                .set_symbol(" ")
                .set_style(base)
                .set_skip(false);
        }

        let previous = Rect::new(rect.x, rect.y, 3.min(rect.width), 1);
        let next = Rect::new(rect.right().saturating_sub(3), rect.y, 3.min(rect.width), 1);
        let inspect_width = if rect.width >= 72 { 10 } else { 3 };
        let presentation_width = if rect.width >= 60 { 9 } else { 3 };
        let inspect = Rect::new(
            next.x.saturating_sub(inspect_width),
            rect.y,
            inspect_width,
            1,
        );
        let presentation_toggle = Rect::new(
            inspect.x.saturating_sub(presentation_width),
            rect.y,
            presentation_width,
            1,
        );
        put(
            buffer,
            previous,
            " ‹ ",
            Style::default().fg(palette.accent).bg(palette.surface0),
        );
        put(
            buffer,
            next,
            " › ",
            Style::default().fg(palette.accent).bg(palette.surface0),
        );
        put(
            buffer,
            presentation_toggle,
            if presentation_width >= 9 {
                if list_views.contains(&collection.collection_id) {
                    " terminal"
                } else {
                    " list    "
                }
            } else if list_views.contains(&collection.collection_id) {
                " t "
            } else {
                " ≡ "
            },
            Style::default().fg(palette.accent).bg(palette.surface0),
        );
        put(
            buffer,
            inspect,
            if inspect_width >= 10 {
                " inspect  "
            } else {
                " i "
            },
            Style::default().fg(palette.teal).bg(palette.surface0),
        );

        let content_x = previous.right().saturating_add(1);
        let content_width = presentation_toggle
            .x
            .saturating_sub(content_x)
            .saturating_sub(1);
        let parent_outside_collection = delegation
            .and_then(|entry| entry.parent_id.as_deref())
            .and_then(|parent_id| delegations_by_id.get(parent_id))
            .and_then(|parent| parent.pane_id.as_deref())
            .is_some_and(|parent_pane_id| {
                !members
                    .iter()
                    .any(|member| member.pane_id == parent_pane_id)
            });
        let structure_icon = match delegation {
            Some(entry) if entry.external_parent_id.is_some() || parent_outside_collection => "↗",
            Some(entry) if entry.depth > 0 => "↳",
            Some(_) => "◆",
            None => "·",
        };
        let archive = if selected.archived { "◇ " } else { "" };
        let status = agents
            .get(selected.pane_id.as_str())
            .map_or(crate::api::schema::AgentStatus::Unknown, |agent| {
                agent.agent_status
            });
        let text = format!(
            "{} {}/{} {} {}{}",
            collection.label.as_deref().unwrap_or("Collection"),
            selected_index + 1,
            members.len(),
            structure_icon,
            archive,
            member_name(
                &selected.pane_id,
                delegation,
                agents.get(selected.pane_id.as_str()).copied(),
                projected_panes.get(selected.pane_id.as_str()).copied(),
            ),
        );
        put(
            buffer,
            Rect::new(content_x, rect.y, content_width, 1),
            &text,
            status_style(status, palette),
        );

        let previous_pane_id = (members.len() > 1).then(|| {
            members[(selected_index + members.len() - 1) % members.len()]
                .pane_id
                .clone()
        });
        let next_pane_id = (members.len() > 1).then(|| {
            members[(selected_index + 1) % members.len()]
                .pane_id
                .clone()
        });
        hits.push(CollectionChromeHit {
            rect,
            collection_id: collection.collection_id.clone(),
            selected_pane_id: pane.pane_id.clone(),
            previous_pane_id,
            next_pane_id,
            previous,
            next,
            presentation_toggle,
            inspect,
        });
    }
    hits
}

pub(super) fn render_collection_lists(
    buffer: &mut Buffer,
    snapshot: &crate::protocol::ClientShellSnapshot,
    structure: Option<&crate::protocol::endpoint::ClientShellStructureProjection>,
    panes: &[PaneHit],
    list_views: &HashSet<String>,
    expanded_members: &HashMap<String, HashSet<String>>,
    preview_heights: &HashMap<String, u16>,
    maximized_pane: Option<&(String, String)>,
    terminal_surfaces: &HashMap<String, crate::protocol::endpoint::ServerTerminalViewSurface>,
    palette: &Palette,
) -> Vec<CollectionMemberHit> {
    let Some(structure) = structure.filter(|structure| {
        structure.boot_id == snapshot.boot_id && structure.revision == snapshot.revision
    }) else {
        return Vec::new();
    };
    let projected_panes = snapshot
        .panes
        .iter()
        .map(|pane| (pane.pane_id.as_str(), pane))
        .collect::<HashMap<_, _>>();
    let agents = snapshot
        .agents
        .iter()
        .map(|agent| (agent.pane_id.as_str(), agent))
        .collect::<HashMap<_, _>>();
    let delegations = structure
        .delegations
        .iter()
        .filter_map(|delegation| {
            delegation
                .pane_id
                .as_deref()
                .map(|pane_id| (pane_id, delegation))
        })
        .collect::<HashMap<_, _>>();
    let delegations_by_id = structure
        .delegations
        .iter()
        .map(|delegation| (delegation.delegation_id.as_str(), delegation))
        .collect::<HashMap<_, _>>();
    let mut hits = Vec::new();

    for collection in structure
        .collections
        .iter()
        .filter(|collection| list_views.contains(&collection.collection_id))
    {
        let Some(selected_pane_id) = collection.selected_pane_id.as_deref() else {
            continue;
        };
        let Some(area) = panes
            .iter()
            .find(|pane| pane.pane_id == selected_pane_id)
            .map(|pane| pane.inner_rect.intersection(buffer.area))
            .filter(|area| !area.is_empty())
        else {
            continue;
        };
        // Collection lists use the host terminal's default background. Clear replaced PTY cells
        // without introducing a list panel or card behind the hierarchy.
        let base = Style::default().fg(palette.text);
        buffer.set_style(area, base);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                buffer[(x, y)]
                    .set_symbol(" ")
                    .set_style(base)
                    .set_skip(false);
            }
        }

        if let Some((_, pane_id)) =
            maximized_pane.filter(|(collection_id, _)| collection_id == &collection.collection_id)
        {
            if let Some(surface) = terminal_surfaces.get(pane_id) {
                if let Some(source) = surface.frame.to_ratatui_buffer() {
                    let height = area.height.min(source.area.height);
                    let width = area.width.min(source.area.width);
                    for preview_y in 0..height {
                        for preview_x in 0..width {
                            buffer[(area.x + preview_x, area.y + preview_y)] = source
                                [(source.area.x + preview_x, source.area.y + preview_y)]
                                .clone();
                            buffer[(area.x + preview_x, area.y + preview_y)].set_skip(false);
                        }
                    }
                }
            } else {
                put(buffer, area, "loading terminal…", base);
            }
            hits.push(CollectionMemberHit {
                rect: Rect::default(),
                disclosure: Rect::default(),
                preview: area,
                collection_id: collection.collection_id.clone(),
                pane_id: pane_id.clone(),
            });
            continue;
        }

        let mut members = collection.members.iter().collect::<Vec<_>>();
        members.sort_by_key(|member| member.index);
        let active_count = members.iter().filter(|member| !member.archived).count();
        let archived_count = members.len().saturating_sub(active_count);
        let archived_by_pane = members
            .iter()
            .map(|member| (member.pane_id.as_str(), member.archived))
            .collect::<HashMap<_, _>>();
        let mut y = area.y;
        put(
            buffer,
            Rect::new(area.x + 1, y, area.width.saturating_sub(2), 1),
            &format!("active {active_count}"),
            Style::default().fg(palette.overlay0),
        );
        y = y.saturating_add(1);
        let mut rendered_members = 0usize;

        for archived in [false, true] {
            if archived && archived_count > 0 && y < area.bottom() {
                put(
                    buffer,
                    Rect::new(area.x + 1, y, area.width.saturating_sub(2), 1),
                    &format!("archive {archived_count}"),
                    Style::default().fg(palette.overlay0),
                );
                y = y.saturating_add(1);
            }
            for member in members
                .iter()
                .copied()
                .filter(|member| member.archived == archived)
            {
                if y >= area.bottom().saturating_sub(1) {
                    break;
                }
                let delegation = delegations.get(member.pane_id.as_str()).copied();
                let local_parent = delegation
                    .and_then(|delegation| delegation.parent_id.as_deref())
                    .and_then(|parent_id| delegations_by_id.get(parent_id).copied())
                    .and_then(|parent| parent.pane_id.as_deref())
                    .is_some_and(|parent_pane_id| {
                        archived_by_pane.get(parent_pane_id) == Some(&member.archived)
                    });
                let external_parent = delegation.is_some_and(|delegation| {
                    delegation.external_parent_id.is_some()
                        || (delegation.parent_id.is_some() && !local_parent)
                });
                let depth = if local_parent {
                    delegation.map_or(0, |delegation| delegation.depth.min(8))
                } else {
                    0
                };
                let indent = "  ".repeat(depth);
                let selected = member.pane_id == selected_pane_id;
                let marker = if selected { "›" } else { " " };
                let state = agents
                    .get(member.pane_id.as_str())
                    .map_or(crate::api::schema::AgentStatus::Unknown, |agent| {
                        agent.agent_status
                    });
                let state_marker = match state {
                    crate::api::schema::AgentStatus::Working => "●",
                    crate::api::schema::AgentStatus::Blocked => "!",
                    crate::api::schema::AgentStatus::Done => "✓",
                    crate::api::schema::AgentStatus::Idle => "○",
                    crate::api::schema::AgentStatus::Unknown => "·",
                };
                let name = member_name(
                    &member.pane_id,
                    delegation,
                    agents.get(member.pane_id.as_str()).copied(),
                    projected_panes.get(member.pane_id.as_str()).copied(),
                );
                let expanded = expanded_members
                    .get(&collection.collection_id)
                    .is_some_and(|expanded| expanded.contains(&member.pane_id));
                let disclosure_symbol = if expanded { "▾" } else { "▸" };
                let provenance = if external_parent { "↗ " } else { "" };
                let text = format!(
                    "{marker} {disclosure_symbol} {indent}{provenance}{state_marker} {name}"
                );
                let row = Rect::new(area.x, y, area.width, 1);
                put(
                    buffer,
                    row,
                    &text,
                    if selected {
                        Style::default().fg(palette.text)
                    } else {
                        Style::default().fg(match state {
                            crate::api::schema::AgentStatus::Working => palette.green,
                            crate::api::schema::AgentStatus::Blocked => palette.yellow,
                            crate::api::schema::AgentStatus::Done => palette.teal,
                            crate::api::schema::AgentStatus::Idle => palette.overlay0,
                            crate::api::schema::AgentStatus::Unknown => palette.subtext0,
                        })
                    },
                );
                let disclosure = Rect::new(area.x.saturating_add(2), y, 2.min(area.width), 1);
                y = y.saturating_add(1);
                rendered_members = rendered_members.saturating_add(1);
                let remaining_rows = members
                    .len()
                    .saturating_sub(rendered_members)
                    .saturating_add(usize::from(!archived && archived_count > 0))
                    .saturating_add(1); // persistent help footer
                let remaining_rows = u16::try_from(remaining_rows).unwrap_or(u16::MAX);
                let mut preview = Rect::default();
                if expanded && y < area.bottom().saturating_sub(remaining_rows) {
                    // A collection has no list viewport. Reserve space for every remaining row
                    // (and the archive heading) before assigning height to this terminal.
                    let available = area
                        .bottom()
                        .saturating_sub(y)
                        .saturating_sub(remaining_rows);
                    let preview_height = preview_heights
                        .get(&member.pane_id)
                        .copied()
                        .unwrap_or_else(|| (area.height / 2).clamp(4, 12))
                        .clamp(2, area.height.saturating_sub(2).max(2))
                        .min(available);
                    let preview_width = area.width.saturating_sub(2);
                    if preview_height >= 2 && preview_width >= 4 {
                        preview = Rect::new(area.x + 1, y, preview_width, preview_height);
                        buffer.set_style(preview, Style::default().fg(palette.text));
                        if let Some(surface) = terminal_surfaces.get(&member.pane_id) {
                            if let Some(source) = surface.frame.to_ratatui_buffer() {
                                let height = preview.height.min(source.area.height);
                                let width = preview.width.min(source.area.width);
                                for preview_y in 0..height {
                                    for preview_x in 0..width {
                                        buffer[(preview.x + preview_x, preview.y + preview_y)] =
                                            source[(
                                                source.area.x + preview_x,
                                                source.area.y + preview_y,
                                            )]
                                                .clone();
                                        buffer[(preview.x + preview_x, preview.y + preview_y)]
                                            .set_skip(false);
                                    }
                                }
                            }
                        } else {
                            put(
                                buffer,
                                Rect::new(
                                    preview.x + 1,
                                    preview.y,
                                    preview.width.saturating_sub(2),
                                    1,
                                ),
                                "loading terminal…",
                                Style::default().fg(palette.overlay0),
                            );
                        }
                        y = y.saturating_add(preview_height);
                    }
                }
                hits.push(CollectionMemberHit {
                    rect: row,
                    disclosure,
                    preview,
                    collection_id: collection.collection_id.clone(),
                    pane_id: member.pane_id.clone(),
                });
            }
        }
        if y < area.bottom() {
            put(
                buffer,
                Rect::new(
                    area.x + 1,
                    area.bottom() - 1,
                    area.width.saturating_sub(2),
                    1,
                ),
                "enter terminal · space expand · z maximize · a archive · p promote",
                Style::default().fg(palette.overlay0),
            );
        }
    }
    hits
}
