//! The harness profiles Fleet ships, one TOML document per harness. They are
//! constants, not files: a pure crate reads nothing at build time. The hook
//! files pi's, opencode's, kilo's, omp's, qwen's and Copilot's name are the
//! runtime adapter's (`fleet_runtime::assets`); the block Kimi's `[install]` names is
//! written by `fleet integration`. A user file of the same `name` in
//! Fleet's config dir replaces one (docs/fleet/controllers/task.md, *Harness
//! profiles*).

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

/// pi. Its hooks are an extension file Fleet writes, passed with `-e`,
/// which loads beside the user's own extensions. pi asks a person nothing of
/// its own, so its hooks report no blocked state. `--approve` trusts the
/// project for this run only, as Codex's launch override does: pi otherwise
/// asks first in a directory with project resources (`.pi`, `.agents`), its
/// parents' included.
pub const PI: &str = r#"
name = "pi"
program = "pi"
authority = "full"

[launch]
args = ["-e", "{asset:fleet-pi.ts}"]

[resume]
args = ["{program}", "{launch_args}", "--session", "{conversation}"]

[trust]
mode = "launch-override"
args = ["--approve"]

[payload]
prompt = ["prompt"]
conversation = ["session_id"]
"#;

/// opencode (OpenCode 2). Its hooks are a plugin directory Fleet writes,
/// named in `OPENCODE_CONFIG_CONTENT`, which opencode merges with the
/// user's and the project's config. It runs its own server: the shared
/// background service was started with another environment, and loads
/// neither the plugin nor the session's signal file.
pub const OPENCODE: &str = r#"
name = "opencode"
program = "opencode"
authority = "full"

[launch]
args = ["--standalone"]
env = { OPENCODE_CONFIG_CONTENT = '{"plugins":["{asset:fleet-opencode}"]}' }

[resume]
args = ["{program}", "{launch_args}", "--session", "{conversation}"]

[payload]
prompt = ["prompt"]
conversation = ["session_id"]
"#;

/// kilo, an opencode fork with the first plugin API. Its hooks are two
/// files Fleet writes: a server plugin named in `KILO_CONFIG_CONTENT`, and a
/// terminal UI config naming a TUI plugin, in `KILO_TUI_CONFIG`. kilo merges
/// each with the user's and the project's config. `KILO_NO_DAEMON` keeps the
/// server in kilo's own process: a running background service was started
/// with another environment, and would load neither the plugin nor the
/// session's signal file.
pub const KILO: &str = r#"
name = "kilo"
program = "kilo"
authority = "full"

[launch]
env = { KILO_CONFIG_CONTENT = '{"plugin":["{asset:fleet-kilo/index.js}"]}', KILO_TUI_CONFIG = "{asset:fleet-kilo/tui.json}", KILO_NO_DAEMON = "1" }

[resume]
args = ["{program}", "{launch_args}", "--session", "{conversation}"]

[payload]
prompt = ["prompt"]
conversation = ["session_id"]
"#;

/// omp, a pi fork. Its hooks are an extension file Fleet writes, passed with
/// `-e`, which loads beside the user's own extensions. Unlike pi, omp asks
/// before a tool runs when its approval mode says so, and trusts every
/// project. `OMP_SKIP_SETUP` skips the setup wizard of omp's first start:
/// Fleet never answers it, as it never answers pi's trust question.
pub const OMP: &str = r#"
name = "omp"
program = "omp"
authority = "full"

[launch]
args = ["-e", "{asset:fleet-omp.ts}"]
env = { OMP_SKIP_SETUP = "1" }
add_dir = "--add-dir"

[resume]
args = ["{program}", "{launch_args}", "--resume", "{conversation}"]

[payload]
prompt = ["prompt"]
conversation = ["session_id"]
"#;

/// Qwen Code. Its hooks are a settings file Fleet writes, named in
/// `QWEN_CODE_SYSTEM_DEFAULTS_PATH`: qwen merges the hooks of its system
/// defaults, user, project and system settings. That path replaces the
/// system defaults file's, which a Fleet session does not read. The system
/// settings path is left alone: qwen derives the defaults path from it when
/// none is given, so naming it would hide both files.
pub const QWEN: &str = r#"
name = "qwen"
program = "qwen"
authority = "full"

