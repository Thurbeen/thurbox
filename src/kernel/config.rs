//! The user's settings, and what happens when they change.
//!
//! `settings.toml` is one file with two kinds of value in it, and the difference
//! is a fact about the code rather than a preference:
//!
//! - **live** — read every frame, so editing one takes effect at once (whether a
//!   surface exists at all, whether deleting is soft);
//! - **restart-only** — consumed once, at startup, by something that cannot be
//!   un-done mid-run: the mouse capture escape the terminal was already sent, the
//!   notifier thread, the scrollback a parser was built with, the audit prune.
//!
//! v1 draws the same line (`Settings::restart_only_differs`) and this holds it:
//! the live half lives here and is replaced on reload, the restart-only half stays
//! as it was published at startup and is read through
//! [`crate::session::settings::global`]. Asking this for a restart-only value and
//! asking the global for a live one would both be wrong, so
//! [`Config::in_force`] is the one answer to "what is the user running with" —
//! and it is what gets published to plugins.
//!
//! The reason this module exists at all: `settings::global` is a write-once
//! `OnceLock` shared with `thurbox-cli` and v1. That is what makes those callers
//! safe, so it must not become mutable — a live setting needs somewhere else to
//! live.

use std::path::PathBuf;
use std::time::SystemTime;

use crate::session::settings::Settings;

/// How often the file is stat'ed for an outside edit. v1 polls its configs at the
/// same cadence; a settings file is not edited often enough to want faster.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1000);

/// What a reload turned out to be, for the caller to report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reloaded {
    /// Only live settings moved: they are already in force, so there is nothing
    /// to tell the user beyond that it happened.
    Live,
    /// Something that cannot take effect until the next launch moved. Reported,
    /// because the alternative is the user believing it is active.
    NeedsRestart,
    /// The file could not be read or parsed. What was in force stays in force.
    Failed(String),
}

/// The settings in force, and the file they came from.
pub struct Config {
    /// What is in force *now*: the restart-only half as published at startup, the
    /// live half as last read from disk.
    in_force: Settings,
    /// What the *file* says, restart-only half included.
    ///
    /// Not the same document as [`in_force`](Self::in_force), and the difference
    /// is the whole reason this field exists: a restart-only change is written to
    /// the file and deliberately *not* taken into force, so anything that seeds an
    /// edit from what is in force silently proposes reverting it. The settings
    /// panel is exactly that — it edits the file — so it is handed this.
    on_disk: Settings,
    path: Option<PathBuf>,
    /// Modification time the file had when it was last read, so an outside edit
    /// can be noticed without re-parsing it every frame.
    stamp: Option<SystemTime>,
    last_poll: Option<std::time::Instant>,
}

impl Config {
    /// Read the file, publish it process-wide, and keep a live copy.
    ///
    /// Publishing must happen before anything reads a restart-only value — and
    /// `Database::open` reads one (it prunes the audit log to
    /// `audit_retention_days`), so this belongs at the very top of `main`. v1
    /// loads it in exactly the same place, for exactly that reason.
    ///
    /// Returns the warnings the loader produced (an unknown key, a value out of
    /// range) so the caller can surface them as startup notices.
    pub fn load() -> (Self, Vec<String>) {
        let (settings, warnings) = crate::agent::settings_config::load_or_seed_with_warnings();
        crate::session::settings::init(settings.clone());
        let path = crate::agent::settings_config::settings_config_path();
        let stamp = path.as_deref().and_then(modified);
        (
            Self {
                on_disk: settings.clone(),
                in_force: settings,
                path,
                stamp,
                last_poll: None,
            },
            warnings,
        )
    }

    /// Build over settings that were never written to a file — the shape a test
    /// wants, and the fallback for a profile with no config directory.
    pub fn detached(settings: Settings) -> Self {
        Self {
            on_disk: settings.clone(),
            in_force: settings,
            path: None,
            stamp: None,
            last_poll: None,
        }
    }

