//! Extension manifests — pure data describing the thurbox resources an opt-in
//! extension needs to function (a dedicated session, a tick automation, …).
//!
//! Extensions (see `extensions/<name>/`) are agent-agnostic add-ons built on
//! `thurbox-cli`; per ADR-20 they live as data + shell scripts, never embedded
//! in the binary. An extension ships an `extension.toml` manifest; its installer
//! copies it into the discovery dir (`~/.config/thurbox/extensions/<name>.toml`),
//! and thurbox core reads *any* manifest without knowing the extension by name.
//!
//! The manifest is the declarative contract behind `thurbox-cli extension
//! activate/deactivate` and the startup/tick self-heal: it names the
//! sessions/automations to (re)create idempotently. Kept here in `session` (the
//! dependency sink) so both `agent` (the loader) and `session_ops` (the
//! orchestration) can depend on the same type without crossing module-isolation
//! rules — exactly like [`crate::session::HostDef`].

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::AgentDef;

/// Token replaced with the resolved (absolute) extension home directory wherever
/// it appears in a manifest (session `repo_path`, file contents marked
/// `substitute`). The only template token the installer understands.
pub const HOME_TOKEN: &str = "{home}";

/// A file the installer lays down under the extension home directory. The
/// content comes from the install source (`<source>/<source_path>`); only
/// `path` is required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionFile {
    /// Destination path, relative to the extension home dir.
    pub path: String,
    /// Source path relative to the install source, when it differs from `path`
    /// (e.g. a `claude-settings.json` template installed as `.claude/settings.json`).
    #[serde(default)]
    pub source: Option<String>,
    /// Mark the written file executable (`chmod +x`). For helper scripts.
    #[serde(default)]
    pub executable: bool,
    /// Only write when the destination is absent (seed files like `repos.md`
    /// the user then edits — never clobbered on reinstall).
    #[serde(default)]
    pub if_absent: bool,
    /// Replace the [`HOME_TOKEN`] in the content with the resolved home path
    /// before writing (e.g. claude permission paths).
    #[serde(default)]
    pub substitute: bool,
}

impl ExtensionFile {
    /// Source-relative path to fetch this file's content from.
    pub fn source_path(&self) -> &str {
        self.source.as_deref().unwrap_or(&self.path)
    }
}

/// A file the installer places **outside** the extension home — into an agent's
/// own config dir (e.g. `~/.config/opencode/plugin/thurbox-status.js`). `path`
/// may be absolute, start with `~`, or contain [`HOME_TOKEN`]. Unlike
/// [`ExtensionFile`] (home-confined), this is how a hook plugin reaches an agent
/// that has no launch flag. Removed on uninstall when still thurbox-managed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalFile {
    /// Destination path: absolute, `~`-relative, or containing `{home}`.
    pub path: String,
    /// Source path relative to the install source (defaults to `path`'s file name).
    #[serde(default)]
    pub source: Option<String>,
    /// Mark the written file executable (`chmod +x`).
    #[serde(default)]
    pub executable: bool,
    /// Only write when the destination is absent (don't clobber a user file).
    #[serde(default)]
    pub if_absent: bool,
    /// Replace [`HOME_TOKEN`] in the content before writing.
    #[serde(default)]
    pub substitute: bool,
    /// Only write when this directory exists (absolute / `~`). Guards against
    /// creating an agent's config tree for an agent the user hasn't installed —
    /// e.g. `~/.config/opencode` for the opencode plugin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_dir: Option<String>,
}

/// Source-relative path for a payload whose `source` defaults to the
/// destination's *file name* (used by [`ExternalFile`] and [`ConfigMerge`],
/// which write outside the home dir to an absolute/`~` destination).
fn source_or_dest_filename<'a>(source: &'a Option<String>, dest: &'a str) -> &'a str {
    source.as_deref().unwrap_or_else(|| {
        std::path::Path::new(dest)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(dest)
    })
}

impl ExternalFile {
    /// Source-relative path to fetch this file's content from (defaults to the
    /// destination's file name when `source` is unset).
    pub fn source_path(&self) -> &str {
        source_or_dest_filename(&self.source, &self.path)
    }
}

