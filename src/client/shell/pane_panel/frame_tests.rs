use super::*;

#[test]
fn unavailable_view_preserves_every_perimeter_and_uses_host_inner_rect() {
    let mut snapshot = super::super::tests::snapshot();
    snapshot.focused_pane_id = Some("w1:pview".into());
    let documents = HostDocumentsProjectionV1 {
        boot_id: snapshot.boot_id.clone(),
        revision: snapshot.revision,
        documents: Vec::new(),
    };
    let index = PanelIndex::with_rows(&documents, true);
    let app = crate::app::AppState::test_new();
    for (width, height) in [(40, 8), (4, 2), (4, 3)] {
        let outer = Rect::new(1, 1, width, height);
        let inner = Rect::new(2, 2, width - 2, height - 2);
        let pane = PaneHit {
            rect: outer,
            inner_rect: inner,
            projected_inner_rect: inner,
            scrollbar_rect: None,
            scroll: None,
            pane_id: "w1:pview".into(),
            popup: false,
            mouse_reporting: false,
            sgr_pixel_mouse: false,
            pixel_width: 0,
            pixel_height: 0,
        };
        let projection = ViewPaneProjection {
            extension: "vqro.tasks",
            view: "Tasks",
            scope: &[],
        };
        let mut buffer = Buffer::empty(Rect::new(0, 0, 44, 12));
        render_view_pane(
            &mut buffer,
            &index,
            &documents,
            &snapshot,
            &pane,
            &projection,
            &app.palette,
        );
        for ((x, y), symbol) in [
            ((outer.x, outer.y), "┌"),
            ((outer.right() - 1, outer.y), "┐"),
            ((outer.x, outer.bottom() - 1), "└"),
            ((outer.right() - 1, outer.bottom() - 1), "┘"),
        ] {
            assert_eq!(buffer[(x, y)].symbol(), symbol);
        }
        assert_eq!(buffer[(0, 0)].symbol(), " ");
        if width == 40 {
            let row = (inner.x..inner.right())
                .map(|x| buffer[(x, inner.y)].symbol())
                .collect::<String>();
            assert!(row.contains("vqro.tasks unavailable"), "{row}");
        }
    }
}
