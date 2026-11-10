//! The coordinator: one iteration of the loop, and the workers it drains.
//!
//! `App` is declared in `main.rs` — it holds the whole of the loop's state and
//! is the one type here. Its methods are split across this directory by *what
//! they are for*, because a hundred of them in one `impl` block was not a
//! surface anybody reviews: the loop and its workers here, then `commands`, `publish`,
//! `draw`, `input`, `mouse`, `focus` and `interface` — plus `paste` (turning a
//! Windows key stream back into a paste), `boot` (the startup
//! sequence `main` delegates to) and `chrome` (the free helpers for the
//! terminal, the error panel and the perf HUD).
//!
//! The split is by file only. There is still exactly one model and one loop —
//! splitting `App` itself into several would be the mistake ADR-22 already
//! rejected for v1's `app/mod.rs`, and for the same reason: the invariants that
//! matter (a frame settles, focus never rests on a hidden pane, a command's
//! result reaches the pane that asked) hold *across* these groups.

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

// The chrome helpers keep their bare names at every call site in this
// directory, which is where all of them live.
pub(crate) use chrome::*;

/// A keystroke's echo, owed by the terminal it was sent to.
pub(crate) struct EchoWait {
    /// The surface the key went to.
    surface: String,
    /// The first output sequence this key may claim as its answer.
    target_seq: u64,
    /// When the key was sent.
    sent: Instant,
}

impl EchoWait {
    /// Until when the keystroke's own frame waits for the echo, so that the
    /// two are painted as one — see [`ECHO_HOLD`].
    pub(crate) fn hold_until(&self) -> Instant {
        self.sent + ECHO_HOLD
    }
}

impl App {
    pub(crate) fn run(&mut self, mut terminal: DefaultTerminal) -> Result<(), Box<dyn Error>> {
        // Consecutive input failures, reset by the first event that reads
        // cleanly. See `drain_input` for why one is survivable.
        let mut input_failures: u32 = 0;
        while !self.quit {
            Counters::bump(&self.perf.iterations);
            // One cached bool, so a default run pays nothing for the two
            // `Instant` reads below (ADR-P11).
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
            // After everything above has published and before the paint, so a
            // handler sees this iteration's state and its writes land in the
            // frame about to be painted. A no-op while nothing is queued.
            self.dispatch_events(&mut terminal);

            // Recorded BEFORE the paint so `tick` and `frame` decompose rather
            // than nest: the iteration is roughly tick + republish + frame +
            // the input wait, and a number that contained the paint could not
            // tell those apart.
            if let Some(start) = tick_start {
                self.timings.tick.record(start.elapsed());
            }

            self.paint_if_due(&mut terminal)?;
            self.rebuild_full_vm();
            // After the paint, which is what says which surface has the focus.
            self.forward_app_copy();
            self.focus_terminal();
            self.report_perf();
            self.drain_input(&mut input_failures)?;
        }
        Ok(())
    }

    /// Tell the terminals which surface the keys go to, so a pane gaining the
    /// focus is sized to its rect here before anything is typed into it
    /// (`Terminals::focus`). Gated as [`Self::forward_app_copy`] is: a pane
    /// under a modal, a float or a field is not where the user is typing.
    pub(crate) fn focus_terminal(&mut self) {
        let takes_keys = !self.overlay_owns_input() && self.focused_wants_session_input();
        let focused = self.focused_surface.as_deref().filter(|_| takes_keys);
        self.terminals.focus(focused);
    }

    /// Put the focused app's OSC 52 copy on the user's clipboard.
    ///
    /// tmux never hands a control-mode client a selection, so this process —
    /// the one on the user's machine reading the pane's bytes — is the only one
    /// that can. Only the pane a keystroke would reach may write: any other,
    /// and the focused one while a modal, a float or a field takes the keys,
    /// is refused at the pane (`Terminals::take_app_copy`). Of the writes since
    /// the last iteration, the newest that is a copy wins, as it would at a
    /// terminal. tmux never sees the write (`set-clipboard external`), so there
    /// is no second copy to suppress.
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

