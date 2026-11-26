pub(crate) mod boot;
pub(crate) mod chrome;
mod commands;
mod control;
mod draw;
pub(crate) mod editor;
pub(crate) mod events;
mod focus;
mod image_drop;
pub(crate) mod input;
mod interface;
pub(crate) mod mouse;
pub(crate) mod paste;
mod publish;

use std::error::Error;
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;

use thurbox::kernel::bands::Level;
use thurbox::kernel::perf::Counters;

use crate::{
    App, DEBOUNCE, ECHO_HOLD, ECHO_POLL, ECHO_WINDOW, IDLE_TICK, PERF_PUBLISH_INTERVAL,
    PERF_WINDOW_TICKS, QUIESCENT_AFTER, TICK,
};

pub(crate) use chrome::*;

pub(crate) struct EchoWait {
    surface: String,
    target_seq: u64,
    sent: Instant,
}

impl EchoWait {
    pub(crate) fn hold_until(&self) -> Instant {
        self.sent + ECHO_HOLD
    }
}

impl App {
    pub(crate) fn run(&mut self, mut terminal: DefaultTerminal) -> Result<(), Box<dyn Error>> {
        let mut input_failures: u32 = 0;
        while !self.quit {
            Counters::bump(&self.perf.iterations);
            let timing = self.perf_timing_active();
            self.host.set_perf_timing(timing);
            let tick_start = timing.then(Instant::now);

            self.poll_reload();
            self.apply_commands(&mut terminal);
            self.poll_command_bus();
            self.sync_terminals_and_agents();
            self.serve_worker_stores();
            self.apply_external_requests();
            self.refresh_control_state(false);
            self.serve_ui_control();
            self.dispatch_events(&mut terminal);

            if let Some(start) = tick_start {
                self.timings.tick.record(start.elapsed());
            }

            self.paint_if_due(&mut terminal)?;
            self.rebuild_full_vm();
            self.forward_app_copy();
            self.focus_terminal();
            self.report_perf();
            self.drain_input(&mut input_failures)?;
        }
        Ok(())
    }

    pub(crate) fn focus_terminal(&mut self) {
        let takes_keys = !self.overlay_owns_input() && self.focused_wants_session_input();
        let focused = self.focused_surface.as_deref().filter(|_| takes_keys);
        self.terminals.focus(focused);
    }

    // tmux never hands a control-mode client an OSC 52 selection.
    pub(crate) fn forward_app_copy(&mut self) {
        let takes_keys = !self.overlay_owns_input() && self.focused_wants_session_input();
        let focused = self.focused_surface.as_deref().filter(|_| takes_keys);
        let requests = self.terminals.take_app_copy(focused);
        let Some(text) = requests
            .iter()
            .rev()
            .find_map(|request| thurbox::clipboard::app_copy_text(&request.target, &request.data))
        else {
            if !requests.is_empty() {
                tracing::debug!("ignored an app's OSC 52 write that is not a copy");
            }
            return;
        };
        let message = match thurbox::clipboard::copy(
            &text,
            self.clipboard.as_mut(),
            self.config.in_force().clipboard.provider,
        ) {
            Ok(route) => format!(
                "app copied {} line(s){}",
                text.lines().count(),
                route.toast_suffix()
            ),
            Err(e) => format!("app copy failed: {e}"),
        };
        self.toast(message);
    }

    pub(crate) fn poll_reload(&mut self) {
        if self.watcher.changed() {
            self.reload_at = Some(Instant::now() + DEBOUNCE);
        }
        match self.config.poll() {
            Some(thurbox::kernel::config::Reloaded::Live) => {
                self.toast("settings reloaded".to_string());
                self.dirty = true;
            }
            Some(thurbox::kernel::config::Reloaded::NeedsRestart) => {
                self.toast("settings reloaded — some changes apply on restart".to_string());
                self.dirty = true;
            }
            Some(thurbox::kernel::config::Reloaded::Failed(error)) => {
                self.report(format!("settings: {error}"), Level::Error);
            }
            None => {}
        }
        if self.reload_at.is_some_and(|at| Instant::now() >= at) {
            self.reload_at = None;
            self.time_op("interface_reload", |app| {
                app.reload_interface(events::ReloadReason::Watch);
                app.refresh_sources();
                app.collect_declarations();
            });
            self.clamp_focus();
            self.dirty = true;
        }
    }

