use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub config_version: Option<u32>,
    #[serde(default = "default_scrollback_lines")]
    pub scrollback_lines: usize,
    #[serde(default = "default_hidden_terminal_secs")]
    pub hidden_terminal_secs: u64,
    #[serde(default)]
    pub stream_panes: bool,
    #[serde(default = "default_two_panel_min_cols")]
    pub two_panel_min_cols: u16,
    #[serde(default = "default_git_poll_secs")]
    pub git_poll_secs: u64,
    #[serde(default)]
    pub features: FeatureFlags,
    #[serde(default)]
    pub notifications: NotificationSettings,
    #[serde(default)]
    pub clipboard: ClipboardSettings,
    #[serde(default, deserialize_with = "fleet_names")]
    pub fleets: BTreeMap<String, FleetDef>,
    #[serde(default)]
    pub telemetry: otel_env::TelemetryConfig,
}

pub const RETIRED_KEYS: &[(&str, &str)] = &[
    (
        "multiplexer",
        "Fleet's runtime runs every session's terminal",
    ),
    ("remote", "thurbox lists no sessions through other hosts"),
    ("features.soft_delete", "Fleet deletes a session at once"),
    (
        "audit_retention_days",
        "thurbox keeps no session database to prune",
    ),
    (
        "deleted_retention_days",
        "thurbox keeps no session database to prune",
    ),
    ("three_panel_min_cols", "the interface has no third column"),
    ("features.file_viewer", "the interface has no file viewer"),
    (
        "features.info_panel",
        "a mod's own pane is switched off in the Interface tab",
    ),
    (
        "features.code_review",
        "a mod's own pane is switched off in the Interface tab",
    ),
    (
        "features.global_search",
        "the search pane is switched off in the Interface tab",
    ),
    (
        "notifications.also_on_waiting",
        "Fleet reports no finished state",
    ),
    ("fleet", "the Fleets are listed as [fleets.<name>]"),
];

pub const IGNORED_KEYS: &[&str] = &["features.tasks"];

pub fn retired_key(path: &str) -> Option<(&'static str, &'static str)> {
    RETIRED_KEYS.iter().copied().find(|(key, _)| {
        path == *key
            || path
                .strip_prefix(key)
                .is_some_and(|rest| rest.starts_with('.'))
    })
}

fn fleet_names<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, FleetDef>, D::Error> {
    let fleets = BTreeMap::<String, FleetDef>::deserialize(deserializer)?;
    match fleets
        .keys()
        .find(|name| name.is_empty() || name.contains(':'))
    {
        Some(name) => Err(serde::de::Error::custom(format!(
            "the Fleet name \"{name}\" is not one: a session's id is <fleet>:<task>, so a \
             name holds no ':' and is not empty"
        ))),
        None => Ok(fleets),
    }
}

pub const IMPLIED_FLEET: &str = "local";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetDef {
    #[serde(default)]
    pub transport: FleetTransport,
    #[serde(default = "default_fleet_program")]
    pub program: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
}

impl Default for FleetDef {
    fn default() -> Self {
        Self {
            transport: FleetTransport::default(),
            program: default_fleet_program(),
            config: None,
        }
    }
}

