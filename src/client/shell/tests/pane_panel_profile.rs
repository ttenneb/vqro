//! Supporting wall-clock evidence for the pane panel's cost inside the
//! pane-scaled render loop. Behavioral fences for the same code live in
//! `tests::pane_panel`; this only reports a scaling delta.

use super::*;

/// A real tiling: every pane has its own rect and its own terminal text, so the
/// panel path is exercised at the cardinality the render rule asks about. (The
/// older profile test clones one 4x2 pane, which cannot draw a panel at all.)
fn tiled_surface(cols: u16, rows: u16, count: usize) -> PaneSurfaceFrame {
    let columns = match count {
        1 => 1,
        _ => 5,
    };
    let lines = (0..rows)
        .map(|row| {
            let mut line = String::new();
            while line.chars().count() < usize::from(cols) {
                line.push_str(&format!("row{row:02} populated 界 terminal output "));
            }
            line.chars().take(usize::from(cols)).collect::<String>()
        })
        .collect::<Vec<_>>();
    let buffer = Buffer::with_lines(lines);
    let pane_width = cols / columns;
    let pane_rows = (count as u16).div_ceil(columns);
    let pane_height = rows / pane_rows.max(1);
    let panes = (0..count)
        .map(|index| {
            let column = index as u16 % columns;
            let row = index as u16 / columns;
            let rect = SurfaceRect {
                x: column * pane_width,
                y: row * pane_height,
                width: pane_width,
                height: pane_height,
            };
            PaneSurfacePane {
                pane_id: format!("pane_{index}"),
                content_revision: 0,
                rect,
                inner_rect: rect,
                scrollbar_rect: None,
                scroll: None,
                focused: index == 0,
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                alternate_screen_active: false,
                pixel_width: 0,
                pixel_height: 0,
            }
        })
        .collect();
    PaneSurfaceFrame {
        boot_id: "boot-1".into(),
        projection_revision: 1,
        surface_revision: 1,
        frame: FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, None, &[]),
        panes,
        collections: Vec::new(),
        splits: Vec::new(),
        graphics: crate::protocol::SurfaceGraphicsScene::default(),
        popup: None,
    }
}

fn panel_state(cols: u16, rows: u16, count: usize, open: bool) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
    state.set_endpoint_capabilities_for(
        &ClientEndpointId::Local,
        vec![crate::protocol::endpoint::PANE_PANELS_CAPABILITY.into()],
    );
    let mut snap = snapshot();
    let pane = snap.panes[0].clone();
    snap.panes = (0..count)
        .map(|index| {
            let mut pane = pane.clone();
            pane.pane_id = format!("pane_{index}");
            pane.focused = index == 0;
            pane
        })
        .collect();
    snap.focused_pane_id = Some("pane_0".into());
    state.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 1, Box::new(snap));
    state.set_pane_surface(tiled_surface(cols, rows, count));
    let mut items = Vec::new();
    for index in 0..count {
        items.push(serde_json::json!({
            "id": format!("pane:pane_{index}:tasks"),
            "role": "section",
            "label": format!("Tasks {index}"),
            "detail": "2/6 done · 1 active · 1 blocked",
            "view": {"pane_id": format!("pane_{index}"), "view_id": "tasks", "state": "in_progress"},
        }));
        for row in 0..4 {
            items.push(serde_json::json!({
                "id": format!("pane:pane_{index}:tasks:row{row}"),
                "parent_id": format!("pane:pane_{index}:tasks"),
                "role": "section",
                "label": format!("Owned task {index}.{row}"),
                "detail": "waiting on you",
                "view": {"state": "blocked"},
            }));
        }
    }
    let projection = serde_json::from_value(serde_json::json!({
        "contract":"host.documents.v1","boot_id":"boot-1","revision":1,
        "documents":[{"provider_priority":1,"document":{
            "contract":"host.view.v1","namespace":"example.tasks","generation":7,"revision":2,
            "surface":"navigation","target":{"kind":"session"},
            "items":items,"actions":[],"required_capabilities":[]}}]
    }))
    .expect("projection");
    state.set_endpoint_host_documents_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(projection),
    );
    if open {
        for index in 0..count {
            state
                .pane_panel_open
                .entry(format!("pane_{index}"))
                .or_default()
                .insert("tasks".into());
        }
    }
    state
}

