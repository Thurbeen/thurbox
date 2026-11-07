//! User-tunable settings (`~/.config/thurbox/settings.toml`): scalar knobs
//! plus the `[features]` whole-feature switches.
//!
//! Pure data + parsing, per the `session/` architecture rule; the file IO and
//! seeding live in `crate::config::settings`. Only knobs a user
//! plausibly wants to change are exposed — timing/buffer internals stay
//! hardcoded. There is no process-wide copy: each binary loads the file once
//! and hands the value to what it builds (`session` holds no state, #1477).

use serde::{Deserialize, Serialize};

/// Settings loaded from `settings.toml`. Every field has a default, so
/// an absent file (the common case) behaves exactly like before the file
/// existed. Unknown keys are tolerated but reported: the loader names every
/// unrecognized key in a startup warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Default multiplexer for new local sessions; an explicit create choice wins.
    #[serde(default)]
    pub multiplexer: Option<String>,
    /// Config-format version, for future migrations. Currently `1`.
    #[serde(default)]
    pub config_version: Option<u32>,
    /// Scrollback lines kept per session terminal (vt100 parser history).
    #[serde(default = "default_scrollback_lines")]
    pub scrollback_lines: usize,
    /// How long a session's terminal may stay off screen before the interface
    /// drops its grid and history, in seconds. tmux keeps both anyway, and the
    /// grid is rebuilt from it the next time the session is shown or searched.
    /// A session never shown since the interface started holds none at all.
    /// `0` keeps every session's grid for as long as it runs.
    ///
    /// The memory it saves is per session: a grid is 32 bytes a cell, so a
    /// 200-column session with its default 1,000 lines of history filled holds
    /// about 6 MiB of them.
    #[serde(default = "default_hidden_terminal_secs")]
    pub hidden_terminal_secs: u64,
    /// Terminal width (columns) below which only the terminal pane renders.
    #[serde(default = "default_two_panel_min_cols")]
    pub two_panel_min_cols: u16,
    /// Terminal width (columns) at which the optional third column (info /
    /// tasks / file viewer) becomes available.
    #[serde(default = "default_three_panel_min_cols")]
    pub three_panel_min_cols: u16,
    /// Days of audit-log and session-event history kept (both pruned on
    /// startup).
    #[serde(default = "default_audit_retention_days")]
    pub audit_retention_days: u64,
    /// Days a deleted session stays restorable before its row is purged —
    /// `0`, the default, keeps every one. A purge is final, so it is the
    /// operator's to turn on (`thurbox-cli session purge` is the one-off).
    #[serde(default)]
    pub deleted_retention_days: u64,
    /// How often each session's git working tree is re-examined, in seconds.
    /// `0` turns the polling off: no diffstat, no ahead/behind, no `git`.
    ///
    /// The one number that governs how much `git` thurbox runs, because the
    /// work is per session: every session costs a `git status` plus, while its
    /// branch is ahead and unlanded, the merge check's handful of subprocesses,
    /// all of it repeated on this interval. Worth raising on an instance
    /// holding many sessions, and worth turning off where a subprocess is
    /// expensive for reasons outside thurbox — an endpoint-protection agent
    /// that scans every process launch (issue #1167).
    #[serde(default = "default_git_poll_secs")]
    pub git_poll_secs: u64,
    /// Per-feature on/off switches (`[features]` table). Absent table = all
    /// enabled.
    #[serde(default)]
    pub features: FeatureFlags,
    /// Desktop-notification settings (`[notifications]` table). Absent table =
    /// defaults (fire on `Attention`, skip the currently-focused session, 5s
    /// per-session dedup).
    #[serde(default)]
    pub notifications: NotificationSettings,
    /// Clipboard settings (`[clipboard]` table). Absent = `auto` transport,
    /// copy-on-select on.
    #[serde(default)]
    pub clipboard: ClipboardSettings,
    /// Remote-host settings (`[remote]` table).
    #[serde(default)]
    pub remote: RemoteSettings,
    /// OpenTelemetry settings (`[telemetry]` table). Off by default.
    #[serde(default)]
    pub telemetry: TelemetrySettings,
}

