use super::*;

#[test]
fn tasks_empty_and_populated_messages_validate_and_render_from_package_documents() {
    // Exercise the real producer, not a hand-authored HostView fixture. This is
    // a private store/unit caller fixture, not native principal attribution.
    let output = std::process::Command::new("python3")
        .env_clear()
        .arg("-B")
        .arg("-c")
        .arg(r#"
import json, tempfile
import document, provider
placement = {"pane_id":"terminal", "workspace_id":"w1", "tab_id":"w1:t1"}
snapshot = {"generation":1, "revision":1, "terminals":[{"terminal_id":"b", "placement":placement}]}
a = {"class":"agent", "terminal_id":"a", "agent":"pi", "execution":"boot:1:1"}
b = {"class":"agent", "terminal_id":"b", "agent":"pi", "execution":"boot:2:2"}
with tempfile.TemporaryDirectory() as root:
    missing = document.project(None, snapshot)
    store = provider.Store(root)
    empty = document.project(store, snapshot)
    store.invoke({"caller":a, "payload":{"action":"message_send", "operation_id":"send", "recipient":b, "body":"Review the bounded message"}})
    populated = document.project(store, snapshot)
    old = {**b, "execution":"boot:2:old"}
    store.invoke({"caller":old, "payload":{"action":"create", "operation_id":"old-anchor", "title":"Old execution anchor", "done_when":"Evidence recorded."}})
    store.invoke({"caller":a, "payload":{"action":"message_send", "operation_id":"replacement", "recipient":b, "body":"Replacement message visible"}})
    replacement = document.project(store, snapshot)
    print(json.dumps([missing, empty, populated, replacement]))
    store.db.close()
"#)
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions/tasks"))
        .output()
        .expect("installed Python runs Tasks producer");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let produced: Vec<crate::extension_contract::HostDocumentV1> =
        serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(produced.len(), 4);
    for (index, document) in produced.into_iter().enumerate() {
        document.validate().unwrap();
        let snapshot = super::super::tests::snapshot();
        let documents: HostDocumentsProjectionV1 = serde_json::from_value(serde_json::json!({
            "boot_id":snapshot.boot_id, "revision":snapshot.revision,
            "documents":[{"provider_priority":1, "document":document}]
        }))
        .unwrap();
        let panel_index = PanelIndex::with_rows(&documents, true);
        let outer = Rect::new(1, 1, 78, 8);
        let inner = Rect::new(2, 2, 76, 6);
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
            extension: "vqro.tasks",
            view: "messages",
            scope: &scope,
        };
        let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 10));
        render_view_pane(
            &mut buffer,
            &panel_index,
            &documents,
            &snapshot,
            &pane,
            &projection,
            &Palette::catppuccin(),
        );
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Messages"), "{text}");
        assert!(
            !text.contains("unavailable") && !text.contains("read-only"),
            "{text}"
        );
        if index < 2 {
            assert!(text.contains("No messages"), "{text}");
        } else {
            assert!(text.contains("Review the bounded message"), "{text}");
            assert!(
                text.contains("UNRESOLVED")
                    && text.contains("Recorded recipient")
                    && !text.contains("waiting on you")
                    && !text.contains("No messages"),
                "{text}"
            );
        }
        if index == 3 {
            assert!(text.contains("Replacement message visible"), "{text}");
            assert!(text.contains("2 recorded waiting"), "{text}");
        }
        let other_scope = vec!["not-covered".into()];
        let other = ViewPaneProjection {
            scope: &other_scope,
            ..projection
        };
        assert!(view_sections(&panel_index, &documents, &other).is_empty());
    }
}

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