    pub(crate) fn poll_timeout(&self) -> Duration {
        if self.last_activity.elapsed() >= QUIESCENT_AFTER {
            IDLE_TICK
        } else {
            TICK
        }
    }

    pub(crate) fn expect_echo(&mut self, surface: &str) -> Option<EchoWait> {
        let current = self.terminals.output_seq(surface)?;
        let after_queued = self
            .echo
            .iter()
            .rev()
            .find(|echo| echo.surface == surface)
            .map_or(current, |echo| echo.target_seq);
        Some(EchoWait {
            surface: surface.to_string(),
            target_seq: current.max(after_queued).saturating_add(1),
            sent: Instant::now(),
        })
    }

    fn echo_arrived(&self) -> bool {
        self.echo.front().is_some_and(|echo| {
            self.terminals
                .output_seq(&echo.surface)
                .is_some_and(|seq| seq >= echo.target_seq)
        })
    }

    pub(crate) fn arm_echo_wake(&self) {
        let seq = self
            .echo
            .front()
            .and_then(|echo| self.terminals.output_seq_cell(&echo.surface));
        thurbox::backend::output_wake::arm(seq);
    }

    pub(crate) fn settle_echo(&mut self) {
        while let Some(echo) = self.echo.front() {
            if self.echo_arrived() {
                self.echo_due = self.echo.pop_front().map(|echo| echo.surface);
                self.dirty = true;
                break;
            }
            if echo.sent.elapsed() < ECHO_WINDOW {
                break;
            }
            self.echo.pop_front();
        }
        self.arm_echo_wake();
    }

