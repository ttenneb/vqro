//! Cached actionless attention presentation. Meaning/counts belong to the publisher.
use super::render::{display_width, put_text};
use super::*;
use crate::extension_contract::{HostItemRole, HostSurface, HostTarget};
use crate::protocol::endpoint::HostDocumentsProjectionV1;

#[derive(Clone, Debug, Default)]
pub(super) struct AttentionNavigation {
    pub(super) plugin: String,
    pub(super) entrypoint: String,
    pub(super) label: String,
    pub(super) document_key: (String, u64, u64),
    measures: [String; 3],
    pub(super) badges: HashMap<String, HashMap<String, AttentionBadge>>,
    pub(super) rows: Vec<AttentionRow>,
}

/// One row of the sidebar's attention (Tasks) list. `pane_id` is the pane the row
/// belongs to when the extension published a terminal link for it: the owner's pane,
/// or the pane where the human acts. A row without one is an agent that has no live
/// pane, so there is nowhere to jump and the row says so instead of doing nothing.
#[derive(Clone, Debug)]
pub(super) struct AttentionRow {
    pub(super) label: String,
    pub(super) detail: String,
    pub(super) pane_id: Option<String>,
}
#[derive(Clone, Debug)]
pub(super) struct AttentionBadge {
    pub(super) label: String,
    counts: [u64; 3],
}
impl AttentionNavigation {
    pub(super) fn badge(&self, kind: &str, id: &str) -> Option<&AttentionBadge> {
        self.badges.get(kind)?.get(id)
    }
    pub(super) fn hit(&self, rect: Rect, kind: &str, ids: Vec<String>) -> AttentionHit {
        AttentionHit {
            rect,
            plugin: self.plugin.clone(),
            entrypoint: self.entrypoint.clone(),
            kind: kind.into(),
            ids,
            document_key: self.document_key.clone(),
        }
    }
    fn combined(&self, ids: &[String]) -> Option<String> {
        let mut counts = [0_u64; 3];
        for id in ids {
            if let Some(badge) = self.badge("workspace", id) {
                for (sum, value) in counts.iter_mut().zip(badge.counts) {
                    *sum = sum.saturating_add(value);
                }
            }
        }
        let labels = counts
            .iter()
            .zip(&self.measures)
            .filter(|(count, _)| **count > 0)
            .map(|(count, label)| format!("{count} {label}"))
            .collect::<Vec<_>>();
        (!labels.is_empty() && self.measures.iter().all(|label| !label.is_empty()))
            .then(|| labels.join(" · "))
    }
}

pub(super) fn navigation(projection: &HostDocumentsProjectionV1) -> Option<AttentionNavigation> {
    let document = &projection
        .documents
        .iter()
        .filter(|entry| {
            let doc = &entry.document;
            // Attention items are selected by convention, not by document
            // shape: one view may also carry panel blocks the attention
            // renderer must ignore. Every attention item must still be an
            // enabled section.
            doc.surface == HostSurface::Navigation
                && doc.target == HostTarget::Session
                && doc.actions.is_empty()
                && doc.required_capabilities.is_empty()
                && doc
                    .items
                    .iter()
                    .filter(|item| item.id.starts_with("context:attention."))
                    .all(|item| item.role == HostItemRole::Section && !item.flags.disabled)
                && doc
                    .items
                    .iter()
                    .any(|item| item.id.starts_with("context:attention.view:"))
        })
        .min_by(|left, right| {
            right
                .provider_priority
                .cmp(&left.provider_priority)
                .then_with(|| left.document.namespace.cmp(&right.document.namespace))
        })?
        .document;
    let launch = document
        .items
        .iter()
        .find(|item| item.id.starts_with("context:attention.view:"))?;
    let (plugin, entrypoint) = launch
        .id
        .strip_prefix("context:attention.view:")?
        .split_once(':')?;
    if plugin.is_empty() || entrypoint.is_empty() || entrypoint.contains(':') {
        return None;
    }
    let mut result = AttentionNavigation {
        plugin: plugin.into(),
        entrypoint: entrypoint.into(),
        label: launch.label.clone(),
        document_key: (
            document.namespace.clone(),
            document.generation,
            document.revision,
        ),
        ..Default::default()
    };
    for item in &document.items {
        if let Some(index) = item.id.strip_prefix("context:attention.measure:") {
            let index = index.parse::<usize>().ok()?;
            if index >= 3 || !result.measures[index].is_empty() {
                return None;
            }
            result.measures[index] = item.label.clone();
        } else if item.id.starts_with("context:attention.row:")
            || item.id.starts_with("context:attention.overflow:")
        {
            result.rows.push(AttentionRow {
                label: item.label.clone(),
                detail: item.detail.clone().unwrap_or_default(),
                pane_id: item
                    .terminal
                    .as_ref()
                    .and_then(|terminal| terminal.placement.as_ref())
                    .map(|placement| placement.pane_id.clone()),
            });
        } else if let Some(id) = item.id.strip_prefix("context:attention.badge:") {
            let mut parts = id.rsplitn(4, ':');
            let third = parts.next()?.parse::<u64>().ok()?;
            let second = parts.next()?.parse::<u64>().ok()?;
            let first = parts.next()?.parse::<u64>().ok()?;
            let (kind, id) = parts.next()?.split_once(':')?;
            if !matches!(kind, "workspace" | "tab" | "session") {
                return None;
            }
            if result
                .badges
                .entry(kind.into())
                .or_default()
                .insert(
                    id.into(),
                    AttentionBadge {
                        label: item.label.clone(),
                        counts: [first, second, third],
                    },
                )
                .is_some()
            {
                return None;
            }
        }
    }
    Some(result)
}

