use std::hint::black_box;
use std::time::{Duration, Instant};

use ratatui::layout::{Direction, Rect};

use crate::app::{App, AppPolicy};
use crate::client::{ClientShellConfig, ClientShellState};
use crate::config::Config;
use crate::kitty_graphics::HostCellSize;
use crate::layout::PaneId;
use crate::protocol::PaneSurfaceFrame;
use crate::terminal::TerminalRuntime;
use crate::workspace::Workspace;

// A client geometry that leaves room for fifteen real panes beside the default
// sidebar: the pane surface is about 210x49, so a 5x3 grid gives every pane
// 42x16. Without that, "15 panes" were fifteen hits on one tiny rect and nothing
// pane-sized (borders, scrollbars, panels) could draw.
const COLS: u16 = 240;
const ROWS: u16 = 50;
/// Every populated pane in a fixture is at least this size.
const MIN_PANE_COLS: u16 = 40;
const MIN_PANE_ROWS: u16 = 12;
const SAMPLE_COUNT: usize = 40;
const WARMUP_COUNT: usize = 5;
const CARDINALITIES: [usize; 3] = [1, 15, 50];
const CLIENT_CARDINALITIES: [usize; 2] = [1, 4];
const SCROLLBACK_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy)]
struct StageStats {
    median_us: u128,
    p95_us: u128,
    max_us: u128,
}

struct PipelineStats {
    server: StageStats,
    client: StageStats,
    total: StageStats,
}

struct RenderPipeline {
    app: App,
    client: ClientShellState,
    graphics_delivery: crate::kitty_graphics::surface::DeliveryCache,
    surface_revision: u64,
}

impl RenderPipeline {
    fn new(workspaces: Vec<Workspace>) -> Self {
        Self::with_config(workspaces, &Config::default())
    }

    fn with_config(workspaces: Vec<Workspace>, config: &Config) -> Self {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            config,
            AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = workspaces;
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.pane_scrollbars = true;

        let mut client = ClientShellState::new(ClientShellConfig::from_config(config));
        client.set_endpoint_capabilities_for(
            &crate::client::endpoint::ClientEndpointId::Local,
            vec![
                crate::protocol::collapse::LAYOUT_COLLAPSE_CAPABILITY.into(),
                crate::native_api::NATIVE_API_CAPABILITY_V1.into(),
            ],
        );
        client.set_endpoint_methods(Some(vec!["layout.collapse".into(), "layout.expand".into()]));
        client.set_snapshot(Box::new(super::client_shell::snapshot(
            &app,
            "bench-boot",
            1,
            None,
            None,
        )));

        Self {
            app,
            client,
            graphics_delivery: crate::kitty_graphics::surface::DeliveryCache::default(),
            surface_revision: 0,
        }
    }

    fn render_once(&mut self) -> (Duration, Duration) {
        let surface_size = self.client.surface_size(COLS, ROWS);
        let started = Instant::now();
        let target = self
            .app
            .state
            .active
            .map(|workspace_index| crate::ui::TabSurfaceTarget {
                workspace_index,
                tab_index: self.app.state.workspaces[workspace_index].active_tab_index(),
            });
        let rendered = crate::server::client_shell::render_pane_surface(
            &mut self.app,
            target,
            Rect::new(0, 0, surface_size.cols, surface_size.rows),
            true,
            true,
            HostCellSize {
                width_px: 1,
                height_px: 1,
            },
            &self.graphics_delivery,
            1,
            true,
        );
        let server_elapsed = started.elapsed();
        self.graphics_delivery = rendered.graphics_delivery;

        let started = Instant::now();
        self.surface_revision += 1;
        let collapse = rendered.collapse;
        self.client.set_pane_surface_with_views(
            PaneSurfaceFrame {
                boot_id: "bench-boot".into(),
                projection_revision: 1,
                surface_revision: self.surface_revision,
                frame: rendered.frame,
                panes: rendered.panes,
                collections: rendered.collections,
                splits: rendered.splits,
                popup: rendered.popup,
                graphics: rendered.graphics,
            },
            rendered.views,
        );
        if let Some(body) = collapse {
            self.client.set_endpoint_collapse_projection_for_generation(
                &crate::client::endpoint::ClientEndpointId::Local,
                0,
                Ok(Box::new(crate::protocol::collapse::CollapseProjectionV1 {
                    boot_id: "bench-boot".into(),
                    projection_revision: 1,
                    base_surface_revision: self.surface_revision,
                    body,
                })),
            );
        }
        black_box(
            self.client
                .compose(COLS, ROWS)
                .expect("benchmark pipeline should compose a complete frame"),
        );
        let client_elapsed = started.elapsed();

        (server_elapsed, client_elapsed)
    }
}

