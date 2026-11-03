use ratatui::layout::Rect;

use crate::app;
use crate::protocol::{self, FrameData};

pub(super) fn structure_projection(
    app: &app::App,
    boot_id: &str,
    revision: u64,
) -> protocol::endpoint::ClientShellStructureProjection {
    let workspace_index = app
        .state
        .workspaces
        .iter()
        .map(|workspace| (workspace.id.as_str(), workspace))
        .collect::<std::collections::HashMap<_, _>>();
    let git_identity_for_workspace = |workspace_id: &str| {
        let fingerprint = workspace_index
            .get(workspace_id)
            .and_then(|workspace| app.git_status_cache.get(&workspace.cached_git_status_key))
            .and_then(|entry| entry.fingerprint.as_ref());
        let upstream = fingerprint
            .and_then(|fingerprint| fingerprint.upstream.as_ref())
            .map(|upstream| {
                upstream
                    .full_ref
                    .strip_prefix("refs/remotes/")
                    .unwrap_or(&upstream.full_ref)
                    .to_owned()
            });
        let repository_web_url = fingerprint.and_then(|fingerprint| {
            crate::repository::canonical_repository_web_url(
                fingerprint.preferred_repository_remote_url()?,
            )
        });
        let upstream_web_url = fingerprint.and_then(|fingerprint| {
            let branch = fingerprint
                .upstream
                .as_ref()?
                .merge_ref
                .strip_prefix("refs/heads/")?;
            crate::repository::repository_branch_web_url(repository_web_url.as_deref()?, branch)
        });
        let detached_head = fingerprint.and_then(|fingerprint| match &fingerprint.head {
            crate::workspace::GitHeadIdentity::Detached { oid } => {
                Some(oid.chars().take(8).collect())
            }
            crate::workspace::GitHeadIdentity::Branch { .. } => None,
        });
        (
            upstream,
            repository_web_url,
            upstream_web_url,
            detached_head,
        )
    };
    let spaces_document = app
        .host_document_caches
        .get(&(
            crate::spaces_extension_contract::SPACES_NAMESPACE.into(),
            crate::spaces_extension_contract::SPACES_SCOPE_ID.into(),
        ))
        .and_then(crate::host_document_cache::HostDocumentCache::current);
    let spaces_repository_ids = spaces_document
        .into_iter()
        .flat_map(|document| document.items.iter())
        .filter(|item| item.parent_id.is_none())
        .filter_map(|item| item.id.strip_prefix("repository:"))
        .collect::<std::collections::HashSet<_>>();
    let repositories = app
        .state
        .repositories
        .iter()
        .filter(|repository| spaces_repository_ids.contains(repository.id.as_str()))
        .map(|repository| {
            let mut web_url = None;
            let checkouts = repository
                .checkout_workspace_ids
                .iter()
                .filter_map(|workspace_id| {
                    let workspace = workspace_index.get(workspace_id.as_str())?;
                    let checkout = workspace.checkout.as_ref()?;
                    let kind = match checkout.kind {
                        crate::repository::CheckoutKind::Primary => {
                            protocol::endpoint::ClientShellCheckoutKind::Primary
                        }
                        crate::repository::CheckoutKind::Linked => {
                            protocol::endpoint::ClientShellCheckoutKind::Linked
                        }
                    };
                    let (upstream, repository_web_url, upstream_web_url, detached_head) =
                        git_identity_for_workspace(workspace_id);
                    if web_url.is_none() {
                        web_url = repository_web_url;
                    }
                    Some(protocol::endpoint::ClientShellRepositoryCheckout {
                        workspace_id: workspace_id.clone(),
                        checkout_path: Some(checkout.checkout_path.display().to_string()),
                        kind,
                        upstream,
                        upstream_web_url,
                        detached_head,
                    })
                })
                .collect::<Vec<_>>();
            let repository_path = repository
                .checkout_workspace_ids
                .iter()
                .filter_map(|workspace_id| workspace_index.get(workspace_id.as_str()))
                .filter_map(|workspace| workspace.checkout.as_ref())
                .find(|checkout| checkout.kind == crate::repository::CheckoutKind::Primary)
                .or_else(|| {
                    repository
                        .checkout_workspace_ids
                        .iter()
                        .filter_map(|workspace_id| workspace_index.get(workspace_id.as_str()))
                        .find_map(|workspace| workspace.checkout.as_ref())
                })
                .map(|checkout| checkout.checkout_path.display().to_string());
            // Repository remotes come from the background Git-status cache. Never read
            // identity marker files while projecting a frame for each attached client.
            let durable_identity = web_url.clone().or_else(|| Some(repository.id.clone()));
            protocol::endpoint::ClientShellRepository {
                repository_id: repository.id.clone(),
                label: repository.display_label().to_owned(),
                repository_path,
                web_url,
                durable_identity,
                checkouts,
                last_focused_workspace_id: repository.last_focused_workspace_id.clone(),
            }
        })
        .collect();
    let space_order = spaces_document
        .into_iter()
        .flat_map(|document| document.items.iter())
        .filter(|item| item.parent_id.is_none())
        .filter_map(|item| {
            item.id
                .strip_prefix("repository:")
                .map(|id| protocol::endpoint::ClientShellSpaceRef {
                    kind: protocol::endpoint::ClientShellSpaceKind::Repository,
                    id: id.into(),
                })
                .or_else(|| {
                    item.id.strip_prefix("workspace:").map(|id| {
                        protocol::endpoint::ClientShellSpaceRef {
                            kind: protocol::endpoint::ClientShellSpaceKind::StandaloneWorkspace,
                            id: id.into(),
                        }
                    })
                })
        })
        .collect();
    let collections = if app.collections_package_runtime_available() {
        app.collection_infos(None, None)
    } else {
        Vec::new()
    }
    .into_iter()
    .map(|collection| protocol::endpoint::ClientShellCollection {
        collection_id: collection.collection_id,
        workspace_id: collection.workspace_id,
        tab_id: collection.tab_id,
        label: collection.label,
        selected_pane_id: collection.selected_pane_id,
        members: collection
            .members
            .into_iter()
            .map(|member| protocol::endpoint::ClientShellCollectionMember {
                pane_id: member.pane_id,
                index: member.index,
                archived: member.archived,
            })
            .collect(),
    })
    .collect();
    let delegations = app
        .delegation_tree()
        .into_iter()
        .map(|entry| protocol::endpoint::ClientShellDelegation {
            delegation_id: entry.delegation.delegation_id,
            pane_id: entry.delegation.pane_id,
            parent_id: entry.delegation.parent_id,
            purpose: entry.delegation.purpose,
            depth: entry.depth,
            external_parent_id: None,
        })
        .collect();
    protocol::endpoint::ClientShellStructureProjection {
        session_id: crate::session::active_name().unwrap_or_else(|| "default".into()),
        boot_id: boot_id.to_owned(),
        revision,
        collections,
        delegations,
        repositories,
        space_order,
    }
}

