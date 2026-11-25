use std::collections::VecDeque;

use ratatui::DefaultTerminal;

use thurbox::kernel::bands::Level;
use thurbox::kernel::events::{Deriver, Event, Field, MAX_DEPTH};
use thurbox::kernel::host::ModError;
use thurbox::kernel::terminal::{ProgramKey, ProgramTransition};

use crate::{App, TrackedCommand};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReloadReason {
    Boot,
    Key,
    Watch,
    Settings,
    Memory,
}

impl ReloadReason {
    fn as_str(self) -> &'static str {
        match self {
            ReloadReason::Boot => "boot",
            ReloadReason::Key => "f10",
            ReloadReason::Watch => "watch",
            ReloadReason::Settings => "settings",
            ReloadReason::Memory => "memory",
        }
    }
}

pub(crate) struct Events {
    queue: VecDeque<Event>,
    deriver: Deriver,
    current: Option<u8>,
    reported: std::collections::HashSet<(String, String)>,
    programs: std::collections::HashSet<String>,
    cascade_reported: bool,
    focus: Option<(Option<String>, Option<String>)>,
}

impl Events {
    pub(crate) fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            deriver: Deriver::new(),
            current: None,
            reported: std::collections::HashSet::new(),
            cascade_reported: false,
            focus: None,
            programs: std::collections::HashSet::new(),
        }
    }
}

impl App {
    pub(crate) fn dispatch_events(&mut self, terminal: &mut DefaultTerminal) {
        self.derive_kernel_events();
        if self.events.queue.is_empty() {
            return;
        }
        // Handlers read the published tables, so they are made current once per
        // batch — the same rule an input batch follows.
        self.republish();
        self.events.cascade_reported = false;
        self.time_op("event_dispatch", |app| {
            while let Some(event) = app.events.queue.pop_front() {
                if event.depth > MAX_DEPTH {
                    if !app.events.cascade_reported {
                        app.events.cascade_reported = true;
                        app.report(
                            format!(
                                "{}: dropped — events cascaded more than {MAX_DEPTH} deep",
                                event.name
                            ),
                            Level::Error,
                        );
                    }
                    continue;
                }
                app.events.current = Some(event.depth);
                for failure in app.host.dispatch_event(&event) {
                    app.report_event_failure(failure, &event.name);
                }
                // What the handlers asked for, applied now rather than next
                // iteration, so an emit lands in this dispatch.
                app.apply_commands(terminal);
            }
        });
        self.events.current = None;
    }

    pub(crate) fn enqueue_event(&mut self, mut event: Event) {
        event.depth = self
            .events
            .current
            .map_or(0, |depth| depth.saturating_add(1));
        self.events.queue.push_back(event);
    }

    fn derive_kernel_events(&mut self) {
        // Focus: the selected session and the focused pane, each compared to
        // what it was. Both are two `Option<String>` compares per iteration.
        let selected = self.host.shared_string("selected");
        let pane = self
            .host
            .focusable()
            .get(self.focus)
            .and_then(|index| self.host.mods.get(*index))
            .map(|loaded| loaded.name.clone());
        match &self.events.focus {
            None => self.events.focus = Some((selected, pane)),
            Some((last_selected, last_pane)) => {
                let mut fired = Vec::new();
                if *last_selected != selected {
                    fired.push(
                        Event::new("focus.session")
                            .with("from", last_selected.as_deref())
                            .with("to", selected.as_deref()),
                    );
                }
                if *last_pane != pane {
                    fired.push(
                        Event::new("focus.pane")
                            .with("from", last_pane.as_deref())
                            .with("to", pane.as_deref()),
                    );
                }
                if !fired.is_empty() {
                    self.events.focus = Some((selected, pane));
                    for event in fired {
                        self.enqueue_event(event);
                    }
                }
            }
        }

        let ended = program_endings(
            &mut self.events.programs,
            self.terminals.program_liveness(),
            self.terminals.take_program_transitions(),
        );
        for (key, program) in ended {
            self.enqueue_event(
                Event::new("program.exited")
                    .to(key.owner)
                    .with("name", Some(key.name))
                    .with("program", Some(program)),
            );
        }

        // The snapshot: one integer compare while nothing moved.
        let version = self.snapshots.version();
        let derived = self
            .events
            .deriver
            .observe(self.snapshots.current(), version);
        for event in derived {
            self.enqueue_event(event);
        }
    }

