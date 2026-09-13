use super::*;

mod settings_overlay;
mod worktree_overlays;

#[derive(Default)]
pub(crate) struct OverlayRender {
    pub(crate) area: Rect,
    pub(crate) menu_rows: Vec<(Rect, usize)>,
    pub(crate) primary: Rect,
    pub(crate) clear: Rect,
    pub(crate) cancel: Rect,
    pub(crate) navigator_popup: Rect,
    pub(crate) navigator_search: Rect,
    pub(crate) navigator_rows: Vec<(Rect, ClientNavigatorTarget)>,
    pub(crate) worktree_search: Rect,
    pub(crate) worktree_rows: Vec<(Rect, usize)>,
    pub(crate) help_popup: Rect,
    pub(crate) help_scrollbar: Rect,
    pub(crate) help_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(crate) help_max_scroll: usize,
    pub(crate) settings_popup: Rect,
    pub(crate) settings_tabs: Vec<(Rect, ClientSettingsSection)>,
    pub(crate) settings_choices: Vec<(Rect, usize)>,
    pub(crate) product_announcement_scrollbar: Rect,
    pub(crate) product_announcement_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(crate) product_announcement_max_scroll: usize,
    pub(crate) release_notes_scrollbar: Rect,
    pub(crate) release_notes_scroll_metrics: Option<crate::pane::ScrollMetrics>,
    pub(crate) release_notes_max_scroll: usize,
    pub(crate) contribution_actions: Vec<(Rect, String)>,
    pub(crate) cursor: Option<crate::protocol::CursorState>,
}

pub(crate) fn render_client_overlay(
    b: &mut Buffer,
    o: &ClientShellOverlay,
    s: &ClientShellSnapshot,
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    k: &LiveKeybindConfig,
    p: &Palette,
) -> Option<OverlayRender> {
    if !matches!(
        o,
        ClientShellOverlay::Navigator(_)
            | ClientShellOverlay::ContextMenu(_)
            | ClientShellOverlay::GlobalMenu(_)
    ) {
        for y in b.area.y..b.area.bottom() {
            for x in b.area.x..b.area.right() {
                let c = &mut b[(x, y)];
                c.set_style(c.style().add_modifier(Modifier::DIM));
            }
        }
    }
    match o {
        ClientShellOverlay::Onboarding => render_onboarding_overlay(b, p),
        ClientShellOverlay::ProductAnnouncement(v) => render_product_announcement_overlay(b, v, p),
        ClientShellOverlay::ReleaseNotes(v) => {
            render_release_notes_overlay(b, v, &s.update_install_command, p)
        }
        ClientShellOverlay::Rename(v) => render_rename_overlay(b, v, p),
        ClientShellOverlay::ConfirmClose(v) => render_confirm_close_overlay(b, v, p),
        ClientShellOverlay::PaneInterruptConfirm(v) => render_confirm_close_overlay(
            b,
            &ClientConfirmCloseOverlay {
                target: ClientConfirmCloseTarget::Workspace {
                    workspace_id: String::new(),
                },
                title: "Interrupt terminal process?".into(),
                detail: format!("Send Ctrl-C to {}. This cannot be undone.", v.pane_id),
            },
            p,
        ),
        ClientShellOverlay::Help(v) => render_help_overlay(b, v, k, p),
        ClientShellOverlay::Navigator(v) => {
            render_navigator_overlay(b, v, endpoints, active_endpoint_id, p)
        }
        ClientShellOverlay::Settings(v) => {
            settings_overlay::render_settings_overlay(b, v, s.integration_updates_available, p)
        }
        ClientShellOverlay::WorktreeCreate(v) => {
            worktree_overlays::render_worktree_create_overlay(b, v, p)
        }
        ClientShellOverlay::WorktreeOpen(v) => {
            worktree_overlays::render_worktree_open_overlay(b, v, p)
        }
        ClientShellOverlay::WorktreeRemove(v) => {
            worktree_overlays::render_worktree_remove_overlay(b, v, p)
        }
        ClientShellOverlay::ContextMenu(_) | ClientShellOverlay::GlobalMenu(_) => None,
    }
}

pub(crate) fn render_global_menu(
    buffer: &mut Buffer,
    launcher: Rect,
    menu: &ClientGlobalMenuOverlay,
    snapshot: &ClientShellSnapshot,
    palette: &Palette,
) -> Option<OverlayRender> {
    let items = super::super::global_menu::global_menu_items(snapshot);
    let screen = buffer.area;
    let width = items
        .iter()
        .map(|(label, action)| {
            display_width(label)
                + u16::from(super::super::global_menu::global_menu_item_has_badge(
                    snapshot, *action,
                )) * 2
        })
        .max()
        .unwrap_or(8)
        .saturating_add(4)
        .min(screen.width.max(1));
    let height = (items.len() as u16)
        .saturating_add(2)
        .min(screen.height.max(1));
    let x = launcher
        .right()
        .saturating_sub(width)
        .min(screen.right().saturating_sub(width));
    let y = launcher.y.saturating_sub(height).max(screen.y);
    let rect = Rect::new(x, y, width, height);
    let inner = panel(buffer, rect, palette.accent, palette.panel_bg)?;
    let mut rows = Vec::new();
    for (index, (label, action)) in items.iter().enumerate() {
        let row_y = inner.y.saturating_add(index as u16);
        if row_y >= inner.bottom() {
            break;
        }
        let row = Rect::new(inner.x, row_y, inner.width, 1);
        let highlighted = index == menu.highlighted;
        let style = if highlighted {
            Style::default()
                .fg(panel_contrast_fg(palette))
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.text).bg(palette.panel_bg)
        };
        buffer.set_style(row, style);
        let has_badge = super::super::global_menu::global_menu_item_has_badge(snapshot, *action);
        if has_badge {
            let badge_style = if highlighted {
                style
            } else {
                Style::default()
                    .fg(palette.accent)
                    .bg(palette.panel_bg)
                    .add_modifier(Modifier::BOLD)
            };
            put_text(buffer, row.x, row.y, row.width.min(2), " ●", badge_style);
            put_text(
                buffer,
                row.x.saturating_add(2),
                row.y,
                row.width.saturating_sub(2),
                &format!(" {label}"),
                style,
            );
        } else {
            put_text(buffer, row.x, row.y, row.width, &format!(" {label}"), style);
        }
        rows.push((row, index));
    }
    Some(OverlayRender {
        area: rect,
        menu_rows: rows,
        ..OverlayRender::default()
    })
}

