//! **Built-in extensions** — the ones that ship embedded in the binary and are
//! auto-activated, rather than being fetched from a source on demand (ADR-20).
//!
//! There are two, and they exist for the same reason: what they wire up has to
//! be there before the user knows to ask for it. [`builtin_hooks`] gives every
//! session its status dot with zero setup; [`builtin_ui_skill`] gives whichever
//! coding CLI the user runs the knowledge of how to edit thurbox's own
//! interface, in every session rather than only the one they thought to attach
//! the interface directory to.
//!
//! Delivery reuses the ordinary extension machinery whole: the embedded assets
//! are materialized into a stable local dir, then [`install_extension`]
//! installs them from there — so install, self-heal and uninstall are the same
//! code paths a fetched extension takes, and a built-in is not a second
//! installer with its own bugs.
//!
//! Opt out of either with `thurbox-cli extension deactivate <name>`, which
//! records a flag ([`Database::set_builtin_extension_optout`]) so startup
//! self-heal won't resurrect it; `activate` (or `install`) clears it.
//!
//! [`builtin_hooks`]: super::builtin_hooks
//! [`builtin_ui_skill`]: super::builtin_ui_skill

use std::path::PathBuf;

use crate::storage::Database;

use super::{install_extension, InstallReport};

/// One built-in extension: its embedded payload, where it installs, and how it
/// describes what it just did.
pub struct Builtin {
    /// Extension name — matches its `extensions/<name>/extension.toml`.
    pub name: &'static str,
    /// Human phrase for the thing being turned on or off, used in the
    /// activate/deactivate summaries (e.g. "agent status hooks").
    pub blurb: &'static str,
    /// Every file the manifest references, embedded. Keyed by the
    /// source-relative path [`crate::session::ExtensionFile::source_path`] and
    /// its siblings resolve to; a manifest entry with no asset here installs to
    /// a fetch error, which `assets_cover_every_manifest_payload` guards.
    pub assets: &'static [(&'static str, &'static str)],
    /// Install home, relative to *this build's* config dir — so a dev build
    /// installs under `~/.config/thurbox-dev/…` and cannot touch the release
    /// tree. The manifest's own `home` is ignored.
    pub home_dir: &'static str,
    /// What to report after a successful ensure. Lives with each built-in
    /// because only it knows which half of the report is the interesting one.
    pub notices: fn(&InstallReport) -> Vec<String>,
}

/// Every built-in, in the order they are ensured at startup.
pub const BUILTINS: &[&Builtin] = &[
    &super::builtin_hooks::HOOKS,
    &super::builtin_ui_skill::UI_SKILL,
];

/// The built-in named `name`, if it is one. This is the test the CLI applies
/// before treating an `install`/`activate`/`deactivate` as an opt-in/opt-out
/// rather than a manifest operation.
pub fn builtin_extension(name: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().copied().find(|b| b.name == name)
}

impl Builtin {
    /// This build's install home for the extension (absolute).
    pub fn home(&self) -> Option<String> {
        crate::paths::config_file()
            .and_then(|p| p.parent().map(|d| d.join(self.home_dir)))
            .map(|p| p.to_string_lossy().into_owned())
    }