    pub(crate) fn note_command_done(&mut self, tracked: &TrackedCommand) {
        self.enqueue_event(
            Event::new("command.done")
                .with("kind", Some(tracked.kind))
                .with(
                    "session",
                    Some(tracked.session.as_str()).filter(|s| !s.is_empty()),
                )
                .with("subject", tracked.label.as_deref()),
        );
        let event = match tracked.kind {
            "create" => {
                // The command named no session; the row is found by the id
                // its Task has in the Fleet the command went to, now that the
                // snapshot has been re-read. Never by its name, which a
                // session of another Fleet can share.
                let row = tracked
                    .created
                    .as_deref()
                    .and_then(|id| self.snapshots.current().session(id));
                let mut event = Event::new("session.post_create")
                    .with("name", tracked.name.as_deref())
                    .with(
                        "parent",
                        Some(tracked.session.as_str()).filter(|s| !s.is_empty()),
                    );
                if let Some(row) = row {
                    event = event
                        .with("session", Some(row.id.as_str()))
                        .with("agent", Some(row.agent.as_str()))
                        .with("repo", row.repo.as_deref())
                        .with("cwd", row.cwd.as_ref().map(|cwd| cwd.display().to_string()))
                        .with("branch", row.branch.as_deref());
                }
                event
            }
            "delete" => Event::new("session.post_delete")
                .with("session", Some(tracked.session.as_str()))
                .with("name", tracked.label.as_deref())
                .with("force", Some(Field::Bool(tracked.force))),
            _ => return,
        };
        self.enqueue_event(event);
    }

    pub(crate) fn note_command_failed(&mut self, tracked: &TrackedCommand, error: &str) {
        if tracked.kind == "start" {
            self.terminals.start_failed(&tracked.session, error);
        }
        self.enqueue_event(
            Event::new("command.failed")
                .with("kind", Some(tracked.kind))
                .with(
                    "session",
                    Some(tracked.session.as_str()).filter(|s| !s.is_empty()),
                )
                .with("subject", tracked.label.as_deref())
                .with("error", Some(error)),
        );
    }

    pub(crate) fn note_reload(&mut self, reason: ReloadReason) {
        self.events.queue.clear();
        self.events.deriver.reset();
        self.events.reported.clear();
        self.events.current = None;
        if reason != ReloadReason::Boot {
            self.enqueue_event(
                Event::new("interface.reloaded").with("reason", Some(reason.as_str())),
            );
        }
    }

    fn report_event_failure(&mut self, failure: ModError, event: &str) {
        let key = (failure.owner.clone(), event.to_string());
        if !self.events.reported.insert(key) {
            return;
        }
        tracing::warn!("mod event handler failed: {failure}");
        self.report(failure.to_string(), Level::Error);
    }
}