pub(crate) fn render_context_menu(
    buffer: &mut Buffer,
    menu: &ClientContextMenuOverlay,
    palette: &Palette,
) -> Option<OverlayRender> {
    let items = menu.items();
    let screen = buffer.area;
    let max_item_width = items
        .iter()
        .map(|item| display_width(item.label))
        .max()
        .unwrap_or(0);
    let width = max_item_width
        .saturating_add(4)
        .max(14)
        .min(screen.width.max(1));
    let height = (items.len() as u16)
        .saturating_add(2)
        .min(screen.height.max(1));
    let x = menu
        .x
        .min(screen.x.saturating_add(screen.width.saturating_sub(width)));
    let y = menu.y.min(
        screen
            .y
            .saturating_add(screen.height.saturating_sub(height)),
    );
    let rect = Rect::new(x, y, width, height);
    let inner = panel(buffer, rect, palette.accent, palette.panel_bg)?;
    let mut rows = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let row_y = inner.y.saturating_add(index as u16);
        if row_y >= inner.bottom() {
            break;
        }
        let row = Rect::new(inner.x, row_y, inner.width, 1);
        let highlighted = index == menu.highlighted;
        let style = if highlighted {
            Style::default()
                .fg(panel_contrast_fg(palette))
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.text).bg(palette.panel_bg)
        };
        buffer.set_style(row, style);
        put_text(buffer, row.x, row.y, row.width, item.label, style);
        rows.push((row, index));
    }
    Some(OverlayRender {
        area: rect,
        menu_rows: rows,
        ..OverlayRender::default()
    })
}

fn panel(
    b: &mut Buffer,
    a: Rect,
    c: ratatui::style::Color,
    bg: ratatui::style::Color,
) -> Option<Rect> {
    let a = a.intersection(b.area);
    if a.width < 2 || a.height < 2 {
        return None;
    }
    let background = Style::default().bg(bg).remove_modifier(Modifier::DIM);
    let border = Style::default().fg(c).bg(bg).remove_modifier(Modifier::DIM);
    for y in a.y..a.bottom() {
        for x in a.x..a.right() {
            b[(x, y)]
                .set_symbol(" ")
                .set_style(background)
                .set_skip(false);
        }
    }
    for x in a.x..a.right() {
        b[(x, a.y)]
            .set_symbol(if x == a.x {
                "┌"
            } else if x + 1 == a.right() {
                "┐"
            } else {
                "─"
            })
            .set_style(border);
        let y = a.bottom() - 1;
        b[(x, y)]
            .set_symbol(if x == a.x {
                "└"
            } else if x + 1 == a.right() {
                "┘"
            } else {
                "─"
            })
            .set_style(border);
    }
    for y in a.y + 1..a.bottom() - 1 {
        b[(a.x, y)].set_symbol("│").set_style(border);
        b[(a.right() - 1, y)].set_symbol("│").set_style(border);
    }
    Some(Rect::new(a.x + 1, a.y + 1, a.width - 2, a.height - 2))
}
fn pane_section(b: &mut Buffer, area: Rect, bg: ratatui::style::Color) -> Option<Rect> {
    let area = area.intersection(b.area);
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let style = Style::default().bg(bg).remove_modifier(Modifier::DIM);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            b[(x, y)].set_symbol(" ").set_style(style).set_skip(false);
        }
    }
    Some(area)
}

pub(in crate::client::shell) fn render_pane_contribution_divider(
    b: &mut Buffer,
    divider: Rect,
    p: &Palette,
) {
    let divider = divider.intersection(b.area);
    if divider.is_empty() {
        return;
    }
    let style = Style::default()
        .fg(p.surface1)
        .bg(p.panel_bg)
        .remove_modifier(Modifier::DIM);
    let symbol = if divider.width == 1 { "│" } else { "─" };
    for y in divider.y..divider.bottom() {
        for x in divider.x..divider.right() {
            b[(x, y)]
                .set_symbol(symbol)
                .set_style(style)
                .set_skip(false);
        }
    }
}

fn popup(a: Rect, w: u16, h: u16) -> Option<Rect> {
    let w = w.min(a.width.saturating_sub(4));
    let h = h.min(a.height.saturating_sub(2));
    if w < 4 || h < 4 {
        return None;
    }
    Some(Rect::new(
        a.x + (a.width - w) / 2,
        a.y + (a.height - h) / 2,
        w,
        h,
    ))
}
fn button(b: &mut Buffer, r: Rect, t: &str, s: Style) {
    b.set_style(r, s);
    let w = display_width(t).min(r.width);
    put_text(b, r.x + (r.width - w) / 2, r.y, w, t, s)
}
fn row(i: Rect, ws: &[u16], gap: u16, off: u16) -> Vec<Rect> {
    let total = ws.iter().sum::<u16>() + gap * (ws.len().saturating_sub(1) as u16);
    let mut x = i.x + i.width.saturating_sub(total) / 2;
    ws.iter()
        .map(|w| {
            let r = Rect::new(
                x,
                i.y + off.min(i.height.saturating_sub(1)),
                (*w).min(i.width.saturating_sub(x - i.x)),
                1,
            );
            x += *w + gap;
            r
        })
        .collect()
}
fn contrast(p: &Palette) -> ratatui::style::Color {
    match p.panel_bg {
        ratatui::style::Color::Reset => p.surface_dim,
        c => c,
    }
}
fn render_release_notes_overlay(
    b: &mut Buffer,
    notes: &crate::app::state::ReleaseNotesState,
    install_command: &str,
    p: &Palette,
) -> Option<OverlayRender> {
    let outer = popup(
        b.area,
        crate::ui::RELEASE_NOTES_MODAL_SIZE.0,
        crate::ui::RELEASE_NOTES_MODAL_SIZE.1,
    )?;
    let inner = panel(b, outer, p.accent, p.panel_bg)?;
    if inner.height < 8 || inner.width < 20 {
        return Some(OverlayRender {
            area: outer,
            ..OverlayRender::default()
        });
    }

    let stack = crate::ui::modal_stack_areas(inner, 2, 1, 0, 1);
    let title_area = Rect::new(
        stack.header.x.saturating_add(1),
        stack.header.y,
        stack.header.width.saturating_sub(2),
        1,
    );
    let subtitle_area = Rect::new(
        stack.header.x.saturating_add(1),
        stack.header.y.saturating_add(1),
        stack.header.width.saturating_sub(2),
        1,
    );
    let base = Style::default()
        .bg(p.panel_bg)
        .remove_modifier(Modifier::DIM);
    put_text(
        b,
        title_area.x,
        title_area.y,
        title_area.width,
        &format!("v{}", notes.version),
        base.fg(p.text).add_modifier(Modifier::BOLD),
    );
    put_text(
        b,
        subtitle_area.x,
        subtitle_area.y,
        subtitle_area.width,
        if notes.preview {
            "update ready"
        } else {
            "what's new in this release"
        },
        base.fg(p.overlay1),
    );
    let close = crate::ui::release_notes_close_button_rect(Rect::new(
        stack.header.x,
        stack.header.y,
        stack.header.width,
        1,
    ));
    button(
        b,
        close,
        " esc close ",
        Style::default()
            .fg(contrast(p))
            .bg(p.accent)
            .add_modifier(Modifier::BOLD)
            .remove_modifier(Modifier::DIM),
    );

    let body = stack.content;
    let lines = crate::ui::release_notes_display_lines(notes, install_command, p);
    let metrics = crate::ui::release_notes_scroll_metrics(notes, install_command, body, p);
    let max_scroll = metrics.max_offset_from_bottom;
    let scroll = usize::from(notes.scroll).min(max_scroll);
    let track = crate::ui::release_notes_scrollbar_rect(body, metrics);
    let text_area = track
        .map(|_| Rect::new(body.x, body.y, body.width.saturating_sub(1), body.height))
        .unwrap_or(body);
    let paragraph = ratatui::widgets::Paragraph::new(
        lines.into_iter().map(|(_, line)| line).collect::<Vec<_>>(),
    )
    .wrap(ratatui::widgets::Wrap { trim: false })
    .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0));
    ratatui::widgets::Widget::render(paragraph, text_area, b);
    if let Some(track) = track {
        crate::ui::render_scrollbar_buffer(b, metrics, track, p.overlay0, p.overlay1, "▐");
    }

    if let Some(footer_area) = stack.footer {
        let footer_line = ratatui::text::Line::from(vec![
            ratatui::text::Span::styled(" scroll ", base.fg(p.overlay0)),
            ratatui::text::Span::styled("wheel ↑↓", base.fg(p.text)),
            ratatui::text::Span::styled("  ·  ", base.fg(p.overlay0)),
            ratatui::text::Span::styled("close", base.fg(p.overlay0)),
            ratatui::text::Span::styled(" esc / enter ", base.fg(p.text)),
        ]);
        ratatui::widgets::Widget::render(
            ratatui::widgets::Paragraph::new(footer_line),
            footer_area,
            b,
        );
    }

    Some(OverlayRender {
        area: outer,
        primary: close,
        release_notes_scrollbar: track.unwrap_or_default(),
        release_notes_scroll_metrics: Some(metrics),
        release_notes_max_scroll: max_scroll,
        ..OverlayRender::default()
    })
}

