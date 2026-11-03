use super::*;

fn choice_style(selected: bool, palette: &Palette) -> Style {
    if selected {
        Style::default()
            .fg(contrast(palette))
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette.text).bg(palette.panel_bg)
    }
}

fn draw_choice(
    buffer: &mut Buffer,
    rect: Rect,
    label: &str,
    selected: bool,
    current: bool,
    palette: &Palette,
) {
    let style = choice_style(selected, palette);
    buffer.set_style(rect, style);
    let marker = if selected { "▸" } else { " " };
    let current = if current { " ✓" } else { "" };
    put_text(
        buffer,
        rect.x,
        rect.y,
        rect.width,
        &format!(" {marker} {label}{current}"),
        style,
    );
}

pub(super) fn render_settings_overlay(
    buffer: &mut Buffer,
    settings: &ClientSettingsOverlay,
    palette: &Palette,
) -> Option<OverlayRender> {
    let popup = popup(buffer.area, 76, 22)?;
    let inner = panel(buffer, popup, palette.accent, palette.panel_bg)?;
    if inner.width < 20 || inner.height < 8 {
        return None;
    }

    put_text(
        buffer,
        inner.x,
        inner.y,
        inner.width,
        " settings",
        Style::default()
            .fg(palette.text)
            .bg(palette.panel_bg)
            .add_modifier(Modifier::BOLD),
    );

    let mut tab_x = inner.x;
    let mut tab_hits = Vec::new();
    for section in ClientSettingsSection::ALL {
        let label = format!(" {} ", section.label());
        let width = display_width(&label).min(inner.right().saturating_sub(tab_x));
        let rect = Rect::new(tab_x, inner.y + 1, width, 1);
        let active = *section == settings.section;
        let style = if active {
            Style::default()
                .fg(contrast(palette))
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.overlay1).bg(palette.panel_bg)
        };
        buffer.set_style(rect, style);
        put_text(buffer, rect.x, rect.y, rect.width, &label, style);
        tab_hits.push((rect, *section));
        tab_x = tab_x.saturating_add(width.saturating_add(1));
        if tab_x >= inner.right() {
            break;
        }
    }
    put_text(
        buffer,
        inner.x,
        inner.y + 2,
        inner.width,
        &"─".repeat(inner.width as usize),
        Style::default().fg(palette.surface0).bg(palette.panel_bg),
    );

    let content = Rect::new(
        inner.x,
        inner.y + 4,
        inner.width,
        inner.height.saturating_sub(7),
    );
    let mut choice_hits = Vec::new();
    let mut python_path_visible = false;
    match settings.section {
        ClientSettingsSection::Tasks => {
            python_path_visible = render_tasks_settings(buffer, content, &settings.tasks, palette);
        }
        ClientSettingsSection::Theme => {
            let visible = usize::from(content.height);
            let scroll = settings.selected.saturating_sub(visible.saturating_sub(1));
            for (visible_index, (index, name)) in crate::config::THEME_NAMES
                .iter()
                .enumerate()
                .skip(scroll)
                .take(visible)
                .enumerate()
            {
                let rect = Rect::new(
                    content.x,
                    content.y + visible_index as u16,
                    content.width,
                    1,
                );
                draw_choice(
                    buffer,
                    rect,
                    name,
                    index == settings.selected,
                    super::super::settings::normalized_theme_name(name)
                        == super::super::settings::normalized_theme_name(
                            &settings.original_theme_name,
                        ),
                    palette,
                );
                choice_hits.push((rect, index));
            }
        }
        ClientSettingsSection::Indicators => {
            render_choice_section(
                buffer,
                content,
                "agent status indicators",
                "choose color dots or distinct symbols for each state",
                &["color dots  ● ● ● ○ ·", "distinct symbols  × ◐ ✓ ○ ·"],
                settings.selected,
                palette,
                &mut choice_hits,
            );
        }
        ClientSettingsSection::Sound => {
            render_choice_section(
                buffer,
                content,
                "sound alerts",
                "play sounds when agents change state in background",
                &["on", "off"],
                settings.selected,
                palette,
                &mut choice_hits,
            );
        }
        ClientSettingsSection::Toast => {
            render_choice_section(
                buffer,
                content,
                "notification popups",
                "choose where background popup notifications should appear",
                &["off", "inside herdr", "via terminal", "via system"],
                settings.selected,
                palette,
                &mut choice_hits,
            );
        }
    }

    let tasks_tab = settings.section == ClientSettingsSection::Tasks;
    let mutating = settings.tasks.pending && settings.tasks.confirming;
    let buttons = row(
        inner,
        &[if tasks_tab { 16 } else { 10 }, 12],
        2,
        inner.height.saturating_sub(1),
    );
    let primary = if tasks_tab
        && (settings.tasks.pending || (settings.tasks.confirming && !python_path_visible))
    {
        Rect::default()
    } else {
        buttons[0]
    };
    let close = if mutating {
        Rect::default()
    } else {
        buttons[1]
    };
    let primary_label = if !tasks_tab {
        " ↵ apply "
    } else if settings.tasks.confirming {
        " ↵ confirm "
    } else if settings
        .tasks
        .interpreter
        .as_ref()
        .is_some_and(|status| status.confirmed)
    {
        " ↵ refresh "
    } else if settings
        .tasks
        .interpreter
        .as_ref()
        .is_some_and(|status| status.path.is_some())
    {
        " ↵ review path "
    } else {
        " ↵ retry "
    };
    button(
        buffer,
        primary,
        primary_label,
        Style::default()
            .fg(contrast(palette))
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    button(
        buffer,
        close,
        if tasks_tab && settings.tasks.confirming {
            " esc cancel "
        } else {
            " esc close "
        },
        Style::default()
            .fg(palette.text)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
    put_text(
        buffer,
        inner.x,
        inner.bottom().saturating_sub(2),
        inner.width,
        if mutating {
            " Confirming path with host…"
        } else if tasks_tab && settings.tasks.confirming && !python_path_visible {
            " Enlarge terminal to display the full path before approval."
        } else if tasks_tab {
            " tab section · setup applies only to Tasks"
        } else {
            " ↑↓ select  tab section"
        },
        Style::default().fg(palette.overlay1).bg(palette.panel_bg),
    );

    Some(OverlayRender {
        area: popup,
        primary,
        cancel: close,
        settings_popup: popup,
        settings_tabs: tab_hits,
        settings_choices: choice_hits,
        ..OverlayRender::default()
    })
}

/// Host facts only; no registry/config/filesystem access during rendering.
fn render_tasks_settings(
    buffer: &mut Buffer,
    area: Rect,
    tasks: &ClientTasksSettings,
    palette: &Palette,
) -> bool {
    let mut lines = vec!["Tasks / Python setup".to_owned(), String::new()];
    let path = if tasks.confirming {
        tasks.confirmation_path.as_deref()
    } else {
        tasks
            .interpreter
            .as_ref()
            .and_then(|status| status.path.as_deref())
    };
    let mut path_visible = false;
    if tasks.pending && !tasks.confirming {
        lines.extend(wrap_literal(
            "Loading interpreter status from host…",
            area.width,
        ));
    } else {
        lines.extend(wrap_literal(
            if tasks.confirming {
                "Confirm this exact absolute executable:"
            } else {
                "Windows Python executable:"
            },
            area.width,
        ));
        if let Some(path) = path {
            let path_lines = wrap_literal(path, area.width);
            path_visible = lines.len() + path_lines.len() <= usize::from(area.height)
                && path_lines
                    .iter()
                    .all(|line| display_width(line) <= area.width);
            lines.extend(path_lines);
        } else {
            lines.extend(wrap_literal("No usable interpreter selected.", area.width));
        }
        lines.push(String::new());
        if let Some(status) = tasks.interpreter.as_ref() {
            lines.extend(wrap_literal(&status.message, area.width));
        } else if tasks.loaded && tasks.message.is_none() {
            lines.extend(wrap_literal("This host did not provide Windows Python setup status. Non-Windows Tasks uses python3; update an older Windows host if needed.", area.width));
        }
    }
    if let Some(message) = tasks.message.as_deref() {
        lines.extend(wrap_literal(message, area.width));
    }
    for (line, y) in lines.into_iter().zip(area.y..area.bottom()) {
        put_text(
            buffer,
            area.x,
            y,
            area.width,
            &line,
            Style::default().fg(palette.text).bg(palette.panel_bg),
        );
    }
    path_visible
}

/// Preserve every path character and space; never collapse or truncate it into
/// a different executable. An overlong path disables the confirmation action.
fn wrap_literal(text: &str, width: u16) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut columns = 0;
    for character in text.chars() {
        if character == '\n' {
            lines.push(std::mem::take(&mut line));
            columns = 0;
            continue;
        }
        let character_width = character.width().unwrap_or(0);
        if columns + character_width > usize::from(width) && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            columns = 0;
        }
        line.push(character);
        columns += character_width;
    }
    lines.push(line);
    lines
}

fn render_choice_section(
    buffer: &mut Buffer,
    area: Rect,
    title: &str,
    description: &str,
    choices: &[&str],
    selected: usize,
    palette: &Palette,
    hits: &mut Vec<(Rect, usize)>,
) {
    put_text(
        buffer,
        area.x,
        area.y,
        area.width,
        title,
        Style::default()
            .fg(palette.text)
            .bg(palette.panel_bg)
            .add_modifier(Modifier::BOLD),
    );
    put_text(
        buffer,
        area.x,
        area.y + 1,
        area.width,
        description,
        Style::default().fg(palette.overlay1).bg(palette.panel_bg),
    );
    let row_gap = u16::from(choices.len() > 2);
    for (index, choice) in choices.iter().enumerate() {
        let y = area.y + 3 + index as u16 * (1 + row_gap);
        if y >= area.bottom() {
            break;
        }
        let rect = Rect::new(area.x, y, area.width, 1);
        draw_choice(buffer, rect, choice, index == selected, false, palette);
        hits.push((rect, index));
    }
}
