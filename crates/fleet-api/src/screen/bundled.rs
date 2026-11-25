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

pub const ALL: &[(&str, &str)] = &[
    ("claude", CLAUDE),
    ("codex", CODEX),
    ("copilot", COPILOT),
    ("opencode", OPENCODE),
    ("pi", PI),
];