fn render_product_announcement_overlay(
    b: &mut Buffer,
    announcement: &crate::app::state::ProductAnnouncementState,
    p: &Palette,
) -> Option<OverlayRender> {
    let outer = popup(
        b.area,
        crate::ui::PRODUCT_ANNOUNCEMENT_MODAL_SIZE.0,
        crate::ui::PRODUCT_ANNOUNCEMENT_MODAL_SIZE.1,
    )?;
    let inner = panel(b, outer, p.accent, p.panel_bg)?;
    if inner.height < 8 || inner.width < 20 {
        return Some(OverlayRender {
            area: outer,
            ..OverlayRender::default()
        });
    }

    let stack = crate::ui::modal_stack_areas(inner, 2, 1, 0, 1);
    let title_area = Rect::new(
        stack.header.x.saturating_add(1),
        stack.header.y,
        stack.header.width.saturating_sub(2),
        1,
    );
    let subtitle_area = Rect::new(
        stack.header.x.saturating_add(1),
        stack.header.y.saturating_add(1),
        stack.header.width.saturating_sub(2),
        1,
    );
    let base = Style::default()
        .bg(p.panel_bg)
        .remove_modifier(Modifier::DIM);
    put_text(
        b,
        title_area.x,
        title_area.y,
        title_area.width,
        &announcement.title,
        base.fg(p.text).add_modifier(Modifier::BOLD),
    );
    let subtitle = if announcement.preview {
        "product announcement preview"
    } else {
        "product announcement"
    };
    put_text(
        b,
        subtitle_area.x,
        subtitle_area.y,
        subtitle_area.width,
        &format!("{subtitle} · v{}", announcement.version),
        base.fg(p.overlay1),
    );
    let close = crate::ui::release_notes_close_button_rect(Rect::new(
        stack.header.x,
        stack.header.y,
        stack.header.width,
        1,
    ));
    button(
        b,
        close,
        " esc close ",
        Style::default()
            .fg(contrast(p))
            .bg(p.accent)
            .add_modifier(Modifier::BOLD)
            .remove_modifier(Modifier::DIM),
    );

    let body = stack.content;
    let lines = crate::ui::product_announcement_display_lines(announcement, p);
    let metrics = crate::ui::product_announcement_scroll_metrics(announcement, body, p);
    let max_scroll = metrics.max_offset_from_bottom;
    let scroll = usize::from(announcement.scroll).min(max_scroll);
    let track = crate::ui::release_notes_scrollbar_rect(body, metrics);
    let text_area = track
        .map(|_| Rect::new(body.x, body.y, body.width.saturating_sub(1), body.height))
        .unwrap_or(body);
    let paragraph = ratatui::widgets::Paragraph::new(
        lines.into_iter().map(|(_, line)| line).collect::<Vec<_>>(),
    )
    .wrap(ratatui::widgets::Wrap { trim: false })
    .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0));
    ratatui::widgets::Widget::render(paragraph, text_area, b);
    if let Some(track) = track {
        crate::ui::render_scrollbar_buffer(b, metrics, track, p.overlay0, p.overlay1, "▐");
    }

    if let Some(footer_area) = stack.footer {
        let footer_line = ratatui::text::Line::from(vec![
            ratatui::text::Span::styled(" scroll ", base.fg(p.overlay0)),
            ratatui::text::Span::styled("wheel ↑↓", base.fg(p.text)),
            ratatui::text::Span::styled("  ·  ", base.fg(p.overlay0)),
            ratatui::text::Span::styled("close", base.fg(p.overlay0)),
            ratatui::text::Span::styled(" esc / enter ", base.fg(p.text)),
        ]);
        ratatui::widgets::Widget::render(
            ratatui::widgets::Paragraph::new(footer_line),
            footer_area,
            b,
        );
    }

    Some(OverlayRender {
        area: outer,
        primary: close,
        product_announcement_scrollbar: track.unwrap_or_default(),
        product_announcement_scroll_metrics: Some(metrics),
        product_announcement_max_scroll: max_scroll,
        ..OverlayRender::default()
    })
}

