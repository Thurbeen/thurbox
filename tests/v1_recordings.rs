//! Golden recordings of what thurbox v1 draws.
//!
//! These pin the contract v2 has to reproduce, and they can only be taken while
//! `src/ui/` still exists — which is why they come before any deletion rather
//! than after it.
//!
//! Two rules from the prior v2 attempt's post-mortem shape this file:
//!
//! 1. **A differential oracle cannot license a deletion.** Comparing a plugin
//!    against a builder *inside the module the deletion removes* means the test
//!    can fail before the handover and not after, and the repair that compiles
//!    is dropping the assertion. So these record to *files*: v1 is captured
//!    once, as data, and v2 is later asserted against the data.
//! 2. **The formatter destructures every field by name.** A field that stops
//!    being rendered becomes a compile error here, not a silently shorter
//!    snapshot.
//!
//! Recordings live in `tests/snapshots/`. Update them with `INSTA_UPDATE=always`
//! only when v1's behaviour genuinely changed.

use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::widgets::ListState;
use ratatui::Terminal;

use thurbox::session::{SessionId, SessionInfo, SessionStatus, WorktreeInfo};
use thurbox::ui::layout::{compute_layout, PanelAreas};
use thurbox::ui::project_list::{render_left_panel, LeftPanelState};
use thurbox::ui::status_bar::{render_footer, FooterState};
use thurbox::ui::terminal_view::render_terminal;
use thurbox::ui::FocusLevel;