    /// The interface directory and live configuration files.
    pub(crate) fn poll_reload(&mut self) {
        if self.watcher.changed() {
            self.reload_at = Some(Instant::now() + DEBOUNCE);
        }
        // The settings file, edited from outside. A live change is already in
        // force by the time this returns; the other two outcomes are the ones
        // worth saying out loud.
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

    /// How long to wait for input before going round the loop again.
    ///
    /// See [`IDLE_TICK`]: this is a poll timeout, not a frame deadline, so
    /// stretching it while nothing is happening costs responsiveness nowhere.
    pub(crate) fn poll_timeout(&self) -> Duration {
        if self.last_activity.elapsed() >= QUIESCENT_AFTER {
            IDLE_TICK
        } else {
            TICK
        }
    }

    /// Start waiting for the echo of input about to be sent to `surface`.
    ///
    /// Called *before* the send, so the sequence it records cannot already
    /// include the answer.
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
            // A previous key may still be waiting when this one is sent. Its
            // answer is not this key's answer, so reserve the following move.
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

    /// Wake the loop only for the oldest echo still owed.
    pub(crate) fn arm_echo_wake(&self) {
        let seq = self
            .echo
            .front()
            .and_then(|echo| self.terminals.output_seq_cell(&echo.surface));
        thurbox::backend::output_wake::arm(seq);
    }

    /// Owe the next frame to an echo that has arrived, or stop waiting for one
    /// that is not coming — see [`ECHO_WINDOW`].
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

    /// The first read of an input batch: wait up to `timeout` for an event —
    /// or, while an echo is owed, until it arrives, whichever is first.
    ///
    /// `event::poll` is woken by the terminal and nothing else, so an echo that
    /// lands mid-wait would sit out the rest of it: at the 10 ms tick that was
    /// most of a keystroke's latency once the output floor was out of the way.
    pub(crate) fn wait_for_input(
        &self,
        timeout: Duration,
    ) -> std::io::Result<Option<crossterm::event::Event>> {
        let Some(echo) = self.echo.front() else {
            return next_event(timeout);
        };
        // Back in time to paint the held keystroke frame if no echo came.
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

    /// Sleep up to `left` for a terminal event or armed agent output
    /// (`backend::output_wake`), returning the event if it was one.
    #[cfg(unix)]
    fn sleep_until_input_or_output(
        &self,
        left: Duration,
    ) -> std::io::Result<Option<crossterm::event::Event>> {
        // crossterm reads the terminal from stdin when stdin is one; polling
        // any other descriptor would be polling the wrong thing.
        // SAFETY: `isatty` only inspects a descriptor number.
        let tty = unsafe { libc::isatty(libc::STDIN_FILENO) } == 1;
        let Some(wake) = thurbox::backend::output_wake::read_fd().filter(|_| tty) else {
            return next_event(left.min(ECHO_POLL));
        };
        // An event crossterm has already read and queued would not make stdin
        // readable again.
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
        // Rounded up: a zero timeout would spin until the deadline.
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

    /// Whether the wall-clock timing of ADR-P11 is being collected.
    ///
    /// One cached bool plus one field read, checked once per iteration: the
    /// hot loop must not pay for observability nobody asked for.
    pub(crate) fn perf_timing_active(&self) -> bool {
        self.perf_log || self.hud
    }

    /// The reporting half of ADR-P11: the periodic `perf_window` log line and
    /// the JSON snapshot `thurbox-cli perf` reads.
    ///
    /// Both are gated on timing being active. Telemetry goes through the
    /// snapshot connection and is excluded from other stores' row generations,
    /// so observing the loop does not cause a snapshot rebuild.
    pub(crate) fn report_perf(&mut self) {
        if !self.perf_timing_active() {
            return;
        }
        // Mirrored from the host, which owns the two caches: the counters are
        // the only way a skip is visible at all (`frame-cost`).
        self.perf.renders_skipped.store(
            self.host.skipped_renders(),
            std::sync::atomic::Ordering::Relaxed,
        );
        self.perf.groups_reused.store(
            self.host.reused_groups(),
            std::sync::atomic::Ordering::Relaxed,
        );
        let counters = self.perf.read();

        // The window line: counter deltas plus this window's percentiles, then
        // the timings reset so each window stands alone.
        if self.perf_log
            && counters.iterations.saturating_sub(self.perf_window_tick) >= PERF_WINDOW_TICKS
        {
            let window = counters.since(&self.perf_window_base);
            let slow: Vec<String> = self
                .timings
                .slow_ops
                .iter_recent()
                .map(|op| match &op.plugin {
                    Some(plugin) => format!("{}:{}ms@{plugin}", op.name, op.ms),
                    None => format!("{}:{}ms", op.name, op.ms),
                })
                .collect();
            let plugins = self.host.plugin_report().summary(5);
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
                plugins,
                "perf_window"
            );
            self.perf_window_base = counters;
            self.perf_window_tick = counters.iterations;
            self.timings.reset_window();
            self.host.reset_plugin_perf();
        }

        // The snapshot, on its own slower cadence: it is a database write, not
        // a log line.
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
            &self.host.plugin_report(),
        );
        // Off the loop: `thurbox-cli perf` reads the file whenever it is asked.
        let text = json.to_string();
        std::thread::spawn(move || {
            if let Err(e) = thurbox::kernel::perf::publish_snapshot(&text) {
                tracing::warn!("could not publish the perf snapshot: {e}");
            }
        });
    }

    /// Time a named synchronous operation, ringing it if it was slow enough to
    /// be felt and warning if it was slow enough to be a stall.
    ///
    /// Always measured, unlike the histograms: the call sites are rare,
    /// user-triggered operations rather than the hot loop, and an interactive
    /// stall has to be attributable even when nobody had a HUD open.
    pub(crate) fn time_op<T>(&mut self, name: &'static str, f: impl FnOnce(&mut Self) -> T) -> T {
        self.host.begin_op();
        let start = Instant::now();
        let out = f(self);
        let elapsed = start.elapsed();
        let plugin = self.host.end_op();
        let log_plugin = plugin.clone().unwrap_or_default();
        if self.timings.record_op(name, elapsed, plugin) {
            tracing::warn!(
                op = name,
                ms = elapsed.as_millis() as u64,
                plugin = log_plugin,
                "slow op"
            );
        }
        out
    }

    /// Advance the shared animation clock, but only while something is moving.
    ///
    /// Conservative in the safe direction: saying "animating" when nothing is
    /// costs one cache miss, while saying "not animating" when something is
    /// would freeze it on screen.
    pub(crate) fn advance_animation(&mut self) {
        let printing = self.terminals.printing();
        let animating = self
            .snapshots
            .current()
            .sessions
            .iter()
            .any(|row| {
                row.status == thurbox::session::SessionState::Working
                    // A `running` session animates only while its pane is
                    // printing, so the clock has to run for that case too —
                    // otherwise the spinner it gates freezes mid-turn.
                    || (row.status == thurbox::session::SessionState::Running
                        && printing.contains(&row.id))
            })
            || self.commands.has_inflight()
            // The creation flow spins over the repo store's reads too, and it
            // is a pure pane: without the clock its "listing…"/"fetching…"
            // spinner freezes for the life of a slow remote read.
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

    /// Note that something a plugin reads has moved.
    ///
    /// Separate from [`Self::note_data_change`] because the callers inside
    /// `republish` are already *in* the frame that will show the new value:
    /// marking it dirty there would buy a second repaint of what is about to be
    /// painted anyway.
    pub(crate) fn note_published_change(&mut self) {
        self.data_epoch = self.data_epoch.wrapping_add(1);
    }

    /// Note that published data moved, and that the screen owes a frame.
    ///
    /// The two go together everywhere this is used: something that changes what
    /// a plugin would read is also something worth repainting for, and worth
    /// polling quickly again after.
    pub(crate) fn note_data_change(&mut self) {
        self.note_published_change();
        // A worker result or a row another process wrote is the answer to
        // something someone asked for, so it earns the tight floor. Only agent
        // output — which arrives whether or not anyone is looking — does not.
        self.note_input();
        self.last_activity = Instant::now();
    }

    /// Note that a person did something — a key, a click, a paste, a resize.
    ///
    /// Distinct from [`Self::note_data_change`] only in what it does *not* do:
    /// no published value moved, so nothing needs re-publishing. Both mark the
    /// frame as owed to a person, which is what buys it [`MIN_FRAME_INTERVAL`]
    /// rather than the slower floor agent output gets.
    pub(crate) fn note_input(&mut self) {
        self.dirty = true;
        self.input_dirty = true;
    }

    /// The worker-backed stores: diffs, repository reads, the deletion reaper,
    /// update checks and metrics.
    pub(crate) fn serve_worker_stores(&mut self) {
        if self.host.poll_files() {
            self.note_data_change();
        }
        // The selected session's diff, computed on a worker. Requesting is
        // idempotent, so calling it every frame costs nothing after the
        // first.
        //
        // Driven by the *selection*, not by `focused_session`: the latter is
        // re-derived each frame from the focused plugin's session surface, so
        // only a pane drawing a terminal ever asked for one. A pane that shows
        // a diff draws no terminal by definition, and would never be handed the
        // thing it exists to show. The selection is what "the session the user
        // is looking at" actually means, and the session list publishes it.
        let wanted = self
            .host
            .shared_string("selected")
            .or_else(|| self.focused_session.clone());
        if self.diffs.retain_selected(wanted.as_deref()) {
            self.note_data_change();
        }
        if let Some(id) = wanted {
            if let Some(row) = self.snapshots.current().session(&id) {
                // Over ssh a diff is network bytes — up to MAX_DIFF_BYTES a
                // read — so it is fetched only while a pane on screen reads
                // it. The held answer stays, aging, for when one does again.
                let shown = self.diffs_shown || !thurbox::kernel::diff::over_ssh(&row.backend);
                if shown {
                    self.diffs.request_row(row);
                }
            }
        }
        if self.diffs.poll() {
            self.note_data_change();
        }
        // Input a stream pane dropped is reported where a command's error
        // is, not drawn into the pane's grid.
        for notice in self.terminals.take_input_notices() {
            self.report(notice, Level::Error);
        }
        // The content search: dispatch what the strip asks for, fold in what
        // the worker finished.
        self.serve_search();

        // The creation flow's reads. It asks by leaving a key in `store`,
        // which is written the moment its handler runs, so a request made
        // on a keystroke is served on the very next iteration. Requesting
        // is idempotent, so asking every iteration costs four lookups.
        let wants = self.repo_wants();
        self.repos.serve(&wants);
        if self.repos.poll() {
            self.note_data_change();
        }

        // What Windows said about its clipboard, for a paste press that could
        // not be answered when it happened (WSL only — `clipboard::ImageProbe`).
        self.poll_image_probe();
        self.poll_remote_drop();

        // An update that replaced binaries is worth saying once; a check that
        // merely found a release shows up in the header instead.
        if let Some(message) = self.updates.poll() {
            self.toast(message);
        }

        // Machine and statusline metrics. Asked whether a sample is
        // due before the subject list is built — `metric_subjects` clones two
        // strings per session, which is worth paying once a second but not per
        // iteration.
        if self.metrics.wants_sample() {
            self.metrics.sample(self.metric_subjects());
        }
        if self.metrics.poll() {
            self.dirty = true;
        }
    }

    /// State another process left for this one, and the notifications that
    /// follow from it.
    pub(crate) fn apply_external_requests(&mut self) {
        // A clicked notification's action callback asks for its session.
        if let Some(id) = thurbox::notifications::take_focus_request() {
            self.focus_on_session(&id);
        }

        // Notify on the blocked edge, observed where status is read so the
        // rule cannot drift from what the list shows.
        self.notifier
            .observe(self.snapshots.current(), self.focused_session.as_deref());
    }

    /// Terminals and agents, plus the per-tick derivations that read them.
    pub(crate) fn sync_terminals_and_agents(&mut self) {
        // Attach to any session that gained a pane, and drop the ones that
        // went away. Sized to the screen as a starting point; each surface
        // resizes its own pane to its real rect when it paints.
        let (cols, rows) = self.screen_size;
        self.terminals.sync(self.snapshots.current(), rows, cols);
        // Only painted surfaces owe output frames. Hidden panes keep parsing,
        // and their metadata still travels through the snapshot and meta gates.
        let output_gen = self.terminals.visible_output_generation();
        if output_gen != self.last_output_gen {
            self.last_output_gen = output_gen;
            self.last_activity = Instant::now();
            // Deliberately NOT a data change: new agent output moves the vt100
            // grid a surface paints from, not anything published. Treating it as
            // one would move the epoch on every frame under a printing agent and
            // make the whole gate worthless.
            self.dirty = true;
        }
        self.settle_echo();
        self.readopt_shells();
        // Programs plugins asked for. Served here because resolving a
        // session to a directory and a machine is the loop's knowledge, not
        // a store's — the store owns the bounds and the queue.
        self.serve_runs();
    }

    /// Take what plugins asked for, start what may start, and publish answers.
    ///
    /// Session directories/routes are snapshotted for the runner; host
    /// resolution and execution stay on its worker. An unknown target is a
    /// reported failure, never a fallback to another machine.
    pub(crate) fn serve_runs(&mut self) {
        let asked = self.host.drain_run_requests();
        // Nothing asked now, nothing ever asked: every interface with no
        // capability-using plugin, i.e. almost all of them. Checked *before*
        // `self.runner()`, which clones every session's id, cwd and backend into a
        // fresh map behind a new `Arc` — the `||` below evaluates it whenever
        // nothing was asked, so that was happening on every 10 ms iteration.
        if asked.is_empty() && self.runs.is_empty() {
            return;
        }
        if !asked.is_empty() || self.runs.poll(&self.runner()) {
            self.dirty = true;
        }
        if !asked.is_empty() {
            let runner = self.runner();
            for (plugin, request) in asked {
                match request {
                    thurbox::kernel::runs::Request::Ask(ask) => {
                        self.runs.request(&plugin, ask, &runner);
                    }
                    thurbox::kernel::runs::Request::Forget(key) => {
                        self.runs.forget(&plugin, &key);
                    }
                }
            }
        }
        // Drained on every pass so the store never holds them; the host ignores
        // them while timing is off.
        for event in self.runs.drain_events() {
            self.host.note_run(&event);
        }
        // Answers, per plugin, for the next render to read.
        let mut answers = thurbox::kernel::host::RunAnswers::new();
        for plugin in &self.host.plugins {
            if plugin.capabilities.is_empty() {
                continue;
            }
            let mine = self.runs.for_plugin(&plugin.path);
            if !mine.is_empty() {
                answers.insert(
                    plugin.path.clone(),
                    mine.into_iter()
                        .map(|(key, run)| (key.to_string(), run.clone()))
                        .collect(),
                );
            }
        }
        self.host.set_runs(answers);
    }

    /// How one ask becomes a program on the right machine.
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
                        "{name} is another machine; a plugin's program runs on this one only"
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
                    "session {session} runs on {name}; a plugin's program runs on this machine only"
                ));
            }
            let command = thurbox::kernel::runs::command_for(&ask.program, cwd);
            thurbox::kernel::runs::capture(command, ask.timeout)
        })
    }

    /// Fold in the shells the terminals opened or lost, adopt again the shell
    /// a session had open before this interface started, and close the shell
    /// of a session Fleet deleted.
    ///
    /// A shell outlives the interface in a window of its own, so it is kept in
    /// [`ShellBook`]: in memory for the loop, saved to `interface.json` by a
    /// worker. A worker's answer is matched against the book, never against a
    /// save still in flight.
    pub(crate) fn readopt_shells(&mut self) {
        use thurbox::config::interface_state::ShellRecord;
        for result in self.terminals.drain_shell_results() {
            use thurbox::kernel::terminal::ShellResult;
            self.dirty = true;
            match result {
                ShellResult::Ready {
                    session,
                    pane,
                    stamp,
                } => {
                    // A re-adopted shell stays on the route it was saved on.
                    let route = match self.shells.records.get(&session) {
                        Some(record) if record.pane == pane && record.stamp == stamp => {
                            Some(record.route.clone())
                        }
                        _ => self
                            .snapshots
                            .current()
                            .session(&session)
                            .and_then(|row| self.terminals.shell_route(row.remote_host.as_deref()))
                            .map(|route| route.format()),
                    };
                    let Some(route) = route else { continue };
                    self.terminals.record_shell(&session, Some(pane.clone()));
                    self.shells
                        .set(&session, Some(ShellRecord { route, pane, stamp }));
                }
                ShellResult::Gone {
                    session,
                    open_requested,
                    pane,
                } => {
                    self.terminals.record_shell(&session, None);
                    self.shells.set(&session, None);
                    if let Some(pane) = pane {
                        self.shells.lost.insert((session.clone(), pane));
                    }
                    // A new shell takes the host's current multiplexer.
                    let current = self
                        .snapshots
                        .current()
                        .session(&session)
                        .and_then(|row| self.terminals.shell_route(row.remote_host.as_deref()));
                    if let Some(route) = current {
                        self.terminals.use_shell_route(&session, &route);
                    }
                    if open_requested {
                        self.open_shell(&session, None);
                    }
                }
                // Nothing proved the pane gone: its record stays, and the
                // re-adopt is tried again.
                ShellResult::Unreachable { session, error } => {
                    tracing::debug!("shell of {session} not re-adopted yet: {error}");
                }
                ShellResult::Failed { session, error } => {
                    self.report(
                        format!("could not open a shell for {session}: {error}"),
                        Level::Error,
                    );
                }
            }
        }
        self.close_shells_of_deleted_sessions();
        // Cheap to ask every iteration: nearly every row has no shell
        // recorded, and the size is the cached one.
        let wanted: Vec<(String, ShellRecord, thurbox::session::Route)> = self
            .snapshots
            .current()
            .sessions
            .iter()
            .filter(|row| self.terminals.is_attached(&row.id))
            .filter(|row| !self.terminals.has_shell(&row.id))
            .filter_map(|row| {
                let record = self.shells.records.get(&row.id)?;
                // A shell on another machine than the one the Task runs on
                // now is not this session's companion any more.
                let route = self
                    .terminals
                    .readopt_route(row.remote_host.as_deref(), &record.route)?;
                Some((row.id.clone(), record.clone(), route))
            })
            .filter(|(session, record, _)| {
                !self
                    .shells
                    .lost
                    .contains(&(session.clone(), record.pane.clone()))
            })
            .collect();
        let (cols, rows) = self.screen_size;
        for (session, record, route) in wanted {
            self.terminals.use_shell_route(&session, &route);
            self.terminals
                .readopt_shell(&session, &record.pane, &record.stamp, rows, cols);
        }
    }

    /// Close the shell of each session that was in the snapshot and is not any
    /// more: its Task was deleted. An interface that merely closes keeps its
    /// shells. The source keeps its last rows while Fleet cannot be read, so a
    /// row leaves the snapshot only when its Task left the store.
    fn close_shells_of_deleted_sessions(&mut self) {
        let snapshot = self.snapshots.current();
        let present: std::collections::HashSet<&str> = snapshot
            .sessions
            .iter()
            .map(|row| row.id.as_str())
            .collect();
        let deleted: Vec<String> = self
            .shells
            .records
            .keys()
            .filter(|session| {
                self.shells.seen.contains(*session) && !present.contains(session.as_str())
            })
            .cloned()
            .collect();
        let seen: Vec<String> = present.iter().map(|id| id.to_string()).collect();
        for session in deleted {
            if let Some(record) = self.shells.records.get(&session) {
                self.terminals.kill_shell_window(
                    &record.route,
                    &record.pane,
                    &record.stamp,
                    &session,
                );
            }
            self.terminals.record_shell(&session, None);
            self.shells.set(&session, None);
            self.shells.seen.remove(&session);
        }
        self.shells.seen.extend(seen);
    }
}

