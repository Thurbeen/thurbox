//! Delivering events to the plugins that subscribed to them.
//!
//! One dispatch point per iteration, after the worker stores and the command
//! bus have published and before the paint — so a handler sees the iteration's
//! fresh state and its `state`/`store` writes land in the frame about to be
//! painted, with no extra frame. It is a `VecDeque::is_empty` check on every
//! iteration with nothing queued, which is what keeps the settle test true
//! (`frame-cost`): dispatch never marks the frame dirty itself. A handler that
//! writes state bumps the state version, and one that enqueues a command goes
//! through `dispatch_tracked` — both already mark it, exactly as a key handler's
//! writes do.
//!
//! The kernel's own events are derived here from the signals the loop already
//! has — the snapshot's version, the focus ring, the command bus — never raised
//! by the code that mutates them. See `kernel::events`.

use std::collections::VecDeque;

use thurbox::kernel::events::{Deriver, Event, Field, MAX_DEPTH};
use thurbox::kernel::terminal::{ProgramKey, ProgramTransition};

use super::*;

/// Why the interface was rebuilt, as `interface.reloaded` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReloadReason {
    /// The first build at startup. Not an event: nothing existed before it to
    /// have missed anything.
    Boot,
    /// `F10`, or the palette's reload entry.
    Key,
    /// The directory watcher, or an edit this process made to the directory.
    Watch,
    /// A switch or a trust change in the settings modal's Interface tab.
    Settings,
}

impl ReloadReason {
    fn as_str(self) -> &'static str {
        match self {
            ReloadReason::Boot => "boot",
            ReloadReason::Key => "f10",
            ReloadReason::Watch => "watch",
            ReloadReason::Settings => "settings",
        }
    }
}

/// Everything the loop holds about events between iterations.
pub(crate) struct Events {
    queue: VecDeque<Event>,
    deriver: Deriver,
    /// The depth of the event whose handlers are running, so an emit from one
    /// is queued one generation deeper. `None` outside a dispatch — a root
    /// event and a handler's emit must not be told apart by whether the queue
    /// happened to be empty, which is what an integer with a zero for "idle"
    /// did: an emit from a depth-zero handler read as a root and cascaded
    /// unbounded.
    current: Option<u8>,
    /// `(plugin, event)` pairs already reported, so a handler that throws on
    /// every `session.status` is reported once per event rather than per
    /// delivery. Cleared on reload, since the plugin was rebuilt.
    reported: std::collections::HashSet<(String, String)>,
    /// The program panes seen RUNNING on the last iteration, by surface id.
    ///
    /// The memo `program.exited` is derived from: an entry here that now reports
    /// `has_exited` is the transition, and firing on the state instead would
    /// re-fire every iteration until the slot was reaped.
    programs: std::collections::HashSet<String>,
    /// Whether the cascade bound has been reported this dispatch.
    cascade_reported: bool,
    /// The selection and the focused pane as last observed, so a change is an
    /// event. `None` until first observed: the first frame's focus is not a
    /// change from anything.
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
    /// Derive what changed, and hand every queued event to its subscribers.
    ///
    /// Takes the terminal because a handler's commands are applied *inside* the
    /// dispatch — that is what lets an `emit` from a handler be delivered in the
    /// same call, and what makes the cascade bound a bound rather than a
    /// per-iteration trickle.
    pub(crate) fn dispatch_events(&mut self, terminal: &mut DefaultTerminal) {
        self.derive_kernel_events();
        if self.events.queue.is_empty() {
            return;
        }
        // Handlers read the published tables, so they are made current once per
        // batch — the same rule an input batch follows.
        self.republish();
        self.events.cascade_reported = false;
        while let Some(event) = self.events.queue.pop_front() {
            if event.depth > MAX_DEPTH {
                if !self.events.cascade_reported {
                    self.events.cascade_reported = true;
                    self.report(
                        format!(
                            "{}: dropped — events cascaded more than {MAX_DEPTH} deep",
                            event.name
                        ),
                        Level::Error,
                    );
                }
                continue;
            }
            self.events.current = Some(event.depth);
            for failure in self.host.dispatch_event(&event) {
                self.report_event_failure(failure, &event.name);
            }
            // What the handlers asked for, applied now rather than next
            // iteration, so an emit lands in this dispatch.
            self.apply_commands(terminal);
        }
        self.events.current = None;
    }