/// Render a closure into a fixed-size buffer and return it as text.
fn record(width: u16, height: u16, draw: impl FnOnce(&mut ratatui::Frame, Rect)) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| {
            let area = frame.area();
            draw(frame, area);
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A session with every field a renderer might read set to something visible.
fn session(name: &str, status: SessionStatus, repo: &str, branch: &str) -> SessionInfo {
    let mut info = SessionInfo::new(name.to_string());
    info.id = SessionId::default();
    info.status = status;
    info.agent = "claude".to_string();
    info.cwd = Some(std::path::PathBuf::from(format!("/src/{repo}")));
    info.repo_display_names = vec![repo.to_string()];
    info.worktrees = vec![WorktreeInfo {
        repo_path: std::path::PathBuf::from(format!("/src/{repo}")),
        worktree_path: std::path::PathBuf::from(format!("/wt/{name}")),
        branch: branch.to_string(),
    }];
    info
}

/// **The formatter.** Every field is destructured by name, so one that stops
/// being rendered is a compile error rather than a shorter recording.
///
/// This is the rule the prior attempt arrived at after a differential oracle
/// let a field disappear silently.
fn describe(info: &SessionInfo) -> String {
    let SessionInfo {
        id,
        name,
        status,
        agent,
        worktrees,
        agent_session_id,
        cwd,
        additional_dirs,
        backend_id,
        shell_backend_id,
        remote_host,
        agent_metrics,
        agent_activity,
        notification,
        git_stats,
        repo_display_names,
        parent_session_id,
        display_order,
        hook_wiring,
    } = info;

    // `id` is random per run, so its *presence* is recorded, not its value.
    format!(
        "name={name} status={status} agent={agent} has_id={} worktrees={} \
         agent_session_id={agent_session_id:?} cwd={cwd:?} additional_dirs={additional_dirs:?} \
         backend_id={backend_id:?} shell_backend_id={shell_backend_id:?} \
         remote_host={remote_host:?} has_metrics={} activity={agent_activity:?} \
         notification={notification:?} has_git_stats={} repos={repo_display_names:?} \
         parent={parent_session_id:?} order={display_order:?} hooks={hook_wiring:?}",
        !id.to_string().is_empty(),
        worktrees.len(),
        agent_metrics.is_some(),
        git_stats.is_some(),
    )
}

/// Which regions a layout produced, by name — the contract v2's `layout.lua`
/// has to reproduce.
fn describe_layout(areas: &PanelAreas) -> String {
    let PanelAreas {
        header,
        left_panel,
        automations_panel,
        info_panel,
        tasks_panel,
        file_viewer,
        global_search,
        ..
    } = areas;
    let present = |label: &str, rect: &Option<Rect>| match rect {
        Some(r) => format!("{label}=w{}", r.width),
        None => format!("{label}=-"),
    };
    format!(
        "header=w{} {} {} {} {} {} {}",
        header.width,
        present("sessions", left_panel),
        present("automations", automations_panel),
        present("info", info_panel),
        present("tasks", tasks_panel),
        present("files", file_viewer),
        present("search", global_search),
    )
}

#[test]
fn record_the_session_list_across_its_states() {
    let sessions = [
        session("fix-osc52", SessionStatus::Working, "thurbox", "fix/osc52"),
        session("add-wsl", SessionStatus::Blocked, "thurbox", "feat/wsl"),
        session("perf-cache", SessionStatus::Done, "thurbox", "perf/cache"),
        session("update-deps", SessionStatus::Idle, "website", "chore/deps"),
    ];
    let refs: Vec<&SessionInfo> = sessions.iter().collect();
    let headers = vec![
        Some("thurbox".to_string()),
        None,
        None,
        Some("website".to_string()),
    ];
    let depths = vec![0u8, 1, 0, 0];
    let matches = vec![None, None, None, None];
    let mut list_state = ListState::default();

    let drawn = record(44, 16, |frame, area| {
        let mut state = LeftPanelState {
            sessions: &refs,
            active_session: 0,
            show_selection: true,
            session_focus: FocusLevel::Focused,
            session_list_state: &mut list_state,
            session_match_positions: &matches,
            session_search_active: false,
            headers: &headers,
            depths: &depths,
            spinner: "⠋",
            pending_spawn: None,
        };
        render_left_panel(frame, area, &mut state);
    });

    insta::assert_snapshot!("v1_session_list", drawn);
}

#[test]
fn record_every_session_field_that_is_rendered() {
    // The field-by-field record. If v1 grows a field, this stops compiling
    // until it is added — which is the point.
    let described: Vec<String> = [
        session("one", SessionStatus::Working, "thurbox", "a"),
        session("two", SessionStatus::Unreachable, "remote", "b"),
    ]
    .iter()
    .map(describe)
    .collect();
    insta::assert_snapshot!("v1_session_fields", described.join("\n"));
}

#[test]
fn record_the_responsive_layout_at_each_threshold() {
    // v1's breakpoints: terminal only below 80, two panels below 120, three
    // above. `ui/layout.lua` has to reproduce exactly this.
    let described: Vec<String> = [60u16, 100, 160]
        .iter()
        .map(|width| {
            let areas = compute_layout(
                Rect {
                    x: 0,
                    y: 0,
                    width: *width,
                    height: 30,
                },
                true,
                true,
                true,
                true,
                false,
                true,
                2,
                true,
            );
            format!("width={width} {}", describe_layout(&areas))
        })
        .collect();
    insta::assert_snapshot!("v1_layout_thresholds", described.join("\n"));
}

#[test]
fn record_the_footer_across_widths() {
    // v1's footer drops pills as the width shrinks, and the order it drops them
    // in is the contract — `ui/plugins/90_footer.lua` has to make the same
    // choices.
    let keybindings = thurbox::session::keybindings::KeyBindings::default();
    let described: Vec<String> = [60u16, 100, 160]
        .iter()
        .map(|width| {
            let drawn = record(*width, 1, |frame, area| {
                let state = FooterState {
                    session_count: 4,
                    status: None,
                    focus_label: "Sessions",
                    sync_in_progress: false,
                    pending_spawn: None,
                    tick_count: 0,
                    automation_count: 2,
                    file_viewer_open: false,
                    tasks_enabled: true,
                    file_viewer_enabled: true,
                    info_panel_enabled: true,
                    keybindings: &keybindings,
                };
                render_footer(frame, area, &state);
            });
            format!("width={width}\n{drawn}")
        })
        .collect();
    insta::assert_snapshot!("v1_footer_widths", described.join("\n"));
}

#[test]
fn record_the_agent_view_states() {
    // Live output, and the frame around it. The terminal itself is a vt100
    // grid, so what is recorded is the chrome: the title, the border, and where
    // the content sits.
    let info = session("fix-osc52", SessionStatus::Working, "thurbox", "fix/osc52");
    let described: Vec<String> = ["", "hello from the agent\n"]
        .iter()
        .map(|output| {
            let mut parser = vt100::Parser::new(6, 40, 100);
            parser.process(output.as_bytes());
            let drawn = record(40, 6, |frame, area| {
                render_terminal(
                    frame,
                    area,
                    &mut parser,
                    &info,
                    thurbox::ui::FocusLevel::Focused,
                    false,
                    0,
                );
            });
            format!("output={output:?}\n{drawn}")
        })
        .collect();
    insta::assert_snapshot!("v1_agent_view", described.join("\n"));
}
