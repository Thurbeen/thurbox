//! Miscellaneous helper functions used by the app module.

use std::path::PathBuf;

pub(super) fn open_url(url: &str) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(cmd)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok();
}

/// Spawn `editor_cmd` with `worktree` appended as the final argument.
///
/// `editor_cmd` is whitespace-split so callers can include flags
/// (e.g. `"code --wait"` or `"nvim --server /tmp/s --remote"`). Returns an
/// error if the command string is empty or fails to spawn.
pub(super) fn open_in_editor(paths: &[PathBuf], editor_cmd: &str) -> std::io::Result<()> {
    if paths.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no paths to open",
        ));
    }
    let mut parts = editor_cmd.split_whitespace();
    let Some(program) = parts.next() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "editor command is empty",
        ));
    };
    let extra_args: Vec<&str> = parts.collect();

    let mut cmd = std::process::Command::new(program);
    cmd.args(&extra_args)
        .args(paths)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    // Detach from Thurbox's process group so signals to Thurbox
    // don't reach the editor. Matters on WSL, where `code`/`zed` are
    // launcher scripts that hand off to Windows via `/init` interop —
    // without this, the interop bridge tears down before the GUI appears.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    cmd.spawn().map(|_| ())
}

/// Resolve the editor command from the DB setting, falling back to
/// `$VISUAL` then `$EDITOR`. Returns `None` if none are set.
pub(super) fn resolve_editor_command(db: &crate::storage::Database) -> Option<String> {
    if let Ok(Some(cmd)) = db.get_editor_command() {
        let trimmed = cmd.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    for var in ["VISUAL", "EDITOR"] {
        if let Ok(val) = std::env::var(var) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_editor_rejects_empty_paths() {
        let err = open_in_editor(&[], "vim").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn open_in_editor_rejects_blank_command() {
        // Whitespace-only command splits to no program token.
        let paths = [PathBuf::from("/tmp/x")];
        let err = open_in_editor(&paths, "   ").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn open_in_editor_parses_program_then_fails_to_spawn_unknown_binary() {
        // A non-blank command with flags gets past the empty-command guard
        // (proving the whitespace-split parsing ran) and then fails at spawn
        // because the program does not exist — deterministic and cross-platform.
        let paths = [PathBuf::from("/tmp/x")];
        let err =
            open_in_editor(&paths, "thurbox-nonexistent-editor-xyz --wait --flag").unwrap_err();
        // NOT the InvalidInput we return for an empty command: the spawn itself failed.
        assert_ne!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn resolve_editor_command_prefers_trimmed_db_value() {
        let db = crate::storage::Database::open_in_memory().unwrap();
        db.set_editor_command("  code --wait  ").unwrap();
        assert_eq!(resolve_editor_command(&db), Some("code --wait".to_string()));
    }

    #[test]
    fn resolve_editor_command_falls_back_to_env_when_db_blank() {
        // All env mutation lives in this one test so it can't race a parallel
        // test reading the same vars; originals are restored before returning.
        let saved_visual = std::env::var("VISUAL").ok();
        let saved_editor = std::env::var("EDITOR").ok();

        let db = crate::storage::Database::open_in_memory().unwrap();
        db.set_editor_command("   ").unwrap(); // blank DB value is ignored

        // VISUAL wins over EDITOR.
        std::env::set_var("VISUAL", "  helix  ");
        std::env::set_var("EDITOR", "nano");
        assert_eq!(resolve_editor_command(&db), Some("helix".to_string()));

        // Falls through to EDITOR when VISUAL is blank.
        std::env::set_var("VISUAL", "  ");
        assert_eq!(resolve_editor_command(&db), Some("nano".to_string()));

        // None of them set (and DB blank) → None.
        std::env::remove_var("VISUAL");
        std::env::remove_var("EDITOR");
        assert_eq!(resolve_editor_command(&db), None);

        match saved_visual {
            Some(v) => std::env::set_var("VISUAL", v),
            None => std::env::remove_var("VISUAL"),
        }
        match saved_editor {
            Some(v) => std::env::set_var("EDITOR", v),
            None => std::env::remove_var("EDITOR"),
        }
    }
}