fn history() -> String {
    (0..2_000).map(|line| format!("line-{line}\r\n")).collect()
}

/// The client's pane area at the fixed benchmark geometry: what a pane fixture
/// must tile inside, and what the client renders into.
fn pane_surface_rect() -> Rect {
    let client = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    let size = client.surface_size(COLS, ROWS);
    Rect::new(0, 0, size.cols.max(1), size.rows.max(1))
}

/// How many panes tile `area` at the fixture minimum, and in what grid. Panes
/// beyond the capacity of one tab go to extra tabs of the same workspace.
fn grid_for(area: Rect, panes: usize) -> (usize, usize) {
    let max_cols = (area.width / MIN_PANE_COLS).max(1) as usize;
    let max_rows = (area.height / MIN_PANE_ROWS).max(1) as usize;
    let cols = max_cols.min(panes.max(1));
    let rows = panes.max(1).div_ceil(cols).min(max_rows);
    (cols, rows.max(1))
}

/// A runtime sized like the pane that owns it, holding real scrollback content
/// plus recent output, so the pane renders a populated screen.
fn populated_runtime(rect: Rect, history: &str) -> TerminalRuntime {
    let cols = rect.width.max(1);
    let rows = rect.height.max(1);
    let runtime = TerminalRuntime::test_with_scrollback_bytes(
        cols,
        rows,
        SCROLLBACK_BYTES,
        history.as_bytes(),
    );
    runtime.test_process_pty_bytes(b"$ bench output arriving in this pane\r\n");
    runtime
}

/// Tiles the active tab into a `cols` x `rows` grid and returns its pane ids.
/// Each split takes an equal share of what is left, so the panes come out even
/// rather than halving down to slivers.
fn tile_active_tab(workspace: &mut Workspace, cols: usize, rows: usize) -> Vec<PaneId> {
    let tab_idx = workspace.active_tab_index();
    let mut row_roots = vec![workspace.tabs[tab_idx].root_pane];
    for index in 1..rows {
        let last = *row_roots.last().expect("a row to split");
        workspace.tabs[tab_idx].layout.focus_pane(last);
        let ratio = 1.0 / (rows - index + 1) as f32;
        row_roots.push(workspace.test_split_with_ratio(Direction::Vertical, ratio));
    }

    let mut pane_ids = Vec::with_capacity(cols * rows);
    for root in row_roots {
        pane_ids.push(root);
        let mut last = root;
        for index in 1..cols {
            workspace.tabs[tab_idx].layout.focus_pane(last);
            let ratio = 1.0 / (cols - index + 1) as f32;
            last = workspace.test_split_with_ratio(Direction::Horizontal, ratio);
            pane_ids.push(last);
        }
    }
    pane_ids
}

