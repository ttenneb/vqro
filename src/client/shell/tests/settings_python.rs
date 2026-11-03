use super::*;

fn setup_state() -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state.open_settings_overlay();
    state
}

fn generation(state: &ClientShellState) -> u64 {
    match state.overlay.as_ref() {
        Some(ClientShellOverlay::Settings(settings)) => settings.tasks.generation,
        _ => panic!("settings overlay"),
    }
}

fn status(path: Option<&str>, confirmed: bool) -> crate::extension_interpreter::InterpreterStatus {
    crate::extension_interpreter::InterpreterStatus {
        path: path.map(str::to_owned),
        confirmed,
        message: if confirmed {
            "Python path confirmed.".into()
        } else {
            "Install CPython 3.9+ from python.org or set tasks_python; confirm in Settings → Tasks."
                .into()
        },
    }
}

fn reply(
    state: &mut ClientShellState,
    kind: PendingEndpointKind,
    interpreter: Option<crate::extension_interpreter::InterpreterStatus>,
) {
    state.handle_settings_interpreter_result(
        kind,
        Ok(crate::api::schema::ResponseResult::ExtensionList {
            extensions: Vec::new(),
            interpreter,
        }),
    );
}

fn ready(path: Option<&str>, confirmed: bool) -> ClientShellState {
    let mut state = setup_state();
    state.select_settings_section(
        ClientSettingsSection::Tasks,
        &mut ClientShellInput::default(),
    );
    let kind = PendingEndpointKind::SettingsInterpreterStatus(generation(&state));
    reply(&mut state, kind, Some(status(path, confirmed)));
    state
}

fn text(state: &mut ClientShellState, width: u16, height: u16) -> String {
    state
        .compose(width, height)
        .unwrap()
        .cells
        .iter()
        .map(|cell| cell.symbol.as_str())
        .collect()
}

fn click(state: &mut ClientShellState, rect: Rect, outcome: &mut ClientShellInput) {
    assert!(!rect.is_empty());
    state.handle_mouse(
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: crossterm::event::KeyModifiers::NONE,
        },
        outcome,
    );
}

fn confirmations(outcome: &ClientShellInput) -> usize {
    outcome.actions.iter().filter(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. } if matches!(request.method, crate::api::schema::Method::ExtensionInterpreterConfirm(_))
    )).count()
}

#[test]
fn tasks_setup_is_a_mouse_accessible_settings_tab_with_reserved_content_and_buttons() {
    let mut state = setup_state();
    let mut outcome = ClientShellInput::default();
    text(&mut state, 110, 34);
    let tab = state
        .hits
        .settings_tabs
        .iter()
        .find(|(_, section)| *section == ClientSettingsSection::Tasks)
        .unwrap()
        .0;
    click(&mut state, tab, &mut outcome);
    assert!(outcome.actions.iter().any(|action| matches!(action, ClientShellAction::Endpoint { request, .. } if matches!(request.method, crate::api::schema::Method::ExtensionList(_)))));
    let gen = generation(&state);
    reply(
        &mut state,
        PendingEndpointKind::SettingsInterpreterStatus(gen),
        Some(status(Some("C:\\Python313\\python.exe"), false)),
    );
    let content = text(&mut state, 110, 34);
    assert!(content.contains("Tasks / Python setup"));
    assert!(content.contains("C:\\Python313\\python.exe"));
    let popup = state.hits.settings_popup;
    let action = state.hits.overlay_primary;
    assert!(popup.contains((action.x, action.y).into()));
    assert!(action.y > popup.y + 5);
    assert!(
        action.x < popup.x + popup.width / 2,
        "ordinary Settings button row, not a corner adornment"
    );
    assert!(content.contains("review path"));
}

