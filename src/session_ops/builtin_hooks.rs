//! The built-in **hooks** extension: wires each coding agent's lifecycle hooks
//! to `thurbox-cli session signal` so sessions report `working`/`blocked`/`done`
//! back to thurbox (see the hooks-driven `SessionStatus`).
//!
//! Unlike user extensions (which are fetched from a source on demand, ADR-20),
//! this one ships **embedded** in the binary and is **auto-activated by default**
//! so the default agent has its hook pre-configured with zero setup. It is
//! delivered through the ordinary extension machinery: the embedded assets are
//! materialized into a stable local dir, then [`install_extension`] installs them
//! from there — so all the install/heal/uninstall logic is shared.
//!
//! Opt out with `thurbox-cli extension deactivate hooks`, which records an
//! opt-out flag so startup self-heal won't resurrect it.

use std::path::PathBuf;

use crate::storage::Database;

use super::install_extension;

/// The extension name (matches `extensions/hooks/extension.toml`).
pub const HOOKS_EXTENSION_NAME: &str = "hooks";

const MANIFEST: &str = include_str!("../../extensions/hooks/extension.toml");
const CLAUDE_SETTINGS: &str = include_str!("../../extensions/hooks/claude.json");
const OPENCODE_PLUGIN: &str = include_str!("../../extensions/hooks/opencode-status.js");
const ANTIGRAVITY_HOOKS: &str = include_str!("../../extensions/hooks/antigravity-hooks.json");
const CODEX_HOOKS: &str = include_str!("../../extensions/hooks/codex-hooks.json");
const VIBE_HOOKS: &str = include_str!("../../extensions/hooks/vibe-hooks.toml");

/// The hooks extension's home, under this build's resolved config dir
/// (`~/.config/thurbox/hooks` for a release build, `~/.config/thurbox-dev/hooks`
/// for a dev build) — so dev and release installs stay isolated and the injected
/// `--settings` path always points inside the same tree the binary uses.
fn hooks_home() -> Option<String> {
    crate::paths::config_file()
        .and_then(|p| p.parent().map(|d| d.join("hooks")))
        .map(|p| p.to_string_lossy().into_owned())
}

