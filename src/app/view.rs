//! View / rendering logic for the Thurbox TUI.
//!
//! Contains the main `App::view` method and helper functions for
//! rendering the help overlay and formatting timestamps.

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::session::{KeyBindings, KeyChord, SessionInfo};
use crate::ui::selection;
use crate::ui::theme::Theme;
use crate::ui::{
    agent_picker_modal, automation_editor_modal, automations_list_modal, automations_panel,
    branch_selector_modal, file_viewer, global_search, info_panel, project_list,
    restore_sessions_modal, session_name_modal, status_bar, task_editor_modal, tasks_panel,
    terminal_view, theme_picker_modal, worktree_name_modal,
};

use super::{
    App, CentralTab, ClickAction, ClickTarget, InputFocus, ScrollTarget, ScrollbarHit, TerminalView,
};
use crate::ui::scrollbar::ScrollbarGeom;

/// One laid-out central-pane tab (Agent/Shell/Review) on the pane's top border:
/// its on-border rect (click target + paint position), the display label (with
/// any shortcut baked in, e.g. `"Review · F7"`), and whether it's the active
/// view. Rendered as a filled pill via `ui::render_pill`, exactly like the
/// footer buttons, so it reads as clickable.
struct CentralTabCell {
    tab: CentralTab,
    rect: Rect,
    label: String,
    active: bool,
}

impl App {
    pub fn view(&mut self, frame: &mut Frame) {
        self.metrics.bump(|p| &mut p.frames_rendered);
        // Rebuilt fresh each frame: every scrollbar and clickable row drawn
        // below records its geometry + target here for the mouse handlers to
        // hit-test.
        self.scrollbar_hits.clear();
        self.click_targets.clear();

        // Keep the central-pane focus aligned with the active session's review
        // (it may have changed under us via a session switch) before laying out.
        self.sync_review_focus();

        let areas = self.layout_for(frame.area());

        self.render_header(frame, areas.header);
        self.render_left_panel(frame, areas.left_panel);
        self.render_automations_pane(frame, areas.automations_panel);
        self.render_info_panel(frame, areas.info_panel);
        self.render_tasks_panel(frame, areas.tasks_panel);
        self.render_file_viewer(frame, areas.file_viewer);
        self.render_central_pane(frame, areas.terminal);
        if let Some(search_area) = areas.global_search {
            let gs = &self.global_search;
            global_search::render_global_search(
                frame,
                search_area,
                &global_search::GlobalSearchView {
                    query: gs.query.value(),
                    cursor: gs.query.cursor_pos(),
                    results: &gs.results,
                    selected: gs.selected,
                },
            );
        }
        self.render_footer(frame, areas.footer);
        self.render_modals(frame);
        self.repaint_theme_background(frame);
        self.apply_hover_highlight(frame);
        self.apply_selection_highlight(frame);
    }

