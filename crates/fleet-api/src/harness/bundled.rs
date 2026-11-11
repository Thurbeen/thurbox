//! The harness profiles Fleet ships, one TOML document per harness. They are
//! constants, not files: a pure crate reads nothing at build time. A user
//! file of the same `name` in Fleet's config dir replaces one
//! (docs/fleet/controllers/task.md, *Harness profiles*).

/// Claude Code. Its hooks are one `--settings`: Claude Code keeps the last
/// of several, so a second would drop Fleet's hooks or the other's. Only a
/// permission prompt needs a person: Claude Code also notifies after 60 s of
/// input idle (`idle_prompt`), which a matcher-less hook would report as
/// attention.
pub const CLAUDE: &str = r#"
name = "claude"
program = "claude"
aliases = ["claude-code"]
authority = "full"

[launch]
args = [
  "--settings",
  '{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"fleet signal idle 2>/dev/null || true"}]}],"UserPromptSubmit":[{"hooks":[{"type":"command","command":"fleet signal prompt 2>/dev/null || true"}]}],"PreToolUse":[{"hooks":[{"type":"command","command":"fleet signal working 2>/dev/null || true"}]}],"Notification":[{"matcher":"permission_prompt","hooks":[{"type":"command","command":"fleet signal blocked 2>/dev/null || true"}]}],"Stop":[{"hooks":[{"type":"command","command":"fleet signal idle 2>/dev/null || true"}]}]}}',
]
add_dir = "--add-dir"

[resume]
args = ["{program}", "{launch_args}", "--resume", "{conversation}"]

[trust]
mode = "dialog"

[payload]
prompt = ["prompt"]
conversation = ["session_id"]
"#;

/// Codex. `notify` runs at each turn's end with the turn as JSON, its
/// prompts included, as the last argument. The trust override is one
/// `projects` key with a TOML inline table: Codex splits a dotted `-c` key
/// on every `.`, and a path has them.
pub const CODEX: &str = r#"
name = "codex"
program = "codex"
authority = "partial"

[launch]
args = ["-c", 'notify=["fleet","signal","idle"]']
add_dir = "--add-dir"

[resume]
args = ["{program}", "resume", "{conversation}", "{launch_args}"]

[trust]
mode = "launch-override"
args = ["-c", 'projects={"{root}"={trust_level="trusted"}}']

[payload]
prompt = ["input-messages"]
conversation = ["thread-id"]
"#;

/// Every bundled profile, in the order `fleet version --json` lists them.
pub const ALL: &[&str] = &[CLAUDE, CODEX];