fn render_view_document_body(
    b: &mut Buffer,
    area: Rect,
    document: &crate::view_document::ViewDocument,
    p: &Palette,
) {
    let base = Style::default()
        .bg(p.panel_bg)
        .remove_modifier(Modifier::DIM);
    let mut y = area.y;
    for section in &document.sections {
        if y >= area.bottom() {
            break;
        }
        if let Some(title) = &section.title {
            put_text(
                b,
                area.x,
                y,
                area.width,
                title,
                base.fg(p.accent).add_modifier(Modifier::BOLD),
            );
            y += 1;
        }
        for block in &section.blocks {
            if y >= area.bottom() {
                break;
            }
            match block {
                crate::view_document::ViewBlock::Text { text, tone } => {
                    put_text(
                        b,
                        area.x,
                        y,
                        area.width,
                        text,
                        inspector_tone_style(*tone, p),
                    );
                    y += 1;
                }
                crate::view_document::ViewBlock::Badge { label, value, tone } => {
                    let key_width = 12.min(area.width.saturating_sub(2) / 2).max(1);
                    put_text(b, area.x, y, key_width, label, base.fg(p.overlay0));
                    put_text(
                        b,
                        area.x.saturating_add(key_width + 1),
                        y,
                        area.width.saturating_sub(key_width + 1),
                        value,
                        inspector_tone_style(*tone, p).add_modifier(Modifier::BOLD),
                    );
                    y += 1;
                }
                crate::view_document::ViewBlock::KeyValue { items } => {
                    for item in items {
                        if y >= area.bottom() {
                            break;
                        }
                        let key_width = 18.min(area.width.saturating_sub(2) / 2).max(1);
                        put_text(b, area.x, y, key_width, &item.key, base.fg(p.overlay0));
                        put_text(
                            b,
                            area.x.saturating_add(key_width + 1),
                            y,
                            area.width.saturating_sub(key_width + 1),
                            &item.value,
                            inspector_tone_style(item.tone, p),
                        );
                        y += 1;
                    }
                }
                crate::view_document::ViewBlock::Table { columns, rows }
                    if area.width
                        < u16::try_from(columns.len())
                            .unwrap_or(u16::MAX)
                            .saturating_mul(7) =>
                {
                    for row in rows {
                        for (column, cell) in columns.iter().zip(row) {
                            if y >= area.bottom() {
                                break;
                            }
                            let text = format!("{column}  {cell}");
                            put_text(b, area.x, y, area.width, &text, base.fg(p.text));
                            y += 1;
                        }
                    }
                }
                crate::view_document::ViewBlock::Table { columns, rows } => {
                    let stride = area.width / u16::try_from(columns.len()).unwrap_or(1).max(1);
                    let cell_width = stride.saturating_sub(1).max(1);
                    for (index, column) in columns.iter().enumerate() {
                        let x = area.x.saturating_add(
                            u16::try_from(index)
                                .unwrap_or(u16::MAX)
                                .saturating_mul(stride),
                        );
                        put_text(b, x, y, cell_width, column, base.fg(p.overlay0));
                    }
                    y += 1;
                    for row in rows {
                        if y >= area.bottom() {
                            break;
                        }
                        for (index, cell) in row.iter().enumerate() {
                            let x = area.x.saturating_add(
                                u16::try_from(index)
                                    .unwrap_or(u16::MAX)
                                    .saturating_mul(stride),
                            );
                            put_text(b, x, y, cell_width, cell, base.fg(p.text));
                        }
                        y += 1;
                    }
                }
                crate::view_document::ViewBlock::Timeline { items } => {
                    for item in items {
                        if y >= area.bottom() {
                            break;
                        }
                        let text = item.detail.as_ref().map_or_else(
                            || format!("• {}", item.label),
                            |detail| format!("• {}  ·  {detail}", item.label),
                        );
                        put_text(
                            b,
                            area.x,
                            y,
                            area.width,
                            &text,
                            inspector_tone_style(item.tone, p),
                        );
                        y += 1;
                    }
                }
            }
        }
        y = y.saturating_add(1);
    }
}

fn inspector_tone_style(tone: crate::view_document::ViewTone, p: &Palette) -> Style {
    let color = match tone {
        crate::view_document::ViewTone::Default => p.text,
        crate::view_document::ViewTone::Muted => p.overlay0,
        crate::view_document::ViewTone::Accent => p.accent,
        crate::view_document::ViewTone::Success => p.green,
        crate::view_document::ViewTone::Warning => p.yellow,
        crate::view_document::ViewTone::Danger => p.red,
    };
    Style::default()
        .fg(color)
        .bg(p.panel_bg)
        .remove_modifier(Modifier::DIM)
}

pub(in crate::client::shell) fn render_pane_contribution(
    b: &mut Buffer,
    outer: Rect,
    contribution: &crate::pane_contribution::PaneContribution,
    p: &Palette,
) -> Option<OverlayRender> {
    let inner = pane_section(b, outer, p.panel_bg)?;
    if inner.width < 12 || inner.height < 6 {
        return Some(OverlayRender::default());
    }
    let base = Style::default()
        .bg(p.panel_bg)
        .remove_modifier(Modifier::DIM);
    let title = base.fg(p.text).add_modifier(Modifier::BOLD);
    let muted = base.fg(p.overlay0);
    let header = Rect::new(inner.x, inner.y, inner.width, 1);
    b.set_style(header, base.fg(p.text).bg(p.surface0));
    let close = Rect::new(inner.right().saturating_sub(3), inner.y, 3, 1);
    put_text(
        b,
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(5),
        &contribution.document.title,
        title.bg(p.surface0),
    );
    button(b, close, " × ", muted.bg(p.surface0));
    put_text(
        b,
        inner.x + 1,
        inner.y + 1,
        inner.width.saturating_sub(2),
        &format!("extension · {}", contribution.owner_service_id),
        muted,
    );
    put_text(
        b,
        inner.x,
        inner.y + 2,
        inner.width,
        &"─".repeat(inner.width as usize),
        base.fg(p.surface1),
    );
    let mut body_y = inner.y.saturating_add(3);
    if let Some(subtitle) = contribution.document.subtitle.as_deref() {
        put_text(
            b,
            inner.x + 1,
            body_y,
            inner.width.saturating_sub(2),
            subtitle,
            muted,
        );
        body_y = body_y.saturating_add(2);
    }
    render_view_document_body(
        b,
        Rect::new(
            inner.x + 1,
            body_y,
            inner.width.saturating_sub(2),
            inner.bottom().saturating_sub(body_y + 1),
        ),
        &contribution.document,
        p,
    );
    let footer_y = inner.bottom().saturating_sub(1);
    let footer = Rect::new(inner.x, footer_y, inner.width, 1);
    b.set_style(footer, base.bg(p.surface0));
    let mut action_x = inner.x + 1;
    let mut action_rows = Vec::new();
    for (index, action) in contribution.actions.iter().enumerate() {
        let has_more = index + 1 < contribution.actions.len();
        let overflow_reserve = if has_more { 9 } else { 0 };
        let available = inner
            .right()
            .saturating_sub(action_x + 1)
            .saturating_sub(overflow_reserve);
        if available < 4 {
            break;
        }
        let display = format!("{} {}", index + 1, action.label);
        let desired_width = u16::try_from(unicode_width::UnicodeWidthStr::width(display.as_str()))
            .unwrap_or(u16::MAX)
            .saturating_add(2);
        if index > 0 && desired_width > available {
            break;
        }
        let width = desired_width.min(available);
        let rect = Rect::new(action_x, footer_y, width, 1);
        button(
            b,
            rect,
            &format!(" {display} "),
            Style::default()
                .fg(if action.destructive { p.red } else { p.teal })
                .bg(p.surface0),
        );
        action_rows.push((rect, action.id.clone()));
        action_x = action_x.saturating_add(width + 1);
    }
    let hidden_actions = contribution.actions.len().saturating_sub(action_rows.len());
    if hidden_actions > 0 {
        put_text(
            b,
            inner.right().saturating_sub(8),
            footer_y,
            7,
            &format!("+{hidden_actions} more"),
            muted.bg(p.surface0),
        );
    }
    if action_rows.is_empty() && hidden_actions == 0 {
        put_text(
            b,
            inner.x + 1,
            footer_y,
            inner.width.saturating_sub(2),
            "esc close",
            muted.bg(p.surface0),
        );
    }
    Some(OverlayRender {
        cancel: close,
        contribution_actions: action_rows,
        ..OverlayRender::default()
    })
}