    /// Highlight the clickable element under the mouse pointer so what a click
    /// would hit is visible before clicking. List/selector rows get a subtle
    /// background band (the theme's `selection_bg`); buttons brighten their
    /// fill to `accent_bright`. Runs on the recorded click targets, after all
    /// rendering; the text selection highlight is applied later and wins on
    /// overlap.
    fn apply_hover_highlight(&self, frame: &mut Frame) {
        let Some((hx, hy)) = self.mouse_hover else {
            return;
        };
        let modal_open = !matches!(self.modal, super::modals::Modal::None);
        // While the global-search strip is open clicks are swallowed (it owns
        // all input), so don't underline rows as if they were clickable.
        if self.global_search.active && !modal_open {
            return;
        }
        let pos = ratatui::layout::Position::new(hx, hy);
        let hovered = self.click_targets.iter().find(|t| {
            // While a modal is open, only its rows/buttons react — the pane and
            // footer targets recorded beneath the overlay are unreachable too.
            let reachable = if modal_open {
                matches!(
                    t.action,
                    ClickAction::ModalRow(_)
                        | ClickAction::ModalButton { .. }
                        | ClickAction::ModalField(_)
                        | ClickAction::RepoFocus(_)
                )
            } else {
                matches!(
                    t.action,
                    ClickAction::SelectSession(_)
                        | ClickAction::SelectTask(_)
                        | ClickAction::SelectAutomation(_)
                        | ClickAction::SelectFileRow(_)
                        | ClickAction::Global(_)
                        | ClickAction::ReviewButton(_)
                        | ClickAction::CentralTab(_)
                        | ClickAction::PaneField { .. }
                )
            };
            reachable && t.rect.contains(pos)
        });
        let Some(target) = hovered else {
            return;
        };
        // Buttons (footer + modal + code-review) get a stronger, button-like
        // hover — brighten their fill to the accent so the chip lights up — while
        // list rows get a subtle background band to mark what a click would hit.
        // For a button we also force the fg to `inverted_fg` so the brightened
        // chip stays legible regardless of the resting style (primary's accent
        // fill and the neutral selection-filled secondary chip carry different fg
        // colours); for rows we tint only the background, leaving each cell's
        // fg/modifiers intact.
        let is_button = matches!(
            target.action,
            ClickAction::Global(_)
                | ClickAction::ModalButton { .. }
                | ClickAction::ReviewButton(_)
                | ClickAction::CentralTab(_)
        );
        let hover_bg = if is_button {
            Theme::accent_bright()
        } else {
            Theme::selection_bg()
        };
        let buf = frame.buffer_mut();
        let rect = target.rect;
        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                if let Some(cell) = buf.cell_mut(ratatui::layout::Position::new(x, y)) {
                    cell.set_bg(hover_bg);
                    if is_button {
                        cell.set_fg(Theme::inverted_fg());
                    }
                }
            }
        }
    }

    /// Render the top status-bar header with the active-session/theme badge.
    fn render_header(&self, frame: &mut Frame, header: Rect) {
        let active_name = self
            .sessions
            .get(self.active_index)
            .map(|s| s.info.name.as_str());
        let theme_label = self.active_theme.display_name.as_str();
        // `update_status` is `Some` only when a newer release exists, so its
        // presence alone drives the badge.
        let update_latest = self.update_status.as_ref().map(|s| s.latest.as_str());
        status_bar::render_header(
            frame,
            header,
            Some(status_bar::HeaderBadge {
                active_session: active_name,
                theme_label,
                update_latest,
            }),
        );
    }

    /// Render the flat session list in the left panel (when present).
    fn render_left_panel(&mut self, frame: &mut Frame, left_area: Option<Rect>) {
        let Some(left_area) = left_area else {
            return;
        };

        // Rebuild the cached ordering only when its inputs changed (content
        // signature). The order is status-independent, so most frames — including
        // every frame while an agent streams output — reuse it and skip the
        // grouping/sort/nest work. The signature borrows all of `self`, so
        // compute it (and refresh the cache) before taking the field borrows
        // below.
        let sig = self.session_order_signature();
        let stale = self
            .cached_session_order
            .as_ref()
            .map_or(true, |(cached, _)| *cached != sig);
        if stale {
            self.metrics.bump(|p| &mut p.ordered_sessions_rebuilds);
            let infos: Vec<&SessionInfo> = self.sessions.iter().map(|s| &s.info).collect();
            let order = project_list::compute_session_order(&infos);
            self.cached_session_order = Some((sig, order));
        }

        let all_sessions: Vec<&SessionInfo> = self.sessions.iter().map(|s| &s.info).collect();

        // While the global-search strip is open, highlight the session list from
        // the global query (live). Otherwise there are no match positions (the
        // session list has no local search of its own anymore). Own the query so
        // it doesn't conflict with the `&mut session_list_state` borrow below.
        let global_query: Option<String> = self.global_search_query().map(|q| q.to_string());
        let global_match_positions: Vec<Option<project_list::SessionMatch>> = match &global_query {
            Some(q) => self
                .sessions
                .iter()
                .map(|s| session_fuzzy(q, &s.info))
                .collect(),
            None => Vec::new(),
        };

        // Remap the cached order onto the current refs / match positions /
        // active_index (these vary independently of the order, so the remap
        // always runs — but it's a cheap O(n) index map, no grouping work).
        let order = &self
            .cached_session_order
            .as_ref()
            .expect("cache populated above")
            .1;
        let ordered = project_list::OrderedSessions::from_order(
            &all_sessions,
            order,
            &global_match_positions,
            self.active_index,
        );

        use crate::ui::FocusLevel;
        let in_automation_context = matches!(
            self.focus,
            InputFocus::Automations
                | InputFocus::AutomationEditor
                | InputFocus::AutomationRunHistory
        );
        let list_focus = match self.focus {
            InputFocus::SessionList => FocusLevel::Focused,
            // In the automations context the central pane shows the
            // automation, not a session — so the session list reads as fully
            // unfocused (no accent border, no selected-row highlight; see
            // `show_selection`).
            _ if in_automation_context => FocusLevel::Inactive,
            InputFocus::Terminal | InputFocus::FileViewer => FocusLevel::Active,
            _ => FocusLevel::Active,
        };
        // Suppress the active-session row highlight while the automations
        // context is active — the active session is irrelevant there.
        let show_selection = !in_automation_context;

        // A (global) search is active iff there's a query — non-matching rows dim.
        let session_search_active = global_query.is_some();

        let spinner =
            crate::ui::SPINNER_FRAMES[self.spinner_frame() % crate::ui::SPINNER_FRAMES.len()];
        let rows = project_list::render_left_panel(
            frame,
            left_area,
            &mut project_list::LeftPanelState {
                sessions: &ordered.sessions,
                active_session: ordered.active_index,
                show_selection,
                session_focus: list_focus,
                session_list_state: &mut self.session_list_state,
                session_match_positions: &ordered.match_positions,
                session_search_active,
                headers: ordered.headers,
                depths: ordered.depths,
                spinner,
            },
        );
        self.record_row_clicks(
            rows,
            ClickAction::SelectSession,
            left_area,
            InputFocus::SessionList,
        );
    }

    /// Render the automations pane beneath the session list (when present).
    fn render_automations_pane(&mut self, frame: &mut Frame, auto_area: Option<Rect>) {
        let Some(auto_area) = auto_area else {
            return;
        };
        let now = crate::sync::current_time_millis();
        self.metrics.bump(|p| &mut p.automation_entries_built);
        let search = self.global_search_query();
        let entries: Vec<automations_panel::AutomationPaneEntry> = self
            .automation_ui
            .cached_automations
            .iter()
            .map(|a| {
                let m = search.and_then(|q| crate::fuzzy::fuzzy_match(q, &a.name));
                automations_panel::AutomationPaneEntry {
                    name: a.name.clone(),
                    summary: super::automation::format_automation_summary(a, now),
                    enabled: a.enabled,
                    match_positions: m.as_ref().map(|m| m.positions.clone()).unwrap_or_default(),
                    // When searching, rows that didn't match are dimmed.
                    dimmed: search.is_some() && m.is_none(),
                }
            })
            .collect();
        let focus = match self.focus {
            InputFocus::Automations => crate::ui::FocusLevel::Focused,
            // While editing / browsing history in the central pane, keep the
            // pane "active" so the row being worked on stays marked.
            InputFocus::AutomationEditor | InputFocus::AutomationRunHistory => {
                crate::ui::FocusLevel::Active
            }
            _ => crate::ui::FocusLevel::Inactive,
        };
        let selected = self
            .automation_ui
            .automation_panel_index
            .min(entries.len().saturating_sub(1));
        let preview_selected =
            self.global_search_preview_kind() == Some(crate::app::search::SearchKind::Automation);
        let rows = automations_panel::render_automations_pane(
            frame,
            auto_area,
            &automations_panel::AutomationsPaneState {
                entries: &entries,
                selected,
                focus,
                preview_selected,
            },
        );
        self.record_row_clicks(
            rows,
            ClickAction::SelectAutomation,
            auto_area,
            InputFocus::Automations,
        );
    }

    /// Render the info panel for the active session (when present).
    fn render_info_panel(&self, frame: &mut Frame, info_area: Option<Rect>) {
        let Some(info_area) = info_area else {
            return;
        };
        let Some(info) = self.sessions.get(self.active_index).map(|s| &s.info) else {
            return;
        };
        let now = crate::sync::current_time_millis();
        let agent_usage = self.usage.get(&info.agent);
        let automation_entries: Vec<info_panel::AutomationEntry> = self
            .automation_ui
            .cached_automations
            .iter()
            .filter(|a| a.enabled && a.next_run_at.is_some())
            .map(|a| {
                let remaining = a.next_run_at.unwrap_or(now).saturating_sub(now);
                info_panel::AutomationEntry {
                    label: truncate_str(&a.name, 30),
                    countdown: format_countdown(remaining),
                }
            })
            .collect();
        // Resolve the parent session's name for child sessions; fall back to
        // the short uuid when the parent is no longer in the list.
        let parent_name = info.parent_session_id.map(|pid| {
            self.sessions
                .iter()
                .find(|s| s.info.id == pid)
                .map(|s| s.info.name.clone())
                .unwrap_or_else(|| {
                    let id = pid.to_string();
                    id.chars().take(8).collect()
                })
        });
        info_panel::render_info_panel(
            frame,
            info_area,
            info,
            Some(&self.metrics.system_metrics),
            &automation_entries,
            agent_usage,
            parent_name.as_deref(),
        );
    }

    /// Render the tasks panel column (when present).
    fn render_tasks_panel(&mut self, frame: &mut Frame, area: Option<Rect>) {
        let Some(area) = area else {
            return;
        };
        let search = self.global_search_query();
        let entries: Vec<tasks_panel::TaskPaneEntry> = self
            .task_ui
            .filtered_task_indices
            .iter()
            .filter_map(|&i| self.task_ui.cached_tasks.get(i))
            .map(|t| {
                let title = truncate_str(&t.title, 40);
                // Match against the displayed (truncated) title so highlight
                // byte offsets stay valid.
                let m = search.and_then(|q| crate::fuzzy::fuzzy_match(q, &title));
                tasks_panel::TaskPaneEntry {
                    title,
                    status: t.status,
                    match_positions: m.as_ref().map(|m| m.positions.clone()).unwrap_or_default(),
                    dimmed: search.is_some() && m.is_none(),
                    linked: !self.task_related_session_indices(t).is_empty(),
                }
            })
            .collect();
        let focus = match self.focus {
            InputFocus::TaskList => crate::ui::FocusLevel::Focused,
            // While the central-pane editor is focused, keep the panel "active"
            // so the row being edited stays marked (like the automations pane).
            InputFocus::TaskEditor => crate::ui::FocusLevel::Active,
            _ => crate::ui::FocusLevel::Inactive,
        };
        let preview_selected =
            self.global_search_preview_kind() == Some(crate::app::search::SearchKind::Task);
        let rows = tasks_panel::render_tasks_panel(
            frame,
            area,
            &tasks_panel::TaskPaneState {
                entries: &entries,
                selected: self.task_ui.task_panel_index,
                focus,
                preview_selected,
            },
        );
        self.record_row_clicks(rows, ClickAction::SelectTask, area, InputFocus::TaskList);
    }

    /// Record a scrollbar drawn this frame as a drag target, if one was drawn.
    fn record_scrollbar(&mut self, geom: Option<ScrollbarGeom>, target: ScrollTarget) {
        if let Some(geom) = geom {
            self.scrollbar_hits.push(ScrollbarHit { geom, target });
        }
    }

    /// Record one clickable region drawn this frame. Recording order is
    /// priority order (first hit wins), so push row targets before their
    /// pane's whole-rect `FocusPane` fallback.
    fn record_click(&mut self, rect: Rect, action: ClickAction) {
        self.click_targets.push(ClickTarget { rect, action });
    }

    /// Record a row hitbox per entry plus the pane's whole-rect fallback.
    fn record_row_clicks(
        &mut self,
        rows: Vec<crate::ui::RowHitbox>,
        to_action: fn(usize) -> ClickAction,
        pane: Rect,
        pane_focus: InputFocus,
    ) {
        for row in rows {
            self.record_click(row.rect, to_action(row.index));
        }
        self.record_click(pane, ClickAction::FocusPane(pane_focus));
    }

    /// Render the file viewer in the right column (when present).
    fn render_file_viewer(&mut self, frame: &mut Frame, fv_area: Option<Rect>) {
        let Some(fv_area) = fv_area else {
            return;
        };
        // While a review is open, this column shows the review's changed-files
        // list (the navigation aid) instead of the working-tree file viewer.
        if self.active_review().is_some() {
            let level = if self.focus == InputFocus::ReviewFiles {
                crate::ui::FocusLevel::Focused
            } else {
                crate::ui::FocusLevel::Active
            };
            let rows = self
                .active_review()
                .map(|cr| crate::ui::code_review::render_files_list(frame, fv_area, cr, level));
            if let Some(rows) = rows {
                for h in rows {
                    self.record_click(h.rect, ClickAction::ReviewFile(h.index));
                }
            }
            // A click anywhere in the column focuses the changed-files pane (rows
            // also jump the diff, recorded above and hit-tested first).
            self.record_click(fv_area, ClickAction::FocusPane(InputFocus::ReviewFiles));
            return;
        }
        if let Some(session) = self.sessions.get(self.active_index) {
            if self.file_viewer.needs_rebuild_for(&session.info) {
                self.file_viewer.rebuild_from_session(&session.info);
            }
        } else {
            self.file_viewer.clear();
        }
        let fv_focus = match self.focus {
            InputFocus::FileViewer => crate::ui::FocusLevel::Focused,
            _ => crate::ui::FocusLevel::Inactive,
        };
        let (geom, rows) =
            file_viewer::render_file_viewer(frame, fv_area, &self.file_viewer, fv_focus);
        self.record_scrollbar(geom, ScrollTarget::FileViewer);
        self.record_row_clicks(
            rows,
            ClickAction::SelectFileRow,
            fv_area,
            InputFocus::FileViewer,
        );
    }

    /// Render the central pane. In the automations context (the pane or its
    /// editor is focused) it shows a single automation editor — a live preview
    /// while the list is focused, editable once the editor itself is focused —
    /// with the scoped automation's run history beneath it. Everything else
    /// shows the session terminal.
    fn render_central_pane(&mut self, frame: &mut Frame, terminal: Rect) {
        if matches!(
            self.focus,
            InputFocus::Automations
                | InputFocus::AutomationEditor
                | InputFocus::AutomationRunHistory
        ) {
            let geom = self.render_automation_workspace(frame, terminal);
            self.record_scrollbar(geom, ScrollTarget::RunHistory);
            return;
        }
        // In the tasks context the central pane shows the task editor (a live
        // preview while the panel is focused, editable once the editor is) with
        // the task's details beneath it.
        if matches!(self.focus, InputFocus::TaskList | InputFocus::TaskEditor) {
            let (geom, field_hits) = self.render_task_workspace(frame, terminal);
            // Per-field click targets (only present while the editor is shown);
            // recorded before any whole-pane fallback so a field click wins.
            for hit in field_hits {
                self.record_click(
                    hit.rect,
                    ClickAction::PaneField {
                        focus: InputFocus::TaskEditor,
                        index: hit.index,
                    },
                );
            }
            self.record_scrollbar(geom, ScrollTarget::TaskPreview);
            return;
        }
        // While the global-search strip previews a task result, mirror that in
        // the central pane (focus stays in the strip, so the normal task-context
        // branch above doesn't fire).
        if self.global_search_preview_kind() == Some(crate::app::search::SearchKind::Task)
            && self.selected_task().is_some()
        {
            // Scope the immutable `task` borrow so it ends before the
            // `&mut self` record_scrollbar call.
            let geom = {
                let task = self.selected_task().expect("checked is_some");
                self.render_task_detail_pane(frame, terminal, task)
            };
            self.record_scrollbar(geom, ScrollTarget::TaskPreview);
            return;
        }

        // Agent terminal / shell / code-review all share the central pane and a
        // clickable tab strip in its top border. Record the tab click targets
        // *before* the pane renders its own whole-rect focus fallback, so a tab
        // click wins over a plain pane-focus click. The review overlay takes the
        // pane whenever the active session has one open (persisted per session
        // like the shell view, so it survives session switches).
        let tabs = self.central_tab_cells(terminal);
        for cell in &tabs {
            self.record_click(cell.rect, ClickAction::CentralTab(cell.tab));
        }
        if self.active_review().is_some() {
            self.render_code_review_pane(frame, terminal);
        } else {
            self.render_terminal_pane(frame, terminal);
        }
        // Drawn last so it overlays the pane's top border (the right-aligned
        // session-info title leaves the left free for the tabs).
        self.draw_central_tabs(frame, &tabs);
    }

    /// Render the open code-review view into the central pane (dimmed when not
    /// the focused pane, like the shell view) and record its click/scroll
    /// targets.
    fn render_code_review_pane(&mut self, frame: &mut Frame, terminal: Rect) {
        let level = if self.focus == InputFocus::CodeReview {
            crate::ui::FocusLevel::Focused
        } else {
            crate::ui::FocusLevel::Active
        };
        let Some(hits) = self
            .active_review_mut()
            .map(|cr| crate::ui::code_review::render(frame, terminal, cr, level))
        else {
            return;
        };
        // Row + button targets first (first match wins), then the whole-pane
        // focus fallback.
        for h in hits.rows {
            self.record_click(h.rect, ClickAction::ReviewRow(h.index));
        }
        for (h, action) in hits.buttons {
            self.record_click(h.rect, ClickAction::ReviewButton(action));
        }
        self.record_click(terminal, ClickAction::FocusPane(InputFocus::CodeReview));
        self.record_scrollbar(hits.scrollbar, ScrollTarget::CodeReview);
    }

    /// Render the active session's terminal (or shell view) into the central
    /// pane — the default when no overlay/review owns it.
    fn render_terminal_pane(&mut self, frame: &mut Frame, terminal: Rect) {
        let terminal_focus = if self.focus == InputFocus::Terminal {
            crate::ui::FocusLevel::Focused
        } else {
            crate::ui::FocusLevel::Active
        };
        let is_shell_view = self.active_terminal_view() == TerminalView::Shell;

        // A click anywhere in the central pane focuses the terminal (row
        // targets in other panes were recorded first and win on overlap —
        // there is none — and the scrollbar check runs before click targets).
        self.record_click(terminal, ClickAction::FocusPane(InputFocus::Terminal));

        // Scope the immutable `session` borrow so it ends before the
        // `&mut self` record_scrollbar call below.
        let mut locked_parser = false;
        let geom = {
            let Some(session) = self.sessions.get(self.active_index) else {
                terminal_view::render_empty_terminal(frame, terminal);
                return;
            };
            let parser_arc = if is_shell_view {
                session.shell_pane.as_ref().map(|sp| &sp.parser)
            } else {
                None
            }
            .unwrap_or(&session.parser);
            if let Ok(mut parser) = parser_arc.lock() {
                locked_parser = true;
                terminal_view::render_terminal(
                    frame,
                    terminal,
                    &mut parser,
                    &session.info,
                    terminal_focus,
                    is_shell_view,
                )
            } else {
                None
            }
        };
        if locked_parser {
            // One parser lock per terminal render (the O(1) scrollback read
            // rides along). Redraw throttling, not caching, bounds the rate.
            self.metrics.bump(|p| &mut p.parser_locks_render);
        }
        self.record_scrollbar(geom, ScrollTarget::Terminal);
    }

    /// Lay out the central-pane tab strip (Agent / Shell / Review) along the top
    /// border of `area` as filled pill buttons. Each cell carries its on-border
    /// rect (click target + paint position), its display label (shortcut baked
    /// in), and whether it's the active view. Packing mirrors `render_button_bar`
    /// (` label ` chip = label+2 wide, one-space gaps) so the recorded hitboxes
    /// match the pills `draw_central_tabs` paints. Shell/Review are gated by
    /// their feature flags; cells stop before the right edge so they never run
    /// into the right-aligned session-info title.
    fn central_tab_cells(&self, area: Rect) -> Vec<CentralTabCell> {
        // No tabs on the empty "No Session" screen, or when the pane is too
        // narrow to hold even one.
        if area.width < 6 || area.height == 0 || self.sessions.get(self.active_index).is_none() {
            return Vec::new();
        }
        let active = self.active_central_tab();
        // (tab, name, action-for-shortcut). Agent has no dedicated key — the
        // Shell toggle returns to it — so it shows no hint.
        let mut specs: Vec<(CentralTab, &str, Option<crate::session::Action>)> =
            vec![(CentralTab::Agent, "Agent", None)];
        if self.features.shell_pane {
            specs.push((
                CentralTab::Shell,
                "Shell",
                Some(crate::session::Action::ToggleShell),
            ));
        }
        if self.features.code_review {
            specs.push((
                CentralTab::Review,
                "Review",
                Some(crate::session::Action::ToggleReview),
            ));
        }

        // Pack pills left-to-right starting one cell in from the rounded corner,
        // separated by a one-space gap (which shows the border between chips),
        // matching `render_button_bar`.
        let mut x = area.x + 1;
        let limit = area.x + area.width.saturating_sub(1);
        let mut cells = Vec::with_capacity(specs.len());
        for (i, (tab, name, action)) in specs.into_iter().enumerate() {
            let gap = u16::from(i > 0);
            let shortcut = action
                .and_then(|a| crate::session::compact_shortcut(self.keybindings.chords_for(a)));
            let label = match shortcut {
                Some(sc) => format!("{name} · {sc}"),
                None => name.to_string(),
            };
            // Pill width = ` label ` (label + one pad cell each side), per
            // `ui::button_width` for a hint-less `ButtonSpec`.
            let width = label.chars().count() as u16 + 2;
            if x + gap + width > limit {
                break; // out of room — drop the rest rather than overrun the title
            }
            x += gap;
            cells.push(CentralTabCell {
                tab,
                rect: Rect::new(x, area.y, width, 1),
                label,
                active: tab == active,
            });
            x += width;
        }
        cells
    }

    /// Paint the central-pane tab strip onto the top border row as filled pill
    /// buttons (same look as the footer Help/Tasks/… pills, so they read as
    /// clickable). The active view is the accent-filled "primary" pill; the rest
    /// are neutral "secondary" pills. One-space gaps between them leave the
    /// pane's border showing, and the right-aligned session-info title is
    /// untouched.
    fn draw_central_tabs(&self, frame: &mut Frame, cells: &[CentralTabCell]) {
        for cell in cells {
            crate::ui::render_pill(frame, cell.rect, &cell.label, cell.active);
        }
    }

    /// Render the bottom status-bar footer.
    fn render_footer(&mut self, frame: &mut Frame, footer: Rect) {
        let is_shell_view = self.active_terminal_view() == TerminalView::Shell;
        let focus_label = match self.focus {
            InputFocus::SessionList => "Sessions",
            InputFocus::Automations => "Automations",
            InputFocus::AutomationEditor => "Edit Automation",
            InputFocus::AutomationRunHistory => "Run history",
            InputFocus::TaskList => "Tasks",
            InputFocus::TaskEditor => "Edit Task",
            InputFocus::Terminal if is_shell_view => "Shell",
            InputFocus::Terminal => "Terminal",
            InputFocus::FileViewer => "Files",
            InputFocus::GlobalSearch => "Search",
            InputFocus::CodeReview => "Review",
            InputFocus::ReviewFiles => "Changed files",
        };
        let button_hits = status_bar::render_footer(
            frame,
            footer,
            &status_bar::FooterState {
                session_count: self.sessions.len(),
                status: self.status_message.as_ref(),
                focus_label,
                sync_in_progress: self.worktree_sync.in_progress,
                tick_count: self.metrics.tick_count,
                // With automations disabled the badge would advertise a
                // feature the TUI won't fire — report 0 so it stays hidden.
                automation_count: if self.features.automations {
                    self.automation_ui
                        .cached_automations
                        .iter()
                        .filter(|a| a.enabled)
                        .count()
                } else {
                    0
                },
                file_viewer_open: self.show_file_viewer,
                tasks_enabled: self.features.tasks,
                file_viewer_enabled: self.features.file_viewer,
                shell_pane_enabled: self.features.shell_pane,
                code_review_enabled: self.features.code_review,
                keybindings: &self.keybindings,
            },
        );
        // The renderer pairs each surviving button with its Action, so the
        // click map can't drift from the (feature-filtered) render.
        for (hit, action) in button_hits {
            self.record_click(hit.rect, ClickAction::Global(action));
        }
    }

    /// Render any active modal overlay on top of everything else and record its
    /// click targets: selector rows as `ModalRow`, editor fields as `ModalField`,
    /// footer buttons as `ModalButton`. Anything not recorded is swallowed while
    /// the modal is open.
    fn render_modals(&mut self, frame: &mut Frame) {
        // Text-input modals report footer buttons (Confirm/Delete/Cancel) but
        // no row hitboxes (every other click is swallowed).
        let text_buttons = self.render_text_input_modals(frame);

        // Selector/editor modals report row hitboxes + footer buttons. Exactly
        // one modal is active at a time, so the first match wins.
        let ((modal_rows, modal_geom), sel_buttons) = self
            .render_selector_modal(frame)
            .unwrap_or(((Vec::new(), None), Vec::new()));

        // Editor modals (Settings / Automation) ship per-field hitboxes in the
        // rows slot — recorded as `ModalField` (select a field), not `ModalRow`
        // (activate a list row).
        let field_editor = matches!(
            self.modal,
            super::modals::Modal::Settings(_) | super::modals::Modal::AutomationEditor(_)
        );
        for row in modal_rows {
            let action = if field_editor {
                ClickAction::ModalField(row.index)
            } else {
                ClickAction::ModalRow(row.index)
            };
            self.record_click(row.rect, action);
        }
        // Footer buttons replay a key through the modal's own handler.
        for (hit, code, mods) in sel_buttons.into_iter().chain(text_buttons) {
            self.record_click(hit.rect, ClickAction::ModalButton { code, mods });
        }
        // The modal's own scrollbar: grabbable while the modal is open
        // (pane scrollbars beneath the overlay are not — see
        // `handle_modal_click`).
        self.record_scrollbar(modal_geom, ScrollTarget::Modal);
    }

    /// Render the text-input modals (worktree / session name) and the
    /// hard-delete confirmation. These report only footer buttons (every other
    /// click is swallowed), so they are rendered separately from selectors.
    fn render_text_input_modals(&self, frame: &mut Frame) -> crate::ui::ModalButtons {
        // Worktree name modal
        if let super::modals::Modal::WorktreeName(ref wn) = self.modal {
            let base = self.new_session.base_branch.as_deref().unwrap_or("");
            return worktree_name_modal::render_worktree_name_modal(
                frame,
                &worktree_name_modal::WorktreeNameState {
                    name: wn.name.value(),
                    cursor: wn.name.cursor_pos(),
                    base_branch: base,
                },
            );
        }

        // Session name modal
        if let super::modals::Modal::SessionName(ref sn) = self.modal {
            return session_name_modal::render_session_name_modal(
                frame,
                &session_name_modal::SessionNameState {
                    name: sn.name.value(),
                    cursor: sn.name.cursor_pos(),
                },
            );
        }

        // Hard-delete confirmation prompt (soft_delete feature off)
        if let super::modals::Modal::ConfirmDelete(ref cd) = self.modal {
            return crate::ui::confirm_delete_modal::render_confirm_delete_modal(
                frame,
                &crate::ui::confirm_delete_modal::ConfirmDeleteState {
                    session_name: &cd.session_name,
                    risk: &cd.risk,
                },
            );
        }

        // Best-effort restore confirmation (a force-deleted session)
        if let super::modals::Modal::ConfirmRestore(ref cr) = self.modal {
            return crate::ui::confirm_restore_modal::render_confirm_restore_modal(
                frame,
                &crate::ui::confirm_restore_modal::ConfirmRestoreState {
                    session_name: &cr.deleted.name,
                },
            );
        }

        Vec::new()
    }

    /// Render whichever selector-style modal is active, returning its row
    /// hitboxes + scrollbar geometry (`None` when no selector modal is open).
    fn render_selector_modal(&mut self, frame: &mut Frame) -> Option<crate::ui::ModalRender> {
        // Help overlay (rendered last, on top of everything)
        if let super::modals::Modal::Help(ref help) = self.modal {
            return Some(render_help_overlay(frame, &self.keybindings, help));
        }

        // Task trigger-time action picker
        if let super::modals::Modal::TaskActionPicker(ref p) = self.modal {
            return Some(
                crate::ui::task_action_picker_modal::render_task_action_picker_modal(frame, p),
            );
        }

        // Branch selector modal
        if let super::modals::Modal::BranchSelector(ref bs) = self.modal {
            return Some(branch_selector_modal::render_branch_selector_modal(
                frame,
                &branch_selector_modal::BranchSelectorState {
                    branches: &bs.branches,
                    selected_index: bs.index,
                },
            ));
        }

        // Agent picker modal
        if let super::modals::Modal::AgentPicker(ref ap) = self.modal {
            return Some(agent_picker_modal::render_agent_picker_modal(frame, ap));
        }

        // Host picker modal
        if let super::modals::Modal::HostPicker(ref hp) = self.modal {
            return Some(crate::ui::host_picker_modal::render_host_picker_modal(
                frame, hp,
            ));
        }

        // Theme picker modal
        if let super::modals::Modal::ThemePicker(ref tp) = self.modal {
            return Some(theme_picker_modal::render_theme_picker_modal(
                frame,
                &theme_picker_modal::ThemePickerState {
                    entries: &crate::ui::theme::all_theme_entries(),
                    selected_index: tp.index,
                },
            ));
        }

        // Settings panel (centered overlay). The field hitboxes ride in the
        // rows slot; `render_modals` records them as `ModalField` (not
        // `ModalRow`) because this is a field editor, not a list.
        if let super::modals::Modal::Settings(ref m) = self.modal {
            let (fields, buttons) = crate::ui::settings_modal::render_settings_modal(
                frame,
                &crate::ui::settings_modal::SettingsModalState {
                    modal: m,
                    restart_pending: m.restart_required_changed(),
                },
            );
            return Some(((fields, None), buttons));
        }

        // Restore sessions modal
        if let super::modals::Modal::RestoreSessions(ref rsm) = self.modal {
            return Some(self.render_restore_sessions_modal(frame, rsm));
        }

        // Automation editor modal (centered overlay — the Ctrl+P list path).
        if let super::modals::Modal::AutomationEditor(ref m) = self.modal {
            // Live preview of when this schedule will next fire (or the
            // validation error for the current input).
            let now = crate::sync::current_time_millis();
            let preview = editor_preview(m, now);
            let (fields, buttons) = automation_editor_modal::render_automation_editor_modal(
                frame,
                &automation_editor_modal::AutomationEditorState::from_modal(m, &preview, true),
            );
            return Some(((fields, None), buttons));
        }

        // Automations list modal
        if let super::modals::Modal::AutomationsList(ref al) = self.modal {
            return Some(self.render_automations_list_modal(frame, al));
        }

        // Repo picker modal. Render under an immutable borrow of the modal,
        // then (borrow released) record click targets that focus its editable
        // sub-fields (path input + search bar).
        if matches!(self.modal, super::modals::Modal::RepoPicker(_)) {
            let (render, areas) = {
                let super::modals::Modal::RepoPicker(ref rp) = self.modal else {
                    unreachable!()
                };
                self.render_repo_picker_modal(frame, rp)
            };
            if let Some(search) = areas.search {
                self.record_click(
                    search,
                    ClickAction::RepoFocus(super::modals::RepoPickerFocus::Search),
                );
            }
            self.record_click(
                areas.input,
                ClickAction::RepoFocus(super::modals::RepoPickerFocus::Input),
            );
            return Some(render);
        }

        None
    }

    fn render_restore_sessions_modal(
        &self,
        frame: &mut Frame,
        rsm: &super::modals::RestoreSessionsModal,
    ) -> crate::ui::ModalRender {
        let entries: Vec<restore_sessions_modal::DeletedSessionEntry> = rsm
            .list
            .iter()
            .map(|d| restore_sessions_modal::DeletedSessionEntry {
                name: d.name.clone(),
                agent: d.agent.clone(),
                deleted_ago: format_time_ago(d.deleted_at),
                has_worktrees: !d.worktrees.is_empty(),
                force_deleted: d.force_deleted,
            })
            .collect();
        restore_sessions_modal::render_restore_sessions_modal(
            frame,
            &restore_sessions_modal::RestoreSessionsModalState {
                entries: &entries,
                selected_index: rsm.index,
            },
        )
    }

    fn render_automations_list_modal(
        &self,
        frame: &mut Frame,
        al: &super::modals::AutomationsListModal,
    ) -> crate::ui::ModalRender {
        let entries: Vec<automations_list_modal::AutomationsListEntry> = al
            .entries
            .iter()
            .map(|e| automations_list_modal::AutomationsListEntry {
                name: e.name.clone(),
                summary: e.summary.clone(),
                enabled: e.enabled,
            })
            .collect();
        automations_list_modal::render_automations_list_modal(
            frame,
            &automations_list_modal::AutomationsListState {
                entries: &entries,
                selected_index: al.index,
            },
        )
    }

    fn render_repo_picker_modal(
        &self,
        frame: &mut Frame,
        rp: &super::modals::RepoPickerModal,
    ) -> (
        crate::ui::ModalRender,
        crate::ui::repo_picker_modal::RepoFocusAreas,
    ) {
        crate::ui::repo_picker_modal::render_repo_picker_modal(
            frame,
            &crate::ui::repo_picker_modal::RepoPickerState {
                bookmarks: &rp.bookmarks,
                selected: &rp.selected,
                worktree: &rp.worktree,
                is_header: &rp.is_header,
                is_child: &rp.is_child,
                collapsed: &rp.collapsed,
                list_index: rp.list_index,
                path_input: rp.path_input.value(),
                path_cursor: rp.path_input.cursor_pos(),
                path_suggestion: rp.path_suggestion.as_deref(),
                focus: rp.focus,
                search_query: rp.search_input.value(),
                search_cursor: rp.search_input.cursor_pos(),
                search_active: rp.focus == super::modals::RepoPickerFocus::Search
                    || !rp.search_input.value().is_empty(),
                filtered_indices: &rp.filtered_indices,
            },
        )
    }

    /// Repaint cells that fell back to terminal-default colours with the
    /// active theme's background and primary text. Themes whose `app_bg`
    /// is `Color::Reset` (e.g. the ANSI-based Default preset) skip this
    /// step so they continue to honour the user's terminal palette.
    fn repaint_theme_background(&self, frame: &mut Frame) {
        let app_bg = Theme::app_bg();
        if app_bg == ratatui::style::Color::Reset {
            return;
        }
        let text_primary = Theme::text_primary();
        let area = frame.area();
        let buf = frame.buffer_mut();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                let pos = ratatui::layout::Position::new(x, y);
                if let Some(cell) = buf.cell_mut(pos) {
                    repaint_reset_cell(cell, app_bg, text_primary);
                }
            }
        }
    }

    /// Apply the selection highlight and refresh the selected-text cache —
    /// runs after all rendering.
    fn apply_selection_highlight(&mut self, frame: &mut Frame) {
        let Some(ref sel) = self.text_selection else {
            self.selected_text_cache = None;
            return;
        };
        let sel_style = Style::default()
            .bg(Theme::selection_bg())
            .fg(Theme::selection_fg());
        let sel_clone = sel.clone();

        selection::highlight_buffer(frame.buffer_mut(), &sel_clone, sel_style);

        let text = selection::extract_text_from_buffer(frame.buffer_mut(), &sel_clone);
        self.selected_text_cache = if text.is_empty() { None } else { Some(text) };
    }

    /// Render the single central-pane automation view: the editor for the
    /// scoped automation (a preview while the list is focused, editable once the
    /// editor is focused), with the automation's run history beneath it. Shows a
    /// discoverability hint when there's nothing to edit.
    fn render_automation_workspace(
        &mut self,
        frame: &mut Frame,
        area: Rect,
    ) -> Option<ScrollbarGeom> {
        let editing = self.focus == InputFocus::AutomationEditor;

        let Some(m) = self.automation_ui.automation_editor.as_ref() else {
            render_empty_workspace_hint(
                frame,
                area,
                " Automation ",
                "No automations yet — press n to create one.",
                editing,
            );
            return None;
        };

        // Run history for the automation being edited (existing automations
        // only). Shown only when the cache matches the scoped automation.
        let show_history =
            m.editing_id.is_some() && self.automation_ui.cached_automation_runs_id == m.editing_id;
        let runs: Vec<crate::ui::automation_detail::AutomationRunRow> = if show_history {
            self.automation_ui
                .cached_automation_runs
                .iter()
                .map(|r| crate::ui::automation_detail::AutomationRunRow {
                    status: r.status,
                    at: format_clock(r.started_at),
                    when: format_time_ago(r.started_at),
                    detail: &r.detail,
                })
                .collect()
        } else {
            Vec::new()
        };

        // Split the pane: editor (sized to its fields) on top, run history
        // taking the remaining space beneath it (when shown).
        let (editor_area, history_area) = if show_history {
            let editor_h = (m.visible_fields().len() as u16 + 5).min(area.height.saturating_sub(4));
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(editor_h), Constraint::Min(3)])
                .split(area);
            (rows[0], Some(rows[1]))
        } else {
            (area, None)
        };

        let now = crate::sync::current_time_millis();
        let preview = editor_preview(m, now);
        let field_hits = automation_editor_modal::render_automation_editor_into(
            frame,
            editor_area,
            &automation_editor_modal::AutomationEditorState::from_modal(m, &preview, editing),
        );

        // Clicks focus the in-pane editor / run-history panels. Direct field
        // pushes (not `record_click`): `runs` above still borrows
        // `self.automation_ui`, so no `&mut self` method can be called here.
        // Per-field targets are pushed before the whole-pane fallback so a
        // click on a field wins (first match).
        for hit in field_hits {
            self.click_targets.push(ClickTarget {
                rect: hit.rect,
                action: ClickAction::PaneField {
                    focus: InputFocus::AutomationEditor,
                    index: hit.index,
                },
            });
        }
        self.click_targets.push(ClickTarget {
            rect: editor_area,
            action: ClickAction::FocusPane(InputFocus::AutomationEditor),
        });

        if let Some(history_area) = history_area {
            self.click_targets.push(ClickTarget {
                rect: history_area,
                action: ClickAction::FocusPane(InputFocus::AutomationRunHistory),
            });
            let history_focus = if self.focus == InputFocus::AutomationRunHistory {
                crate::ui::FocusLevel::Focused
            } else {
                crate::ui::FocusLevel::Inactive
            };
            crate::ui::automation_detail::render_run_history(
                frame,
                history_area,
                &runs,
                self.automation_ui.automation_run_index,
                history_focus,
            )
        } else {
            None
        }
    }

    /// Render the central pane for the tasks context as a **full-screen
    /// toggle**: while the editor is focused (`TaskEditor`) it shows the
    /// editor full-screen; while the tasks panel is focused (`TaskList`) it
    /// shows the selected task's read-only, scrollable markdown preview.
    fn render_task_workspace(
        &self,
        frame: &mut Frame,
        area: Rect,
    ) -> (Option<ScrollbarGeom>, Vec<crate::ui::RowHitbox>) {
        let editing = self.focus == InputFocus::TaskEditor;

        let Some(m) = self.task_ui.task_editor.as_ref() else {
            render_empty_workspace_hint(
                frame,
                area,
                " Task ",
                "No tasks yet — press n to create one.",
                editing,
            );
            return (None, Vec::new());
        };

        if editing {
            // Full-screen editor — its per-field hitboxes drive click-to-edit.
            let field_hits = task_editor_modal::render_task_editor_into(
                frame,
                area,
                &task_editor_modal::TaskEditorState::from_modal(m, true),
            );
            return (None, field_hits);
        }

        // Preview mode: render the selected (scoped) task's details + markdown
        // full-screen. A brand-new task always lands in `TaskEditor`, so the
        // preview branch only ever has a scoped task.
        let scoped = m
            .editing_id
            .and_then(|id| self.task_ui.cached_tasks.iter().find(|t| t.id == id));
        let Some(task) = scoped else {
            render_empty_workspace_hint(frame, area, " Task ", "No task selected.", false);
            return (None, Vec::new());
        };

        (self.render_task_detail_pane(frame, area, task), Vec::new())
    }

    /// Render a single task's read-only details + scrollable markdown preview
    /// full-screen. Shared by the tasks-panel preview and the global-search
    /// task preview (so previewing a task result also fills the central pane).
    fn render_task_detail_pane(
        &self,
        frame: &mut Frame,
        area: Rect,
        task: &crate::session::Task,
    ) -> Option<ScrollbarGeom> {
        // Related running sessions (spawned `<title> · #<id>` and/or a Send target).
        let related = self.task_related_session_indices(task);
        let sessions = if related.is_empty() {
            "none open".to_string()
        } else {
            related
                .iter()
                .filter_map(|&i| self.sessions.get(i))
                .map(|s| s.info.name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        };
        // Advertise the panel actions only while the tasks panel is focused (not
        // during a global-search preview); offer `o open` only when there is a
        // session to open.
        let focused = self.focus == InputFocus::TaskList;
        let hints: &[(&str, &str)] = if !focused {
            &[]
        } else if related.is_empty() {
            &[
                ("e", " edit  "),
                ("r", " run  "),
                ("Space", " status  "),
                ("n", " new  "),
                ("d", " del"),
            ]
        } else {
            &[
                ("e", " edit  "),
                ("r", " run  "),
                ("o", " open  "),
                ("Space", " status  "),
                ("n", " new  "),
                ("d", " del"),
            ]
        };
        crate::ui::task_detail::render_task_detail(
            frame,
            area,
            &crate::ui::task_detail::TaskDetail {
                title: &task.title,
                linkage: task_linkage(task),
                sessions,
                status: task.status.label(),
                source: &task.source,
                description: task.description.as_deref().unwrap_or(""),
                created: format_time_ago(task.created_at),
                updated: format_time_ago(task.updated_at),
            },
            self.task_ui.task_preview_scroll,
            hints,
        )
    }
}

