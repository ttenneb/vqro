//! Raster-only collapsed headers. Geometry and targets remain host layout facts.
use ratatui::{
    buffer::Buffer,
    layout::{Direction, Rect},
    style::Style,
    Frame,
};
use unicode_width::UnicodeWidthStr;

use crate::{app::AppState, layout::CollapsedFrame, view_headers::MAX_HEADER_SCALARS};

pub(super) fn render_collapsed_frames(
    app: &AppState,
    target: Option<super::tab_surface::TabSurfaceTarget>,
    headers: &[CollapsedFrame],
    frame: &mut Frame,
) {
    let Some(workspace) = target.and_then(|target| app.workspaces.get(target.workspace_index))
    else {
        return;
    };
    for header in headers {
        let text = header
            .title_pane
            .and_then(|pane| {
                app.view_headers
                    .panes
                    .get(&pane)
                    .map(|text| text.0.as_str())
            })
            .or_else(|| {
                header
                    .title_pane
                    .and_then(|pane| workspace.pane_state(pane))
                    .and_then(|pane| app.terminals.get(&pane.attached_terminal_id))
                    .and_then(|terminal| {
                        terminal
                            .terminal_title
                            .as_deref()
                            .or(terminal.manual_label.as_deref())
                    })
            })
            .unwrap_or("Pane");
        draw_header(
            frame.buffer_mut(),
            header.rect,
            header.direction,
            text,
            Style::default().fg(app.palette.overlay0),
        );
    }
}

fn draw_header(buffer: &mut Buffer, rect: Rect, direction: Direction, text: &str, style: Style) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    // Terminal titles use the existing cached narrow fact; bound their derived
    // raster text too. Never let an extended/wide cluster spill into a sibling.
    let bounded = text
        .chars()
        .filter(|ch| !ch.is_control())
        .take(MAX_HEADER_SCALARS)
        .collect::<String>();
    buffer.set_stringn(rect.x, rect.y, "▸", 1, style);
    if direction == Direction::Horizontal {
        let mut row = rect.y.saturating_add(1);
        let span = ratatui::text::Span::raw(&bounded);
        for glyph in span.styled_graphemes(style) {
            let glyph = glyph.symbol;
            if row >= rect.bottom() {
                break;
            }
            let width = glyph.width();
            if width == 0 {
                continue;
            }
            let glyph = if width == 1 { glyph } else { "�" };
            buffer.set_stringn(rect.x, row, glyph, 1, style);
            row += 1;
        }
    } else if rect.width > 2 {
        buffer.set_stringn(
            rect.x + 2,
            rect.y,
            &bounded,
            usize::from(rect.width - 2),
            style,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn side_strip_clips_wide_combining_and_emoji_clusters_without_touching_siblings() {
        let area = Rect::new(0, 0, 5, 6);
        let mut buffer = Buffer::filled(area, ratatui::buffer::Cell::new("s"));
        draw_header(
            &mut buffer,
            Rect::new(1, 1, 1, 4),
            Direction::Horizontal,
            "界e\u{301}👩‍💻after",
            Style::default(),
        );
        assert_eq!(buffer[(1, 1)].symbol(), "▸");
        assert_eq!(buffer[(1, 2)].symbol(), "�");
        assert_eq!(buffer[(1, 3)].symbol(), "e\u{301}");
        assert_eq!(buffer[(1, 4)].symbol(), "�");
        for y in 0..6 {
            for x in 0..5 {
                if x != 1 || !(1..5).contains(&y) {
                    assert_eq!(buffer[(x, y)].symbol(), "s");
                }
            }
        }
    }
    #[test]
    fn horizontal_header_clipping_stays_inside_its_row() {
        let area = Rect::new(0, 0, 8, 3);
        let mut buffer = Buffer::filled(area, ratatui::buffer::Cell::new("s"));
        draw_header(
            &mut buffer,
            Rect::new(1, 1, 4, 1),
            Direction::Vertical,
            "界abc",
            Style::default(),
        );
        for y in [0, 2] {
            for x in 0..8 {
                assert_eq!(buffer[(x, y)].symbol(), "s");
            }
        }
        for x in [0, 5, 6, 7] {
            assert_eq!(buffer[(x, 1)].symbol(), "s");
        }
    }
}
