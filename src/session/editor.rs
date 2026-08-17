//! Which editor wants a terminal, and which wants to be left alone.
//!
//! Pure classification over a configured command string: no spawning, no
//! settings lookup, no interface. It sits here rather than beside either
//! interface's launcher because both v1's `Ctrl+O` and the v2 kernel's have to
//! reach the same verdict — a GUI launcher handed a TTY misbehaves, and a
//! terminal editor spawned detached vanishes, whichever binary made the mistake.

use super::settings::EditorMode;

/// Split a configured editor command into `(program, extra_args)`. Whitespace-
/// split; the program is the first token. Errors on an empty/whitespace-only
/// command (no program token).
pub fn parse_editor_command(editor_cmd: &str) -> std::io::Result<(String, Vec<String>)> {
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
pub fn is_terminal_editor(program: &str, extra_args: &[String], mode: EditorMode) -> bool {
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