/// One record per companion shell, keyed by session.
type ShellRecords =
    std::collections::BTreeMap<String, thurbox::config::interface_state::ShellRecord>;

/// One change to the book: a session's shell, or `None` for none.
type ShellChange = (
    String,
    Option<thurbox::config::interface_state::ShellRecord>,
);

/// The companion shells this interface knows: the records, the panes found
/// gone, and the sessions seen in the snapshot. Changes are made here, on the
/// loop, and one worker writes each to `interface.json`, in order, through the
/// read-change-write every writer of the file takes
/// (`interface_state::update`): it changes that one session's entry in the
/// file as it is then, so two interfaces on one config directory keep each
/// other's shells, and the loop never waits on the file's lock.
pub(crate) struct ShellBook {
    pub(crate) records: ShellRecords,
    /// `(session, pane)` found gone, until that session's record changes.
    pub(crate) lost: std::collections::HashSet<(String, String)>,
    /// Sessions the snapshot has shown: one that leaves it was deleted.
    pub(crate) seen: std::collections::HashSet<String>,
    saves: Option<std::sync::mpsc::Sender<ShellChange>>,
    saver: Option<std::thread::JoinHandle<()>>,
}

impl ShellBook {
    /// The book as `interface.json` holds it, with its saver started.
    pub(crate) fn load() -> Self {
        Self::start(
            thurbox::config::interface_state::load().shells,
            |session, record| {
                let saved = thurbox::config::interface_state::update(|state| match record {
                    Some(record) => {
                        state.shells.insert(session.to_string(), record.clone());
                    }
                    None => {
                        state.shells.remove(session);
                    }
                });
                if let Err(e) = saved {
                    tracing::warn!("could not record the shell of {session}: {e}");
                }
            },
        )
    }

