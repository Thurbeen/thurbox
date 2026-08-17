//! Miscellaneous helper functions used by the app module.

use std::io::Write;
use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::prelude::IntoCrossterm;
use ratatui::style::Style;
use unicode_width::UnicodeWidthChar;

use crate::session::hyperlink;
use crate::session::settings::EditorMode;

/// One drawn hyperlink run to re-emit to the outer terminal: where it sits on
/// screen, what it points at, and the exact cells the frame painted there (so
/// re-printing them changes nothing visible).
pub(super) struct HyperlinkPaint {
    pub x: u16,
    pub y: u16,
    pub url: String,
    pub cells: Vec<(String, Style)>,
}

/// The cells `label` occupies in the frame just drawn, or `None` when the pane
/// no longer prints it there.
///
/// The check is what makes the re-paint safe: a modal, the review view, a
/// scrolled pane or a repainted row all fail it, so nothing is written over
/// content that has moved on. A label running past the pane's right edge is
/// linked as far as it is visible.
pub(super) fn drawn_label_cells(
    buf: &Buffer,
    inner: Rect,
    x: u16,
    y: u16,
    label: &str,
) -> Option<Vec<(String, Style)>> {
    let mut cells = Vec::new();
    let mut cx = x;
    for ch in label.chars() {
        // A wide glyph owns two cells: advancing by its width skips ratatui's
        // filler cell, matching what re-printing the glyph does to the cursor.
        let width = UnicodeWidthChar::width(ch).unwrap_or(1).max(1) as u16;
        if cx.saturating_add(width) > inner.right() {
            break;
        }
        let cell = buf.cell(Position::new(cx, y))?;
        if !cell.symbol().starts_with(ch) {
            return None;
        }
        cells.push((
            cell.symbol().to_string(),
            Style::new()
                .fg(cell.fg)
                .bg(cell.bg)
                .add_modifier(cell.modifier),
        ));
        cx += width;
    }
    (!cells.is_empty()).then_some(cells)
}

/// Re-print each run wrapped in OSC 8, so the terminal thurbox runs in knows
/// those cells are a link and can offer its own open-link gesture (in Windows
/// Terminal, kitty, WezTerm, iTerm2 …, a `Ctrl`/`Cmd`+click that opens the
/// user's own browser — the only route to a browser when thurbox runs on a
/// remote host).
///
/// Called after the frame is flushed and re-prints the *same* glyphs with the
/// *same* styles, so nothing changes visually; only the terminal's hyperlink
/// state is added. Writes go to `stdout` after the backend's flush, so they
/// cannot interleave with the frame.
pub(super) fn paint_hyperlinks(paints: &[HyperlinkPaint]) -> std::io::Result<()> {
    use crossterm::cursor::MoveTo;
    use crossterm::queue;
    use crossterm::style::{Print, PrintStyledContent, ResetColor};

    let mut out = std::io::stdout();
    for paint in paints {
        queue!(
            out,
            MoveTo(paint.x, paint.y),
            Print(hyperlink::osc8_open(&paint.url))
        )?;
        for (symbol, style) in &paint.cells {
            let content = (*style).into_crossterm();
            queue!(out, PrintStyledContent(content.apply(symbol.as_str())))?;
        }
        queue!(out, Print(hyperlink::OSC8_CLOSE), ResetColor)?;
    }
    out.flush()
}

/// Hand `url` to the platform's URL opener.
///
/// `Err` carries a short, user-facing reason the machine can't open a browser —
/// the common case being a thurbox running on a headless or SSH host, where the
/// caller falls back to putting the URL on the clipboard instead. The child is
/// **not** waited on (a browser launcher can take seconds, and the render loop
/// must not park), so a spawn that succeeds is reported as opened.
pub(super) fn open_url(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let (program, args): (&str, Vec<&str>) = ("open", vec![url]);
    #[cfg(target_os = "windows")]
    // `start` is a cmd builtin; its first quoted argument is the window title,
    // so an empty one keeps the URL from being eaten as one.
    let (program, args): (&str, Vec<&str>) = ("cmd", vec!["/C", "start", "", url]);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let (program, args): (&str, Vec<&str>) = {
        // Spawning `xdg-open` with nothing to open into either fails or, worse,
        // succeeds and does nothing — so decide up front rather than report a
        // browser that never appeared.
        if !has_browser_target(
            std::env::var("DISPLAY").ok().as_deref(),
            std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
            std::env::var("BROWSER").ok().as_deref(),
        ) {
            return Err("No display to open a browser on".into());
        }
        ("xdg-open", vec![url])
    };

    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("No URL opener ({program} not installed)"),
            _ => format!("Could not run {program}: {e}"),
        })
}

/// Whether anything on this machine could show a URL: a display server, or a
/// `BROWSER` the user set themselves (which overrides the display check — they
/// have told us how to open a URL, e.g. a terminal browser).
///
/// Unused on macOS/Windows, whose openers need no display server.
#[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
fn has_browser_target(
    display: Option<&str>,
    wayland_display: Option<&str>,
    browser: Option<&str>,
) -> bool {
    [display, wayland_display, browser]
        .iter()
        .any(|value| value.is_some_and(|v| !v.trim().is_empty()))
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

// The editor classification itself is agent- and interface-neutral data, so it
// lives in `session::editor` where the v2 kernel can reach it too — a `Ctrl+O`
// must not decide terminal-vs-GUI differently depending on which binary you ran.
pub(super) use crate::session::editor::{is_terminal_editor, parse_editor_command};

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
    fn browser_target_needs_a_display_or_an_explicit_browser() {
        // The SSH / headless case: nothing to open into, so the caller falls
        // back to the clipboard instead of spawning an opener that goes nowhere.
        assert!(!has_browser_target(None, None, None));
        // An empty var is as good as unset (a stripped SSH environment).
        assert!(!has_browser_target(Some(""), Some(" "), Some("")));

        assert!(has_browser_target(Some(":0"), None, None));
        assert!(has_browser_target(None, Some("wayland-0"), None));
        // `BROWSER` is the user telling us how to open a URL without a display.
        assert!(has_browser_target(None, None, Some("firefox")));
    }

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