/// An append-args patch applied to an **existing** agent in `agents.toml` — one
/// the extension does not own (e.g. the built-in `claude`). Unlike `[[agents]]`
/// (which only *adds* new agents), this injects `append_args` into the named
/// agent's `args`, reversibly: uninstall removes exactly this subsequence.
/// [`HOME_TOKEN`] is substituted in `append_args`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentPatch {
    /// Name of the existing agent whose `args` to extend.
    pub name: String,
    /// Args appended to the agent's `args` (idempotent; removed on uninstall).
    #[serde(default)]
    pub append_args: Vec<String>,
}

/// A **non-destructive, reversible JSON merge** into a config file an agent owns
/// (e.g. `~/.gemini/settings.json`). Unlike [`ExternalFile`] (which writes a
/// whole file and would clobber the user's config), this deep-merges the shipped
/// `source` JSON into the target in place: objects recurse, arrays union by
/// deep-equality, and uninstall removes exactly our entries (see
/// `agent::json_merge`). For agents whose hooks live in a *shared* config file
/// that has no drop-in plugin location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigMerge {
    /// Target config file: absolute, `~`-relative, or containing [`HOME_TOKEN`].
    pub path: String,
    /// Source path (relative to the install source) of the JSON to merge in;
    /// defaults to `path`'s file name.
    #[serde(default)]
    pub source: Option<String>,
    /// Only merge when this directory exists (absolute / `~`) — skips the merge
    /// when the agent isn't installed (e.g. `~/.gemini`), mirroring
    /// [`ExternalFile::requires_dir`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_dir: Option<String>,
}

impl ConfigMerge {
    /// Source-relative path to read the JSON-to-merge from (defaults to the
    /// destination's file name).
    pub fn source_path(&self) -> &str {
        source_or_dest_filename(&self.source, &self.path)
    }
}

/// A symlink the installer creates under the extension home directory. Used to
/// surface a spec file under each agent CLI's context-file name
/// (`CLAUDE.md`/`AGENTS.md`/`GEMINI.md` → `FLOW.md`). An existing *regular* file
/// at `link` is never clobbered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionSymlink {
    /// Link path, relative to the extension home dir.
    pub link: String,
    /// Link target (relative to the link's directory).
    pub target: String,
}

/// A session an extension wants kept alive. Identified by `name`, so an existing
/// active session of that name is reused rather than duplicated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionSession {
    /// Session name (also the tmux window `tb-<name>`). Used to find/reuse it.
    pub name: String,
    /// Agent name to launch (an `agents.toml` entry the installer registers).
    pub agent: String,
    /// Directory the agent runs in (absolute; the installer resolves any `~`).
    pub repo_path: PathBuf,
}

/// An automation an extension wants kept alive. Identified by `name`.
///
/// Two flavours: a **send** automation prompts an extension session
/// (`session_ref` + `prompt`), or an **exec** automation runs a shell `command`
/// headlessly (no session — a deterministic scheduled job). Set exactly one of
/// `command` or (`session_ref` + `prompt`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionAutomation {
    /// Automation name. Used to find/reuse it.
    pub name: String,
    /// Trigger spec, same grammar as `thurbox-cli automation create --trigger`
    /// (`hourly` | `daily` | `weekdays` | `weekly` | `cron:<expr>` | `at:<ms>`).
    pub trigger: String,
    /// Name of the extension session this automation sends its prompt to. Must
    /// match one of the manifest's `[[sessions]]` entries. Omitted for `command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ref: Option<String>,
    /// Prompt text delivered on each fire (e.g. `tick`). Omitted for `command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Shell command run headlessly on each fire (the `Exec` action). Mutually
    /// exclusive with `session_ref`/`prompt`; `{home}` is substituted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl ExtensionAutomation {
    /// Reject the invalid flavour combinations the 3-`Option` shape allows but
    /// the `send`-xor-`exec` model forbids: setting `command` *and* a send field
    /// (downstream `command` silently wins, dropping the send fields), or
    /// setting neither. Mirrors [`crate::session::message::validate_kind_body`];
    /// called where the manifest is turned into resources
    /// ([`crate::session_ops::ensure_extension`]).
    pub fn validate(&self) -> Result<(), String> {
        let has_send_fields = self.session_ref.is_some() || self.prompt.is_some();
        if self.command.is_some() && has_send_fields {
            return Err(format!(
                "automation '{}' sets both `command` (exec) and send fields \
                 (`session_ref`/`prompt`); set exactly one flavour",
                self.name
            ));
        }
        if self.command.is_none() && self.session_ref.is_none() {
            return Err(format!(
                "automation '{}' has neither a `command` nor a `session_ref`",
                self.name
            ));
        }
        Ok(())
    }
}

