//! The built-in **ui-skill** extension: installs one agent skill, `thurbox-ui`,
//! into each coding CLI's *personal* skill directory, so an agent in **any**
//! session knows how to change thurbox's own interface.
//!
//! It exists because the interface is Lua in a config directory of the user's,
//! which an agent working in an unrelated repository has no reason to know
//! about. The workaround it replaces is attaching that directory to every
//! session as an extra repo, which puts it in front of the agent whether or not
//! the session is about the TUI; a skill loads only when the request is.
//!
//! Like [`super::builtin_hooks`] it ships **embedded** and is auto-activated,
//! for the same reason: a person who does not already know the interface is
//! editable will not go looking for the extension that says so. The generic
//! half is [`super::builtin`]; this module is the spec. Opt out with
//! `thurbox-cli extension deactivate ui-skill`.
//!
//! The payload is one `SKILL.md`, delivered by the manifest's
//! `[[external_files]]` to each CLI's skill dir behind a `requires_dir` guard,
//! so a CLI the user does not run is skipped rather than having a config tree
//! conjured for it.

use super::builtin::Builtin;
use super::InstallReport;

/// The extension name (matches `extensions/ui-skill/extension.toml`).
pub const UI_SKILL_EXTENSION_NAME: &str = "ui-skill";

pub(crate) const MANIFEST: &str = include_str!("../../extensions/ui-skill/extension.toml");
pub(crate) const SKILL: &str = include_str!("../../extensions/ui-skill/SKILL.md");
pub(crate) const README: &str = include_str!("../../extensions/ui-skill/README.md");

/// The built-in ui-skill extension. Its home is the *default* an ordinary
/// `extension install ui-skill` would pick (`<config>/extensions/ui-skill`)
/// rather than a bespoke one: the two paths agreeing is what stops the
/// stale-home migration in [`Builtin::ensure`] tearing the install down and
/// rebuilding it every time the user alternates between them.
pub(crate) static UI_SKILL: Builtin = Builtin {
    name: UI_SKILL_EXTENSION_NAME,
    blurb: "the thurbox-ui agent skill",
    assets: &[
        ("extension.toml", MANIFEST),
        ("SKILL.md", SKILL),
        ("README.md", README),
    ],
    home_dir: "extensions/ui-skill",
    notices: ui_skill_notices,
};

/// What an ensure of ui-skill is worth saying. Its whole product is files
/// written outside the home dir, so that count is the only interesting half —
/// and it is genuinely variable, since each destination is guarded on the CLI
/// being installed.
fn ui_skill_notices(report: &InstallReport) -> Vec<String> {
    if report.external_files_written.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "ui-skill: installed the thurbox-ui skill for {} agent CLI(s)",
        report.external_files_written.len()
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frontmatter is what makes the file a skill rather than a document:
    /// a CLI with no `name`/`description` to match against never loads it, and
    /// the failure is silent — the skill simply never triggers.
    #[test]
    fn skill_payload_opens_with_usable_frontmatter() {
        let mut lines = SKILL.lines();
        assert_eq!(
            lines.next(),
            Some("---"),
            "SKILL.md must open with frontmatter"
        );
        let front: Vec<&str> = lines.take_while(|l| *l != "---").collect();
        let name = front
            .iter()
            .find_map(|l| l.strip_prefix("name: "))
            .expect("frontmatter declares a name");
        assert_eq!(name, "thurbox-ui");
        let desc = front
            .iter()
            .find_map(|l| l.strip_prefix("description: "))
            .expect("frontmatter declares a description");
        // A plain YAML scalar cannot carry ": " — the frontmatter fails to parse
        // and the skill is skipped, with nothing said about why.
        assert!(!desc.contains(": "), "description needs quoting: {desc}");
    }

    /// The installer only overwrites a file still carrying its managed marker,
    /// so a payload without one is written once and then never updated again.
    #[test]
    fn skill_payload_carries_the_managed_marker() {
        assert!(
            SKILL.contains(super::super::extensions::MANAGED_MARKER),
            "SKILL.md must carry the managed marker or updates silently stop"
        );
    }

    /// Every destination is an agent's own config dir, so each must be guarded:
    /// an unguarded entry creates `~/.codex` for someone who does not run codex.
    #[test]
    fn every_destination_is_guarded_on_the_cli_being_installed() {
        let def: crate::session::ExtensionDef = toml::from_str(MANIFEST).unwrap();
        assert!(
            !def.external_files.is_empty(),
            "ui-skill delivers external files"
        );
        for f in &def.external_files {
            assert!(
                f.requires_dir.is_some(),
                "{} would be written for a CLI the user may not have",
                f.path
            );
        }
    }
}
