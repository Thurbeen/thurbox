//! Miscellaneous helper functions used by the app module.

use std::path::PathBuf;

use crate::session::settings::EditorMode;

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
/// error if the command string is empty or fails to spawn. This is the
/// **detached** path for GUI editors (launchers that fork-and-exit / open
/// their own window); terminal editors must instead get a real TTY via
/// [`classify_editor`] + the main loop's popup/suspend path.
pub(super) fn open_in_editor(paths: &[PathBuf], editor_cmd: &str) -> std::io::Result<()> {
    let (program, extra_args) = parse_editor_command(editor_cmd)?;
    if paths.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no paths to open",
        ));
    }

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

/// Split a configured editor command into `(program, extra_args)`. Whitespace-
/// split; the program is the first token. Errors on an empty/whitespace-only
/// command (no program token).
pub(super) fn parse_editor_command(editor_cmd: &str) -> std::io::Result<(String, Vec<String>)> {
    let mut parts = editor_cmd.split_whitespace();
    let Some(program) = parts.next() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "editor command is empty",
        ));
    };
    let extra_args: Vec<String> = parts.map(String::from).collect();
    Ok((program.to_string(), extra_args))
}

/// Editors known to run **in the terminal** (need a controlling TTY). Matched
/// against the configured program basename, case-insensitively. Deliberately
/// the well-known set only: a terminal editor left off the list just falls
/// back to the detached GUI path (override with `editor mode terminal`),
/// whereas a GUI launcher wrongly on this list would get a TTY and misbehave —
/// so precision matters more than recall here.
const TERMINAL_EDITORS: &[&str] = &[
    "vim",
    "nvim",
    "vi",
    "view",
    "ex",
    "nano",
    "pico",
    "joe",
    "jmacs",
    "jstar",
    "jpico",
    "jove",
    "jed",
    "ne",
    "mcedit",
    "mc",
    "kak",
    "kakoune",
    "micro",
    "helix",
    "hx",
    "amp",
    "emacsclient",
    "ttt",
];

/// Editors that open their **own window** (launcher scripts that fork-and-exit
/// or message a running server, independent of any terminal). Matched
/// case-insensitively on the basename.
const GUI_EDITORS: &[&str] = &[
    "code",
    "code-insiders",
    "codium",
    "vscodium",
    "vscode",
    "cursor",
    "zed",
    "zeditor",
    "subl",
    "sublime",
    "atom",
    "mate",
    "idea",
    "idea.sh",
    "webstorm",
    "phpstorm",
    "pycharm",
    "goland",
    "clion",
    "rust-rover",
    "rustrover",
    "datagrip",
    "rubymine",
    "fleet",
    "nova",
    "bbedit",
    "textmate",
    "notepad++",
    "notepad-plus-plus",
    "codeblocks",
    "geany",
    "leafpad",
    "mousepad",
    "pluma",
    "gedit",
    "kate",
    "kwrite",
    "xfwrite",
];

/// Flags that force a GUI editor onto the terminal path (it then runs with a
/// real TTY). `emacs -nw` / `--no-window-system` flip emacs; `--tty` /
/// `--terminal` are the generic spellings some launchers accept. (`emacsclient`
/// is already in [`TERMINAL_EDITORS`], so its `-t` needs no flag here — and
/// bare `-t` is too generic a token to match.)
const FORCE_TERMINAL_FLAGS: &[&str] = &["-nw", "--no-window-system", "--tty", "--terminal"];

/// The basename of a command path (`/usr/bin/nvim` → `nvim`), or the whole
/// string when it carries no separator.
fn basename_of(program: &str) -> &str {
    match program.rfind(['/', '\\']) {
        Some(i) => &program[i + 1..],
        None => program,
    }
}

/// Whether the resolved editor should be launched with a real TTY (the terminal
/// path) rather than detached. Honors an explicit [`EditorMode`] override; in
/// `Auto` mode it consults the curated lists plus the `emacs -nw`-style flag.
///
/// `Auto` falls back to **GUI** (detached) for unknown editors so an unlisted
/// GUI launcher keeps working as before — force the TTY path with
/// `editor mode terminal` for an unlisted terminal editor.
pub(super) fn is_terminal_editor(program: &str, extra_args: &[String], mode: EditorMode) -> bool {
    match mode {
        EditorMode::Terminal => true,
        EditorMode::Gui => false,
        EditorMode::Auto => {
            let base = basename_of(program);
            let listed_terminal = TERMINAL_EDITORS
                .iter()
                .any(|e| e.eq_ignore_ascii_case(base));
            let listed_gui = GUI_EDITORS.iter().any(|g| g.eq_ignore_ascii_case(base));
            let force_term = extra_args
                .iter()
                .any(|a| FORCE_TERMINAL_FLAGS.iter().any(|f| a == *f));
            // `emacs` is GUI by default; `-nw`/`--no-window-system` forces terminal.
            force_term || (listed_terminal && !listed_gui)
        }
    }
}

/// How the main loop should run the editor for one `Ctrl+O` press.
pub(super) enum EditorLaunch {
    /// GUI editor: fire-and-forget detached spawn (the classic path), done
    /// here in `helpers` so it never touches the render loop.
    Detached,
    /// Terminal editor: hand the constructed invocation to the main loop,
    /// which runs it with a real TTY (`tmux display-popup` inside tmux, or a
    /// TUI suspend-and-resume outside). The `paths` are appended to `args`.
    Terminal(crate::app::EditorInvocation),
}