/// A full extension manifest: the resources to ensure when the extension is
/// active. One manifest per `extension.toml` file.
///
/// Unknown fields are tolerated but reported by the loader as a warning, so a
/// newer manifest doesn't strand an older thurbox on defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionDef {
    /// Unique extension name (matches the discovery file stem and what
    /// `thurbox-cli extension activate <name>` expects).
    pub name: String,
    /// Optional human-readable summary, shown in `extension list`.
    #[serde(default)]
    pub description: Option<String>,
    /// Manifest-format version, for future migrations. Currently `1`.
    #[serde(default)]
    pub config_version: Option<u32>,
    /// The extension's own semantic version (e.g. `"1.2.0"`), authored in the
    /// source `extension.toml` and bumped by the extension's maintainer. Lets
    /// `extension update` report what moved and surfaces in `extension list`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Minimum thurbox version this extension needs (e.g. `"0.113.0"`). Install
    /// and activate emit a compatibility **warning** (never a hard block, to
    /// stay graceful) when the running binary is older. Dev builds skip it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_thurbox_version: Option<String>,
    /// The thurbox version that performed the install. **Stamped** into the
    /// discovery-dir copy by the installer (never authored in source); compared
    /// against the running binary to flag a stale extension after a thurbox
    /// upgrade. `None` in a source manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_with: Option<String>,
    /// The resolved install target (a bare name, `http(s)://` URL, or local
    /// path) the extension was installed from. **Stamped** into the discovery
    /// copy so `extension update` can re-fetch from the same place. `None` in a
    /// source manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Default install home directory (may use `~`), where payload files land
    /// and the session runs. The `--home` flag overrides it. Required to
    /// install; unused once installed.
    #[serde(default)]
    pub home: Option<String>,
    /// Agents to register in `agents.toml` on install (idempotent — existing
    /// names are left untouched). Install-time only.
    #[serde(default)]
    pub agents: Vec<AgentDef>,
    /// Payload files the installer lays down under the home dir. Install-time only.
    #[serde(default)]
    pub files: Vec<ExtensionFile>,
    /// Files the installer places **outside** the home dir, into agents' own
    /// config dirs (hook plugins, etc.). Install-time only.
    #[serde(default)]
    pub external_files: Vec<ExternalFile>,
    /// Append-args patches applied to existing agents in `agents.toml`
    /// (reversible). Install-time only.
    #[serde(default)]
    pub agent_patches: Vec<AgentPatch>,
    /// Reversible JSON merges into agents' own config files (hooks into a shared
    /// `settings.json`/`hooks.json`). Install-time only.
    #[serde(default)]
    pub config_merges: Vec<ConfigMerge>,
    /// Symlinks the installer creates under the home dir. Install-time only.
    #[serde(default)]
    pub symlinks: Vec<ExtensionSymlink>,
    /// Sessions to ensure exist while the extension is active.
    #[serde(default)]
    pub sessions: Vec<ExtensionSession>,
    /// Automations to ensure exist while the extension is active.
    #[serde(default)]
    pub automations: Vec<ExtensionAutomation>,
}

