//! Scrollable bounded question/options, using the ordinary client modal chrome.
use super::*;
use crate::client::shell::render::overlays::{button, panel, popup, OverlayRender};

pub(super) struct Layout {
    pub body: Rect,
    pub lines: Vec<(String, Option<usize>)>,
    pub options: Vec<(Rect, usize)>,
    pub primary: Rect,
    pub cancel: Rect,
    pub max_scroll: usize,
    pub scroll: usize,
}
fn wrap(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    let logical_line = ratatui::text::Line::raw(text);
    for styled in logical_line.styled_graphemes(Style::default()) {
        let grapheme = if styled.symbol.chars().any(char::is_control) {
            " "
        } else {
            styled.symbol
        };
        let columns = usize::from(display_width(grapheme));
        if used + columns > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        line.push_str(grapheme);
        used += columns;
    }
    lines.push(line);
    lines
}
pub(super) fn layout(dialog: &Dialog, area: Rect) -> Layout {
    let inner = area.inner(ratatui::layout::Margin::new(2, 1));
    let body = Rect::new(
        inner.x,
        inner.y.saturating_add(1),
        inner.width,
        inner.height.saturating_sub(4),
    );
    let mut lines = Vec::new();
    if let Phase::Outcome(message) = &dialog.phase {
        lines.extend(
            wrap(message, body.width)
                .into_iter()
                .map(|line| (line, None)),
        );
        lines.push((String::new(), None));
    }
    if matches!(dialog.phase, Phase::Confirm(_)) {
        if let Affordance::Choice { options, .. } = &dialog.intent.affordance {
            if let Some(choice) = options.iter().find(|choice| {
                Some(choice.key.as_str()) == dialog.intent.selection.option.as_deref()
            }) {
                lines.extend(
                    wrap(
                        &format!("Chosen answer: [{}] {}", choice.key, choice.label),
                        body.width,
                    )
                    .into_iter()
                    .map(|line| (line, None)),
                );
                lines.push((String::new(), None));
            }
        }
    }
    let (label, detail) = match &dialog.intent.affordance {
        Affordance::Action { label, detail, .. } | Affordance::Choice { label, detail, .. } => {
            (label, detail)
        }
    };
    for text in [label.as_str(), detail.as_str()] {
        lines.extend(wrap(text, body.width).into_iter().map(|line| (line, None)));
    }
    lines.push((String::new(), None));
    if let Affordance::Choice {
        options,
        default,
        reversible,
        ..
    } = &dialog.intent.affordance
    {
        let policy = format!(
            "Default: {default} · {}",
            if *reversible {
                "reversible"
            } else {
                "irreversible; confirmation required"
            }
        );
        lines.extend(
            wrap(&policy, body.width)
                .into_iter()
                .map(|line| (line, None)),
        );
        lines.push((String::new(), None));
        for (index, choice) in options.iter().enumerate() {
            let text = format!(
                "[{}] {}{}",
                choice.key,
                choice.label,
                if &choice.key == default {
                    " (default)"
                } else {
                    ""
                }
            );
            lines.extend(
                wrap(&text, body.width)
                    .into_iter()
                    .map(|line| (line, Some(index))),
            );
            lines.push((String::new(), None));
        }
    }
    if let Some(option) = dialog.intent.selection.option.as_deref() {
        lines.extend(
            wrap(&format!("Chosen option: {option}"), body.width)
                .into_iter()
                .map(|line| (line, None)),
        );
    }
    let status = match dialog.phase {
        Phase::Choosing => "Choose a listed option; no default is applied automatically.",
        Phase::Preparing => "Checking the local client. Nothing submitted yet.",
        Phase::Confirm(_) => "Confirm this irreversible answer, or cancel.",
        Phase::Submitting => "Waiting for the original operation. Closing does not cancel it.",
        Phase::Outcome(_) => "Close when finished inspecting this original operation.",
    };
    lines.extend(
        wrap(status, body.width)
            .into_iter()
            .map(|line| (line, None)),
    );
    let max_scroll = lines.len().saturating_sub(usize::from(body.height));
    let scroll = dialog.scroll.min(max_scroll);
    let options = lines
        .iter()
        .skip(scroll)
        .take(usize::from(body.height))
        .enumerate()
        .filter_map(|(row, (_, index))| {
            index.map(|index| {
                (
                    Rect::new(body.x, body.y.saturating_add(row as u16), body.width, 1),
                    index,
                )
            })
        })
        .collect();
    let footer = inner.bottom().saturating_sub(2);
    let primary = if matches!(dialog.phase, Phase::Confirm(_)) {
        Rect::new(inner.x, footer, inner.width.min(31), 1)
    } else {
        Rect::default()
    };
    let cancel = Rect::new(
        inner.right().saturating_sub(12.min(inner.width)),
        footer,
        12.min(inner.width),
        1,
    );
    Layout {
        body,
        lines,
        options,
        primary,
        cancel,
        max_scroll,
        scroll,
    }
}
pub(in crate::client::shell) fn render_dialog(
    buffer: &mut Buffer,
    dialog: &Dialog,
    palette: &Palette,
) -> Option<OverlayRender> {
    let area = popup(
        buffer.area,
        90,
        buffer.area.height.saturating_sub(2).min(32),
    )?;
    panel(buffer, area, palette.accent, palette.panel_bg)?;
    let layout = layout(dialog, area);
    // A painted border alone cannot authorize a choice/confirmation. Keep
    // readable content and distinct standard footer controls available.
    let usable = layout.body.width >= 44
        && layout.body.height >= 3
        && layout.primary.intersection(layout.cancel).is_empty();
    let title = if !usable {
        "Enlarge terminal to choose or confirm"
    } else if matches!(dialog.phase, Phase::Confirm(_)) {
        "Confirm irreversible answer"
    } else {
        "View action"
    };
    put_text(
        buffer,
        area.x.saturating_add(2),
        area.y,
        area.width.saturating_sub(4),
        title,
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    for (row, (line, option)) in layout
        .lines
        .iter()
        .skip(layout.scroll)
        .take(usize::from(layout.body.height))
        .enumerate()
    {
        let selected = option.is_some()
            && *option == dialog.highlighted
            && matches!(dialog.phase, Phase::Choosing);
        put_text(
            buffer,
            layout.body.x,
            layout.body.y.saturating_add(row as u16),
            layout.body.width,
            line,
            Style::default()
                .fg(if option.is_some() {
                    palette.accent
                } else {
                    palette.text
                })
                .bg(if selected {
                    palette.surface0
                } else {
                    palette.panel_bg
                }),
        );
    }
    if usable && !layout.primary.is_empty() {
        button(
            buffer,
            layout.primary,
            "Confirm irreversible answer",
            Style::default()
                .fg(palette.text)
                .bg(palette.red)
                .add_modifier(Modifier::BOLD),
        );
    }
    button(
        buffer,
        layout.cancel,
        if matches!(dialog.phase, Phase::Submitting | Phase::Outcome(_)) {
            "Esc close"
        } else {
            "Esc cancel"
        },
        Style::default().fg(palette.text).bg(palette.surface0),
    );
    put_text(
        buffer,
        area.x.saturating_add(2),
        area.bottom().saturating_sub(2),
        area.width.saturating_sub(4),
        &format!(
            "Wheel / PgUp / PgDn · lines {}–{} of {}",
            layout.scroll + 1,
            (layout.scroll + usize::from(layout.body.height)).min(layout.lines.len()),
            layout.lines.len()
        ),
        Style::default().fg(palette.overlay0),
    );
    Some(OverlayRender {
        area,
        view_action_popup: if usable { area } else { Rect::default() },
        primary: if usable {
            layout.primary
        } else {
            Rect::default()
        },
        cancel: layout.cancel,
        ..Default::default()
    })
}