/// A populated tab: every pane sized for its own rect, with content and output.
fn populate_tab(
    workspace: &mut Workspace,
    pane_count: usize,
    surface: Rect,
    history: &str,
) -> Vec<PaneId> {
    if pane_count == 0 {
        return Vec::new();
    }
    let (cols, rows) = grid_for(surface, pane_count);
    let pane_ids = tile_active_tab(workspace, cols, rows.min(pane_count.div_ceil(cols)));
    let tab_idx = workspace.active_tab_index();
    let rects = workspace.tabs[tab_idx]
        .layout
        .panes(surface)
        .into_iter()
        .map(|info| (info.id, info.rect))
        .collect::<std::collections::HashMap<_, _>>();
    for pane_id in pane_ids
        .iter()
        .filter(|id| rects.contains_key(id))
        .take(pane_count)
    {
        let rect = rects[pane_id];
        workspace.insert_test_runtime(*pane_id, populated_runtime(rect, history));
    }
    pane_ids
}

/// Populates `pane_count` panes across the surfaces the client can actually
/// show at `COLS` x `ROWS`: the active tab is a real grid, and panes past its
/// capacity live in further tabs of the same workspace, still populated and
/// still parsed, but not on screen at the same time.
fn populated_panes(pane_count: usize) -> Workspace {
    let history = history();
    let surface = pane_surface_rect();
    let (cols, rows) = grid_for(surface, pane_count);
    let capacity = (cols * rows).max(1);
    let mut remaining = pane_count.max(1);
    let mut workspace = Workspace::test_new("bench");
    let first = remaining.min(capacity);
    populate_tab(&mut workspace, first, surface, &history);
    remaining -= first;
    while remaining > 0 {
        let tab_idx = workspace.test_add_tab(None);
        // The tiler works in the active tab, so make the new one active.
        workspace.switch_tab(tab_idx);
        let next = remaining.min(capacity);
        populate_tab(&mut workspace, next, surface, &history);
        remaining -= next;
    }
    let first_root = workspace.tabs[0].root_pane;
    workspace.switch_tab(0);
    workspace.tabs[0].layout.focus_pane(first_root);
    workspace
}

fn workspaces(workspace_count: usize) -> Vec<Workspace> {
    let history = history();
    let surface = pane_surface_rect();
    (0..workspace_count)
        .map(|index| {
            let mut workspace = Workspace::test_new(&format!("bench-{}", index + 1));
            let root_pane = workspace.tabs[0].root_pane;
            workspace.insert_test_runtime(root_pane, populated_runtime(surface, &history));
            workspace
        })
        .collect()
}

fn active_panes(pane_count: usize) -> Vec<Workspace> {
    vec![populated_panes(pane_count)]
}

fn zoomed_panes(pane_count: usize) -> Vec<Workspace> {
    let mut workspaces = active_panes(pane_count);
    workspaces[0].tabs[0].zoomed = true;
    workspaces
}

/// A workspace whose active tab holds `view_count` view panes beside a populated
/// terminal, so the surface projection walks `view_count` drawn views, each with
/// positive geometry. Splitting the largest pane along its longer axis keeps the
/// views pane-sized instead of halving one branch into slivers.
///
/// This exercises descriptor/scope projection (content lookup plus the
/// placement-scope walk), not populated view content composition; no document
/// fixture is loaded.
fn view_populated(view_count: usize) -> Workspace {
    let history = history();
    let surface = pane_surface_rect();
    let mut workspace = Workspace::test_new("bench-views");
    let root = workspace.tabs[0].root_pane;
    workspace.insert_test_runtime(root, populated_runtime(surface, &history));
    for _ in 0..view_count {
        let Some((target, direction)) = workspace.tabs[0]
            .layout
            .panes(surface)
            .into_iter()
            // A terminal parent rect includes its attached views. Split the
            // largest actual allocation, not that overlapping outer identity.
            .max_by_key(|pane| pane.inner_rect.area())
            .map(|pane| {
                let direction = if pane.inner_rect.width >= pane.inner_rect.height {
                    Direction::Horizontal
                } else {
                    Direction::Vertical
                };
                (pane.id, direction)
            })
        else {
            break;
        };
        if workspace
            .split_pane_with_view(target, "bench.views", "v", direction, 0.5, false)
            .is_none()
        {
            break;
        }
    }
    workspace
}

