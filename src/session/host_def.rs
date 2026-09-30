//! Remote-host definitions — pure data describing the off-local targets
//! thurbox can run sessions on: SSH machines and local WSL distros.
//!
//! Loaded from `~/.config/thurbox/hosts.toml` by
//! [`crate::agent::host_config`] (and, for WSL, auto-discovered there too).
//! Kept here in `session` (the dependency sink) so both `agent` (which builds
//! the tmux backend) and `git` (which runs `git` on the host for remote
//! worktrees) can depend on the same type without crossing the
//! module-isolation rules.
//!
//! A WSL distro is modeled as "SSH without the ssh": the only difference from
//! a remote host is the launch prefix (`wsl.exe -d <distro>` instead of
//! `ssh <dest>`). tmux, git, the agent, and the worktrees all run *inside* the
//! distro at native Linux paths, so everything downstream of the launcher is
//! identical to the SSH path.

use serde::{Deserialize, Serialize};

use super::{Multiplexer, Platform, Route, Via};

/// The environment variable every WSL2 distro's init sets to that distro's own
/// name. Present only *inside* a distro — not on Windows, not on a plain Linux
/// or macOS host.
pub const WSL_DISTRO_NAME_VAR: &str = "WSL_DISTRO_NAME";

/// The WSL distro thurbox is itself running inside, if any.
///
/// Read from the environment rather than probed for: `wsl.exe` is on `PATH`
/// inside a distro (interop), so its presence says a distro is *reachable* and
/// never which one we are already in.
pub fn current_wsl_distro() -> Option<String> {
    std::env::var(WSL_DISTRO_NAME_VAR)
        .ok()
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
}

/// Whether `distro` is the one [`current_wsl_distro`] reports, compared the way
/// `wsl.exe -d` matches a distro name — case-insensitively.
fn is_current_wsl_distro(distro: &str) -> bool {
    current_wsl_distro().is_some_and(|d| d.eq_ignore_ascii_case(distro))
}

/// The row rewrites the one-time WSL repair owes, decided by the registry.
///
/// Pure data, and here rather than in `storage` or `agent` because both need
/// to name it: `agent::host_config::wsl_repair_plan` decides it from
/// `hosts.toml`, and `storage` applies it — and `storage` may reference
/// `session` but not `agent`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WslRepairPlan {
    /// Backend names whose rows are this machine's own local rows, recorded as
    /// remote by a loopback host, and which no host the registry serves claims.
    pub to_local: Vec<String>,
    /// Candidate backend names left alone because a host the registry still
    /// serves registers under one of them, so the rows there cannot be told
    /// apart from that host's own.
    ///
    /// A **final** verdict, reported only so the user can be told which rows
    /// were left: those rows never become classifiable, so
    /// `session_ops::repair_wsl_loopback_rows` retires the repair rather than
    /// waiting for the claim to disappear — which would rewrite them on no
    /// better evidence than this pass had.
    pub withheld: Vec<String>,
}

impl WslRepairPlan {
    /// Whether there are no rows to rewrite — which a plan that
    /// [withheld](Self::withheld) every candidate also satisfies.
    pub fn is_empty(&self) -> bool {
        self.to_local.is_empty()
    }
}

/// How thurbox reaches a host: over SSH, or into a local WSL distro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HostKind {
    /// A remote machine reached with `ssh <destination>`.
    #[default]
    Ssh,
    /// A local Windows Subsystem for Linux distro reached with
    /// `wsl.exe -d <distro>`.
    Wsl,
}