[launch]
env = { QWEN_CODE_SYSTEM_DEFAULTS_PATH = "{asset:fleet-qwen.json}" }
add_dir = "--add-dir"

[resume]
args = ["{program}", "{launch_args}", "--resume", "{conversation}"]

[payload]
prompt = ["prompt"]
conversation = ["session_id"]
"#;

/// GitHub Copilot CLI. Its hooks are a plugin directory Fleet writes, passed
/// with `--plugin-dir`, which loads beside the user's plugins and hooks.
/// Copilot runs `permissionRequest` for every permission check, an allowed
/// one included, so the plugin reports `blocked` from the notification that
/// a person is asked. Copilot starts its session, and reports it, at its
/// first message.
pub const COPILOT: &str = r#"
name = "copilot"
program = "copilot"
authority = "full"

[launch]
args = ["--plugin-dir", "{asset:fleet-copilot}"]
add_dir = "--add-dir"

[resume]
args = ["{program}", "{launch_args}", "--resume", "{conversation}"]

[trust]
mode = "dialog"

[payload]
prompt = ["prompt"]
conversation = ["sessionId"]
"#;

/// Kimi Code (`kimi`). It reads hooks from its own `config.toml` only: no
/// flag or variable adds any at launch, and `KIMI_CODE_HOME` moves its
/// whole home, sign-in included. So its hooks are a block `fleet
/// integration install kimi` appends to that file. Each command is a no-op
/// outside a Fleet session, and a missing `fleet` never fails a turn. Kimi
/// starts its session, and reports it, at its first message.
pub const KIMI: &str = r#"
name = "kimi"
program = "kimi"
authority = "full"

[launch]
add_dir = "--add-dir"

[resume]
args = ["{program}", "{launch_args}", "--session", "{conversation}"]

[payload]
prompt = ["prompt"]
conversation = ["session_id"]

[install]
version = 1
dir_env = ["KIMI_CODE_HOME"]
dir = "~/.kimi-code"

[[install.blocks]]
path = "config.toml"
text = '''
[[hooks]]
event = "SessionStart"
command = '[ -z "$FLEET_SIGNAL_FILE" ] || fleet signal idle --harness kimi 2>/dev/null || true'
timeout = 10

[[hooks]]
event = "UserPromptSubmit"
command = '[ -z "$FLEET_SIGNAL_FILE" ] || fleet signal prompt --harness kimi 2>/dev/null || true'
timeout = 10

[[hooks]]
event = "PreToolUse"
command = '[ -z "$FLEET_SIGNAL_FILE" ] || fleet signal working --harness kimi 2>/dev/null || true'
timeout = 10

[[hooks]]
event = "PermissionRequest"
command = '[ -z "$FLEET_SIGNAL_FILE" ] || fleet signal blocked --harness kimi 2>/dev/null || true'
timeout = 10

[[hooks]]
event = "PermissionResult"
command = '[ -z "$FLEET_SIGNAL_FILE" ] || fleet signal working --harness kimi 2>/dev/null || true'
timeout = 10

[[hooks]]
event = "Stop"
command = '[ -z "$FLEET_SIGNAL_FILE" ] || fleet signal idle --harness kimi 2>/dev/null || true'
timeout = 10

[[hooks]]
event = "StopFailure"
command = '[ -z "$FLEET_SIGNAL_FILE" ] || fleet signal idle --harness kimi 2>/dev/null || true'
timeout = 10

[[hooks]]
event = "Interrupt"
command = '[ -z "$FLEET_SIGNAL_FILE" ] || fleet signal idle --harness kimi 2>/dev/null || true'
timeout = 10
'''
"#;

/// Every bundled profile, in the order `fleet version --json` lists them.
pub const ALL: &[&str] = &[CLAUDE, CODEX, PI, OPENCODE, KILO, OMP, QWEN, COPILOT, KIMI];