pub(in crate::client::shell) fn render_terminal_inspector_pending(
    b: &mut Buffer,
    outer: Rect,
    inspector: &ClientTerminalInspectorPane,
    p: &Palette,
) -> Option<OverlayRender> {
    let inner = pane_section(b, outer, p.panel_bg)?;
    if inner.width < 12 || inner.height < 6 {
        return Some(OverlayRender::default());
    }
    let base = Style::default()
        .bg(p.panel_bg)
        .remove_modifier(Modifier::DIM);
    let title = base.fg(p.text).add_modifier(Modifier::BOLD);
    let muted = base.fg(p.overlay0);
    let accent = base.fg(p.accent).add_modifier(Modifier::BOLD);
    let header = Rect::new(inner.x, inner.y, inner.width, 1);
    b.set_style(header, base.fg(p.text).bg(p.surface0));
    let close = Rect::new(inner.right().saturating_sub(3), inner.y, 3, 1);
    put_text(
        b,
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(5),
        "Terminal inspector",
        title.bg(p.surface0),
    );
    button(b, close, " × ", muted.bg(p.surface0));
    put_text(
        b,
        inner.x + 1,
        inner.y + 1,
        inner.width.saturating_sub(2),
        &format!(
            "{} · {} · {}",
            inspector.pane_label,
            inspector.pane_id,
            if inspector.loading {
                "loading"
            } else if inspector.error.is_some() {
                "error"
            } else {
                "ready"
            }
        ),
        muted,
    );
    let divider_y = inner.y + 2;
    put_text(
        b,
        inner.x,
        divider_y,
        inner.width,
        &"─".repeat(inner.width as usize),
        base.fg(p.surface1),
    );

    let mut y = divider_y + 1;
    if inspector.loading && inspector.telemetry.is_none() {
        put_text(
            b,
            inner.x + 1,
            y,
            inner.width.saturating_sub(2),
            "◌ sampling process telemetry…",
            accent,
        );
    } else if let Some(error) = inspector.error.as_deref() {
        put_text(
            b,
            inner.x + 1,
            y,
            inner.width.saturating_sub(2),
            "Telemetry unavailable",
            base.fg(p.red).add_modifier(Modifier::BOLD),
        );
        put_text(
            b,
            inner.x + 1,
            y + 1,
            inner.width.saturating_sub(2),
            error,
            muted,
        );
    } else if let Some(telemetry) = inspector.telemetry.as_ref() {
        let agent = telemetry
            .display_agent
            .as_deref()
            .or(telemetry.agent.as_deref())
            .unwrap_or("none");
        let status = format!("{:?}", telemetry.agent_status).to_ascii_lowercase();
        put_text(b, inner.x + 1, y, 10, "AGENT", muted);
        put_text(
            b,
            inner.x + 12,
            y,
            inner.width.saturating_sub(13),
            &format!("{agent}  ·  {status}"),
            accent,
        );
        y += 1;
        if !telemetry.state_labels.is_empty() {
            let labels = telemetry
                .state_labels
                .iter()
                .map(|(key, value)| format!("{key}: {value}"))
                .collect::<Vec<_>>()
                .join("  ·  ");
            put_text(
                b,
                inner.x + 12,
                y,
                inner.width.saturating_sub(13),
                &labels,
                muted,
            );
            y += 1;
        }
        put_text(b, inner.x + 1, y, 10, "SAMPLE", muted);
        put_text(
            b,
            inner.x + 12,
            y,
            inner.width.saturating_sub(13),
            &format!(
                "shell {}  ·  pgrp {}  ·  cache {} ms{}",
                telemetry
                    .shell_pid
                    .map_or_else(|| "—".into(), |value| value.to_string()),
                telemetry
                    .foreground_process_group_id
                    .map_or_else(|| "—".into(), |value| value.to_string()),
                telemetry.cache_age_ms,
                if inspector.loading {
                    "  ·  refreshing"
                } else {
                    ""
                },
            ),
            base.fg(p.overlay1),
        );
        y += 2;
        match telemetry.availability {
            crate::api::schema::PaneTelemetryAvailability::Available => {
                put_text(b, inner.x + 1, y, 7, "PID", muted);
                put_text(b, inner.x + 9, y, 20, "PROCESS", muted);
                put_text(b, inner.x + 30, y, 9, "CPU", muted);
                put_text(b, inner.x + 40, y, 11, "MEMORY", muted);
                put_text(b, inner.x + 52, y, 8, "THREADS", muted);
                put_text(
                    b,
                    inner.x + 61,
                    y,
                    inner.width.saturating_sub(62),
                    "I/O",
                    muted,
                );
                y += 1;
                let available_rows = inner.bottom().saturating_sub(y + 2) as usize;
                for process in telemetry.processes.iter().take(available_rows) {
                    let cpu = process.cpu_tenths_percent.map_or_else(
                        || "—".into(),
                        |value| format!("{}.{:01}%", value / 10, value % 10),
                    );
                    let memory = process
                        .resident_bytes
                        .map_or_else(|| "—".into(), format_bytes);
                    let threads = process
                        .thread_count
                        .map_or_else(|| "—".into(), |value| value.to_string());
                    let io = process.read_bytes.zip(process.write_bytes).map_or_else(
                        || "—".into(),
                        |(read, write)| format!("{}↓ {}↑", format_bytes(read), format_bytes(write)),
                    );
                    put_text(
                        b,
                        inner.x + 1,
                        y,
                        7,
                        &process.pid.to_string(),
                        base.fg(p.text),
                    );
                    put_text(b, inner.x + 9, y, 20, &process.name, base.fg(p.text));
                    put_text(b, inner.x + 30, y, 9, &cpu, base.fg(p.text));
                    put_text(b, inner.x + 40, y, 11, &memory, base.fg(p.text));
                    put_text(b, inner.x + 52, y, 8, &threads, base.fg(p.text));
                    put_text(
                        b,
                        inner.x + 61,
                        y,
                        inner.width.saturating_sub(62),
                        &io,
                        base.fg(p.overlay1),
                    );
                    y += 1;
                }
                let hidden = telemetry.processes.len().saturating_sub(available_rows);
                if telemetry.truncated || hidden > 0 {
                    put_text(
                        b,
                        inner.x + 1,
                        inner.bottom().saturating_sub(2),
                        inner.width.saturating_sub(2),
                        "Process list is bounded; additional processes are not shown.",
                        muted,
                    );
                }
            }
            crate::api::schema::PaneTelemetryAvailability::Unavailable
            | crate::api::schema::PaneTelemetryAvailability::Unsupported => {
                put_text(
                    b,
                    inner.x + 1,
                    y,
                    inner.width.saturating_sub(2),
                    telemetry
                        .detail
                        .as_deref()
                        .unwrap_or("Process telemetry unavailable"),
                    muted,
                );
            }
        }
    }
    let footer_y = inner.bottom().saturating_sub(1);
    let footer = Rect::new(inner.x, footer_y, inner.width, 1);
    b.set_style(footer, base.bg(p.surface0));
    let refresh = if inner.width >= 20 {
        Rect::new(inner.right().saturating_sub(11), footer_y, 10, 1)
    } else {
        Rect::default()
    };
    put_text(
        b,
        inner.x + 1,
        footer_y,
        inner
            .width
            .saturating_sub(if refresh.is_empty() { 2 } else { 13 }),
        "argv/env never collected · on demand",
        muted.bg(p.surface0),
    );
    if !refresh.is_empty() {
        button(b, refresh, " r refresh ", muted.bg(p.surface0));
    }
    Some(OverlayRender {
        primary: refresh,
        cancel: close,
        ..OverlayRender::default()
    })
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if bytes >= GIB {
        format!("{:.1}G", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1}M", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1}K", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes}B")
    }
}

