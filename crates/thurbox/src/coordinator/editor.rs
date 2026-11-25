use thurbox::session::settings::EditorMode;

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

const FORCE_TERMINAL_FLAGS: &[&str] = &["-nw", "--no-window-system", "--tty", "--terminal"];

fn basename_of(program: &str) -> &str {
    match program.rfind(['/', '\\']) {
        Some(i) => &program[i + 1..],
        None => program,
    }
}

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
