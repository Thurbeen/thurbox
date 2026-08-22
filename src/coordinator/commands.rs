//! The write side: draining what Lua asked for, and reporting what happened.
//!
//! A plugin never acts; it queues a command and the answer arrives on a later
//! frame (rule 3). This is where the queue is drained, the ones the loop owns
//! are applied here, and everything that touches the world is handed to a worker
//! (rule 5). `tracked_commands` is why a finished command can still be reported:
//! a command simply *leaves* the in-flight list, so what it was about has to be
//! captured while the row it names still exists.

use super::*;

impl App {
    /// Commands plugins issued last frame, handed to the bus.
    ///
    /// Draining here rather than inside the render call is what keeps `command()`
    /// a queue push and nothing more.
    pub(crate) fn apply_commands(&mut self, terminal: &mut DefaultTerminal) {
        for command in self.host.drain_commands() {
            if self.apply_local_command(&command, terminal) {
                continue;
            }
            self.dispatch_tracked(command);
        }
    }

    /// The commands the loop applies itself, because they touch something no
    /// worker thread can reach — in-process state, the tty, the interface's own
    /// files. `true` = handled here; `false` = hand it to a worker.
    pub(crate) fn apply_local_command(
        &mut self,
        command: &thurbox::kernel::command::Command,
        terminal: &mut DefaultTerminal,
    ) -> bool {
        use thurbox::kernel::command::Command;
        match command {
            // Theme is applied here rather than dispatched: it mutates in-process
            // state a worker thread cannot reach, and it is instant, so nothing is
            // gained by making it asynchronous.
            Command::Theme { name } => {
                if let Err(e) = self.themes.select(name, snapshots_db().as_ref()) {
                    self.report(e, Level::Error);
                }
            }
            // Open a link, or copy it where nothing can open one — a remote
            // session or a bare tty has no browser, and spawning an opener there
            // goes nowhere.
            Command::OpenLink { url } => self.open_or_copy_link(url),
            // Editing the interface's own files. Applied here because it is two
            // file operations and the watcher turns the write into a reload — a
            // worker would only add a race.
            Command::Plugin { file, edit } => self.apply_plugin_edit(file, *edit),
            // Focus is the loop's own state, so it is applied here.
            Command::Focus { plugin, toggle } => self.apply_focus_command(plugin, *toggle),
            Command::Shell { session } => self.apply_shell_command(session),
            Command::Program {
                owner,
                name,
                program,
                argv,
                close,
            } => self.apply_program(owner, name, program, argv, *close),
            // The editor wants a controlling tty, which only this thread can hand
            // it — see `Command::Editor`.
            Command::Editor { session } => self.apply_editor_command(session, terminal),
            Command::Diff { session } => {
                // Drop the answer; the request at the top of the next iteration
                // recomputes it on the worker.
                self.diffs.invalidate(session);
                self.dirty = true;
            }
            // Copy needs the vt100 screen and the tty, neither of which a worker
            // thread can reach.
            Command::Copy { session } => self.apply_copy_command(session),
            // Settings live in the registry, which is in-process too.
            Command::Setting { key, value } => {
                let (plugin, id) = key.split_once('.').unwrap_or((key.as_str(), ""));
                if let Err(e) = self.registry.set_setting(plugin, id, value.clone()) {
                    self.report(e, Level::Error);
                }
            }
            // Repository memory is a read the flow also writes, so note the write
            // and drop the cached rows when it lands. Still dispatched.
            Command::Bookmark { .. } => {
                self.bookmark_in_flight = true;
                return false;
            }
            _ => return false,
        }
        true
    }

    /// `Command::Focus`: move focus, or return whence it came.
    pub(crate) fn apply_focus_command(&mut self, plugin: &str, toggle: bool) {
        let Some(index) = self.host.index_of(plugin) else {
            return;
        };
        let position = self
            .host
            .focusable()
            .iter()
            .position(|candidate| *candidate == index);
        // Already here, and asked to toggle: go back where the last focus change
        // came from. That memory is `focus_return`, which is the same one `Esc`
        // uses — so a pane reached by its own key leaves by either.
        if toggle && position == Some(self.focus) {
            let back = self.focus_return;
            if self.host.focusable().get(back).is_some() {
                self.focus_return = self.focus;
                self.focus = back;
                self.dirty = true;
            }
            return;
        }
        // Through `focus_plugin`, not by assigning `focus`: it records where focus
        // came from, and holds a request for a slot the arrangement has not placed
        // yet rather than refusing it (`kernel::focus::defer_until_placed`) —
        // which is what a pane opening itself needs. Assigning directly skipped
        // both, so a pane reached from a plugin command could not be left with
        // `Esc`.
        self.focus_plugin(index);
        self.dirty = true;
    }