    /// Queue an event for the next dispatch.
    ///
    /// Depth is stamped from the dispatch in progress: an event queued while no
    /// handler runs is a root, and one queued by a handler is a generation deeper
    /// than the event it was handling.
    pub(crate) fn enqueue_event(&mut self, mut event: Event) {
        event.depth = self
            .events
            .current
            .map_or(0, |depth| depth.saturating_add(1));
        self.events.queue.push_back(event);
    }

    /// The kernel's own events, from the signals the loop already tracks.
    fn derive_kernel_events(&mut self) {
        // Focus: the selected session and the focused pane, each compared to
        // what it was. Both are two `Option<String>` compares per iteration.
        let selected = self.host.shared_string("selected");
        let pane = self
            .host
            .focusable()
            .get(self.focus)
            .and_then(|index| self.host.plugins.get(*index))
            .map(|plugin| plugin.name.clone());
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

        // Programs: which of a plugin's own panes have ended since the last
        // look. The kernel has always known — `has_exited` is an atomic the
        // reader loop sets — but nothing published it, so a pane could neither
        // say that its program had finished nor move on from it.
        //
        // Derived here rather than fired where the process is reaped because
        // nothing reaps it: `start_program` replaces a finished slot lazily, the
        // next time the plugin asks. Two readings, because the live walk alone
        // cannot see every ending — see [`program_endings`].
        let ended = program_endings(
            &mut self.events.programs,
            self.terminals.program_liveness(),
            self.terminals.take_program_transitions(),
        );
        for (key, program) in ended {
            self.enqueue_event(
                Event::new("program.exited")
                    .to(key.plugin)
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

    /// The events a finished command owes: `command.done`, and for the four
    /// lifecycle operations the matching `session.post_*`.
    ///
    /// Named as `hooks.toml` names them so a user learns one vocabulary; the
    /// shell hook already ran inside the operation on the worker, so "shell
    /// post-hook, then Lua post-event" holds without the kernel knowing hooks
    /// exist.
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
            "create" | "fork" => {
                // The command named no session; the row is found by the name it
                // was given, newest first, now that the snapshot has been
                // re-read.
                let row = tracked.name.as_deref().and_then(|name| {
                    self.snapshots
                        .current()
                        .sessions
                        .iter()
                        .rev()
                        .find(|row| row.name == name)
                });
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
            "restart" | "restore" => {
                let name = if tracked.kind == "restart" {
                    "session.post_restart"
                } else {
                    "session.post_restore"
                };
                let mut event = Event::new(name)
                    .with("session", Some(tracked.session.as_str()))
                    .with("name", tracked.label.as_deref());
                if let Some(row) = self.snapshots.current().session(&tracked.session) {
                    event = event
                        .with("agent", Some(row.agent.as_str()))
                        .with("repo", row.repo.as_deref())
                        .with("cwd", row.cwd.as_ref().map(|cwd| cwd.display().to_string()))
                        .with("branch", row.branch.as_deref());
                }
                event
            }
            _ => return,
        };
        self.enqueue_event(event);
    }

    pub(crate) fn note_command_failed(&mut self, tracked: &TrackedCommand, error: &str) {
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

    /// A reload replaces every plugin: what was queued for the old ones is
    /// dropped, the deriver seeds again from the next snapshot, and the rebuilt
    /// plugins hear `interface.reloaded` first.
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

    fn report_event_failure(&mut self, failure: PluginError, event: &str) {
        let key = (failure.plugin.clone(), event.to_string());
        if !self.events.reported.insert(key) {
            return;
        }
        tracing::warn!("plugin event handler failed: {failure}");
        self.report(failure.to_string(), Level::Error);
    }
}

/// Which program panes ended since the last look, and what to remember for the
/// next one.
///
/// `seen` is the memo of surfaces whose death would be news — panes known
/// running, and panes this run spawned. It is updated in place.
///
/// The live walk (`liveness`) is what is held right now: a key that reports
/// `has_exited` and is in the memo is the ordinary transition. It cannot see
/// two things, and `transitions` — the kernel's ordered log of what happened to
/// the slots since the last drain — carries both:
///
/// - **`Replaced`** — an ending whose slot is already gone. The loop applies
///   commands before it derives, so a plugin asking to restart on the frame
///   after its program died hands this walk a *live* pane under the same key.
/// - **`Started`** — a pane spawned here. Without it the memo is the only
///   evidence a pane ever ran, and a program that dies inside one iteration —
///   a bad file, a missing binary inside a wrapper, a program that prints usage
///   and exits — was never in it, so its ending was dropped by the very gate
///   that exists to ignore *adopted* corpses from a previous run.
///
/// Walked in order, and that is what keeps one death one event: a `Replaced`
/// consumes the memo entry it reports, so the same death cannot be reported
/// again by a restart that follows it, and a `Started` after it vouches only
/// for the pane it spawned. Read as two unordered sets instead, a restart on a
/// frame *after* the ending was announced re-seeds the memo and the
/// already-told death is told a second time.
///
/// A key can still legitimately appear twice: a pane that died, was restarted,
/// and died again inside one iteration is two programs ending, and a plugin
/// that restarts on the event owes itself both.
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
        ProgramKey::new("plugins/90_files.lua", name)
    }

    fn seen(keys: &[&ProgramKey]) -> HashSet<String> {
        keys.iter().map(|k| k.surface_id()).collect()
    }

    fn names(ended: &[(ProgramKey, String)]) -> Vec<String> {
        ended.iter().map(|(k, _)| k.name.clone()).collect()
    }

    /// The ordinary transition: a pane seen running is now finished.
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

    /// And it ends once, not once per frame.
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

    /// A corpse found on startup is not an ending. This is the rule the seeding
    /// below has to leave standing: a window adopted from a previous run of the
    /// interface holds a program that stopped while nothing was watching, and
    /// announcing it would tell a plugin its editor "just closed" at boot.
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

    /// A program that starts and dies inside one iteration still ends.
    ///
    /// The loop applies commands, talks to tmux and serves its workers before it
    /// derives; a program with a bad argument is gone by then and was never seen
    /// running. Without the spawn being recorded, the gate above — written for
    /// adopted corpses — drops it, and the pane paints a dead grid for ever with
    /// nothing to say why.
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

    /// An ending whose slot was already replaced is still an ending.
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

    /// Two deaths in one iteration are two events, not one and not three.
    ///
    /// A pane died, was restarted, and the replacement died too before anything
    /// looked. Both endings are real, and a plugin that restarts its program on
    /// the event owes itself both — but a single death must never arrive twice,
    /// which is what draining against the old memo is for.
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

    /// Each ending is addressed to the plugin whose pane it was.
    #[test]
    fn endings_carry_the_plugin_that_owns_them() {
        let mine = ProgramKey::new("plugins/90_files.lua", "editor");
        let theirs = ProgramKey::new("plugins/50_notes.lua", "editor");
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
                .map(|(k, _)| k.plugin.as_str())
                .collect::<Vec<_>>(),
            ["plugins/90_files.lua"],
            "two plugins' panes share a name, and the wrong one was told"
        );
    }

    /// A death announced once is not announced again when the pane is restarted
    /// on a later frame.
    ///
    /// The restart records the slot it overwrites, and the spawn beside it puts
    /// the surface back in the memo — so a drain that reads the memo *after* the
    /// spawn finds the same death vouched for a second time. The plugin that
    /// restarts on the event then restarts twice for one process.
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
