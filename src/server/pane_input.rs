use bytes::Bytes;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};

use crate::protocol::{AttachScrollDirection, AttachScrollSource, ClientPaneInputEvent};
use crate::terminal::ForwardedInputEffect;

/// What forwarded human pane input could have done to the target editor:
/// `None` proves nothing. The last proving event decides, so a typed line
/// followed by Enter reads as submitted.
///
/// Input that can put text in the editor is [`ForwardedInputEffect::Text`], and
/// the only submit key is a plain unmodified Enter, which takes the editor's
/// content as the submitted input. Cursor movement, Esc, function keys and
/// releases cannot write text, so they are [`ForwardedInputEffect::NoText`]:
/// with an editor report in hand they must not look like a draft, or moving the
/// cursor in an empty box would refuse for as long as the report predates the
/// key. Up and Down are the exception among the arrows: Pi browses prompt
/// history with them, which loads text into the editor. Everything else that
/// might insert text stays `Text`: ctrl/alt chords recall history or yank, Tab
/// completes, modified Enter inserts a newline, and the keys that look like
/// clears prove nothing either (Ctrl-C copies the selection, Ctrl-U deletes to
/// the line start only in Pi).
///
/// The `NoText` set assumes Pi's default bindings. A key the user rebinds to
/// history recall, or anything else that writes text, is then covered only by
/// the adapter's immediate re-read after a keystroke, not by this split.
pub(super) fn human_input_unsent_text(
    events: &[ClientPaneInputEvent],
) -> Option<ForwardedInputEffect> {
    let mut effect = None;
    for event in events {
        let event_effect = match event {
            ClientPaneInputEvent::Key {
                code,
                modifiers,
                kind,
                ..
            } => {
                if *kind == crate::protocol::ClientKeyKind::Release {
                    None
                } else if *code == crate::protocol::ClientKeyCode::Enter && *modifiers == 0 {
                    Some(ForwardedInputEffect::Submitted)
                } else if matches!(
                    code,
                    crate::protocol::ClientKeyCode::Left
                        | crate::protocol::ClientKeyCode::Right
                        | crate::protocol::ClientKeyCode::Home
                        | crate::protocol::ClientKeyCode::End
                        | crate::protocol::ClientKeyCode::PageUp
                        | crate::protocol::ClientKeyCode::PageDown
                        | crate::protocol::ClientKeyCode::Insert
                        | crate::protocol::ClientKeyCode::Esc
                        | crate::protocol::ClientKeyCode::F(_)
                        | crate::protocol::ClientKeyCode::Null
                ) {
                    Some(ForwardedInputEffect::NoText)
                } else {
                    Some(ForwardedInputEffect::Text)
                }
            }
            ClientPaneInputEvent::TextCommit(text) | ClientPaneInputEvent::Paste(text) => {
                (!text.is_empty()).then_some(ForwardedInputEffect::Text)
            }
            ClientPaneInputEvent::Mouse { .. } => None,
        };
        if event_effect.is_some() {
            effect = match event_effect {
                // A cursor key or an unproven event changes nothing: a batch that
                // typed or submitted before it keeps that effect, or a trailing
                // Left would hide the text the same batch wrote.
                Some(ForwardedInputEffect::NoText) => effect.or(Some(ForwardedInputEffect::NoText)),
                other => other,
            };
        }
    }
    effect
}

/// What raw input forwarded by a directly attached client could have done to the
/// target editor: `None` proves nothing. Attach forwards the outer terminal's
/// bytes verbatim, so the last byte decides: a trailing Return is the human
/// submitting the line. Any other bytes count as text even when they are only a
/// cursor key's escape sequence, because those bytes cannot be told from a
/// bracket that inserts text; that errs toward refusing, and the editor's own
/// report settles it.
pub(super) fn attach_input_unsent_text(data: &[u8]) -> Option<ForwardedInputEffect> {
    match *data.last()? {
        b'\r' => Some(ForwardedInputEffect::Submitted),
        _ => Some(ForwardedInputEffect::Text),
    }
}