impl ExtensionDef {
    /// Whether the manifest declares no runtime resources (nothing to ensure).
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty() && self.automations.is_empty()
    }

    /// Return a copy with the [`HOME_TOKEN`] resolved to `home` in every session
    /// `repo_path`, and with `home` itself stored as the resolved absolute path.
    /// This is what gets written to the discovery dir at install time, so
    /// `activate`/self-heal read absolute paths (they don't expand tokens
    /// themselves) and uninstall knows the real home directory.
    pub fn resolved_for_home(&self, home: &str) -> ExtensionDef {
        let mut out = self.clone();
        out.home = Some(home.to_string());
        for s in &mut out.sessions {
            let p = s.repo_path.to_string_lossy().replace(HOME_TOKEN, home);
            s.repo_path = PathBuf::from(p);
        }
        // External-file destinations and patched agent args may reference the
        // home dir (e.g. `--settings {home}/hooks/claude.json`); resolve them so
        // activate/heal/uninstall read absolute paths without expanding tokens.
        for f in &mut out.external_files {
            f.path = f.path.replace(HOME_TOKEN, home);
            if let Some(req) = &f.requires_dir {
                f.requires_dir = Some(req.replace(HOME_TOKEN, home));
            }
        }
        for p in &mut out.agent_patches {
            for arg in &mut p.append_args {
                *arg = arg.replace(HOME_TOKEN, home);
            }
        }
        for m in &mut out.config_merges {
            m.path = m.path.replace(HOME_TOKEN, home);
            if let Some(req) = &m.requires_dir {
                m.requires_dir = Some(req.replace(HOME_TOKEN, home));
            }
        }
        // An exec automation's command typically calls a script under the home
        // dir (e.g. `{home}/sync.sh`); resolve it to an absolute path.
        for a in &mut out.automations {
            if let Some(cmd) = &a.command {
                a.command = Some(cmd.replace(HOME_TOKEN, home));
            }
        }
        out
    }

    /// Stamp install provenance onto the (resolved) manifest before it's written
    /// to the discovery dir: which thurbox version installed it and where it came
    /// from, so staleness can be detected and `update` can re-fetch. Returns
    /// `self` for chaining off [`Self::resolved_for_home`].
    pub fn with_provenance(mut self, installed_with: &str, source: &str) -> ExtensionDef {
        self.installed_with = Some(installed_with.to_string());
        self.source = Some(source.to_string());
        self
    }

    /// Whether this extension was installed under a thurbox version different
    /// from `current` — i.e. an upgrade has happened since and re-running
    /// `extension update` would refresh it. Always `false` for a dev build
    /// (`current` is unstable) or a manifest with no recorded install version.
    pub fn is_stale(&self, current: &str) -> bool {
        if is_dev_version(current) {
            return false;
        }
        match &self.installed_with {
            Some(installed) => installed != current,
            None => false,
        }
    }

    /// A compatibility warning if this extension declares a `min_thurbox_version`
    /// the running `current` binary doesn't satisfy, else `None`. Dev builds are
    /// treated as compatible with everything (their version is unstable).
    pub fn compat_warning(&self, current: &str) -> Option<String> {
        if is_dev_version(current) {
            return None;
        }
        let min = self.min_thurbox_version.as_deref()?;
        if compare_versions(current, min) == std::cmp::Ordering::Less {
            Some(format!(
                "extension '{}' wants thurbox >= {min} but this binary is {current}; \
                 some features may not work — upgrade thurbox",
                self.name
            ))
        } else {
            None
        }
    }
}

/// Whether a version string denotes an unstable dev build (`0.0.0-dev`, or any
/// version carrying a `-dev`/pre-release suffix). Such builds skip staleness and
/// compatibility checks because their version doesn't order against releases.
pub fn is_dev_version(v: &str) -> bool {
    v.contains("-dev") || v.trim_start_matches('v').starts_with("0.0.0")
}