/// Space membership comes only from Spaces' current admitted checkout projection.
/// This is a presentation join/sum, never task classification or a second task store.
pub(super) fn render_workspace_badge(
    buffer: &mut Buffer,
    rect: Rect,
    nav: Option<&AttentionNavigation>,
    workspace: &str,
    repository: Option<&crate::protocol::endpoint::ClientShellRepository>,
    palette: &Palette,
) {
    let Some(nav) = nav else {
        return;
    };
    let ids = repository.map_or_else(
        || vec![workspace.to_string()],
        |repository| {
            repository
                .checkouts
                .iter()
                .map(|checkout| checkout.workspace_id.clone())
                .collect()
        },
    );
    let label = if repository.is_some() {
        nav.combined(&ids)
    } else {
        nav.badge("workspace", workspace)
            .map(|badge| badge.label.clone())
    };
    let Some(label) = label else {
        return;
    };
    if rect.width < 8 {
        return;
    }
    let badge_rect = if rect.height >= 2 {
        Rect::new(rect.x + 2, rect.y + 1, rect.width.saturating_sub(4), 1)
    } else {
        let width = display_width(&label)
            .saturating_add(2)
            .min(rect.width.saturating_sub(7));
        Rect::new(rect.right().saturating_sub(width + 1), rect.y, width, 1)
    };
    put_text(
        buffer,
        badge_rect.x,
        badge_rect.y,
        badge_rect.width,
        &format!("[{label}]"),
        Style::default().fg(palette.peach),
    );
    // No hit of its own, for the same reason as a tab badge: a click anywhere on the
    // row, badge included, is the row's own action.
}

pub(super) fn render_panel(
    buffer: &mut Buffer,
    area: Rect,
    nav: Option<&AttentionNavigation>,
    config: &ClientShellConfig,
    scroll: &mut usize,
    hits: &mut ShellHitMap,
) {
    if area.height < 2 {
        return;
    }
    let p = &config.palette;
    put_text(
        buffer,
        area.x,
        area.y,
        area.width,
        &"─".repeat(area.width as usize),
        Style::default().fg(p.surface_dim),
    );
    put_text(
        buffer,
        area.x,
        area.y + 1,
        area.width,
        &format!(" {}", nav.map_or("Tasks", |nav| nav.label.as_str())),
        Style::default().fg(p.overlay0).add_modifier(Modifier::BOLD),
    );
    let toggle = Rect::new(
        area.right().saturating_sub(8),
        area.y + 1,
        8.min(area.width),
        1,
    );
    put_text(
        buffer,
        toggle.x,
        toggle.y,
        toggle.width,
        "[agents]",
        Style::default().fg(p.accent),
    );
    hits.attention_toggle = toggle;
    if let Some(nav) = nav {
        hits.attention.push(nav.hit(
            Rect::new(area.x, area.y + 1, area.width.saturating_sub(8), 1),
            "all",
            Vec::new(),
        ));
    }
    let body = Rect::new(
        area.x,
        area.y + 2,
        area.width,
        area.height.saturating_sub(2),
    );
    let Some(nav) = nav else {
        put_text(
            buffer,
            body.x,
            body.y,
            body.width,
            " Tasks unavailable",
            Style::default().fg(p.peach),
        );
        return;
    };
    if nav.rows.is_empty() {
        put_text(
            buffer,
            body.x,
            body.y,
            body.width,
            " No durable attention",
            Style::default().fg(p.overlay0),
        );
        return;
    }
    if let Some(global) = nav.badge("session", "global") {
        put_text(
            buffer,
            body.x,
            body.y,
            body.width,
            &format!(" Global: {}", global.label),
            Style::default().fg(p.peach),
        );
        hits.attention.push(nav.hit(
            Rect::new(body.x, body.y, body.width, 1),
            "session",
            vec!["global".into()],
        ));
    }
    let start = usize::from(nav.badge("session", "global").is_some());
    let page = usize::from(body.height).saturating_sub(start) / 2;
    *scroll = (*scroll).min(nav.rows.len().saturating_sub(page.max(1)));
    hits.agent_body = body;
    hits.agent_max_scroll = nav.rows.len().saturating_sub(page.max(1));
    for (offset, row) in nav.rows.iter().skip(*scroll).take(page).enumerate() {
        let y = body.y + (start + offset * 2) as u16;
        // A row whose agent has no live pane has nowhere to jump, so it says so in
        // its own line rather than looking clickable and doing nothing.
        let live = row.pane_id.is_some();
        let (label_style, detail_style) = if live {
            (Style::default().fg(p.text), Style::default().fg(p.overlay0))
        } else {
            (
                Style::default().fg(p.overlay0),
                Style::default().fg(p.overlay0),
            )
        };
        put_text(
            buffer,
            body.x,
            y,
            body.width,
            &format!(" {}", row.label),
            label_style,
        );
        // Packages own row meaning: an unlinked row need not describe execution.
        // Retain the execution fallback only when no detail was supplied.
        let detail = if live || !row.detail.is_empty() {
            row.detail.clone()
        } else {
            "not running".to_string()
        };
        put_text(
            buffer,
            body.x,
            y + 1,
            body.width,
            &format!(" {detail}"),
            detail_style,
        );
        // Both lines of the row are the row: the whole entry is one target.
        hits.attention.push(AttentionHit {
            rect: Rect::new(body.x, y, body.width, 2),
            plugin: nav.plugin.clone(),
            entrypoint: nav.entrypoint.clone(),
            kind: "attention_row".into(),
            ids: row
                .pane_id
                .clone()
                .map(|pane_id| vec![pane_id])
                .unwrap_or_default(),
            document_key: nav.document_key.clone(),
        });
    }
}

