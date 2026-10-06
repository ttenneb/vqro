use super::*;

/// A tab badge is a marker, not a sentence: the attention label ("1 waiting on you")
/// becomes its count, so the tab keeps its own label readable.
fn compact_badge_label(label: &str) -> String {
    let digits = label
        .trim()
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>();
    if digits.is_empty() {
        return label.trim().chars().take(4).collect();
    }
    // A count that keeps growing would eat the tab it belongs to: the badge is a
    // marker, capped at two digits, and "99+" says everything "100" would in this
    // space.
    if digits.len() > 2 {
        return "99+".to_string();
    }
    digits
}

/// How a tab splits its own cells between its label and a badge on it: the badge
/// takes the tab's tail, leaving a cell of gutter before the right edge, and the
/// label gets everything strictly before that. The two allocations are disjoint -
/// a label centred across the badge's cells is what turned "Plans" into "Pla[100]".
fn tab_cell_split(width: u16, badge_width: u16) -> (u16, u16) {
    let badge = badge_width.min(width.saturating_sub(1));
    let label = width
        .saturating_sub(badge)
        .saturating_sub(u16::from(badge > 0));
    (label, badge)
}

const TAB_SCROLL_BUTTON_WIDTH: u16 = 3;
const MIN_TAB_STRIP_WIDTH: u16 =
    MIN_TAB_WIDTH + NEW_TAB_WIDTH + TAB_SCROLL_BUTTON_WIDTH.saturating_mul(2);