pub(super) fn snapshot(
    app: &app::App,
    boot_id: &str,
    revision: u64,
    config_diagnostic: Option<&str>,
    location: Option<&crate::server::clients::ClientShellLocation>,
) -> protocol::ClientShellSnapshot {
    let snapshot = app.session_snapshot_v1_compat_metadata();
    let focused_workspace_id = location
        .and_then(|location| location.focused_workspace_id.clone())
        .or_else(|| snapshot.focused_workspace_id.clone());
    let focused_tab_id = location
        .and_then(|location| location.focused_tab_id().map(str::to_owned))
        .or_else(|| snapshot.focused_tab_id.clone());
    let focused_pane_id = focused_tab_id
        .as_deref()
        .and_then(|tab_id| app.parse_tab_id(tab_id))
        .and_then(|(workspace_index, tab_index)| {
            let pane_id = app
                .state
                .workspaces
                .get(workspace_index)?
                .tabs
                .get(tab_index)?
                .layout
                .focused();
            app.public_pane_id(workspace_index, pane_id)
        })
        .or_else(|| snapshot.focused_pane_id.clone());
    let workspaces = snapshot
        .workspaces
        .into_iter()
        .zip(&app.state.workspaces)
        .enumerate()
        .map(|(workspace_index, (workspace, state))| {
            let mut tokens = workspace.tokens.into_iter().collect::<Vec<_>>();
            tokens.sort_by(|left, right| left.0.cmp(&right.0));
            let workspace_id = workspace.workspace_id;
            let active_tab_id = location
                .and_then(|location| location.active_tab_ids.get(&workspace_id))
                .cloned()
                .unwrap_or(workspace.active_tab_id);
            let active_tab_index =
                app.parse_tab_id(&active_tab_id)
                    .and_then(|(tab_workspace_index, tab_index)| {
                        (tab_workspace_index == workspace_index).then_some(tab_index)
                    });
            protocol::ClientShellWorkspace {
                focused: focused_workspace_id.as_deref() == Some(workspace_id.as_str()),
                workspace_id,
                active_tab_id,
                new_workspace_cwd: app
                    .resolved_new_workspace_cwd_from_tab(workspace_index, active_tab_index)
                    .display()
                    .to_string(),
                number: workspace.number,
                label: workspace.label,
                custom_label: state.custom_name.is_some(),
                branch: state.branch(),
                git_ahead_behind: state.git_ahead_behind(),
                tokens,
                worktree: workspace
                    .worktree
                    .map(|worktree| protocol::ClientShellWorktree {
                        key: worktree.repo_key,
                        label: worktree.repo_name,
                        is_linked_worktree: worktree.is_linked_worktree,
                    }),
                agent_status: workspace.agent_status,
            }
        })
        .collect();
    let tabs = snapshot
        .tabs
        .into_iter()
        .zip(
            app.state
                .workspaces
                .iter()
                .flat_map(|workspace| workspace.tabs.iter()),
        )
        .map(|(tab, state)| {
            let tab_id = tab.tab_id;
            protocol::ClientShellTab {
                focused: focused_tab_id.as_deref() == Some(tab_id.as_str()),
                tab_id,
                workspace_id: tab.workspace_id,
                number: tab.number,
                label: tab.label,
                custom_label: !state.is_auto_named(),
                zoomed: state.zoomed,
                agent_status: tab.agent_status,
            }
        })
        .collect();
    let panes: Vec<protocol::ClientShellPane> = snapshot
        .panes
        .into_iter()
        .map(|pane| {
            let pane_id = pane.pane_id;
            let focused = focused_pane_id.as_deref() == Some(pane_id.as_str());
            let right_click_passthrough = app
                .parse_pane_id(&pane_id)
                .and_then(|(workspace_index, pane_id)| {
                    app.state
                        .workspaces
                        .get(workspace_index)?
                        .pane_state(pane_id)
                })
                .is_some_and(|pane| pane.right_click_passthrough);
            protocol::ClientShellPane {
                pane_id,
                workspace_id: pane.workspace_id,
                tab_id: pane.tab_id,
                label: pane.label,
                cwd: pane.cwd,
                foreground_cwd: pane.foreground_cwd,
                focused,
                right_click_passthrough,
            }
        })
        .collect();
    let agents = snapshot
        .agents
        .into_iter()
        .map(|agent| {
            let pane_id = agent.pane_id;
            let focused = focused_pane_id.as_deref() == Some(pane_id.as_str());
            let mut state_labels = agent.state_labels.into_iter().collect::<Vec<_>>();
            state_labels.sort_by(|left, right| left.0.cmp(&right.0));
            let mut tokens = agent.tokens.into_iter().collect::<Vec<_>>();
            tokens.sort_by(|left, right| left.0.cmp(&right.0));
            protocol::ClientShellAgent {
                pane_id,
                workspace_id: agent.workspace_id,
                tab_id: agent.tab_id,
                name: agent.name,
                display_agent: agent.display_agent,
                agent: agent.agent,
                title: agent.title,
                terminal_title: agent.terminal_title,
                terminal_title_stripped: agent.terminal_title_stripped,
                agent_status: agent.agent_status,
                state_change_seq: agent.state_change_seq,
                state_labels,
                tokens,
                focused,
            }
        })
        .collect();

    let agent_view_label = app
        .state
        .agent_view_override
        .as_ref()
        .map(|view| view.label.clone().unwrap_or_else(|| "filtered".to_owned()));
    let agent_order = crate::ui::agent_panel_entries_from(&app.state, &app.terminal_runtimes)
        .into_iter()
        .filter_map(|entry| app.public_pane_id(entry.ws_idx, entry.pane_id))
        .collect();

    let zoomed = focused_tab_id
        .as_deref()
        .and_then(|tab_id| app.parse_tab_id(tab_id))
        .and_then(|(workspace_index, tab_index)| {
            app.state
                .workspaces
                .get(workspace_index)?
                .tabs
                .get(tab_index)
        })
        .is_some_and(|tab| tab.zoomed);
    let tab_bar_right = app
        .state
        .tab_bar_right
        .iter()
        .filter_map(|segment| match segment {
            crate::app::state::TabBarStatusSegment::Zoom if zoomed => {
                Some(protocol::ClientShellTabStatusSegment {
                    text: "ZOOM".to_owned(),
                    accent: true,
                })
            }
            crate::app::state::TabBarStatusSegment::Text(Some(text)) if !text.is_empty() => {
                Some(protocol::ClientShellTabStatusSegment {
                    text: text.clone(),
                    accent: false,
                })
            }
            crate::app::state::TabBarStatusSegment::Zoom
            | crate::app::state::TabBarStatusSegment::Text(_) => None,
        })
        .collect();

    let product_announcement = app.state.product_announcement.as_ref().map(|announcement| {
        protocol::ClientShellProductAnnouncement {
            version: announcement.version.clone(),
            id: announcement.id.clone(),
            title: announcement.title.clone(),
            body: announcement.body.clone(),
            preview: announcement.preview,
        }
    });
    let release_notes =
        app.state
            .latest_release_notes
            .as_ref()
            .map(|notes| protocol::ClientShellReleaseNotes {
                version: notes.version.clone(),
                body: notes.body.clone(),
                preview: notes.preview,
            });

    protocol::ClientShellSnapshot {
        boot_id: boot_id.to_owned(),
        revision,
        config_diagnostic: config_diagnostic.map(str::to_owned),
        product_announcement,
        update_available: app.state.update_available.clone(),
        update_install_command: app.state.update_install_command.clone(),
        server_keybindings_toml: app.client_shell_keybindings_profile().map(str::to_owned),
        latest_release_notes_available: app.state.latest_release_notes_available,
        integration_updates_available: app.state.integration_updates_available(),
        worktree_directory: app.state.worktree_directory.to_string_lossy().into_owned(),
        release_notes,
        focused_workspace_id,
        focused_tab_id,
        focused_pane_id,
        tab_bar_right,
        tab_bar_right_separator: app.state.tab_bar_right_separator.clone(),
        agent_view_label,
        agent_order,
        workspaces,
        tabs,
        panes,
        agents,
        commands: app.client_shell_command_manifest(),
    }
}

