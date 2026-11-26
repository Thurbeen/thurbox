use ratatui::DefaultTerminal;

use thurbox::kernel::bands::Level;

use super::open_editor;
use crate::{App, TrackedCommand};

impl App {
    pub(crate) fn apply_commands(&mut self, terminal: &mut DefaultTerminal) {
        for (chain, command) in self.host.drain_requests() {
            if self.refuse_ungranted(&chain, &command) || self.apply_local_command(&chain, &command)
            {
                continue;
            }
            self.dispatch_tracked(command);
        }
        self.open_requested_editor(terminal);
    }

    fn refuse_ungranted(
        &mut self,
        chain: &[String],
        command: &thurbox::kernel::command::Command,
    ) -> bool {
        let Some(error) = command.refusal_by(chain, |owner, need| self.host.may_path(owner, need))
        else {
            return false;
        };
        let subject = command.subject();
        self.enqueue_event(
            thurbox::kernel::events::Event::new("command.failed")
                .with("kind", Some(command.kind()))
                .with("subject", subject.as_deref())
                .with("error", Some(error.as_str())),
        );
        self.report(error, Level::Error);
        true
    }

    pub(crate) fn apply_local_command(
        &mut self,
        chain: &[String],
        command: &thurbox::kernel::command::Command,
    ) -> bool {
        use thurbox::kernel::command::Command;
        match command {
            Command::Theme { name } => {
                if let Err(e) = self.themes.select(name) {
                    self.report(e, Level::Error);
                }
                self.store_chosen_theme();
            }
            Command::OpenLink { url } => self.open_or_copy_link(url),
            Command::Mod { file, edit } => self.apply_mod_edit(file, *edit),
            Command::Focus { mod_name, toggle } => self.apply_focus_command(mod_name, *toggle),
            Command::Shell { session } => self.apply_shell_command(session),
            Command::Program {
                owner,
                name,
                program,
                argv,
                close,
                keys,
            } => self.apply_program(owner, name, program, argv, *close, keys.as_deref()),
            Command::Editor { session } => self.editor_request = Some(session.clone()),
            Command::Diff { session } => {
                self.diffs.invalidate(session);
                self.dirty = true;
            }
            Command::Copy { session } => self.apply_copy_command(session),
            Command::Setting { key, value } => {
                let (mod_name, id) = key.split_once('.').unwrap_or((key.as_str(), ""));
                if let Err(e) = self.registry.set_setting(mod_name, id, value.clone()) {
                    self.report(e, Level::Error);
                }
            }
            Command::Bookmark { .. } => {
                self.bookmark_in_flight = true;
                return false;
            }
            Command::Emit {
                owner,
                name,
                payload,
            } => {
                let source = self
                    .host
                    .name_of_path(owner)
                    .unwrap_or(owner.as_str())
                    .to_string();
                let mut event = thurbox::kernel::events::Event::new(name.clone());
                event.payload = payload.clone();
                event.payload.push(("source".to_string(), source.into()));
                self.enqueue_event(event);
            }
            Command::Action { fallback, action } => {
                let asked = self.fallback_pane(fallback);
                let asker = command.runs_for(chain);
                let previous = self.host.acting_for(asker);
                self.run_clicked_action(action, asked);
                self.host.acting_for(previous);
            }
            Command::ActionTarget {
                fallback,
                action,
                argument,
                value,
            } => {
                let asked = self.fallback_pane(fallback);
                let asker = command.runs_for(chain);
                let previous = self.host.acting_for(asker);
                self.run_clicked_action_with_args(action, asked, &[(argument, value)]);
                self.host.acting_for(previous);
            }
            Command::Message { text, level } => self.report(text.clone(), *level),
            _ => return false,
        }
        true
    }

    fn fallback_pane(&self, fallback: &str) -> usize {
        self.host
            .name_of_path(fallback)
            .and_then(|name| self.host.index_of(name))
            .unwrap_or(self.focus)
    }

    pub(crate) fn apply_focus_command(&mut self, mod_name: &str, toggle: bool) {
        let Some(index) = self.host.index_of(mod_name) else {
            return;
        };
        let position = self
            .host
            .focusable()
            .iter()
            .position(|candidate| *candidate == index);
        if toggle && position == Some(self.focus) {
            let back = self.focus_return;
            if self.host.focusable().get(back).is_some() {
                self.focus_return = self.focus;
                self.focus = back;
                self.dirty = true;
            }
            return;
        }
        self.focus_mod(index);
        self.dirty = true;
    }

    pub(crate) fn apply_shell_command(&mut self, session: &str) {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let Some(row) = self.snapshots.current().session(session).cloned() else {
            self.report(
                format!("could not open a shell: no session {session}"),
                Level::Error,
            );
            return;
        };
        match self.terminals.open_shell(&row, rows, cols) {
            Ok(thurbox::kernel::terminal::ShellOpening::Opening) => {}
            Ok(thurbox::kernel::terminal::ShellOpening::AfterStart) => {
                self.dispatch_tracked(thurbox::kernel::command::Command::Start {
                    session: session.to_string(),
                });
            }
            Err(e) => self.report(format!("could not open a shell: {e}"), Level::Error),
        }
    }

    pub(crate) fn store_chosen_theme(&mut self) {
        if let Some(name) = self.themes.take_chosen() {
            std::thread::spawn(move || {
                if let Err(e) =
                    thurbox::config::interface_state::update(|state| state.theme = Some(name))
                {
                    tracing::warn!("could not store the chosen theme: {e}");
                }
            });
        }
    }

