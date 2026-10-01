use super::render::put_text;
use super::*;

pub(super) fn render_collapsed(
    buffer: &mut Buffer,
    area: Rect,
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    config: &ClientShellConfig,
    hits: &mut ShellHitMap,
) {
    let rows = agent_rows(endpoints, active_endpoint_id, config);
    for (index, row) in rows.into_iter().take(area.height as usize).enumerate() {
        let rect = Rect::new(area.x, area.y + index as u16, area.width, 1);
        if row.agent.focused {
            buffer.set_style(rect, Style::default().bg(config.palette.active_row_bg));
        }
        let initial = row.machine_label.chars().next().unwrap_or('?');
        put_text(
            buffer,
            rect.x,
            rect.y,
            rect.width,
            &format!(
                "{initial}{}",
                status_icon(row.agent.status, config.status_indicators)
            ),
            Style::default()
                .fg(if row.stale {
                    config.palette.overlay0
                } else {
                    status_color(row.agent.status, &config.palette)
                })
                .add_modifier(if row.stale {
                    Modifier::DIM
                } else {
                    Modifier::empty()
                }),
        );
        hits.endpoint_agents
            .push((rect, row.endpoint_id, row.agent.pane_id));
    }
}

pub(super) fn render_expanded(
    buffer: &mut Buffer,
    area: Rect,
    agent_view_label: Option<&str>,
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    config: &ClientShellConfig,
    agent_scroll: &mut usize,
    hits: &mut ShellHitMap,
) {
    if !super::agent_sidebar::render_agent_panel_header(
        buffer,
        area,
        agent_view_label,
        config,
        hits,
    ) {
        return;
    }
    let rows = agent_rows(endpoints, active_endpoint_id, config);
    super::agent_sidebar::render_agent_list(
        buffer,
        area,
        &rows,
        agent_view_label.map(|_| " no matching agents"),
        config,
        agent_scroll,
        hits,
        |row| row.agent.rows.len() + usize::from(row.agent.group_label.is_some()),
        |buffer, rect, row, hits| {
            super::agent_sidebar::render_agent_row(buffer, rect, &row.agent, config);
            if row.stale {
                buffer.set_style(
                    rect,
                    Style::default()
                        .fg(config.palette.overlay0)
                        .add_modifier(Modifier::DIM),
                );
            }
            let header_rows = u16::from(row.agent.group_label.is_some());
            hits.endpoint_agents.push((
                Rect::new(
                    rect.x,
                    rect.y.saturating_add(header_rows),
                    rect.width,
                    rect.height.saturating_sub(header_rows),
                ),
                row.endpoint_id.clone(),
                row.agent.pane_id.clone(),
            ));
        },
    );
}

struct EndpointAgentRow {
    endpoint_id: ClientEndpointId,
    machine_label: String,
    stale: bool,
    agent: super::agent_sidebar::AgentRow,
}

fn agent_rows(
    endpoints: &[ClientShellEndpoint],
    active_endpoint_id: &ClientEndpointId,
    config: &ClientShellConfig,
) -> Vec<EndpointAgentRow> {
    let suppress_document_groups = endpoints
        .iter()
        .find(|endpoint| &endpoint.endpoint_id == active_endpoint_id)
        .is_some_and(|endpoint| {
            endpoint
                .snapshot
                .as_deref()
                .is_some_and(|snapshot| snapshot.agent_view_label.is_some())
                || ClientShellState::endpoint_agent_view(endpoint)
                    .is_some_and(|view| matches!(view, Ok(Some(_))))
        });
    let navigation = super::aggregate_navigation::agent_navigation_cache(endpoints);
    let mut rendered_rows = endpoints
        .iter()
        .filter_map(|endpoint| {
            endpoint.snapshot.as_deref().map(|snapshot| {
                let context = super::extension_context::navigation(
                    snapshot,
                    endpoint.current_host_documents(),
                );
                let documented_by_pane = navigation
                    .get(&endpoint.endpoint_id)
                    .and_then(Option::as_ref)
                    .map(|rows| {
                        rows.iter()
                            .map(|row| (row.pane_id.as_str(), row))
                            .collect::<HashMap<_, _>>()
                    });
                snapshot
                    .agents
                    .iter()
                    .filter_map(|agent| {
                        let documented = documented_by_pane
                            .as_ref()
                            .and_then(|rows| rows.get(agent.pane_id.as_str()).copied());
                        let mut row = super::agent_sidebar::agent_row_with_document(
                            snapshot,
                            &agent.pane_id,
                            config,
                            Some(&endpoint.label),
                            documented,
                        )?;
                        super::agent_sidebar::append_context(&mut row, context.get(&agent.pane_id));
                        Some(row)
                    })
                    .map(|agent| ((endpoint.endpoint_id.clone(), agent.pane_id.clone()), agent))
                    .collect::<Vec<_>>()
            })
        })
        .flatten()
        .collect::<HashMap<_, _>>();

    super::aggregate_navigation::aggregate_agent_rows_cached(
        endpoints,
        active_endpoint_id,
        config.agent_panel_sort,
        &navigation,
    )
    .into_iter()
    .filter_map(|row| {
        let key = (row.endpoint.endpoint_id.clone(), row.agent.pane_id.clone());
        let mut agent = rendered_rows.remove(&key)?;
        agent.focused &= row.endpoint.endpoint_id == active_endpoint_id;
        if suppress_document_groups {
            agent.group_label = None;
        }
        Some(EndpointAgentRow {
            endpoint_id: row.endpoint.endpoint_id.clone(),
            machine_label: row.endpoint.label.to_owned(),
            stale: row.endpoint.stale(),
            agent,
        })
    })
    .collect()
}