/// A single off-local host: an SSH machine or a local WSL distro.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostDef {
    /// Short, unique name. The backend is registered as `ssh:<name>` or
    /// `wsl:<name>` depending on [`kind`](Self::kind).
    pub name: String,
    /// Transport kind: SSH (default) or WSL.
    #[serde(default)]
    pub kind: HostKind,
    /// SSH destination (e.g. `me@devbox`), resolved via the user's
    /// `~/.ssh/config`. Required for [`HostKind::Ssh`]; ignored for WSL.
    #[serde(default)]
    pub destination: String,
    /// WSL distro name (e.g. `Ubuntu`). Defaults to [`name`](Self::name) when
    /// unset. Ignored for [`HostKind::Ssh`].
    #[serde(default)]
    pub distro: Option<String>,
    /// Optional override for the host's `tmux -L` socket name. Defaults to the
    /// same socket thurbox uses locally.
    #[serde(default)]
    pub socket: Option<String>,
    /// Optional override for the host's tmux session name.
    #[serde(default)]
    pub session: Option<String>,
    /// Extra `ssh` flags inserted before the destination (e.g.
    /// `["-o", "ControlMaster=auto"]`). SSH-only.
    #[serde(default)]
    pub ssh_opts: Vec<String>,
    /// Optional absolute directory (inside the host / distro) under which git
    /// worktrees are created. When unset, the host's
    /// `$HOME/.local/share/thurbox/worktrees` is resolved at spawn time.
    #[serde(default)]
    pub worktrees_dir: Option<String>,
    /// Optional multiplexer binary on the host. Defaults to `tmux` (the WSL
    /// distro and a Unix SSH host both run `tmux`); set to `psmux` for a
    /// Windows SSH host (psmux speaks the same control-mode wire protocol).
    #[serde(default)]
    pub multiplexer: Option<String>,
    /// The host's operating system: `"posix"` or `"windows"`. Independent of
    /// [`multiplexer`](Self::multiplexer) and of [`kind`](Self::kind); read
    /// through [`platform`](Self::platform), which settles an entry that
    /// leaves it unset.
    #[serde(default)]
    pub platform: Option<Platform>,
    /// Whether the host's own thurbox database is the record of the sessions
    /// on it (`true`, the default): a remote thurbox mirrors that database and
    /// delegates create/delete/restart/restore to `thurbox-cli` on the host,
    /// provisioning that CLI when the host has none. `false` uses the host
    /// exactly as before sharing existed — worktrees and hooks driven from
    /// here, nothing mirrored, nothing installed on the host.
    #[serde(default = "default_share_sessions")]
    pub share_sessions: bool,
    /// Directories put in front of the agent's `PATH` on the host, ahead of
    /// the login-shell `PATH` thurbox reads there (`agent::host_path`). For a
    /// host whose login shell cannot be run non-interactively. Absolute or
    /// `~`-rooted; anything else is ignored.
    #[serde(default)]
    pub path_prepend: Vec<String>,
}

fn default_share_sessions() -> bool {
    true
}

// By hand rather than derived: a derived `Default` would make `share_sessions`
// `false`, and every `HostDef { name, .. Default::default() }` — the tests, the
// WSL constructor — would silently opt its host out of sharing.
impl Default for HostDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: HostKind::default(),
            destination: String::new(),
            distro: None,
            socket: None,
            session: None,
            ssh_opts: Vec::new(),
            worktrees_dir: None,
            multiplexer: None,
            platform: None,
            share_sessions: true,
            path_prepend: Vec::new(),
        }
    }
}

impl HostDef {
    /// Construct an auto-discovered WSL host for `distro` with all defaults.
    pub fn wsl(distro: impl Into<String>) -> Self {
        let distro = distro.into();
        Self {
            name: distro.clone(),
            kind: HostKind::Wsl,
            distro: Some(distro),
            share_sessions: true,
            ..Self::default()
        }
    }

    /// Whether sessions on this host are shared through its own database
    /// ([`share_sessions`](Self::share_sessions)).
    pub fn shareable(&self) -> bool {
        self.share_sessions
    }

    /// Whether this host is a WSL distro.
    pub fn is_wsl(&self) -> bool {
        self.kind == HostKind::Wsl
    }

    /// The WSL distro name (the explicit `distro` field, else the host `name`).
    /// Only meaningful for [`HostKind::Wsl`].
    pub fn distro_name(&self) -> String {
        self.distro.clone().unwrap_or_else(|| self.name.clone())
    }

    /// Whether this host is a **loopback**: the WSL distro thurbox is itself
    /// running inside.
    ///
    /// `wsl.exe -d <us>` from inside `<us>` lands back on this same machine —
    /// the same tmux server, the same worktrees, the same thurbox database — so
    /// such a host is not off-local at all, and registering one made every
    /// LOCAL session on this machine remote. A shareable host's own database is
    /// the record of the sessions on it (ADR-24), and here that database *is*
    /// ours: the mirror pass read our own rows back and rewrote each one's
    /// `backend_type` to `wsl:<us>`, after which every attach, diff and delete
    /// went out through `wsl.exe` and failed against the machine it started on.
    ///
    /// So a loopback is dropped at the registry — the one chokepoint every
    /// caller shares — rather than guarded for at each use. A *sibling* distro
    /// stays an ordinary host: reaching one from inside another is supported
    /// (see `shell::wsl_command`), and only self-reference is the bug.
    pub fn is_wsl_loopback(&self) -> bool {
        self.is_wsl() && is_current_wsl_distro(&self.distro_name())
    }