pub(super) struct RenderedPaneSurface {
    pub(super) frame: FrameData,
    pub(super) panes: Vec<protocol::PaneSurfacePane>,
    /// View-pane descriptors for a `shell.surface.v2` peer. Always empty for the
    /// `shell.surface.v1` projection.
    pub(super) views: Vec<protocol::PaneSurfaceViewEntryV2>,
    pub(super) collections: Vec<protocol::PaneSurfaceCollection>,
    pub(super) splits: Vec<protocol::PaneSurfaceSplit>,
    pub(super) popup: Option<Box<protocol::ClientShellPopupSurface>>,
    pub(super) graphics: protocol::SurfaceGraphicsScene,
    pub(super) graphics_delivery: crate::kitty_graphics::surface::DeliveryCache,
    pub(super) graphics_sources: crate::kitty_graphics::surface::SourceFiles,
}

/// The view panes the frame will actually draw, in tree order.
///
/// A hidden stack member or a zoomed-away view is not in `drawn`, so the surface
/// projection never walks it: a hidden view performs no presentation work.
pub(super) fn drawn_view_panes(
    layout: &crate::layout::TileLayout,
    drawn: &std::collections::HashSet<crate::layout::PaneId>,
) -> Vec<crate::layout::PaneId> {
    layout
        .view_panes()
        .into_iter()
        .filter(|pane| drawn.contains(pane))
        .collect()
}

