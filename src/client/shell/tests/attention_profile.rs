use super::*;

// Supporting wall-clock evidence; deterministic cache/admission tests are the contract.
#[test]
#[ignore]
fn render_scale_profile_native_attention() {
    for count in [1, 15] {
        let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
        let mut snap = snapshot();
        let pane = snap.panes[0].clone();
        snap.panes = (0..count)
            .map(|index| {
                let mut pane = pane.clone();
                pane.pane_id = format!("pane_{index}");
                pane
            })
            .collect();
        state.set_endpoint_status(&ClientEndpointId::Local, ClientEndpointStatus::Online);
        state.set_endpoint_snapshot_for_generation(&ClientEndpointId::Local, 1, Box::new(snap));
        let mut pane_surface = surface();
        let pane = pane_surface.panes[0].clone();
        pane_surface.panes = (0..count)
            .map(|index| {
                let mut pane = pane.clone();
                pane.pane_id = format!("pane_{index}");
                pane
            })
            .collect();
        state.set_pane_surface(pane_surface);
        let mut items = vec![
            serde_json::json!({"id":"context:attention.view:independent:read","role":"section","label":"Work"}),
            serde_json::json!({"id":format!("context:attention.badge:workspace:ws_1:0:{count}:0"),"role":"section","label":format!("{count} blocked")}),
            serde_json::json!({"id":format!("context:attention.badge:tab:tab_1:0:{count}:0"),"role":"section","label":format!("{count} blocked")}),
        ];
        items.extend((0..count).map(|index|serde_json::json!({"id":format!("context:attention.row:task-{index}"),"role":"section","label":format!("Durable debt {index}"),"detail":"Owner · unblock"})));
        let projection=serde_json::from_value(serde_json::json!({"contract":"host.documents.v1","boot_id":"boot-1","revision":1,
            "documents":[{"provider_priority":1,"document":{"contract":"host.document.v1","namespace":"independent.attention","generation":7,"revision":2,
                "surface":"navigation","target":{"kind":"session"},"items":items,"actions":[],"required_capabilities":[]}}]})).expect("projection");
        state.set_endpoint_host_documents_for_generation(
            &ClientEndpointId::Local,
            1,
            Box::new(projection),
        );
        state.compose(106, 40).expect("warm frame");
        let start = std::time::Instant::now();
        for _ in 0..1000 {
            std::hint::black_box(state.compose(106, 40).expect("frame"));
        }
        println!(
            "native_attention panes={count} geometry=106x40 full_frames=1000 total_us={}",
            start.elapsed().as_micros()
        );
    }
}
