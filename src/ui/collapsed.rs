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
                            .manual_label
                            .as_deref()
                            .or(terminal.terminal_title.as_deref())
                    })
            })
            .unwrap_or("Pane");
        let style = Style::default().fg(if header.focused {
            app.palette.accent
        } else {
            app.palette.overlay0
        });
        draw_header(
            frame.buffer_mut(),
            header.rect,
            header.direction,
            text,
            header.pane_count,
            style,
        );
        if header.focused && header.direction == Direction::Horizontal {
            draw_full_label(
                frame.buffer_mut(),
                header.rect,
                text,
                header.pane_count,
                style.bg(app.palette.surface0),
            );
        }
    }
}

fn draw_header(
    buffer: &mut Buffer,
    rect: Rect,
    direction: Direction,
    text: &str,
    count: usize,
    style: Style,
) {
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
        // One initial/icon and a count, never an unreadable vertical title.
        let span = ratatui::text::Span::raw(&bounded);
        if rect.height > 1 {
            let initial = span
                .styled_graphemes(style)
                .find(|glyph| !glyph.symbol.trim().is_empty() && glyph.symbol.width() > 0)
                .map(|glyph| glyph.symbol)
                .filter(|glyph| glyph.width() == 1)
                .unwrap_or("□");
            buffer.set_stringn(rect.x, rect.y + 1, initial, 1, style);
        }
        if rect.height > 2 {
            let count = if count > 9 {
                "+".to_owned()
            } else {
                count.to_string()
            };
            buffer.set_stringn(rect.x, rect.y + 2, count, 1, style);
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

fn draw_full_label(buffer: &mut Buffer, strip: Rect, text: &str, count: usize, style: Style) {
    let title: String = text
        .chars()
        .filter(|ch| !ch.is_control())
        .take(MAX_HEADER_SCALARS)
        .collect();
    let label = format!(" {title} · {count} ");
    let left = strip.x.saturating_sub(buffer.area.x);
    let right = buffer.area.right().saturating_sub(strip.right());
    let on_right = right >= left;
    let width = label
        .width()
        .min(usize::from(if on_right { right } else { left })) as u16;
    if width == 0 || strip.is_empty() {
        return;
    }
    let x = if on_right {
        strip.right()
    } else {
        strip.x - width
    };
    let row = Rect::new(x, strip.y, width, 1).intersection(buffer.area);
    for x in row.x..row.right() {
        buffer[(x, row.y)].set_symbol(" ").set_style(style);
    }
    buffer.set_stringn(row.x, row.y, label, usize::from(row.width), style);
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
            3,
            Style::default(),
        );
        assert_eq!(buffer[(1, 1)].symbol(), "▸");
        assert_eq!(buffer[(1, 2)].symbol(), "□");
        assert_eq!(buffer[(1, 3)].symbol(), "3");
        assert_eq!(buffer[(1, 4)].symbol(), "s");
        for y in 0..6 {
            for x in 0..5 {
                if x != 1 || !(1..5).contains(&y) {
                    assert_eq!(buffer[(x, y)].symbol(), "s");
                }
            }
        }
    }
    #[test]
    fn collapsed_side_strip_reveals_full_horizontal_label_only_on_focus() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("labels")];
        app.active = Some(0);
        app.ensure_test_terminals();
        let pane = app.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.workspaces[0].terminal_id(pane).unwrap().clone();
        app.terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_manual_label("Shell".into());
        let target = Some(super::super::tab_surface::TabSurfaceTarget {
            workspace_index: 0,
            tab_index: 0,
        });
        for focused in [false, true] {
            let header = CollapsedFrame {
                rect: Rect::new(11, 0, 1, 5),
                path: Vec::new(),
                direction: Direction::Horizontal,
                title_pane: Some(pane),
                leaf_pane: Some(pane),
                pane_count: 1,
                focused,
            };
            let mut terminal = Terminal::new(TestBackend::new(12, 5)).unwrap();
            terminal
                .draw(|frame| {
                    render_collapsed_frames(&app, target, std::slice::from_ref(&header), frame)
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(11, 0)].symbol(), "▸");
            assert_eq!(buffer[(11, 1)].symbol(), "S");
            assert_eq!(buffer[(11, 2)].symbol(), "1");
            let top = (0..11).map(|x| buffer[(x, 0)].symbol()).collect::<String>();
            assert_eq!(top.contains("Shell"), focused, "{top:?}");
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
            1,
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