    /// Whether this host would **register under the loopback's backend name**
    /// (`wsl:<us>`) while pointing `wsl.exe` at some other distro.
    ///
    /// A host is registered — and persisted in `sessions.backend_type` — under
    /// its [`name`](Self::name), not its [`distro`](Self::distro). So
    /// `name = "<us>"` with `distro = "<a sibling>"` is a working remote host
    /// that writes rows spelled exactly like the ones the loopback bug wrote.
    /// It is left **exactly as written** — nothing about it is wrong, and it is
    /// the only outcome that never acts on the wrong machine.
    ///
    /// What that costs is the one-time repair, and only for that spelling: the
    /// rows under it are two populations at once — local rows the bug
    /// relabelled before the entry existed, and this host's own sibling rows
    /// written after — and nothing in the database tells them apart.
    /// Relabelling them all local would send the sibling's sessions at this
    /// machine; moving them all onto the sibling would send this machine's at
    /// the sibling. So the repair skips the name entirely and any mislabelled
    /// local row under it stays mislabelled. That residue is the pre-existing
    /// corruption left unhealed, not damage the repair does.
    ///
    /// This predicate is only the *warning*: what withholds the name is the
    /// general rule that a candidate claimed by a host the registry serves
    /// goes to [`WslRepairPlan::withheld`], and such an entry claims
    /// `wsl:<us>` like any other host claims its own backend name.
    pub fn shadows_current_wsl_distro(&self) -> bool {
        self.is_wsl() && !self.is_wsl_loopback() && is_current_wsl_distro(&self.name)
    }

    /// The key this host's machine goes by, multiplexer unsaid: `ssh:<name>`
    /// or `wsl:<name>` — what the host picker carries and what a row written
    /// before routes named their multiplexer says.
    pub fn backend_name(&self) -> String {
        self.route(None).format()
    }

    /// A short detail string for the host picker (the SSH destination, or
    /// `WSL` for a distro).
    pub fn picker_detail(&self) -> String {
        match self.kind {
            HostKind::Ssh => self.destination.clone(),
            HostKind::Wsl => "WSL".to_string(),
        }
    }

    /// The multiplexer this host's entry prefers, when it names one thurbox
    /// knows.
    pub fn multiplexer(&self) -> Option<Multiplexer> {
        self.multiplexer
            .as_deref()
            .and_then(|name| Multiplexer::parse(name).ok())
    }

    /// The host's multiplexer binary (`tmux` unless overridden).
    pub fn mux(&self) -> String {
        self.multiplexer
            .clone()
            .unwrap_or_else(|| "tmux".to_string())
    }

    /// The host's operating system — what decides its shell and path
    /// semantics, whichever multiplexer serves it and however it is reached.
    ///
    /// A WSL distro is Linux inside, always. Otherwise the entry's own
    /// [`platform`](Self::platform) field, and for an entry that names none,
    /// the meaning such an entry has always had: `multiplexer = "psmux"` was
    /// the declaration that a host is Windows before a platform could be
    /// written down, so it still is one, and anything else is POSIX. That is a
    /// reading of old configuration, not a rule: a Windows host on another
    /// multiplexer says `platform = "windows"`, and nothing reads a platform
    /// off a route or a multiplexer's name.
    pub fn platform(&self) -> Platform {
        if self.is_wsl() {
            return Platform::Posix;
        }
        self.platform.unwrap_or(match self.multiplexer() {
            Some(Multiplexer::Psmux) => Platform::Windows,
            _ => Platform::Posix,
        })
    }

    /// Whether this host is **native Windows** — no POSIX shell, `\` paths,
    /// PowerShell rather than `sh` ([`platform`](Self::platform)).
    pub fn is_windows(&self) -> bool {
        self.platform() == Platform::Windows
    }

    /// This entry as the adapter serving `mux` on it is told: `multiplexer`
    /// set to that binary, and [`platform`](Self::platform) pinned first, so
    /// a legacy entry that is Windows *because* it names psmux stays Windows
    /// when one of its rows is driven with another multiplexer.
    pub fn served_by(&self, mux: Multiplexer) -> Self {
        Self {
            platform: Some(self.platform()),
            multiplexer: Some(mux.name().to_string()),
            ..self.clone()
        }
    }

    /// The route to this host served by `mux` — or unqualified, the way a row
    /// written before routes named their multiplexer reads.
    pub fn route(&self, mux: Option<Multiplexer>) -> Route {
        let via = if self.is_wsl() { Via::Wsl } else { Via::Ssh };
        Route::remote(via, self.name.clone(), mux)
    }
}