    /// Everything the user is running with, live half and restart-only half.
    pub fn in_force(&self) -> &Settings {
        &self.in_force
    }

    /// The settings as the file has them — what an editor of the file must show
    /// and seed its draft from. See the field's own note for why this is not
    /// [`in_force`](Self::in_force).
    pub fn on_disk(&self) -> &Settings {
        &self.on_disk
    }

    /// The feature switches, with the live ones as currently edited.
    pub fn features(&self) -> &crate::session::settings::FeatureFlags {
        &self.in_force.features
    }

    /// Notice an outside edit and apply its live half.
    ///
    /// Throttled to once a second: this is a `stat`, and the loop runs a
    /// hundred times a second. `None` means nothing changed.
    pub fn poll(&mut self) -> Option<Reloaded> {
        if self
            .last_poll
            .is_some_and(|at| at.elapsed() < POLL_INTERVAL)
        {
            return None;
        }
        self.last_poll = Some(std::time::Instant::now());

        let path = self.path.clone()?;
        let stamp = modified(&path)?;
        if self.stamp == Some(stamp) {
            return None;
        }
        // Stamped before the read, so a file that fails to parse is not re-read
        // on every poll until it is fixed.
        self.stamp = Some(stamp);

        let (fresh, warnings) = crate::agent::settings_config::load_or_seed_with_warnings();
        if let Some(first) = warnings.first() {
            // The loader falls back to defaults per bad key rather than failing,
            // so this is a report, not a refusal.
            return Some(Reloaded::Failed(first.clone()));
        }
        Some(self.adopt(fresh))
    }

    /// Take the live half of `fresh` into force, and say whether anything that
    /// needs a restart moved.
    ///
    /// The restart-only half is deliberately *not* taken: it was consumed at
    /// startup, and pretending otherwise is the misreport this whole split exists
    /// to prevent.
    pub fn adopt(&mut self, fresh: Settings) -> Reloaded {
        let needs_restart = self.in_force.restart_only_differs(&fresh);
        // `fresh` is what the file holds — it was either just read from it or is
        // about to be written to it — so it is the whole document that is kept,
        // not only the half taken into force.
        self.on_disk = fresh.clone();
        let restart_only = self.in_force.clone();
        self.in_force = Settings {
            // Live: the feature switches that only gate whether a surface is
            // drawn, which is decided per frame.
            features: crate::session::settings::FeatureFlags {
                tasks: fresh.features.tasks,
                file_viewer: fresh.features.file_viewer,
                info_panel: fresh.features.info_panel,
                global_search: fresh.features.global_search,
                shell_pane: fresh.features.shell_pane,
                code_review: fresh.features.code_review,
                perf_hud: fresh.features.perf_hud,
                soft_delete: fresh.features.soft_delete,
                // Restart-only: capture is an escape already sent, the notifier
                // is a thread already started (or not), the heartbeat is armed
                // once, and the update work ran at startup.
                mouse: restart_only.features.mouse,
                notifications: restart_only.features.notifications,
                automations: restart_only.features.automations,
                version_check: restart_only.features.version_check,
                auto_update: restart_only.features.auto_update,
            },
            // Restart-only wholesale.
            notifications: restart_only.notifications,
            clipboard: restart_only.clipboard,
            scrollback_lines: restart_only.scrollback_lines,
            two_panel_min_cols: restart_only.two_panel_min_cols,
            three_panel_min_cols: restart_only.three_panel_min_cols,
            audit_retention_days: restart_only.audit_retention_days,
            config_version: fresh.config_version,
        };
        if needs_restart {
            Reloaded::NeedsRestart
        } else {
            Reloaded::Live
        }
    }

    /// Note that *this* process wrote the file, so the write is not reported back
    /// as somebody else's edit. v1's `mark_settings_saved`.
    pub fn mark_saved(&mut self) {
        self.stamp = self.path.as_deref().and_then(modified);
    }
}

