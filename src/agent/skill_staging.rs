//! Per-session Claude config staging for selected skills.
//!
//! Builds `~/.local/share/thurbox/sessions/<key>/claude-home/` containing:
//! - `skills/` with symlinks to the selected skill directories
//! - Top-level entries symlinked from `~/.claude/` (settings.json, agents, …)
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
const MIRRORED_CLAUDE_ENTRIES: &[&str] = &[
    ".credentials.json",
    "settings.json",
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
