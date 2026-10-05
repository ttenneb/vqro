use ratatui::style::Color;

use super::*;

fn terminal_scroll_metrics(
    surface: &crate::protocol::endpoint::ServerTerminalViewSurface,
) -> Option<crate::pane::ScrollMetrics> {
    let scroll = surface.scroll?;
    Some(crate::pane::ScrollMetrics {
        offset_from_bottom: usize::try_from(scroll.offset_from_bottom).ok()?,
        max_offset_from_bottom: usize::try_from(scroll.max_offset_from_bottom).ok()?,
        viewport_rows: usize::try_from(scroll.viewport_rows).ok()?,
    })
}

fn render_preview_scrollbar(
    buffer: &mut Buffer,
    metrics: crate::pane::ScrollMetrics,
    track: Rect,
    palette: &Palette,
    focused: bool,
) {
    crate::ui::render_pane_scrollbar_buffer(buffer, metrics, track, palette, focused);
    // Pane scrollbars use right-edge block glyphs. Preview resize borders use box drawing, so
    // normalize this scrollbar to matching vertical glyphs that visibly join the lower corner.
    for y in track.y..track.bottom() {
        let cell = &mut buffer[(track.x, y)];
        match cell.symbol() {
            "▐" => {
                cell.set_symbol("┃");
            }
            "▕" => {
                cell.set_symbol("│");
            }
            _ => {}
        }
    }
}

fn member_name(
    pane_id: &str,
    purpose: Option<&str>,
    agent: Option<&crate::protocol::ClientShellAgent>,
    pane: Option<&crate::protocol::ClientShellPane>,
) -> String {
    purpose
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
    Style::default().fg(color).bg(Color::Reset)
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

#[derive(Clone, Debug)]
pub(super) struct CollectionSurface {
    pub(super) collection_id: String,
    pub(super) rect: Rect,
    pub(super) inner_rect: Rect,
    /// Focused collection leaf, so its frame can use the focused border color.
    pub(super) focused: bool,
}

/// Header control rects for a collection row: previous/next member, the
/// list/terminal presentation toggle, and inspect.
fn collection_control_rects(rect: Rect) -> (Rect, Rect, Rect, Rect) {
    let previous = Rect::new(rect.x, rect.y, 3.min(rect.width), 1);
    let next = Rect::new(rect.right().saturating_sub(3), rect.y, 3.min(rect.width), 1);
    let inspect_width = if rect.width >= 72 { 10 } else { 3 };
    let presentation_width = if rect.width >= 76 {
        14
    } else if rect.width >= 44 {
        10
    } else {
        3
    };
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
    (previous, next, presentation_toggle, inspect)
}

fn presentation_toggle_text(presentation_width: u16, shows_terminal: bool) -> &'static str {
    if presentation_width >= 14 {
        if shows_terminal {
            "[ t terminal ]"
        } else {
            "[ ≡ list ]    "
        }
    } else if presentation_width >= 10 {
        if shows_terminal {
            "[ t term ]"
        } else {
            "[ ≡ list ]"
        }
    } else if shows_terminal {
        "[t]"
    } else {
        "[≡]"
    }
}

fn inspect_text(inspect_width: u16) -> &'static str {
    if inspect_width >= 10 {
        " inspect  "
    } else {
        " i "
    }
}

/// The standalone frame a collection gets while it has no member pane for the
/// server to draw a border for: the same box a member pane's border draws, in
/// the same color, so an empty collection is framed instead of blank.
fn draw_collection_frame(buffer: &mut Buffer, surface: &CollectionSurface, palette: &Palette) {
    let rect = surface.rect.intersection(buffer.area);
    if rect.width < 3 || rect.height < 3 {
        return;
    }
    let style = Style::default().fg(if surface.focused {
        palette.accent
    } else {
        palette.overlay0
    });
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(style);
    ratatui::widgets::Widget::render(block, rect, buffer);
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            if x == rect.x || x == rect.right() - 1 || y == rect.y || y == rect.bottom() - 1 {
                buffer[(x, y)].set_skip(false);
            }
        }
    }
}