pub(crate) fn render_tab_bar(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    config: &ClientShellConfig,
    tab_scroll: &mut usize,
    reveal_focused_tab: &mut bool,
    tab_drag_insert_index: Option<usize>,
    attention: Option<&super::attention::AttentionNavigation>,
    hits: &mut ShellHitMap,
) {
    let palette = &config.palette;
    buffer.set_style(area, Style::default().bg(palette.panel_bg));
    let tabs = snapshot
        .tabs
        .iter()
        .filter(|tab| Some(tab.workspace_id.as_str()) == snapshot.focused_workspace_id.as_deref())
        .collect::<Vec<_>>();
    let desired_widths = tabs
        .iter()
        .map(|tab| {
            let label = tab_label(tab);
            // The width the tab asks for is the width it draws: the compact badge
            // label, not the attention sentence, or the badge takes the tab over and
            // later tabs fall off the strip.
            let badge_width = attention
                .and_then(|nav| nav.badge("tab", &tab.tab_id))
                .map_or(0, |label| {
                    display_width(&compact_badge_label(&label.label)).saturating_add(3)
                });
            display_width(&label)
                .saturating_add(badge_width)
                .saturating_add(4)
                .max(MIN_TAB_WIDTH)
        })
        .collect::<Vec<_>>();
    let content = tab_bar_content_area(snapshot, area);
    let mouse_chrome = config.mouse_capture;
    let new_tab_width = if mouse_chrome { NEW_TAB_WIDTH } else { 0 };
    let desired_total = desired_widths
        .iter()
        .copied()
        .fold(0_u16, u16::saturating_add)
        .saturating_add(tabs.len().saturating_sub(1).min(u16::MAX as usize) as u16)
        .saturating_add(new_tab_width);
    let overflow =
        desired_total > content.width && (!mouse_chrome || content.width >= MIN_TAB_STRIP_WIDTH);
    let available = if overflow && mouse_chrome {
        content
            .width
            .saturating_sub(NEW_TAB_WIDTH)
            .saturating_sub(TAB_SCROLL_BUTTON_WIDTH.saturating_mul(2))
    } else {
        content.width.saturating_sub(new_tab_width)
    };
    let max_scroll = max_tab_scroll(&desired_widths, available);
    if !overflow {
        *tab_scroll = 0;
    } else if *reveal_focused_tab {
        if let Some(focused) = tabs.iter().position(|tab| tab.focused) {
            *tab_scroll = centered_tab_scroll(focused, &desired_widths, available).min(max_scroll);
        }
    } else {
        *tab_scroll = (*tab_scroll).min(max_scroll);
    }
    *reveal_focused_tab = false;

    let mut x = content.x;
    let tab_right = if overflow && mouse_chrome {
        hits.tab_scroll_left = Rect::new(
            content.x,
            content.y,
            TAB_SCROLL_BUTTON_WIDTH.min(content.width),
            1,
        );
        put_text(
            buffer,
            hits.tab_scroll_left.x,
            content.y,
            hits.tab_scroll_left.width,
            " < ",
            Style::default()
                .fg(if *tab_scroll > 0 {
                    palette.overlay1
                } else {
                    palette.overlay0
                })
                .bg(palette.surface0),
        );
        x = hits.tab_scroll_left.right();
        content
            .right()
            .saturating_sub(NEW_TAB_WIDTH + TAB_SCROLL_BUTTON_WIDTH)
    } else {
        content.right().saturating_sub(new_tab_width)
    };

    let mut first_visible = None;
    let mut last_visible = None;
    for (index, tab) in tabs.iter().enumerate().skip(*tab_scroll) {
        let name = tab_label(tab);
        let desired = desired_widths[index];
        let remaining = tab_right.saturating_sub(x);
        let width = desired.min(remaining);
        if width == 0 {
            break;
        }
        // Known before the label is drawn, because the label is centred in the space
        // the badge leaves rather than across the badge.
        let badge = attention
            .and_then(|nav| nav.badge("tab", &tab.tab_id))
            .map(|badge| compact_badge_label(&badge.label));
        let (name_width, badge_width) = {
            let wanted = badge
                .as_ref()
                .map_or(0, |label| display_width(label).saturating_add(2));
            if badge.is_some() {
                tab_cell_split(width, wanted)
            } else {
                (width, 0)
            }
        };
        let rect = Rect::new(x, area.y, width, 1);
        let style = if tab.focused {
            let base = Style::default()
                .fg(panel_contrast_fg(palette))
                .bg(palette.accent);
            if tab.custom_label {
                base.add_modifier(Modifier::BOLD)
            } else {
                base
            }
        } else if tab.custom_label {
            Style::default().fg(palette.overlay1).bg(palette.surface0)
        } else {
            Style::default().fg(palette.overlay0).bg(palette.surface0)
        };
        // The label is centred in its own allocation, which the badge's cells are not
        // part of: centring across the whole width would push the name under the badge.
        let padding = name_width.saturating_sub(display_width(&name));
        let left = padding / 2;
        let text = format!(
            "{empty:left$}{name}{empty:right_padding$}",
            empty = "",
            left = left as usize,
            right_padding = padding.saturating_sub(left) as usize,
        );
        put_text(buffer, rect.x, rect.y, name_width, &text, style);
        if let Some(label) = badge {
            // The badge is part of the tab button, not a control inside it: it takes
            // the tab's own background, so the selected tab's highlight covers it; it
            // is short enough not to take the tab over; and it registers no hit of
            // its own, so a click anywhere on the tab selects the tab, which is the
            // primary action a tab button owes the user. Opening the panes is what
            // the selected tab's own panels and border badges are for.
            let badge_rect = Rect::new(
                rect.right().saturating_sub(badge_width + 1),
                rect.y,
                badge_width,
                1,
            );
            debug_assert!(
                badge_rect.x >= rect.x.saturating_add(name_width),
                "the label and the badge never share a cell"
            );
            put_text(
                buffer,
                badge_rect.x,
                badge_rect.y,
                badge_rect.width,
                &format!("[{label}]"),
                style.fg(palette.peach),
            );
        }
        hits.tabs.push((rect, tab.tab_id.clone()));
        first_visible.get_or_insert(index);
        last_visible = Some(index);
        x = x.saturating_add(width + 1);
        if width < desired {
            break;
        }
    }

    if overflow && mouse_chrome {
        hits.tab_scroll_right = Rect::new(tab_right, area.y, TAB_SCROLL_BUTTON_WIDTH, 1);
        let can_scroll_right = last_visible.is_some_and(|index| index + 1 < tabs.len());
        put_text(
            buffer,
            hits.tab_scroll_right.x,
            area.y,
            hits.tab_scroll_right.width,
            " > ",
            Style::default()
                .fg(if can_scroll_right {
                    palette.overlay1
                } else {
                    palette.overlay0
                })
                .bg(palette.surface0),
        );
        hits.new_tab = Rect::new(
            hits.tab_scroll_right.right(),
            area.y,
            content
                .right()
                .saturating_sub(hits.tab_scroll_right.right())
                .min(NEW_TAB_WIDTH),
            1,
        );
    } else if mouse_chrome {
        hits.new_tab = Rect::new(
            x.min(content.right()),
            area.y,
            content.right().saturating_sub(x).min(NEW_TAB_WIDTH),
            1,
        );
    }
    if mouse_chrome {
        put_text(
            buffer,
            hits.new_tab.x,
            area.y,
            hits.new_tab.width,
            " + ",
            Style::default().fg(palette.overlay1).bg(palette.panel_bg),
        );
    }

    if first_visible.is_some_and(|index| index > 0) {
        let ellipsis_x = if hits.tab_scroll_left.width > 0 {
            hits.tab_scroll_left.right()
        } else {
            content.x
        };
        put_text(
            buffer,
            ellipsis_x,
            area.y,
            u16::from(ellipsis_x < content.right()),
            "…",
            Style::default().fg(palette.overlay0),
        );
    }
    if last_visible.is_some_and(|index| index + 1 < tabs.len()) {
        let ellipsis_x = if hits.tab_scroll_right.width > 0 {
            hits.tab_scroll_right.x.saturating_sub(1)
        } else {
            content.right().saturating_sub(1)
        };
        put_text(
            buffer,
            ellipsis_x,
            area.y,
            u16::from(ellipsis_x >= content.x && ellipsis_x < content.right()),
            "…",
            Style::default().fg(palette.overlay0),
        );
    }

    if let Some(insert_index) = tab_drag_insert_index {
        if let Some(indicator_x) = tab_drop_indicator_x(hits, &tabs, insert_index) {
            put_text(
                buffer,
                indicator_x.min(content.right().saturating_sub(1)),
                area.y,
                1,
                "│",
                Style::default().fg(palette.accent),
            );
        }
    }
    render_tab_bar_status(buffer, area, snapshot, palette);
}