pub(super) fn downgrade_ineligible_pixel_mouse(
    events: &mut [ClientPaneInputEvent],
    pixel_mouse: bool,
    runtime_size: (u16, u16),
    runtime_pixels: Option<(u32, u32)>,
) {
    let (runtime_rows, runtime_cols) = runtime_size;
    for event in events {
        let ClientPaneInputEvent::Mouse {
            position, geometry, ..
        } = event
        else {
            continue;
        };
        let crate::protocol::ClientMousePosition::Pixels { x, y, column, row } = *position else {
            continue;
        };
        let exact = pixel_mouse
            && geometry.is_some_and(|geometry| {
                (runtime_rows, runtime_cols) == (geometry.rows, geometry.cols)
                    && runtime_pixels == Some((geometry.width_px, geometry.height_px))
                    && column < geometry.cols
                    && row < geometry.rows
                    && x > 0
                    && y > 0
                    && x <= geometry.width_px
                    && y <= geometry.height_px
            });
        if !exact {
            *position = crate::protocol::ClientMousePosition::Cell { column, row };
            *geometry = None;
        }
    }
}

pub(super) fn terminal_attach_mouse_position(
    runtime: &crate::terminal::TerminalRuntime,
    terminal_size: (u16, u16),
    cell_size: crate::kitty_graphics::HostCellSize,
    pixel_mouse: bool,
    host_sgr_pixels_active: bool,
    position: crate::protocol::ClientMousePosition,
    geometry: Option<crate::protocol::ClientMouseGeometry>,
) -> Option<crate::protocol::ClientMousePosition> {
    let runtime_size = runtime.current_size();
    let cell_fallback = |column, row| {
        (column < runtime_size.1 && row < runtime_size.0)
            .then_some(crate::protocol::ClientMousePosition::Cell { column, row })
    };
    let (x, y, column, row) = match position {
        crate::protocol::ClientMousePosition::Cell { column, row } => {
            return cell_fallback(column, row);
        }
        crate::protocol::ClientMousePosition::Pixels { x, y, column, row } => (x, y, column, row),
    };
    let Some(geometry) = geometry else {
        return cell_fallback(column, row);
    };
    let host_geometry = crate::input::mouse::HostGeometry::new(
        geometry.cols,
        geometry.rows,
        geometry.width_px,
        geometry.height_px,
    )?;
    if host_geometry.cell(x, y) != Some((column, row)) {
        return None;
    }
    let exact = (|| {
        let average_width = (geometry.width_px / u32::from(geometry.cols)).max(1);
        let average_height = (geometry.height_px / u32::from(geometry.rows)).max(1);
        let (child_width_px, child_height_px) = runtime.pixel_size()?;
        if !pixel_mouse
            || !host_sgr_pixels_active
            || !runtime.sgr_pixel_mouse_enabled()
            || terminal_size != (geometry.cols, geometry.rows)
            || runtime_size != (geometry.rows, geometry.cols)
            || !cell_size.is_known()
            || average_width != cell_size.width_px
            || average_height != cell_size.height_px
        {
            return None;
        }
        let crate::input::mouse::Position::Pixels { x, y } = (crate::input::mouse::HostPixels {
            x,
            y,
            geometry: host_geometry,
        })
        .pane_position(
            ratatui::layout::Rect::new(0, 0, geometry.cols, geometry.rows),
            child_width_px,
            child_height_px,
        )?
        else {
            return None;
        };
        Some(crate::protocol::ClientMousePosition::Pixels { x, y, column, row })
    })();
    exact.or_else(|| cell_fallback(column, row))
}

pub(super) fn apply_terminal_attach_scroll(
    runtime: &crate::terminal::TerminalRuntime,
    source: AttachScrollSource,
    direction: AttachScrollDirection,
    lines: u16,
    column: Option<u16>,
    row: Option<u16>,
    modifiers: u8,
) -> Result<(), String> {
    apply_scroll(
        runtime,
        source,
        direction,
        lines,
        crate::input::mouse::Position::Cell {
            column: column.unwrap_or(0),
            row: row.unwrap_or(0),
        },
        modifiers,
    )
}