    /// A book over `records` whose changes `save` writes, one at a time, on
    /// a worker of its own.
    fn start(
        records: ShellRecords,
        save: impl Fn(&str, Option<&thurbox::config::interface_state::ShellRecord>) + Send + 'static,
    ) -> Self {
        let (saves, queue) = std::sync::mpsc::channel::<ShellChange>();
        let saver = std::thread::spawn(move || {
            while let Ok((session, record)) = queue.recv() {
                save(&session, record.as_ref());
            }
        });
        Self {
            records,
            lost: std::collections::HashSet::new(),
            seen: std::collections::HashSet::new(),
            saves: Some(saves),
            saver: Some(saver),
        }
    }

    /// Record `session`'s shell, or that it has none, and queue the change.
    /// The panes found gone for the session are forgotten: they were judged
    /// against the record this replaces.
    pub(crate) fn set(
        &mut self,
        session: &str,
        record: Option<thurbox::config::interface_state::ShellRecord>,
    ) {
        let changed = match &record {
            Some(record) => {
                self.records.insert(session.to_string(), record.clone()) != Some(record.clone())
            }
            None => self.records.remove(session).is_some(),
        };
        if changed {
            self.lost.retain(|(lost, _)| lost != session);
            if let Some(saves) = &self.saves {
                let _ = saves.send((session.to_string(), record));
            }
        }
    }