/// Materialize the embedded hooks-extension assets into a stable local dir under
/// the data directory and return it, so [`install_extension`] can treat it as a
/// local source. Rewritten on every call so the assets track the binary.
fn materialize_source() -> Result<PathBuf, String> {
    let base = crate::paths::builtin_extensions_directory()
        .ok_or("cannot resolve builtin-extensions dir")?;
    let dir = base.join(HOOKS_EXTENSION_NAME);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let writes = [
        ("extension.toml", MANIFEST),
        ("claude.json", CLAUDE_SETTINGS),
        ("opencode-status.js", OPENCODE_PLUGIN),
        ("antigravity-hooks.json", ANTIGRAVITY_HOOKS),
        ("codex-hooks.json", CODEX_HOOKS),
        ("vibe-hooks.toml", VIBE_HOOKS),
    ];
    for (name, contents) in writes {
        let path = dir.join(name);
        // Skip the write when unchanged — this runs on every startup + 60s tick.
        if std::fs::read_to_string(&path).is_ok_and(|c| c == contents) {
            continue;
        }
        std::fs::write(&path, contents).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(dir)
}

/// Ensure the built-in hooks extension is installed + active, unless the user
/// opted out. Idempotent — safe to call at every TUI startup / automation tick;
/// it re-applies the agent patches, payload + external files, and re-stamps the
/// manifest so an upgrade refreshes the wiring. Returns human-readable status
/// lines (empty when there's nothing to report).
pub fn ensure_builtin_hooks_extension(db: &Database) -> Vec<String> {
    if db.builtin_hooks_opted_out().unwrap_or(false) {
        return Vec::new();
    }
    let dir = match materialize_source() {
        Ok(d) => d,
        Err(e) => return vec![format!("hooks extension: {e}")],
    };
    // Home lives under *this build's* config dir (`thurbox` vs `thurbox-dev`), so
    // a dev build patches its dev `agents.toml` with a `--settings` path inside
    // the dev tree — never the release config. Manifest `home` is ignored.
    let Some(home) = hooks_home() else {
        return vec!["hooks extension: cannot resolve config dir".into()];
    };

    // Migrate a stale install whose home points elsewhere (e.g. an earlier build
    // that used the release path): tear down its patches/files before reinstalling
    // under the correct home, so claude doesn't end up with two `--settings`.
    if let Some(existing) = crate::agent::extension_config::load_manifest(HOOKS_EXTENSION_NAME) {
        if existing.home.as_deref() != Some(home.as_str()) {
            let _ = super::uninstall_extension(db, HOOKS_EXTENSION_NAME, false);
        }
    }

    match install_extension(db, &dir.to_string_lossy(), Some(&home), false) {
        Ok(report) => {
            let mut msgs = Vec::new();
            if !report.agents_patched.is_empty() {
                msgs.push(format!(
                    "hooks: wired agent hooks for {}",
                    report.agents_patched.join(", ")
                ));
            }
            if !report.external_files_written.is_empty() {
                msgs.push("hooks: installed opencode status plugin".to_string());
            }
            msgs
        }
        Err(e) => vec![format!("hooks extension: {e}")],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_assets_are_present() {
        assert!(MANIFEST.contains("name = \"hooks\""));
        assert!(CLAUDE_SETTINGS.contains("session signal --state working"));
        // The opencode plugin must carry the managed marker so uninstall can
        // safely remove it (see `is_user_modified`).
        assert!(OPENCODE_PLUGIN.contains("thurbox `extension install`"));
        // codex's hooks.json reports the full idle/working/done range.
        assert!(CODEX_HOOKS.contains("session signal --state idle"));
        // The vibe payload carries the signal marker (prune) and the managed
        // marker (external-file uninstall, see `is_user_modified`).
        assert!(VIBE_HOOKS.contains("thurbox-cli session signal"));
        assert!(VIBE_HOOKS.contains("thurbox `extension install`"));
    }

    #[test]
    fn embedded_manifest_parses_with_codex_vibe_and_antigravity_wiring() {
        // Parse the embedded manifest exactly as the installer does — this guards
        // the codex + antigravity config_merges (and the vibe external file) from
        // silently breaking the build.
        let def: crate::session::ExtensionDef =
            toml::from_str(MANIFEST).expect("embedded manifest parses");

        // codex now JSON-merges a claude-shaped hooks.json into ~/.codex/hooks.json
        // (idle/working/done) rather than the old `-c notify=…` agent patch.
        let codex = def
            .config_merges
            .iter()
            .find(|m| m.path.contains(".codex"))
            .expect("codex config merge present");
        assert_eq!(codex.source_path(), "codex-hooks.json");
        assert_eq!(codex.requires_dir.as_deref(), Some("~/.codex"));
        assert!(
            def.agent_patches.iter().all(|p| p.name != "codex"),
            "codex should no longer be wired via an agent patch"
        );

        // The codex payload is valid JSON, claude-shaped, and carries the marker.
        let codex_payload: serde_json::Value =
            serde_json::from_str(CODEX_HOOKS).expect("codex payload is valid JSON");
        assert!(codex_payload["hooks"]["SessionStart"].is_array());
        assert!(codex_payload["hooks"]["Stop"].is_array());
        assert!(CODEX_HOOKS.contains("thurbox-cli session signal"));

        // vibe drops a managed hooks.toml into ~/.vibe/ (guarded by requires_dir).
        let vibe = def
            .external_files
            .iter()
            .find(|f| f.path.contains(".vibe"))
            .expect("vibe external file present");
        assert_eq!(vibe.source_path(), "vibe-hooks.toml");
        assert_eq!(vibe.requires_dir.as_deref(), Some("~/.vibe"));

        // antigravity (agy) shares gemini's ~/.gemini/settings.json for hooks.
        let antigravity = def
            .config_merges
            .iter()
            .find(|m| m.path.contains(".gemini"))
            .expect("antigravity config merge present");
        assert_eq!(antigravity.source_path(), "antigravity-hooks.json");
        assert_eq!(antigravity.requires_dir.as_deref(), Some("~/.gemini"));

        // The antigravity payload is valid JSON and carries the prune marker.
        let payload: serde_json::Value =
            serde_json::from_str(ANTIGRAVITY_HOOKS).expect("antigravity payload is valid JSON");
        // agy 1.0.9 adopted claude's hook schema; guard against a regression back
        // to the gemini-era `BeforeTool`/`AfterAgent` names (which agy never fires,
        // so working/done would silently stop reporting).
        for event in ["SessionStart", "PreToolUse", "Notification", "Stop"] {
            assert!(
                payload["hooks"][event].is_array(),
                "antigravity hook event {event} missing"
            );
        }
        assert!(payload["hooks"]["BeforeTool"].is_null());
        assert!(payload["hooks"]["AfterAgent"].is_null());
        assert!(ANTIGRAVITY_HOOKS.contains("thurbox-cli session signal"));
    }

    #[test]
    fn hooks_home_derives_from_build_config_dir() {
        // Home must track the resolved config dir (so a dev build lands under
        // `thurbox-dev`, not the release tree) — never a hardcoded path.
        let tmp = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());
        let home = hooks_home().expect("home resolves");
        let expected = crate::paths::config_file()
            .unwrap()
            .parent()
            .unwrap()
            .join("hooks");
        assert_eq!(std::path::Path::new(&home), expected);
    }

    #[test]
    fn opt_out_skips_install() {
        let db = Database::open_in_memory().unwrap();
        db.set_builtin_hooks_optout(true).unwrap();
        // With opt-out set, ensure is a no-op (no install attempted).
        assert!(ensure_builtin_hooks_extension(&db).is_empty());
    }
}