/// OpenTelemetry for the sessions thurbox spawns (`[telemetry]` table).
///
/// thurbox configures each agent's *own* OTel exporter rather than exporting
/// on its behalf: the OTel spec's `OTEL_EXPORTER_OTLP_*` variables for every
/// session, plus the agent's `telemetry_env` / `telemetry_args` from
/// `agents.toml` for the switches the spec does not cover. Read at each spawn
/// and restart, so a change applies to the next one; a running agent keeps the
/// environment it launched with. See ADR-36 in `docs/thurbox/architecture.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelemetrySettings {
    /// Master switch.
    #[serde(default)]
    pub enabled: bool,
    /// OTLP base URL (`OTEL_EXPORTER_OTLP_ENDPOINT`, `{otlp_endpoint}`) for
    /// local sessions; a remote one uses its host's `otel_endpoint`.
    #[serde(default = "default_otlp_endpoint")]
    pub endpoint: String,
    /// OTLP protocol (`OTEL_EXPORTER_OTLP_PROTOCOL`, `{otlp_protocol}`).
    /// `http/protobuf` is the one every native agent exporter accepts.
    #[serde(default = "default_otlp_protocol")]
    pub protocol: String,
    /// OTLP request headers (`OTEL_EXPORTER_OTLP_HEADERS`), e.g. a hosted
    /// backend's auth token. They sit in the pane's environment, so the agent
    /// process can read them.
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    /// Pass the spec variables and each agent's `telemetry_env` /
    /// `telemetry_args` to the sessions thurbox spawns.
    #[serde(default = "default_true")]
    pub configure_agents: bool,
    /// Export thurbox's own `thurbox.*` session metrics from `automation
    /// tick` (ADR-37).
    #[serde(default = "default_true")]
    pub export_thurbox_metrics: bool,
    /// Export one OTLP log record per session event from `automation tick`.
    #[serde(default = "default_true")]
    pub export_session_events: bool,
    /// Bridge the interface's own WARN-and-above `tracing` events to the
    /// collector (ADR-38). Restart-only: the subscriber is built once, at
    /// boot. Off by default — it serves debugging thurbox itself.
    #[serde(default)]
    pub export_diagnostic_logs: bool,
    /// Label thurbox's own metrics with `thurbox.session.id`. Off by default:
    /// every session then becomes a series of its own in the backend. The
    /// session event records always carry it.
    #[serde(default)]
    pub per_session: bool,
}

fn default_otlp_endpoint() -> String {
    "http://localhost:4318".into()
}
fn default_otlp_protocol() -> String {
    "http/protobuf".into()
}

impl Default for TelemetrySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: default_otlp_endpoint(),
            protocol: default_otlp_protocol(),
            headers: Default::default(),
            configure_agents: true,
            export_thurbox_metrics: true,
            export_session_events: true,
            export_diagnostic_logs: false,
            per_session: false,
        }
    }
}

impl TelemetrySettings {
    /// Whether spawned sessions get any telemetry configuration at all.
    pub fn configures_agents(&self) -> bool {
        self.enabled && self.configure_agents
    }

    /// Whether `automation tick` exports anything of thurbox's own.
    pub fn exports_thurbox(&self) -> bool {
        self.enabled && (self.export_thurbox_metrics || self.export_session_events)
    }

    /// Whether the interface bridges its diagnostic logs to the collector.
    pub fn exports_diagnostic_logs(&self) -> bool {
        self.enabled && self.export_diagnostic_logs
    }

    /// Whether the interface's diagnostic-log bridge, built once at boot,
    /// would be built differently from `other`. The rest of the table is live.
    fn diagnostic_bridge_differs(&self, other: &Self) -> bool {
        let on = self.exports_diagnostic_logs();
        on != other.exports_diagnostic_logs()
            || (on
                && (self.endpoint != other.endpoint
                    || self.protocol != other.protocol
                    || self.headers != other.headers))
    }
}

/// How sessions on remote hosts are mirrored (`[remote]` table).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteSettings {
    /// Also list the sessions a shareable host mirrors from hosts of its own
    /// (A → B → C: C's sessions, seen on A through B). Each session is listed
    /// once, on the most direct path this instance has to it. Off by default:
    /// a host's list is its own sessions, not a remote's remotes. Read by
    /// every mirror pass, so a change applies on the next one.
    #[serde(default)]
    pub transitive_sessions: bool,
}

