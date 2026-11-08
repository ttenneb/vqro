//! Neutral view affordances. Only explicit input creates intent; rendering never dispatches.
use super::*;
use crate::view_action::{Affordance, PrepareParams, SubmitParams};

mod dialog;
#[cfg(all(test, not(windows)))]
mod tests;
pub(in crate::client::shell) use dialog::render_dialog;

#[derive(Debug, Clone)]
pub(in crate::client::shell) struct Intent {
    pub selection: PrepareParams,
    pub affordance: Affordance,
}
#[derive(Debug, Clone)]
pub(in crate::client::shell) enum Phase {
    Choosing,
    Preparing,
    Confirm(SubmitParams),
    Submitting,
    Outcome(String),
}
#[derive(Debug, Clone)]
pub(in crate::client::shell) struct Dialog {
    pub intent: Intent,
    pub phase: Phase,
    pub scroll: usize,
    pub highlighted: Option<usize>,
}

/// Borrow just presentation scalars in the pane-scaled drawing loop. Full,
/// bounded parsing and opaque-subject cloning happen only on explicit input.
pub(super) fn action_label(item: &HostDocumentItem) -> Option<&str> {
    if item.flags.disabled {
        return None;
    }
    let value = item.view.as_ref()?.affordance.as_ref()?;
    if !matches!(value.get("kind")?.as_str()?, "action" | "choice") {
        return None;
    }
    let label = value.get("label")?.as_str()?;
    (!label.is_empty() && label.len() <= 128 && !label.chars().any(char::is_control))
        .then_some(label)
}
pub(super) fn action_rect(row: Rect, label: &str) -> Rect {
    let width = display_width(label)
        .min(20)
        .saturating_add(2)
        .min(row.width / 2);
    if width < 4 {
        return Rect::default();
    }
    Rect::new(row.right().saturating_sub(width), row.y, width, 1)
}
pub(super) fn draw_action(buffer: &mut Buffer, row: Rect, label: &str, palette: &Palette) {
    let area = action_rect(row, label);
    if !area.is_empty() {
        let style = Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD);
        put_text(buffer, area.x, area.y, 1, "[", style);
        put_text(
            buffer,
            area.x + 1,
            area.y,
            area.width.saturating_sub(2),
            label,
            style,
        );
        put_text(
            buffer,
            area.right().saturating_sub(1),
            area.y,
            1,
            "]",
            style,
        );
    }
}