    /// Write what is still queued, and stop the saver. Called once the
    /// interface has drawn its last frame, so a clean exit loses no change.
    pub(crate) fn finish(&mut self) {
        drop(self.saves.take());
        if let Some(saver) = self.saver.take() {
            let _ = saver.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ShellBook;
    use thurbox::config::interface_state::ShellRecord;

    fn record(pane: &str) -> ShellRecord {
        ShellRecord {
            route: "local:tmux".into(),
            pane: pane.into(),
            stamp: "s".into(),
        }
    }

    /// A book whose saver writes `interface.json` under `dir`.
    fn book_over(dir: &std::path::Path) -> ShellBook {
        let dir = dir.to_path_buf();
        ShellBook::start(Default::default(), move |session, record| {
            let _isolated = thurbox::paths::TestPathGuard::new(&dir);
            thurbox::config::interface_state::update(|state| match record {
                Some(record) => {
                    state.shells.insert(session.to_string(), record.clone());
                }
                None => {
                    state.shells.remove(session);
                }
            })
            .expect("save");
        })
    }

    /// A pane found gone is remembered per session, and forgotten once that
    /// session's record changes: the same id on another session, or a new
    /// shell of this one that the server gave the old id, is judged afresh.
    #[test]
    fn a_lost_pane_belongs_to_its_session_and_clears_with_its_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut book = book_over(dir.path());
        book.set("a", Some(record("%1")));
        book.lost.insert(("a".into(), "%1".into()));
        book.lost.insert(("b".into(), "%1".into()));
        book.set("a", Some(record("%1")));
        assert_eq!(
            book.lost.len(),
            2,
            "an unchanged record keeps what was lost"
        );
        book.set("a", Some(record("%2")));
        assert!(!book.lost.contains(&("a".into(), "%1".into())));
        assert!(
            book.lost.contains(&("b".into(), "%1".into())),
            "another session's pane stays lost"
        );
        book.finish();
    }

    /// Two interfaces on one config directory each save their own session's
    /// shell into the file as it is then, so neither erases the other's, and
    /// what is queued at a clean exit is written.
    #[test]
    fn two_books_on_one_file_keep_each_others_shells() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = book_over(dir.path());
        let mut second = book_over(dir.path());
        first.set("a", Some(record("%1")));
        second.set("b", Some(record("%2")));
        first.finish();
        second.finish();
        let _isolated = thurbox::paths::TestPathGuard::new(dir.path());
        let shells = thurbox::config::interface_state::load().shells;
        assert_eq!(shells.get("a"), Some(&record("%1")), "{shells:?}");
        assert_eq!(shells.get("b"), Some(&record("%2")), "{shells:?}");
    }
}