    /// `Command::Shell`: a companion shell beside a session's agent.
    pub(crate) fn apply_shell_command(&mut self, session: &str) {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        // Resolved from the snapshot rather than the live
        // `Session`: an adopted one carries no cwd of its own.
        let cwd = self
            .snapshots
            .current()
            .session(session)
            .and_then(|row| self.terminals.launch_cwd(row));
        if let Err(e) = self
            .terminals
            .open_shell(session, rows, cols, cwd.as_deref())
        {
            self.report(format!("could not open a shell: {e}"), Level::Error);
        } else {
            // Its window outlives this process, so the id has to
            // as well — otherwise the next start forgets the
            // shell and orphans the window it left running.
            self.remember_shell(session);
        }
    }

    /// `Command::Editor`: hand the configured editor this thread's tty.
    pub(crate) fn apply_editor_command(&mut self, session: &str, terminal: &mut DefaultTerminal) {
        let dirs = self
            .snapshots
            .current()
            .session(session)
            .map(|row| row.member_dirs.clone())
            .unwrap_or_default();
        self.toast(match open_editor(terminal, &dirs) {
            Ok(message) => message,
            Err(e) => e,
        });
    }

    /// `Command::Copy`: the focused session's visible screen to the clipboard.
    pub(crate) fn apply_copy_command(&mut self, session: &str) {
        match self.terminals.visible_text(session) {
            Some(text) => {
                let outcome = thurbox::clipboard::copy(
                    &text,
                    self.clipboard.as_mut(),
                    thurbox::session::settings::global().clipboard.provider,
                );
                self.toast(match outcome {
                    Ok(route) => {
                        format!(
                            "copied {} lines{}",
                            text.lines().count(),
                            route.toast_suffix()
                        )
                    }
                    Err(e) => format!("copy failed: {e}"),
                });
            }
            None => self.report(
                "nothing to copy — this session has no live terminal yet",
                Level::Error,
            ),
        }
    }

    /// The command bus, and the snapshot a finished command invalidates.
    pub(crate) fn poll_command_bus(&mut self) {
        // A completed command changes what the rows say, so refresh at once
        // rather than waiting out the interval — that is what makes a
        // delete feel immediate instead of arriving up to 400ms later.
        if self.commands.poll() {
            self.report_finished_commands();
            // A finished creation is deliberately not acted on here: see
            // `focus_on_session` for why a session that just spawned does not
            // pull the view onto itself.

            // Wait for the last one: two adds in quick succession would
            // otherwise re-read between them and publish a list missing the
            // second.
            if self.bookmark_in_flight
                && !self
                    .commands
                    .inflight()
                    .iter()
                    .any(|item| item.kind == "bookmark")
            {
                self.bookmark_in_flight = false;
                self.repos.invalidate_bookmarks();
            }
            self.snapshots.refresh();
            self.note_data_change();
        } else {
            self.snapshots.refresh_if_due();
        }
    }

    /// Surface a command that failed, once.
    ///
    /// A failure lingers in the in-flight list briefly so a pane can draw it,
    /// which is not the same as telling the user: the row it appears on may not
    /// even be visible. The message band is where a failure is *reported*, at
    /// error severity, and each is reported once — the linger would otherwise
    /// re-raise it on every poll.
    pub(crate) fn report_finished_commands(&mut self) {
        // Failures, once each. The linger that lets a pane draw a failed row
        // would otherwise re-report it on every poll.
        let failures: Vec<(u64, String)> = self
            .commands
            .inflight()
            .iter()
            .filter(|item| !self.reported_failures.contains(&item.id))
            .filter_map(|item| item.error.clone().map(|error| (item.id, error)))
            .collect();
        for (id, error) in failures {
            self.reported_failures.insert(id);
            if let Some(tracked) = self.tracked_commands.get_mut(&id) {
                let (kind, label) = (tracked.kind, tracked.label.clone());
                tracked.failed = true;
                self.report(
                    thurbox::kernel::messages::failed(kind, label.as_deref(), &error),
                    Level::Error,
                );
            }
        }

        // Anything that left the list without having failed is a command that
        // worked. Reported because most of them change something the screen does
        // not obviously show — a sync, a restart, a worktree — and silence after
        // a keystroke reads as the key not having worked.
        let live: std::collections::HashSet<u64> = self
            .commands
            .inflight()
            .iter()
            .map(|item| item.id)
            .collect();
        let finished: Vec<(u64, TrackedCommand)> = self
            .tracked_commands
            .iter()
            .filter(|(id, _)| !live.contains(id))
            .map(|(id, tracked)| (*id, tracked.clone()))
            .collect();
        for (id, tracked) in finished {
            self.tracked_commands.remove(&id);
            if tracked.failed {
                continue;
            }
            // A command that replaced the session's pane must be followed by
            // letting go of the old one, or the interface keeps painting a pane
            // that no longer exists — a session that looks restarted and takes no
            // keys. This is told rather than detected: see `Terminals::forget`.
            if matches!(tracked.kind, "restart" | "restore") && !tracked.session.is_empty() {
                self.terminals.forget(&tracked.session);
                self.dirty = true;
            }
            if let Some(message) =
                thurbox::kernel::messages::done(tracked.kind, tracked.label.as_deref())
            {
                self.toast(message);
            }
        }
        self.reported_failures.retain(|id| live.contains(id));
    }

