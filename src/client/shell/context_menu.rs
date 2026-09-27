use super::*;

impl ClientContextMenuOverlay {
    pub(super) fn items(&self) -> Vec<ClientContextMenuItem> {
        use ClientContextMenuAction as Action;

        let item = |label, action| ClientContextMenuItem { label, action };
        match &self.target {
            ClientContextMenuTarget::Workspace { is_git: false, .. } => {
                vec![item("Rename", Action::Rename), item("Close", Action::Close)]
            }
            ClientContextMenuTarget::Workspace {
                is_linked_worktree: false,
                has_worktree_children: false,
                ..
            } => vec![
                item("Rename", Action::Rename),
                item("Close", Action::Close),
                item("New worktree", Action::NewWorktree),
                item("Open worktree...", Action::OpenWorktree),
            ],
            ClientContextMenuTarget::Workspace {
                is_linked_worktree: true,
                ..
            } => vec![
                item("Rename", Action::Rename),
                item("Close", Action::Close),
                item("Delete worktree checkout...", Action::RemoveWorktree),
            ],
            ClientContextMenuTarget::Workspace {
                has_worktree_children: true,
                collapsed,
                ..
            } => vec![
                item("Rename", Action::Rename),
                item("Close group", Action::Close),
                item("New worktree", Action::NewWorktree),
                item("Open worktree...", Action::OpenWorktree),
                item(
                    if *collapsed { "Expand" } else { "Collapse" },
                    Action::ToggleGroup,
                ),
            ],
            ClientContextMenuTarget::Tab { .. } => vec![
                item("New tab", Action::NewTab),
                item("Rename", Action::Rename),
                item("Close", Action::Close),
            ],
            ClientContextMenuTarget::Collection { .. } => vec![
                item("Promote members to tabs", Action::PromoteCollectionMembers),
                item(
                    "Close collection and members...",
                    Action::CloseCollectionCascade,
                ),
            ],
            ClientContextMenuTarget::Pane {
                source_pane_id,
                has_manual_label,
                right_click_passthrough,
                collection_id,
                collection_archived,
                ..
            } => {
                let mut items = vec![
                    item("Inspect terminal...", Action::InspectTerminal),
                    item("Rename pane", Action::RenamePane),
                ];
                if *has_manual_label {
                    items.push(item("Clear pane name", Action::ClearPaneName));
                }
                if source_pane_id.is_some() {
                    items.push(item("Swap with focused pane", Action::SwapWithFocusedPane));
                }
                if collection_id.is_some() {
                    items.push(item(
                        if *collection_archived {
                            "Restore member"
                        } else {
                            "Archive member"
                        },
                        if *collection_archived {
                            Action::RestoreCollectionMember
                        } else {
                            Action::ArchiveCollectionMember
                        },
                    ));
                    items.push(item(
                        "Promote from collection",
                        Action::PromoteCollectionMember,
                    ));
                }
                items.extend([
                    item("Split right", Action::SplitRight),
                    item("Split down", Action::SplitDown),
                    item("Zoom", Action::Zoom),
                    item(
                        if *right_click_passthrough {
                            "Use vqro right-click menu"
                        } else {
                            "Send right-clicks to pane"
                        },
                        Action::ToggleRightClickPassthrough,
                    ),
                    item("Close pane", Action::ClosePane),
                ]);
                items
            }
        }
    }
}

