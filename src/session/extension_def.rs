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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionAutomation {
    /// Automation name. Used to find/reuse it.
    pub name: String,
    /// Trigger spec, same grammar as `thurbox-cli automation create --trigger`
    /// (`hourly` | `daily` | `weekdays` | `weekly` | `cron:<expr>` | `at:<ms>`).
    pub trigger: String,
    /// Name of the extension session this automation sends its prompt to. Must
    /// match one of the manifest's `[[sessions]]` entries.
    pub session_ref: String,
    /// Prompt text delivered on each fire (e.g. `tick`).
    pub prompt: String,
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
        out
    }
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
        assert_eq!(def.automations[0].session_ref, "flow");
        assert_eq!(def.automations[0].prompt, "tick");
        assert!(!def.is_empty());
    }

    #[test]
    fn round_trips_through_toml() {
        let def = ExtensionDef {
            name: "flow".into(),
            description: None,
            config_version: Some(1),
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
                session_ref: "flow".into(),
                prompt: "tick".into(),
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