/// Whole-feature switches (`[features]` in settings.toml). Each flag hides the
/// feature's UI and blocks its keybinding; disabling `automations` also stops
/// the TUI firing schedules and arming the heartbeat. Data and
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
    /// Native code-review view (tuicr-like): the diff/comment view + its
    /// keybinding.
    #[serde(default = "default_true")]
    pub code_review: bool,
    /// Perf HUD overlay (F12): live perf counters + frame/tick timing. Opening
    /// it also turns on wall-clock timing collection (see docs/thurbox/performance.md).
    #[serde(default = "default_true")]
    pub perf_hud: bool,
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
    /// Keep sessions undoable on delete, leaving windows and worktrees intact.
    /// Off by default in every build: TUI and CLI delete runtime resources
    /// immediately. CLI `--soft` and `--force` override this preference.
    #[serde(default)]
    pub soft_delete: bool,
    /// Version-update check: the TUI header "update available" badge and the
    /// `thurbox-cli version --check` command. **On by default for 1.0** — it
    /// makes a network call to GitHub to learn when a newer release is
    /// available. Turn it off with `[features] version_check = false`.
    #[serde(default = "default_true")]
    pub version_check: bool,
    /// Silent auto-update: the TUI silently downloads, verifies, and replaces
    /// the installed binaries on startup when a newer release exists, and the
    /// `thurbox-cli update` command does the same on demand. Also keeps installed
    /// extensions fresh — once the binary upgrades, the self-heal pass (TUI
    /// startup + headless tick) refreshes any extension that is now stale instead
    /// of merely nudging. **On by default for 1.0** — opt out with `[features]
    /// auto_update = false`; it makes a network call and replaces files on
    /// disk. The new version applies on the next launch.
    #[serde(default = "default_true")]
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

/// Which clipboard transport to use (`[clipboard] provider`). `Auto` (the
/// default) tries the local display server first and falls back to OSC 52,
/// which reaches the terminal emulator's clipboard through any number of SSH
/// hops. The forcing variants exist because no auto-detection is right for
/// everyone: `Native` suits a local desktop whose terminal mangles OSC 52,
/// `Osc52` suits a machine with a stale/unwanted local clipboard, and `None`
/// disables writes entirely.
///
/// Copy only — clipboard *reads* never use OSC 52 (see [`crate::clipboard`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ClipboardProvider {
    /// Native clipboard when it works, OSC 52 otherwise.
    #[default]
    Auto,
    /// Local display server only; never emit OSC 52.
    Native,
    /// OSC 52 only; skip the native clipboard.
    Osc52,
    /// Disable clipboard writes.
    None,
}

/// Clipboard settings (`[clipboard]` in settings.toml).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardSettings {
    /// Transport selection. See [`ClipboardProvider`].
    #[serde(default)]
    pub provider: ClipboardProvider,
    /// Copy a mouse selection when the drag is released, with no key pressed.
    ///
    /// On by default, as in Herdr, Zellij and WezTerm: it is the one copy no
    /// terminal emulator can intercept, which is what macOS needs where the
    /// terminal keeps `Cmd+C` for itself. While it is on, `Ctrl+C` is always
    /// the interrupt; turned off, `Ctrl+C` copies a selection instead.
    #[serde(default = "default_true")]
    pub copy_on_select: bool,
}

impl Default for ClipboardSettings {
    fn default() -> Self {
        Self {
            provider: ClipboardProvider::default(),
            copy_on_select: true,
        }
    }
}

/// How `Ctrl+O` launches the editor (the DB `editor_mode` key, set via
/// `thurbox-cli editor mode`). `Auto` (the default) detects terminal vs GUI
/// editors from the command name and gives terminal editors (vim, nano,
/// `ttt`, helix, …) a real TTY — a floating `tmux display-popup` when thurbox
/// runs inside tmux, or a TUI-suspend-and-resume when it does not — while GUI
/// editors (`code`, `zed`, …) stay detached (the classic behavior). `Terminal`
/// forces the TTY path for every editor; `Gui` forces the detached spawn.
/// Stored in SQLite (not `settings.toml`) so it applies live, like
/// `editor_command`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EditorMode {
    /// Detect from the command name (curated terminal/GUI lists).
    #[default]
    Auto,
    /// Always give the editor a real TTY (popup / suspend).
    Terminal,
    /// Always spawn detached (the pre-terminal-support behavior).
    Gui,
}

impl EditorMode {
    /// Parse a stored `editor_mode` value, tolerating any case; `None`,
    /// empty, or an unrecognized value yields the default (`Auto`). Used by
    /// the DB getter so a corrupted row can't panic the TUI.
    pub fn parse_stored(stored: Option<&str>) -> Self {
        match stored.map(str::trim) {
            Some(s) => match s.to_ascii_lowercase().as_str() {
                "terminal" | "term" | "tty" => EditorMode::Terminal,
                "gui" | "detached" | "graphical" => EditorMode::Gui,
                _ => EditorMode::Auto,
            },
            None => EditorMode::Auto,
        }
    }