    /// Dispatch a command, remembering what it was so its outcome can be
    /// reported and its session let go of when it finishes.
    ///
    /// Tracked **here** rather than by sampling the in-flight list, because a
    /// command that succeeds is removed from that list inside `CommandBus::poll`
    /// — before anything gets to look at it. Sampling only caught commands that
    /// happened to be running when some *other* command finished, which for the
    /// common case (one restart, nothing else in flight) is never: the restart
    /// was never tracked, so it was never seen to finish, so its pane was never
    /// let go of and the session stayed frozen.
    ///
    /// It is also the only moment the subject can be resolved: a delete's row is
    /// gone by the time it reports.
    pub(crate) fn dispatch_tracked(&mut self, command: thurbox::kernel::command::Command) {
        let kind = command.kind();
        let session = command.session().to_string();
        let label = self
            .snapshots
            .current()
            .session(&session)
            .map(|row| row.name.clone())
            .filter(|label| !label.is_empty());
        let id = self.commands.dispatch(command);
        self.tracked_commands.insert(
            id,
            TrackedCommand {
                kind,
                session,
                label,
                failed: false,
            },
        );
        // Accepting a command changes `thurbox.commands`, which panes draw from:
        // the session list drops the row a `delete` names as soon as one is
        // accepted. Such a pane is `pure`, so without moving the epoch here it
        // is handed the tree built *before* the command existed, and the change
        // waits for whatever moves a signal next — the animation clock 125ms
        // later, or the completion. `poll_command_bus` already does this for the
        // completion; this is the submission half.
        self.note_data_change();
    }

    /// Tell the host which plugins the user turned off.
    ///
    /// Derived from the *stored* absolute paths rather than from the loaded
    /// plugins, because a disabled one is not loaded — it would not be in the
    /// list to filter. Relative, because that is what `build` compares against.
    /// Start or close a plugin's program pane.
    ///
    /// The gate is here rather than in the queue: a command is honoured after the
    /// call that made it, so the check is "may the plugin that asked, right now" —
    /// which is also what makes revoking trust take effect on the next frame
    /// rather than at the next reload.
    ///
    /// A refusal is **reported to the plugin's own error channel** rather than
    /// silently dropped, because a pane that cannot start needs to be able to say
    /// why: an empty box that never explains itself is the failure mode the
    /// capability model is otherwise prone to.
    pub(crate) fn apply_program(
        &mut self,
        owner: &str,
        name: &str,
        program: &str,
        argv: &[String],
        close: bool,
    ) {
        let key = thurbox::kernel::terminal::ProgramKey::new(owner, name);
        if close {
            if self.terminals.release_program(&key) {
                self.changed_this_frame = true;
            }
            return;
        }
        // Absent rather than refusing, as `run` is: a plugin that did not declare
        // the capability, or that the user has not trusted, simply cannot start
        // anything. Reported so an untrusted pane can say so instead of looking
        // broken.
        if !self
            .host
            .may_path(owner, thurbox::kernel::host::Capability::Program)
        {
            self.report(
                format!("{owner} may not run a program — trust it in settings → Interface"),
                Level::Error,
            );
            return;
        }

        // Born at the rect it will be painted into where the last frame recorded
        // one; `open_shell` documents why the render-time resize cannot correct a
        // bad birth size once the size memo looks settled.
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let rect = self.terminals.last_rect(&key.surface_id());
        let (rows, cols) = match rect {
            Some(rect) if rect.width > 0 && rect.height > 0 => (rect.height, rect.width),
            _ => (rows, cols),
        };
        // The interface directory: the one directory a plugin-owned pane can be
        // said to belong to, since it has no session and therefore no worktree.
        let cwd = self.ui_dir.clone();
        if let Err(e) = self
            .terminals
            .start_program(&key, program, argv, Some(&cwd), rows, cols)
        {
            self.report(e, Level::Error);
            return;
        }
        self.changed_this_frame = true;
    }
}