pub(crate) fn tab_bar_status_width(snapshot: &ClientShellSnapshot) -> u16 {
    let content = snapshot.tab_bar_right.iter().fold(0u16, |width, segment| {
        width.saturating_add(display_width(&segment.text))
    });
    let separators = snapshot.tab_bar_right.len().saturating_sub(1);
    content.saturating_add(
        display_width(&snapshot.tab_bar_right_separator)
            .saturating_mul(separators.min(u16::MAX as usize) as u16),
    )
}

fn tab_bar_status_area(snapshot: &ClientShellSnapshot, area: Rect) -> Option<Rect> {
    let width = tab_bar_status_width(snapshot);
    if width == 0 {
        return None;
    }
    let reserved = width.saturating_add(1);
    (area.width.saturating_sub(reserved) >= MIN_TAB_STRIP_WIDTH)
        .then(|| Rect::new(area.right().saturating_sub(width), area.y, width, 1))
}

fn tab_bar_content_area(snapshot: &ClientShellSnapshot, area: Rect) -> Rect {
    let reserved = tab_bar_status_area(snapshot, area)
        .map(|status| status.width.saturating_add(1))
        .unwrap_or(0);
    Rect {
        width: area.width.saturating_sub(reserved),
        ..area
    }
}

fn render_tab_bar_status(
    buffer: &mut Buffer,
    area: Rect,
    snapshot: &ClientShellSnapshot,
    palette: &Palette,
) {
    let Some(status) = tab_bar_status_area(snapshot, area) else {
        return;
    };
    let separator_width = display_width(&snapshot.tab_bar_right_separator);
    let mut x = status.x;
    for (index, segment) in snapshot.tab_bar_right.iter().enumerate() {
        if index > 0 && separator_width > 0 {
            put_text(
                buffer,
                x,
                area.y,
                separator_width,
                &snapshot.tab_bar_right_separator,
                Style::default().fg(palette.overlay0).bg(palette.panel_bg),
            );
            x = x.saturating_add(separator_width);
        }
        let width = display_width(&segment.text);
        let style = if segment.accent {
            Style::default()
                .fg(panel_contrast_fg(palette))
                .bg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.overlay1).bg(palette.panel_bg)
        };
        put_text(buffer, x, area.y, width, &segment.text, style);
        x = x.saturating_add(width);
    }
}