/// Render a bordered "nothing scoped yet" placeholder in the central pane,
/// shared by the automation and task workspaces. `title` labels the border and
/// `hint` is the muted call-to-action; `focused` drives the border colour.
fn render_empty_workspace_hint(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    hint: &str,
    focused: bool,
) {
    let border = if focused {
        Theme::border_focused()
    } else {
        Theme::border_unfocused()
    };
    let block = ratatui::widgets::Block::default()
        .title(title.to_string())
        .borders(ratatui::widgets::Borders::ALL)
        .border_style(Style::default().fg(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint.to_string(),
            Style::default().fg(Theme::text_muted()),
        ))),
        inner,
    );
}

/// First 8 chars of a session UUID — enough to identify it in a compact label.
fn short_session_id(session_id: &crate::session::SessionId) -> String {
    session_id.to_string().chars().take(8).collect()
}

/// The final path component of a repo (e.g. `myrepo`), falling back to the full
/// path when it has no file name.
fn repo_display_name(repo_path: &std::path::Path) -> String {
    repo_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| repo_path.to_string_lossy().into_owned())
}

/// One-line description of a task's agent linkage for the details panel.
fn task_linkage(task: &crate::session::Task) -> String {
    use crate::session::AutomationAction;
    match &task.action {
        // `None`, and the automation-only `Exec` (a task never carries one), are
        // plain local todos with no agent linkage to show.
        None | Some(AutomationAction::Exec { .. }) => "local todo".to_string(),
        Some(AutomationAction::Send { session_id }) => {
            format!("send → {}", short_session_id(session_id))
        }
        Some(AutomationAction::Spawn {
            repo_path,
            worktree_branch,
            ..
        }) => {
            let repo = repo_display_name(repo_path);
            let target = match worktree_branch {
                Some(b) => format!("{repo}#{b}"),
                None => repo,
            };
            format!("spawn → {target}")
        }
    }
}