pub(super) fn render_agents_toggle(
    buffer: &mut Buffer,
    area: Rect,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) {
    if area.height < 2 {
        return;
    }
    let rect = Rect::new(
        area.x.saturating_add(8),
        area.y + 1,
        7.min(area.width.saturating_sub(8)),
        1,
    );
    put_text(
        buffer,
        rect.x,
        rect.y,
        rect.width,
        "[tasks]",
        Style::default().fg(config.palette.accent),
    );
    hits.attention_toggle = rect;
}

impl ClientShellState {
    /// A badge click opens the pane panels of the scope it covers, in this
    /// client. The extension's own tab view is retired: a badge is a way into
    /// the panel, not a way into another terminal app.
    pub(super) fn open_attention(&mut self, launch: AttentionHit, outcome: &mut ClientShellInput) {
        let current = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .filter(|endpoint| endpoint.current_host_documents().is_some())
            .and_then(|endpoint| endpoint.attention.as_ref());
        if !current.is_some_and(|nav| {
            nav.plugin == launch.plugin
                && nav.entrypoint == launch.entrypoint
                && nav.document_key == launch.document_key
        }) {
            return;
        }
        let Some(documents) = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
            .and_then(ClientShellEndpoint::current_host_documents)
            .cloned()
        else {
            return;
        };
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        // A row of the list is one target: the pane it belongs to. Clicking it goes
        // there, which is the primary action a task entry owes the user. A row whose
        // agent has no live pane says so in the row and reports that here, rather
        // than silently doing nothing.
        if launch.kind == "attention_row" {
            match launch
                .ids
                .first()
                .filter(|pane_id| snapshot.panes.iter().any(|pane| &pane.pane_id == *pane_id))
            {
                Some(pane_id) => self.push_endpoint_method(
                    crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                        pane_id: pane_id.clone(),
                    }),
                    outcome,
                ),
                None => {
                    self.endpoint_error = Some(
                        "that entry's agent has no running pane, so there is nowhere to jump"
                            .to_string(),
                    );
                }
            }
            return;
        }
        // The scope the badge covers: the panes of those panes' tabs.
        let mut panes: Vec<(String, String)> = Vec::new();
        for pane in &snapshot.panes {
            let in_scope = match launch.kind.as_str() {
                "workspace" => launch.ids.iter().any(|id| id == &pane.workspace_id),
                "tab" => launch.ids.iter().any(|id| id == &pane.tab_id),
                "session" => true,
                _ => false,
            };
            if !in_scope {
                continue;
            }
            for view_id in super::pane_panel::published_views(&documents, &pane.pane_id) {
                panes.push((pane.pane_id.clone(), view_id));
            }
        }
        if panes.is_empty() {
            // Nothing to open here; keep the terminal usable rather than
            // launching a foreign tab view.
            self.set_endpoint_error("no extension views for this scope".to_string());
            outcome.repaint = true;
            return;
        }
        for (pane_id, view_id) in panes {
            self.pane_panel_open
                .entry(pane_id.clone())
                .or_default()
                .insert(view_id);
            self.pane_panel_display.entry(pane_id).or_default().refit();
        }
        self.persist_pane_panels();
        outcome.repaint = true;
        outcome.resize = true;
    }
}

#[derive(Clone, Debug)]
pub(super) struct AttentionHit {
    pub(super) rect: Rect,
    pub(super) plugin: String,
    pub(super) entrypoint: String,
    pub(super) kind: String,
    pub(super) ids: Vec<String>,
    pub(super) document_key: (String, u64, u64),
}
