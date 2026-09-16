use super::*;

pub(super) fn render_extensions_overlay(
    buffer: &mut Buffer,
    extensions: &ClientExtensionsOverlay,
    palette: &Palette,
) -> Option<OverlayRender> {
    let outer = popup(buffer.area, 92, 30)?;
    let inner = panel(buffer, outer, palette.accent, palette.panel_bg)?;
    let base = Style::default()
        .fg(palette.text)
        .bg(palette.panel_bg)
        .remove_modifier(Modifier::DIM);
    let muted = base.fg(palette.overlay0);
    let selected_style = base
        .fg(contrast(palette))
        .bg(palette.accent)
        .add_modifier(Modifier::BOLD);

    put_text(
        buffer,
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(13),
        "Extensions",
        base.add_modifier(Modifier::BOLD),
    );
    let close = Rect::new(inner.right().saturating_sub(11), inner.y, 10, 1);
    button(buffer, close, " esc close ", muted.bg(palette.surface0));

    if inner.width < 72 || inner.height < 14 {
        put_text(
            buffer,
            inner.x + 1,
            inner.y + 2,
            inner.width.saturating_sub(2),
            "Make the terminal larger to manage extensions.",
            muted,
        );
        return Some(OverlayRender {
            area: outer,
            cancel: close,
            extensions_popup: outer,
            ..OverlayRender::default()
        });
    }

    let browse = Rect::new(inner.x + 1, inner.y + 2, 10, 1);
    let installed = Rect::new(inner.x + 12, inner.y + 2, 12, 1);
    for (rect, label, active) in [
        (
            browse,
            " Browse ",
            extensions.tab == ClientExtensionsTab::Browse,
        ),
        (
            installed,
            " Installed ",
            extensions.tab == ClientExtensionsTab::Installed,
        ),
    ] {
        button(
            buffer,
            rect,
            label,
            if active {
                selected_style
            } else {
                muted.bg(palette.surface0)
            },
        );
    }

    let search = Rect::new(inner.x + 26, inner.y + 2, inner.width.saturating_sub(27), 1);
    buffer.set_style(search, base.bg(palette.surface0));
    let search_text = if extensions.query.is_empty() && !extensions.search_focused {
        " / search".to_string()
    } else {
        format!(" / {}", extensions.query)
    };
    put_text(
        buffer,
        search.x,
        search.y,
        search.width,
        &search_text,
        if extensions.search_focused {
            base.bg(palette.surface0)
        } else {
            muted.bg(palette.surface0)
        },
    );

    let category_y = inner.y + 4;
    let mut category_x = inner.x + 1;
    let mut category_hits = Vec::new();
    let categories = [None, Some("Agents"), Some("Workflow"), Some("Interface")];
    for category in categories {
        let label = category.unwrap_or("All");
        let width = u16::try_from(label.len())
            .unwrap_or(u16::MAX)
            .saturating_add(2);
        let rect = Rect::new(category_x, category_y, width, 1);
        let active = extensions.category.as_deref() == category;
        button(
            buffer,
            rect,
            &format!(" {label} "),
            if active {
                selected_style
            } else {
                muted.bg(palette.surface0)
            },
        );
        category_hits.push((rect, category.map(str::to_string)));
        category_x = category_x.saturating_add(width + 1);
    }

    let body_y = inner.y + 6;
    let footer_y = inner.bottom().saturating_sub(2);
    let body_height = footer_y.saturating_sub(body_y);
    let list_width = (inner.width / 2).clamp(24, 38);
    let list = Rect::new(inner.x, body_y, list_width, body_height);
    let details = Rect::new(
        inner.x.saturating_add(list_width + 1),
        body_y,
        inner.width.saturating_sub(list_width + 1),
        body_height,
    );
    if list.right() < inner.right() {
        for y in body_y..footer_y {
            buffer[(list.right(), y)]
                .set_symbol("│")
                .set_style(base.fg(palette.surface1));
        }
    }

    let filtered = extensions.filtered_indices();
    let selected = extensions.selected.min(filtered.len().saturating_sub(1));
    let scroll = selected.saturating_sub(body_height.saturating_sub(1) as usize);
    let mut row_hits = Vec::new();
    for (visible, extension_index) in filtered
        .iter()
        .skip(scroll)
        .take(body_height as usize)
        .enumerate()
    {
        let extension = &extensions.extensions[*extension_index];
        let rect = Rect::new(list.x, list.y + visible as u16, list.width, 1);
        let active = scroll + visible == selected;
        let style = if active { selected_style } else { base };
        buffer.set_style(rect, style);
        let state = if extension.installed {
            if extension.enabled {
                "on"
            } else {
                "off"
            }
        } else {
            "get"
        };
        put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width.saturating_sub(6),
            &format!(" {}", extension.name),
            style,
        );
        put_right_text(
            buffer,
            rect,
            rect.y,
            state,
            if active { style } else { muted },
        );
        row_hits.push((rect, *extension_index));
    }
    if extensions.loading {
        put_text(
            buffer,
            list.x + 1,
            list.y,
            list.width.saturating_sub(2),
            "Loading…",
            muted,
        );
    } else if filtered.is_empty() {
        put_text(
            buffer,
            list.x + 1,
            list.y,
            list.width.saturating_sub(2),
            "No matching extensions.",
            muted,
        );
    }

    if let Some(index) = filtered.get(selected) {
        let extension = &extensions.extensions[*index];
        let mut y = details.y;
        put_text(
            buffer,
            details.x + 1,
            y,
            details.width.saturating_sub(2),
            &extension.name,
            base.add_modifier(Modifier::BOLD),
        );
        y += 1;
        put_text(
            buffer,
            details.x + 1,
            y,
            details.width.saturating_sub(2),
            &if extension.installed {
                format!(
                    "{} · v{} · load {} (later wins)",
                    extension.package_kind,
                    extension.active_version,
                    extension.load_order.saturating_add(1)
                )
            } else {
                format!(
                    "{} · v{} · not installed",
                    extension.package_kind, extension.available_version
                )
            },
            muted,
        );
        y += 1;
        if extension.source_commit.is_some() {
            put_text(
                buffer,
                details.x + 1,
                y,
                details.width.saturating_sub(2),
                if extension.package_kind == "bundled" {
                    "source pin: catalog attestation · runtime: native host provider"
                } else if extension.artifact_verified {
                    "source artifact: retained (press u to reverify)"
                } else {
                    "source artifact: not retained (press u to verify)"
                },
                if extension.package_kind == "bundled" || extension.artifact_verified {
                    base.fg(palette.green)
                } else {
                    base.fg(palette.yellow)
                },
            );
            y += 1;
        }
        if let Some(status) = extension.integration_status.as_deref() {
            put_text(
                buffer,
                details.x + 1,
                y,
                details.width.saturating_sub(2),
                &format!("adapter: {}", status.replace('_', " ")),
                if status == "current" {
                    base.fg(palette.green)
                } else {
                    base.fg(palette.yellow)
                },
            );
            y += 1;
        }
        y += 1;
        for line in wrap_text(&extension.summary, details.width.saturating_sub(2)) {
            if y >= details.bottom() {
                break;
            }
            put_text(
                buffer,
                details.x + 1,
                y,
                details.width.saturating_sub(2),
                &line,
                base,
            );
            y += 1;
        }
        y += 1;
        for line in wrap_text(&extension.details, details.width.saturating_sub(2)) {
            if y >= details.bottom().saturating_sub(5) {
                break;
            }
            put_text(
                buffer,
                details.x + 1,
                y,
                details.width.saturating_sub(2),
                &line,
                muted,
            );
            y += 1;
        }
        if y < details.bottom().saturating_sub(2) {
            y += 1;
            put_text(
                buffer,
                details.x + 1,
                y,
                details.width.saturating_sub(2),
                &match extension.source_commit.as_deref() {
                    Some(commit) => format!("{} @ {}", extension.repository, commit),
                    None => extension.repository.clone(),
                },
                base.fg(palette.mauve),
            );
        }
        if extension.repository_override_supported {
            if let Some(repository_id) = extensions.repository_context.as_deref() {
                let override_enabled = extension
                    .repository_overrides
                    .iter()
                    .find(|override_info| override_info.repository_id == repository_id)
                    .map(|override_info| override_info.enabled);
                let effective = override_enabled.unwrap_or(extension.enabled);
                put_text(
                    buffer,
                    details.x + 1,
                    details.bottom().saturating_sub(3),
                    details.width.saturating_sub(2),
                    &format!(
                        "repository: {} · {} ({})",
                        repository_id,
                        if effective { "on" } else { "off" },
                        if override_enabled.is_some() {
                            "override"
                        } else {
                            "global"
                        }
                    ),
                    base.fg(palette.teal),
                );
            }
        }
        if let Some(error) = extension.activation_error.as_deref() {
            put_text(
                buffer,
                details.x + 1,
                details.bottom().saturating_sub(2),
                details.width.saturating_sub(2),
                &format!("activation failed: {error}"),
                base.fg(palette.red),
            );
        }
    }

    let footer = Rect::new(inner.x, footer_y, inner.width, 2);
    buffer.set_style(footer, base.bg(palette.surface0));
    let footer_text = if extensions.mutating {
        " Working…"
    } else if let Some(confirmation) = extensions.confirmation {
        match confirmation {
            ClientExtensionConfirmation::Install => " Install and enable this extension?",
            ClientExtensionConfirmation::Remove => {
                " Remove this extension? Config and state are retained."
            }
            ClientExtensionConfirmation::DeleteData => {
                " Permanently delete extension config and state?"
            }
            ClientExtensionConfirmation::Update => " Approve this extension update?",
            ClientExtensionConfirmation::Rollback => " Roll back to the previous version?",
        }
    } else if extensions
        .selected_extension()
        .is_some_and(|extension| !extension.installed && !extension.install_supported)
    {
        " Installation is unavailable for this curated entry."
    } else {
        " [ ] order · o repository · r remove · x data · u update · b rollback · c category"
    };
    let unsupported_install = extensions
        .selected_extension()
        .is_some_and(|extension| !extension.installed && !extension.install_supported);
    let action = if extensions.mutating || unsupported_install || footer.width < 18 {
        Rect::default()
    } else {
        Rect::new(footer.right().saturating_sub(17), footer.y, 17, 1)
    };
    put_text(
        buffer,
        footer.x,
        footer.y,
        action.x.saturating_sub(footer.x).min(footer.width),
        footer_text,
        muted.bg(palette.surface0),
    );
    if !action.is_empty() {
        let label = if extensions.confirmation.is_some() {
            " enter confirm "
        } else if extensions
            .selected_extension()
            .is_some_and(|extension| extension.installed)
        {
            " enter toggle "
        } else {
            " enter install "
        };
        button(buffer, action, label, selected_style);
    }
    if let Some(message) = extensions.message.as_deref() {
        put_text(
            buffer,
            footer.x,
            footer.y + 1,
            footer.width,
            message,
            base.fg(palette.yellow).bg(palette.surface0),
        );
    } else {
        put_text(
            buffer,
            footer.x,
            footer.y + 1,
            footer.width,
            " Extensions run as trusted same-user code; they are not sandboxed.",
            muted.bg(palette.surface0),
        );
    }

    Some(OverlayRender {
        area: outer,
        primary: action,
        cancel: close,
        extensions_popup: outer,
        extensions_tabs: vec![
            (browse, ClientExtensionsTab::Browse),
            (installed, ClientExtensionsTab::Installed),
        ],
        extensions_categories: category_hits,
        extensions_rows: row_hits,
        extensions_search: search,
        cursor: extensions
            .search_focused
            .then_some(crate::protocol::CursorState {
                x: search
                    .x
                    .saturating_add(3)
                    .saturating_add(display_width(&extensions.query))
                    .min(search.right().saturating_sub(1)),
                y: search.y,
                visible: true,
                shape: 0,
            }),
        ..OverlayRender::default()
    })
}

fn wrap_text(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len().saturating_add(word.len() + 1) > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}