fn program_endings(
    seen: &mut std::collections::HashSet<String>,
    liveness: Vec<(ProgramKey, String, bool)>,
    transitions: Vec<ProgramTransition>,
) -> Vec<(ProgramKey, String)> {
    let mut ended = Vec::new();
    for transition in transitions {
        match transition {
            ProgramTransition::Replaced(key, program) => {
                if seen.remove(&key.surface_id()) {
                    ended.push((key, program));
                }
            }
            ProgramTransition::Started(key) => {
                seen.insert(key.surface_id());
            }
        }
    }
    let mut running = std::collections::HashSet::with_capacity(liveness.len());
    for (key, program, exited) in liveness {
        let surface = key.surface_id();
        if exited {
            if seen.contains(&surface) {
                ended.push((key, program));
            }
        } else {
            running.insert(surface);
        }
    }
    *seen = running;
    ended
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;

    fn key(name: &str) -> ProgramKey {
        ProgramKey::new("mods/90_files.lua", name)
    }

    fn seen(keys: &[&ProgramKey]) -> HashSet<String> {
        keys.iter().map(|k| k.surface_id()).collect()
    }

    fn names(ended: &[(ProgramKey, String)]) -> Vec<String> {
        ended.iter().map(|(k, _)| k.name.clone()).collect()
    }

    #[test]
    fn a_pane_that_was_running_and_is_now_finished_has_ended() {
        let editor = key("editor");
        let mut memo = seen(&[&editor]);
        let ended = program_endings(
            &mut memo,
            vec![(editor.clone(), "nvim".into(), true)],
            vec![],
        );
        assert_eq!(names(&ended), ["editor"]);
        assert!(
            memo.is_empty(),
            "a finished pane must leave the memo, or it ends again every frame"
        );
    }

    #[test]
    fn a_pane_that_stays_dead_ends_only_once() {
        let editor = key("editor");
        let mut memo = seen(&[&editor]);
        let liveness = vec![(editor.clone(), "nvim".to_string(), true)];
        program_endings(&mut memo, liveness.clone(), vec![]);
        let again = program_endings(&mut memo, liveness, vec![]);
        assert!(
            again.is_empty(),
            "a pane that is still dead ended again: {:?}",
            names(&again)
        );
    }

    #[test]
    fn a_corpse_adopted_from_a_previous_run_is_not_an_ending() {
        let mut memo = HashSet::new();
        let ended = program_endings(
            &mut memo,
            vec![(key("editor"), "nvim".into(), true)],
            vec![],
        );
        assert!(ended.is_empty(), "{:?}", names(&ended));
    }

    #[test]
    fn a_program_that_dies_before_the_first_look_still_ends() {
        let editor = key("editor");
        let mut memo = HashSet::new();
        let ended = program_endings(
            &mut memo,
            vec![(editor.clone(), "nvim".into(), true)],
            vec![ProgramTransition::Started(editor)],
        );
        assert_eq!(
            names(&ended),
            ["editor"],
            "a program that never lived long enough to be seen running was never \
             announced as finished"
        );
    }

    #[test]
    fn a_restart_does_not_swallow_the_ending_it_replaced() {
        let editor = key("editor");
        let mut memo = seen(&[&editor]);
        let ended = program_endings(
            &mut memo,
            // The restarted pane, alive — what the walk alone would see.
            vec![(editor.clone(), "nvim".into(), false)],
            vec![
                ProgramTransition::Replaced(editor.clone(), "nvim".into()),
                ProgramTransition::Started(editor.clone()),
            ],
        );
        assert_eq!(names(&ended), ["editor"]);
        assert!(
            memo.contains(&editor.surface_id()),
            "the pane that took its place is running and must be watched"
        );
    }

    #[test]
    fn a_pane_that_died_twice_in_one_iteration_ends_twice() {
        let editor = key("editor");
        let mut memo = seen(&[&editor]);
        let ended = program_endings(
            &mut memo,
            vec![(editor.clone(), "nvim".into(), true)],
            vec![
                ProgramTransition::Replaced(editor.clone(), "nvim".into()),
                ProgramTransition::Started(editor),
            ],
        );
        assert_eq!(names(&ended), ["editor", "editor"]);
    }

    #[test]
    fn endings_carry_the_mod_that_owns_them() {
        let mine = ProgramKey::new("mods/90_files.lua", "editor");
        let theirs = ProgramKey::new("mods/50_notes.lua", "editor");
        let mut memo = seen(&[&mine, &theirs]);
        let ended = program_endings(
            &mut memo,
            vec![
                (mine.clone(), "nvim".into(), true),
                (theirs.clone(), "helix".into(), false),
            ],
            vec![],
        );
        assert_eq!(
            ended
                .iter()
                .map(|(k, _)| k.owner.as_str())
                .collect::<Vec<_>>(),
            ["mods/90_files.lua"],
            "two mods' panes share a name, and the wrong one was told"
        );
    }

    #[test]
    fn an_ending_already_announced_is_not_announced_again_by_the_restart() {
        let editor = key("editor");
        let mut memo = seen(&[&editor]);
        let first = program_endings(
            &mut memo,
            vec![(editor.clone(), "nvim".into(), true)],
            vec![],
        );
        assert_eq!(names(&first), ["editor"], "the death itself must be told");
        let second = program_endings(
            &mut memo,
            // The restart, alive.
            vec![(editor.clone(), "nvim".into(), false)],
            vec![
                ProgramTransition::Replaced(editor.clone(), "nvim".into()),
                ProgramTransition::Started(editor),
            ],
        );
        assert!(
            second.is_empty(),
            "one process died and ended twice: {:?}",
            names(&second)
        );
    }
}