/// Render the active tab pane surface under `shell.surface.v2`.
#[allow(clippy::too_many_arguments)]
pub(super) fn render_pane_surface(
    app: &mut app::App,
    target: Option<crate::ui::TabSurfaceTarget>,
    area: Rect,
    resize_panes: bool,
    show_popup: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
    graphics_delivery: &crate::kitty_graphics::surface::DeliveryCache,
    client_id: u64,
) -> RenderedPaneSurface {
    let content_revisions_before = target
        .and_then(|target| {
            let workspace = app.state.workspaces.get(target.workspace_index)?;
            let tab = workspace.tabs.get(target.tab_index)?;
            Some(
                tab.layout
                    .pane_ids()
                    .into_iter()
                    .filter_map(|pane_id| {
                        app.state
                            .runtime_for_pane_in_workspace(
                                &app.terminal_runtimes,
                                target.workspace_index,
                                pane_id,
                            )
                            .map(|runtime| (pane_id, runtime.content_seq()))
                    })
                    .collect::<std::collections::HashMap<_, _>>(),
            )
        })
        .unwrap_or_default();
    let (buffer, cursor, hyperlinks, layout) =
        crate::server::render_stream::render_tab_surface_virtual(
            &app.state,
            &app.terminal_runtimes,
            target,
            area,
            resize_panes,
            cell_size,
        );
    // One pass over the drawn view panes: each one's content identity and the
    // public ids it covers. A hidden or zoomed-away view is not walked, so it
    // does no presentation work, and the frame below only reads the panes it
    // drew. A per-pane tree walk would be a search per pane.
    let drawn_panes: std::collections::HashSet<crate::layout::PaneId> =
        layout.pane_infos.iter().map(|pane| pane.id).collect();
    let view_projection = target
        .and_then(|target| {
            let workspace = app.state.workspaces.get(target.workspace_index)?;
            let tab = workspace.tabs.get(target.tab_index)?;
            Some(
                drawn_view_panes(&tab.layout, &drawn_panes)
                    .into_iter()
                    .map(|view_pane| {
                        let content = tab.layout.view_at(view_pane);
                        let scope = tab
                            .layout
                            .view_scope(view_pane)
                            .into_iter()
                            .filter_map(|covered| {
                                app.public_pane_id(target.workspace_index, covered)
                            })
                            .collect::<Vec<_>>();
                        (view_pane, (content, scope))
                    })
                    .collect::<std::collections::HashMap<
                        crate::layout::PaneId,
                        (Option<crate::layout::ViewRef>, Vec<String>),
                    >>(),
            )
        })
        .unwrap_or_default();
    let mut views: Vec<protocol::PaneSurfaceViewEntryV2> = Vec::new();
    let panes = target
        .map(|target| {
            let workspace_index = target.workspace_index;
            layout
                .pane_infos
                .iter()
                .filter_map(|pane| {
                    app.public_pane_id(workspace_index, pane.id).map(|pane_id| {
                        let runtime = app.state.runtime_for_pane_in_workspace(
                            &app.terminal_runtimes,
                            workspace_index,
                            pane.id,
                        );
                        if let Some((Some(content), scope)) = view_projection.get(&pane.id) {
                            views.push(protocol::PaneSurfaceViewEntryV2 {
                                pane_id: pane_id.clone(),
                                extension: content.extension.clone(),
                                view: content.view.clone(),
                                scope: scope.clone(),
                            });
                        }
                        let mouse_reporting =
                            runtime.is_some_and(|runtime| runtime.mouse_reporting_enabled());
                        let sgr_pixel_mouse =
                            runtime.is_some_and(|runtime| runtime.sgr_pixel_mouse_enabled());
                        let (pixel_width, pixel_height) = if cell_size.is_known() {
                            (
                                u32::from(pane.inner_rect.width) * cell_size.width_px,
                                u32::from(pane.inner_rect.height) * cell_size.height_px,
                            )
                        } else {
                            (0, 0)
                        };
                        let content_revision = runtime.map_or(0, |runtime| {
                            let after = runtime.content_seq();
                            if content_revisions_before.get(&pane.id).copied() == Some(after)
                                && after.is_multiple_of(2)
                            {
                                after
                            } else {
                                after | 1
                            }
                        });
                        protocol::PaneSurfacePane {
                            pane_id,
                            content_revision,
                            rect: pane.rect.into(),
                            inner_rect: pane.inner_rect.into(),
                            scrollbar_rect: pane.scrollbar_rect.map(Into::into),
                            scroll: runtime.and_then(|runtime| runtime.scroll_metrics()).map(
                                |metrics| protocol::PaneSurfaceScrollMetrics {
                                    offset_from_bottom: metrics.offset_from_bottom as u64,
                                    max_offset_from_bottom: metrics.max_offset_from_bottom as u64,
                                    viewport_rows: metrics.viewport_rows as u64,
                                },
                            ),
                            focused: pane.is_focused,
                            mouse_reporting,
                            sgr_pixel_mouse,
                            alternate_screen_active: runtime
                                .is_some_and(|runtime| runtime.alternate_screen_active()),
                            pixel_width,
                            pixel_height,
                        }
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let collections: Vec<protocol::PaneSurfaceCollection> = target
        .and_then(|target| {
            let workspace = app.state.workspaces.get(target.workspace_index)?;
            let tab = workspace.tabs.get(target.tab_index)?;
            Some(
                tab.layout
                    .collection_ids()
                    .into_iter()
                    .filter_map(|collection_id| {
                        let rect = *layout
                            .leaf_rects
                            .get(&crate::layout::LayoutLeaf::Stack(collection_id))?;
                        let inner_rect = Rect::new(
                            rect.x,
                            rect.y.saturating_add(1),
                            rect.width,
                            rect.height.saturating_sub(1),
                        );
                        Some(protocol::PaneSurfaceCollection {
                            collection_id: format!("collection_{}", collection_id.raw()),
                            rect: rect.into(),
                            inner_rect: inner_rect.into(),
                            focused: tab.layout.focused_leaf()
                                == crate::layout::LayoutLeaf::Stack(collection_id),
                        })
                    })
                    .collect(),
            )
        })
        .unwrap_or_default();
    let pane_frames = layout
        .pane_infos
        .iter()
        .map(|pane| pane.rect)
        .chain(collections.iter().map(|collection| {
            Rect::new(
                collection.rect.x,
                collection.rect.y,
                collection.rect.width,
                collection.rect.height,
            )
        }))
        .collect::<Vec<_>>();
    let splits = layout
        .split_borders
        .iter()
        .filter_map(|split| {
            let hit_rect = split_hit_rect(
                split,
                app.state.pane_borders.draws_borders(),
                app.state.pane_gaps,
                &pane_frames,
            )?;
            let direction = match split.direction {
                ratatui::layout::Direction::Horizontal => {
                    protocol::PaneSurfaceSplitDirection::Horizontal
                }
                ratatui::layout::Direction::Vertical => {
                    protocol::PaneSurfaceSplitDirection::Vertical
                }
            };
            Some(protocol::PaneSurfaceSplit {
                direction,
                pos: split.pos,
                area: split.area.into(),
                hit_rect: hit_rect.into(),
                path: split.path.clone(),
            })
        })
        .collect();
    let popup = show_popup
        .then(|| render_popup_surface(app, area, resize_panes, cell_size))
        .flatten();
    let (graphics, next_graphics_delivery, graphics_sources) =
        crate::server::client_shell_graphics::collect(
            app,
            &layout.pane_infos,
            &layout.split_borders,
            popup.as_deref(),
            target,
            cell_size,
            graphics_delivery,
            client_id,
        );
    RenderedPaneSurface {
        frame: FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, cursor, &hyperlinks),
        panes,
        views,
        collections,
        splits,
        popup,
        graphics,
        graphics_delivery: next_graphics_delivery,
        graphics_sources,
    }
}

fn render_popup_surface(
    app: &app::App,
    area: Rect,
    resize_runtime: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> Option<Box<protocol::ClientShellPopupSurface>> {
    let popup = app.state.popup_pane.as_ref()?;
    let geometry = if resize_runtime {
        resize_popup_runtime(app, area, cell_size)?
    } else {
        crate::popup_size::resolve_popup_geometry(popup.width, popup.height, area)?
    };
    let runtime = app.terminal_runtimes.get(&popup.terminal_id)?;
    let content_area = Rect::new(0, 0, geometry.inner.width, geometry.inner.height);
    let (buffer, cursor) =
        crate::server::render_stream::render_terminal_virtual(runtime, content_area);
    let hyperlinks = runtime.visible_hyperlinks(content_area);
    let title = app
        .state
        .terminals
        .get(&popup.terminal_id)
        .and_then(|terminal| terminal.manual_label.clone())
        .unwrap_or_else(|| "popup".to_owned());
    let (pixel_width, pixel_height) = if cell_size.is_known() {
        (
            u32::from(content_area.width) * cell_size.width_px,
            u32::from(content_area.height) * cell_size.height_px,
        )
    } else {
        (0, 0)
    };
    Some(Box::new(protocol::ClientShellPopupSurface {
        terminal_id: popup.terminal_id.to_string(),
        title,
        width: popup.width.map(client_popup_size),
        height: popup.height.map(client_popup_size),
        frame: FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, cursor, &hyperlinks),
        mouse_reporting: runtime.mouse_reporting_enabled(),
        sgr_pixel_mouse: runtime.sgr_pixel_mouse_enabled(),
        pixel_width,
        pixel_height,
    }))
}

pub(super) fn resize_popup_runtime(
    app: &app::App,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> Option<crate::popup_size::PopupResolvedGeometry> {
    let popup = app.state.popup_pane.as_ref()?;
    let geometry = crate::popup_size::resolve_popup_geometry(popup.width, popup.height, area)?;
    let runtime = app.terminal_runtimes.get(&popup.terminal_id)?;
    if !app
        .state
        .direct_attach_resize_locks
        .contains(&popup.terminal_id)
    {
        runtime.resize(
            geometry.inner.height,
            geometry.inner.width,
            cell_size.width_px,
            cell_size.height_px,
        );
    }
    Some(geometry)
}

fn client_popup_size(size: crate::popup_size::PopupSize) -> protocol::ClientShellPopupSize {
    match size {
        crate::popup_size::PopupSize::Cells(cells) => protocol::ClientShellPopupSize::Cells(cells),
        crate::popup_size::PopupSize::Percent(percent) => {
            protocol::ClientShellPopupSize::Percent(percent)
        }
    }
}

fn split_hit_rect(
    split: &crate::layout::SplitBorder,
    pane_borders: bool,
    pane_gaps: bool,
    pane_frames: &[Rect],
) -> Option<Rect> {
    if split.independent_borders {
        // Both child perimeters are resize handles; neither is terminal content.
        let hit = match split.direction {
            ratatui::layout::Direction::Horizontal => Rect::new(
                if split.view_before {
                    split.pos.saturating_sub(1)
                } else {
                    split.pos
                },
                split.area.y,
                1,
                split.area.height,
            ),
            ratatui::layout::Direction::Vertical => Rect::new(
                split.area.x,
                if split.view_before {
                    split.pos.saturating_sub(1)
                } else {
                    split.pos
                },
                split.area.width,
                1,
            ),
        }
        .intersection(split.area);
        return (!hit.is_empty()).then_some(hit);
    }
    let hit = match (split.direction, pane_borders, pane_gaps) {
        (ratatui::layout::Direction::Horizontal, true, false) => {
            Rect::new(split.pos, split.area.y, 1, split.area.height)
        }
        (ratatui::layout::Direction::Horizontal, true, true) => {
            let start = split.pos.saturating_sub(1);
            Rect::new(
                start,
                split.area.y,
                split.pos.saturating_sub(start).saturating_add(1),
                split.area.height,
            )
        }
        (ratatui::layout::Direction::Horizontal, false, true) => Rect::new(
            split.pos.checked_sub(1)?,
            split.area.y,
            1,
            split.area.height,
        ),
        (ratatui::layout::Direction::Vertical, true, false) => {
            Rect::new(split.area.x, split.pos, split.area.width, 1)
        }
        (ratatui::layout::Direction::Vertical, true, true) => {
            let start = split.pos.saturating_sub(1);
            Rect::new(
                split.area.x,
                start,
                split.area.width,
                split.pos.saturating_sub(start).saturating_add(1),
            )
        }
        (ratatui::layout::Direction::Vertical, false, true) => {
            Rect::new(split.area.x, split.pos.checked_sub(1)?, split.area.width, 1)
        }
        (_, false, false) => return None,
    };
    if !pane_borders
        && pane_frames.iter().any(|pane| {
            hit.x < pane.right()
                && hit.right() > pane.x
                && hit.y < pane.bottom()
                && hit.bottom() > pane.y
        })
    {
        return None;
    }
    Some(hit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn snapshot_metadata_skips_scroll_reads_and_preserves_public_session_data() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        // Fake runtimes are required for this test to mean anything: the counter
        // lives on the runtime, and test terminals alone would make every sum
        // zero even if a projection did read live scroll state.
        let history = (0..40)
            .map(|line| format!("line-{line}\r\n"))
            .collect::<String>();
        let mut active = crate::workspace::Workspace::test_new("active");
        let active_pane = active.tabs[0].root_pane;
        let inactive_pane = active.test_split(ratatui::layout::Direction::Horizontal);
        let hidden_tab = active.test_add_tab(None);
        assert_eq!(hidden_tab, 1, "the second tab is the hidden one");
        let hidden_pane = active.tabs[hidden_tab]
            .layout
            .pane_ids()
            .first()
            .copied()
            .expect("hidden tab pane");
        for pane in [active_pane, inactive_pane, hidden_pane] {
            active.insert_test_runtime(
                pane,
                crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                    20,
                    5,
                    1000,
                    history.as_bytes(),
                ),
            );
        }
        let mut background = crate::workspace::Workspace::test_new("background");
        let background_pane = background.tabs[0].root_pane;
        background.insert_test_runtime(
            background_pane,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                20,
                5,
                1000,
                history.as_bytes(),
            ),
        );
        app.state.workspaces = vec![active, background];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        for terminal in app.state.terminals.values_mut() {
            terminal.set_detected_state(
                Some(crate::detect::Agent::Pi),
                crate::detect::AgentState::Working,
            );
        }
        let scroll_reads = |app: &crate::app::App| {
            (0..app.state.workspaces.len())
                .flat_map(|ws_idx| {
                    app.state.workspaces[ws_idx]
                        .tabs
                        .iter()
                        .flat_map(|tab| tab.layout.pane_ids())
                        .map(move |pane_id| (ws_idx, pane_id))
                })
                .filter_map(|(ws_idx, pane_id)| {
                    app.state
                        .runtime_for_pane_in_workspace(&app.terminal_runtimes, ws_idx, pane_id)
                        .map(|runtime| runtime.test_scroll_metrics_reads())
                })
                .sum::<usize>()
        };

        // The full projection presents scroll position, so it must read live
        // scroll state once per pane; the metadata projection must not.
        let before_full = scroll_reads(&app);
        let full = app.session_snapshot_v1_compat();
        let after_full = scroll_reads(&app);
        assert_eq!(
            after_full - before_full,
            full.panes.len(),
            "the full projection must read live scroll state for every pane"
        );
        assert!(!full.panes.is_empty(), "fixture must produce panes");

        let before_metadata = scroll_reads(&app);
        let metadata = app.session_snapshot_v1_compat_metadata();
        let after_metadata = scroll_reads(&app);
        assert_eq!(
            before_metadata, after_metadata,
            "metadata snapshot must not read live scroll state"
        );
        assert!(metadata.panes.iter().all(|pane| pane.scroll.is_none()));

        let full = app.session_snapshot_v1_compat();
        assert_eq!(full.panes.len(), metadata.panes.len());
        for (full, metadata) in full.panes.iter().zip(metadata.panes.iter()) {
            assert_eq!(full.pane_id, metadata.pane_id);
            assert_eq!(full.terminal_id, metadata.terminal_id);
            assert_eq!(full.workspace_id, metadata.workspace_id);
            assert_eq!(full.tab_id, metadata.tab_id);
            assert_eq!(full.focused, metadata.focused);
            assert_eq!(full.cwd, metadata.cwd);
            assert_eq!(full.agent, metadata.agent);
            assert_eq!(full.agent_status, metadata.agent_status);
            assert_eq!(full.terminal_size, metadata.terminal_size);
            assert_eq!(full.revision, metadata.revision);
        }
        assert_eq!(full.workspaces, metadata.workspaces);
        assert_eq!(full.tabs, metadata.tabs);
        assert_eq!(full.layouts, metadata.layouts);
        assert_eq!(full.agents, metadata.agents);
    }

    #[test]
    fn snapshot_projects_cached_release_and_update_facts() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.integration_recommendations.clear();
        app.state.update_available = Some("0.8.3".into());
        app.state.update_install_command = "herdr update".into();
        app.state.latest_release_notes_available = true;
        app.state.latest_release_notes = Some(crate::release_notes::ReleaseNotes {
            version: "0.8.3".into(),
            body: "### Changed\n- Client shell".into(),
            preview: true,
        });

        let snapshot = snapshot(&app, "boot", 7, None, None);

        assert_eq!(snapshot.update_available.as_deref(), Some("0.8.3"));
        assert_eq!(snapshot.update_install_command, "herdr update");
        assert!(snapshot.latest_release_notes_available);
        assert!(!snapshot.integration_updates_available);
        assert_eq!(
            snapshot.release_notes.as_ref().map(|notes| (
                notes.version.as_str(),
                notes.body.as_str(),
                notes.preview
            )),
            Some(("0.8.3", "### Changed\n- Client shell", true))
        );
    }

    #[test]
    fn snapshot_badges_only_outdated_integrations() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.integration_recommendations =
            vec![crate::integration::IntegrationRecommendation {
                target: crate::api::schema::IntegrationTarget::Claude,
                label: "claude",
                command: "claude",
                available: true,
                path: std::path::PathBuf::from("claude-hook"),
                state: crate::integration::IntegrationStatusKind::NotInstalled,
            }];

        assert!(!snapshot(&app, "boot", 1, None, None).integration_updates_available);

        app.state.integration_recommendations[0].state =
            crate::integration::IntegrationStatusKind::Outdated;
        assert!(snapshot(&app, "boot", 2, None, None).integration_updates_available);
    }

    #[test]
    fn structure_projects_authoritative_repository_spaces_and_checkout_order() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut primary = crate::workspace::Workspace::test_new("primary");
        let mut linked = crate::workspace::Workspace::test_new("linked");
        let primary_id = primary.id.clone();
        let linked_id = linked.id.clone();
        primary.checkout = Some(crate::repository::CheckoutProvenance {
            repository_id: "repo_1".into(),
            checkout_path: "/repo".into(),
            kind: crate::repository::CheckoutKind::Primary,
        });
        linked.checkout = Some(crate::repository::CheckoutProvenance {
            repository_id: "repo_1".into(),
            checkout_path: "/repo-feature".into(),
            kind: crate::repository::CheckoutKind::Linked,
        });
        app.state.workspaces = vec![primary, linked];
        app.state.repositories = vec![crate::repository::Repository {
            id: "repo_1".into(),
            git_common_dir: "/repo/.git".into(),
            label: "repo".into(),
            custom_name: Some("custom repo".into()),
            preferred_base: None,
            checkout_workspace_ids: vec![primary_id.clone(), linked_id.clone()],
            last_focused_workspace_id: Some(linked_id.clone()),
        }];
        app.state.space_order = vec![crate::repository::SpaceRef::Repository("repo_1".into())];
        let item = |id: &str, parent_id: Option<&str>, role, label: &str| {
            crate::extension_contract::HostDocumentItem {
                id: id.into(),
                parent_id: parent_id.map(str::to_owned),
                role,
                label: label.into(),
                detail: None,
                terminal: None,
                action_ids: Vec::new(),
                flags: crate::extension_contract::HostDocumentItemFlags::default(),
                view: None,
            }
        };
        let document = crate::extension_contract::HostDocumentV1 {
            contract: crate::extension_contract::HOST_DOCUMENT_CONTRACT_V1.into(),
            namespace: crate::spaces_extension_contract::SPACES_NAMESPACE.into(),
            generation: 1,
            revision: 1,
            surface: crate::extension_contract::HostSurface::Navigation,
            target: crate::extension_contract::HostTarget::Session,
            items: vec![
                item(
                    "repository:repo_1",
                    None,
                    crate::extension_contract::HostItemRole::Group,
                    "custom repo",
                ),
                item(
                    &format!("workspace:{primary_id}"),
                    Some("repository:repo_1"),
                    crate::extension_contract::HostItemRole::Section,
                    "primary",
                ),
                item(
                    &format!("workspace:{linked_id}"),
                    Some("repository:repo_1"),
                    crate::extension_contract::HostItemRole::Section,
                    "linked",
                ),
            ],
            actions: Vec::new(),
            required_capabilities: Vec::new(),
        };
        app.host_document_caches
            .entry((
                crate::spaces_extension_contract::SPACES_NAMESPACE.into(),
                crate::spaces_extension_contract::SPACES_SCOPE_ID.into(),
            ))
            .or_default()
            .publish_for_namespace(
                crate::spaces_extension_contract::SPACES_NAMESPACE,
                1,
                100,
                &std::collections::HashSet::new(),
                document,
            )
            .unwrap();

        let projected = structure_projection(&app, "boot", 3);

        assert_eq!(projected.repositories.len(), 1);
        assert_eq!(projected.repositories[0].label, "custom repo");
        assert_eq!(
            projected.repositories[0].repository_path.as_deref(),
            Some("/repo")
        );
        assert_eq!(
            projected.repositories[0]
                .checkouts
                .iter()
                .map(|checkout| (
                    checkout.workspace_id.as_str(),
                    checkout.checkout_path.as_deref(),
                    checkout.kind,
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    primary_id.as_str(),
                    Some("/repo"),
                    protocol::endpoint::ClientShellCheckoutKind::Primary,
                ),
                (
                    linked_id.as_str(),
                    Some("/repo-feature"),
                    protocol::endpoint::ClientShellCheckoutKind::Linked,
                ),
            ]
        );
        assert_eq!(projected.space_order[0].id, "repo_1");
    }

    #[test]
    fn pane_surface_exposes_empty_collection_leaf_geometry() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("plain");
        let root = workspace.tabs[0].root_pane;
        let collection = workspace.tabs[0]
            .create_collection_near(
                crate::layout::LayoutLeaf::Pane(root),
                ratatui::layout::Direction::Horizontal,
                0.5,
                Some("empty".into()),
            )
            .unwrap();
        workspace.tabs[0].focus_layout_leaf(crate::layout::LayoutLeaf::Stack(collection.into()));
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);

        let rendered = render_pane_surface(
            &mut app,
            Some(crate::ui::TabSurfaceTarget {
                workspace_index: 0,
                tab_index: 0,
            }),
            Rect::new(0, 0, 80, 20),
            false,
            false,
            crate::kitty_graphics::HostCellSize::default(),
            &crate::kitty_graphics::surface::DeliveryCache::default(),
            1,
        );

        assert_eq!(rendered.collections.len(), 1);
        let projected = &rendered.collections[0];
        assert_eq!(
            projected.collection_id,
            format!("collection_{}", collection.raw())
        );
        assert_eq!(
            projected.rect,
            protocol::SurfaceRect {
                x: 40,
                y: 0,
                width: 40,
                height: 20
            }
        );
        assert_eq!(
            projected.inner_rect,
            protocol::SurfaceRect {
                x: 40,
                y: 1,
                width: 40,
                height: 19
            }
        );
        assert!(projected.focused);
    }

    #[test]
    fn pane_surface_v2_carries_view_descriptors_and_terminal_geometry() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("plain");
        let root = workspace.tabs[0].root_pane;
        let (_, view_pane) = workspace
            .split_pane_with_view(
                root,
                "vqro.tasks",
                "tasks",
                ratatui::layout::Direction::Horizontal,
                0.5,
                false,
            )
            .expect("valid view identity is admitted");
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        let target = Some(crate::ui::TabSurfaceTarget {
            workspace_index: 0,
            tab_index: 0,
        });
        let area = Rect::new(0, 0, 80, 20);
        let delivery = crate::kitty_graphics::surface::DeliveryCache::default();
        let cell_size = crate::kitty_graphics::HostCellSize::default();

        let rendered = render_pane_surface(
            &mut app, target, area, false, false, cell_size, &delivery, 1,
        );
        let view_public = app
            .public_pane_id(0, view_pane)
            .expect("view pane has a public id");
        let root_public = app
            .public_pane_id(0, root)
            .expect("terminal has a public id");
        // The geometry of both panes is in `panes`; the view descriptor names the
        // view's public pane id and the terminal it covers, and both panes keep
        // their own rects (the view is a sibling, not a projection).
        assert_eq!(rendered.panes.len(), 2);
        assert_eq!(rendered.views.len(), 1);
        assert_eq!(rendered.views[0].pane_id, view_public);
        assert_eq!(rendered.views[0].extension, "vqro.tasks");
        assert_eq!(rendered.views[0].view, "tasks");
        assert_eq!(rendered.views[0].scope, vec![root_public.clone()]);
        let view_rect = rendered
            .panes
            .iter()
            .find(|pane| pane.pane_id == view_public)
            .map(|pane| pane.rect)
            .expect("view pane geometry");
        let root_rect = rendered
            .panes
            .iter()
            .find(|pane| pane.pane_id == root_public)
            .expect("terminal pane geometry");
        assert_eq!(
            (
                root_rect.rect.x,
                root_rect.rect.y,
                root_rect.rect.width,
                root_rect.rect.height
            ),
            (0, 0, 80, 20),
            "terminal identity owns the actual outer frame"
        );
        assert_eq!(
            view_rect.width + (view_rect.x - root_rect.inner_rect.x),
            78,
            "body allocation plus view occupy the outer inset"
        );
        assert_eq!(root_rect.inner_rect.x, 1);
        assert!(root_rect.inner_rect.x + root_rect.inner_rect.width <= view_rect.x);
        assert_eq!(view_rect.height, 18);
        assert_eq!(view_rect.y, 1);
        let split = &rendered.splits[0];
        let hit = Rect::new(
            split.hit_rect.x,
            split.hit_rect.y,
            split.hit_rect.width,
            split.hit_rect.height,
        );
        let split_area = Rect::new(
            split.area.x,
            split.area.y,
            split.area.width,
            split.area.height,
        );
        assert_eq!(
            hit.width, 1,
            "only the view border resizes, never terminal content"
        );
        assert_eq!(hit.intersection(split_area), hit);
        assert_eq!(split_area, Rect::new(1, 1, 78, 18));
    }

    #[tokio::test]
    async fn named_view_geometry_is_gathered_once_for_one_and_fifteen_populated_boxes() {
        for count in [1, 15] {
            let (_send, receive) = tokio::sync::mpsc::unbounded_channel();
            let mut app = crate::app::App::new(
                &crate::config::Config::default(),
                crate::app::AppPolicy::TEST,
                None,
                receive,
                crate::api::EventHub::default(),
            );
            let mut workspace = crate::workspace::Workspace::test_new("named views");
            let mut terminals = vec![workspace.tabs[0].root_pane];
            for _ in 1..count {
                terminals.push(workspace.test_split(ratatui::layout::Direction::Horizontal));
            }
            for (index, pane) in terminals.into_iter().enumerate() {
                let collection = workspace.tabs[0]
                    .create_collection_near(
                        crate::layout::LayoutLeaf::Pane(pane),
                        ratatui::layout::Direction::Horizontal,
                        0.5,
                        Some(format!("box {index}")),
                    )
                    .unwrap();
                workspace.tabs[0]
                    .collect_tiled_pane(pane, collection)
                    .unwrap();
                workspace
                    .split_pane_with_view(
                        pane,
                        "sample.views",
                        "rows",
                        ratatui::layout::Direction::Horizontal,
                        0.7,
                        false,
                    )
                    .unwrap();
                workspace.insert_test_runtime(
                    pane,
                    crate::terminal::TerminalRuntime::test_with_screen_bytes(
                        80,
                        24,
                        b"populated terminal\r\nrow two",
                    ),
                );
            }
            app.state.workspaces = vec![workspace];
            app.state.active = Some(0);
            app.state.ensure_test_terminals();
            crate::layout::TileLayout::take_geometry_gathers_for_test();
            let rendered = render_pane_surface(
                &mut app,
                Some(crate::ui::TabSurfaceTarget {
                    workspace_index: 0,
                    tab_index: 0,
                }),
                Rect::new(0, 0, 160, 48),
                false,
                false,
                crate::kitty_graphics::HostCellSize::default(),
                &crate::kitty_graphics::surface::DeliveryCache::default(),
                1,
            );
            assert_eq!(crate::layout::TileLayout::take_geometry_gathers_for_test(),1,"{count} populated named/view groups share a single fact pass including collection projection");
            assert_eq!(rendered.collections.len(), count);
            assert_eq!(rendered.views.len(), count);
            app.state.assert_invariants_for_test();
        }
    }

    #[tokio::test]
    async fn nested_kitty_placements_clip_to_terminal_body_side_and_stacked() {
        for (direction, before) in [
            (ratatui::layout::Direction::Horizontal, false),
            (ratatui::layout::Direction::Vertical, false),
            (ratatui::layout::Direction::Horizontal, true),
            (ratatui::layout::Direction::Vertical, true),
        ] {
            let (_send, receive) = tokio::sync::mpsc::unbounded_channel();
            let mut app = crate::app::App::new(
                &crate::config::Config::default(),
                crate::app::AppPolicy::TEST,
                None,
                receive,
                crate::api::EventHub::default(),
            );
            let mut workspace = crate::workspace::Workspace::test_new("image clipping");
            let terminal = workspace.tabs[0].root_pane;
            let (_, view) = workspace
                .split_pane_with_view(terminal, "sample.views", "rows", direction, 0.7, false)
                .unwrap();
            if before {
                let crate::layout::TypedNode::Split {
                    ratio,
                    first,
                    second,
                    ..
                } = workspace.tabs[0].layout.typed_root().clone()
                else {
                    panic!("expected split")
                };
                workspace.tabs[0].layout = crate::layout::TileLayout::from_tree(
                    crate::layout::TypedNode::Split {
                        direction,
                        ratio: 1.0 - ratio,
                        first: second,
                        second: first,
                    },
                    crate::layout::LayoutLeaf::Pane(terminal),
                )
                .unwrap();
            }
            // Real Kitty bytes enter the real terminal parser; the placement is
            // intentionally much larger than either allocated terminal body.
            workspace.insert_test_runtime(
                terminal,
                crate::terminal::TerminalRuntime::test_with_screen_bytes(
                    80,
                    24,
                    b"\x1b_Ga=T,f=32,t=d,i=7,p=3,s=4,v=4,c=120,r=90,C=1,q=2;/wAA/wD/AP8AAP////8A//8AAP8A/wD/AAD/////AP//AAD/AP8A/wAA/////wD//wAA/wD/AP8AAP////8A/w==\x1b\\",
                ),
            );
            app.state.workspaces = vec![workspace];
            app.state.active = Some(0);
            app.state.ensure_test_terminals();
            let terminal_public = app.public_pane_id(0, terminal).unwrap();
            let view_public = app.public_pane_id(0, view).unwrap();
            let rendered = render_pane_surface(
                &mut app,
                Some(crate::ui::TabSurfaceTarget {
                    workspace_index: 0,
                    tab_index: 0,
                }),
                Rect::new(0, 0, 80, 20),
                true,
                false,
                crate::kitty_graphics::HostCellSize {
                    width_px: 10,
                    height_px: 20,
                },
                &crate::kitty_graphics::surface::DeliveryCache::default(),
                1,
            );
            let terminal = rendered
                .panes
                .iter()
                .find(|pane| pane.pane_id == terminal_public)
                .unwrap();
            let view = rendered
                .panes
                .iter()
                .find(|pane| pane.pane_id == view_public)
                .unwrap();
            let body = Rect::new(
                terminal.inner_rect.x,
                terminal.inner_rect.y,
                terminal.inner_rect.width,
                terminal.inner_rect.height,
            );
            let view_rect = Rect::new(view.rect.x, view.rect.y, view.rect.width, view.rect.height);
            assert!(
                !rendered.graphics.placements.is_empty(),
                "test must actually project a Kitty image"
            );
            if let Ok(root) = std::env::var("VQRO_IMAGE_EVIDENCE_ROOT") {
                let record = serde_json::json!({"columns":80,"rows":20,"cell_width":10,"cell_height":20,"panes":&rendered.panes,"graphics":&rendered.graphics,"frame":&rendered.frame,"body":{"x":body.x,"y":body.y,"width":body.width,"height":body.height},"view":{"x":view_rect.x,"y":view_rect.y,"width":view_rect.width,"height":view_rect.height}});
                let name = match (direction, before) {
                    (ratatui::layout::Direction::Horizontal, false) => "side",
                    (ratatui::layout::Direction::Horizontal, true) => "side-before",
                    (_, false) => "stacked",
                    (_, true) => "stacked-before",
                };
                std::fs::create_dir_all(&root).unwrap();
                std::fs::write(
                    std::path::Path::new(&root).join(format!("{name}.json")),
                    serde_json::to_vec_pretty(&record).unwrap(),
                )
                .unwrap();
            }
            for placement in &rendered.graphics.placements {
                let image = Rect::new(
                    placement.x,
                    placement.y,
                    u16::try_from(placement.cols).unwrap(),
                    u16::try_from(placement.rows).unwrap(),
                );
                assert_eq!(
                    image.intersection(body),
                    image,
                    "image cannot escape terminal body into parent title/border"
                );
                assert!(
                    image.intersection(view_rect).is_empty(),
                    "image cannot cover view/title/frame"
                );
                assert_eq!(placement.x_offset, 0);
                assert_eq!(placement.y_offset, 0);
            }
        }
    }

    #[test]
    fn hidden_and_zoomed_views_do_no_projection_work() {
        use crate::layout::{LayoutLeaf, PaneId, TileLayout, TypedNode};

        let visible = PaneId::from_raw(1);
        let hidden = PaneId::from_raw(2);
        let terminal = PaneId::from_raw(3);
        // A stack whose selected child is one view and whose other child is a
        // hidden view, beside a terminal.
        let stack = TypedNode::Stack {
            id: crate::terminal_container::TerminalContainerId::parse(9_001).unwrap(),
            selected: 0,
            children: vec![
                TypedNode::View {
                    pane: visible,
                    extension: "vqro.tasks".into(),
                    view: "tasks".into(),
                },
                TypedNode::View {
                    pane: hidden,
                    extension: "vqro.tasks".into(),
                    view: "hidden".into(),
                },
            ],
        };
        let tree = TypedNode::Split {
            direction: ratatui::layout::Direction::Horizontal,
            ratio: 0.5,
            first: Box::new(TypedNode::Leaf(terminal)),
            second: Box::new(stack),
        };
        let layout = TileLayout::from_tree(tree, LayoutLeaf::Pane(visible)).unwrap();

        // The frame drew the terminal and the stack's selected view: only that
        // view is walked, so the hidden stack member does no presentation work.
        let drawn = std::collections::HashSet::from([terminal, visible]);
        assert_eq!(drawn_view_panes(&layout, &drawn), vec![visible]);
        // A zoomed view draws alone; a zoomed terminal draws no view at all.
        let zoomed_view = std::collections::HashSet::from([visible]);
        assert_eq!(drawn_view_panes(&layout, &zoomed_view), vec![visible]);
        let zoomed_terminal = std::collections::HashSet::from([terminal]);
        assert!(drawn_view_panes(&layout, &zoomed_terminal).is_empty());
    }

    #[test]
    fn collection_non_agents_never_leak_into_filtered_agent_order() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("plain");
        let root = workspace.tabs[0].root_pane;
        let member = workspace.test_split(ratatui::layout::Direction::Horizontal);
        let collection = workspace.tabs[0]
            .create_collection_near(
                crate::layout::LayoutLeaf::Pane(root),
                ratatui::layout::Direction::Vertical,
                0.5,
                Some("plain terminals".into()),
            )
            .unwrap();
        workspace.tabs[0]
            .collect_tiled_pane(member, collection)
            .unwrap();
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.agent_view_override = Some(crate::api::schema::AgentViewSetParams {
            source: "test".into(),
            label: Some("filtered".into()),
            filter: None,
            sort: Vec::new(),
        });

        let projected = snapshot(&app, "boot", 1, None, None);

        assert_eq!(projected.agent_view_label.as_deref(), Some("filtered"));
        assert!(projected.agents.is_empty());
        assert!(projected.agent_order.is_empty());
        let structure = structure_projection(&app, "boot", 1);
        assert!(structure.collections.is_empty());
        let generic = app.session_snapshot_v1_compat();
        assert_eq!(generic.layouts[0].panes.len(), 2);
    }

    #[test]
    fn client_runtime_facts_do_not_depend_on_bundled_document_providers() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("neutral-facts")];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        let pane = app.state.workspaces[0].focused_pane_id().unwrap();
        let terminal = app.state.workspaces[0]
            .pane_state(pane)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal)
            .unwrap()
            .set_detected_state(
                Some(crate::detect::Agent::Pi),
                crate::detect::AgentState::Working,
            );
        app.state
            .delegations
            .create(Some(pane), None, None)
            .unwrap();
        let before = snapshot(&app, "boot", 1, None, None);
        let structure_before = structure_projection(&app, "boot", 1);
        assert_eq!(before.agents.len(), 1);
        app.disabled_extensions.insert("vqro.agent-status".into());
        app.disabled_extensions.insert("vqro.delegation".into());
        assert_eq!(snapshot(&app, "boot", 1, None, None), before);
        assert_eq!(structure_projection(&app, "boot", 1), structure_before);
    }

    #[test]
    fn unavailable_spaces_hides_projection_without_hiding_generic_workspaces() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut repository_workspace = crate::workspace::Workspace::test_new("repo");
        repository_workspace.checkout = Some(crate::repository::CheckoutProvenance {
            repository_id: "repo_1".into(),
            checkout_path: "/repo".into(),
            kind: crate::repository::CheckoutKind::Primary,
        });
        let repository_workspace_id = repository_workspace.id.clone();
        let standalone = crate::workspace::Workspace::test_new("scratch");
        let standalone_id = standalone.id.clone();
        app.state.workspaces = vec![repository_workspace, standalone];
        app.state.repositories = vec![crate::repository::Repository {
            id: "repo_1".into(),
            git_common_dir: "/repo/.git".into(),
            label: "repo".into(),
            custom_name: None,
            preferred_base: None,
            checkout_workspace_ids: vec![repository_workspace_id],
            last_focused_workspace_id: None,
        }];
        app.configured_disabled_extensions
            .insert("vqro.spaces".into());
        app.apply_bundled_extension_state();

        let projection = structure_projection(&app, "boot", 1);

        assert!(projection.repositories.is_empty());
        assert!(projection.space_order.is_empty());
        let generic = app.session_snapshot_v1_compat();
        assert_eq!(generic.workspaces.len(), 2);
        assert!(generic
            .workspaces
            .iter()
            .any(|workspace| workspace.workspace_id == standalone_id));
    }

    #[test]
    fn split_hits_follow_released_border_and_gap_geometry() {
        let horizontal = crate::layout::SplitBorder {
            pos: 20,
            direction: ratatui::layout::Direction::Horizontal,
            ratio: 0.5,
            area: Rect::new(2, 3, 40, 12),
            path: vec![false],
            independent_borders: false,
            view_before: false,
        };
        assert_eq!(
            split_hit_rect(&horizontal, true, false, &[]),
            Some(Rect::new(20, 3, 1, 12))
        );
        assert_eq!(
            split_hit_rect(&horizontal, true, true, &[]),
            Some(Rect::new(19, 3, 2, 12))
        );
        assert_eq!(
            split_hit_rect(&horizontal, false, true, &[]),
            Some(Rect::new(19, 3, 1, 12))
        );
        assert_eq!(split_hit_rect(&horizontal, false, false, &[]), None);

        let vertical = crate::layout::SplitBorder {
            pos: 9,
            direction: ratatui::layout::Direction::Vertical,
            ratio: 0.5,
            area: Rect::new(2, 3, 40, 12),
            path: vec![true],
            independent_borders: false,
            view_before: false,
        };
        assert_eq!(
            split_hit_rect(&vertical, true, true, &[]),
            Some(Rect::new(2, 8, 40, 2))
        );

        let edge = crate::layout::SplitBorder {
            pos: 0,
            direction: ratatui::layout::Direction::Horizontal,
            ratio: 0.5,
            area: Rect::new(0, 0, 1, 4),
            path: Vec::new(),
            independent_borders: false,
            view_before: false,
        };
        assert_eq!(
            split_hit_rect(&edge, true, true, &[]),
            Some(Rect::new(0, 0, 1, 4))
        );
        assert_eq!(split_hit_rect(&edge, false, true, &[]), None);
        assert_eq!(
            split_hit_rect(&horizontal, false, true, &[Rect::new(19, 3, 1, 12)]),
            None
        );
    }
}