fn modified(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings::default()
    }

    #[test]
    fn a_live_flag_is_taken_into_force() {
        let mut config = Config::detached(settings());
        let mut edited = settings();
        edited.features.soft_delete = false;
        assert_eq!(config.adopt(edited), Reloaded::Live);
        assert!(!config.features().soft_delete);
    }

    #[test]
    fn a_restart_only_flag_is_reported_and_not_taken() {
        // Turning mouse capture off mid-run would mean not sending an escape the
        // terminal was already sent, so the value in force stays as it was.
        let mut config = Config::detached(settings());
        let mut edited = settings();
        edited.features.mouse = false;
        assert_eq!(config.adopt(edited), Reloaded::NeedsRestart);
        assert!(
            config.features().mouse,
            "the value in force is still the one this process started with"
        );
    }

    #[test]
    fn a_restart_only_scalar_is_reported_and_not_taken() {
        let mut config = Config::detached(settings());
        let mut edited = settings();
        edited.scrollback_lines += 1000;
        assert_eq!(config.adopt(edited), Reloaded::NeedsRestart);
        assert_eq!(
            config.in_force().scrollback_lines,
            settings().scrollback_lines
        );
    }

    #[test]
    fn a_notification_knob_is_restart_only() {
        // The dispatcher thread is started once with the backend it resolved.
        let mut config = Config::detached(settings());
        let mut edited = settings();
        edited.notifications.sound = !edited.notifications.sound;
        assert_eq!(config.adopt(edited), Reloaded::NeedsRestart);
        assert_eq!(config.in_force().notifications, settings().notifications);
    }

    #[test]
    fn every_live_flag_survives_a_round_trip() {
        // The split is spelled out field by field in `adopt`, so a new flag added
        // to the wrong half is a silent bug. This is the guard: flipping every
        // live flag must change every one of them.
        let mut config = Config::detached(settings());
        let mut edited = settings();
        edited.features.tasks = false;
        edited.features.file_viewer = false;
        edited.features.info_panel = false;
        edited.features.global_search = false;
        edited.features.shell_pane = false;
        edited.features.code_review = false;
        edited.features.perf_hud = false;
        edited.features.soft_delete = false;
        assert_eq!(config.adopt(edited), Reloaded::Live);
        let live = config.features();
        for (name, value) in [
            ("tasks", live.tasks),
            ("file_viewer", live.file_viewer),
            ("info_panel", live.info_panel),
            ("global_search", live.global_search),
            ("shell_pane", live.shell_pane),
            ("code_review", live.code_review),
            ("perf_hud", live.perf_hud),
            ("soft_delete", live.soft_delete),
        ] {
            assert!(!value, "{name} was not taken into force");
        }
    }

    #[test]
    fn the_split_matches_what_v1_calls_restart_only() {
        // Two definitions of the same line would drift. `adopt` keeps a value
        // when `restart_only_differs` names it, so flipping one must always be
        // reported *and* ignored — checked here against the shared predicate.
        let base = settings();
        let mut edited = settings();
        edited.features.mouse = false;
        edited.notifications.min_interval_secs += 1;
        edited.two_panel_min_cols += 10;
        assert!(base.restart_only_differs(&edited));

        let mut config = Config::detached(base.clone());
        assert_eq!(config.adopt(edited), Reloaded::NeedsRestart);
        assert_eq!(config.features().mouse, base.features.mouse);
        assert_eq!(
            config.in_force().notifications.min_interval_secs,
            base.notifications.min_interval_secs
        );
        assert_eq!(
            config.in_force().two_panel_min_cols,
            base.two_panel_min_cols
        );
    }

    #[test]
    fn a_config_with_no_file_never_polls() {
        let mut config = Config::detached(settings());
        assert_eq!(config.poll(), None);
    }
}