fn apply_scroll(
    runtime: &crate::terminal::TerminalRuntime,
    source: AttachScrollSource,
    direction: AttachScrollDirection,
    lines: u16,
    position: crate::input::mouse::Position,
    modifiers: u8,
) -> Result<(), String> {
    let wheel_kind = match direction {
        AttachScrollDirection::Up => MouseEventKind::ScrollUp,
        AttachScrollDirection::Down => MouseEventKind::ScrollDown,
    };
    if let AttachScrollSource::PageKey { input } = source {
        let host_scroll = runtime
            .plain_page_keys_use_host_scrollback()
            .unwrap_or(false);
        if host_scroll {
            match direction {
                AttachScrollDirection::Up => runtime.scroll_up(lines.max(1) as usize),
                AttachScrollDirection::Down => runtime.scroll_down(lines.max(1) as usize),
            }
            return Ok(());
        }
        return apply_terminal_attach_input(runtime, input);
    }

    match runtime.wheel_routing() {
        Some(crate::pane::WheelRouting::MouseReport) => {
            runtime.scroll_reset();
            let Some(bytes) = runtime.encode_mouse_wheel(
                wheel_kind,
                position,
                KeyModifiers::from_bits_truncate(modifiers),
            ) else {
                return Err(format!(
                    "failed to encode terminal attach mouse wheel event: {wheel_kind:?}"
                ));
            };
            runtime
                .try_send_bytes(Bytes::from(bytes))
                .map_err(|err| format!("terminal attach mouse wheel input failed: {err}"))?;
        }
        Some(crate::pane::WheelRouting::AlternateScroll) => {
            runtime.scroll_reset();
            let Some(bytes) = runtime.encode_alternate_scroll(wheel_kind) else {
                return Ok(());
            };
            runtime
                .try_send_bytes(Bytes::from(bytes))
                .map_err(|err| format!("terminal attach alternate scroll input failed: {err}"))?;
        }
        Some(crate::pane::WheelRouting::HostScroll) | None => match direction {
            AttachScrollDirection::Up => runtime.scroll_up(lines.max(1) as usize),
            AttachScrollDirection::Down => runtime.scroll_down(lines.max(1) as usize),
        },
    }
    Ok(())
}

pub(super) fn apply_terminal_attach_input(
    runtime: &crate::terminal::TerminalRuntime,
    data: Vec<u8>,
) -> Result<(), String> {
    runtime.scroll_reset();
    if let Some(text) = crate::raw_input::complete_text_bracketed_paste(&data) {
        runtime
            .try_send_paste(text.to_owned())
            .map_err(|err| format!("terminal attach paste failed: {err}"))
    } else {
        runtime
            .try_send_bytes(Bytes::from(data))
            .map_err(|err| format!("terminal attach input failed: {err}"))
    }
}

#[cfg(test)]
pub(crate) fn test_apply_client_pane_input_events(
    runtime: &crate::terminal::TerminalRuntime,
    events: &[ClientPaneInputEvent],
) -> Result<bool, String> {
    apply_client_pane_input_events(runtime, events)
}

pub(super) fn apply_client_pane_input_events(
    runtime: &crate::terminal::TerminalRuntime,
    events: &[ClientPaneInputEvent],
) -> Result<bool, String> {
    let mut keyboard_forwarded = false;
    apply_client_terminal_input_events(runtime, events, true, &mut keyboard_forwarded)?;
    Ok(keyboard_forwarded)
}

pub(super) fn apply_client_popup_input_events(
    runtime: &crate::terminal::TerminalRuntime,
    events: &[ClientPaneInputEvent],
) -> Result<bool, String> {
    let mut keyboard_forwarded = false;
    apply_client_terminal_input_events(runtime, events, false, &mut keyboard_forwarded)?;
    Ok(keyboard_forwarded)
}

