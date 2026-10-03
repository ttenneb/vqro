use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use super::widgets::panel_contrast_fg;
use crate::{
    app::state::{CopyFeedback, Palette},
    config::ToastClipboardPosition,
};

pub(crate) fn copy_feedback_rect(
    area: Rect,
    feedback: &CopyFeedback,
    offset_rows: u16,
    position: ToastClipboardPosition,
) -> Rect {
    if area.is_empty() {
        return Rect::default();
    }

    let content_width = feedback.message.len() as u16 + 4;
    let width = content_width.min(area.width);
    let height = 3u16.min(area.height);
    let x = match position {
        ToastClipboardPosition::TopLeft | ToastClipboardPosition::BottomLeft => area.x,
        ToastClipboardPosition::TopCenter | ToastClipboardPosition::BottomCenter => {
            area.x + area.width.saturating_sub(width) / 2
        }
        ToastClipboardPosition::TopRight | ToastClipboardPosition::BottomRight => {
            area.x + area.width.saturating_sub(width)
        }
    };
    let y = match position {
        ToastClipboardPosition::TopLeft
        | ToastClipboardPosition::TopCenter
        | ToastClipboardPosition::TopRight => area.y + offset_rows.min(area.height),
        ToastClipboardPosition::BottomLeft
        | ToastClipboardPosition::BottomCenter
        | ToastClipboardPosition::BottomRight => {
            area.y + area.height.saturating_sub(height + offset_rows)
        }
    };
    Rect::new(x, y, width, height)
}

pub(crate) fn render_copy_feedback_buffer(
    buffer: &mut Buffer,
    area: Rect,
    feedback: &CopyFeedback,
    offset_rows: u16,
    position: ToastClipboardPosition,
    palette: &Palette,
) -> Rect {
    let feedback_area = copy_feedback_rect(area, feedback, offset_rows, position);
    if feedback_area.is_empty() {
        return feedback_area;
    }

    Clear.render(feedback_area, buffer);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.green))
        .style(Style::default().bg(palette.panel_bg));
    let inner = block.inner(feedback_area);
    block.render(feedback_area, buffer);

    if inner.height == 0 {
        return feedback_area;
    }

    let text = Line::from(vec![
        Span::styled("●", Style::default().fg(palette.green).bg(palette.panel_bg)),
        Span::raw(" "),
        Span::styled(
            &feedback.message,
            Style::default()
                .fg(palette.text)
                .bg(palette.panel_bg)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    Paragraph::new(text).render(inner, buffer);
    feedback_area
}

/// The banner rows a message occupies at this width. Banners are one row tall by
/// default, so a long message (a restart command, for example) is wrapped at word
/// boundaries instead of being clipped away.
pub(crate) fn diagnostic_banner_lines(message: &str, width: u16, height: u16) -> Vec<String> {
    let wrap_width = (width as usize).saturating_sub(2).max(1);
    let mut lines = Vec::new();
    for line in message.lines().filter(|line| !line.trim().is_empty()) {
        let mut current = String::new();
        for word in line.split_whitespace() {
            if !current.is_empty()
                && display_width(&current) + 1 + display_width(word) <= wrap_width
            {
                current.push(' ');
                current.push_str(word);
                continue;
            }
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            if display_width(word) <= wrap_width {
                current.push_str(word);
                continue;
            }
            // A token wider than the row (a long socket override, for example):
            // split it cell-aware so no cell of the command is dropped.
            let mut chunk = String::new();
            let mut chunk_width = 0usize;
            for character in word.chars() {
                let character_width =
                    unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
                if chunk_width + character_width > wrap_width {
                    lines.push(std::mem::take(&mut chunk));
                    chunk_width = 0;
                }
                chunk.push(character);
                chunk_width += character_width;
            }
            current = chunk;
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    lines.truncate(height as usize);
    lines
}

fn display_width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

/// Draws already-wrapped banner rows. The caller wraps once with
/// `diagnostic_banner_lines`, so it knows the row count without wrapping twice
/// per frame.
pub(crate) fn render_diagnostic_rows(
    buffer: &mut Buffer,
    area: Rect,
    lines: &[String],
    palette: &Palette,
    mut covered: impl FnMut(Rect),
) -> u16 {
    let style = Style::default()
        .fg(panel_contrast_fg(palette))
        .bg(palette.yellow)
        .add_modifier(Modifier::BOLD);
    let mut rendered_rows = 0u16;

    for (row, line) in lines.iter().take(area.height as usize).enumerate() {
        let text = format!(" {line} ");
        let width = (display_width(&text) as u16).min(area.width);
        let diagnostic_area = Rect::new(
            area.x + area.width.saturating_sub(width),
            area.y + row as u16,
            width,
            1,
        );

        covered(diagnostic_area);
        Clear.render(diagnostic_area, buffer);
        Paragraph::new(Span::styled(text, style)).render(diagnostic_area, buffer);
        rendered_rows = rendered_rows.saturating_add(1);
    }

    rendered_rows
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_long_banner_wraps_at_word_boundaries() {
        let notice = "the running vqro server is a different build (server 0.9.0-dev.561da0e3, \
                      client 0.9.0)\nrestart it yourself: `vqro server stop`; stopping it exits its \
                      pane processes, and this client never restarts it.";
        let lines = diagnostic_banner_lines(notice, 106, 20);
        assert!(lines.len() > 2, "{lines:?}");
        for line in &lines {
            assert!(line.chars().count() <= 104, "{line}");
        }
        let command_row = lines
            .iter()
            .find(|line| line.contains("vqro server stop"))
            .unwrap_or_else(|| panic!("command split across rows: {lines:?}"));
        assert!(command_row.contains("restart it yourself"), "{command_row}");
        assert!(
            lines.iter().any(|line| line.contains("exits its pane")),
            "{lines:?}"
        );
    }

    #[test]
    fn an_over_long_token_keeps_every_cell_across_rows() {
        let command = "VQRO_SOCKET_PATH=/home/someone/.local/share/herdr/scratch/deep/nested/root/sock vqro server stop";
        let message = format!("a different server build\nrestart it yourself: `{command}`; stopping it exits its pane processes.");
        let lines = diagnostic_banner_lines(&message, 60, 20);
        // Rows are the whole banner here, so dropping the row breaks (and every
        // space) must leave the command intact.
        let rebuilt: String = lines
            .concat()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("");
        let expected: String = command.split_whitespace().collect::<Vec<_>>().join("");
        assert!(rebuilt.contains(&expected), "{lines:?}");
        for line in &lines {
            assert!(
                unicode_width::UnicodeWidthStr::width(line.as_str()) <= 58,
                "{line}"
            );
        }
    }

    #[test]
    fn a_banner_never_exceeds_its_height() {
        let lines = diagnostic_banner_lines("one\ntwo\nthree", 40, 2);
        assert_eq!(lines, vec!["one".to_owned(), "two".to_owned()]);
    }

    #[test]
    fn copy_feedback_rect_uses_configured_position() {
        let area = Rect::new(10, 20, 100, 40);
        let feedback = CopyFeedback {
            message: "copied to clipboard".to_owned(),
        };

        let top = copy_feedback_rect(area, &feedback, 0, ToastClipboardPosition::TopCenter);
        assert_eq!(top.y, area.y);
        assert_eq!(top.x, area.x + area.width.saturating_sub(top.width) / 2);

        let bottom = copy_feedback_rect(area, &feedback, 0, ToastClipboardPosition::BottomCenter);
        assert_eq!(bottom.bottom(), area.bottom());
        assert_eq!(
            bottom.x,
            area.x + area.width.saturating_sub(bottom.width) / 2
        );
    }
}