impl ClientShellState {
    /// Expanded-body input only; collapse/header, overlay and selection leases
    /// have already intercepted their events in the ordinary mouse path.
    pub(in crate::client::shell) fn handle_view_action_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};
        if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
            return false;
        }
        let point = (mouse.column, mouse.row);
        let Some(pane) = self
            .hits
            .panes
            .iter()
            .find(|pane| super::super::contains(pane.inner_rect, point))
        else {
            return false;
        };
        let Some(view) = self
            .pane_surface_views
            .iter()
            .find(|view| view.pane_id == pane.pane_id)
        else {
            return false;
        };
        let Some(documents) = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .and_then(ClientShellEndpoint::current_host_documents)
        else {
            return false;
        };
        let index = PanelIndex::with_rows(documents, true);
        let projection = ViewPaneProjection {
            extension: &view.extension,
            view: &view.view,
            scope: &view.scope,
        };
        let views = view_sections(&index, documents, &projection);
        let body = pane.inner_rect.intersection(pane.rect);
        let mut y = body.y;
        let mut found = None;
        'views: for section in views {
            for row in section.rows {
                if y >= body.bottom() {
                    break 'views;
                }
                let rect = Rect::new(body.x, y, body.width, 1);
                y = y.saturating_add(1);
                let Some(label) = action_label(row.item) else {
                    continue;
                };
                if !super::super::contains(action_rect(rect, label), point) {
                    continue;
                }
                let document = &documents.documents[section.document_index].document;
                let Some(value) = row
                    .item
                    .view
                    .as_ref()
                    .and_then(|view| view.affordance.as_ref())
                else {
                    continue;
                };
                found = Some((
                    PrepareParams {
                        namespace: document.namespace.clone(),
                        scope_id: "session".into(),
                        pane_id: section.root_pane.to_owned(),
                        consumer_pane_id: pane.pane_id.clone(),
                        terminal_scope: view.scope.clone(),
                        generation: document.generation,
                        revision: document.revision,
                        item_id: row.item.id.clone(),
                        option: None,
                        operation_id: String::new(),
                    },
                    Affordance::parse(value),
                ));
                break 'views;
            }
        }
        let Some((mut selection, affordance)) = found else {
            return false;
        };
        let composed =
            self.view_action_composed
                .as_ref()
                .is_some_and(|(boot, revision, surface)| {
                    boot == &documents.boot_id
                        && *revision == documents.revision
                        && self
                            .pane_surface
                            .as_ref()
                            .is_some_and(|frame| frame.projection_revision == *surface)
                });
        if !composed {
            self.set_endpoint_error(
                "View changed before this click; refresh and choose again. Nothing was submitted.",
            );
            // Recompose cached current publications; never replace and retry intent.
            outcome.repaint = true;
            return true;
        }
        let affordance = match affordance {
            Ok(value) => value,
            Err(message) => {
                self.set_endpoint_error(message);
                outcome.repaint = true;
                return true;
            }
        };
        // This is platform policy, not an OS API or a client identity claim.
        if cfg!(windows) {
            self.set_endpoint_error(
                "View actions are read-only on Windows; use the publishing extension's CLI instead.",
            );
            outcome.repaint = true;
            return true;
        }
        let nonce = match crate::view_action::token() {
            Ok(value) => value,
            Err(message) => {
                self.set_endpoint_error(message);
                outcome.repaint = true;
                return true;
            }
        };
        selection.operation_id = nonce;
        let intent = Intent {
            selection,
            affordance,
        };
        if !self.supports_endpoint_method(&crate::api::schema::Method::HostViewActionPrepare(
            intent.selection.clone(),
        )) || !self.supports_endpoint_method(&crate::api::schema::Method::HostViewActionSubmit(
            SubmitParams {
                token: String::new(),
                confirmation_token: None,
                cancel: false,
            },
        )) {
            self.set_endpoint_error("This server has no negotiated view actions; use the publishing extension's CLI instead.");
            outcome.repaint = true;
            return true;
        }
        let choice = matches!(intent.affordance, Affordance::Choice { .. });
        self.hits.view_action_popup = Rect::default();
        self.overlay = Some(ClientShellOverlay::ViewAction(Dialog {
            intent: intent.clone(),
            phase: Phase::Choosing,
            scroll: 0,
            highlighted: None,
        }));
        if !choice {
            self.prepare_view_action(intent, outcome);
        }
        outcome.repaint = true;
        true
    }

    fn view_action_intent_current(&self, intent: &Intent) -> bool {
        self.pane_surface_views.iter().any(|view| {
            view.pane_id == intent.selection.consumer_pane_id
                && view.extension == intent.selection.namespace
                && view.scope == intent.selection.terminal_scope
        }) && self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .and_then(ClientShellEndpoint::current_host_documents)
            .is_some_and(|documents| {
                documents.documents.iter().any(|entry| {
                    entry.document.namespace == intent.selection.namespace
                        && entry.document.generation == intent.selection.generation
                        && entry.document.revision == intent.selection.revision
                })
            })
    }
    fn prepare_view_action(&mut self, intent: Intent, outcome: &mut ClientShellInput) {
        if !self.view_action_intent_current(&intent) {
            self.overlay = None;
            self.set_endpoint_error(
                "View changed; refresh and choose again. Nothing was submitted.",
            );
            outcome.repaint = true;
            return;
        }
        self.hits.view_action_popup = Rect::default();
        if let Some(ClientShellOverlay::ViewAction(dialog)) = &mut self.overlay {
            dialog.intent = intent.clone();
            dialog.phase = Phase::Preparing;
        }
        let queued = self.push_endpoint_method_with_kind(
            crate::api::schema::Method::HostViewActionPrepare(intent.selection.clone()),
            PendingEndpointKind::ViewAction {
                intent: Box::new(intent),
                submitting: false,
            },
            outcome,
        );
        if !queued {
            if let Some(ClientShellOverlay::ViewAction(dialog)) = &mut self.overlay {
                dialog.phase = Phase::Outcome(
                    "Preparation unavailable; nothing was submitted. Refresh and choose again."
                        .into(),
                );
            }
        }
    }
    fn submit_view_action(
        &mut self,
        intent: Intent,
        params: SubmitParams,
        outcome: &mut ClientShellInput,
    ) {
        if !params.cancel && !self.view_action_intent_current(&intent) {
            self.cancel_view_action_token(params, outcome);
            self.overlay = None;
            self.set_endpoint_error(
                "View changed after preparation; refresh and choose again. Nothing was submitted.",
            );
            outcome.repaint = true;
            return;
        }
        self.hits.view_action_popup = Rect::default();
        if let Some(ClientShellOverlay::ViewAction(dialog)) = &mut self.overlay {
            dialog.phase = Phase::Submitting;
        }
        let queued = self.push_endpoint_method_with_kind(
            crate::api::schema::Method::HostViewActionSubmit(params),
            PendingEndpointKind::ViewAction {
                intent: Box::new(intent),
                submitting: true,
            },
            outcome,
        );
        if !queued {
            if let Some(ClientShellOverlay::ViewAction(dialog)) = &mut self.overlay {
                dialog.phase = Phase::Outcome(
                    "Submission was not queued. Refresh and choose again; no automatic retry."
                        .into(),
                );
            }
        }
    }
    fn cancel_view_action_token(
        &mut self,
        mut params: SubmitParams,
        outcome: &mut ClientShellInput,
    ) {
        params.cancel = true;
        self.push_endpoint_method(
            crate::api::schema::Method::HostViewActionSubmit(params),
            outcome,
        );
    }
    pub(in crate::client::shell) fn cancel_view_action_dialog(
        &mut self,
        outcome: &mut ClientShellInput,
    ) {
        if let Some(ClientShellOverlay::ViewAction(dialog)) = self.overlay.take() {
            if let Phase::Confirm(params) = dialog.phase {
                self.cancel_view_action_token(params, outcome);
            }
            // Preparing replies are discarded/canceled on arrival. Submitting
            // is not canceled: its pending result keeps the original operation.
            outcome.repaint = true;
        }
    }
    pub(in crate::client::shell) fn complete_view_action(
        &mut self,
        intent: Intent,
        submitting: bool,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
        outcome: &mut ClientShellInput,
    ) {
        let matching = matches!(self.overlay.as_ref(),Some(ClientShellOverlay::ViewAction(dialog)) if dialog.intent.selection.operation_id==intent.selection.operation_id);
        match result {
            Ok(crate::api::schema::ResponseResult::HostViewActionPrepared {
                token,
                confirmation_token,
                requires_confirmation,
                operation_id,
                ..
            }) if !submitting && operation_id == intent.selection.operation_id => {
                let params = SubmitParams {
                    token,
                    confirmation_token,
                    cancel: false,
                };
                if !matching {
                    self.cancel_view_action_token(params, outcome);
                    return;
                }
                let expected = intent.affordance.select(intent.selection.option.as_deref());
                if expected != Ok(requires_confirmation)
                    || requires_confirmation != params.confirmation_token.is_some()
                {
                    self.cancel_view_action_token(params, outcome);
                    self.overlay = None;
                    self.set_endpoint_error(
                        "Preparation did not match the displayed choice; nothing submitted.",
                    );
                    return;
                }
                if requires_confirmation {
                    self.hits.view_action_popup = Rect::default();
                    if let Some(ClientShellOverlay::ViewAction(dialog)) = &mut self.overlay {
                        dialog.phase = Phase::Confirm(params);
                        dialog.scroll = 0;
                    }
                } else {
                    self.submit_view_action(intent, params, outcome);
                }
            }
            Ok(crate::api::schema::ResponseResult::HostViewActionSubmitted {
                operation_id,
                provider_response,
                audit_warning,
                ..
            }) if submitting && operation_id == intent.selection.operation_id => {
                let unknown = provider_response.get("error").is_some();
                let title = if unknown {
                    "Inspect original operation"
                } else {
                    "View action replied"
                };
                let message = format!(
                    "Operation {operation_id}: {provider_response}. {}{}",
                    if unknown {
                        "A provider error does not prove no effect. Do not blindly retry. "
                    } else {
                        ""
                    },
                    audit_warning.unwrap_or_default()
                );
                if matching {
                    if let Some(ClientShellOverlay::ViewAction(dialog)) = &mut self.overlay {
                        dialog.phase = Phase::Outcome(message);
                        dialog.scroll = 0;
                    }
                } else {
                    self.push_endpoint_notice(
                        ClientEndpointNoticeKind::Rejected,
                        format!("view-action:{operation_id}"),
                        title,
                        message,
                    );
                }
            }
            Err(error) => {
                let message = if submitting {
                    format!("Outcome not confirmed for operation {}: {}. Inspect that original operation; do not blindly retry.",intent.selection.operation_id,error.message)
                } else {
                    format!("Action not prepared: {}. Refresh this view and choose again; no submission was sent.",error.message)
                };
                if matching {
                    if let Some(ClientShellOverlay::ViewAction(dialog)) = &mut self.overlay {
                        dialog.phase = Phase::Outcome(message);
                        dialog.scroll = 0;
                    }
                } else {
                    self.push_endpoint_notice(
                        ClientEndpointNoticeKind::Rejected,
                        format!("view-action:{}", intent.selection.operation_id),
                        if submitting {
                            "Inspect original operation"
                        } else {
                            "Refresh view"
                        },
                        message,
                    );
                }
            }
            _ => {
                if matching {
                    self.overlay = None;
                }
                self.set_endpoint_error(format!("Unexpected view action reply. Inspect original operation {}; do not retry blindly.",intent.selection.operation_id));
            }
        }
        outcome.repaint = true;
    }
    pub(in crate::client::shell) fn handle_view_action_dialog_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crossterm::event::{KeyCode, KeyEventKind};
        if !matches!(self.overlay, Some(ClientShellOverlay::ViewAction(_))) {
            return false;
        }
        if key.kind != KeyEventKind::Press {
            return true;
        }
        if key.code == KeyCode::Esc {
            self.cancel_view_action_dialog(outcome);
            return true;
        }
        let Some(ClientShellOverlay::ViewAction(dialog)) = &mut self.overlay else {
            return true;
        };
        match key.code {
            KeyCode::PageDown => dialog.scroll = dialog.scroll.saturating_add(8),
            KeyCode::PageUp => dialog.scroll = dialog.scroll.saturating_sub(8),
            KeyCode::Down | KeyCode::Up if matches!(dialog.phase, Phase::Choosing) => {
                if let Affordance::Choice { options, .. } = &dialog.intent.affordance {
                    dialog.highlighted = Some(match (dialog.highlighted, key.code) {
                        (Some(index), KeyCode::Down) => (index + 1) % options.len(),
                        (Some(index), _) => index.checked_sub(1).unwrap_or(options.len() - 1),
                        _ => 0,
                    });
                }
            }
            KeyCode::Enter => {
                self.activate_view_action_dialog(None, outcome);
                return true;
            }
            _ => {}
        }
        outcome.repaint = true;
        true
    }
    fn activate_view_action_dialog(
        &mut self,
        option: Option<usize>,
        outcome: &mut ClientShellInput,
    ) {
        if self.hits.view_action_popup.is_empty() {
            return;
        }
        let Some(ClientShellOverlay::ViewAction(dialog)) = self.overlay.as_ref() else {
            return;
        };
        match &dialog.phase {
            Phase::Choosing => {
                let Affordance::Choice { options, .. } = &dialog.intent.affordance else {
                    return;
                };
                let Some(choice) = option
                    .or(dialog.highlighted)
                    .and_then(|index| options.get(index))
                else {
                    return;
                };
                let mut intent = dialog.intent.clone();
                intent.selection.option = Some(choice.key.clone());
                self.prepare_view_action(intent, outcome);
            }
            Phase::Confirm(params) => {
                self.submit_view_action(dialog.intent.clone(), params.clone(), outcome)
            }
            _ => {}
        }
        outcome.repaint = true;
    }
    pub(in crate::client::shell) fn handle_view_action_dialog_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};
        let Some(ClientShellOverlay::ViewAction(dialog)) = self.overlay.as_ref() else {
            return false;
        };
        let layout = dialog::layout(dialog, self.hits.view_action_popup);
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                if let Some(ClientShellOverlay::ViewAction(dialog)) = &mut self.overlay {
                    dialog.scroll = dialog.scroll.saturating_add(3).min(layout.max_scroll);
                }
            }
            MouseEventKind::ScrollUp => {
                if let Some(ClientShellOverlay::ViewAction(dialog)) = &mut self.overlay {
                    dialog.scroll = dialog.scroll.saturating_sub(3);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let point = (mouse.column, mouse.row);
                if super::super::contains(self.hits.overlay_cancel, point) {
                    self.cancel_view_action_dialog(outcome);
                } else if matches!(dialog.phase, Phase::Confirm(_))
                    && super::super::contains(self.hits.overlay_primary, point)
                {
                    self.activate_view_action_dialog(None, outcome);
                } else if matches!(dialog.phase, Phase::Choosing) {
                    if let Some((_, index)) = layout
                        .options
                        .iter()
                        .find(|(rect, _)| super::super::contains(*rect, point))
                    {
                        self.activate_view_action_dialog(Some(*index), outcome);
                    }
                }
            }
            _ => {}
        }
        outcome.repaint = true;
        true
    }
}
