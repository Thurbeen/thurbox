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
env = { CLAUDE_CODE_MESSAGING_SOCKET = "" }

[resume]
args = ["{program}", "{launch_args}", "--resume", "{conversation}"]

[trust]
mode = "dialog"

[payload]
prompt = ["prompt"]
conversation = ["session_id"]
paste = "pasted_content"

[inbox]
address = "CLAUDE_CODE_MESSAGING_SOCKET"
args = ["fleet", "inbox-post", "{address}", "--", "{text}"]

[telemetry]
env = { CLAUDE_CODE_ENABLE_TELEMETRY = "1", OTEL_METRICS_EXPORTER = "otlp", OTEL_LOGS_EXPORTER = "otlp", OTEL_EXPORTER_OTLP_ENDPOINT = "{endpoint}", OTEL_EXPORTER_OTLP_PROTOCOL = "{protocol}", OTEL_EXPORTER_OTLP_HEADERS = "{headers}", OTEL_SERVICE_NAME = "claude", OTEL_RESOURCE_ATTRIBUTES = "{resource}" }
"#;

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

[inbox]
args = ["{program}", "queue", "--thread", "{conversation}", "--message={text}"]

[telemetry]
args = [
  "-c", 'otel.exporter={otlp-http={endpoint="{endpoint}/v1/logs",protocol="binary"}}',
  "-c", 'otel.trace_exporter={otlp-http={endpoint="{endpoint}/v1/traces",protocol="binary"}}',
  "-c", 'otel.metrics_exporter={otlp-http={endpoint="{endpoint}/v1/metrics",protocol="binary"}}',
]
env = { OTEL_RESOURCE_ATTRIBUTES = "{resource}" }
"#;

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

[telemetry]
env = { OTEL_EXPORTER_OTLP_ENDPOINT = "{endpoint}", OTEL_EXPORTER_OTLP_HEADERS = "{headers}", OTEL_RESOURCE_ATTRIBUTES = "{resource}" }
"#;

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

[telemetry]
env = { OTEL_EXPORTER_OTLP_ENDPOINT = "{endpoint}", OTEL_EXPORTER_OTLP_PROTOCOL = "{protocol}", OTEL_EXPORTER_OTLP_HEADERS = "{headers}", OTEL_SERVICE_NAME = "omp", OTEL_RESOURCE_ATTRIBUTES = "{resource}" }
"#;

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

[telemetry]
env = { QWEN_TELEMETRY_ENABLED = "true", QWEN_TELEMETRY_OTLP_ENDPOINT = "{endpoint}", QWEN_TELEMETRY_OTLP_PROTOCOL = "http", QWEN_TELEMETRY_LOG_PROMPTS = "false", OTEL_EXPORTER_OTLP_HEADERS = "{headers}", OTEL_SERVICE_NAME = "qwen", OTEL_RESOURCE_ATTRIBUTES = "{resource}" }
"#;

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

[telemetry]
env = { COPILOT_OTEL_ENABLED = "true", OTEL_EXPORTER_OTLP_ENDPOINT = "{endpoint}", OTEL_EXPORTER_OTLP_PROTOCOL = "{protocol}", OTEL_EXPORTER_OTLP_HEADERS = "{headers}", OTEL_SERVICE_NAME = "copilot", OTEL_RESOURCE_ATTRIBUTES = "{resource}" }
"#;

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

pub const ALL: &[&str] = &[CLAUDE, CODEX, PI, OPENCODE, KILO, OMP, QWEN, COPILOT, KIMI];
