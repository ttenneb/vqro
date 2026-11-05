use super::*;
use crossterm::event::{KeyCode, KeyModifiers};

pub(super) fn normalized_theme_name(name: &str) -> String {
    name.to_lowercase().replace([' ', '_'], "-")
}

fn theme_index(name: &str) -> usize {
    let normalized = normalized_theme_name(name);
    crate::config::THEME_NAMES
        .iter()
        .position(|candidate| normalized_theme_name(candidate) == normalized)
        .unwrap_or(0)
}

fn indicator_index(style: crate::config::StatusIndicatorStyle) -> usize {
    usize::from(style == crate::config::StatusIndicatorStyle::Symbols)
}

fn toast_index(delivery: crate::config::ToastDelivery) -> usize {
    match delivery {
        crate::config::ToastDelivery::Off => 0,
        crate::config::ToastDelivery::Herdr => 1,
        crate::config::ToastDelivery::Terminal => 2,
        crate::config::ToastDelivery::System => 3,
    }
}

impl ClientShellState {
    pub(super) fn open_settings_overlay(&mut self) {
        self.overlay = Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
            section: ClientSettingsSection::Theme,
            selected: theme_index(&self.config.theme_name),
            original_theme_name: self.config.theme_name.clone(),
            original_palette: self.config.palette.clone(),
            tasks: ClientTasksSettings {
                generation: self.next_request_id,
                ..Default::default()
            },
        }));
    }

    pub(super) fn settings_python_mutating(&self) -> bool {
        matches!(self.overlay.as_ref(), Some(ClientShellOverlay::Settings(settings)) if settings.tasks.confirming && settings.tasks.pending)
    }

    pub(super) fn cancel_settings_action(&mut self, outcome: &mut ClientShellInput) {
        if self.settings_python_mutating() {
            return;
        }
        if let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() {
            if settings.tasks.confirming {
                settings.tasks.confirming = false;
                settings.tasks.confirmation_path = None;
                outcome.repaint = true;
                return;
            }
        }
        self.cancel_settings_overlay();
        outcome.repaint = true;
    }

    fn queue_settings_interpreter(
        &mut self,
        method: crate::api::schema::Method,
        kind: PendingEndpointKind,
        outcome: &mut ClientShellInput,
    ) {
        if let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() {
            settings.tasks.pending = true;
            settings.tasks.message = None;
        }
        if !self.push_endpoint_method_with_kind(method, kind, outcome) {
            if let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() {
                settings.tasks.pending = false;
                settings.tasks.confirming = false;
                settings.tasks.confirmation_path = None;
                settings.tasks.message = Some(
                    "This server does not support Python setup. Update the host and retry.".into(),
                );
            }
        }
        outcome.repaint = true;
    }

    fn request_settings_interpreter(&mut self, outcome: &mut ClientShellInput) {
        let generation = match self.overlay.as_mut() {
            Some(ClientShellOverlay::Settings(settings)) if !settings.tasks.pending => {
                settings.tasks.loaded = false;
                settings.tasks.interpreter = None;
                settings.tasks.confirming = false;
                settings.tasks.confirmation_path = None;
                settings.tasks.generation
            }
            _ => return,
        };
        self.queue_settings_interpreter(
            crate::api::schema::Method::ExtensionList(
                crate::api::schema::ExtensionListParams::default(),
            ),
            PendingEndpointKind::SettingsInterpreterStatus(generation),
            outcome,
        );
    }

    fn apply_tasks_python_choice(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() else {
            return;
        };
        if settings.tasks.pending {
            return;
        }
        if settings.tasks.confirming {
            // A new confirmation invalidates old hit geometry until the full
            // captured path has actually been drawn, including after resize.
            if self.hits.overlay_primary.is_empty() {
                return;
            }
            let Some(path) = settings.tasks.confirmation_path.clone() else {
                return;
            };
            let generation = settings.tasks.generation;
            self.queue_settings_interpreter(
                crate::api::schema::Method::ExtensionInterpreterConfirm(
                    crate::api::schema::ExtensionInterpreterConfirmParams {
                        expected_path: path,
                        confirmed: true,
                    },
                ),
                PendingEndpointKind::SettingsInterpreterConfirm(generation),
                outcome,
            );
        } else if let Some(status) = settings
            .tasks
            .interpreter
            .as_ref()
            .filter(|status| !status.confirmed && status.path.is_some())
        {
            settings.tasks.confirmation_path = status.path.clone();
            settings.tasks.confirming = true;
            self.hits.overlay_primary = Rect::default();
            outcome.repaint = true;
        } else {
            self.request_settings_interpreter(outcome);
        }
    }

    pub(super) fn handle_settings_interpreter_result(
        &mut self,
        kind: PendingEndpointKind,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let (generation, confirming) = match kind {
            PendingEndpointKind::SettingsInterpreterStatus(generation) => (generation, false),
            PendingEndpointKind::SettingsInterpreterConfirm(generation) => (generation, true),
            _ => return (false, Vec::new()),
        };
        let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() else {
            return (false, Vec::new());
        };
        if settings.tasks.generation != generation {
            return (false, Vec::new());
        }
        settings.tasks.pending = false;
        settings.tasks.loaded = true;
        match result {
            Ok(crate::api::schema::ResponseResult::ExtensionList { interpreter, .. }) => {
                let approved = interpreter.as_ref().is_some_and(|status| {
                    status.confirmed && status.path == settings.tasks.confirmation_path
                });
                settings.tasks.interpreter = interpreter;
                settings.tasks.message = confirming.then(|| if approved {
                    "Python path confirmed. No package capabilities were granted.".into()
                } else {
                    "The host did not confirm the captured path. Review current setup status before trying again.".into()
                });
                if confirming {
                    settings.tasks.confirming = false;
                    settings.tasks.confirmation_path = None;
                }
            }
            result => {
                settings.tasks.interpreter = None;
                settings.tasks.confirming = false;
                settings.tasks.confirmation_path = None;
                settings.tasks.message = Some(match result {
                    Err(error) => error.message,
                    Ok(_) => "Unexpected Python setup response. Retry to refresh the current path."
                        .into(),
                });
            }
        }
        (true, Vec::new())
    }

    fn selected_index_for_settings_section(&self, section: ClientSettingsSection) -> usize {
        match section {
            ClientSettingsSection::Theme => theme_index(&self.config.theme_name),
            ClientSettingsSection::Indicators => indicator_index(self.config.status_indicators),
            ClientSettingsSection::Sound => usize::from(!self.config.sound_enabled),
            ClientSettingsSection::Toast => toast_index(self.config.toast_delivery),
            ClientSettingsSection::Tasks => 0,
        }
    }

    pub(super) fn select_settings_section(
        &mut self,
        section: ClientSettingsSection,
        outcome: &mut ClientShellInput,
    ) {
        if self.settings_python_mutating() {
            return;
        }
        let selected = self.selected_index_for_settings_section(section);
        if let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() {
            if settings.section != section {
                settings.tasks.confirming = false;
                settings.tasks.confirmation_path = None;
            }
            settings.section = section;
            settings.selected = selected;
        }
        if section == ClientSettingsSection::Tasks
            && matches!(self.overlay.as_ref(), Some(ClientShellOverlay::Settings(settings)) if !settings.tasks.loaded && !settings.tasks.pending)
        {
            self.request_settings_interpreter(outcome);
        }
        outcome.repaint = true;
    }

    fn move_settings_section(&mut self, delta: isize, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_ref() else {
            return;
        };
        let current = ClientSettingsSection::ALL
            .iter()
            .position(|section| *section == settings.section)
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(ClientSettingsSection::ALL.len() as isize)
            as usize;
        self.select_settings_section(ClientSettingsSection::ALL[next], outcome);
    }

    fn settings_choice_count(&self) -> usize {
        match self.overlay.as_ref() {
            Some(ClientShellOverlay::Settings(settings)) => match settings.section {
                ClientSettingsSection::Theme => crate::config::THEME_NAMES.len(),
                ClientSettingsSection::Indicators | ClientSettingsSection::Sound => 2,
                ClientSettingsSection::Toast => 4,
                ClientSettingsSection::Tasks => 0,
            },
            _ => 0,
        }
    }

    pub(super) fn move_settings_selection(&mut self, delta: isize) {
        let count = self.settings_choice_count();
        let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() else {
            return;
        };
        if count == 0 {
            settings.selected = 0;
            return;
        }
        settings.selected = (settings.selected as isize + delta)
            .clamp(0, count.saturating_sub(1) as isize) as usize;
        if settings.section == ClientSettingsSection::Theme {
            self.preview_selected_theme();
        }
    }

    pub(super) fn select_settings_choice(&mut self, index: usize) {
        let count = self.settings_choice_count();
        if let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_mut() {
            if count > 0 {
                settings.selected = index.min(count - 1);
            }
        }
        if matches!(
            self.overlay,
            Some(ClientShellOverlay::Settings(ClientSettingsOverlay {
                section: ClientSettingsSection::Theme,
                ..
            }))
        ) {
            self.preview_selected_theme();
        }
    }

    fn preview_selected_theme(&mut self) {
        let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_ref() else {
            return;
        };
        let Some(name) = crate::config::THEME_NAMES.get(settings.selected) else {
            return;
        };
        self.config.theme_name = (*name).to_owned();
        self.config.palette =
            crate::app::client_palette_for_theme(&self.config.theme_runtime, name);
    }

    pub(super) fn cancel_settings_overlay(&mut self) {
        let Some(ClientShellOverlay::Settings(settings)) = self.overlay.take() else {
            return;
        };
        self.config.theme_name = settings.original_theme_name;
        self.config.palette = settings.original_palette;
    }

    fn save_settings_edit(
        &mut self,
        edit: crate::config::ConfigEdit<'_>,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if let Err(error) = crate::config::write_edit(edit) {
            self.set_endpoint_error(error);
            outcome.repaint = true;
            return false;
        }
        self.reload_client_config();
        self.push_endpoint_method_with_kind(
            crate::api::schema::Method::ServerReloadConfig(
                crate::api::schema::EmptyParams::default(),
            ),
            PendingEndpointKind::ReloadConfig,
            outcome,
        );
        outcome.repaint = true;
        true
    }

    pub(super) fn apply_settings_choice(&mut self, outcome: &mut ClientShellInput) {
        let Some(ClientShellOverlay::Settings(settings)) = self.overlay.as_ref() else {
            return;
        };
        let section = settings.section;
        let selected = settings.selected;
        match section {
            ClientSettingsSection::Tasks => self.apply_tasks_python_choice(outcome),
            ClientSettingsSection::Theme => {
                let Some(name) = crate::config::THEME_NAMES.get(selected).copied() else {
                    return;
                };
                if self.save_settings_edit(crate::config::ConfigEdit::Theme(name), outcome) {
                    self.overlay = None;
                }
            }
            ClientSettingsSection::Indicators => {
                let style = if selected == 0 {
                    crate::config::StatusIndicatorStyle::Dots
                } else {
                    crate::config::StatusIndicatorStyle::Symbols
                };
                self.save_settings_edit(
                    crate::config::ConfigEdit::StatusIndicators(style),
                    outcome,
                );
            }
            ClientSettingsSection::Sound => {
                self.save_settings_edit(crate::config::ConfigEdit::Sound(selected == 0), outcome);
            }
            ClientSettingsSection::Toast => {
                let delivery = match selected {
                    0 => crate::config::ToastDelivery::Off,
                    1 => crate::config::ToastDelivery::Herdr,
                    2 => crate::config::ToastDelivery::Terminal,
                    _ => crate::config::ToastDelivery::System,
                };
                self.save_settings_edit(
                    crate::config::ConfigEdit::ToastDelivery(delivery),
                    outcome,
                );
            }
        }
    }

    pub(super) fn route_settings_key(
        &mut self,
        key: &crate::input::TerminalKey,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !matches!(self.overlay, Some(ClientShellOverlay::Settings(_))) {
            return false;
        }
        if self.settings_python_mutating() {
            return true;
        }
        let (code, modifiers) = crate::config::normalize_key_combo((key.code, key.modifiers));
        if code == KeyCode::Esc {
            self.cancel_settings_action(outcome);
            return true;
        }
        if matches!(code, KeyCode::Tab | KeyCode::Right | KeyCode::Char('l'))
            && modifiers.is_empty()
        {
            self.move_settings_section(1, outcome);
            return true;
        }
        if matches!(code, KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h'))
            && modifiers.difference(KeyModifiers::SHIFT).is_empty()
        {
            self.move_settings_section(-1, outcome);
            return true;
        }
        if matches!(code, KeyCode::Up | KeyCode::Char('k')) && modifiers.is_empty() {
            self.move_settings_selection(-1);
            outcome.repaint = true;
            return true;
        }
        if matches!(code, KeyCode::Down | KeyCode::Char('j')) && modifiers.is_empty() {
            self.move_settings_selection(1);
            outcome.repaint = true;
            return true;
        }
        if matches!(code, KeyCode::Enter | KeyCode::Char(' ')) && modifiers.is_empty() {
            self.apply_settings_choice(outcome);
            return true;
        }
        true
    }
}
