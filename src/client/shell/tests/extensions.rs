use super::*;

fn extension_info(installed: bool) -> crate::api::schema::ExtensionInfo {
    crate::api::schema::ExtensionInfo {
        id: "vqro.collections".into(),
        repository: "ttenneb/vqro-extension-collections".into(),
        source_commit: Some("93624cc67dc47b380e7233b60223acc1da64c47f".into()),
        source_sha256: Some(
            "35b8a4fb8a6b434faf15625dc8d213e1953d939cef77f5affd5cef7be0062376".into(),
        ),
        manifest_sha256: Some(
            "6b796bf133eae9496695616e299fd614e34ed619d73bdf99cd656902bc48e69d".into(),
        ),
        manifest_path: Some("herdr-plugin.toml".into()),
        name: "Collections".into(),
        summary: "Group related terminal panes.".into(),
        details: "First-party collection workflows through a public contract.".into(),
        categories: vec!["Workflow".into(), "Interface".into()],
        requested_capabilities: vec!["host.document.render".into()],
        package_kind: crate::api::schema::ExtensionPackageKind::Bundled,
        integration_target: None,
        integration_status: None,
        available_version: "1.0.0".into(),
        active_version: "1.0.0".into(),
        installed,
        enabled: installed,
        install_supported: true,
        artifact_verified: true,
        repository_override_supported: true,
        load_order: 0,
        pending_version: None,
        rollback_version: None,
        activation_error: None,
        repository_overrides: Vec::new(),
    }
}

#[test]
fn global_menu_opens_browse_and_requests_optional_extension_list() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let mut outcome = ClientShellInput::default();

    state.open_extensions_overlay(&mut outcome);

    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Extensions(ClientExtensionsOverlay {
            tab: ClientExtensionsTab::Browse,
            loading: true,
            ..
        }))
    ));
    assert!(outcome.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(request.method, crate::api::schema::Method::ExtensionList(_))
    )));
}

#[test]
fn extension_overlay_renders_categories_details_and_installed_tab() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.overlay = Some(ClientShellOverlay::Extensions(ClientExtensionsOverlay {
        generation: 1,
        tab: ClientExtensionsTab::Browse,
        query: String::new(),
        search_focused: false,
        category: None,
        selected: 0,
        extensions: vec![extension_info(true)],
        loading: false,
        mutating: false,
        confirmation: None,
        confirmation_extension: None,
        repository_context: None,
        message: None,
    }));

    let frame = state.compose(110, 34).expect("extensions frame");
    let text = frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(text.contains("Extensions"));
    assert!(text.contains("Browse"));
    assert!(text.contains("Installed"));
    assert!(text.contains("Agents"));
    assert!(text.contains("Workflow"));
    assert!(text.contains("Collections"));
    assert!(text.contains("requested capabilities: host.document.render"));
    assert!(text.contains("constrained WASI components"));
    assert!(!state.hits.extensions_rows.is_empty());
}

#[test]
fn install_requires_host_confirmation_before_endpoint_mutation() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.overlay = Some(ClientShellOverlay::Extensions(ClientExtensionsOverlay {
        generation: 1,
        tab: ClientExtensionsTab::Browse,
        query: String::new(),
        search_focused: false,
        category: None,
        selected: 0,
        extensions: vec![extension_info(false)],
        loading: false,
        mutating: false,
        confirmation: None,
        confirmation_extension: None,
        repository_context: None,
        message: None,
    }));
    let mut outcome = ClientShellInput::default();

    state.activate_selected_extension(&mut outcome);
    assert!(outcome.actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Extensions(ClientExtensionsOverlay {
            confirmation: Some(ClientExtensionConfirmation::Install),
            ..
        }))
    ));

    state.confirm_extension_action(&mut outcome);
    assert!(outcome.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                request.method,
                crate::api::schema::Method::ExtensionInstall(
                    crate::api::schema::ExtensionConfirmedParams { confirmed: true, .. }
                )
            )
    )));
}

#[test]
fn confirmation_remains_bound_to_the_original_extension() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let first = extension_info(false);
    let mut second = extension_info(false);
    second.id = "vqro.spaces".into();
    second.repository = "ttenneb/vqro-extension-spaces".into();
    second.name = "Spaces".into();
    state.overlay = Some(ClientShellOverlay::Extensions(ClientExtensionsOverlay {
        generation: 1,
        tab: ClientExtensionsTab::Browse,
        query: String::new(),
        search_focused: false,
        category: None,
        selected: 0,
        extensions: vec![first.clone(), second],
        loading: false,
        mutating: false,
        confirmation: None,
        confirmation_extension: None,
        repository_context: None,
        message: None,
    }));
    let mut outcome = ClientShellInput::default();

    state.activate_selected_extension(&mut outcome);
    if let Some(ClientShellOverlay::Extensions(extensions)) = state.overlay.as_mut() {
        extensions.selected = 1;
    }
    state.confirm_extension_action(&mut outcome);

    assert!(outcome.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                crate::api::schema::Method::ExtensionInstall(params)
                    if params.repository == first.repository
            )
    )));
}