/// Live preview of when the editor's current schedule will next fire, or the
/// validation error for the current input.
fn editor_preview(m: &super::modals::AutomationEditorModal, now: u64) -> String {
    match m.build_schedule(now) {
        Ok(sched) => match sched.next_after(now, m.timezone().as_deref()) {
            Some(next) => format_countdown(next.saturating_sub(now)),
            None => "never (check schedule)".to_string(),
        },
        Err(e) => e,
    }
}

/// The rebindable section of the F1 help body: the rendered lines, the line
/// index of the selected action row (for centering the scroll), and the
/// `(line index, action index)` pairs used to build click hitboxes.
struct RebindableRows {
    help_lines: Vec<Line<'static>>,
    selected_line: usize,
    action_rows: Vec<(usize, usize)>,
}

/// Build the editable keybinding section of the help overlay (one section
/// header + one row per rebindable action), driven by `help_sections()` so the
/// row index matches `help.selected`.
fn build_rebindable_rows(
    keybindings: &KeyBindings,
    help: &super::modals::HelpModal,
) -> RebindableRows {
    let mut help_lines: Vec<Line<'static>> = Vec::new();
    let mut idx = 0usize;
    let mut selected_line = 0usize;
    let mut action_rows: Vec<(usize, usize)> = Vec::new();
    for (title, actions) in crate::session::keybindings::help_sections() {
        help_lines.push(help_section(title));
        for action in actions {
            let selected = idx == help.selected;
            let key = if selected && help.capturing {
                "Press the new shortcut… (Esc cancels)".to_string()
            } else {
                chords_display(keybindings.chords_for(action))
            };
            if selected {
                selected_line = help_lines.len();
            }
            action_rows.push((help_lines.len(), idx));
            help_lines.push(help_row(key, action.label(), selected));
            idx += 1;
        }
        help_lines.push(Line::from(""));
    }
    RebindableRows {
        help_lines,
        selected_line,
        action_rows,
    }
}