fn default_fleet_program() -> String {
    "fleet".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum FleetTransport {
    Local(Local),
    Ssh {
        ssh: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ssh_opts: Vec<String>,
    },
    Wsl {
        wsl: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Local {
    Local,
}

impl Default for FleetTransport {
    fn default() -> Self {
        Self::Local(Local::Local)
    }
}

#[derive(Deserialize)]
struct TransportTable {
    ssh: Option<String>,
    ssh_opts: Option<Vec<String>>,
    wsl: Option<String>,
}

impl<'de> Deserialize<'de> for FleetTransport {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = FleetTransport;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(
                    "transport \"local\", { ssh = \"<destination>\", ssh_opts = [...] } or \
                     { wsl = \"<distro>\" }",
                )
            }

            fn visit_str<E: serde::de::Error>(self, word: &str) -> Result<Self::Value, E> {
                match word {
                    "local" => Ok(FleetTransport::default()),
                    other => Err(E::custom(format!(
                        "transport \"{other}\" is not one: write \"local\", {{ ssh = ... }} or \
                         {{ wsl = ... }}"
                    ))),
                }
            }

            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                map: M,
            ) -> Result<Self::Value, M::Error> {
                use serde::de::Error;
                let table =
                    TransportTable::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                match table {
                    TransportTable {
                        ssh: Some(ssh),
                        ssh_opts,
                        wsl: None,
                    } => Ok(FleetTransport::Ssh {
                        ssh,
                        ssh_opts: ssh_opts.unwrap_or_default(),
                    }),
                    TransportTable {
                        ssh: None,
                        ssh_opts: None,
                        wsl: Some(wsl),
                    } => Ok(FleetTransport::Wsl { wsl }),
                    TransportTable {
                        ssh: Some(_),
                        wsl: Some(_),
                        ..
                    } => Err(M::Error::custom(
                        "transport names both ssh and wsl: a Fleet is reached one way",
                    )),
                    TransportTable {
                        ssh: None,
                        ssh_opts: Some(_),
                        ..
                    } => Err(M::Error::custom(
                        "transport has ssh_opts but no ssh destination",
                    )),
                    TransportTable {
                        ssh: None,
                        wsl: None,
                        ..
                    } => Err(M::Error::custom(
                        "transport names no ssh destination or wsl distro",
                    )),
                }
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

impl FleetTransport {
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local(_))
    }
}

impl Settings {
    pub fn fleets(&self) -> Vec<(String, FleetDef)> {
        if self.fleets.is_empty() {
            return vec![(IMPLIED_FLEET.to_string(), FleetDef::default())];
        }
        self.fleets
            .iter()
            .map(|(name, def)| (name.clone(), def.clone()))
            .collect()
    }

    pub fn fleet_in_force(&self) -> (String, FleetDef) {
        self.fleets().swap_remove(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureFlags {
    #[serde(default = "default_true")]
    pub shell_pane: bool,
    #[serde(default = "default_true")]
    pub perf_hud: bool,
    #[serde(default = "default_true")]
    pub mouse: bool,
    #[serde(default = "default_true")]
    pub notifications: bool,
    #[serde(default = "default_true")]
    pub version_check: bool,
    #[serde(default = "default_true")]
    pub auto_update: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NotificationBackend {
    #[default]
    Auto,
    Dbus,
    Windows,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ClipboardProvider {
    #[default]
    Auto,
    Native,
    Osc52,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardSettings {
    #[serde(default)]
    pub provider: ClipboardProvider,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EditorMode {
    #[default]
    Auto,
    Terminal,
    Gui,
}

impl EditorMode {
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

    pub fn as_db_value(self) -> &'static str {
        match self {
            EditorMode::Auto => "auto",
            EditorMode::Terminal => "terminal",
            EditorMode::Gui => "gui",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationSettings {
    #[serde(default = "default_true")]
    pub suppress_for_active: bool,
    #[serde(default = "default_true")]
    pub sound: bool,
    #[serde(default = "default_notification_min_interval_secs")]
    pub min_interval_secs: u64,
    #[serde(default)]
    pub backend: NotificationBackend,
}

fn default_notification_min_interval_secs() -> u64 {
    5
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
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

impl Default for FeatureFlags {
    fn default() -> Self {
        Self {
            shell_pane: true,
            perf_hud: true,
            mouse: true,
            notifications: true,
            version_check: true,
            auto_update: true,
        }
    }
}

pub fn default_scrollback_lines() -> usize {
    1000
}
fn default_hidden_terminal_secs() -> u64 {
    30
}
fn default_two_panel_min_cols() -> u16 {
    80
}
fn default_git_poll_secs() -> u64 {
    5
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applies {
    Live,
    NextLaunch,
}

pub struct Field {
    pub key: &'static str,
    pub applies: Applies,
    differs: fn(&Settings, &Settings) -> bool,
    take: fn(&mut Settings, &Settings),
}

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

pub const FIELDS: &[Field] = &[
    field!(Live, config_version),
    field!(NextLaunch, scrollback_lines),
    field!(NextLaunch, hidden_terminal_secs),
    field!(NextLaunch, stream_panes),
    field!(NextLaunch, two_panel_min_cols),
    field!(NextLaunch, git_poll_secs),
    field!(Live, features.shell_pane),
    field!(Live, features.perf_hud),
    field!(NextLaunch, features.mouse),
    field!(NextLaunch, features.notifications),
    field!(NextLaunch, features.version_check),
    field!(NextLaunch, features.auto_update),
    field!(NextLaunch, notifications),
    field!(Live, clipboard),
    field!(NextLaunch, fleets),
    field!(NextLaunch, telemetry),
];

impl Settings {
    pub fn restart_only_differs(&self, other: &Settings) -> bool {
        FIELDS
            .iter()
            .any(|f| f.applies == Applies::NextLaunch && (f.differs)(self, other))
    }

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
            config_version: None,
            scrollback_lines: default_scrollback_lines(),
            hidden_terminal_secs: default_hidden_terminal_secs(),
            stream_panes: false,
            two_panel_min_cols: default_two_panel_min_cols(),
            git_poll_secs: default_git_poll_secs(),
            features: FeatureFlags::default(),
            notifications: NotificationSettings::default(),
            clipboard: ClipboardSettings::default(),
            fleets: BTreeMap::new(),
            telemetry: otel_env::TelemetryConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_local_fleet_is_implied_only_when_none_is_listed() {
        let none: Settings = toml::from_str("").unwrap();
        assert_eq!(
            none.fleets(),
            vec![(IMPLIED_FLEET.to_string(), FleetDef::default())]
        );
        assert!(none.fleet_in_force().1.transport.is_local());

        let listed: Settings = toml::from_str(
            "[fleets.laptop]\n\
             [fleets.box]\ntransport = { ssh = \"me@box\", ssh_opts = [\"-p\", \"2222\"] }\n\
             program = \"/home/me/.cargo/bin/fleet\"\nconfig = \"/home/me/fleet.toml\"\n\
             [fleets.ubuntu]\ntransport = { wsl = \"Ubuntu\" }\n",
        )
        .unwrap();
        let names: Vec<String> = listed.fleets().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["box", "laptop", "ubuntu"]);
        let (name, def) = listed.fleet_in_force();
        assert_eq!(name, "box");
        assert_eq!(
            def,
            FleetDef {
                transport: FleetTransport::Ssh {
                    ssh: "me@box".into(),
                    ssh_opts: vec!["-p".into(), "2222".into()],
                },
                program: "/home/me/.cargo/bin/fleet".into(),
                config: Some("/home/me/fleet.toml".into()),
            }
        );
        assert_eq!(listed.fleets["laptop"], FleetDef::default());
        assert_eq!(
            listed.fleets["ubuntu"].transport,
            FleetTransport::Wsl {
                wsl: "Ubuntu".into()
            }
        );
        assert!(toml::from_str::<Settings>("[fleets.x]\ntransport = \"remote\"\n").is_err());
        assert!(toml::from_str::<Settings>("[fleets.\"a:b\"]\n").is_err());
    }

    #[test]
    fn empty_document_yields_defaults() {
        let s: Settings = toml::from_str("").unwrap();
        assert_eq!(s, Settings::default());
        assert_eq!(s.scrollback_lines, 1000);
        assert_eq!(s.two_panel_min_cols, 80);
        assert_eq!(s.git_poll_secs, 5);
        assert!(s.clipboard.copy_on_select);
        assert_eq!(s.clipboard.provider, ClipboardProvider::Auto);
        let f = &s.features;
        assert!(f.shell_pane && f.perf_hud && f.mouse && f.notifications);
        assert!(f.version_check && f.auto_update);
    }

    #[test]
    fn notifications_table_defaults() {
        let s: Settings = toml::from_str("").unwrap();
        assert_eq!(s.notifications, NotificationSettings::default());
        assert!(s.notifications.suppress_for_active);
        assert!(s.notifications.sound);
        assert_eq!(s.notifications.min_interval_secs, 5);
        assert_eq!(s.notifications.backend, NotificationBackend::Auto);
    }

    #[test]
    fn a_key_overrides_only_its_own_setting() {
        let with = |edit: fn(&mut Settings)| {
            let mut s = Settings::default();
            edit(&mut s);
            s
        };
        let cases: &[(&str, Settings)] = &[
            ("[features]", Settings::default()),
            ("[notifications]", Settings::default()),
            (
                "scrollback_lines = 5000",
                with(|s| s.scrollback_lines = 5000),
            ),
            ("git_poll_secs = 30", with(|s| s.git_poll_secs = 30)),
            ("git_poll_secs = 0", with(|s| s.git_poll_secs = 0)),
            (
                "[clipboard]\nprovider = \"osc52\"",
                with(|s| s.clipboard.provider = ClipboardProvider::Osc52),
            ),
            (
                "[clipboard]\ncopy_on_select = false",
                with(|s| s.clipboard.copy_on_select = false),
            ),
            (
                "[features]\nshell_pane = false",
                with(|s| s.features.shell_pane = false),
            ),
            (
                "[features]\nmouse = false",
                with(|s| s.features.mouse = false),
            ),
            (
                "[features]\nnotifications = false",
                with(|s| s.features.notifications = false),
            ),
            (
                "[features]\nversion_check = false",
                with(|s| s.features.version_check = false),
            ),
            (
                "[features]\nauto_update = false",
                with(|s| s.features.auto_update = false),
            ),
            (
                "[notifications]\nsound = false\nmin_interval_secs = 30",
                with(|s| {
                    s.notifications.sound = false;
                    s.notifications.min_interval_secs = 30;
                }),
            ),
            (
                "[notifications]\nbackend = \"auto\"",
                with(|s| s.notifications.backend = NotificationBackend::Auto),
            ),
            (
                "[notifications]\nbackend = \"dbus\"",
                with(|s| s.notifications.backend = NotificationBackend::Dbus),
            ),
            (
                "[notifications]\nbackend = \"windows\"",
                with(|s| s.notifications.backend = NotificationBackend::Windows),
            ),
            (
                "[notifications]\nbackend = \"off\"",
                with(|s| s.notifications.backend = NotificationBackend::Off),
            ),
        ];
        for (document, want) in cases {
            let parsed: Settings = toml::from_str(document).unwrap();
            assert_eq!(&parsed, want, "{document}");
        }
    }

    #[test]
    fn a_value_of_the_wrong_type_or_name_is_rejected() {
        for (document, key) in [
            ("scrollback_lines = \"many\"", "scrollback_lines"),
            ("[features]\nshell_pane = \"no\"", "shell_pane"),
            ("[notifications]\nsound = \"loud\"", "sound"),
            ("[notifications]\nbackend = \"telepathy\"", "telepathy"),
        ] {
            let err = toml::from_str::<Settings>(document).unwrap_err();
            assert!(err.to_string().contains(key), "{document}: {err}");
        }
    }

    #[test]
    fn git_poll_secs_takes_effect_on_the_next_launch() {
        let base = Settings::default();
        let changed = Settings {
            git_poll_secs: base.git_poll_secs + 1,
            ..base.clone()
        };
        assert!(base.restart_only_differs(&changed));
    }

    #[test]
    fn restart_only_differs_ignores_live_flags_but_catches_restart_ones() {
        let base = Settings::default();

        let mut live = base.clone();
        live.features.shell_pane = !live.features.shell_pane;
        assert!(!base.restart_only_differs(&live));

        let mut mouse = base.clone();
        mouse.features.mouse = !mouse.features.mouse;
        assert!(base.restart_only_differs(&mouse));

        let mut scrollback = base.clone();
        scrollback.scrollback_lines += 1;
        assert!(base.restart_only_differs(&scrollback));

        let mut notif = base.clone();
        notif.notifications.sound = !notif.notifications.sound;
        assert!(base.restart_only_differs(&notif));

        assert!(!base.restart_only_differs(&base.clone()));
    }

    #[test]
    fn every_feature_flag_is_classified_restart_or_live() {
        let FeatureFlags {
            shell_pane,
            perf_hud,
            mouse,
            notifications,
            version_check,
            auto_update,
        } = FeatureFlags::default();
        let _ = (
            shell_pane,
            perf_hud,
            mouse,
            notifications,
            version_check,
            auto_update,
        );

        let live: [fn(&mut FeatureFlags); 2] = [
            |f| f.shell_pane = !f.shell_pane,
            |f| f.perf_hud = !f.perf_hud,
        ];
        let restart: [fn(&mut FeatureFlags); 4] = [
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
        let base = Settings::default();
        let other: Settings = toml::from_str(
            "config_version = 2\nscrollback_lines = 1\n\
             hidden_terminal_secs = 1\nstream_panes = true\ntwo_panel_min_cols = 1\n\
             git_poll_secs = 1\n\
             [features]\nshell_pane = false\n\
             perf_hud = false\nmouse = false\nnotifications = false\n\
             version_check = false\nauto_update = false\n\
             [notifications]\nsound = false\n[clipboard]\nprovider = \"none\"\n\
             [fleets.box]\nprogram = \"/opt/fleet\"\n\
             [telemetry]\nendpoint = \"http://c:4318\"\n",
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