    pub(crate) fn wait_for_input(
        &self,
        timeout: Duration,
    ) -> std::io::Result<Option<crossterm::event::Event>> {
        let Some(echo) = self.echo.front() else {
            return next_event(timeout);
        };
        let now = Instant::now();
        let hold = echo.hold_until();
        let deadline = if hold > now {
            (now + timeout).min(hold)
        } else {
            now + timeout
        };
        loop {
            if self.echo_arrived() {
                return Ok(None);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            if let Some(event) = self.sleep_until_input_or_output(left)? {
                return Ok(Some(event));
            }
        }
    }

    #[cfg(unix)]
    fn sleep_until_input_or_output(
        &self,
        left: Duration,
    ) -> std::io::Result<Option<crossterm::event::Event>> {
        // crossterm reads the terminal from stdin when stdin is a tty.
        // SAFETY: `isatty` only inspects a descriptor number.
        let tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
        let Some(wake) = thurbox::backend::output_wake::read_fd().filter(|_| tty) else {
            return next_event(left.min(ECHO_POLL));
        };
        // An event crossterm has already queued does not make stdin readable again.
        if let Some(event) = next_event(Duration::ZERO)? {
            return Ok(Some(event));
        }
        let mut fds = [
            libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // `poll` with a zero timeout returns at once, so round up.
        let ms = left.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32;
        // SAFETY: `fds` is a valid array of two pollfds for the duration of the
        // call.
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
        thurbox::backend::output_wake::drain();
        next_event(Duration::ZERO)
    }

    #[cfg(not(unix))]
    fn sleep_until_input_or_output(
        &self,
        left: Duration,
    ) -> std::io::Result<Option<crossterm::event::Event>> {
        next_event(left.min(ECHO_POLL))
    }

    pub(crate) fn perf_timing_active(&self) -> bool {
        self.perf_log || self.hud
    }

    pub(crate) fn report_perf(&mut self) {
        if !self.perf_timing_active() {
            return;
        }
        self.perf.renders_skipped.store(
            self.host.skipped_renders(),
            std::sync::atomic::Ordering::Relaxed,
        );
        self.perf.groups_reused.store(
            self.host.reused_groups(),
            std::sync::atomic::Ordering::Relaxed,
        );
        let counters = self.perf.read();

        if self.perf_log
            && counters.iterations.saturating_sub(self.perf_window_tick) >= PERF_WINDOW_TICKS
        {
            let window = counters.since(&self.perf_window_base);
            let slow: Vec<String> = self
                .timings
                .slow_ops
                .iter_recent()
                .map(|op| match &op.owner {
                    Some(owner) => format!("{}:{}ms@{owner}", op.name, op.ms),
                    None => format!("{}:{}ms", op.name, op.ms),
                })
                .collect();
            let mods = self.host.mod_report().summary(5);
            tracing::info!(
                iterations = window.iterations,
                frames = window.frames,
                skipped = window.skipped,
                renders = window.renders,
                failures = window.failures,
                reloads = window.reloads,
                frame_p50_us = self.timings.frame.percentile_us(50),
                frame_p95_us = self.timings.frame.percentile_us(95),
                frame_max_us = self.timings.frame.max_us(),
                republish_p50_us = self.timings.republish.percentile_us(50),
                republish_p95_us = self.timings.republish.percentile_us(95),
                tick_p50_us = self.timings.tick.percentile_us(50),
                tick_p95_us = self.timings.tick.percentile_us(95),
                slow_ops = slow.join(" "),
                plugins = mods,
                "perf_window"
            );
            self.perf_window_base = counters;
            self.perf_window_tick = counters.iterations;
            self.timings.reset_window();
            self.host.reset_mod_perf();
        }

        let due = self
            .perf_published_at
            .is_none_or(|at| at.elapsed() >= PERF_PUBLISH_INTERVAL);
        if !due {
            return;
        }
        self.perf_published_at = Some(Instant::now());
        let json = thurbox::kernel::perf::snapshot_json(
            &counters,
            &self.timings,
            &self.startup,
            self.snapshots.current().sessions.len(),
            &self.host.mod_report(),
        );
        let text = json.to_string();
        std::thread::spawn(move || {
            if let Err(e) = thurbox::kernel::perf::publish_snapshot(&text) {
                tracing::warn!("could not publish the perf snapshot: {e}");
            }
        });
    }

    pub(crate) fn time_op<T>(&mut self, name: &'static str, f: impl FnOnce(&mut Self) -> T) -> T {
        self.host.begin_op();
        let start = Instant::now();
        let out = f(self);
        let elapsed = start.elapsed();
        let owner = self.host.end_op();
        let log_mod = owner.clone().unwrap_or_default();
        if self.timings.record_op(name, elapsed, owner) {
            tracing::warn!(
                op = name,
                ms = elapsed.as_millis() as u64,
                plugin = log_mod,
                "slow op"
            );
        }
        out
    }

    pub(crate) fn advance_animation(&mut self) {
        let printing = self.terminals.printing();
        let animating = self.snapshots.current().sessions.iter().any(|row| {
            row.status == thurbox::session::SessionState::Working
                || (row.status == thurbox::session::SessionState::Running
                    && printing.contains(&row.id))
        }) || self.commands.has_inflight()
            || self.terminals.starting()
            || self.repos.in_flight();
        if !animating {
            return;
        }
        let step =
            (self.started.elapsed().as_secs_f64() * thurbox::kernel::host::ANIMATION_HZ) as u64;
        if step != self.animation_step {
            self.animation_step = step;
            self.animation_tick = self.animation_tick.wrapping_add(1);
        }
    }

    pub(crate) fn note_published_change(&mut self) {
        self.data_epoch = self.data_epoch.wrapping_add(1);
    }

    pub(crate) fn note_data_change(&mut self) {
        self.note_published_change();
        self.note_input();
        self.last_activity = Instant::now();
    }

    pub(crate) fn note_input(&mut self) {
        self.dirty = true;
        self.input_dirty = true;
    }

    pub(crate) fn serve_worker_stores(&mut self) {
        if self.host.poll_files() {
            self.note_data_change();
        }
        let wanted = self
            .host
            .shared_string("selected")
            .or_else(|| self.focused_session.clone());
        if self.diffs.retain_selected(wanted.as_deref()) {
            self.note_data_change();
        }
        if let Some(id) = wanted {
            if let Some(row) = self.snapshots.current().session(&id) {
                let shown = self.diffs_shown || !thurbox::kernel::diff::over_ssh(&row.backend);
                if shown {
                    self.diffs.request_row(row);
                }
            }
        }
        if self.diffs.poll() {
            self.note_data_change();
        }
        for notice in self.terminals.take_input_notices() {
            self.report(notice, Level::Error);
        }
        self.serve_search();

        let wants = self.repo_wants();
        self.repos.serve(&wants);
        self.snapshots.want_places(
            self.host
                .shared_string(thurbox::kernel::snapshot::WANT_PLACES)
                .filter(|fleet| !fleet.is_empty()),
        );
        if self.repos.poll() {
            self.note_data_change();
        }

        self.poll_image_probe();
        self.poll_remote_drop();

        if let Some(message) = self.updates.poll() {
            self.toast(message);
        }

        if self.metrics.wants_sample() {
            self.metrics.sample(self.metric_subjects());
        }
        if self.metrics.poll() {
            self.dirty = true;
        }
    }

    pub(crate) fn apply_external_requests(&mut self) {
        if let Some(id) = thurbox::notifications::take_focus_request() {
            self.focus_on_session(&id);
        }

        self.notifier
            .observe(self.snapshots.current(), self.focused_session.as_deref());
    }

    pub(crate) fn sync_terminals_and_agents(&mut self) {
        let (cols, rows) = self.screen_size;
        self.terminals.sync(self.snapshots.current(), rows, cols);
        let output_gen = self.terminals.visible_output_generation();
        if output_gen != self.last_output_gen {
            self.last_output_gen = output_gen;
            self.last_activity = Instant::now();
            self.dirty = true;
        }
        self.settle_echo();
        self.collect_shells();
        self.serve_runs();
    }

    pub(crate) fn serve_runs(&mut self) {
        let asked = self.host.drain_run_requests();
        if asked.is_empty() && self.runs.is_empty() {
            return;
        }
        if !asked.is_empty() || self.runs.poll(&self.runner()) {
            self.dirty = true;
        }
        if !asked.is_empty() {
            let runner = self.runner();
            for (owner, request) in asked {
                match request {
                    thurbox::kernel::runs::Request::Ask(ask) => {
                        self.runs.request(&owner, ask, &runner);
                    }
                    thurbox::kernel::runs::Request::Forget(key) => {
                        self.runs.forget(&owner, &key);
                    }
                }
            }
        }
        for event in self.runs.drain_events() {
            self.host.note_run(&event);
        }
        let mut answers = thurbox::kernel::host::RunAnswers::new();
        for loaded in &self.host.mods {
            if loaded.capabilities.is_empty() {
                continue;
            }
            let mine = self.runs.for_mod(&loaded.path);
            if !mine.is_empty() {
                answers.insert(
                    loaded.path.clone(),
                    mine.into_iter()
                        .map(|(key, run)| (key.to_string(), run.clone()))
                        .collect(),
                );
            }
        }
        self.host.set_runs(answers);
    }

    pub(crate) fn runner(&self) -> std::sync::Arc<thurbox::kernel::runs::Runner> {
        let sessions: std::collections::HashMap<String, (std::path::PathBuf, Option<String>)> =
            self.snapshots
                .current()
                .sessions
                .iter()
                .filter_map(|row| {
                    let cwd = row.cwd.clone()?;
                    Some((row.id.clone(), (cwd, row.remote_host.clone())))
                })
                .collect();
        std::sync::Arc::new(move |ask: &thurbox::kernel::runs::Ask| {
            let session = match &ask.target {
                thurbox::kernel::runs::Target::Session(session) => session,
                thurbox::kernel::runs::Target::Host(name) => {
                    return thurbox::kernel::runs::Run::Failed(format!(
                        "{name} is another machine; a mod's program runs on this one only"
                    ));
                }
                thurbox::kernel::runs::Target::Local => {
                    let Some(home) = thurbox::paths::home_dir() else {
                        return thurbox::kernel::runs::Run::Failed(
                            "no home directory to run it in".to_string(),
                        );
                    };
                    let command = thurbox::kernel::runs::command_for(&ask.program, &home);
                    return thurbox::kernel::runs::capture(command, ask.timeout);
                }
            };
            let Some((cwd, remote)) = sessions.get(session) else {
                return thurbox::kernel::runs::Run::Failed(format!(
                    "no session {session} to run it in"
                ));
            };
            if let Some(name) = remote {
                return thurbox::kernel::runs::Run::Failed(format!(
                    "session {session} runs on {name}; a mod's program runs on this machine only"
                ));
            }
            let command = thurbox::kernel::runs::command_for(&ask.program, cwd);
            thurbox::kernel::runs::capture(command, ask.timeout)
        })
    }

    pub(crate) fn collect_shells(&mut self) {
        for result in self.terminals.drain_shell_results() {
            use thurbox::kernel::terminal::ShellResult;
            self.dirty = true;
            if let ShellResult::Failed { session, error } = result {
                self.report(
                    format!("could not open a shell for {session}: {error}"),
                    Level::Error,
                );
            }
        }
    }
}