/// Scroll offset for the help body so the selected action row stays visible
/// (roughly centered), clamped to the real content range. `0` when everything
/// fits.
fn help_body_scroll(total: usize, body_h: usize, selected_line: usize) -> usize {
    if total <= body_h {
        return 0;
    }
    let max_scroll = total - body_h;
    selected_line.saturating_sub(body_h / 2).min(max_scroll)
}

/// Hitboxes for the rebindable action rows currently on screen after scrolling.
fn help_action_hitboxes(
    action_rows: &[(usize, usize)],
    scroll: usize,
    body_h: usize,
    body: Rect,
) -> Vec<crate::ui::RowHitbox> {
    action_rows
        .iter()
        .filter_map(|&(line, idx)| {
            let on_screen = line.checked_sub(scroll)?;
            if on_screen >= body_h {
                return None;
            }
            Some(crate::ui::RowHitbox {
                rect: Rect::new(body.x, body.y + on_screen as u16, body.width, 1),
                index: idx,
            })
        })
        .collect()
}

fn render_help_overlay(
    frame: &mut Frame,
    keybindings: &KeyBindings,
    help: &super::modals::HelpModal,
) -> crate::ui::ModalRender {
    let area = centered_rect(60, 70, frame.area());

    let inner = crate::ui::render_modal_frame(frame, area, "Keybindings");

    // Editable sections — driven by `keybindings::help_sections()`, the same
    // ordering as `Action::rebindable_in_order()`, so the row index lines up
    // with `help.selected` from the interactive editor. EVERY row here is
    // rebindable.
    let RebindableRows {
        mut help_lines,
        selected_line,
        action_rows,
    } = build_rebindable_rows(keybindings, help);

    // Fixed keys that are NOT rebindable: stateful sub-modes (file-viewer
    // search, modal selectors) and terminal pass-through. Shown for reference,
    // never selectable.
    help_lines.push(help_section("Fixed (not rebindable)"));
    help_lines.push(help_line(
        "/ then type".into(),
        "File viewer: search; Enter/↑/↓ cycle matches, Tab commits, Esc cancels",
    ));
    help_lines.push(help_line(
        "j/k/Enter/Esc".into(),
        "Modal selectors & automation run-history (the automations/tasks panes are rebindable above)",
    ));
    help_lines.push(help_line(
        "j/k {}/[]".into(),
        "Code review: move · { } prev/next file · [ ] prev/next hunk · g/G top/bottom · v split view",
    ));
    help_lines.push(help_line(
        "c/f/s".into(),
        "Code review: comment line/file/summary · r/R mark file/hunk reviewed (folds file)",
    ));
    help_lines.push(help_line(
        "Enter · x · y/e · t".into(),
        "Code review: fold/unfold file (edit comment on a comment row) · delete · copy/send · target",
    ));
    help_lines.push(help_line(
        "Mouse wheel".into(),
        "Terminal: scroll three lines",
    ));
    help_lines.push(help_line("Click+drag".into(), "Select text"));
    help_lines.push(help_line(
        "Click".into(),
        "Select row / focus pane; pickers: confirm row; footer & modal buttons",
    ));
    help_lines.push(help_line(
        "Hover".into(),
        "Highlight the clickable row/button under the pointer",
    ));
    help_lines.push(help_line(
        "*".into(),
        "Terminal: all other keys forwarded to session",
    ));

    // Cmd chords only arrive through the kitty keyboard protocol — worth a
    // note on the platform whose defaults include them.
    #[cfg(target_os = "macos")]
    {
        help_lines.push(Line::from(""));
        help_lines.push(help_section("macOS"));
        help_lines.push(help_line(
            "cmd+…".into(),
            "Needs a kitty-protocol terminal (iTerm2 3.5+, kitty, WezTerm, Ghostty) — not Terminal.app",
        ));
    }

    // Reserve the bottom row for the controls footer so it stays visible even
    // when the body overflows the modal height (the body scrolls, the footer
    // never does).
    let [body, footer] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .areas(inner);

    // On short terminals the body overflows; scroll it so the selected action
    // row stays visible (roughly centered), clamped to the real content range.
    let total = help_lines.len();
    let body_h = body.height as usize;
    let scroll = help_body_scroll(total, body_h, selected_line);

    frame.render_widget(Paragraph::new(help_lines).scroll((scroll as u16, 0)), body);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "j/k move · r rebind · d reset",
            Style::default().fg(Theme::text_muted()),
        ))),
        footer,
    );
    // Clickable `[ Reset all ]` (Shift+D) / `[ Close ]` (Esc) buttons.
    let button_hits = crate::ui::render_button_bar(
        frame,
        footer,
        &[
            crate::ui::ButtonSpec::secondary("Reset all"),
            crate::ui::ButtonSpec::primary("Close"),
        ],
        true,
    );
    let buttons = crate::ui::modal_button_keys(
        button_hits,
        &[
            (KeyCode::Char('D'), KeyModifiers::SHIFT),
            (KeyCode::Esc, KeyModifiers::NONE),
        ],
    );

    // Hitboxes for the rebindable action rows visible after scrolling;
    // section headers and the fixed-keys reference are not clickable.
    let total_actions = action_rows.len();
    let hitboxes = help_action_hitboxes(&action_rows, scroll, body_h, body);

    // Scrollbar (action-index space, so a drag maps straight to a selection)
    // when the body overflows the modal height.
    let geom = if total > body_h {
        let viewport = hitboxes.len().max(1);
        crate::ui::scrollbar::render_into(frame, body, total_actions, viewport, help.selected)
    } else {
        None
    };
    ((hitboxes, geom), buttons)
}