    /// The value written back to the DB (canonical, lowercase).
    pub fn as_db_value(self) -> &'static str {
        match self {
            EditorMode::Auto => "auto",
            EditorMode::Terminal => "terminal",
            EditorMode::Gui => "gui",
        }
    }
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
    /// Notify when an automation run fails or times out, from the headless
    /// `automation tick` as well — once per failing streak (issue #1508).
    /// Off by default.
    #[serde(default)]
    pub on_automation_failure: bool,
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
            on_automation_failure: false,
        }
    }
}

fn default_true() -> bool {
    true
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
            code_review: true,
            perf_hud: true,
            mouse: true,
            notifications: true,
            soft_delete: false,
            version_check: true,
            auto_update: true,
        }
    }
}

/// The scrollback a terminal keeps when `settings.toml` names none.
pub fn default_scrollback_lines() -> usize {
    1000
}
fn default_hidden_terminal_secs() -> u64 {
    30
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
fn default_git_poll_secs() -> u64 {
    5
}

/// When a change to a field of [`Settings`] takes effect in a running interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies {
    /// Read where it is used, every time, so an edit is in force at once.
    Live,
    /// Consumed once, at startup, by something that cannot be redone mid-run.
    NextLaunch,
}

/// One row of [`FIELDS`]: a field of `settings.toml` and when it takes effect.
pub struct Field {
    /// The path the file spells it as: a top-level key, a `[features]` flag
    /// (`features.mouse`), or a whole table.
    pub key: &'static str,
    /// When a change to it takes effect.
    pub applies: Applies,
    differs: fn(&Settings, &Settings) -> bool,
    take: fn(&mut Settings, &Settings),
}

/// A row comparing and copying the field at `$path` as a whole.
macro_rules! field {
    ($applies:ident, $first:ident $(. $rest:ident)*) => {
        Field {
            key: concat!(stringify!($first) $(, ".", stringify!($rest))*),
            applies: Applies::$applies,
            differs: |a, b| a.$first $(.$rest)* != b.$first $(.$rest)*,
            take: |into, from| into.$first $(.$rest)* = from.$first $(.$rest)*.clone(),
        }
    };
}

/// Every field of `settings.toml`, and when a change to it takes effect.
///
/// The one place that says so: [`Settings::restart_only_differs`] and
/// [`Settings::with_live_from`] (what `kernel::config::adopt` takes into force)
/// both read it, so a field cannot be live in one and restart-only in the
/// other. Two hand-kept lists did drift — `[clipboard]` was held as booted
/// and reported live (issue #1638). A test fails until a new field has a row.
pub const FIELDS: &[Field] = &[
    // The multiplexer a session is created on is picked from the booted
    // registry.
    field!(NextLaunch, multiplexer),
    field!(Live, config_version),
    // Every terminal's parser is built with it.
    field!(NextLaunch, scrollback_lines),
    // The terminals read these once, when they are built.
    field!(NextLaunch, hidden_terminal_secs),
    field!(NextLaunch, two_panel_min_cols),
    field!(NextLaunch, three_panel_min_cols),
    // Pruned once, at startup.
    field!(NextLaunch, audit_retention_days),
    field!(NextLaunch, deleted_retention_days),
    // The git-stat cache is built with it, and a cache that re-read it would
    // also have to re-age every entry.
    field!(NextLaunch, git_poll_secs),
    // The feature switches that only gate whether a surface is drawn, which is
    // decided per frame.
    field!(Live, features.tasks),
    field!(Live, features.file_viewer),
    field!(Live, features.info_panel),
    field!(Live, features.global_search),
    field!(Live, features.shell_pane),
    field!(Live, features.code_review),
    field!(Live, features.perf_hud),
    field!(Live, features.soft_delete),
    // Capture is an escape already sent, the notifier is a thread already
    // started (or not), the heartbeat is armed once, and the update work ran
    // at startup.
    field!(NextLaunch, features.mouse),
    field!(NextLaunch, features.notifications),
    field!(NextLaunch, features.automations),
    field!(NextLaunch, features.version_check),
    field!(NextLaunch, features.auto_update),
    // The dispatcher thread is started once with the backend it resolved.
    field!(NextLaunch, notifications),
    // Read at every copy.
    field!(Live, clipboard),
    // The mirror reads the file itself on every pass.
    field!(Live, remote),
    // Spawns, restarts and the tick read the file themselves, so the table
    // applies live there. What is in force here feeds only the diagnostic-log
    // bridge, built once at boot (ADR-38), so it is held as booted and only a
    // change to how that bridge would be built needs a restart.
    Field {
        key: "telemetry",
        applies: Applies::NextLaunch,
        differs: |a, b| a.telemetry.diagnostic_bridge_differs(&b.telemetry),
        take: |into, from| into.telemetry = from.telemetry.clone(),
    },
];

