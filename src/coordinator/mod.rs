//! The coordinator: one iteration of the loop, and the workers it drains.
//!
//! `App` is declared in `main.rs` — it holds the whole of the loop's state and
//! is the one type here. Its methods are split across this directory by *what
//! they are for*, because a hundred of them in one `impl` block was not a
//! surface anybody reviews: the loop and its workers here, then `commands`, `publish`,
//! `draw`, `input`, `mouse`, `focus` and `interface`.
//!
//! The split is by file only. There is still exactly one model and one loop —
//! splitting `App` itself into several would be the mistake ADR-22 already
//! rejected for v1's `app/mod.rs`, and for the same reason: the invariants that
//! matter (a frame settles, focus never rests on a hidden pane, a command's
//! result reaches the pane that asked) hold *across* these groups.

mod commands;
mod draw;
mod focus;
mod input;
mod interface;
mod mouse;
mod publish;

use super::*;

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
            let tick_start = timing.then(Instant::now);

            self.poll_reload();
            self.apply_commands(&mut terminal);
            self.poll_command_bus();
            self.sync_terminals_and_agents();
            self.serve_worker_stores();
            self.apply_external_requests();

            // Recorded BEFORE the paint so `tick` and `frame` decompose rather
            // than nest: the iteration is roughly tick + republish + frame +
            // the input wait, and a number that contained the paint could not
            // tell those apart.
            if let Some(start) = tick_start {
                self.timings.tick.record(start.elapsed());
            }

            self.paint_if_due(&mut terminal)?;
            self.report_perf();
            self.drain_input(&mut input_failures)?;
        }
        Ok(())
    }

    /// The interface directory and `settings.toml`, both edited from outside.
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
                app.reload_interface();
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
    /// Both are gated on timing being active. Publishing especially: each write
    /// bumps every other thurbox connection's `data_version`, which costs them a
    /// full shared-state reload on their next poll — an idle default instance
    /// must never churn that row.
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
                .map(|op| format!("{}:{}ms", op.name, op.ms))
                .collect();
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
                "perf_window"
            );
            self.perf_window_base = counters;
            self.perf_window_tick = counters.iterations;
            self.timings.reset_window();
        }

        // The snapshot, on its own slower cadence: it is a database write, not
        // a log line.
        // `map_or(true, ..)` rather than `is_none_or`: the latter is stable
        // since 1.82 and this crate's MSRV is 1.75.
        let due = self
            .perf_published_at
            .map_or(true, |at| at.elapsed() >= PERF_PUBLISH_INTERVAL);
        if !due {
            return;
        }
        self.perf_published_at = Some(Instant::now());
        let json = thurbox::kernel::perf::snapshot_json(
            &counters,
            &self.timings,
            &self.startup,
            self.snapshots.current().sessions.len(),
        );
        if let Some(db) = snapshots_db() {
            if let Err(e) = db.set_perf_snapshot(&json.to_string()) {
                // Never worth failing a frame over; the CLI simply reports the
                // older snapshot, or none.
                tracing::warn!("could not publish the perf snapshot: {e}");
            }
        }
    }

    /// Time a named synchronous operation, ringing it if it was slow enough to
    /// be felt and warning if it was slow enough to be a stall.
    ///
    /// Always measured, unlike the histograms: the call sites are rare,
    /// user-triggered operations rather than the hot loop, and an interactive
    /// stall has to be attributable even when nobody had a HUD open.
    pub(crate) fn time_op<T>(&mut self, name: &'static str, f: impl FnOnce(&mut Self) -> T) -> T {
        let start = Instant::now();
        let out = f(self);
        let elapsed = start.elapsed();
        if self.timings.record_op(name, elapsed) {
            tracing::warn!(op = name, ms = elapsed.as_millis() as u64, "slow op");
        }
        out
    }

    /// Advance the shared animation clock, but only while something is moving.
    ///
    /// Conservative in the safe direction: saying "animating" when nothing is
    /// costs one cache miss, while saying "not animating" when something is
    /// would freeze it on screen.
    pub(crate) fn advance_animation(&mut self) {
        let animating = self
            .snapshots
            .current()
            .sessions
            .iter()
            .any(|row| row.status == "working")
            || self.commands.has_inflight();
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
        if let Some(id) = wanted {
            if let Some(row) = self.snapshots.current().session(&id) {
                if let Some(cwd) = row.cwd.clone() {
                    // `base_branch`, never `branch`. Against its own branch a
                    // session diffs to nothing, and publishing that as `ready`
                    // is a confident wrong answer.
                    self.diffs
                        .request(&id, cwd, row.base_branch.clone(), &row.backend);
                }
            }
        }
        if self.diffs.poll() {
            self.note_data_change();
        }

        // The creation flow's reads. It asks by leaving a key in `store`,
        // which is written the moment its handler runs, so a request made
        // on a keystroke is served on the very next iteration. Requesting
        // is idempotent, so asking every iteration costs three lookups.
        let wants = self.repo_wants();
        self.repos.serve(&wants);
        if self.repos.poll() {
            self.note_data_change();
        }

        // A session that has stayed deleted past its undo window keeps its
        // worktrees and loses its agent — the reap v1 does when the undo
        // expires.
        for session in self
            .reaper
            .observe(self.snapshots.current(), Instant::now())
        {
            self.commands
                .dispatch(thurbox::kernel::command::Command::Reap { session });
        }

        // An update that replaced binaries is worth saying once; a check that
        // merely found a release shows up in the header instead.
        if let Some(message) = self.updates.poll() {
            self.toast(message);
        }

        // Machine, statusline and account metrics. Both calls are gated
        // internally (an interval, and one sample in flight at a time), so
        // running them every iteration costs a comparison.
        self.metrics.sample(self.metric_subjects());
        if self.metrics.poll() {
            self.note_data_change();
        }
    }

    /// State another process left for this one, and the notifications that
    /// follow from it.
    pub(crate) fn apply_external_requests(&mut self) {
        // A focus request from another process: a clicked notification's
        // action callback, or `thurbox-cli session focus`, both of which
        // leave a row in the database for whoever is running the interface.
        // Taken atomically, so two instances cannot both claim it.
        if let Some(id) = self.snapshots.take_focus_request() {
            self.focus_on_session(&id);
        }

        // Acknowledge a finished turn on the way out of it: moving the
        // selection off a `done` session is what retires its filled dot.
        // Read from the store because the selection belongs to the session
        // list, which is a plugin.
        let selected = self.host.shared_string("selected");
        if selected != self.last_selected_session {
            if let Some(left) = self.last_selected_session.take() {
                self.snapshots.acknowledge(&left);
            }
            self.last_selected_session = selected;
            self.dirty = true;
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
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        self.terminals.sync(self.snapshots.current(), rows, cols);
        // New agent output, noticed the way v1 notices it: one lock-free sum
        // of atomics per iteration, marking the screen dirty. Without this a
        // printing agent is drawn only when the 250ms floor comes round.
        let output_gen = self.terminals.output_generation();
        if output_gen != self.last_output_gen {
            self.last_output_gen = output_gen;
            self.last_activity = Instant::now();
            // Deliberately NOT a data change: new agent output moves the vt100
            // grid a surface paints from, not anything published. Treating it as
            // one would move the epoch on every frame under a printing agent and
            // make the whole gate worthless (design.md — Risks).
            self.dirty = true;
        }
        // The stuck-`working` fallback, run here because it asks the
        // terminals a question and they have just been synced — and run
        // every tick rather than at refresh, because output moves between
        // two reads of the database.
        let terminals = &self.terminals;
        if self
            .snapshots
            .apply_output_quiescence(|id| terminals.millis_since_output(id))
            > 0
        {
            self.dirty = true;
        }
        // A session whose agent is gone gets it back, which is what v1 does
        // at restore and what makes a session survive a reboot.
        self.respawn_missing_agents();
        // And the shell it had open, whose window outlived the interface.
        self.readopt_shells();
        // Programs plugins asked for. Served here because resolving a
        // session to a directory and a machine is the loop's knowledge, not
        // a store's — the store owns the bounds and the queue.
        self.serve_runs();
        // A remote agent's hooks cannot call `thurbox-cli` — they set a tmux pane
        // option, which arrives here over the control-mode subscription. Draining
        // it into the same columns a local signal writes is the whole of remote
        // status: the dot, the acknowledgment and the notification are all
        // downstream of that one write.
        let events = self.terminals.drain_hook_events();
        if !events.is_empty() && self.snapshots.apply_hook_states(events, Instant::now()) > 0 {
            self.dirty = true;
        }
    }

    /// Take what plugins asked for, start what may start, and publish answers.
    ///
    /// The runner closure is what makes a run land on the right machine: it
    /// resolves the session to its directory and its host **before** the worker
    /// starts, so an unreachable host is a reported failure rather than a
    /// program run against a path that only exists somewhere else.
    pub(crate) fn serve_runs(&mut self) {
        let asked = self.host.drain_runs();
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
            for (plugin, ask) in asked {
                self.runs.request(&plugin, ask, &runner);
            }
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
        let sessions: std::collections::HashMap<String, (std::path::PathBuf, String)> = self
            .snapshots
            .current()
            .sessions
            .iter()
            .filter_map(|row| {
                let cwd = row.cwd.clone()?;
                Some((row.id.clone(), (cwd, row.backend.clone())))
            })
            .collect();
        std::sync::Arc::new(move |ask: &thurbox::kernel::runs::Ask| {
            let Some((cwd, backend)) = sessions.get(&ask.session) else {
                return thurbox::kernel::runs::Run::Failed(format!(
                    "no session {} to run it in",
                    ask.session
                ));
            };
            let Some(host) = thurbox::session_ops::resolve_host(backend) else {
                return thurbox::kernel::runs::Run::Failed(format!(
                    "'{backend}' is not in hosts.toml — cannot reach the machine it runs on"
                ));
            };
            let command = thurbox::kernel::runs::command_for(&ask.program, cwd, host.as_ref());
            thurbox::kernel::runs::capture(command, ask.timeout)
        })
    }

    /// Relaunch the agent of any session whose window is gone.
    ///
    /// v1's `respawn_stale_session`, reached differently. v1 asks the question
    /// once, synchronously, during restore; here discovery is a worker, so the
    /// answer arrives a moment later and the question is asked each frame until
    /// it can be answered. Once per session per run either way — a session that
    /// cannot be relaunched must not be relaunched forever.
    ///
    /// It goes through `restart`, which is exactly a respawn once killing a
    /// window that is not there stopped being an error: same session id, same
    /// worktrees, same host, and `resume_trigger_for` deciding whether the agent
    /// resumes its conversation or starts fresh.
    pub(crate) fn respawn_missing_agents(&mut self) {
        let missing = self.terminals.missing_agents(self.snapshots.current());
        for id in missing {
            if !self.respawned.insert(id.clone()) {
                continue;
            }
            let name = self
                .snapshots
                .current()
                .session(&id)
                .map(|row| row.name.clone())
                .unwrap_or_else(|| id.clone());
            self.dispatch_tracked(thurbox::kernel::command::Command::Restart {
                session: id.clone(),
            });
            self.toast(format!("{name}: agent was gone, relaunching"));
        }
    }

    /// Re-attach any session to the shell window it already had.
    ///
    /// v1 does this at restore (`readopt_shell_pane`); here attaching is a
    /// worker, so the shell is picked up on the frame after the agent's own pane
    /// lands. Cheap to ask every iteration: it is a map lookup for a session that
    /// has no shell recorded, which is nearly all of them.
    pub(crate) fn readopt_shells(&mut self) {
        let wanted: Vec<(String, String)> = self
            .snapshots
            .current()
            .sessions
            .iter()
            .filter(|row| row.shell_backend_id.is_some())
            .filter(|row| self.terminals.is_attached(&row.id))
            .filter(|row| !self.terminals.has_shell(&row.id))
            .filter_map(|row| {
                row.shell_backend_id
                    .clone()
                    .map(|pane| (row.id.clone(), pane))
            })
            .collect();
        // Asked before the size is: `terminal::size()` is a syscall, and this
        // runs on every iteration of a loop that polls every 10ms. Almost always
        // there is nothing to adopt.
        if wanted.is_empty() {
            return;
        }
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        for (session, pane) in wanted {
            if self.terminals.readopt_shell(&session, &pane, rows, cols) {
                self.dirty = true;
            } else {
                // The window is gone — closed outside thurbox, or its server
                // restarted. Forget it, or this is retried on every iteration
                // forever against a pane that will never answer.
                let _ = self.snapshots.forget_shell(&session);
            }
        }
    }

    /// Persist the pane id of a session's companion shell.
    pub(crate) fn remember_shell(&mut self, session: &str) {
        let Some(pane) = self.terminals.shell_pane_id(session) else {
            return;
        };
        if let Err(e) = self.snapshots.remember_shell(session, &pane) {
            tracing::warn!("could not record the shell pane for {session}: {e}");
        }
    }
}