fn view_panes(view_count: usize) -> Vec<Workspace> {
    vec![view_populated(view_count)]
}

fn named_view_groups(count: usize) -> Vec<Workspace> {
    let mut workspace = populated_panes(count);
    for tab_index in 0..workspace.tabs.len() {
        let terminals = workspace.tabs[tab_index]
            .layout
            .panes(pane_surface_rect())
            .into_iter()
            .map(|pane| pane.id)
            .collect::<Vec<_>>();
        for (index, pane) in terminals.into_iter().enumerate() {
            let collection = workspace.tabs[tab_index]
                .create_collection_near(
                    crate::layout::LayoutLeaf::Pane(pane),
                    Direction::Horizontal,
                    0.5,
                    Some(format!("named {index}")),
                )
                .expect("named fixture box");
            workspace.tabs[tab_index]
                .collect_tiled_pane(pane, collection)
                .expect("populated named member");
            workspace
                .split_pane_with_view(pane, "bench.views", "v", Direction::Horizontal, 0.7, false)
                .expect("attached fixture view");
        }
    }
    vec![workspace]
}

/// Output keeps arriving between samples, so each sample renders a screen that
/// just changed rather than one frozen frame. The feed itself is outside the
/// measured region.
fn feed_output(app: &mut App) {
    for workspace in &app.state.workspaces {
        for runtime in workspace.test_runtimes.values() {
            runtime.test_process_pty_bytes(b"> bench sample output\r\n");
        }
    }
}

fn summarize(mut samples: Vec<Duration>) -> StageStats {
    samples.sort_unstable();
    StageStats {
        median_us: samples[SAMPLE_COUNT / 2].as_micros(),
        p95_us: samples[(SAMPLE_COUNT - 1) * 95 / 100].as_micros(),
        max_us: samples[SAMPLE_COUNT - 1].as_micros(),
    }
}

fn profile(build: fn(usize) -> Vec<Workspace>, count: usize) -> PipelineStats {
    profile_pipeline(RenderPipeline::new(build(count)))
}

fn profile_pipeline(mut pipeline: RenderPipeline) -> PipelineStats {
    for _ in 0..WARMUP_COUNT {
        feed_output(&mut pipeline.app);
        black_box(pipeline.render_once());
    }

    let mut server = Vec::with_capacity(SAMPLE_COUNT);
    let mut client = Vec::with_capacity(SAMPLE_COUNT);
    let mut total = Vec::with_capacity(SAMPLE_COUNT);
    for _ in 0..SAMPLE_COUNT {
        feed_output(&mut pipeline.app);
        let (server_elapsed, client_elapsed) = pipeline.render_once();
        server.push(server_elapsed);
        client.push(client_elapsed);
        total.push(server_elapsed + client_elapsed);
    }

    PipelineStats {
        server: summarize(server),
        client: summarize(client),
        total: summarize(total),
    }
}

fn print_stage(
    label: &str,
    rows: &[(usize, PipelineStats)],
    stage: fn(&PipelineStats) -> StageStats,
) {
    let baseline = stage(&rows[0].1);
    println!("  {label}");
    println!("       count  median_us  p95_us  max_us  median_vs_1x  p95_vs_1x");
    for (count, pipeline) in rows {
        let stats = stage(pipeline);
        println!(
            "  {count:>10}  {:>9}  {:>6}  {:>6}  {:>12.2}  {:>9.2}",
            stats.median_us,
            stats.p95_us,
            stats.max_us,
            stats.median_us as f64 / baseline.median_us.max(1) as f64,
            stats.p95_us as f64 / baseline.p95_us.max(1) as f64,
        );
    }
}