impl ClientShellState {
    pub(super) fn open_workspace_context_menu(&mut self, workspace_id: String, x: u16, y: u16) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(workspace) = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == workspace_id)
        else {
            return;
        };
        let worktree = workspace.worktree.as_ref();
        let repository = self.structure.as_deref().and_then(|structure| {
            structure.repositories.iter().find(|repository| {
                repository
                    .checkouts
                    .iter()
                    .any(|checkout| checkout.workspace_id == workspace_id)
            })
        });
        let checkout = repository.and_then(|repository| {
            repository
                .checkouts
                .iter()
                .find(|checkout| checkout.workspace_id == workspace_id)
        });
        let repository_group_key = repository.map(|repository| {
            super::sidebar::repository_group_key(
                &self.active_endpoint_id,
                &repository.repository_id,
            )
        });
        let has_worktree_children = repository.map_or_else(
            || {
                worktree.is_some_and(|worktree| {
                    !worktree.is_linked_worktree
                        && snapshot
                            .workspaces
                            .iter()
                            .filter(|candidate| {
                                candidate
                                    .worktree
                                    .as_ref()
                                    .is_some_and(|candidate| candidate.key == worktree.key)
                            })
                            .count()
                            >= 2
                })
            },
            |repository| repository.checkouts.len() >= 2,
        );
        let collapsed = repository_group_key.as_ref().map_or_else(
            || {
                worktree.is_some_and(|worktree| {
                    self.group_is_collapsed(&self.active_endpoint_id, &worktree.key)
                })
            },
            |key| self.group_is_collapsed(&self.active_endpoint_id, key),
        );
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Workspace {
                workspace_id,
                is_git: repository.is_some() || worktree.is_some() || workspace.branch.is_some(),
                is_linked_worktree: checkout.map_or_else(
                    || worktree.is_some_and(|worktree| worktree.is_linked_worktree),
                    |checkout| {
                        checkout.kind == crate::protocol::endpoint::ClientShellCheckoutKind::Linked
                    },
                ),
                has_worktree_children,
                collapsed,
            },
            x,
            y,
            highlighted: 0,
        }));
    }

    pub(super) fn open_tab_context_menu(&mut self, tab_id: String, x: u16, y: u16) {
        let Some(tab) = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id))
        else {
            return;
        };
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Tab {
                tab_id,
                workspace_id: tab.workspace_id.clone(),
            },
            x,
            y,
            highlighted: 0,
        }));
    }

    pub(super) fn open_pane_context_menu(&mut self, pane_id: String, x: u16, y: u16) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(pane) = snapshot.panes.iter().find(|pane| pane.pane_id == pane_id) else {
            return;
        };
        let source_pane_id = snapshot
            .focused_pane_id
            .clone()
            .filter(|focused| focused != &pane_id);
        let membership = self.structure.as_deref().and_then(|structure| {
            structure.collections.iter().find_map(|collection| {
                collection
                    .members
                    .iter()
                    .find(|member| member.pane_id == pane_id)
                    .map(|member| (collection.collection_id.clone(), member.archived))
            })
        });
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Pane {
                pane_id,
                workspace_id: pane.workspace_id.clone(),
                source_pane_id,
                has_manual_label: pane.label.is_some(),
                right_click_passthrough: pane.right_click_passthrough,
                collection_id: membership
                    .as_ref()
                    .map(|(collection_id, _)| collection_id.clone()),
                collection_archived: membership.is_some_and(|(_, archived)| archived),
            },
            x,
            y,
            highlighted: 0,
        }));
    }

    pub(super) fn move_context_menu_selection(&mut self, delta: isize) {
        let Some(ClientShellOverlay::ContextMenu(menu)) = self.overlay.as_mut() else {
            return;
        };
        let item_count = menu.items().len();
        if item_count == 0 {
            return;
        }
        menu.highlighted = (menu.highlighted as isize + delta)
            .clamp(0, item_count.saturating_sub(1) as isize) as usize;
    }

    pub(super) fn activate_context_menu_item(
        &mut self,
        index: usize,
        outcome: &mut ClientShellInput,
    ) {
        let Some(ClientShellOverlay::ContextMenu(menu)) = self.overlay.take() else {
            return;
        };
        let Some(action) = menu.items().get(index).map(|item| item.action) else {
            outcome.repaint = true;
            return;
        };
        match menu.target {
            ClientContextMenuTarget::Workspace { workspace_id, .. } => {
                self.activate_workspace_context_action(workspace_id, action, outcome)
            }
            ClientContextMenuTarget::Tab {
                tab_id,
                workspace_id,
            } => self.activate_tab_context_action(tab_id, workspace_id, action, outcome),
            ClientContextMenuTarget::Collection { collection_id } => match action {
                ClientContextMenuAction::PromoteCollectionMembers => {
                    self.push_endpoint_method(
                        crate::api::schema::Method::CollectionClose(
                            crate::api::schema::CollectionCloseParams {
                                collection_id,
                                disposition: Some(
                                    crate::api::schema::CollectionCloseDisposition::PromoteMembers,
                                ),
                            },
                        ),
                        outcome,
                    );
                }
                ClientContextMenuAction::CloseCollectionCascade => {
                    let member_count = self
                        .structure
                        .as_deref()
                        .and_then(|structure| {
                            structure
                                .collections
                                .iter()
                                .find(|collection| collection.collection_id == collection_id)
                        })
                        .map_or(0, |collection| collection.members.len());
                    self.overlay = Some(ClientShellOverlay::ConfirmClose(
                        ClientConfirmCloseOverlay {
                            target: ClientConfirmCloseTarget::Collection { collection_id },
                            title: "Close collection and member terminals?".into(),
                            detail: format!(
                                "This closes {member_count} real terminal process{}.",
                                if member_count == 1 { "" } else { "es" }
                            ),
                        },
                    ));
                }
                _ => {}
            },
            ClientContextMenuTarget::Pane {
                pane_id,
                workspace_id,
                source_pane_id,
                right_click_passthrough,
                collection_id,
                collection_archived,
                ..
            } => self.activate_pane_context_action(
                pane_id,
                workspace_id,
                source_pane_id,
                right_click_passthrough,
                collection_id,
                collection_archived,
                action,
                outcome,
            ),
        }
        outcome.repaint = true;
    }

    fn activate_workspace_context_action(
        &mut self,
        workspace_id: String,
        action: ClientContextMenuAction,
        outcome: &mut ClientShellInput,
    ) {
        use crate::input::KeybindAction;

        match action {
            ClientContextMenuAction::Rename => {
                let label = self
                    .snapshot
                    .as_deref()
                    .and_then(|snapshot| {
                        snapshot
                            .workspaces
                            .iter()
                            .find(|workspace| workspace.workspace_id == workspace_id)
                    })
                    .map(|workspace| workspace.label.clone());
                if let Some(label) = label {
                    self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                        title: "rename workspace",
                        input: label,
                        replace_on_type: false,
                        target: ClientRenameTarget::Workspace { workspace_id },
                    }));
                }
            }
            ClientContextMenuAction::Close => {
                if self.config.confirm_close {
                    self.open_confirm_close_overlay(workspace_id);
                } else {
                    self.push_endpoint_method(
                        crate::api::schema::Method::WorkspaceClose(
                            crate::api::schema::WorkspaceCloseParams {
                                workspace_id,
                                close_group: true,
                            },
                        ),
                        outcome,
                    );
                }
            }
            ClientContextMenuAction::NewWorktree => {
                self.begin_worktree_action_for(KeybindAction::NewWorktree, workspace_id, outcome)
            }
            ClientContextMenuAction::OpenWorktree => {
                self.begin_worktree_action_for(KeybindAction::OpenWorktree, workspace_id, outcome)
            }
            ClientContextMenuAction::RemoveWorktree => {
                self.begin_worktree_action_for(KeybindAction::RemoveWorktree, workspace_id, outcome)
            }
            ClientContextMenuAction::ToggleGroup => {
                let key = self
                    .structure
                    .as_deref()
                    .and_then(|structure| {
                        structure.repositories.iter().find_map(|repository| {
                            repository
                                .checkouts
                                .iter()
                                .any(|checkout| checkout.workspace_id == workspace_id)
                                .then(|| {
                                    super::sidebar::repository_group_key(
                                        &self.active_endpoint_id,
                                        &repository.repository_id,
                                    )
                                })
                        })
                    })
                    .or_else(|| {
                        self.snapshot.as_deref().and_then(|snapshot| {
                            snapshot
                                .workspaces
                                .iter()
                                .find(|workspace| workspace.workspace_id == workspace_id)
                                .and_then(|workspace| workspace.worktree.as_ref())
                                .map(|worktree| worktree.key.clone())
                        })
                    });
                if let Some(key) = key {
                    let endpoint_id = self.active_endpoint_id.clone();
                    self.toggle_collapsed_group(&endpoint_id, key);
                    self.persist_chrome_preferences(outcome);
                }
            }
            _ => {}
        }
    }

    fn activate_tab_context_action(
        &mut self,
        tab_id: String,
        workspace_id: String,
        action: ClientContextMenuAction,
        outcome: &mut ClientShellInput,
    ) {
        use crate::api::schema::{Method, TabTarget};

        self.push_endpoint_method(
            Method::TabFocus(TabTarget {
                tab_id: tab_id.clone(),
            }),
            outcome,
        );
        match action {
            ClientContextMenuAction::NewTab => {
                if self.config.prompt_new_tab_name {
                    let default_name = (self
                        .snapshot
                        .as_deref()
                        .map(|snapshot| {
                            snapshot
                                .tabs
                                .iter()
                                .filter(|tab| tab.workspace_id == workspace_id)
                                .count()
                        })
                        .unwrap_or(0)
                        + 1)
                    .to_string();
                    self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                        title: "new tab",
                        input: default_name.clone(),
                        replace_on_type: true,
                        target: ClientRenameTarget::NewTab {
                            workspace_id,
                            default_name,
                        },
                    }));
                } else {
                    self.push_endpoint_method(
                        Method::TabCreate(crate::api::schema::TabCreateParams {
                            workspace_id: Some(workspace_id),
                            cwd: None,
                            focus: true,
                            label: None,
                            env: Default::default(),
                        }),
                        outcome,
                    );
                }
            }
            ClientContextMenuAction::Rename => {
                let tab = self
                    .snapshot
                    .as_deref()
                    .and_then(|snapshot| snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id));
                if let Some(tab) = tab {
                    self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                        title: "rename tab",
                        input: tab.label.clone(),
                        replace_on_type: false,
                        target: ClientRenameTarget::Tab {
                            tab_id,
                            auto_name: !tab.custom_label,
                            original_name: tab.label.clone(),
                        },
                    }));
                }
            }
            ClientContextMenuAction::Close => {
                self.push_endpoint_method(Method::TabClose(TabTarget { tab_id }), outcome);
            }
            _ => {}
        }
    }

    fn activate_pane_context_action(
        &mut self,
        pane_id: String,
        workspace_id: String,
        source_pane_id: Option<String>,
        right_click_passthrough: bool,
        collection_id: Option<String>,
        collection_archived: bool,
        action: ClientContextMenuAction,
        outcome: &mut ClientShellInput,
    ) {
        use crate::api::schema::{
            Method, PaneInputSetParams, PaneRenameParams, PaneRightClickTarget, PaneSplitParams,
            PaneSwapParams, PaneTarget, PaneZoomMode, PaneZoomParams, SplitDirection,
        };

        match action {
            ClientContextMenuAction::InspectTerminal => {
                self.open_terminal_inspector(pane_id, outcome);
            }
            ClientContextMenuAction::RenamePane => {
                let label = self.snapshot.as_deref().and_then(|snapshot| {
                    snapshot
                        .panes
                        .iter()
                        .find(|pane| pane.pane_id == pane_id)
                        .and_then(|pane| pane.label.clone())
                });
                self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                    title: "rename pane",
                    input: label.clone().unwrap_or_default(),
                    replace_on_type: label.is_none(),
                    target: ClientRenameTarget::Pane { pane_id },
                }));
            }
            ClientContextMenuAction::ClearPaneName => self.push_endpoint_method(
                Method::PaneRename(PaneRenameParams {
                    pane_id,
                    label: None,
                }),
                outcome,
            ),
            ClientContextMenuAction::SwapWithFocusedPane => {
                if let Some(source_pane_id) = source_pane_id {
                    self.push_endpoint_method(
                        Method::PaneSwap(PaneSwapParams {
                            pane_id: None,
                            direction: None,
                            source_pane_id: Some(source_pane_id.clone()),
                            target_pane_id: Some(pane_id),
                        }),
                        outcome,
                    );
                    self.push_endpoint_method(
                        Method::PaneFocus(PaneTarget {
                            pane_id: source_pane_id,
                        }),
                        outcome,
                    );
                }
            }
            ClientContextMenuAction::SplitRight | ClientContextMenuAction::SplitDown => {
                self.push_endpoint_method(
                    Method::PaneSplit(PaneSplitParams {
                        workspace_id: Some(workspace_id),
                        target_pane_id: Some(pane_id),
                        direction: if action == ClientContextMenuAction::SplitRight {
                            SplitDirection::Right
                        } else {
                            SplitDirection::Down
                        },
                        ratio: None,
                        cwd: None,
                        focus: true,
                        right_click: Default::default(),
                        env: Default::default(),
                    }),
                    outcome,
                );
            }
            ClientContextMenuAction::Zoom => self.push_endpoint_method(
                Method::PaneZoom(PaneZoomParams {
                    pane_id: Some(pane_id),
                    mode: PaneZoomMode::Toggle,
                }),
                outcome,
            ),
            ClientContextMenuAction::ToggleRightClickPassthrough => self.push_endpoint_method(
                Method::PaneInputSet(PaneInputSetParams {
                    pane_id,
                    right_click: if right_click_passthrough {
                        PaneRightClickTarget::Herdr
                    } else {
                        PaneRightClickTarget::Pane
                    },
                }),
                outcome,
            ),
            ClientContextMenuAction::ArchiveCollectionMember
            | ClientContextMenuAction::RestoreCollectionMember => {
                if let Some(collection_id) = collection_id {
                    self.push_endpoint_method(
                        if action == ClientContextMenuAction::RestoreCollectionMember
                            || collection_archived
                        {
                            Method::CollectionRestore(crate::api::schema::CollectionMemberTarget {
                                collection_id,
                                pane_id,
                            })
                        } else {
                            Method::CollectionArchive(crate::api::schema::CollectionMemberTarget {
                                collection_id,
                                pane_id,
                            })
                        },
                        outcome,
                    );
                }
            }
            ClientContextMenuAction::PromoteCollectionMember => {
                if let Some(collection_id) = collection_id {
                    let member_ids = self
                        .structure
                        .as_deref()
                        .and_then(|structure| {
                            structure
                                .collections
                                .iter()
                                .find(|collection| collection.collection_id == collection_id)
                        })
                        .map(|collection| {
                            collection
                                .members
                                .iter()
                                .map(|member| member.pane_id.as_str())
                                .collect::<HashSet<_>>()
                        })
                        .unwrap_or_default();
                    let pane_tab_id = self.snapshot.as_deref().and_then(|snapshot| {
                        snapshot
                            .panes
                            .iter()
                            .find(|pane| pane.pane_id == pane_id)
                            .map(|pane| pane.tab_id.as_str())
                    });
                    let target_pane_id = self.snapshot.as_deref().and_then(|snapshot| {
                        snapshot
                            .panes
                            .iter()
                            .find(|pane| {
                                Some(pane.tab_id.as_str()) == pane_tab_id
                                    && !member_ids.contains(pane.pane_id.as_str())
                            })
                            .map(|pane| pane.pane_id.clone())
                    });
                    if let Some(target_pane_id) = target_pane_id {
                        self.push_endpoint_method(
                            Method::CollectionPromote(
                                crate::api::schema::CollectionPromoteParams {
                                    pane_id,
                                    target_pane_id,
                                    direction: SplitDirection::Right,
                                    ratio: Some(0.5),
                                    focus: true,
                                },
                            ),
                            outcome,
                        );
                    }
                }
            }
            ClientContextMenuAction::ClosePane => {
                self.push_endpoint_method(Method::PaneClose(PaneTarget { pane_id }), outcome)
            }
            _ => {}
        }
    }

    pub(super) fn open_terminal_inspector(
        &mut self,
        pane_id: String,
        outcome: &mut ClientShellInput,
    ) {
        self.exit_copy_mode(false, outcome);
        self.selection = None;
        self.stop_selection_autoscroll();
        let placement = if self
            .supports_endpoint_capability(crate::protocol::endpoint::PANE_VIEWPORT_CAPABILITY)
            && !self.graphics.pane_has_graphics(&pane_id)
        {
            crate::pane_contribution::PaneContributionPlacement::Accessory
        } else {
            crate::pane_contribution::PaneContributionPlacement::Replacement
        };
        let pane_label = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.panes.iter().find(|pane| pane.pane_id == pane_id))
            .and_then(|pane| pane.label.clone())
            .unwrap_or_else(|| "Terminal".to_owned());
        self.inspector = Some(Box::new(ClientTerminalInspectorPane {
            pane_id: pane_id.clone(),
            pane_label,
            loading: true,
            focused: true,
            request_revision: 0,
            view_request_id: None,
            service_generation: None,
            pending_action: None,
            placement,
            contribution: None,
            telemetry: None,
            error: None,
        }));
        self.set_pane_contribution_viewport(&pane_id, Some(placement), outcome);
        self.refresh_terminal_inspector(pane_id, outcome);
    }

    pub(super) fn close_terminal_inspector(&mut self, outcome: &mut ClientShellInput) {
        let Some(inspector) = self.inspector.as_ref() else {
            return;
        };
        let pane_id = inspector.pane_id.clone();
        let request_id = inspector.view_request_id.clone();
        let contribution_revision = inspector.request_revision;
        if let (Some(snapshot), Some(request_id)) = (self.snapshot.as_deref(), request_id) {
            let cancel = crate::protocol::endpoint::ClientPaneViewCancel {
                boot_id: snapshot.boot_id.clone(),
                request_id,
                pane_id: pane_id.clone(),
                contribution_revision,
            };
            if let Ok(data) = serde_json::to_string(&cancel) {
                outcome.actions.push(ClientShellAction::EndpointControl {
                    endpoint_id: self.active_endpoint_id.clone(),
                    message: ClientMessage::EndpointControl {
                        kind: crate::protocol::endpoint::PANE_VIEW_CANCEL_KIND.into(),
                        data,
                    },
                });
            }
        }
        self.set_pane_contribution_viewport(&pane_id, None, outcome);
        self.inspector = None;
    }

    pub(super) fn set_pane_contribution_viewport(
        &mut self,
        pane_id: &str,
        placement: Option<crate::pane_contribution::PaneContributionPlacement>,
        outcome: &mut ClientShellInput,
    ) {
        if !self.supports_endpoint_capability(crate::protocol::endpoint::PANE_VIEWPORT_CAPABILITY) {
            return;
        }
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let terminal = placement.and_then(|placement| {
            let pane = self
                .hits
                .panes
                .iter()
                .find(|pane| pane.pane_id == pane_id)?;
            contributions::contribution_geometry(pane.projected_inner_rect, placement).terminal
        });
        let request = contributions::viewport_request(
            &snapshot.boot_id,
            snapshot.revision,
            pane_id,
            terminal,
        );
        if self.last_pane_viewport_request.as_ref() == Some(&request) {
            return;
        }
        let Ok(data) = serde_json::to_string(&request) else {
            return;
        };
        self.last_pane_viewport_request = Some(request);
        outcome.actions.push(ClientShellAction::EndpointControl {
            endpoint_id: self.active_endpoint_id.clone(),
            message: ClientMessage::EndpointControl {
                kind: crate::protocol::endpoint::PANE_VIEWPORT_KIND.into(),
                data,
            },
        });
    }

    pub(super) fn refresh_terminal_inspector(
        &mut self,
        pane_id: String,
        outcome: &mut ClientShellInput,
    ) {
        if let Some(inspector) = self.inspector.as_mut() {
            inspector.loading = true;
            inspector.request_revision = inspector.request_revision.saturating_add(1);
            inspector.contribution = None;
            inspector.service_generation = None;
            inspector.pending_action = None;
            inspector.telemetry = None;
            inspector.error = None;
        }
        self.hits.contribution_actions.clear();
        if matches!(
            self.overlay.as_ref(),
            Some(ClientShellOverlay::PaneInterruptConfirm(confirm)) if confirm.pane_id == pane_id
        ) {
            self.overlay = None;
        }
        self.request_pane_contribution(&pane_id, outcome);
        let sent = self.push_endpoint_method_with_kind(
            crate::api::schema::Method::PaneTelemetry(crate::api::schema::PaneTelemetryParams {
                pane_id: Some(pane_id.clone()),
            }),
            PendingEndpointKind::TerminalInspector { pane_id },
            outcome,
        );
        if !sent {
            if let Some(inspector) = self.inspector.as_mut() {
                inspector.loading = false;
                inspector.error = Some("Telemetry is unavailable from this endpoint.".to_owned());
            }
        }
    }

    fn request_pane_contribution(&mut self, pane_id: &str, outcome: &mut ClientShellInput) {
        if !self.supports_endpoint_capability(crate::protocol::endpoint::PANE_VIEW_CAPABILITY) {
            return;
        }
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(inspector) = self.inspector.as_mut() else {
            return;
        };
        if let Some(previous_request_id) = inspector.view_request_id.take() {
            let cancel = crate::protocol::endpoint::ClientPaneViewCancel {
                boot_id: snapshot.boot_id.clone(),
                request_id: previous_request_id,
                pane_id: pane_id.to_owned(),
                contribution_revision: inspector.request_revision.saturating_sub(1),
            };
            if let Ok(data) = serde_json::to_string(&cancel) {
                outcome.actions.push(ClientShellAction::EndpointControl {
                    endpoint_id: self.active_endpoint_id.clone(),
                    message: ClientMessage::EndpointControl {
                        kind: crate::protocol::endpoint::PANE_VIEW_CANCEL_KIND.into(),
                        data,
                    },
                });
            }
        }
        let request_id = format!("view-{}-{}", inspector.request_revision, pane_id);
        let request = crate::protocol::endpoint::ClientPaneViewRequest {
            boot_id: snapshot.boot_id.clone(),
            projection_revision: snapshot.revision,
            request_id: request_id.clone(),
            pane_id: pane_id.to_owned(),
            contribution_revision: inspector.request_revision,
        };
        let Ok(data) = serde_json::to_string(&request) else {
            return;
        };
        inspector.view_request_id = Some(request_id);
        outcome.actions.push(ClientShellAction::EndpointControl {
            endpoint_id: self.active_endpoint_id.clone(),
            message: ClientMessage::EndpointControl {
                kind: crate::protocol::endpoint::PANE_VIEW_REQUEST_KIND.into(),
                data,
            },
        });
    }
}
