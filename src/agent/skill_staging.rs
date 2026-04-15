//! Per-session Claude config staging for selected skills.
//!
//! Builds `~/.local/share/thurbox/sessions/<key>/claude-home/` containing:
//! - `skills/` with symlinks to the selected skill directories
//! - `settings.json` materialized with Thurbox's `statusLine` merged over the
//!   user's global `~/.claude/settings.json`
//! - Other top-level entries symlinked from `~/.claude/` (agents, credentials, …)
//!
//! Then `CLAUDE_CONFIG_DIR` is set to that path when spawning `claude`, so
//! per-session skills never touch the repo worktree.

use std::path::PathBuf;

use anyhow::{anyhow, Result};

use crate::session::SkillConfig;

/// Root of per-session staging directories.
fn staging_root() -> Result<PathBuf> {
    crate::paths::log_directory()
        .map(|p| p.join("sessions"))
        .ok_or_else(|| anyhow!("thurbox data directory unavailable"))
}

/// Path to the claude-home directory for a given session key.
fn claude_home_dir(session_key: &str) -> Result<PathBuf> {
    Ok(staging_root()?.join(session_key).join("claude-home"))
}

/// Entries Claude Code is known to read or write at the top of its config dir.
///
/// We pre-create dangling symlinks for each so that any writes inside the
/// session (e.g. `claude login` storing credentials, new project history,
/// updated settings) resolve through the symlink back to the real `~/.claude/`
/// — so login and state persist across sessions and outside thurbox.
///
/// Note: `settings.json` is intentionally excluded — Thurbox materializes a
/// real (merged) `settings.json` in the staging dir so it can inject its own
/// `statusLine` without mutating the user's global `~/.claude/settings.json`.
const MIRRORED_CLAUDE_ENTRIES: &[&str] = &[
    ".credentials.json",
    "settings.local.json",
    "CLAUDE.md",
    "projects",
    "todos",
    "statsig",
    "shell-snapshots",
    "ide",
    "agents",
    "commands",
    "plugins",
];

/// Build (or refresh) the per-session claude-home directory and populate its
/// `skills/` subdirectory with symlinks to the selected skills. Returns the
/// absolute path suitable for `CLAUDE_CONFIG_DIR`.
///
/// The top-level directory is preserved across calls (only `skills/` is
/// rebuilt) so that any files Claude wrote there (e.g. new credentials) are
/// not destroyed. Known writable entries are symlinked back to `~/.claude/`
/// proactively so logins/state propagate.
pub fn prepare(session_key: &str, skills: &[SkillConfig]) -> Result<PathBuf> {
    let dir = claude_home_dir(session_key)?;
    std::fs::create_dir_all(&dir)?;

    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        let src_claude = home.join(".claude");

        // `.claude.json` lives at $HOME/.claude.json, not inside ~/.claude/.
        // With CLAUDE_CONFIG_DIR set, Claude looks for it inside the config
        // dir — symlink it back to the real location so state is shared.
        let claude_json = dir.join(".claude.json");
        if claude_json.symlink_metadata().is_err() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(home.join(".claude.json"), &claude_json)?;
        }

        // Pre-create dangling symlinks for known writable entries. Writes
        // through these resolve to ~/.claude/ even if the target doesn't exist
        // yet (e.g. user has not logged in before first launch).
        for name in MIRRORED_CLAUDE_ENTRIES {
            let dst = dir.join(name);
            if dst.symlink_metadata().is_ok() {
                continue;
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(src_claude.join(name), &dst)?;
        }

        // Also mirror any other existing top-level entries we don't know
        // about, to avoid breaking unknown features. Skip `skills` (managed
        // below) and anything we've already linked.
        if src_claude.is_dir() {
            for entry in std::fs::read_dir(&src_claude)? {
                let entry = entry?;
                let name = entry.file_name();
                if name == "skills" {
                    continue;
                }
                let dst = dir.join(&name);
                if dst.symlink_metadata().is_ok() {
                    continue;
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(entry.path(), &dst)?;
            }
        }
    }

    // Materialize a real `settings.json` in the staging dir so Thurbox can
    // set its own `statusLine` (feeding the info panel's token/context
    // metrics) without mutating the user's global `~/.claude/settings.json`.
    // Any existing keys in the user's global file are preserved.
    write_session_settings_json(&dir)?;

    // `skills/` is fully rebuilt each time so de-selected skills disappear.
    let skills_dir = dir.join("skills");
    if skills_dir.exists() {
        std::fs::remove_dir_all(&skills_dir)?;
    }
    std::fs::create_dir_all(&skills_dir)?;
    for skill in skills {
        let dst = skills_dir.join(&skill.name);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&skill.path, &dst)?;
    }

    Ok(dir)
}