#[test]
#[ignore = "release-only event sizing profile; run with just bench-render-scale"]
fn render_scale_profile_armed_boxes_with_populated_documents() {
    println!("cached sizing EVENT pass; fixed 300x200; populated terminal history; two accepted four-row view roots per box");
    println!("boxes median_us p95_us max_us");
    for count in [1, 15] {
        let mut pipeline = RenderPipeline::new(active_panes(count));
        let terminals = pipeline.app.state.workspaces[0].tabs[0].layout.pane_ids();
        let mut views = Vec::new();
        let mut items = Vec::new();
        for (index, terminal) in terminals.into_iter().enumerate() {
            let public = pipeline.app.public_pane_id(0, terminal).unwrap();
            let (_, added) = pipeline.app.state.workspaces[0]
                .split_pane_with_view_box(
                    terminal,
                    &[
                        ("bench.views".into(), "one".into()),
                        ("bench.views".into(), "two".into()),
                    ],
                    Direction::Vertical,
                    0.5,
                )
                .unwrap();
            views.extend(added);
            for view in ["one", "two"] {
                let root = format!("{index}-{view}");
                items.push(serde_json::json!({"id":root,"role":"section","label":"Populated view","view":{"pane_id":public,"view_id":view}}));
                for row in 0..4 {
                    items.push(serde_json::json!({"id":format!("{root}-{row}"),"parent_id":root,"role":"section","label":"A populated neutral document row"}));
                }
            }
        }
        let document = serde_json::from_value(serde_json::json!({"contract":"host.view.v1","namespace":"bench.views","generation":1,"revision":1,"surface":"navigation","target":{"kind":"session"},"items":items})).unwrap();
        pipeline
            .app
            .host_document_caches
            .entry(("bench.views".into(), "session".into()))
            .or_default()
            .publish_for_namespace("bench.views", 1, 0, &Default::default(), document)
            .unwrap();
        for view in views {
            pipeline.app.activate_auto_view(0, 0, view);
        }
        assert!(pipeline
            .app
            .state
            .view_sizing
            .active
            .values()
            .all(|view| view.measure.rows == 4));
        let target = crate::ui::TabSurfaceTarget {
            workspace_index: 0,
            tab_index: 0,
        };
        let mut samples = Vec::new();
        for sample in 0..WARMUP_COUNT + SAMPLE_COUNT {
            let started = Instant::now();
            black_box(pipeline.app.resize_auto_views(
                target,
                Rect::new(0, 0, 300, 200),
                HostCellSize::default(),
            ));
            let elapsed = started.elapsed();
            if sample >= WARMUP_COUNT {
                samples.push(elapsed);
            }
        }
        let stats = summarize(samples);
        println!(
            "{count} {} {} {}",
            stats.median_us, stats.p95_us, stats.max_us
        );
    }
}

fn print_profiles(label: &str, build: fn(usize) -> Vec<Workspace>) {
    let rows = CARDINALITIES.map(|count| (count, profile(build, count)));
    println!("{label}");
    print_stage("server pane surface", &rows, |stats| stats.server);
    print_stage("client shell composition", &rows, |stats| stats.client);
    print_stage("combined pipeline", &rows, |stats| stats.total);
}

fn profile_snapshot_encoding(
    build: fn(usize) -> Vec<Workspace>,
    count: usize,
    client_count: usize,
) -> StageStats {
    let pipeline = RenderPipeline::new(build(count));
    let run = || {
        let started = Instant::now();
        let template = super::client_shell::snapshot(&pipeline.app, "bench-boot", 1, None, None);
        for client_index in 0..client_count {
            let mut snapshot = template.clone();
            snapshot.revision = client_index as u64 + 1;
            let message = crate::protocol::endpoint::snapshot_message(&snapshot)
                .expect("benchmark snapshot should serialize");
            black_box(
                bincode::serde::encode_to_vec(message, bincode::config::standard())
                    .expect("benchmark snapshot message should frame"),
            );
        }
        started.elapsed()
    };
    for _ in 0..WARMUP_COUNT {
        black_box(run());
    }
    summarize((0..SAMPLE_COUNT).map(|_| run()).collect())
}