pub(super) fn render_collection_chrome(
    buffer: &mut Buffer,
    snapshot: &crate::protocol::ClientShellSnapshot,
    structure: Option<&crate::protocol::endpoint::ClientShellStructureProjection>,
    documented_delegations: Option<&super::document_navigation::DelegationNavigation>,
    panes: &[PaneHit],
    surfaces: &[CollectionSurface],
    list_views: &HashSet<String>,
    maximized_pane: Option<&(String, String)>,
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
        let documented_delegation = documented_delegations
            .as_ref()
            .and_then(|delegations| delegations.for_pane(&selected.pane_id));
        let delegation = documented_delegations
            .is_none()
            .then(|| delegations.get(selected.pane_id.as_str()).copied())
            .flatten();
        let header_y = pane.inner_rect.y.saturating_sub(1);
        if header_y < pane.rect.y || pane.inner_rect.width < 12 {
            continue;
        }
        let rect = Rect::new(pane.inner_rect.x, header_y, pane.inner_rect.width, 1);
        let base = Style::default().fg(palette.text).bg(Color::Reset);
        buffer.set_style(rect, base);
        for x in rect.x..rect.right() {
            buffer[(x, rect.y)]
                .set_symbol(" ")
                .set_style(base)
                .set_skip(false);
        }

        let (previous, next, presentation_toggle, inspect) = collection_control_rects(rect);
        put(
            buffer,
            previous,
            " ‹ ",
            Style::default().fg(palette.accent).bg(Color::Reset),
        );
        put(
            buffer,
            next,
            " › ",
            Style::default().fg(palette.accent).bg(Color::Reset),
        );
        let shows_terminal = list_views.contains(&collection.collection_id)
            && maximized_pane
                .is_none_or(|(collection_id, _)| collection_id != &collection.collection_id);
        put(
            buffer,
            presentation_toggle,
            presentation_toggle_text(presentation_toggle.width, shows_terminal),
            Style::default().fg(palette.overlay1).bg(Color::Reset),
        );
        put(
            buffer,
            inspect,
            inspect_text(inspect.width),
            Style::default().fg(palette.teal).bg(Color::Reset),
        );

        let content_x = previous.right().saturating_add(1);
        let content_width = presentation_toggle
            .x
            .saturating_sub(content_x)
            .saturating_sub(1);
        let parent_outside_collection = documented_delegation
            .and_then(|entry| entry.parent_pane_id.as_deref())
            .or_else(|| {
                delegation
                    .and_then(|entry| entry.parent_id.as_deref())
                    .and_then(|parent_id| delegations_by_id.get(parent_id))
                    .and_then(|parent| parent.pane_id.as_deref())
            })
            .is_some_and(|parent_pane_id| {
                !members
                    .iter()
                    .any(|member| member.pane_id == parent_pane_id)
            });
        let structure_icon = if let Some(entry) = documented_delegation {
            if parent_outside_collection {
                "↗"
            } else if entry.depth > 0 {
                "↳"
            } else {
                "◆"
            }
        } else {
            match delegation {
                Some(entry) if entry.external_parent_id.is_some() || parent_outside_collection => {
                    "↗"
                }
                Some(entry) if entry.depth > 0 => "↳",
                Some(_) => "◆",
                None => "·",
            }
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
                documented_delegation
                    .map(|entry| entry.label.as_str())
                    .or_else(|| delegation.and_then(|entry| entry.purpose.as_deref())),
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
        let surface_rect = surfaces
            .iter()
            .find(|surface| surface.collection_id == collection.collection_id)
            .map_or(pane.rect, |surface| surface.rect);
        hits.push(CollectionChromeHit {
            rect,
            surface_rect,
            collection_id: collection.collection_id.clone(),
            selected_pane_id: Some(pane.pane_id.clone()),
            previous_pane_id,
            next_pane_id,
            previous,
            next,
            presentation_toggle,
            inspect,
        });
    }

    for collection in structure
        .collections
        .iter()
        .filter(|collection| collection.selected_pane_id.is_none())
    {
        let Some(surface) = surfaces
            .iter()
            .find(|surface| surface.collection_id == collection.collection_id)
        else {
            continue;
        };
        let header_y = surface.inner_rect.y.saturating_sub(1);
        if header_y < surface.rect.y || surface.inner_rect.width < 12 {
            continue;
        }
        // Same frame, header and controls as a collection with terminals; only
        // the member name and the body differ, so an empty collection is never a
        // blank or unframed area.
        draw_collection_frame(buffer, surface, palette);
        // The collection's inner rect spans its full width, frame included, so
        // blank only what is inside the box: its sides and bottom stay drawn.
        let area = Rect::new(
            surface.inner_rect.x.saturating_add(1),
            surface.inner_rect.y,
            surface.inner_rect.width.saturating_sub(2),
            surface.inner_rect.height.saturating_sub(1),
        )
        .intersection(buffer.area);
        let base = Style::default().fg(palette.text).bg(Color::Reset);
        if !area.is_empty() {
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    buffer[(x, y)]
                        .set_symbol(" ")
                        .set_style(base)
                        .set_skip(false);
                }
            }
        }
        let rect = Rect::new(surface.inner_rect.x, header_y, surface.inner_rect.width, 1);
        buffer.set_style(rect, base);
        for x in rect.x..rect.right() {
            buffer[(x, rect.y)]
                .set_symbol(" ")
                .set_style(base)
                .set_skip(false);
        }
        let (previous, next, presentation_toggle, inspect) = collection_control_rects(rect);
        put(
            buffer,
            previous,
            " ‹ ",
            Style::default().fg(palette.overlay0).bg(Color::Reset),
        );
        put(
            buffer,
            next,
            " › ",
            Style::default().fg(palette.overlay0).bg(Color::Reset),
        );
        let shows_terminal =
            list_views.contains(&collection.collection_id) && maximized_pane.is_none();
        put(
            buffer,
            presentation_toggle,
            presentation_toggle_text(presentation_toggle.width, shows_terminal),
            Style::default().fg(palette.overlay1).bg(Color::Reset),
        );
        put(
            buffer,
            inspect,
            inspect_text(inspect.width),
            Style::default().fg(palette.overlay0).bg(Color::Reset),
        );
        let content_x = previous.right().saturating_add(1);
        let content_width = presentation_toggle
            .x
            .saturating_sub(content_x)
            .saturating_sub(1);
        let label = collection.label.as_deref().unwrap_or("Collection");
        let text = format!("{label} 0/0 · no terminals");
        put(
            buffer,
            Rect::new(content_x, rect.y, content_width, 1),
            &text,
            base,
        );
        // The Vqro-drawn empty state: what is missing and the existing command
        // that adds a pane to this collection.
        if area.height >= 1 {
            let title = "No terminals in this collection";
            let width = u16::try_from(title.len())
                .unwrap_or(u16::MAX)
                .min(area.width);
            let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
            let y = area.y.saturating_add(area.height.saturating_sub(2) / 2);
            put(
                buffer,
                Rect::new(x, y, width, 1),
                title,
                Style::default().fg(palette.overlay1).bg(Color::Reset),
            );
        }
        if area.height >= 3 {
            let hint = "add a pane with: vqro collection add <collection> <pane>";
            let width = u16::try_from(hint.len())
                .unwrap_or(u16::MAX)
                .min(area.width);
            let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
            let y = area.y.saturating_add(area.height.saturating_sub(2) / 2 + 1);
            put(
                buffer,
                Rect::new(x, y, width, 1),
                hint,
                Style::default().fg(palette.overlay0).bg(Color::Reset),
            );
        }
        hits.push(CollectionChromeHit {
            rect,
            surface_rect: surface.rect,
            collection_id: collection.collection_id.clone(),
            selected_pane_id: None,
            previous_pane_id: None,
            next_pane_id: None,
            previous: Rect::default(),
            next: Rect::default(),
            presentation_toggle: Rect::default(),
            inspect: Rect::default(),
        });
    }
    hits
}

