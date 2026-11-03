use super::*;

#[test]
fn published_views_use_terminal_default_background_including_title_frame_and_rows() {
    // Reset is the terminal default-cell contract, including host theme/alpha.
    // A deliberately contrasting panel color catches accidental UI tinting.
    let mut palette = Palette::catppuccin();
    palette.panel_bg = ratatui::style::Color::Rgb(170, 20, 90);
    for title in ["Tasks", "Messages"] {
        for focused in [false, true] {
            let mut snapshot = super::super::tests::snapshot();
            snapshot.focused_pane_id = focused.then(|| "view".into());
            let documents:HostDocumentsProjectionV1=serde_json::from_value(serde_json::json!({
            "boot_id":snapshot.boot_id,"revision":snapshot.revision,"documents":[{"provider_priority":1,"document":{
                "contract":"host.document.v1","namespace":"sample.views","generation":1,"revision":1,
                "surface":"pane","target":{"kind":"session"},"items":[
                    {"id":"root","role":"section","label":title,"detail":"ready","view":{"kind":"section","pane_id":"terminal","view_id":"main"}},
                    {"id":"row","parent_id":"root","role":"section","label":"Published row","view":{"kind":"row"}}
                ]
            }}]
        })).unwrap();
            let index = PanelIndex::with_rows(&documents, true);
            let outer = Rect::new(2, 2, 60, 7);
            let inner = Rect::new(3, 3, 58, 5);
            let pane = PaneHit {
                rect: outer,
                inner_rect: inner,
                projected_inner_rect: inner,
                scrollbar_rect: None,
                scroll: None,
                pane_id: "view".into(),
                popup: false,
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                pixel_width: 0,
                pixel_height: 0,
            };
            let scope = vec!["terminal".into()];
            let projection = ViewPaneProjection {
                extension: "sample.views",
                view: "main",
                scope: &scope,
            };
            let mut buffer = Buffer::empty(Rect::new(0, 0, 64, 11));
            buffer.set_style(outer, Style::default().bg(palette.panel_bg));
            render_view_pane(
                &mut buffer,
                &index,
                &documents,
                &snapshot,
                &pane,
                &projection,
                &palette,
            );
            let text = buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(
                text.contains(title) && text.contains("Published row"),
                "{text}"
            );
            // Includes every border/title cell and the one-cell surrounding strip;
            // no independent background may appear even when focused.
            for y in outer.y - 1..=outer.bottom() {
                for x in outer.x - 1..=outer.right() {
                    assert_eq!(
                        buffer[(x, y)].bg,
                        ratatui::style::Color::Reset,
                        "{title} focused={focused} at{x},{y}"
                    );
                }
            }
        }
    }
}