fn print_snapshot_encoding_profiles(label: &str, build: fn(usize) -> Vec<Workspace>) {
    println!("{label} snapshot projection + JSON framing");
    println!("       panes  clients  median_us  p95_us  max_us");
    for count in CARDINALITIES {
        for client_count in CLIENT_CARDINALITIES {
            let stats = profile_snapshot_encoding(build, count, client_count);
            println!(
                "  {count:>10}  {client_count:>7}  {:>9}  {:>6}  {:>6}",
                stats.median_us, stats.p95_us, stats.max_us
            );
        }
    }
}

fn print_token_rule_profiles() {
    let rules = std::iter::repeat_n(
        "{ contains = 'NO-MATCH', ignore_case = true, bold = true }",
        15,
    )
    .chain(std::iter::once("{ starts_with = 'bench', bold = true }"))
    .collect::<Vec<_>>()
    .join(",");
    for (label, build) in [
        ("background", workspaces as fn(usize) -> Vec<Workspace>),
        ("active", active_panes),
    ] {
        for conditional in [false, true] {
            let config: Config = toml::from_str(&format!(
                "[ui.sidebar.agents]\nrows = [[{{ token = 'workspace', rules = [{}] }}]]\n[ui.sidebar.spaces]\nrows = [[{{ token = 'workspace', rules = [{}] }}]]",
                if conditional { &rules } else { "" }, if conditional { &rules } else { "" },
            )).unwrap();
            let rows = [1, 15].map(|count| {
                let mut pipeline = RenderPipeline::with_config(build(count), &config);
                pipeline.app.state.ensure_test_terminals();
                for terminal in pipeline.app.state.terminals.values_mut() {
                    terminal.detected_agent = Some(crate::detect::Agent::Pi);
                }
                pipeline
                    .client
                    .set_snapshot(Box::new(super::client_shell::snapshot(
                        &pipeline.app,
                        "bench-boot",
                        1,
                        None,
                        None,
                    )));
                (count, profile_pipeline(pipeline))
            });
            println!(
                "token rules {label}: populated agents, rules_per_token={}",
                if conditional { 16 } else { 0 }
            );
            print_stage("client shell composition", &rows, |stats| stats.client);
        }
    }
}

fn print_recovery_browser_profile() {
    let rows = [1usize, 15].map(|count| {
        let mut pipeline = RenderPipeline::new(active_panes(count));
        pipeline.client.open_recovery_browser();
        (count, profile_pipeline(pipeline))
    });
    println!("neutral recovery browser (cached rows; normalization outside samples)");
    print_stage("client shell composition", &rows, |stats| stats.client);
}

/// The surface projection's view branch at 1 and 15 drawn view panes.
///
/// This measures view descriptor/scope projection only (not populated view
/// content composition, which would need a document fixture); the flat
/// terminal-only fixture cannot measure this branch at all.
fn print_view_panes_profile() {
    let rows = [1usize, 15].map(|count| (count, profile(view_panes, count)));
    println!("active view panes (view descriptor/scope projection only, no document fixture)");
    print_stage("server pane surface", &rows, |stats| stats.server);
    // The combined row includes client compose, which without published
    // documents does no per-view row work, so it too excludes populated view
    // content.
    print_stage(
        "combined pipeline (client compose draws no view rows without documents)",
        &rows,
        |stats| stats.total,
    );
}