/// Write a real `settings.json` into the staging directory, merging the user's
/// global `~/.claude/settings.json` (if any) with Thurbox's `statusLine` entry.
///
/// The merged file wins over the user's global because `CLAUDE_CONFIG_DIR`
/// short-circuits Claude's config discovery to the staging dir.
fn write_session_settings_json(dir: &std::path::Path) -> Result<()> {
    use serde_json::{json, Value};

    let target = dir.join("settings.json");

    // Remove any pre-existing symlink/file so we never overwrite through a
    // symlink pointing at the user's real settings.
    if target.symlink_metadata().is_ok() {
        std::fs::remove_file(&target)?;
    }

    // Seed from the user's global file so unrelated keys (theme, env, …)
    // are preserved.
    let mut settings: Value = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join(".claude").join("settings.json"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_else(|| json!({}));

    if let Some(script) = crate::paths::statusline_script_path() {
        settings["statusLine"] = json!({
            "type": "command",
            "command": script.display().to_string(),
        });
    }

    let pretty = serde_json::to_string_pretty(&settings)
        .map_err(|e| anyhow!("serialize settings.json: {e}"))?;
    std::fs::write(&target, pretty)?;
    Ok(())
}

/// Remove the per-session staging directory. Best-effort.
pub fn cleanup(session_key: &str) {
    if let Ok(root) = staging_root() {
        let dir = root.join(session_key);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::TestPathGuard;

    #[test]
    fn prepare_creates_skills_symlinks() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = TestPathGuard::new(temp.path());

        let skill_src = temp.path().join("src-skill");
        std::fs::create_dir_all(&skill_src).unwrap();
        std::fs::write(skill_src.join("SKILL.md"), "hi").unwrap();

        let skills = vec![SkillConfig {
            name: "my-skill".to_string(),
            path: skill_src.clone(),
        }];

        let dir = prepare("sess-1", &skills).unwrap();
        let linked = dir.join("skills").join("my-skill");
        assert!(linked.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_link(&linked).unwrap(), skill_src);
    }

    #[test]
    fn prepare_is_idempotent() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = TestPathGuard::new(temp.path());

        let skill_src = temp.path().join("s");
        std::fs::create_dir_all(&skill_src).unwrap();
        let skills = vec![SkillConfig {
            name: "a".to_string(),
            path: skill_src,
        }];

        prepare("sess-2", &skills).unwrap();
        let dir = prepare("sess-2", &skills).unwrap();
        assert!(dir.join("skills").join("a").exists());
    }

    #[test]
    fn prepare_symlinks_claude_json_to_home() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = TestPathGuard::new(temp.path());
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &fake_home);

        let dir = prepare("sess-json", &[]).unwrap();

        let link = dir.join(".claude.json");
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            fake_home.join(".claude.json"),
            ".claude.json must point to $HOME/.claude.json (not ~/.claude/.claude.json)"
        );

        if let Some(h) = prev_home {
            std::env::set_var("HOME", h);
        } else {
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn prepare_creates_dangling_symlinks_for_mirrored_entries() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = TestPathGuard::new(temp.path());
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &fake_home);

        let dir = prepare("sess-mirror", &[]).unwrap();

        for name in MIRRORED_CLAUDE_ENTRIES {
            let link = dir.join(name);
            let target = std::fs::read_link(&link)
                .unwrap_or_else(|_| panic!("expected {name} to be a symlink"));
            assert_eq!(target, fake_home.join(".claude").join(name));
        }

        if let Some(h) = prev_home {
            std::env::set_var("HOME", h);
        } else {
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn prepare_preserves_files_written_into_staging() {
        // Simulates the case where Claude writes a new credential file into
        // CLAUDE_CONFIG_DIR: re-preparing the session must not destroy it.
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = TestPathGuard::new(temp.path());

        let dir = prepare("sess-persist", &[]).unwrap();
        let written = dir.join("brand-new-file");
        std::fs::write(&written, b"secret").unwrap();

        prepare("sess-persist", &[]).unwrap();
        assert!(
            written.exists(),
            "files written into staging must survive re-prepare"
        );
        assert_eq!(std::fs::read(&written).unwrap(), b"secret");
    }

    #[test]
    fn prepare_rebuilds_skills_dir_on_reconfigure() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = TestPathGuard::new(temp.path());

        let skill_a = temp.path().join("a");
        let skill_b = temp.path().join("b");
        std::fs::create_dir_all(&skill_a).unwrap();
        std::fs::create_dir_all(&skill_b).unwrap();

        let dir = prepare(
            "sess-reconf",
            &[
                SkillConfig {
                    name: "a".into(),
                    path: skill_a.clone(),
                },
                SkillConfig {
                    name: "b".into(),
                    path: skill_b.clone(),
                },
            ],
        )
        .unwrap();
        assert!(dir.join("skills").join("a").exists());
        assert!(dir.join("skills").join("b").exists());

        // Deselect "a" and re-prepare — it must disappear.
        prepare(
            "sess-reconf",
            &[SkillConfig {
                name: "b".into(),
                path: skill_b,
            }],
        )
        .unwrap();
        assert!(dir.join("skills").join("a").symlink_metadata().is_err());
        assert!(dir.join("skills").join("b").exists());
    }

    #[test]
    fn prepare_materializes_settings_json_with_statusline() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = TestPathGuard::new(temp.path());
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &fake_home);

        let dir = prepare("sess-settings", &[]).unwrap();

        let settings_path = dir.join("settings.json");
        let meta = settings_path.symlink_metadata().unwrap();
        assert!(
            !meta.file_type().is_symlink(),
            "settings.json must be a real file, not a symlink to the user's global config"
        );

        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        let cmd = value
            .pointer("/statusLine/command")
            .and_then(|v| v.as_str())
            .expect("statusLine.command must be set");
        assert!(
            cmd.ends_with("statusline.sh"),
            "statusLine.command must point at Thurbox's statusline.sh, got {cmd}"
        );

        if let Some(h) = prev_home {
            std::env::set_var("HOME", h);
        } else {
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn prepare_merges_user_global_settings() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = TestPathGuard::new(temp.path());
        let fake_home = temp.path().join("home");
        let user_claude = fake_home.join(".claude");
        std::fs::create_dir_all(&user_claude).unwrap();
        std::fs::write(
            user_claude.join("settings.json"),
            r#"{"theme":"dark","statusLine":{"type":"command","command":"/user/own.sh"}}"#,
        )
        .unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &fake_home);

        let dir = prepare("sess-merge", &[]).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap())
                .unwrap();

        assert_eq!(value.get("theme").and_then(|v| v.as_str()), Some("dark"));
        let cmd = value
            .pointer("/statusLine/command")
            .and_then(|v| v.as_str())
            .unwrap();
        assert!(
            cmd.ends_with("statusline.sh"),
            "Thurbox statusLine must override the user's, got {cmd}"
        );

        // User's global file must be pristine.
        let global = std::fs::read_to_string(user_claude.join("settings.json")).unwrap();
        assert!(global.contains("/user/own.sh"));

        if let Some(h) = prev_home {
            std::env::set_var("HOME", h);
        } else {
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn prepare_refreshes_settings_json_on_second_call() {
        // Re-preparing a session must re-materialize settings.json as a real
        // file (the mirror loop must never turn it back into a symlink) and
        // must re-apply the current statusLine from the user's global.
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = TestPathGuard::new(temp.path());
        let fake_home = temp.path().join("home");
        std::fs::create_dir_all(fake_home.join(".claude")).unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &fake_home);

        let dir = prepare("sess-refresh", &[]).unwrap();
        prepare("sess-refresh", &[]).unwrap();

        let meta = dir.join("settings.json").symlink_metadata().unwrap();
        assert!(
            !meta.file_type().is_symlink(),
            "settings.json must remain a real file after re-prepare"
        );
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap())
                .unwrap();
        assert!(value.pointer("/statusLine/command").is_some());

        if let Some(h) = prev_home {
            std::env::set_var("HOME", h);
        } else {
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn cleanup_removes_staging_dir() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = TestPathGuard::new(temp.path());

        prepare("sess-3", &[]).unwrap();
        let root = staging_root().unwrap().join("sess-3");
        assert!(root.exists());

        cleanup("sess-3");
        assert!(!root.exists());
    }
}