fn tab_drop_indicator_x(
    hits: &ShellHitMap,
    tabs: &[&ClientShellTab],
    insert_index: usize,
) -> Option<u16> {
    let visible = hits
        .tabs
        .iter()
        .filter_map(|(rect, tab_id)| {
            tabs.iter()
                .position(|tab| tab.tab_id == *tab_id)
                .map(|index| (index, *rect))
        })
        .collect::<Vec<_>>();
    let (first_index, first_rect) = *visible.first()?;
    let (last_index, last_rect) = *visible.last()?;
    if insert_index == 0 {
        return Some(if first_index == 0 {
            first_rect.x
        } else {
            hits.tab_scroll_left.right()
        });
    }
    if let Some((_, rect)) = visible.iter().find(|(index, _)| *index == insert_index) {
        return Some(rect.x.saturating_sub(1));
    }
    if insert_index >= tabs.len() {
        return Some(if last_index + 1 >= tabs.len() {
            last_rect.right()
        } else {
            hits.tab_scroll_right.x.saturating_sub(1)
        });
    }
    None
}

fn centered_tab_scroll(focused: usize, widths: &[u16], available: u16) -> usize {
    let mut best = focused;
    let mut best_distance = u16::MAX;
    for start in 0..=focused {
        let before = widths
            .iter()
            .copied()
            .enumerate()
            .skip(start)
            .take(focused.saturating_sub(start))
            .fold(0u16, |width, (_, tab)| width.saturating_add(tab + 1));
        if before >= available {
            continue;
        }
        let focused_width = widths[focused].min(available.saturating_sub(before));
        let center = before.saturating_mul(2).saturating_add(focused_width);
        let distance = center.abs_diff(available);
        if distance <= best_distance {
            best_distance = distance;
            best = start;
        }
    }
    best
}

fn max_tab_scroll(widths: &[u16], available: u16) -> usize {
    (0..widths.len())
        .find(|start| last_visible_tab(*start, widths, available) == widths.len().checked_sub(1))
        .unwrap_or(0)
}

fn last_visible_tab(start: usize, widths: &[u16], available: u16) -> Option<usize> {
    let mut remaining = available;
    let mut last = None;
    for (index, width) in widths.iter().copied().enumerate().skip(start) {
        if remaining == 0 {
            break;
        }
        last = Some(index);
        if width >= remaining {
            break;
        }
        remaining = remaining.saturating_sub(width.saturating_add(1));
    }
    last
}

fn tab_label(tab: &ClientShellTab) -> String {
    if tab.zoomed {
        format!("{} Z", tab.label)
    } else {
        tab.label.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{compact_badge_label, tab_cell_split};

    #[test]
    fn a_tab_badge_count_is_capped_at_two_digits() {
        // The attention label is a sentence; only its leading count reaches the tab.
        assert_eq!(compact_badge_label("1 waiting on you"), "1");
        assert_eq!(compact_badge_label("42 waiting on you"), "42");
        // Two digits are the cap: the largest count drawn is 99, and everything past
        // it reads the same, so a growing count cannot take the tab's own label.
        assert_eq!(compact_badge_label("99 waiting on you"), "99");
        assert_eq!(compact_badge_label("100 waiting on you"), "99+");
        assert_eq!(compact_badge_label("100000"), "99+");
        // A label with no count keeps a short word instead of an empty badge.
        assert_eq!(compact_badge_label("  blocked "), "bloc");
    }

    #[test]
    fn a_tab_badge_and_its_label_never_share_a_cell() {
        // QA's P3: the label used to be allocated `width - badge_width`, which is the
        // badge's own first cell as well, so on a clipped tab the badge overwrote the
        // last cell of the label's allocation. Every width and badge size must split
        // into two allocations that do not overlap, and the label keeps one cell.
        for width in 1_u16..40 {
            for badge_width in 0_u16..8 {
                let (label, badge) = tab_cell_split(width, badge_width);
                let badge_start = width
                    .saturating_sub(badge)
                    .saturating_sub(u16::from(badge > 0));
                assert!(
                    label <= badge_start,
                    "width {width}, badge {badge_width}: label {label} overlaps a badge \
                     starting at {badge_start}"
                );
                assert!(label + badge <= width, "width {width}, badge {badge_width}");
                if badge > 0 {
                    // A badge so large that it needs the whole tab is the one case the
                    // label can go without a cell; anything smaller leaves it one.
                    assert!(
                        label >= 1 || badge >= width.saturating_sub(1),
                        "width {width}, badge {badge_width}: a badge that does not take the \
                         whole tab leaves the label a cell"
                    );
                }
            }
        }
    }
}
