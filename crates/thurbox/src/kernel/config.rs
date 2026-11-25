use std::path::PathBuf;
use std::time::SystemTime;

use crate::session::settings::Settings;

const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1000);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reloaded {
    Live,
    NeedsRestart,
    Failed(String),
}

pub struct Config {
    in_force: Settings,
    on_disk: Settings,
    path: Option<PathBuf>,
    stamp: Option<SystemTime>,
    last_poll: Option<std::time::Instant>,
}

impl Config {
    pub fn load() -> (Self, Vec<String>) {
        let (settings, warnings) = crate::config::settings::load_or_seed_with_warnings();
        let path = crate::config::settings::settings_config_path();
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

    pub fn detached(settings: Settings) -> Self {
        Self {
            on_disk: settings.clone(),
            in_force: settings,
            path: None,
            stamp: None,
            last_poll: None,
        }
    }

    pub fn in_force(&self) -> &Settings {
        &self.in_force
    }

    pub fn on_disk(&self) -> &Settings {
        &self.on_disk
    }

    pub fn features(&self) -> &crate::session::settings::FeatureFlags {
        &self.in_force.features
    }

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

        let (fresh, warnings) = crate::config::settings::load_or_seed_with_warnings();
        // A key 3.0 no longer reads was reported at startup; the file still
        // loads, so a later edit beside it applies.
        if let Some(first) = warnings
            .iter()
            .find(|warning| !crate::config::settings::is_retired_notice(warning))
        {
            // The loader falls back to defaults per bad key rather than failing,
            // so this is a report, not a refusal.
            return Some(Reloaded::Failed(first.clone()));
        }
        Some(self.adopt(fresh))
    }

    pub fn adopt(&mut self, fresh: Settings) -> Reloaded {
        let needs_restart = self.in_force.restart_only_differs(&fresh);
        // `fresh` is what the file holds — it was either just read from it or is
        // about to be written to it — so it is the whole document that is kept,
        // not only the half taken into force.
        self.in_force = self.in_force.with_live_from(&fresh);
        self.on_disk = fresh;
        if needs_restart {
            Reloaded::NeedsRestart
        } else {
            Reloaded::Live
        }
    }

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
        edited.features.shell_pane = false;
        assert_eq!(config.adopt(edited), Reloaded::Live);
        assert!(!config.features().shell_pane);
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
        // Flipping every live flag must change every one of them.
        let mut config = Config::detached(settings());
        let mut edited = settings();
        edited.features.shell_pane = false;
        edited.features.perf_hud = false;
        assert_eq!(config.adopt(edited), Reloaded::Live);
        let live = config.features();
        for (name, value) in [("shell_pane", live.shell_pane), ("perf_hud", live.perf_hud)] {
            assert!(!value, "{name} was not taken into force");
        }
    }

    #[test]
    fn the_split_matches_what_v1_calls_restart_only() {
        // `adopt` keeps a value when `restart_only_differs` names it, so
        // flipping one must always be reported *and* ignored.
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
    fn a_clipboard_edit_is_taken_into_force() {
        // Issue #1638: reported `Live`, and the booted provider stayed in force.
        let mut config = Config::detached(settings());
        let mut edited = settings();
        edited.clipboard.provider = crate::session::settings::ClipboardProvider::Osc52;
        assert_eq!(config.adopt(edited), Reloaded::Live);
        assert_eq!(
            config.in_force().clipboard.provider,
            crate::session::settings::ClipboardProvider::Osc52
        );
    }

    #[test]
    fn a_config_with_no_file_never_polls() {
        let mut config = Config::detached(settings());
        assert_eq!(config.poll(), None);
    }
}