fn render_onboarding_overlay(b: &mut Buffer, p: &Palette) -> Option<OverlayRender> {
    let outer = popup(b.area, 64, 16)?;
    let inner = panel(b, outer, p.accent, p.panel_bg)?;
    if inner.height < 11 {
        return Some(OverlayRender {
            area: outer,
            ..OverlayRender::default()
        });
    }
    let stack = crate::ui::modal_stack_areas(inner, 2, 0, 1, 1);
    let base = Style::default()
        .bg(p.panel_bg)
        .remove_modifier(Modifier::DIM);
    let title = base.fg(p.text).add_modifier(Modifier::BOLD);
    let muted = base.fg(p.overlay0);
    let text = base.fg(p.overlay1);
    let accent = base.fg(p.accent).add_modifier(Modifier::BOLD);

    put_text(
        b,
        stack.header.x,
        stack.header.y,
        stack.header.width,
        crate::ui::ONBOARDING_TITLE,
        title,
    );
    put_text(
        b,
        stack.header.x,
        stack.header.y.saturating_add(1),
        stack.header.width,
        crate::ui::ONBOARDING_SUBTITLE,
        muted,
    );

    let content = stack.content;
    for (offset, line) in crate::ui::ONBOARDING_DESCRIPTION.iter().enumerate() {
        put_text(
            b,
            content.x,
            content.y.saturating_add(offset as u16),
            content.width,
            line,
            text,
        );
    }

    let key_y = content.y.saturating_add(4);
    let mut key_x = content.x;
    for (value, style) in [
        ("  ", base),
        (crate::ui::ONBOARDING_PREFIX_LABEL, accent),
        (crate::ui::ONBOARDING_PREFIX_SUFFIX, text),
        (crate::ui::ONBOARDING_HELP_LABEL, accent),
        (crate::ui::ONBOARDING_HELP_SUFFIX, text),
    ] {
        let width = display_width(value);
        put_text(
            b,
            key_x,
            key_y,
            content.right().saturating_sub(key_x),
            value,
            style,
        );
        key_x = key_x.saturating_add(width);
    }
    put_text(
        b,
        content.x,
        content.y.saturating_add(5),
        content.width,
        crate::ui::ONBOARDING_NEXT,
        text,
    );

    let primary = crate::ui::onboarding_welcome_continue_rect(stack.actions.unwrap_or_default());
    button(
        b,
        primary,
        " ↵ continue ",
        Style::default()
            .fg(contrast(p))
            .bg(p.accent)
            .add_modifier(Modifier::BOLD)
            .remove_modifier(Modifier::DIM),
    );
    Some(OverlayRender {
        area: outer,
        primary,
        ..OverlayRender::default()
    })
}

fn render_rename_overlay(
    b: &mut Buffer,
    v: &ClientRenameOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    let q = popup(b.area, 56, 7)?;
    let i = panel(b, q, p.accent, p.panel_bg)?;
    put_text(
        b,
        i.x,
        i.y,
        i.width,
        v.title,
        Style::default()
            .fg(p.text)
            .bg(p.panel_bg)
            .add_modifier(Modifier::BOLD),
    );
    let input = Rect::new(i.x, i.y + 2, i.width, 1);
    b.set_style(input, Style::default().fg(p.text).bg(p.surface0));
    put_text(
        b,
        input.x,
        input.y,
        input.width.saturating_sub(1),
        &format!(" {}", v.input),
        Style::default().fg(p.text).bg(p.surface0),
    );
    let rs = row(i, &[8, 10, 12], 2, 3);
    let [save, clear, cancel] = rs.as_slice() else {
        return None;
    };
    button(
        b,
        *save,
        " ↵ save ",
        Style::default()
            .fg(contrast(p))
            .bg(p.accent)
            .add_modifier(Modifier::BOLD),
    );
    let n = Style::default()
        .fg(p.text)
        .bg(p.surface0)
        .add_modifier(Modifier::BOLD);
    button(b, *clear, " ^c clear ", n);
    button(b, *cancel, " esc cancel ", n);
    Some(OverlayRender {
        area: q,
        primary: *save,
        clear: *clear,
        cancel: *cancel,
        navigator_popup: Rect::default(),
        navigator_search: Rect::default(),
        navigator_rows: Vec::new(),
        worktree_search: Rect::default(),
        worktree_rows: Vec::new(),
        cursor: Some(crate::protocol::CursorState {
            x: (input.x + 1 + display_width(&v.input)).min(input.right() - 1),
            y: input.y,
            visible: true,
            shape: 0,
        }),
        ..OverlayRender::default()
    })
}

/// Mark following siblings in preorder without rescanning descendants for each row.
/// The reverse stack contains at most one entry per depth; every entry is pushed
/// and popped at most once, so this pass is linear in the number of rows.
fn navigator_following_siblings(rows: &[ClientNavigatorRow]) -> Vec<bool> {
    let mut following = vec![false; rows.len()];
    let mut depths = Vec::new();
    for (index, row) in rows.iter().enumerate().rev() {
        while depths.last().is_some_and(|depth| *depth > row.depth) {
            depths.pop();
        }
        following[index] = depths.last() == Some(&row.depth);
        if !following[index] {
            depths.push(row.depth);
        }
    }
    following
}

