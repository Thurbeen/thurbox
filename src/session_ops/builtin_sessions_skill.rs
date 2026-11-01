//! The built-in **sessions-skill** extension: installs one agent skill,
//! `thurbox-sessions`, into each coding CLI's *personal* skill directory, so an
//! agent in **any** session knows how to find the other sessions, read their
//! context without disturbing them, and message them safely.
//!
//! [`super::builtin_ui_skill`]'s twin, shipped the same way and for the same
//! reason: an agent does not know its peers are reachable through
//! `thurbox-cli` until something says so, and nobody goes looking for the
//! extension that would. Opt out with
//! `thurbox-cli extension deactivate sessions-skill`.

use super::builtin::Builtin;
use super::InstallReport;

/// The extension name (matches `extensions/sessions-skill/extension.toml`).
pub const SESSIONS_SKILL_EXTENSION_NAME: &str = "sessions-skill";

pub(crate) const MANIFEST: &str = include_str!("../../extensions/sessions-skill/extension.toml");
pub(crate) const SKILL: &str = include_str!("../../extensions/sessions-skill/SKILL.md");
pub(crate) const README: &str = include_str!("../../extensions/sessions-skill/README.md");

/// The built-in sessions-skill extension. Its home is the default an ordinary
/// `extension install` would pick, for the reason
/// [`super::builtin_ui_skill`] gives.
pub(crate) static SESSIONS_SKILL: Builtin = Builtin {
    name: SESSIONS_SKILL_EXTENSION_NAME,
    blurb: "the thurbox-sessions agent skill",
    assets: &[
        ("extension.toml", MANIFEST),
        ("SKILL.md", SKILL),
        ("README.md", README),
    ],
    home_dir: "extensions/sessions-skill",
    notices: sessions_skill_notices,
};

fn sessions_skill_notices(report: &InstallReport) -> Vec<String> {
    if report.external_files_written.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "sessions-skill: installed the thurbox-sessions skill for {} agent CLI(s)",
        report.external_files_written.len()
    )]
}
