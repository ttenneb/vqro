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
        provided_contracts: Vec::new(),
        required_contracts: Vec::new(),
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

fn python_setup_state(path: Option<String>) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.open_extensions_overlay(&mut ClientShellInput::default());
    if let Some(ClientShellOverlay::Extensions(extensions)) = state.overlay.as_mut() {
        extensions.loading = false;
        extensions.extensions = vec![extension_info(true)];
        extensions.interpreter = Some(crate::extension_interpreter::InterpreterStatus {
            path,
            confirmed: false,
            message: "Install CPython 3.9+ from python.org or set tasks_python, then confirm."
                .into(),
        });
    }
    state
}

#[test]
fn python_setup_mouse_confirmation_sends_exact_displayed_path() {
    let path = "C:\\Users\\A  B\\Python313\\python.exe";
    let mut state = python_setup_state(Some(path.into()));
    state.compose(110, 34).unwrap();
    let button = state.hits.extensions_python;
    assert!(!button.is_empty());
    let mut outcome = ClientShellInput::default();
    state.handle_mouse(
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: button.x,
            row: button.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        },
        &mut outcome,
    );
    let frame = state.compose(110, 34).unwrap();
    let text = frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("A  B"));
    assert!(!state.hits.overlay_primary.is_empty());
    // A refresh must not transfer the dialog to a different candidate.
    if let Some(ClientShellOverlay::Extensions(extensions)) = state.overlay.as_mut() {
        extensions.interpreter.as_mut().unwrap().path = Some("C:\\new\\python.exe".into());
    }
    let frame = state.compose(110, 34).unwrap();
    let text = frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("A  B"));
    assert!(!text.contains("C:\\new\\python.exe"));
    state.confirm_extension_action(&mut outcome);
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. } if matches!(&request.method,
            crate::api::schema::Method::ExtensionInterpreterConfirm(params) if params.confirmed && params.expected_path == path))));
}

#[test]
fn missing_or_clipped_python_cannot_be_confirmed_and_other_extensions_still_work() {
    let mut state = python_setup_state(None);
    let mut outcome = ClientShellInput::default();
    state.activate_selected_extension(&mut outcome);
    assert!(outcome.actions.iter().any(|action| matches!(action, ClientShellAction::Endpoint { request, .. } if matches!(request.method, crate::api::schema::Method::ExtensionEnable(_)))));
    let mut state = python_setup_state(None);
    state.begin_python_confirmation(&mut ClientShellInput::default());
    let frame = state.compose(110, 34).unwrap();
    let text = frame
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    assert!(text.contains("python.org"));
    assert!(state.hits.overlay_primary.is_empty());
    let mut outcome = ClientShellInput::default();
    state.confirm_extension_action(&mut outcome);
    assert!(outcome.actions.is_empty());
    let mut state = python_setup_state(Some(format!("C:\\{}\\python.exe", "x".repeat(2000))));
    state.begin_python_confirmation(&mut ClientShellInput::default());
    state.compose(110, 34).unwrap();
    assert!(state.hits.overlay_primary.is_empty());
    state.confirm_extension_action(&mut outcome);
    assert!(outcome.actions.is_empty());
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
        interpreter: None,
        interpreter_confirmation_path: None,
        loading: false,
        mutating: false,
        confirmation: None,
        confirmation_extension: None,
        repository_context: None,
        message: None,
        git_install_mode: false,
        git_inspection: None,
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
        interpreter: None,
        interpreter_confirmation_path: None,
        loading: false,
        mutating: false,
        confirmation: None,
        confirmation_extension: None,
        repository_context: None,
        message: None,
        git_install_mode: false,
        git_inspection: None,
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
        interpreter: None,
        interpreter_confirmation_path: None,
        loading: false,
        mutating: false,
        confirmation: None,
        confirmation_extension: None,
        repository_context: None,
        message: None,
        git_install_mode: false,
        git_inspection: None,
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

#[test]
fn inspected_git_package_requires_digest_bound_approval() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.overlay = Some(ClientShellOverlay::Extensions(ClientExtensionsOverlay {
        generation: 7,
        tab: ClientExtensionsTab::Browse,
        query: String::new(),
        search_focused: false,
        category: None,
        selected: 0,
        extensions: Vec::new(),
        interpreter: None,
        interpreter_confirmation_path: None,
        loading: true,
        mutating: false,
        confirmation: None,
        confirmation_extension: None,
        repository_context: None,
        message: None,
        git_install_mode: true,
        git_inspection: None,
    }));
    let inspection = crate::api::schema::ExtensionPackageInspection {
        source: crate::api::schema::ExtensionPackageSource {
            url: "https://example.test/extension.git".into(),
            commit: "a".repeat(40),
            package_path: "package.vqrox".into(),
        },
        package_id: "example.git".into(),
        version: "1.0.0".into(),
        package_sha256: "b".repeat(64),
        manifest_sha256: "c".repeat(64),
        namespaces: vec!["example.git".into()],
        services: vec!["main".into()],
        requested_capabilities: vec!["host.state.read".into()],
        provided_contracts: vec![crate::api::schema::ExtensionContractInspection {
            service_id: "main".into(),
            endpoint_id: "echo".into(),
            contract_id: "example.echo".into(),
            contract_version: "1.0.0".into(),
            descriptor_sha256: "d".repeat(64),
            required: None,
            methods: vec![crate::api::schema::ExtensionContractMethodInspection {
                id: "echo".into(),
                request_information: vec!["user.supplied.text".into()],
                response_information: vec!["provider.generated.text".into()],
                max_request_bytes: 4096,
                max_response_bytes: 4096,
            }],
        }],
        required_contracts: Vec::new(),
    };

    let (handled, actions) = state.handle_extensions_endpoint_result(
        PendingEndpointKind::ExtensionPackageInspect(7),
        Ok(
            crate::api::schema::ResponseResult::ExtensionPackageInspected {
                inspection: inspection.clone(),
            },
        ),
    );
    assert!(handled);
    assert!(actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Extensions(ClientExtensionsOverlay {
            confirmation: Some(ClientExtensionConfirmation::Install),
            ..
        }))
    ));
    let Some(ClientShellOverlay::Extensions(extensions)) = state.overlay.as_ref() else {
        unreachable!()
    };
    let confirmation = extensions.confirmation_extension.as_ref().unwrap();
    assert_eq!(
        confirmation.provided_contracts,
        inspection.provided_contracts
    );
    assert_eq!(
        confirmation.provided_contracts[0].methods[0].request_information,
        ["user.supplied.text"]
    );

    let mut outcome = ClientShellInput::default();
    state.confirm_extension_action(&mut outcome);
    assert!(outcome.actions.iter().any(|action| matches!(
        action,
        ClientShellAction::Endpoint { request, .. }
            if matches!(
                &request.method,
                crate::api::schema::Method::ExtensionPackageInstall(params)
                    if params.approved
                        && params.source == inspection.source
                        && params.expected_package_sha256 == inspection.package_sha256
                        && params.expected_manifest_sha256 == inspection.manifest_sha256
            )
    )));
}