#[test]
fn settings_confirmation_captures_literal_path_and_blocks_double_submit_before_draw() {
    let path = "C:\\Users\\A  B\\Python313\\python.exe";
    let mut state = ready(Some(path), false);
    text(&mut state, 110, 34);
    let mut outcome = ClientShellInput::default();
    state.apply_settings_choice(&mut outcome);
    state.apply_settings_choice(&mut outcome);
    assert_eq!(
        confirmations(&outcome),
        0,
        "must draw the complete captured path before approval"
    );
    assert!(text(&mut state, 110, 34).contains(path));
    let gen = generation(&state);
    reply(
        &mut state,
        PendingEndpointKind::SettingsInterpreterStatus(gen),
        Some(status(Some("C:\\replacement\\python.exe"), false)),
    );
    let content = text(&mut state, 110, 34);
    assert!(content.contains(path));
    assert!(!content.contains("C:\\replacement\\python.exe"));
    let button = state.hits.overlay_primary;
    click(&mut state, button, &mut outcome);
    state.apply_settings_choice(&mut outcome);
    assert_eq!(confirmations(&outcome), 1);
    assert!(outcome.actions.iter().any(|action| matches!(action,
        ClientShellAction::Endpoint { request, .. } if matches!(&request.method,
            crate::api::schema::Method::ExtensionInterpreterConfirm(params) if params.confirmed && params.expected_path == path))));
    reply(
        &mut state,
        PendingEndpointKind::SettingsInterpreterConfirm(gen),
        Some(status(Some(path), true)),
    );
    assert!(text(&mut state, 110, 34).contains("refresh"));
    assert!(
        matches!(state.overlay.as_ref(), Some(ClientShellOverlay::Settings(settings)) if !settings.tasks.confirming && settings.tasks.interpreter.as_ref().unwrap().confirmed)
    );
    state.apply_settings_choice(&mut outcome);
    assert_eq!(
        confirmations(&outcome),
        1,
        "a confirmed path is not offered another approval"
    );
}

#[test]
fn settings_cancel_only_discards_confirmation_and_keeps_other_tabs_usable() {
    let mut state = ready(Some("C:\\Python313\\python.exe"), false);
    let mut outcome = ClientShellInput::default();
    state.apply_settings_choice(&mut outcome);
    text(&mut state, 110, 34);
    let cancel = state.hits.overlay_cancel;
    click(&mut state, cancel, &mut outcome);
    assert_eq!(confirmations(&outcome), 0);
    assert!(
        matches!(state.overlay.as_ref(), Some(ClientShellOverlay::Settings(settings)) if !settings.tasks.confirming && settings.tasks.confirmation_path.is_none())
    );
    state.select_settings_section(ClientSettingsSection::Sound, &mut outcome);
    assert!(text(&mut state, 110, 34).contains("sound alerts"));
    state.cancel_settings_overlay();
    assert!(state.overlay.is_none());
}

#[test]
fn missing_and_clipped_paths_never_offer_confirmation_in_settings() {
    let mut missing = ready(None, false);
    let content = text(&mut missing, 110, 34);
    assert!(content.contains("python.org"));
    let mut outcome = ClientShellInput::default();
    missing.apply_settings_choice(&mut outcome);
    assert_eq!(confirmations(&outcome), 0);
    assert!(outcome.actions.iter().any(|action| matches!(action, ClientShellAction::Endpoint { request, .. } if matches!(request.method, crate::api::schema::Method::ExtensionList(_)))));

    let path = format!("C:\\{}\\python.exe", "x".repeat(600));
    let mut state = ready(Some(&path), false);
    let mut outcome = ClientShellInput::default();
    state.apply_settings_choice(&mut outcome);
    text(&mut state, 110, 34);
    assert!(!state.hits.overlay_primary.is_empty());
    text(&mut state, 70, 18);
    assert!(state.hits.overlay_primary.is_empty());
    state.apply_settings_choice(&mut outcome);
    assert_eq!(confirmations(&outcome), 0);
    text(&mut state, 110, 34);
    assert!(!state.hits.overlay_primary.is_empty());
}