pub(super) struct CollectionProjection<'a> {
    pub(super) structure: Option<&'a crate::protocol::endpoint::ClientShellStructureProjection>,
    pub(super) host_documents: Option<&'a crate::protocol::endpoint::HostDocumentsProjectionV1>,
    pub(super) documented_delegations: Option<&'a super::document_navigation::DelegationNavigation>,
    pub(super) surfaces: &'a [CollectionSurface],
}

pub(super) fn render_collection_lists(
    buffer: &mut Buffer,
    snapshot: &crate::protocol::ClientShellSnapshot,
    projection: CollectionProjection<'_>,
    panes: &[PaneHit],
    list_views: &HashSet<String>,
    expanded_members: &HashMap<String, HashSet<String>>,
    preview_heights: &HashMap<String, u16>,
    maximized_pane: Option<&(String, String)>,
    entered_pane: Option<&str>,
    terminal_surfaces: &HashMap<String, crate::protocol::endpoint::ServerTerminalViewSurface>,
    palette: &Palette,
) -> Vec<CollectionMemberHit> {
    let Some(structure) = projection.structure.filter(|structure| {
        structure.boot_id == snapshot.boot_id && structure.revision == snapshot.revision
    }) else {
        return Vec::new();
    };
    let documented_delegations = projection.documented_delegations;
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
        .filter(|collection| collection.selected_pane_id.is_none())
        .filter(|collection| list_views.contains(&collection.collection_id))
    {
        let Some(area) = projection
            .surfaces
            .iter()
            .find(|surface| surface.collection_id == collection.collection_id)
            .map(|surface| surface.inner_rect.intersection(buffer.area))
            .filter(|area| !area.is_empty())
        else {
            continue;
        };
        let base = Style::default().fg(palette.text).bg(Color::Reset);
        buffer.set_style(area, base);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                buffer[(x, y)]
                    .set_symbol(" ")
                    .set_style(base)
                    .set_skip(false);
            }
        }
        let message = "No members";
        let width = u16::try_from(message.len())
            .unwrap_or(u16::MAX)
            .min(area.width);
        let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
        let y = area.y.saturating_add(area.height / 2);
        put(
            buffer,
            Rect::new(x, y, width, 1),
            message,
            Style::default().fg(palette.overlay1).bg(Color::Reset),
        );
    }

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
            .map(|pane| {
                let right = pane
                    .scrollbar_rect
                    .map_or(pane.inner_rect.right(), |scrollbar| scrollbar.right())
                    .max(pane.inner_rect.right());
                Rect::new(
                    pane.inner_rect.x,
                    pane.inner_rect.y,
                    right.saturating_sub(pane.inner_rect.x),
                    pane.inner_rect.height,
                )
                .intersection(buffer.area)
            })
            .filter(|area| !area.is_empty())
        else {
            continue;
        };
        // Clear list-owned cells to the terminal's configured default background. Color::Reset is
        // explicit: unlike an unset Style background, it erases stale child colors while retaining
        // the user's normal terminal transparency or background.
        let base = Style::default().fg(palette.text).bg(Color::Reset);
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
            let surface = terminal_surfaces.get(pane_id);
            let scroll = surface.and_then(terminal_scroll_metrics);
            let preview = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
            if let Some(surface) = surface {
                if let Some(source) = surface.frame.to_ratatui_buffer() {
                    let height = preview.height.min(source.area.height);
                    let width = preview.width.min(source.area.width);
                    for preview_y in 0..height {
                        for preview_x in 0..width {
                            buffer[(preview.x + preview_x, preview.y + preview_y)] = source
                                [(source.area.x + preview_x, source.area.y + preview_y)]
                                .clone();
                            buffer[(preview.x + preview_x, preview.y + preview_y)].set_skip(false);
                        }
                    }
                }
            } else {
                put(buffer, preview, "loading terminal…", base);
            }
            let scrollbar = scroll
                .filter(|metrics| metrics.max_offset_from_bottom > 0)
                .map(|metrics| {
                    let track = Rect::new(preview.right(), preview.y, 1, preview.height);
                    render_preview_scrollbar(
                        buffer,
                        metrics,
                        track,
                        palette,
                        entered_pane == Some(pane_id.as_str()),
                    );
                    track
                });
            hits.push(CollectionMemberHit {
                rect: Rect::default(),
                disclosure: Rect::default(),
                preview,
                resize_handle: Rect::default(),
                scrollbar,
                scroll,
                collection_id: collection.collection_id.clone(),
                pane_id: pane_id.clone(),
                host_document_action: None,
            });
            continue;
        }

        let tab_id = projected_panes
            .get(selected_pane_id)
            .map(|pane| pane.tab_id.as_str());
        let document_entry = projection
            .host_documents
            .filter(|projection| {
                projection.boot_id == snapshot.boot_id && projection.revision == snapshot.revision
            })
            .and_then(|projection| {
                projection
                    .documents
                    .iter()
                    .filter(|entry| {
                        let document = &entry.document;
                        matches!(
                            &document.target,
                            crate::extension_contract::HostTarget::Tab { tab_id: target }
                                if Some(target.as_str()) == tab_id
                        ) && document.items.iter().any(|item| {
                            item.terminal
                                .as_ref()
                                .and_then(|terminal| terminal.placement.as_ref())
                                .is_some_and(|placement| {
                                    collection
                                        .members
                                        .iter()
                                        .any(|member| member.pane_id == placement.pane_id)
                                })
                        })
                    })
                    .max_by(|left, right| {
                        left.provider_priority
                            .cmp(&right.provider_priority)
                            .then_with(|| right.document.namespace.cmp(&left.document.namespace))
                    })
            });
        let document = document_entry.map(|entry| &entry.document);
        let item_for_pane = |pane_id: &str| {
            document.and_then(|document| {
                document.items.iter().find(|item| {
                    item.terminal
                        .as_ref()
                        .and_then(|terminal| terminal.placement.as_ref())
                        .is_some_and(|placement| placement.pane_id == pane_id)
                })
            })
        };
        let mut members = collection.members.iter().collect::<Vec<_>>();
        members.sort_by_key(|member| {
            item_for_pane(&member.pane_id)
                .and_then(|item| {
                    document?
                        .items
                        .iter()
                        .position(|candidate| candidate.id == item.id)
                })
                .unwrap_or(usize::MAX)
        });
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
            Style::default().fg(palette.overlay0).bg(Color::Reset),
        );
        y = y.saturating_add(1);
        let mut rendered_members = 0usize;

        for archived in [false, true] {
            if archived && archived_count > 0 && y < area.bottom() {
                put(
                    buffer,
                    Rect::new(area.x + 1, y, area.width.saturating_sub(2), 1),
                    &format!("archive {archived_count}"),
                    Style::default().fg(palette.overlay0).bg(Color::Reset),
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
                let documented_delegation = documented_delegations
                    .as_ref()
                    .and_then(|delegations| delegations.for_pane(&member.pane_id));
                let delegation = documented_delegations
                    .is_none()
                    .then(|| delegations.get(member.pane_id.as_str()).copied())
                    .flatten();
                let parent_pane_id = documented_delegation
                    .and_then(|entry| entry.parent_pane_id.as_deref())
                    .or_else(|| {
                        delegation
                            .and_then(|entry| entry.parent_id.as_deref())
                            .and_then(|parent_id| delegations_by_id.get(parent_id).copied())
                            .and_then(|parent| parent.pane_id.as_deref())
                    });
                let local_parent = parent_pane_id.is_some_and(|parent_pane_id| {
                    archived_by_pane.get(parent_pane_id) == Some(&member.archived)
                });
                let has_parent = documented_delegation
                    .is_some_and(|entry| entry.parent_id.is_some())
                    || delegation.is_some_and(|entry| entry.parent_id.is_some());
                let external_parent = documented_delegation
                    .is_some_and(|_| has_parent && !local_parent)
                    || delegation.is_some_and(|entry| {
                        entry.external_parent_id.is_some() || (has_parent && !local_parent)
                    });
                let depth = if local_parent {
                    documented_delegation.map_or_else(
                        || delegation.map_or(0, |entry| entry.depth.min(8)),
                        |entry| entry.depth.min(8),
                    )
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
                let item = item_for_pane(&member.pane_id);
                let name = item.map_or_else(
                    || {
                        member_name(
                            &member.pane_id,
                            documented_delegation
                                .map(|entry| entry.label.as_str())
                                .or_else(|| delegation.and_then(|entry| entry.purpose.as_deref())),
                            agents.get(member.pane_id.as_str()).copied(),
                            projected_panes.get(member.pane_id.as_str()).copied(),
                        )
                    },
                    |item| item.label.clone(),
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
                        Style::default().fg(palette.text).bg(Color::Reset)
                    } else {
                        Style::default()
                            .fg(match state {
                                crate::api::schema::AgentStatus::Working => palette.green,
                                crate::api::schema::AgentStatus::Blocked => palette.yellow,
                                crate::api::schema::AgentStatus::Done => palette.teal,
                                crate::api::schema::AgentStatus::Idle => palette.overlay0,
                                crate::api::schema::AgentStatus::Unknown => palette.subtext0,
                            })
                            .bg(Color::Reset)
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
                        .saturating_sub(remaining_rows)
                        .saturating_sub(1); // terminal resize handle
                    let preview_height = preview_heights
                        .get(&member.pane_id)
                        .copied()
                        .unwrap_or_else(|| (area.height / 2).clamp(4, 12))
                        .clamp(2, area.height.saturating_sub(2).max(2))
                        .min(available);
                    let preview_width = area.width.saturating_sub(2);
                    if preview_height >= 2 && preview_width >= 4 {
                        preview = Rect::new(area.x + 1, y, preview_width, preview_height);
                        buffer
                            .set_style(preview, Style::default().fg(palette.text).bg(Color::Reset));
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
                                Style::default().fg(palette.overlay0).bg(Color::Reset),
                            );
                        }
                        y = y.saturating_add(preview_height);
                    }
                }
                let surface = terminal_surfaces.get(&member.pane_id);
                let scroll = surface.and_then(terminal_scroll_metrics);
                let scrollbar = (!preview.is_empty()
                    && scroll.is_some_and(|metrics| metrics.max_offset_from_bottom > 0))
                .then(|| Rect::new(preview.right(), preview.y, 1, preview.height));
                if let (Some(track), Some(metrics)) = (scrollbar, scroll) {
                    render_preview_scrollbar(
                        buffer,
                        metrics,
                        track,
                        palette,
                        entered_pane == Some(member.pane_id.as_str()),
                    );
                }
                let resize_handle = if !preview.is_empty() && y < area.bottom() {
                    // Span the full list interior so both corners align with its edges. The
                    // terminal remains inset by one cell, and its scrollbar lands on the same
                    // column as the lower-right corner.
                    let handle = Rect::new(area.x, y, area.width, 1);
                    let grip_width = 17.min(handle.width);
                    let grip = Rect::new(
                        handle
                            .x
                            .saturating_add(handle.width.saturating_sub(grip_width) / 2),
                        handle.y,
                        grip_width,
                        1,
                    );
                    let edge_style = Style::default().fg(palette.overlay1).bg(Color::Reset);
                    buffer.set_style(handle, edge_style);
                    for x in handle.x..handle.right() {
                        buffer[(x, handle.y)]
                            .set_symbol("─")
                            .set_style(edge_style)
                            .set_skip(false);
                    }
                    put(buffer, grip, "[ drag height ↕ ]", edge_style);
                    put(buffer, Rect::new(handle.x, handle.y, 1, 1), "└", edge_style);
                    put(
                        buffer,
                        Rect::new(handle.right().saturating_sub(1), handle.y, 1, 1),
                        "┘",
                        edge_style,
                    );
                    y = y.saturating_add(1);
                    handle
                } else {
                    Rect::default()
                };
                hits.push(CollectionMemberHit {
                    rect: row,
                    disclosure,
                    preview,
                    resize_handle,
                    scrollbar,
                    scroll,
                    collection_id: collection.collection_id.clone(),
                    pane_id: member.pane_id.clone(),
                    host_document_action: document.and_then(|document| {
                        let item = item.filter(|item| !item.flags.disabled)?;
                        let action_id = item
                            .action_ids
                            .iter()
                            .find(|action_id| {
                                document.actions.iter().any(|action| {
                                    action.id == action_id.as_str()
                                        && action
                                            .subject_id
                                            .as_deref()
                                            .is_none_or(|subject_id| subject_id == item.id)
                                })
                            })?
                            .clone();
                        Some(crate::api::schema::HostDocumentActionParams {
                            namespace: document.namespace.clone(),
                            scope_id: match &document.target {
                                crate::extension_contract::HostTarget::Tab { tab_id } => {
                                    tab_id.clone()
                                }
                                _ => return None,
                            },
                            generation: document.generation,
                            revision: document.revision,
                            confirmation_token: document_entry
                                .and_then(|entry| entry.confirmation_tokens.get(&action_id))
                                .cloned(),
                            action_id,
                            confirmed: false,
                        })
                    }),
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
                Style::default().fg(palette.overlay0).bg(Color::Reset),
            );
        }
    }
    hits
}
