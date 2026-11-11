//! The screen rule sets Fleet ships, one TOML document per harness, named
//! as the harness profile that uses it. Each rule was read off the agent's
//! own screen; crates/fleet-api/tests/screen_rules.rs holds those screens
//! and checks every rule against them.

/// Claude Code: its workspace-trust dialog and a permission question, which
/// both wait on a person, the interrupt hint of a running turn, and the
/// composer between its two rules. Its title keeps `✳`
/// whether a turn runs or not, so no rule reads it.
pub const CLAUDE: &str = r#"
[[rule]]
id = "permission"
state = "blocked"
priority = 900
region = "bottom_lines(12)"
contains = ["Do you want to proceed?", "Esc to cancel"]

[[rule]]
id = "trust"
state = "blocked"
priority = 900
region = "whole"
contains = ["Accessing workspace:", "Yes, I trust this folder", "Enter to confirm"]

[[rule]]
id = "interrupt"
state = "working"
priority = 500
region = "bottom_lines(3)"
contains = ["esc to interrupt"]

[[rule]]
id = "composer"
state = "idle"
priority = 100
region = "bottom_lines(4)"
contains = ["────", "❯"]
"#;

/// Codex: its title says when an approval waits, and spins while a turn
/// runs; the status line under the turn is gone while text streams.
pub const CODEX: &str = r#"
[[rule]]
id = "action-required"
state = "blocked"
priority = 900
region = "osc_title"
contains = ["Action Required"]

[[rule]]
id = "spinner"
state = "working"
priority = 500
region = "osc_title"
any = [
  { contains = ["⠋"] }, { contains = ["⠙"] }, { contains = ["⠹"] }, { contains = ["⠸"] },
  { contains = ["⠼"] }, { contains = ["⠴"] }, { contains = ["⠦"] }, { contains = ["⠧"] },
  { contains = ["⠇"] }, { contains = ["⠏"] },
]

[[rule]]
id = "composer"
state = "idle"
priority = 100
region = "bottom_lines(3)"
contains = ["? for shortcuts"]
"#;

/// GitHub Copilot CLI: a selection dialog (a permission or the folder
/// trust), the working line of its footer, and its idle footer.
pub const COPILOT: &str = r#"
[[rule]]
id = "select"
state = "blocked"
priority = 900
region = "bottom_lines(3)"
contains = ["enter to select", "esc to cancel"]

[[rule]]
id = "working"
state = "working"
priority = 500
region = "bottom_lines(1)"
contains = ["Working", "esc"]

[[rule]]
id = "commands"
state = "idle"
priority = 100
region = "bottom_lines(1)"
contains = ["/ commands"]
"#;

/// opencode: its permission panel, the interrupt hint of a running turn,
/// and its footer.
pub const OPENCODE: &str = r#"
[[rule]]
id = "permission"
state = "blocked"
priority = 900
region = "bottom_lines(10)"
contains = ["△ Permission required"]

[[rule]]
id = "interrupt"
state = "working"
priority = 500
region = "bottom_lines(1)"
contains = ["esc interrupt"]

[[rule]]
id = "commands"
state = "idle"
priority = 100
region = "bottom_lines(1)"
contains = ["ctrl+p commands"]
"#;

/// pi: a busy turn offers to cancel above its editor, which sits between
/// two rules over the footer. pi asks no permission, so no rule blocks.
pub const PI: &str = r#"
[[rule]]
id = "escape-to-cancel"
state = "working"
priority = 500
region = "bottom_lines(8)"
contains = ["(escape to cancel)"]

[[rule]]
id = "editor"
state = "idle"
priority = 100
region = "bottom_lines(6)"
contains = ["────────"]
"#;

/// Every bundled set, by name.
pub const ALL: &[(&str, &str)] = &[
    ("claude", CLAUDE),
    ("codex", CODEX),
    ("copilot", COPILOT),
    ("opencode", OPENCODE),
    ("pi", PI),
];