#[test]
fn stale_settings_replies_cannot_mutate_a_reopened_overlay() {
    let mut state = setup_state();
    state.select_settings_section(
        ClientSettingsSection::Tasks,
        &mut ClientShellInput::default(),
    );
    let old = generation(&state);
    state.cancel_settings_overlay();
    state.open_settings_overlay();
    state.select_settings_section(
        ClientSettingsSection::Tasks,
        &mut ClientShellInput::default(),
    );
    let new = generation(&state);
    assert_ne!(old, new);
    reply(
        &mut state,
        PendingEndpointKind::SettingsInterpreterStatus(old),
        Some(status(Some("C:\\old\\python.exe"), true)),
    );
    assert!(
        matches!(state.overlay.as_ref(), Some(ClientShellOverlay::Settings(settings)) if settings.tasks.pending && settings.tasks.interpreter.is_none())
    );
    reply(
        &mut state,
        PendingEndpointKind::SettingsInterpreterStatus(new),
        Some(status(Some("C:\\current\\python.exe"), false)),
    );
    assert!(text(&mut state, 110, 34).contains("C:\\current\\python.exe"));
    reply(
        &mut state,
        PendingEndpointKind::SettingsInterpreterConfirm(old),
        Some(status(Some("C:\\old\\python.exe"), true)),
    );
    assert!(text(&mut state, 110, 34).contains("C:\\current\\python.exe"));
}

#[test]
fn confirmed_or_unsupported_settings_status_does_not_reprompt_or_break_other_tabs() {
    let mut state = ready(Some("C:\\Python313\\python.exe"), true);
    assert!(text(&mut state, 110, 34).contains("refresh"));
    let mut outcome = ClientShellInput::default();
    state.apply_settings_choice(&mut outcome);
    assert_eq!(confirmations(&outcome), 0);
    let gen = generation(&state);
    reply(
        &mut state,
        PendingEndpointKind::SettingsInterpreterStatus(gen),
        None,
    );
    assert!(text(&mut state, 110, 34).contains("did not provide"));
    state.select_settings_section(
        ClientSettingsSection::Indicators,
        &mut ClientShellInput::default(),
    );
    assert!(text(&mut state, 110, 34).contains("agent status indicators"));
}

#[test]
fn settings_confirmation_reply_must_match_the_captured_approved_path() {
    for returned in [
        None,
        Some(status(Some("C:\\different\\python.exe"), true)),
        Some(status(Some("C:\\Python313\\python.exe"), false)),
    ] {
        let mut state = ready(Some("C:\\Python313\\python.exe"), false);
        state.apply_settings_choice(&mut ClientShellInput::default());
        text(&mut state, 110, 34);
        state.apply_settings_choice(&mut ClientShellInput::default());
        let gen = generation(&state);
        reply(
            &mut state,
            PendingEndpointKind::SettingsInterpreterConfirm(gen),
            returned,
        );
        assert!(
            matches!(state.overlay.as_ref(), Some(ClientShellOverlay::Settings(settings)) if settings.tasks.message.as_deref().unwrap().contains("did not confirm"))
        );
    }
}

#[test]
fn settings_confirmation_error_requires_fresh_status_instead_of_replaying_approval() {
    let mut state = ready(Some("C:\\Python313\\python.exe"), false);
    state.apply_settings_choice(&mut ClientShellInput::default());
    text(&mut state, 110, 34);
    state.apply_settings_choice(&mut ClientShellInput::default());
    let gen = generation(&state);
    state.handle_settings_interpreter_result(
        PendingEndpointKind::SettingsInterpreterConfirm(gen),
        Err(ClientShellEndpointError {
            code: Some("interpreter_confirmation_failed".into()),
            message: "Candidate changed; retry in Settings → Tasks.".into(),
        }),
    );
    assert!(text(&mut state, 110, 34).contains("Candidate changed"));
    let mut outcome = ClientShellInput::default();
    state.apply_settings_choice(&mut outcome);
    assert_eq!(confirmations(&outcome), 0);
    assert!(outcome.actions.iter().any(|action| matches!(action, ClientShellAction::Endpoint { request, .. } if matches!(request.method, crate::api::schema::Method::ExtensionList(_)))));
}