fn print_terminal_views_profile() {
    println!("independent terminal view projection");
    for count in [1usize, 15] {
        let runtimes = (0..count)
            .map(|index| {
                TerminalRuntime::test_with_screen_bytes(
                    40,
                    8,
                    format!("nested terminal {index}").as_bytes(),
                )
            })
            .collect::<Vec<_>>();
        let mut samples = Vec::with_capacity(SAMPLE_COUNT);
        for sample in 0..WARMUP_COUNT + SAMPLE_COUNT {
            let started = Instant::now();
            for runtime in &runtimes {
                let rendered =
                    super::render_stream::render_terminal_virtual(runtime, Rect::new(0, 0, 40, 8));
                black_box(rendered);
            }
            if sample >= WARMUP_COUNT {
                samples.push(started.elapsed());
            }
        }
        let stats = summarize(samples);
        println!(
            "  views={count:>2} median_us={:>6} p95_us={:>6} max_us={:>6}",
            stats.median_us, stats.p95_us, stats.max_us
        );
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "manual client-rendered pipeline scaling profile"]
async fn render_scale_profile() {
    print_profiles("background workspaces (one pane each)", workspaces);
    print_snapshot_encoding_profiles("background workspaces", workspaces);
    print_profiles("active panes (one workspace)", active_panes);
    print_profiles(
        "zoomed populated panes (fixed visible body, canonical ancestry guard)",
        zoomed_panes,
    );
    print_snapshot_encoding_profiles("active panes", active_panes);
    print_token_rule_profiles();
    print_recovery_browser_profile();
    print_view_panes_profile();
    let named = [1usize, 15].map(|count| (count, profile(named_view_groups, count)));
    println!("populated named terminal/view groups (no document fixture)");
    print_stage("server pane surface", &named, |stats| stats.server);
    print_stage("combined pipeline", &named, |stats| stats.total);
    print_terminal_views_profile();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture must show real panes, not N hits on one tiny rect: every pane
    /// a tab shows is at least the fixture minimum, and every pane in every tab
    /// is populated.
    #[tokio::test(flavor = "current_thread")]
    async fn every_populated_pane_is_real() {
        let surface = pane_surface_rect();
        for count in [1usize, 15, 50] {
            let workspace = populated_panes(count);
            let mut total = 0;
            for (tab_idx, tab) in workspace.tabs.iter().enumerate() {
                let visible = tab.layout.panes(surface);
                total += visible.len();
                for info in visible {
                    assert!(
                        info.rect.width >= MIN_PANE_COLS && info.rect.height >= MIN_PANE_ROWS,
                        "{count} panes, tab {tab_idx}: {:?} is below the fixture minimum",
                        info.rect
                    );
                }
            }
            assert_eq!(total, count, "every pane is populated");
        }
        let surface = pane_surface_rect();
        let missing = populated_panes(15)
            .tabs
            .iter()
            .flat_map(|tab| tab.layout.panes(surface))
            .filter(|info| !info.rect.is_empty())
            .count();
        assert_eq!(missing, 15);
    }

    /// The view fixture proves its own claim: the requested cardinality of view
    /// panes is placed, every one is drawn, and every one has positive geometry.
    /// Without this, a chain of halved splits could report "15 views" while only
    /// the first few have any area.
    #[tokio::test(flavor = "current_thread")]
    async fn view_populated_fixture_places_the_requested_nonzero_views() {
        let surface = pane_surface_rect();
        for count in [1usize, 15] {
            let workspace = view_populated(count);
            let tab = &workspace.tabs[0];
            assert_eq!(
                tab.layout.view_panes().len(),
                count,
                "the fixture places {count} view panes"
            );
            let drawn_views = tab
                .layout
                .panes(surface)
                .into_iter()
                .filter(|info| tab.layout.view_at(info.id).is_some())
                .collect::<Vec<_>>();
            assert_eq!(
                drawn_views.len(),
                count,
                "the frame draws {count} view panes"
            );
            for info in drawn_views {
                assert!(
                    !info.rect.is_empty(),
                    "{count} views: view {:?} has zero-area geometry {:?}",
                    info.id,
                    info.rect
                );
            }
        }
    }
}