/// All configured remote hosts, in declaration order.
///
/// Unknown fields are tolerated but reported: the loader names every
/// unrecognized key in a startup warning.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRegistry {
    /// Config-format version, for future migrations. Currently `1`.
    #[serde(default)]
    pub config_version: Option<u32>,
    #[serde(default)]
    pub hosts: Vec<HostDef>,
}

impl HostRegistry {
    /// Look up a host by its bare name (not the `ssh:` backend name).
    pub fn get(&self, name: &str) -> Option<&HostDef> {
        self.hosts.iter().find(|h| h.name == name)
    }

    /// The host a route runs on — `None` for a local route, or one naming a
    /// host this registry does not describe. The one resolver from a persisted
    /// `backend_type` to its host: the host comes back exactly as configured,
    /// whatever multiplexer the route names, so nothing downstream reads a
    /// platform off a route.
    ///
    /// The route's `ssh:`/`wsl:` prefix is **not** checked against the host's
    /// own [`kind`](HostDef::kind): `ssh:ubuntu` finds a host named `ubuntu`
    /// even if it is a WSL distro. That is deliberate — a name is unique across
    /// kinds (`host_config::load_all` dedupes by it), and the only way the two
    /// disagree is a persisted `backend_type` written before the host's kind
    /// was changed, where resolving to the host that now carries that name is
    /// what lets the session re-adopt instead of going unreachable.
    pub fn host_of(&self, route: &Route) -> Option<&HostDef> {
        self.get(route.host()?)
    }

    /// `route` with its multiplexer settled against this machine and the host
    /// it names — [`Route::qualify`] with the inputs a running thurbox has.
    pub fn qualify(&self, route: &Route) -> Route {
        route.qualify(
            Multiplexer::platform_default(),
            self.host_of(route).and_then(HostDef::multiplexer),
        )
    }

    /// Look up a host by **either** spelling: a route (`ssh:devbox`,
    /// `ssh:devbox:tmux`) or the bare name.
    ///
    /// The two spellings both circulate as "the host" and which one a caller
    /// holds depends on where it came from — a session row and the interface's
    /// host picker carry the backend name, `hosts.toml` and `--host` carry the
    /// bare one. Resolving only one of them is the mistake this exists to stop:
    /// the new-session flow handed `spawn` a backend name and every remote
    /// creation failed with "Unknown host 'ssh:devbox'".
    pub fn resolve(&self, key: &str) -> Option<&HostDef> {
        match Route::parse(key) {
            Ok(route) if route.is_remote() => self.host_of(&route),
            _ => self.get(key),
        }
    }

    /// Drop every entry whose name holds a `:`, returning a warning for each.
    ///
    /// A route is `ssh:<name>[:<multiplexer>]`, so `box:rmux` as a name would
    /// be the host `box` on rmux as far as every row is concerned. Refused at
    /// load rather than disambiguated at each read: a parse that guessed
    /// would be the silent kind of wrong.
    pub fn refuse_unroutable_names(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        self.hosts.retain(|host| {
            let routable = !host.name.contains(':');
            if !routable {
                warnings.push(format!(
                    "hosts.toml: host '{}' is ignored: a host name may not contain ':', \
                     which separates it from the multiplexer in a session's route",
                    host.name
                ));
            }
            routable
        });
        warnings
    }

    /// All host names in declaration order.
    pub fn names(&self) -> Vec<&str> {
        self.hosts.iter().map(|h| h.name.as_str()).collect()
    }

    /// Whether any remote hosts are configured.
    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }
}

