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