pub(super) fn apply_client_terminal_input_events(
    runtime: &crate::terminal::TerminalRuntime,
    events: &[ClientPaneInputEvent],
    host_page_keys: bool,
    keyboard_forwarded: &mut bool,
) -> Result<(), String> {
    for event in events {
        if let ClientPaneInputEvent::Mouse {
            kind,
            position,
            modifiers,
            lines,
            ..
        } = event
        {
            let kind = kind.to_crossterm();
            let modifiers = KeyModifiers::from_bits_truncate(*modifiers);
            let position = match position {
                crate::protocol::ClientMousePosition::Cell { column, row } => {
                    crate::input::mouse::Position::Cell {
                        column: *column,
                        row: *row,
                    }
                }
                crate::protocol::ClientMousePosition::Pixels { x, y, column, row } => {
                    if runtime.sgr_pixel_mouse_enabled() {
                        crate::input::mouse::Position::Pixels { x: *x, y: *y }
                    } else {
                        crate::input::mouse::Position::Cell {
                            column: *column,
                            row: *row,
                        }
                    }
                }
            };
            let bytes = match kind {
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    let direction = if kind == MouseEventKind::ScrollUp {
                        AttachScrollDirection::Up
                    } else {
                        AttachScrollDirection::Down
                    };
                    apply_scroll(
                        runtime,
                        AttachScrollSource::Wheel,
                        direction,
                        (*lines).max(1),
                        position,
                        modifiers.bits(),
                    )?;
                    continue;
                }
                MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => runtime
                    .encode_mouse_wheel(kind, position, modifiers)
                    .unwrap_or_default(),
                MouseEventKind::Down(_) | MouseEventKind::Up(_) | MouseEventKind::Drag(_) => {
                    runtime
                        .encode_mouse_button(kind, position, modifiers)
                        .unwrap_or_default()
                }
                MouseEventKind::Moved => runtime
                    .encode_mouse_motion(kind, position, modifiers)
                    .unwrap_or_default(),
            };
            if !bytes.is_empty() {
                if kind != MouseEventKind::Moved {
                    runtime.scroll_reset();
                }
                runtime
                    .try_send_bytes(Bytes::from(bytes))
                    .map_err(|err| format!("targeted pane mouse input failed: {err}"))?;
            }
            continue;
        }

        match event.to_raw_input_event() {
            crate::raw_input::RawInputEvent::Key(key) => {
                let key_event = key.as_key_event();
                if host_page_keys
                    && matches!(key_event.code, KeyCode::PageUp | KeyCode::PageDown)
                    && key_event.modifiers.is_empty()
                    && runtime.plain_page_keys_use_host_scrollback() == Some(true)
                {
                    match key_event.kind {
                        KeyEventKind::Release => continue,
                        KeyEventKind::Press | KeyEventKind::Repeat => {
                            let lines = runtime.current_size().0.max(1) as usize;
                            if key_event.code == KeyCode::PageUp {
                                runtime.scroll_up(lines);
                            } else {
                                runtime.scroll_down(lines);
                            }
                            continue;
                        }
                    }
                }

                runtime.scroll_reset();
                let bytes = runtime.encode_terminal_key(key);
                if !bytes.is_empty() {
                    runtime
                        .try_send_bytes(Bytes::from(bytes))
                        .map_err(|err| format!("targeted pane key input failed: {err}"))?;
                    *keyboard_forwarded |= key_event.kind != KeyEventKind::Release;
                }
            }
            crate::raw_input::RawInputEvent::Text(text) => {
                runtime.scroll_reset();
                runtime
                    .try_send_bytes(Bytes::copy_from_slice(text.as_str().as_bytes()))
                    .map_err(|err| format!("targeted pane text input failed: {err}"))?;
                *keyboard_forwarded |= !text.as_str().is_empty();
            }
            crate::raw_input::RawInputEvent::Paste(text) => {
                runtime.scroll_reset();
                runtime
                    .try_send_paste(text)
                    .map_err(|err| format!("targeted pane paste failed: {err}"))?;
            }
            crate::raw_input::RawInputEvent::Mouse(_)
            | crate::raw_input::RawInputEvent::OuterFocusGained
            | crate::raw_input::RawInputEvent::OuterFocusLost
            | crate::raw_input::RawInputEvent::HostDefaultColor { .. }
            | crate::raw_input::RawInputEvent::HostPaletteColors { .. }
            | crate::raw_input::RawInputEvent::HostColorSchemeChanged(_)
            | crate::raw_input::RawInputEvent::HostCellSizeReport { .. }
            | crate::raw_input::RawInputEvent::Unsupported => {
                return Err("non-pane input reached targeted pane input".to_owned());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> ClientPaneInputEvent {
        ClientPaneInputEvent::from_terminal_key(crate::input::TerminalKey::new(code, modifiers))
            .expect("pane input key")
    }

    fn released(code: KeyCode) -> ClientPaneInputEvent {
        ClientPaneInputEvent::from_terminal_key(
            crate::input::TerminalKey::new(code, KeyModifiers::NONE)
                .with_kind(KeyEventKind::Release),
        )
        .expect("pane input key")
    }

    /// Only the submit key proves the editor empty; every other key or byte may
    /// have left text in it (autocomplete, history, a line edited again, or a
    /// partial delete such as Pi's `tui.editor.deleteToLineStart` on Ctrl-U).
    #[test]
    fn forwarded_human_input_marks_unsent_text_except_the_submit_key() {
        use ForwardedInputEffect::{NoText, Submitted, Text};

        let cases = [
            (key(KeyCode::Enter, KeyModifiers::NONE), Some(Submitted)),
            (key(KeyCode::Char('h'), KeyModifiers::NONE), Some(Text)),
            // Cursor movement and function keys cannot write text, except the
            // Up/Down arrows, which Pi browses prompt history with.
            (key(KeyCode::Up, KeyModifiers::NONE), Some(Text)),
            (key(KeyCode::Down, KeyModifiers::NONE), Some(Text)),
            (key(KeyCode::Left, KeyModifiers::NONE), Some(NoText)),
            (key(KeyCode::Home, KeyModifiers::NONE), Some(NoText)),
            (key(KeyCode::PageDown, KeyModifiers::NONE), Some(NoText)),
            (key(KeyCode::F(5), KeyModifiers::NONE), Some(NoText)),
            (key(KeyCode::Tab, KeyModifiers::NONE), Some(Text)),
            (key(KeyCode::Backspace, KeyModifiers::NONE), Some(Text)),
            // Delete-to-line-start and copy-selection are not clears: text can
            // survive them (Pi: `tui.editor.deleteToLineStart`, `tui.input.copy`).
            (key(KeyCode::Char('u'), KeyModifiers::CONTROL), Some(Text)),
            (key(KeyCode::Char('c'), KeyModifiers::CONTROL), Some(Text)),
            (key(KeyCode::Char('r'), KeyModifiers::CONTROL), Some(Text)),
            (key(KeyCode::Esc, KeyModifiers::NONE), Some(NoText)),
            (key(KeyCode::Enter, KeyModifiers::SHIFT), Some(Text)),
            (key(KeyCode::Char('c'), KeyModifiers::ALT), Some(Text)),
            (ClientPaneInputEvent::TextCommit("typed".into()), Some(Text)),
            (ClientPaneInputEvent::Paste("pasted".into()), Some(Text)),
            (released(KeyCode::Char('h')), None),
            (ClientPaneInputEvent::TextCommit(String::new()), None),
            (ClientPaneInputEvent::Paste(String::new()), None),
            (
                ClientPaneInputEvent::Mouse {
                    kind: crate::protocol::ClientMouseKind::Down(
                        crate::protocol::ClientMouseButton::Left,
                    ),
                    position: crate::protocol::ClientMousePosition::Cell { column: 1, row: 1 },
                    geometry: None,
                    modifiers: 0,
                    lines: 1,
                },
                None,
            ),
        ];
        for (event, expected) in cases {
            assert_eq!(
                human_input_unsent_text(std::slice::from_ref(&event)),
                expected,
                "{event:?}"
            );
        }
        assert_eq!(human_input_unsent_text(&[]), None);

        // A batch is decided by its last proving event, so a line the human
        // typed and then submitted is not left unsent.
        assert_eq!(
            human_input_unsent_text(&[
                key(KeyCode::Char('h'), KeyModifiers::NONE),
                released(KeyCode::Char('h')),
                key(KeyCode::Enter, KeyModifiers::NONE),
            ]),
            Some(Submitted)
        );
        assert_eq!(
            human_input_unsent_text(&[
                key(KeyCode::Char('u'), KeyModifiers::CONTROL),
                key(KeyCode::Char('h'), KeyModifiers::NONE),
            ]),
            Some(Text)
        );

        // A no-text key in the same batch must not hide what the batch wrote:
        // the client can deliver a paste or a typed word and a cursor key
        // together, and the text is still in the editor.
        let mixed = [
            (
                vec![
                    ClientPaneInputEvent::TextCommit("human draft".into()),
                    key(KeyCode::Left, KeyModifiers::NONE),
                ],
                Some(Text),
            ),
            (
                vec![
                    ClientPaneInputEvent::TextCommit("human draft".into()),
                    key(KeyCode::Left, KeyModifiers::NONE),
                    key(KeyCode::Enter, KeyModifiers::NONE),
                ],
                Some(Submitted),
            ),
            (
                vec![
                    key(KeyCode::Enter, KeyModifiers::NONE),
                    key(KeyCode::Char('h'), KeyModifiers::NONE),
                ],
                Some(Text),
            ),
            (
                vec![
                    ClientPaneInputEvent::Paste("pasted".into()),
                    key(KeyCode::Home, KeyModifiers::NONE),
                ],
                Some(Text),
            ),
            (
                vec![
                    released(KeyCode::Char('h')),
                    key(KeyCode::Left, KeyModifiers::NONE),
                ],
                Some(NoText),
            ),
            (
                vec![
                    key(KeyCode::Enter, KeyModifiers::NONE),
                    key(KeyCode::Left, KeyModifiers::NONE),
                ],
                Some(Submitted),
            ),
        ];
        for (events, expected) in mixed {
            assert_eq!(human_input_unsent_text(&events), expected, "{events:?}");
        }

        // Attached clients forward the outer terminal's bytes, so the last
        // byte decides the same way.
        let attached = [
            (b"".as_slice(), None),
            (b"\r".as_slice(), Some(Submitted)),
            // Not clears on the raw path either: the outer terminal's own bytes
            // for Ctrl-C and Ctrl-U prove nothing about the editor.
            (b"\x03".as_slice(), Some(Text)),
            (b"\x15".as_slice(), Some(Text)),
            (b"x".as_slice(), Some(Text)),
            (b"half typed\r".as_slice(), Some(Submitted)),
            (b"\x1b[A".as_slice(), Some(Text)),
        ];
        for (data, expected) in attached {
            assert_eq!(attach_input_unsent_text(data), expected, "{data:?}");
        }
    }

    #[tokio::test]
    async fn terminal_attach_stale_geometry_falls_back_to_the_canonical_cell() {
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 5, b"");
        let position = crate::protocol::ClientMousePosition::Pixels {
            x: 121,
            y: 81,
            column: 12,
            row: 4,
        };

        assert_eq!(
            terminal_attach_mouse_position(
                &runtime,
                (20, 5),
                crate::kitty_graphics::HostCellSize {
                    width_px: 10,
                    height_px: 20,
                },
                true,
                false,
                position,
                Some(crate::protocol::ClientMouseGeometry {
                    cols: 20,
                    rows: 5,
                    width_px: 200,
                    height_px: 100,
                }),
            ),
            Some(crate::protocol::ClientMousePosition::Cell { column: 12, row: 4 })
        );
        assert_eq!(
            terminal_attach_mouse_position(
                &runtime,
                (20, 5),
                crate::kitty_graphics::HostCellSize {
                    width_px: 10,
                    height_px: 20,
                },
                true,
                false,
                crate::protocol::ClientMousePosition::Pixels {
                    x: 120,
                    y: 80,
                    column: 12,
                    row: 4,
                },
                Some(crate::protocol::ClientMouseGeometry {
                    cols: 20,
                    rows: 5,
                    width_px: 200,
                    height_px: 100,
                }),
            ),
            None
        );
        assert_eq!(
            terminal_attach_mouse_position(
                &runtime,
                (80, 24),
                crate::kitty_graphics::HostCellSize::default(),
                false,
                false,
                crate::protocol::ClientMousePosition::Cell { column: 12, row: 4 },
                None,
            ),
            Some(crate::protocol::ClientMousePosition::Cell { column: 12, row: 4 })
        );
    }

    #[test]
    fn ineligible_shell_pixel_mouse_uses_its_canonical_cell_position() {
        let mut events = vec![ClientPaneInputEvent::Mouse {
            kind: crate::protocol::ClientMouseKind::Down(crate::protocol::ClientMouseButton::Left),
            position: crate::protocol::ClientMousePosition::Pixels {
                x: 121,
                y: 81,
                column: 12,
                row: 4,
            },
            geometry: Some(crate::protocol::ClientMouseGeometry {
                cols: 20,
                rows: 5,
                width_px: 200,
                height_px: 100,
            }),
            modifiers: 0,
            lines: 1,
        }];

        downgrade_ineligible_pixel_mouse(&mut events, false, (5, 20), Some((200, 100)));

        assert!(matches!(
            events.as_slice(),
            [ClientPaneInputEvent::Mouse {
                position: crate::protocol::ClientMousePosition::Cell { column: 12, row: 4 },
                ..
            }]
        ));
    }

    #[test]
    fn eligible_shell_pixel_mouse_remains_exact() {
        let position = crate::protocol::ClientMousePosition::Pixels {
            x: 121,
            y: 81,
            column: 12,
            row: 4,
        };
        let mut events = vec![ClientPaneInputEvent::Mouse {
            kind: crate::protocol::ClientMouseKind::Moved,
            position,
            geometry: Some(crate::protocol::ClientMouseGeometry {
                cols: 20,
                rows: 5,
                width_px: 200,
                height_px: 100,
            }),
            modifiers: 0,
            lines: 1,
        }];

        downgrade_ineligible_pixel_mouse(&mut events, true, (5, 20), Some((200, 100)));

        assert!(matches!(
            events.as_slice(),
            [ClientPaneInputEvent::Mouse {
                position: current,
                ..
            }] if *current == position
        ));
    }

    #[test]
    fn stale_shell_pixel_geometry_downgrades_to_its_canonical_cell() {
        let mut events = vec![ClientPaneInputEvent::Mouse {
            kind: crate::protocol::ClientMouseKind::Moved,
            position: crate::protocol::ClientMousePosition::Pixels {
                x: 121,
                y: 81,
                column: 12,
                row: 4,
            },
            geometry: Some(crate::protocol::ClientMouseGeometry {
                cols: 20,
                rows: 5,
                width_px: 200,
                height_px: 100,
            }),
            modifiers: 0,
            lines: 1,
        }];

        downgrade_ineligible_pixel_mouse(&mut events, true, (6, 20), Some((200, 120)));

        assert!(matches!(
            events.as_slice(),
            [ClientPaneInputEvent::Mouse {
                position: crate::protocol::ClientMousePosition::Cell { column: 12, row: 4 },
                geometry: None,
                ..
            }]
        ));
    }
}