#[test]
#[ignore = "wall-clock evidence, run by `just bench-render-scale`"]
fn render_scale_profile_pane_panels() {
    let (cols, rows) = (106, 40);
    let mut baseline = 0_u128;
    for count in [1, 15] {
        for open in [false, true] {
            let mut state = panel_state(cols, rows, count, open);
            state.compose(cols, rows).expect("warm frame");
            let mut samples = Vec::new();
            for _ in 0..35 {
                let start = std::time::Instant::now();
                for _ in 0..20 {
                    std::hint::black_box(state.compose(cols, rows).expect("frame"));
                }
                samples.push(start.elapsed().as_micros());
            }
            samples.sort_unstable();
            let median = samples[samples.len() / 2];
            let p95 = samples[samples.len() * 95 / 100];
            if !open {
                baseline = median;
            }
            println!(
                "pane_panels panes={count} open={open} drawn_panels={} median_us={median} p95_us={p95} vs_baseline={:.2}x",
                state.pane_panel_areas.len(),
                median as f64 / baseline.max(1) as f64
            );
        }
    }
}

/// One accepted update's cost, at the update frequency QA's gate asks about: the
/// direct retained patch in the fast path against the fallback that composes. This
/// is the client's own caller shape (`client/mod.rs`): a fast application presents
/// the patch's rows; `Applied(None)` composes and presents a frame.
#[test]
#[ignore = "wall-clock evidence, run by `just bench-render-scale`"]
fn render_scale_profile_panel_patch_feed() {
    let (cols, rows) = (106, 40);
    for count in [1, 15] {
        for open in [false, true] {
            let mut state = panel_state(cols, rows, count, open);
            let _ = state.compose(cols, rows).expect("warm frame");
            let pane_id = state
                .pane_surface
                .as_ref()
                .expect("surface")
                .panes
                .first()
                .map(|pane| pane.pane_id.clone())
                .expect("a pane");
            let mut samples = Vec::new();
            let mut fast = 0_usize;
            let mut fallback = 0_usize;
            for _ in 0..35 {
                let start = std::time::Instant::now();
                for step in 0..20_u32 {
                    let pane = state
                        .pane_surface
                        .as_ref()
                        .expect("surface")
                        .panes
                        .iter()
                        .find(|pane| pane.pane_id == pane_id)
                        .cloned()
                        .expect("pane");
                    let surface = state.pane_surface.as_ref().expect("surface");
                    let mut cell = surface.frame.cells.first().cloned().expect("a cell");
                    cell.symbol = ((b'0' + (step % 10) as u8) as char).to_string();
                    let patch = crate::protocol::PaneSurfacePatch {
                        boot_id: "boot-1".into(),
                        projection_revision: 1,
                        base_surface_revision: surface.surface_revision,
                        surface_revision: surface.surface_revision.saturating_add(1),
                        rows: vec![crate::protocol::PaneSurfacePatchRow {
                            x: pane.inner_rect.x,
                            y: pane.inner_rect.y.saturating_add(1),
                            cells: vec![cell],
                        }],
                        panes: vec![pane],
                        cursor: None,
                    };
                    match state.apply_pane_surface_patch(patch) {
                        crate::client::shell::ClientPaneSurfacePatchOutcome::Applied(Some(
                            composed,
                        )) => {
                            fast += 1;
                            std::hint::black_box(composed);
                        }
                        crate::client::shell::ClientPaneSurfacePatchOutcome::Applied(None) => {
                            fallback += 1;
                            std::hint::black_box(state.compose(cols, rows).expect("frame"));
                        }
                        crate::client::shell::ClientPaneSurfacePatchOutcome::Rejected => {
                            panic!("the profile's patches are valid")
                        }
                    }
                }
                samples.push(start.elapsed().as_micros());
            }
            samples.sort_unstable();
            let median = samples[samples.len() / 2];
            let p95 = samples[samples.len() * 95 / 100];
            println!(
                "panel_patch_feed panes={count} open={open} drawn_panels={} fast_updates={fast} fallback_updates={fallback} per_update_us={:.1} median_20_us={median} p95_20_us={p95}",
                state.pane_panel_areas.len(),
                median as f64 / 20.0
            );
        }
    }
}
