//! User-tunable settings (`~/.config/thurbox/settings.toml`): scalar knobs
//! plus the `[features]` whole-feature switches.
//!
//! Pure data + parsing, per the `session/` architecture rule; the file IO and
//! seeding live in `crate::agent::settings_config`. Only knobs a user
//! plausibly wants to change are exposed — timing/buffer internals stay
//! hardcoded. The loaded value is published process-wide via [`init`] /
//! [`global`] because the consumers span modules that must not know about each
//! other (terminal wiring, layout, storage retention).

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// Settings loaded from `settings.toml`. Every field has a default, so
/// an absent file (the common case) behaves exactly like before the file
/// existed. Unknown keys are tolerated but reported: the loader names every
/// unrecognized key in a startup warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Config-format version, for future migrations. Currently `1`.
    #[serde(default)]
    pub config_version: Option<u32>,
    /// Scrollback lines kept per session terminal (vt100 parser history).
    #[serde(default = "default_scrollback_lines")]
    pub scrollback_lines: usize,
    /// Terminal width (columns) below which only the terminal pane renders.
    #[serde(default = "default_two_panel_min_cols")]
    pub two_panel_min_cols: u16,
    /// Terminal width (columns) at which the optional third column (info /
    /// tasks / file viewer) becomes available.
    #[serde(default = "default_three_panel_min_cols")]
    pub three_panel_min_cols: u16,
    /// Days of audit-log history kept (pruned on startup).
    #[serde(default = "default_audit_retention_days")]
    pub audit_retention_days: u64,
    /// Per-feature on/off switches (`[features]` table). Absent table = all
    /// enabled.
    #[serde(default)]
    pub features: FeatureFlags,
    /// Desktop-notification settings (`[notifications]` table). Absent table =
    /// defaults (fire on `Attention`, skip the currently-focused session, 5s
    /// per-session dedup).
    #[serde(default)]
    pub notifications: NotificationSettings,
}

/// Whole-feature switches (`[features]` in settings.toml). Each flag hides the
/// feature's UI and blocks its keybinding; disabling `automations` also stops
/// the TUI firing schedules and arming the tmux heartbeat. Data and
/// `thurbox-cli` surfaces stay fully functional regardless, so re-enabling a
/// flag is lossless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureFlags {
    /// Tasks panel (F5/Ctrl+W) and task search results.
    #[serde(default = "default_true")]
    pub tasks: bool,
    /// Automations pane, Ctrl+P editor, TUI schedule firing, heartbeat arming.
    #[serde(default = "default_true")]
    pub automations: bool,
    /// File viewer column (F3) and file search results.
    #[serde(default = "default_true")]
    pub file_viewer: bool,
    /// Global search strip (Ctrl+/).
    #[serde(default = "default_true")]
    pub global_search: bool,
    /// Info panel column (F2).
    #[serde(default = "default_true")]
    pub info_panel: bool,
    /// Per-session shell pane toggle (Ctrl+T).
    #[serde(default = "default_true")]
    pub shell_pane: bool,
    /// Mouse support: terminal mouse capture plus all click/scroll/hover
    /// handling (click-to-select, drag selection, Ctrl+Click URLs,
    /// scrollbars). Disable to keep the terminal's native mouse behavior
    /// (e.g. its own text selection).
    #[serde(default = "default_true")]
    pub mouse: bool,
    /// OS desktop notifications when a session needs the user's attention.
    /// Disabled = no notifications fire and the dispatcher thread never
    /// starts (zero overhead). Linux gets click-to-focus; macOS shows a
    /// passive banner only (the modern API requires a signed app bundle).
    #[serde(default = "default_true")]
    pub notifications: bool,
    /// Soft-delete sessions in the TUI (Ctrl+D): mark the DB row deleted and
    /// offer Ctrl+Z undo, leaving the tmux window + worktrees intact. Disabled
    /// = the TUI **hard-deletes** (kills the tmux window, removes worktrees +
    /// symlink workspace, disables send automations) after a confirmation
    /// prompt. `thurbox-cli session delete` is unaffected (always soft unless
    /// `--force`).
    #[serde(default = "default_true")]
    pub soft_delete: bool,
    /// Version-update check: the TUI header "update available" badge and the
    /// `thurbox-cli version --check` command. **Off by default** — unlike the
    /// other flags, this one is opt-in because it makes a network call to
    /// GitHub. Enable it to learn when a newer release is available.
    #[serde(default = "default_false")]
    pub version_check: bool,
    /// Silent auto-update: the TUI silently downloads, verifies, and replaces
    /// the installed binaries on startup when a newer release exists, and the
    /// `thurbox-cli update` command does the same on demand. Also keeps installed
    /// extensions fresh — once the binary upgrades, the self-heal pass (TUI
    /// startup + headless tick) refreshes any extension that is now stale instead
    /// of merely nudging. **Off by default** — opt-in because it makes a network
    /// call and replaces files on disk. The new version applies on the next launch.
    #[serde(default = "default_false")]
    pub auto_update: bool,
}