    /// Materialize the embedded assets into a stable local dir under the data
    /// directory and return it, so [`install_extension`] can treat it as a
    /// local source. Rewritten on every call so the assets track the binary.
    fn materialize_source(&self) -> Result<PathBuf, String> {
        let base = crate::paths::builtin_extensions_directory()
            .ok_or("cannot resolve builtin-extensions dir")?;
        let dir = base.join(self.name);
        for (name, contents) in self.assets {
            let path = dir.join(name);
            // Skip the write when unchanged — this runs on every startup + 60s tick.
            if std::fs::read_to_string(&path).is_ok_and(|c| &c == contents) {
                continue;
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("create {}: {e}", parent.display()))?;
            }
            std::fs::write(&path, contents)
                .map_err(|e| format!("write {}: {e}", path.display()))?;
        }
        Ok(dir)
    }

    /// Ensure this built-in is installed + active, unless the user opted out.
    /// Idempotent — safe to call at every TUI startup / automation tick; it
    /// re-applies the payload and re-stamps the manifest, so an upgrade
    /// refreshes the wiring. Returns human-readable status lines (empty when
    /// there is nothing to report, including when it is opted out).
    pub fn ensure(&self, db: &Database) -> Vec<String> {
        if db.builtin_extension_opted_out(self.name).unwrap_or(false) {
            return Vec::new();
        }
        let dir = match self.materialize_source() {
            Ok(d) => d,
            Err(e) => return vec![format!("{} extension: {e}", self.name)],
        };
        let Some(home) = self.home() else {
            return vec![format!(
                "{} extension: cannot resolve config dir",
                self.name
            )];
        };

        // Migrate a stale install whose home points elsewhere (e.g. an earlier
        // build that used the release path): tear down its patches/files before
        // reinstalling under the correct home, so claude doesn't end up with two
        // `--settings`.
        if let Some(existing) = crate::agent::extension_config::load_manifest(self.name) {
            if existing.home.as_deref() != Some(home.as_str()) {
                let _ = super::uninstall_extension(db, self.name, false);
            }
        }

        match install_extension(db, &dir.to_string_lossy(), Some(&home), false) {
            Ok(report) => (self.notices)(&report),
            Err(e) => vec![format!("{} extension: {e}", self.name)],
        }
    }
}

/// Ensure every built-in extension. Called at TUI startup and at the top of the
/// headless `automation tick`, so a built-in stays wired with the TUI closed.
pub fn ensure_builtin_extensions(db: &Database) -> Vec<String> {
    BUILTINS.iter().flat_map(|b| b.ensure(db)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manifest entry naming a payload the binary does not carry installs to a
    /// fetch error on the user's machine — the one failure mode embedding
    /// introduces that a fetched extension cannot have. Adding a `[[files]]` or
    /// `[[external_files]]` entry without adding its `include_str!` is the way
    /// in, so check the two agree.
    #[test]
    fn assets_cover_every_manifest_payload() {
        for b in BUILTINS {
            let manifest = b
                .assets
                .iter()
                .find(|(n, _)| *n == "extension.toml")
                .map(|(_, c)| *c)
                .unwrap_or_else(|| panic!("built-in '{}' embeds no extension.toml", b.name));
            let def: crate::session::ExtensionDef =
                toml::from_str(manifest).unwrap_or_else(|e| panic!("{}: {e}", b.name));
            assert_eq!(
                def.name, b.name,
                "built-in embeds another extension's manifest"
            );

            let referenced = def
                .files
                .iter()
                .map(crate::session::ExtensionFile::source_path)
                .chain(
                    def.external_files
                        .iter()
                        .map(crate::session::ExternalFile::source_path),
                )
                .chain(
                    def.config_merges
                        .iter()
                        .map(crate::session::ConfigMerge::source_path),
                );
            for src in referenced {
                assert!(
                    b.assets.iter().any(|(n, _)| *n == src),
                    "built-in '{}' references {src} but embeds no such asset",
                    b.name
                );
            }
        }
    }

    /// The assets table pairs a source-relative *name* with a constant by hand,
    /// and `include_str!` cannot check that the two describe the same file — a
    /// tuple copied from another built-in ships one payload under another's
    /// name, and the manifest check above still passes. Compare each against the
    /// path its name claims.
    #[test]
    fn each_asset_is_the_file_its_name_claims() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions");
        for b in BUILTINS {
            for (name, contents) in b.assets {
                let path = root.join(b.name).join(name);
                let on_disk = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
                assert_eq!(
                    &on_disk,
                    contents,
                    "{} drifted from the binary",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn builtin_extension_looks_up_by_name() {
        assert!(builtin_extension("hooks").is_some());
        assert!(builtin_extension("ui-skill").is_some());
        assert!(builtin_extension("flow").is_none());
    }

    #[test]
    fn home_lands_under_this_builds_config_dir() {
        for b in BUILTINS {
            let Some(home) = b.home() else { continue };
            let config = crate::paths::config_file().unwrap();
            let parent = config.parent().unwrap();
            assert!(
                home.starts_with(&*parent.to_string_lossy()),
                "'{}' home {home} escapes the config dir",
                b.name
            );
        }
    }
}