fn render_navigator_overlay(
    b: &mut Buffer,
    n: &ClientNavigatorOverlay,
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    p: &Palette,
) -> Option<OverlayRender> {
    let a = b.area;
    let mx = (a.width / 16).max(2);
    let my = (a.height / 10).max(1);
    let q = Rect::new(
        a.x + mx,
        a.y + my,
        a.width.saturating_sub(mx * 2).max(4),
        a.height.saturating_sub(my * 2).max(4),
    );
    let i = panel(b, q, p.accent, p.panel_bg)?;
    let rows = super::aggregate_navigation::navigator_rows(endpoints, active_endpoint_id, n);
    let search = if n.search_focused {
        format!(" / {}", n.query)
    } else if let Some(f) = n.filter {
        format!(
            " / {}",
            match f {
                ClientNavigatorFilter::Blocked => "blocked",
                ClientNavigatorFilter::Working => "working",
                ClientNavigatorFilter::Idle => "idle",
                ClientNavigatorFilter::Done => "done",
            }
        )
    } else if n.query.is_empty() {
        " / search panes".to_owned()
    } else {
        format!(" / {}", n.query)
    };
    put_text(
        b,
        i.x,
        i.y,
        i.width,
        &search,
        Style::default()
            .fg(if n.search_focused { p.text } else { p.overlay0 })
            .bg(p.panel_bg),
    );
    put_right_text(
        b,
        i,
        i.y,
        &format!(
            "{} panes",
            rows.iter()
                .filter(|row| matches!(row.target, ClientNavigatorTarget::Pane { .. }))
                .count()
        ),
        Style::default().fg(p.overlay0).bg(p.panel_bg),
    );
    put_text(
        b,
        i.x,
        i.y + 1,
        i.width,
        &"─".repeat(i.width as usize),
        Style::default().fg(p.surface1).bg(p.panel_bg),
    );
    let body = Rect::new(i.x, i.y + 2, i.width, i.height.saturating_sub(4));
    let selected = super::aggregate_navigation::navigator_selected_index(&rows, n).unwrap_or(0);
    let max = rows.len().saturating_sub(body.height as usize);
    let scroll = n
        .scroll
        .max(selected.saturating_sub(body.height.saturating_sub(1) as usize))
        .min(selected)
        .min(max);
    let following_siblings = navigator_following_siblings(&rows);
    let mut ancestor_siblings = Vec::new();
    let federated = endpoints.len() > 1;
    let mut row_hits = Vec::new();
    for (ix, r) in rows.iter().enumerate().take(scroll + body.height as usize) {
        ancestor_siblings.truncate(usize::from(r.depth));
        ancestor_siblings.push(following_siblings[ix]);
        if ix < scroll {
            continue;
        }
        let rect = Rect::new(body.x, body.y + (ix - scroll) as u16, body.width, 1);
        row_hits.push((rect, r.target.clone()));
        let st = if r.stale {
            Style::default()
                .fg(p.overlay0)
                .bg(if ix == selected {
                    p.surface0
                } else {
                    p.panel_bg
                })
                .add_modifier(Modifier::DIM)
        } else if ix == selected {
            Style::default()
                .fg(contrast(p))
                .bg(p.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(if r.current { p.text } else { p.subtext0 })
                .bg(p.panel_bg)
        };
        b.set_style(rect, st);
        let tree = match &r.target {
            ClientNavigatorTarget::Machine { .. } => "▾".to_owned(),
            ClientNavigatorTarget::Workspace {
                endpoint_id,
                workspace_id,
            } if n
                .expanded_workspaces
                .contains(&(endpoint_id.clone(), workspace_id.clone())) =>
            {
                if r.depth == 0 { "▾" } else { "  ▾" }.to_owned()
            }
            ClientNavigatorTarget::Workspace { .. } => {
                if r.depth == 0 { "▸" } else { "  ▸" }.to_owned()
            }
            ClientNavigatorTarget::Tab { .. } | ClientNavigatorTarget::Pane { .. } => {
                // Machines and workspaces keep their existing caret decoration.
                // Connected branches begin below each workspace.
                let mut prefix = if federated { "    " } else { "" }.to_owned();
                for &following in ancestor_siblings
                    .iter()
                    .take(usize::from(r.depth))
                    .skip(if federated { 2 } else { 1 })
                {
                    prefix.push_str(if following { "│  " } else { "   " });
                }
                prefix.push_str(if following_siblings[ix] {
                    "├──"
                } else {
                    "└──"
                });
                prefix
            }
        };
        let current = if r.current { "◆ " } else { "" };
        let status = r.status.map(status_dot).unwrap_or_default();
        let status_separator = if status.is_empty() { "" } else { " " };
        let label = format!(" {tree} {current}{status}{status_separator}{}", r.label);
        put_text(b, rect.x, rect.y, rect.width, &label, st);
        if let Some(status) = r.status {
            let prefix = format!(" {tree} {current}");
            let status_style = if r.stale || ix == selected {
                st
            } else {
                Style::default().fg(status_color(status, p)).bg(p.panel_bg)
            };
            put_text(
                b,
                rect.x.saturating_add(display_width(&prefix)),
                rect.y,
                display_width(status_dot(status)),
                status_dot(status),
                status_style,
            );
        }
        let machine_status = match &r.target {
            ClientNavigatorTarget::Machine { endpoint_id } if !endpoint_id.is_local() => endpoints
                .iter()
                .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
                .map(|endpoint| endpoint.status),
            _ => None,
        };
        if let Some(status) = machine_status {
            let (glyph, state, color) = endpoint_status_presentation(status, p);
            let signal = if status == ClientEndpointStatus::Online {
                glyph.to_owned()
            } else {
                format!("{glyph} {state}")
            };
            let signal_style = if ix == selected {
                st
            } else {
                Style::default()
                    .fg(color)
                    .bg(p.panel_bg)
                    .add_modifier(if r.stale {
                        Modifier::DIM
                    } else {
                        Modifier::empty()
                    })
            };
            put_right_text(b, rect, rect.y, &signal, signal_style);
        } else if !r.meta.is_empty() {
            let label_width = display_width(&label).min(rect.width);
            let meta = Rect::new(
                rect.x.saturating_add(label_width).saturating_add(1),
                rect.y,
                rect.width.saturating_sub(label_width.saturating_add(1)),
                1,
            );
            put_right_text(b, meta, rect.y, &r.meta, st)
        }
    }
    let dy = i.bottom() - 2;
    if let Some(r) = rows.get(selected) {
        put_text(
            b,
            i.x,
            dy,
            i.width,
            &format!(" {} · {}", r.label, r.meta),
            Style::default().fg(p.overlay0).bg(p.panel_bg),
        )
    }
    put_text(
        b,
        i.x,
        i.bottom() - 1,
        i.width,
        if n.search_focused {
            " search type · move ↑↓/ctrl+n/p · open enter · back esc"
        } else {
            " move j/k · expand space · filter a/b/w/i/d · search / · open enter · close esc"
        },
        Style::default().fg(p.overlay0).bg(p.panel_bg),
    );
    Some(OverlayRender {
        area: q,
        primary: Rect::default(),
        clear: Rect::default(),
        cancel: Rect::default(),
        navigator_popup: q,
        navigator_search: Rect::new(i.x, i.y, i.width, 1),
        navigator_rows: row_hits,
        worktree_search: Rect::default(),
        worktree_rows: Vec::new(),
        cursor: n.search_focused.then(|| crate::protocol::CursorState {
            x: i.x + 3 + display_width(&n.query),
            y: i.y,
            visible: true,
            shape: 0,
        }),
        ..OverlayRender::default()
    })
}

fn help_lines(
    keybinds: &LiveKeybindConfig,
    query: &str,
    palette: &Palette,
) -> Vec<(usize, ratatui::text::Line<'static>)> {
    use ratatui::text::{Line, Span};

    let groups = crate::input::filter_keybind_help_groups(
        crate::input::keybind_help_groups(&keybinds.keybinds, keybinds.prefix),
        query,
    );
    let key_width = groups
        .iter()
        .flat_map(|(_, entries)| entries.iter().map(|(key, _)| key.chars().count()))
        .max()
        .unwrap_or(8);
    if groups.is_empty() {
        let message = " no matching keybinds";
        return vec![(
            message.chars().count(),
            Line::from(Span::styled(
                message,
                Style::default().fg(palette.overlay1).bg(palette.panel_bg),
            )),
        )];
    }

    let mut lines = Vec::new();
    for (group, entries) in groups {
        lines.push((
            group.len() + 1,
            Line::from(Span::styled(
                format!(" {group}"),
                Style::default()
                    .fg(palette.accent)
                    .bg(palette.panel_bg)
                    .add_modifier(Modifier::BOLD),
            )),
        ));
        for (key, label) in entries {
            let padded_key = format!(" {key:<key_width$} ");
            let width = padded_key.chars().count() + label.chars().count();
            lines.push((
                width,
                Line::from(vec![
                    Span::styled(
                        padded_key,
                        Style::default()
                            .fg(palette.mauve)
                            .bg(palette.panel_bg)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        label.into_owned(),
                        Style::default().fg(palette.text).bg(palette.panel_bg),
                    ),
                ]),
            ));
        }
        lines.push((0, Line::raw("")));
    }
    lines
}

fn render_help_overlay(
    b: &mut Buffer,
    h: &ClientHelpOverlay,
    k: &LiveKeybindConfig,
    p: &Palette,
) -> Option<OverlayRender> {
    use ratatui::widgets::{Paragraph, Widget, Wrap};

    let q = popup(b.area, 76, 22)?;
    let i = panel(b, q, p.accent, p.panel_bg)?;
    if i.width < 20 || i.height < 6 {
        return None;
    }
    put_text(
        b,
        i.x,
        i.y,
        i.width,
        "keybinds",
        Style::default()
            .fg(p.text)
            .bg(p.panel_bg)
            .add_modifier(Modifier::BOLD),
    );
    let close = Rect::new(i.right() - 13, i.y, 13, 1);
    button(
        b,
        close,
        if h.search_focused {
            " esc back "
        } else {
            " esc close "
        },
        Style::default()
            .fg(contrast(p))
            .bg(p.accent)
            .add_modifier(Modifier::BOLD),
    );
    let sy = i.y + 1;
    put_text(
        b,
        i.x,
        sy,
        i.width,
        &if h.search_focused {
            format!(" / {}", h.query)
        } else {
            " / press / to filter by command or shortcut".to_owned()
        },
        Style::default()
            .fg(if h.search_focused { p.text } else { p.overlay0 })
            .bg(p.panel_bg),
    );

    let body = Rect::new(i.x, i.y + 3, i.width, i.height.saturating_sub(5));
    let lines = help_lines(k, &h.query, p);
    let viewport_rows = usize::from(body.height.max(1));
    let wrapped_rows = |width: u16| {
        let width = usize::from(width.max(1));
        lines
            .iter()
            .map(|(line_width, _)| line_width.max(&1).div_ceil(width))
            .sum::<usize>()
    };
    let needs_scrollbar = wrapped_rows(body.width) > viewport_rows;
    let text_area = if needs_scrollbar {
        Rect::new(body.x, body.y, body.width.saturating_sub(1), body.height)
    } else {
        body
    };
    let total_rows = wrapped_rows(text_area.width);
    let max_scroll = total_rows.saturating_sub(viewport_rows);
    let scroll = h.scroll.min(max_scroll);
    let metrics = crate::pane::ScrollMetrics {
        offset_from_bottom: max_scroll.saturating_sub(scroll),
        max_offset_from_bottom: max_scroll,
        viewport_rows,
    };
    let scrollbar = needs_scrollbar.then_some(Rect::new(
        body.right().saturating_sub(1),
        body.y,
        1,
        body.height,
    ));
    Widget::render(
        Paragraph::new(lines.into_iter().map(|(_, line)| line).collect::<Vec<_>>())
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        text_area,
        b,
    );
    if let Some(track) = scrollbar {
        if let Some(thumb) = crate::ui::scrollbar_thumb(metrics, track) {
            for y in track.y..track.bottom() {
                b[(track.x, y)]
                    .set_symbol("▐")
                    .set_style(Style::default().fg(p.overlay0).bg(p.panel_bg));
            }
            for y in thumb.top..thumb.top.saturating_add(thumb.len) {
                b[(track.x, y)]
                    .set_symbol("▐")
                    .set_style(Style::default().fg(p.overlay1).bg(p.panel_bg));
            }
        }
    }

    put_text(
        b,
        i.x,
        i.bottom() - 1,
        i.width,
        if h.search_focused {
            " filter type/backspace · clear ctrl+u · scroll ↑↓/pgup/pgdn · back esc"
        } else {
            " search / · scroll j/k/↑↓/pgup/pgdn · close esc/enter"
        },
        Style::default().fg(p.overlay0).bg(p.panel_bg),
    );
    Some(OverlayRender {
        area: q,
        cancel: close,
        help_popup: q,
        help_scrollbar: scrollbar.unwrap_or_default(),
        help_scroll_metrics: Some(metrics),
        help_max_scroll: max_scroll,
        cursor: h.search_focused.then(|| crate::protocol::CursorState {
            x: (i.x + 3 + display_width(&h.query)).min(i.right() - 1),
            y: sy,
            visible: true,
            shape: 0,
        }),
        ..OverlayRender::default()
    })
}
fn render_confirm_close_overlay(
    b: &mut Buffer,
    c: &ClientConfirmCloseOverlay,
    p: &Palette,
) -> Option<OverlayRender> {
    let q = popup(b.area, 64, 6)?;
    let i = panel(b, q, p.red, p.panel_bg)?;
    put_text(
        b,
        i.x,
        i.y,
        i.width,
        &format!(" {}", c.title),
        Style::default()
            .fg(p.red)
            .bg(p.panel_bg)
            .add_modifier(Modifier::BOLD),
    );
    put_text(
        b,
        i.x,
        i.y + 1,
        i.width,
        &format!(" {}", c.detail),
        Style::default().fg(p.text).bg(p.panel_bg),
    );
    let rs = row(i, &[13, 12], 2, 3);
    let [ok, cancel] = rs.as_slice() else {
        return None;
    };
    button(
        b,
        *ok,
        " ↵ confirm ",
        Style::default()
            .fg(contrast(p))
            .bg(p.red)
            .add_modifier(Modifier::BOLD),
    );
    button(
        b,
        *cancel,
        " esc cancel ",
        Style::default()
            .fg(p.text)
            .bg(p.surface0)
            .add_modifier(Modifier::BOLD),
    );
    Some(OverlayRender {
        area: q,
        primary: *ok,
        clear: Rect::default(),
        cancel: *cancel,
        navigator_popup: Rect::default(),
        navigator_search: Rect::default(),
        navigator_rows: Vec::new(),
        worktree_search: Rect::default(),
        worktree_rows: Vec::new(),
        cursor: None,
        ..OverlayRender::default()
    })
}