/// Resolve the editor command + mode, parse it, and decide detached vs TTY.
/// Returns `Err` on an empty/unparseable command string.
pub(super) fn classify_editor(
    paths: &[PathBuf],
    editor_cmd: &str,
    mode: EditorMode,
) -> std::io::Result<EditorLaunch> {
    let (program, extra_args) = parse_editor_command(editor_cmd)?;
    if is_terminal_editor(&program, &extra_args, mode) {
        let mut args = extra_args;
        args.extend(paths.iter().map(|p| p.to_string_lossy().into_owned()));
        Ok(EditorLaunch::Terminal(crate::app::EditorInvocation {
            program,
            args,
        }))
    } else {
        Ok(EditorLaunch::Detached)
    }
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

/// Resolve the editor launch mode from the DB setting (defaults to `Auto`).
pub(super) fn resolve_editor_mode(db: &crate::storage::Database) -> EditorMode {
    db.get_editor_mode().unwrap_or(EditorMode::Auto)
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
    fn parse_editor_command_splits_program_and_flags() {
        let (program, args) = parse_editor_command("code --wait --new-window").unwrap();
        assert_eq!(program, "code");
        assert_eq!(args, vec!["--wait", "--new-window"]);
    }

    #[test]
    fn parse_editor_command_rejects_blank() {
        assert_eq!(
            parse_editor_command("  ").unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn is_terminal_editor_auto_detects_known_names() {
        // Terminal editors by basename (the ttt/vim/nano family).
        for name in [
            "vim", "nvim", "vi", "nano", "ttt", "helix", "hx", "micro", "kak",
        ] {
            assert!(
                is_terminal_editor(name, &[], EditorMode::Auto),
                "{name} should be terminal"
            );
        }
        // …and full paths (basename match).
        assert!(is_terminal_editor("/usr/bin/nvim", &[], EditorMode::Auto));
        // Case-insensitive.
        assert!(is_terminal_editor("VIM", &[], EditorMode::Auto));
    }

    #[test]
    fn is_terminal_editor_auto_treats_gui_launchers_as_detached() {
        for name in ["code", "code-insiders", "zed", "cursor", "subl", "idea"] {
            assert!(
                !is_terminal_editor(name, &[], EditorMode::Auto),
                "{name} should be GUI"
            );
        }
    }

    #[test]
    fn is_terminal_editor_emacs_nw_forces_terminal() {
        // `emacs` alone is GUI; `-nw` flips it to terminal.
        assert!(!is_terminal_editor("emacs", &[], EditorMode::Auto));
        assert!(
            is_terminal_editor("emacs", &["-nw".into()], EditorMode::Auto),
            "emacs -nw must be terminal"
        );
        assert!(is_terminal_editor(
            "emacs",
            &["--no-window-system".into()],
            EditorMode::Auto
        ));
    }

    #[test]
    fn is_terminal_editor_auto_defaults_unknown_to_gui() {
        // Unknown editor → no regression of the old detached behavior.
        assert!(!is_terminal_editor(
            "my-custom-editor",
            &[],
            EditorMode::Auto
        ));
    }

    #[test]
    fn is_terminal_editor_mode_override_wins_over_name() {
        // `Terminal` forces TTY even for `code`; `Gui` forces detached even for vim.
        assert!(is_terminal_editor("code", &[], EditorMode::Terminal));
        assert!(!is_terminal_editor("vim", &[], EditorMode::Gui));
    }

    #[test]
    fn classify_editor_terminal_appends_paths_to_args() {
        let paths = [PathBuf::from("/repo/a"), PathBuf::from("/repo/b")];
        let launch = classify_editor(&paths, "ttt", EditorMode::Auto).unwrap();
        let inv = match launch {
            EditorLaunch::Terminal(inv) => inv,
            EditorLaunch::Detached => panic!("ttt should classify as terminal"),
        };
        assert_eq!(inv.program, "ttt");
        assert_eq!(inv.args, vec!["/repo/a", "/repo/b"]);
    }

    #[test]
    fn classify_editor_terminal_keeps_extra_flags_before_paths() {
        let paths = [PathBuf::from("/r")];
        let launch = classify_editor(&paths, "nvim --clean", EditorMode::Auto).unwrap();
        let inv = match launch {
            EditorLaunch::Terminal(inv) => inv,
            EditorLaunch::Detached => panic!(),
        };
        assert_eq!(inv.program, "nvim");
        assert_eq!(inv.args, vec!["--clean", "/r"]);
    }

    #[test]
    fn classify_editor_gui_yields_detached() {
        let paths = [PathBuf::from("/r")];
        assert!(matches!(
            classify_editor(&paths, "code --wait", EditorMode::Auto).unwrap(),
            EditorLaunch::Detached
        ));
    }

    #[test]
    fn resolve_editor_mode_defaults_auto_on_missing_row() {
        let db = crate::storage::Database::open_in_memory().unwrap();
        assert_eq!(resolve_editor_mode(&db), EditorMode::Auto);
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