/// Compare two dotted version strings (`a.b.c`, optional leading `v`, any
/// `-suffix` ignored) numerically, component by component. Missing trailing
/// components count as `0` (so `1.2` == `1.2.0`). Non-numeric components sort as
/// `0`. A dependency-free stand-in for the `semver` crate, sufficient for the
/// `major.minor.patch` tags thurbox ships.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parts = |s: &str| -> Vec<u64> {
        s.trim_start_matches('v')
            .split('-')
            .next()
            .unwrap_or("")
            .split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    };
    let (pa, pb) = (parts(a), parts(b));
    let n = pa.len().max(pb.len());
    for i in 0..n {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_manifest() {
        let toml = r#"
name = "flow"
description = "Focus-protecting triage agent"
config_version = 1

[[sessions]]
name = "flow"
agent = "flow"
repo_path = "/home/me/flow"

[[automations]]
name = "flow-tick"
trigger = "cron:*/5 * * * *"
session_ref = "flow"
prompt = "tick"
"#;
        let def: ExtensionDef = toml::from_str(toml).unwrap();
        assert_eq!(def.name, "flow");
        assert_eq!(
            def.description.as_deref(),
            Some("Focus-protecting triage agent")
        );
        assert_eq!(def.sessions.len(), 1);
        assert_eq!(def.sessions[0].agent, "flow");
        assert_eq!(def.sessions[0].repo_path, PathBuf::from("/home/me/flow"));
        assert_eq!(def.automations.len(), 1);
        assert_eq!(def.automations[0].session_ref.as_deref(), Some("flow"));
        assert_eq!(def.automations[0].prompt.as_deref(), Some("tick"));
        assert!(!def.is_empty());
    }

    #[test]
    fn automation_validate_rejects_invalid_flavours() {
        let auto = |session_ref: Option<&str>, prompt: Option<&str>, command: Option<&str>| {
            ExtensionAutomation {
                name: "a".into(),
                trigger: "hourly".into(),
                session_ref: session_ref.map(str::to_string),
                prompt: prompt.map(str::to_string),
                command: command.map(str::to_string),
            }
        };
        assert!(auto(Some("flow"), Some("tick"), None).validate().is_ok());
        assert!(auto(None, None, Some("sync.sh")).validate().is_ok());
        // Both flavours set: exec would silently win, dropping the send fields.
        assert!(auto(Some("flow"), Some("tick"), Some("sync.sh"))
            .validate()
            .is_err());
        assert!(auto(None, Some("tick"), Some("sync.sh"))
            .validate()
            .is_err());
        assert!(auto(None, None, None).validate().is_err());
        assert!(auto(None, Some("tick"), None).validate().is_err());
    }

    #[test]
    fn round_trips_through_toml() {
        let def = ExtensionDef {
            name: "flow".into(),
            description: None,
            config_version: Some(1),
            version: Some("1.0.0".into()),
            min_thurbox_version: Some("0.113.0".into()),
            installed_with: Some("0.113.0".into()),
            source: Some("flow".into()),
            home: Some("~/flow".into()),
            agents: vec![AgentDef {
                name: "flow".into(),
                command: "claude".into(),
                args: vec!["--model".into(), "claude-haiku-4-5".into()],
                resume_args: vec![],
                fork_args: vec![],
                new_session_args: vec![],
                resume_latest: false,
            }],
            files: vec![ExtensionFile {
                path: "FLOW.md".into(),
                source: None,
                executable: false,
                if_absent: false,
                substitute: false,
            }],
            external_files: vec![],
            agent_patches: vec![],
            config_merges: vec![],
            symlinks: vec![ExtensionSymlink {
                link: "CLAUDE.md".into(),
                target: "FLOW.md".into(),
            }],
            sessions: vec![ExtensionSession {
                name: "flow".into(),
                agent: "flow".into(),
                repo_path: PathBuf::from("{home}"),
            }],
            automations: vec![ExtensionAutomation {
                name: "flow-tick".into(),
                trigger: "cron:*/5 * * * *".into(),
                session_ref: Some("flow".into()),
                prompt: Some("tick".into()),
                command: None,
            }],
        };
        let text = toml::to_string(&def).unwrap();
        let back: ExtensionDef = toml::from_str(&text).unwrap();
        assert_eq!(def, back);
    }

    #[test]
    fn minimal_manifest_has_no_resources() {
        let def: ExtensionDef = toml::from_str("name = \"bare\"\n").unwrap();
        assert_eq!(def.name, "bare");
        assert!(def.is_empty());
        assert!(def.agents.is_empty());
        assert!(def.files.is_empty());
    }

    #[test]
    fn config_merge_source_path_defaults_to_dest_filename() {
        let explicit = ConfigMerge {
            path: "~/.gemini/settings.json".into(),
            source: Some("gemini-hooks.json".into()),
            requires_dir: None,
        };
        assert_eq!(explicit.source_path(), "gemini-hooks.json");
        // Unset source falls back to the destination's file name (not its full
        // path) — the shared `source_or_dest_filename` behavior.
        let defaulted = ConfigMerge {
            path: "~/.gemini/settings.json".into(),
            source: None,
            requires_dir: None,
        };
        assert_eq!(defaulted.source_path(), "settings.json");
    }

    #[test]
    fn resolved_for_home_substitutes_config_merge_paths() {
        let def: ExtensionDef = toml::from_str(
            "name = \"hooks\"\n[[config_merges]]\npath = \"{home}/settings.json\"\nrequires_dir = \"{home}\"\n",
        )
        .unwrap();
        let resolved = def.resolved_for_home("/home/me/.gemini");
        assert_eq!(
            resolved.config_merges[0].path,
            "/home/me/.gemini/settings.json"
        );
        assert_eq!(
            resolved.config_merges[0].requires_dir.as_deref(),
            Some("/home/me/.gemini")
        );
        // Original untouched.
        assert_eq!(def.config_merges[0].path, "{home}/settings.json");
    }

    #[test]
    fn resolved_for_home_substitutes_session_repo_path() {
        let def: ExtensionDef = toml::from_str(
            "name = \"flow\"\n[[sessions]]\nname = \"flow\"\nagent = \"flow\"\nrepo_path = \"{home}\"\n",
        )
        .unwrap();
        let resolved = def.resolved_for_home("/home/me/flow");
        assert_eq!(
            resolved.sessions[0].repo_path,
            PathBuf::from("/home/me/flow")
        );
        // Original is untouched.
        assert_eq!(def.sessions[0].repo_path, PathBuf::from("{home}"));
    }

    #[test]
    fn compare_versions_orders_numerically() {
        use std::cmp::Ordering;
        assert_eq!(compare_versions("0.113.0", "0.113.0"), Ordering::Equal);
        assert_eq!(compare_versions("0.114.0", "0.113.0"), Ordering::Greater);
        assert_eq!(compare_versions("0.113.0", "0.114.0"), Ordering::Less);
        // Numeric, not lexical: 0.20 > 0.9.
        assert_eq!(compare_versions("0.20.0", "0.9.0"), Ordering::Greater);
        // Leading `v` and missing trailing components are tolerated.
        assert_eq!(compare_versions("v1.2", "1.2.0"), Ordering::Equal);
        assert_eq!(compare_versions("2.0.0", "1.99.99"), Ordering::Greater);
        // Pre-release suffix is ignored for ordering.
        assert_eq!(compare_versions("0.113.0-rc1", "0.113.0"), Ordering::Equal);
    }

    #[test]
    fn is_dev_version_detects_unstable_builds() {
        assert!(is_dev_version("0.0.0-dev"));
        assert!(is_dev_version("0.113.0-dev"));
        assert!(is_dev_version("0.0.0"));
        assert!(!is_dev_version("0.113.0"));
        assert!(!is_dev_version("v1.2.3"));
    }

    #[test]
    fn is_stale_compares_install_version_to_current() {
        let mut def = ExtensionDef {
            name: "flow".into(),
            installed_with: Some("0.113.0".into()),
            ..Default::default()
        };
        assert!(def.is_stale("0.114.0"), "binary upgraded → stale");
        assert!(!def.is_stale("0.113.0"), "same version → fresh");
        // Dev binary never flags staleness (its version is unstable).
        assert!(!def.is_stale("0.0.0-dev"));
        // No recorded install version → can't be stale.
        def.installed_with = None;
        assert!(!def.is_stale("0.114.0"));
    }

    #[test]
    fn compat_warning_fires_only_when_binary_too_old() {
        let def = ExtensionDef {
            name: "flow".into(),
            min_thurbox_version: Some("0.113.0".into()),
            ..Default::default()
        };
        assert!(
            def.compat_warning("0.112.0").is_some(),
            "older binary warns"
        );
        assert!(def.compat_warning("0.113.0").is_none(), "exact match ok");
        assert!(def.compat_warning("0.200.0").is_none(), "newer binary ok");
        // Dev builds are treated as compatible with everything.
        assert!(def.compat_warning("0.0.0-dev").is_none());
        // No declared minimum → never warns.
        let bare = ExtensionDef {
            name: "x".into(),
            ..Default::default()
        };
        assert!(bare.compat_warning("0.1.0").is_none());
    }

    #[test]
    fn with_provenance_stamps_install_metadata() {
        let def = ExtensionDef {
            name: "flow".into(),
            ..Default::default()
        }
        .with_provenance("0.113.0", "flow");
        assert_eq!(def.installed_with.as_deref(), Some("0.113.0"));
        assert_eq!(def.source.as_deref(), Some("flow"));
    }

    #[test]
    fn file_source_path_falls_back_to_path() {
        let f = ExtensionFile {
            path: ".claude/settings.json".into(),
            source: Some("claude-settings.json".into()),
            executable: false,
            if_absent: false,
            substitute: true,
        };
        assert_eq!(f.source_path(), "claude-settings.json");
        let g = ExtensionFile {
            path: "FLOW.md".into(),
            source: None,
            executable: false,
            if_absent: false,
            substitute: false,
        };
        assert_eq!(g.source_path(), "FLOW.md");
    }
}