    fn open_requested_editor(&mut self, terminal: &mut DefaultTerminal) {
        let Some(session) = self.editor_request.take() else {
            return;
        };
        let state = thurbox::config::interface_state::load();
        let row = self.snapshots.current().session(&session);
        if row.is_some_and(|row| !row.workspace_reachable()) {
            self.toast("this Task's workspace is on another machine; no editor opens here");
            return;
        }
        let dirs = row.map(|row| row.member_dirs.clone()).unwrap_or_default();
        self.toast(
            match open_editor(terminal, &dirs, state.editor_command, state.editor_mode) {
                Ok(message) => message,
                Err(e) => e,
            },
        );
        self.last_link_paints.clear();
    }

    pub(crate) fn apply_copy_command(&mut self, session: &str) {
        match self.terminals.visible_text(session) {
            Some(text) => {
                let outcome = thurbox::clipboard::copy(
                    &text,
                    self.clipboard.as_mut(),
                    self.config.in_force().clipboard.provider,
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

    pub(crate) fn poll_command_bus(&mut self) {
        if self.commands.poll() {
            for id in self.commands.take_deleted() {
                self.snapshots.hide_until_gone(id);
            }
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
            self.record_command_failures();
            let live: std::collections::HashSet<u64> = self
                .commands
                .inflight()
                .iter()
                .map(|item| item.id)
                .collect();
            let created = self
                .tracked_commands
                .iter()
                .filter(|(id, tracked)| {
                    !live.contains(id) && !tracked.failed && tracked.kind == "create"
                })
                .filter_map(|(_, tracked)| tracked.created.clone())
                .collect();
            self.report_after = Some(self.snapshots.request_refresh_awaiting(created));
        }
        self.snapshots.refresh_if_due();
        if let Some(ticket) = self.report_after {
            if self.snapshots.fresh_since(ticket) {
                self.report_after = None;
                self.report_finished_commands();
                self.note_data_change();
            }
        }
    }

    pub(crate) fn record_command_failures(&mut self) {
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
                let tracked = tracked.clone();
                self.report(
                    thurbox::kernel::messages::failed(kind, label.as_deref(), &error),
                    Level::Error,
                );
                self.note_command_failed(&tracked, &error);
            }
        }
    }

    pub(crate) fn report_finished_commands(&mut self) {
        self.record_command_failures();
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
            if let Some(message) =
                thurbox::kernel::messages::done(tracked.kind, tracked.label.as_deref())
            {
                self.toast(message);
            }
            self.note_command_done(&tracked);
        }
        self.reported_failures.retain(|id| live.contains(id));
    }

    pub(crate) fn dispatch_tracked(&mut self, command: thurbox::kernel::command::Command) {
        use thurbox::kernel::command::Command;
        let kind = command.kind();
        let session = command.session().to_string();
        let label = self
            .snapshots
            .current()
            .session(&session)
            .map(|row| row.name.clone())
            .filter(|label| !label.is_empty());
        let (name, fleet) = match &command {
            Command::Create { name, fleet, .. } => {
                (Some(name.clone()).filter(|n| !n.is_empty()), fleet.clone())
            }
            _ => (None, None),
        };
        let created = name
            .as_deref()
            .and_then(|name| self.commands.created_id(name, fleet.as_deref()));
        let force = matches!(command, Command::Delete { force: true, .. });
        if let Command::Start { session } = &command {
            self.terminals.start_requested(session);
        }
        let id = self.commands.dispatch(command);
        self.tracked_commands.insert(
            id,
            TrackedCommand {
                kind,
                session,
                label,
                failed: false,
                name,
                created,
                force,
            },
        );
        self.note_data_change();
    }

    pub(crate) fn apply_program(
        &mut self,
        owner: &str,
        name: &str,
        program: &str,
        argv: &[String],
        close: bool,
        keys: Option<&[u8]>,
    ) {
        let key = thurbox::kernel::terminal::ProgramKey::new(owner, name);
        if close {
            if self.terminals.release_program(&key) {
                self.changed_this_frame = true;
            }
            return;
        }
        if !self
            .host
            .may_path(owner, thurbox::kernel::host::Capability::Program)
        {
            self.refuse_program(
                name,
                format!("{owner} may not run a program — trust it in settings → Interface"),
            );
            return;
        }

        if let Some(keys) = keys {
            use thurbox::kernel::terminal::{plan_keys, KeysPlan};
            let running = self
                .terminals
                .program_state(&key)
                .is_some_and(|(_, exited)| !exited);
            match plan_keys(owner, name, running, program) {
                KeysPlan::Send => {
                    match self.terminals.send_to_program(&key, keys.to_vec()) {
                        Ok(()) => self.changed_this_frame = true,
                        Err(e) => self.refuse_program(
                            name,
                            format!("{owner} could not type into {name:?}: {e}"),
                        ),
                    }
                    return;
                }
                KeysPlan::Refuse(error) => {
                    self.refuse_program(name, error);
                    return;
                }
                KeysPlan::Start => {}
            }
        }

        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let rect = self.terminals.last_rect(&key.surface_id());
        let (rows, cols) = match rect {
            Some(rect) if rect.width > 0 && rect.height > 0 => (rect.height, rect.width),
            _ => (rows, cols),
        };
        let cwd = self.ui_dir.clone();
        if let Err(e) = self
            .terminals
            .start_program(&key, program, argv, Some(&cwd), rows, cols)
        {
            self.refuse_program(name, e);
            return;
        }
        self.changed_this_frame = true;
    }

    fn refuse_program(&mut self, name: &str, error: String) {
        self.enqueue_event(
            thurbox::kernel::events::Event::new("command.failed")
                .with("kind", Some("program"))
                .with("subject", Some(name))
                .with("error", Some(error.as_str())),
        );
        self.report(error, Level::Error);
    }
}