/// Which OS-notification delivery backend to use (`[notifications] backend`).
/// `Auto` (the default) detects the right one at startup: dbus on a normal
/// Linux desktop, a Windows toast (via `powershell.exe`) under WSL when no
/// dbus notification daemon is reachable, the native banner on macOS. The
/// other variants force a specific path, and `Off` disables delivery entirely
/// without touching the `[features] notifications` switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NotificationBackend {
    /// Detect the best backend for the host at startup.
    #[default]
    Auto,
    /// Force the freedesktop dbus path (`org.freedesktop.Notifications`).
    Dbus,
    /// Force the WSL → Windows toast path (`powershell.exe`).
    Windows,
    /// Disable delivery (the dispatcher still starts but drops every
    /// notification — a soft off-switch distinct from `[features]`).
    Off,
}

/// Knobs for the OS notification feature (`[notifications]` table). All
/// fields have defaults so an empty / absent table behaves like the seeded
/// configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationSettings {
    /// Also notify when a session **finishes** (`Working → Done`, reported by an
    /// agent hook), not just when it becomes `Blocked` (which always fires).
    /// Off by default. (The field name is historical — it now governs the Done
    /// edge.)
    #[serde(default)]
    pub also_on_waiting: bool,
    /// Skip notifications for the session currently in focus (you're already
    /// looking at it). Defaults on; flip off if you run thurbox in a
    /// background window and want every transition surfaced.
    #[serde(default = "default_true")]
    pub suppress_for_active: bool,
    /// Play the OS default notification sound.
    #[serde(default = "default_true")]
    pub sound: bool,
    /// Per-session floor between two notifications, in seconds. Prevents an
    /// agent that flips Attention → Busy → Attention from spamming.
    #[serde(default = "default_notification_min_interval_secs")]
    pub min_interval_secs: u64,
    /// Delivery backend. `auto` (default) detects dbus vs. Windows-toast vs.
    /// macOS at startup; `dbus`/`windows` force one; `off` disables delivery.
    #[serde(default)]
    pub backend: NotificationBackend,
}

fn default_notification_min_interval_secs() -> u64 {
    5
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            also_on_waiting: false,
            suppress_for_active: true,
            sound: true,
            min_interval_secs: default_notification_min_interval_secs(),
            backend: NotificationBackend::Auto,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_false() -> bool {
    false
}

impl Default for FeatureFlags {
    fn default() -> Self {
        Self {
            tasks: true,
            automations: true,
            file_viewer: true,
            global_search: true,
            info_panel: true,
            shell_pane: true,
            mouse: true,
            notifications: true,
            soft_delete: true,
            version_check: false,
            auto_update: false,
        }
    }
}

fn default_scrollback_lines() -> usize {
    1000
}
fn default_two_panel_min_cols() -> u16 {
    80
}
fn default_three_panel_min_cols() -> u16 {
    120
}
fn default_audit_retention_days() -> u64 {
    90
}

impl Settings {
    /// Whether any **restart-only** setting differs between `self` and `other`.
    ///
    /// These are the values read once at startup (the scalars, every
    /// `[notifications]` knob, and the feature flags whose effect is wired at
    /// launch — `automations`, `mouse`, `notifications`, `version_check`). The
    /// remaining feature flags gate UI panels read from `App.features` every
    /// frame, so they apply live and are intentionally excluded here. Drives the
    /// "some changes apply after restart" hint shown by the settings panel and
    /// the live-reload toast.
    pub fn restart_only_differs(&self, other: &Settings) -> bool {
        self.scrollback_lines != other.scrollback_lines
            || self.two_panel_min_cols != other.two_panel_min_cols
            || self.three_panel_min_cols != other.three_panel_min_cols
            || self.audit_retention_days != other.audit_retention_days
            || self.notifications != other.notifications
            || self.features.automations != other.features.automations
            || self.features.mouse != other.features.mouse
            || self.features.notifications != other.features.notifications
            || self.features.version_check != other.features.version_check
            || self.features.auto_update != other.features.auto_update
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            config_version: None,
            scrollback_lines: default_scrollback_lines(),
            two_panel_min_cols: default_two_panel_min_cols(),
            three_panel_min_cols: default_three_panel_min_cols(),
            audit_retention_days: default_audit_retention_days(),
            features: FeatureFlags::default(),
            notifications: NotificationSettings::default(),
        }
    }
}

static GLOBAL: OnceLock<Settings> = OnceLock::new();