impl Settings {
    /// Whether any [`Applies::NextLaunch`] field of [`FIELDS`] differs between
    /// `self` and `other`. Drives the "some changes apply after restart" hint
    /// shown by the settings panel and the live-reload toast.
    pub fn restart_only_differs(&self, other: &Settings) -> bool {
        FIELDS
            .iter()
            .any(|f| f.applies == Applies::NextLaunch && (f.differs)(self, other))
    }

    /// `self` with every [`Applies::Live`] field of [`FIELDS`] taken from
    /// `fresh`, and every [`Applies::NextLaunch`] field left as it is.
    pub fn with_live_from(&self, fresh: &Settings) -> Settings {
        let mut adopted = self.clone();
        for field in FIELDS.iter().filter(|f| f.applies == Applies::Live) {
            (field.take)(&mut adopted, fresh);
        }
        adopted
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            multiplexer: None,
            config_version: None,
            scrollback_lines: default_scrollback_lines(),
            hidden_terminal_secs: default_hidden_terminal_secs(),
            two_panel_min_cols: default_two_panel_min_cols(),
            three_panel_min_cols: default_three_panel_min_cols(),
            audit_retention_days: default_audit_retention_days(),
            deleted_retention_days: 0,
            git_poll_secs: default_git_poll_secs(),
            features: FeatureFlags::default(),
            notifications: NotificationSettings::default(),
            clipboard: ClipboardSettings::default(),
            remote: RemoteSettings::default(),
            telemetry: TelemetrySettings::default(),
        }
    }
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
    fn copy_on_select_is_on_unless_turned_off() {
        let s: Settings = toml::from_str("").unwrap();
        assert!(s.clipboard.copy_on_select);
        let s: Settings = toml::from_str("[clipboard]\nprovider = \"osc52\"").unwrap();
        assert!(s.clipboard.copy_on_select);
        let s: Settings = toml::from_str("[clipboard]\ncopy_on_select = false").unwrap();
        assert!(!s.clipboard.copy_on_select);
        assert_eq!(s.clipboard.provider, ClipboardProvider::Auto);
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
    fn soft_delete_feature_flag_defaults_false_and_parses() {
        assert!(!FeatureFlags::default().soft_delete);
        let s: Settings = toml::from_str("[features]\nsoft_delete = true").unwrap();
        assert!(s.features.soft_delete);
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
    fn git_poll_secs_defaults_to_five_and_zero_turns_polling_off() {
        // The one number that governs how much `git` an instance runs: every
        // session is re-statted on this interval, so the cost is it, times the
        // session count. `0` is off — the answer for a machine where an
        // endpoint-protection agent scans every subprocess.
        let s: Settings = toml::from_str("").unwrap();
        assert_eq!(s.git_poll_secs, 5);

        let s: Settings = toml::from_str("git_poll_secs = 30").unwrap();
        assert_eq!(s.git_poll_secs, 30);

        let s: Settings = toml::from_str("git_poll_secs = 0").unwrap();
        assert_eq!(s.git_poll_secs, 0);
    }

    #[test]
    fn git_poll_secs_takes_effect_on_the_next_launch() {
        // Read once, where the cache is built, so the panel has to mark it
        // restart-only rather than promise a change it cannot deliver.
        let base = Settings::default();
        let changed = Settings {
            git_poll_secs: base.git_poll_secs + 1,
            ..base.clone()
        };
        assert!(base.restart_only_differs(&changed));
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
        assert!(s.notifications.suppress_for_active);
        assert!(s.notifications.sound);
    }

    #[test]
    fn notifications_type_mismatch_is_rejected() {
        let err = toml::from_str::<Settings>("[notifications]\nsound = \"loud\"").unwrap_err();
        assert!(err.to_string().contains("sound"));
    }

    #[test]
    fn version_check_flag_defaults_on_and_parses() {
        // On by default for 1.0 (it makes a network call to GitHub).
        let s: Settings = toml::from_str("[features]").unwrap();
        assert!(s.features.version_check, "version_check defaults on");
        assert!(s.features.tasks, "other flags still default on");

        let s: Settings = toml::from_str("[features]\nversion_check = false").unwrap();
        assert!(!s.features.version_check);
        assert!(s.features.mouse, "untouched flags stay at their default");
    }

    #[test]
    fn auto_update_flag_defaults_on_and_parses() {
        // On by default for 1.0 (network call + writes to disk).
        let s: Settings = toml::from_str("[features]").unwrap();
        assert!(s.features.auto_update, "auto_update defaults on");
        assert!(s.features.tasks, "other flags still default on");

        let s: Settings = toml::from_str("[features]\nauto_update = false").unwrap();
        assert!(!s.features.auto_update);
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
    fn only_the_diagnostic_log_bridge_makes_telemetry_restart_only() {
        let off = Settings::default();
        let mut moved = off.clone();
        moved.telemetry.enabled = true;
        moved.telemetry.endpoint = "http://collector:4318".into();
        assert!(
            !off.restart_only_differs(&moved),
            "spawns and ticks read [telemetry] live"
        );

        let mut bridged = moved.clone();
        bridged.telemetry.export_diagnostic_logs = true;
        assert!(moved.restart_only_differs(&bridged));

        let mut elsewhere = bridged.clone();
        elsewhere.telemetry.endpoint = "http://other:4318".into();
        assert!(bridged.restart_only_differs(&elsewhere));
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
            code_review,
            perf_hud,
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
            code_review,
            perf_hud,
            mouse,
            notifications,
            soft_delete,
            version_check,
            auto_update,
        );

        // `live` flags gate UI panels read from `App.features` every frame, so
        // flipping one is NOT a restart-only difference; the rest are read once
        // at startup and MUST register as one.
        let live: [fn(&mut FeatureFlags); 8] = [
            |f| f.tasks = !f.tasks,
            |f| f.file_viewer = !f.file_viewer,
            |f| f.global_search = !f.global_search,
            |f| f.info_panel = !f.info_panel,
            |f| f.shell_pane = !f.shell_pane,
            |f| f.code_review = !f.code_review,
            |f| f.perf_hud = !f.perf_hud,
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
    fn every_field_has_exactly_one_row() {
        // A field with no row is neither taken live nor reported as needing a
        // restart: the silent half of #1638. Each top-level key is a row, or
        // (for `[features]`) each of its keys is.
        let value = serde_json::to_value(Settings::default()).unwrap();
        let mut keys = Vec::new();
        for (key, inner) in value.as_object().unwrap() {
            match inner.as_object() {
                Some(table) if key == "features" => {
                    keys.extend(table.keys().map(|flag| format!("features.{flag}")));
                }
                _ => keys.push(key.clone()),
            }
        }
        let mut rows: Vec<&str> = FIELDS.iter().map(|f| f.key).collect();
        rows.sort_unstable();
        keys.sort_unstable();
        assert_eq!(rows, keys);
    }

    #[test]
    fn a_next_launch_row_is_never_taken_and_a_live_one_always_is() {
        // Two settings that disagree on every field: a row's `take` must make
        // it agree, and only a live row may be taken.
        let base = Settings::default();
        let other: Settings = toml::from_str(
            "multiplexer = \"rmux\"\nconfig_version = 2\nscrollback_lines = 1\n\
             hidden_terminal_secs = 1\ntwo_panel_min_cols = 1\nthree_panel_min_cols = 1\n\
             audit_retention_days = 1\ndeleted_retention_days = 1\ngit_poll_secs = 1\n\
             [features]\ntasks = false\nautomations = false\nfile_viewer = false\n\
             global_search = false\ninfo_panel = false\nshell_pane = false\n\
             code_review = false\nperf_hud = false\nmouse = false\nnotifications = false\n\
             soft_delete = true\nversion_check = false\nauto_update = false\n\
             [notifications]\nsound = false\n[clipboard]\nprovider = \"none\"\n\
             [remote]\ntransitive_sessions = true\n\
             [telemetry]\nenabled = true\nexport_diagnostic_logs = true\n",
        )
        .unwrap();
        let adopted = base.with_live_from(&other);
        for field in FIELDS {
            assert!(
                (field.differs)(&base, &other),
                "{} is not varied",
                field.key
            );
            let expect = field.applies == Applies::NextLaunch;
            assert_eq!(
                (field.differs)(&adopted, &other),
                expect,
                "{} was taken or left wrongly",
                field.key
            );
        }
        assert!(base.restart_only_differs(&other));
        assert!(!adopted.restart_only_differs(&base));
    }
}