/// Run `f` with [`WSL_DISTRO_NAME_VAR`] set to `distro` (or unset, for
/// `None`), restoring what was there before.
///
/// Serialized on a process-wide lock, and the **only** way a test may set it:
/// it is process state, and under plain `cargo test` the unit tests that need
/// it run concurrently in one process, where interleaved writes make one test
/// observe another's distro. (`nextest`, the repo's gate, gives each test its
/// own process — this is what keeps the other entry point honest.) Mirrors
/// `paths::with_path`, which `session` may not reach.
#[cfg(test)]
pub(crate) fn with_wsl_distro<T>(distro: Option<&str>, f: impl FnOnce() -> T) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var_os(WSL_DISTRO_NAME_VAR);
    match distro {
        Some(d) => std::env::set_var(WSL_DISTRO_NAME_VAR, d),
        None => std::env::remove_var(WSL_DISTRO_NAME_VAR),
    }
    let out = f();
    match saved {
        Some(v) => std::env::set_var(WSL_DISTRO_NAME_VAR, v),
        None => std::env::remove_var(WSL_DISTRO_NAME_VAR),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> HostRegistry {
        HostRegistry {
            config_version: None,
            hosts: vec![
                HostDef {
                    name: "devbox".into(),
                    destination: "me@devbox".into(),
                    ..Default::default()
                },
                HostDef::wsl("Ubuntu"),
                HostDef {
                    name: "winbox".into(),
                    destination: "me@winbox".into(),
                    multiplexer: Some("psmux".into()),
                    ..Default::default()
                },
            ],
        }
    }

    #[test]
    fn sharing_is_on_unless_the_entry_says_otherwise() {
        let on: HostDef = toml::from_str("name = \"devbox\"\ndestination = \"me@devbox\"").unwrap();
        assert!(on.shareable());
        assert!(HostDef::default().shareable());
        assert!(HostDef::wsl("Ubuntu").shareable());
        let off: HostDef = toml::from_str(
            "name = \"devbox\"\ndestination = \"me@devbox\"\nshare_sessions = false",
        )
        .unwrap();
        assert!(!off.shareable());
    }

    #[test]
    fn resolve_accepts_both_spellings_of_a_host() {
        // Which spelling a caller holds depends on where it came from, and
        // resolving only one of them refused every session the new-session flow
        // tried to create on a host ("Unknown host 'ssh:devbox'").
        let hosts = registry();
        for key in ["devbox", "ssh:devbox"] {
            assert_eq!(
                hosts.resolve(key).map(|h| h.name.as_str()),
                Some("devbox"),
                "{key} should resolve"
            );
        }
        for key in ["Ubuntu", "wsl:Ubuntu"] {
            assert_eq!(
                hosts.resolve(key).map(|h| h.name.as_str()),
                Some("Ubuntu"),
                "{key} should resolve"
            );
        }
    }

    #[test]
    fn resolve_does_not_invent_a_host() {
        let hosts = registry();
        for key in ["", "nope", "ssh:nope", "wsl:nope", "ssh:", "devbox2"] {
            assert!(hosts.resolve(key).is_none(), "{key} must not resolve");
        }
    }

    /// Which multiplexer a row names says nothing about the OS its host runs:
    /// with the host's configuration held fixed, resolving the row under every
    /// multiplexer, qualified or not, leaves the host's platform where the
    /// configuration put it. Reading the OS off the route would provision a
    /// Linux box with PowerShell because one row said `psmux`.
    #[test]
    fn a_rows_multiplexer_never_decides_its_hosts_platform() {
        for configured in [
            None,
            Some("tmux"),
            Some("psmux"),
            Some("rmux"),
            Some("herdr"),
        ] {
            let host = HostDef {
                name: "box".into(),
                multiplexer: configured.map(str::to_string),
                ..Default::default()
            };
            let hosts = HostRegistry {
                config_version: None,
                hosts: vec![host.clone()],
            };
            let keys = ["tmux", "psmux", "rmux", "herdr"]
                .map(|mux| format!("ssh:box:{mux}"))
                .into_iter()
                .chain(["ssh:box".to_string()]);
            for key in keys {
                let route = Route::parse(&key).expect("a route");
                let resolved = hosts.host_of(&route).expect("box resolves");
                assert_eq!(
                    resolved.is_windows(),
                    host.is_windows(),
                    "{key} against a host configured with {configured:?}"
                );
            }
        }
    }

    /// Every multiplexer, including the prospective rmux and herdr, can be
    /// named against every kind of host on either OS: route, host OS, the way
    /// the host is reached and the multiplexer are four choices, and making
    /// one leaves the other three where they were. Naming a multiplexer here
    /// is a route identity only — whether anything implements it is the
    /// registry's answer, and it is not asked here.
    #[test]
    fn route_host_os_launcher_and_multiplexer_are_independent() {
        use super::super::{BackendChoice, Platform};
        for kind in [HostKind::Ssh, HostKind::Wsl] {
            for platform in [None, Some(Platform::Posix), Some(Platform::Windows)] {
                for preference in std::iter::once(None).chain(Multiplexer::ALL.map(Some)) {
                    let host = HostDef {
                        name: "box".into(),
                        kind,
                        destination: "me@box".into(),
                        platform,
                        multiplexer: preference.map(|m| m.name().to_string()),
                        ..Default::default()
                    };
                    let os = match (kind, platform, preference) {
                        (HostKind::Wsl, _, _) => Platform::Posix,
                        (HostKind::Ssh, Some(os), _) => os,
                        (HostKind::Ssh, None, Some(Multiplexer::Psmux)) => Platform::Windows,
                        (HostKind::Ssh, None, _) => Platform::Posix,
                    };
                    let hosts = HostRegistry {
                        config_version: None,
                        hosts: vec![host.clone()],
                    };
                    for chosen in Multiplexer::ALL {
                        let case = format!("{kind:?}/{platform:?}/{preference:?} on {chosen:?}");
                        let choice =
                            BackendChoice::resolve(Some(host.clone()), Some(chosen.name()), None)
                                .unwrap();
                        assert_eq!(choice.multiplexer, chosen, "{case}");
                        assert_eq!(choice.route, host.route(Some(chosen)), "{case}");
                        let route = Route::parse(&choice.backend_type()).unwrap();
                        let via = match kind {
                            HostKind::Ssh => Via::Ssh,
                            HostKind::Wsl => Via::Wsl,
                        };
                        assert_eq!(route.via(), Some(via), "{case}");
                        assert_eq!(route.host(), Some("box"), "{case}");
                        assert_eq!(route.mux, Some(chosen), "{case}");
                        let resolved = hosts.host_of(&route).expect("box resolves");
                        assert_eq!(resolved.platform(), os, "{case}");
                        assert_eq!(hosts.qualify(&route), route, "{case}");
                    }
                }
            }
        }
    }

    /// This machine can name every multiplexer as well, whichever OS it is
    /// built for — no multiplexer's route is reserved to one platform.
    /// Simulated: `Platform::local`'s test override stands in for the build OS.
    #[test]
    fn every_multiplexer_is_a_local_route_on_either_os() {
        use super::super::{platform::simulate_local, BackendChoice, Platform};
        for os in Platform::ALL {
            for chosen in Multiplexer::ALL {
                let choice = simulate_local(os, || {
                    BackendChoice::resolve(None, Some(chosen.name()), None).unwrap()
                });
                assert_eq!(choice.route, Route::local(Some(chosen)), "{os:?}");
                assert_eq!(
                    choice.backend_type(),
                    format!("local:{}", chosen.name()),
                    "{os:?}"
                );
            }
        }
    }

    /// An entry written before a platform could be declared keeps the meaning
    /// it always had — `psmux` was the declaration of a Windows host — and a
    /// declared platform wins over that reading. A WSL distro is Linux inside
    /// whatever its entry says.
    #[test]
    fn a_legacy_entry_keeps_its_platform_and_a_declared_one_wins() {
        use super::super::Platform;
        let legacy: HostRegistry = toml::from_str(
            "[[hosts]]\nname = \"win\"\ndestination = \"me@win\"\nmultiplexer = \"psmux\"\n\n\
             [[hosts]]\nname = \"nix\"\ndestination = \"me@nix\"\n",
        )
        .unwrap();
        assert_eq!(legacy.get("win").unwrap().platform(), Platform::Windows);
        assert_eq!(legacy.get("nix").unwrap().platform(), Platform::Posix);

        let declared: HostRegistry = toml::from_str(
            "[[hosts]]\nname = \"win\"\ndestination = \"me@win\"\nplatform = \"windows\"\nmultiplexer = \"tmux\"\n\n\
             [[hosts]]\nname = \"odd\"\ndestination = \"me@odd\"\nplatform = \"posix\"\nmultiplexer = \"psmux\"\n\n\
             [[hosts]]\nname = \"distro\"\nkind = \"wsl\"\nplatform = \"windows\"\n",
        )
        .unwrap();
        assert_eq!(declared.get("win").unwrap().platform(), Platform::Windows);
        assert_eq!(declared.get("odd").unwrap().platform(), Platform::Posix);
        assert_eq!(declared.get("distro").unwrap().platform(), Platform::Posix);
        assert!(toml::from_str::<HostRegistry>(
            "[[hosts]]\nname = \"x\"\ndestination = \"d\"\nplatform = \"beos\"\n"
        )
        .is_err());
    }

    #[test]
    fn the_backend_prefix_is_not_checked_against_the_hosts_kind() {
        // A name is unique across kinds, so the prefix carries no information
        // the name does not — and the one case where they disagree is a
        // persisted `backend_type` written before the host's kind changed, where
        // matching on the name is what lets the session re-adopt rather than go
        // unreachable. Asserted so the leniency is a decision, not an accident.
        let hosts = registry();
        assert_eq!(
            hosts.resolve("ssh:Ubuntu").map(|h| h.kind),
            Some(HostKind::Wsl)
        );
        assert_eq!(
            hosts.resolve("wsl:devbox").map(|h| h.kind),
            Some(HostKind::Ssh)
        );
    }

    #[test]
    fn is_windows_is_declared_by_the_multiplexer_alone() {
        let hosts = registry();
        assert!(hosts.resolve("winbox").expect("winbox").is_windows());
        // Every other shape is POSIX: the default, an explicit `tmux`, and a
        // WSL distro (Linux inside, whatever machine hosts it).
        assert!(!hosts.resolve("devbox").expect("devbox").is_windows());
        assert!(!hosts.resolve("Ubuntu").expect("Ubuntu").is_windows());
        let explicit_tmux = HostDef {
            name: "box".into(),
            multiplexer: Some("tmux".into()),
            ..Default::default()
        };
        assert!(!explicit_tmux.is_windows());
    }

    #[test]
    fn backend_name_prefixes_with_ssh() {
        let h = HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        };
        assert_eq!(h.backend_name(), "ssh:devbox");
    }

    #[test]
    fn wsl_constructor_and_backend_name() {
        let h = HostDef::wsl("Ubuntu");
        assert!(h.is_wsl());
        assert_eq!(h.kind, HostKind::Wsl);
        assert_eq!(h.distro_name(), "Ubuntu");
        assert_eq!(h.backend_name(), "wsl:Ubuntu");
        assert_eq!(h.picker_detail(), "WSL");
        // WSL distros run `tmux` inside the distro, same as a Unix SSH host.
        assert_eq!(h.mux(), "tmux");

        // An SSH host (the default kind) keeps the ssh prefix and is not WSL.
        let ssh = HostDef {
            name: "devbox".into(),
            destination: "me@devbox".into(),
            ..Default::default()
        };
        assert!(!ssh.is_wsl());
        assert_eq!(ssh.picker_detail(), "me@devbox");
    }

    #[test]
    fn distro_name_falls_back_to_host_name() {
        let h = HostDef {
            name: "work".into(),
            kind: HostKind::Wsl,
            distro: None,
            ..Default::default()
        };
        assert_eq!(h.distro_name(), "work");
    }

    #[test]
    fn registry_lookup_by_name_and_backend() {
        let reg = HostRegistry {
            config_version: None,
            hosts: vec![
                HostDef {
                    name: "devbox".into(),
                    destination: "me@devbox".into(),
                    ..Default::default()
                },
                HostDef::wsl("Ubuntu"),
            ],
        };
        assert_eq!(reg.get("devbox").unwrap().destination, "me@devbox");
        let host_of = |key: &str| {
            reg.host_of(&Route::parse(key).expect("a route"))
                .map(|h| h.name.as_str())
        };
        assert_eq!(host_of("ssh:devbox"), Some("devbox"));
        assert_eq!(host_of("ssh:devbox:psmux"), Some("devbox"));
        assert_eq!(host_of("wsl:Ubuntu"), Some("Ubuntu"));
        assert_eq!(host_of("local-tmux"), None);
        assert_eq!(host_of("ssh:nope"), None);
    }

    #[test]
    fn a_recorded_multiplexer_survives_a_changed_host_preference() {
        let registry = HostRegistry {
            config_version: None,
            hosts: vec![HostDef {
                name: "example".into(),
                multiplexer: Some("rmux".into()),
                ..Default::default()
            }],
        };
        let qualified = |key: &str| registry.qualify(&Route::parse(key).unwrap()).mux;
        assert_eq!(qualified("ssh:example"), Some(Multiplexer::Tmux));
        assert_eq!(qualified("ssh:example:rmux"), Some(Multiplexer::Rmux));
        assert_eq!(qualified("ssh:example:psmux"), Some(Multiplexer::Psmux));
        // The host comes back as configured, whichever route found it.
        let host = registry
            .host_of(&Route::parse("ssh:example:psmux").unwrap())
            .unwrap();
        assert_eq!(host.multiplexer(), Some(Multiplexer::Rmux));
    }

    #[test]
    fn a_host_name_holding_a_colon_is_refused_at_load() {
        let mut registry: HostRegistry =
            toml::from_str("[[hosts]]\nname = \"example:rmux\"\n\n[[hosts]]\nname = \"example\"\n")
                .unwrap();
        let warnings = registry.refuse_unroutable_names();
        assert_eq!(registry.names(), ["example"]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("'example:rmux'"), "{}", warnings[0]);
    }

    #[test]
    fn parses_wsl_host_from_toml() {
        let toml = r#"
[[hosts]]
name = "Ubuntu"
kind = "wsl"

[[hosts]]
name = "custom"
kind = "wsl"
distro = "Debian"
worktrees_dir = "/home/me/wt"
"#;
        let reg: HostRegistry = toml::from_str(toml).unwrap();
        let u = reg.get("Ubuntu").unwrap();
        assert!(u.is_wsl());
        assert_eq!(u.distro_name(), "Ubuntu");
        assert_eq!(u.backend_name(), "wsl:Ubuntu");
        let c = reg.get("custom").unwrap();
        assert_eq!(c.distro_name(), "Debian");
        assert_eq!(c.worktrees_dir.as_deref(), Some("/home/me/wt"));
    }

    #[test]
    fn parses_minimal_and_full_toml() {
        let toml = r#"
[[hosts]]
name = "minimal"
destination = "host1"

[[hosts]]
name = "full"
destination = "me@host2"
socket = "tb2"
session = "tb2"
ssh_opts = ["-o", "ControlMaster=auto"]
worktrees_dir = "/home/me/wt"
"#;
        let reg: HostRegistry = toml::from_str(toml).unwrap();
        assert_eq!(reg.hosts.len(), 2);
        let minimal = reg.get("minimal").unwrap();
        assert_eq!(minimal.destination, "host1");
        assert!(minimal.socket.is_none());
        assert!(minimal.ssh_opts.is_empty());
        let full = reg.get("full").unwrap();
        assert_eq!(full.socket.as_deref(), Some("tb2"));
        assert_eq!(full.ssh_opts, ["-o", "ControlMaster=auto"]);
        assert_eq!(full.worktrees_dir.as_deref(), Some("/home/me/wt"));
    }

    #[test]
    fn the_distro_we_run_in_is_a_loopback_and_its_siblings_are_not() {
        with_wsl_distro(Some("Ubuntu"), || {
            assert!(HostDef::wsl("Ubuntu").is_wsl_loopback());
            // wsl.exe matches a distro name case-insensitively; so does this.
            assert!(HostDef::wsl("ubuntu").is_wsl_loopback());
            assert!(!HostDef::wsl("Debian").is_wsl_loopback());
            // A prefix is a different distro, not us.
            assert!(!HostDef::wsl("Ubuntu-22.04").is_wsl_loopback());
            // The `distro` field is what wsl.exe is handed, so it — not the
            // host's own name — decides.
            assert!(HostDef {
                name: "work".into(),
                kind: HostKind::Wsl,
                distro: Some("Ubuntu".into()),
                ..Default::default()
            }
            .is_wsl_loopback());
            // An SSH host is never one, whatever it is called.
            assert!(!HostDef {
                name: "Ubuntu".into(),
                destination: "me@ubuntu".into(),
                ..Default::default()
            }
            .is_wsl_loopback());
        });
    }

    #[test]
    fn a_host_named_after_our_distro_shadows_its_backend_name() {
        with_wsl_distro(Some("Ubuntu"), || {
            // Reaches a real sibling, but would register as `wsl:Ubuntu` —
            // the very spelling the loopback repair reads as "this machine".
            let shadow = HostDef {
                name: "Ubuntu".into(),
                kind: HostKind::Wsl,
                distro: Some("Debian".into()),
                ..Default::default()
            };
            assert!(shadow.shadows_current_wsl_distro());
            assert!(!shadow.is_wsl_loopback());
            assert_eq!(shadow.backend_name(), "wsl:Ubuntu");

            // A true loopback is not also a shadow: the two are reported
            // separately so each gets the warning that fits it.
            assert!(!HostDef::wsl("Ubuntu").shadows_current_wsl_distro());
            // Any other name, and the predicate does not hold.
            assert!(!HostDef {
                name: "work".into(),
                kind: HostKind::Wsl,
                distro: Some("Debian".into()),
                ..Default::default()
            }
            .shadows_current_wsl_distro());
            // An SSH host named after the distro collides with nothing: it
            // registers as `ssh:Ubuntu`.
            assert!(!HostDef {
                name: "Ubuntu".into(),
                destination: "me@ubuntu".into(),
                ..Default::default()
            }
            .shadows_current_wsl_distro());
        });
    }

    #[test]
    fn off_wsl_nothing_is_a_loopback() {
        with_wsl_distro(None, || {
            assert_eq!(current_wsl_distro(), None);
            assert!(!HostDef::wsl("Ubuntu").is_wsl_loopback());
            assert!(!HostDef::wsl("Ubuntu").shadows_current_wsl_distro());
        });
        // A distro that set the variable empty is no distro at all.
        with_wsl_distro(Some("  "), || {
            assert_eq!(current_wsl_distro(), None);
            assert!(!HostDef::wsl("Ubuntu").is_wsl_loopback());
        });
    }
}