/// Publish the loaded settings process-wide. Call once at startup, before the
/// first [`global`] read; later calls are ignored (first writer wins).
pub fn init(settings: Settings) {
    let _ = GLOBAL.set(settings);
}

/// The process-wide settings; defaults when [`init`] was never called (tests,
/// or library use outside the binaries).
pub fn global() -> &'static Settings {
    GLOBAL.get_or_init(Settings::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_document_yields_defaults() {
        let s: Settings = toml::from_str("").unwrap();
        assert_eq!(s, Settings::default());
        assert_eq!(s.scrollback_lines, 1000);
        assert_eq!(s.two_panel_min_cols, 80);
        assert_eq!(s.three_panel_min_cols, 120);
        assert_eq!(s.audit_retention_days, 90);
    }

    #[test]
    fn partial_override_keeps_other_defaults() {
        let s: Settings = toml::from_str("scrollback_lines = 5000").unwrap();
        assert_eq!(s.scrollback_lines, 5000);
        assert_eq!(s.audit_retention_days, 90);
    }

    #[test]
    fn type_mismatch_is_rejected() {
        let err = toml::from_str::<Settings>("scrollback_lines = \"many\"").unwrap_err();
        assert!(err.to_string().contains("scrollback_lines"));
    }

    #[test]
    fn absent_features_table_enables_everything() {
        let s: Settings = toml::from_str("").unwrap();
        assert_eq!(s.features, FeatureFlags::default());
        assert!(s.features.tasks && s.features.automations);
    }

    #[test]
    fn empty_features_table_enables_everything() {
        let s: Settings = toml::from_str("[features]").unwrap();
        assert_eq!(s.features, FeatureFlags::default());
    }

    #[test]
    fn partial_features_override_keeps_other_flags_enabled() {
        let s: Settings = toml::from_str("[features]\ntasks = false").unwrap();
        assert!(!s.features.tasks);
        assert!(s.features.automations);
        assert!(s.features.file_viewer);
        assert!(s.features.global_search);
        assert!(s.features.info_panel);
        assert!(s.features.shell_pane);
        assert!(s.features.mouse);
    }

    #[test]
    fn mouse_feature_flag_parses() {
        let s: Settings = toml::from_str("[features]\nmouse = false").unwrap();
        assert!(!s.features.mouse);
        assert!(s.features.tasks, "untouched flags stay enabled");
    }

    #[test]
    fn notifications_feature_flag_parses() {
        let s: Settings = toml::from_str("[features]\nnotifications = false").unwrap();
        assert!(!s.features.notifications);
        assert!(s.features.tasks, "untouched flags stay enabled");
    }

    #[test]
    fn soft_delete_feature_flag_defaults_true_and_parses() {
        assert!(FeatureFlags::default().soft_delete);
        let s: Settings = toml::from_str("[features]\nsoft_delete = false").unwrap();
        assert!(!s.features.soft_delete);
        assert!(s.features.tasks, "untouched flags stay enabled");
    }

    #[test]
    fn notifications_table_defaults() {
        let s: Settings = toml::from_str("").unwrap();
        assert_eq!(s.notifications, NotificationSettings::default());
        assert!(!s.notifications.also_on_waiting);
        assert!(s.notifications.suppress_for_active);
        assert!(s.notifications.sound);
        assert_eq!(s.notifications.min_interval_secs, 5);
        assert_eq!(s.notifications.backend, NotificationBackend::Auto);
    }

    #[test]
    fn notifications_backend_parses_each_variant() {
        for (raw, want) in [
            ("auto", NotificationBackend::Auto),
            ("dbus", NotificationBackend::Dbus),
            ("windows", NotificationBackend::Windows),
            ("off", NotificationBackend::Off),
        ] {
            let s: Settings =
                toml::from_str(&format!("[notifications]\nbackend = \"{raw}\"\n")).unwrap();
            assert_eq!(s.notifications.backend, want, "backend = {raw}");
        }
    }

    #[test]
    fn notifications_backend_rejects_unknown_value() {
        let err =
            toml::from_str::<Settings>("[notifications]\nbackend = \"telepathy\"").unwrap_err();
        assert!(err.to_string().contains("backend") || err.to_string().contains("variant"));
    }

    #[test]
    fn notifications_table_partial_override() {
        let s: Settings =
            toml::from_str("[notifications]\nalso_on_waiting = true\nmin_interval_secs = 30\n")
                .unwrap();
        assert!(s.notifications.also_on_waiting);
        assert_eq!(s.notifications.min_interval_secs, 30);
        // Untouched fields stay on defaults.
        assert!(s.notifications.suppress_for_active);
        assert!(s.notifications.sound);
    }

    #[test]
    fn notifications_type_mismatch_is_rejected() {
        let err = toml::from_str::<Settings>("[notifications]\nsound = \"loud\"").unwrap_err();
        assert!(err.to_string().contains("sound"));
    }

    #[test]
    fn version_check_flag_defaults_off_and_parses() {
        // Unlike the other flags, version_check is opt-in (network call).
        let s: Settings = toml::from_str("[features]").unwrap();
        assert!(!s.features.version_check, "version_check defaults off");
        assert!(s.features.tasks, "other flags still default on");

        let s: Settings = toml::from_str("[features]\nversion_check = true").unwrap();
        assert!(s.features.version_check);
        assert!(s.features.mouse, "untouched flags stay at their default");
    }

    #[test]
    fn auto_update_flag_defaults_off_and_parses() {
        // Like version_check, auto_update is opt-in (network call + writes to disk).
        let s: Settings = toml::from_str("[features]").unwrap();
        assert!(!s.features.auto_update, "auto_update defaults off");
        assert!(s.features.tasks, "other flags still default on");

        let s: Settings = toml::from_str("[features]\nauto_update = true").unwrap();
        assert!(s.features.auto_update);
        assert!(s.features.mouse, "untouched flags stay at their default");
    }

    #[test]
    fn feature_flag_type_mismatch_is_rejected() {
        let err = toml::from_str::<Settings>("[features]\ntasks = \"no\"").unwrap_err();
        assert!(err.to_string().contains("tasks"));
    }

    #[test]
    fn restart_only_differs_ignores_live_flags_but_catches_restart_ones() {
        let base = Settings::default();

        // A live UI-panel flag is not a restart-only difference.
        let mut live = base.clone();
        live.features.tasks = !live.features.tasks;
        assert!(!base.restart_only_differs(&live));

        // A restart-only feature flag, a scalar, and a notification knob all are.
        let mut mouse = base.clone();
        mouse.features.mouse = !mouse.features.mouse;
        assert!(base.restart_only_differs(&mouse));

        let mut scrollback = base.clone();
        scrollback.scrollback_lines += 1;
        assert!(base.restart_only_differs(&scrollback));

        let mut notif = base.clone();
        notif.notifications.sound = !notif.notifications.sound;
        assert!(base.restart_only_differs(&notif));

        // Identical settings never differ.
        assert!(!base.restart_only_differs(&base.clone()));
    }

    #[test]
    fn every_feature_flag_is_classified_restart_or_live() {
        // Destructuring WITHOUT `..` makes a newly-added feature flag fail to
        // compile here until it is explicitly classified below — the safety net
        // that stops a startup-wired flag from defaulting to "applies live" by
        // omission in `restart_only_differs` (which would wrongly toast "applied
        // live"). Each binding is consumed by `cases`, so none goes unused.
        let FeatureFlags {
            tasks,
            automations,
            file_viewer,
            global_search,
            info_panel,
            shell_pane,
            mouse,
            notifications,
            soft_delete,
            version_check,
            auto_update,
        } = FeatureFlags::default();
        // Consume every binding so the no-`..` destructure above stays a hard
        // compile-time guard (an unused binding would otherwise be the only
        // warning, not an error).
        let _ = (
            tasks,
            automations,
            file_viewer,
            global_search,
            info_panel,
            shell_pane,
            mouse,
            notifications,
            soft_delete,
            version_check,
            auto_update,
        );

        // `live` flags gate UI panels read from `App.features` every frame, so
        // flipping one is NOT a restart-only difference; the rest are read once
        // at startup and MUST register as one.
        let live: [fn(&mut FeatureFlags); 6] = [
            |f| f.tasks = !f.tasks,
            |f| f.file_viewer = !f.file_viewer,
            |f| f.global_search = !f.global_search,
            |f| f.info_panel = !f.info_panel,
            |f| f.shell_pane = !f.shell_pane,
            |f| f.soft_delete = !f.soft_delete,
        ];
        let restart: [fn(&mut FeatureFlags); 5] = [
            |f| f.automations = !f.automations,
            |f| f.mouse = !f.mouse,
            |f| f.notifications = !f.notifications,
            |f| f.version_check = !f.version_check,
            |f| f.auto_update = !f.auto_update,
        ];

        let base = Settings::default();
        let check = |flip: fn(&mut FeatureFlags), expect_restart: bool| {
            let mut other = base.clone();
            flip(&mut other.features);
            assert_eq!(
                base.restart_only_differs(&other),
                expect_restart,
                "a feature flag's restart classification disagrees with restart_only_differs",
            );
        };
        live.into_iter().for_each(|flip| check(flip, false));
        restart.into_iter().for_each(|flip| check(flip, true));
    }

    #[test]
    fn global_defaults_when_uninitialized() {
        // Note: other tests may have init()'d already; both paths are default.
        assert_eq!(global().two_panel_min_cols, 80);
    }
}