/// Format a slice of chords as the F1-help key column, e.g.
/// `"ctrl+y / f4"`. Empty input renders as `"<unbound>"` — should not
/// occur for built-in actions, but keeps the overlay legible if a user
/// override drops every chord.
fn chords_display(chords: &[KeyChord]) -> String {
    if chords.is_empty() {
        return "<unbound>".into();
    }
    chords
        .iter()
        .map(KeyChord::display)
        .collect::<Vec<_>>()
        .join(" / ")
}

fn help_section(title: &'static str) -> Line<'static> {
    Line::from(Span::styled(title, Theme::section_header()))
}

fn help_line(key: String, desc: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {key:<16}"), Theme::keybind()),
        Span::styled(desc, Style::default().fg(Theme::text_primary())),
    ])
}

/// A rebindable keybinding row in the interactive help editor. When
/// `selected`, the row is rendered with the active-item style and a `›`
/// marker so the user can see which action a captured chord will bind to.
fn help_row(key: String, desc: &'static str, selected: bool) -> Line<'static> {
    if selected {
        Line::from(vec![
            Span::styled(format!("› {key:<16}"), Theme::selected_item()),
            Span::styled(desc, Theme::selected_item()),
        ])
    } else {
        help_line(key, desc)
    }
}

/// Create a centered rectangle within the given area.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

/// Replace a cell's terminal-default (`Color::Reset`) background/foreground
/// with the active theme's `app_bg` / `text_primary`. Cells with an explicit
/// colour are left untouched.
fn repaint_reset_cell(
    cell: &mut ratatui::buffer::Cell,
    app_bg: ratatui::style::Color,
    text_primary: ratatui::style::Color,
) {
    if cell.bg == ratatui::style::Color::Reset {
        cell.bg = app_bg;
    }
    if cell.fg == ratatui::style::Color::Reset {
        cell.fg = text_primary;
    }
}

/// Format a millisecond timestamp as an absolute local clock time
/// (`"MM-DD HH:MM"`), for run-history rows.
pub(super) fn format_clock(millis: u64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(millis as i64).single() {
        Some(dt) => dt.format("%m-%d %H:%M").to_string(),
        None => String::new(),
    }
}

/// Format a millisecond timestamp as a human-readable "time ago" string.
pub(super) fn format_time_ago(millis: u64) -> String {
    let now = crate::sync::current_time_millis();
    let elapsed_secs = now.saturating_sub(millis) / 1000;
    if elapsed_secs < 60 {
        format!("{elapsed_secs}s ago")
    } else if elapsed_secs < 3600 {
        format!("{}m ago", elapsed_secs / 60)
    } else if elapsed_secs < 86400 {
        format!("{}h ago", elapsed_secs / 3600)
    } else {
        format!("{}d ago", elapsed_secs / 86400)
    }
}

/// Format a remaining-milliseconds value as a human-readable countdown.
pub(super) fn format_countdown(remaining_ms: u64) -> String {
    let secs = remaining_ms / 1000;
    if secs == 0 {
        "due".to_string()
    } else if secs < 60 {
        format!("in {secs}s")
    } else if secs < 3600 {
        let m = secs / 60;
        let s = secs % 60;
        if s == 0 {
            format!("in {m}m")
        } else {
            format!("in {m}m {s}s")
        }
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m == 0 {
            format!("in {h}h")
        } else {
            format!("in {h}h {m}m")
        }
    }
}

/// Truncate a string to `max_len` characters, appending "..." if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

/// Fuzzy-match a query against a session's fields (name/agent/branch/cwd/status),
/// returning highlight positions per field — drives live session-list
/// highlighting from the global-search query.
fn session_fuzzy(query: &str, info: &SessionInfo) -> Option<project_list::SessionMatch> {
    let name = crate::fuzzy::fuzzy_match(query, &info.name).map(|m| m.positions);
    let agent = crate::fuzzy::fuzzy_match(query, &info.agent).map(|m| m.positions);
    let branch = info
        .worktrees
        .first()
        .and_then(|wt| crate::fuzzy::fuzzy_match(query, &wt.branch))
        .map(|m| m.positions);
    let cwd = info
        .repo_display_names
        .iter()
        .find_map(|n| crate::fuzzy::fuzzy_match(query, n))
        .map(|m| m.positions);
    let status_str = info.status.to_string();
    let status = crate::fuzzy::fuzzy_match(query, &status_str).map(|m| m.positions);
    project_list::SessionMatch::from_matches(name, agent, branch, cwd, status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_countdown_zero() {
        assert_eq!(format_countdown(0), "due");
    }

    #[test]
    fn format_countdown_sub_minute() {
        assert_eq!(format_countdown(999), "due");
        assert_eq!(format_countdown(1_000), "in 1s");
        assert_eq!(format_countdown(45_000), "in 45s");
        assert_eq!(format_countdown(59_999), "in 59s");
    }

    #[test]
    fn format_countdown_minutes() {
        assert_eq!(format_countdown(60_000), "in 1m");
        assert_eq!(format_countdown(90_000), "in 1m 30s");
        assert_eq!(format_countdown(300_000), "in 5m");
        assert_eq!(format_countdown(3_599_000), "in 59m 59s");
    }

    #[test]
    fn format_countdown_hours() {
        assert_eq!(format_countdown(3_600_000), "in 1h");
        assert_eq!(format_countdown(5_400_000), "in 1h 30m");
        assert_eq!(format_countdown(7_200_000), "in 2h");
    }

    #[test]
    fn truncate_str_short() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_exact_fit() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn truncate_str_needs_truncation() {
        assert_eq!(truncate_str("hello world", 8), "hello...");
    }

    #[test]
    fn truncate_str_empty() {
        assert_eq!(truncate_str("", 5), "");
    }
}
