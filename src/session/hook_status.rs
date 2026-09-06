//! What a session's hooks-driven state *is*, and what it is *worth* — the one
//! vocabulary ([`SessionState`]), the read-time rules that derive it, and the
//! age, coverage and pane agreement that say how much to trust it.
//!
//! It lives in `session` rather than in either consumer because both the
//! interface and `thurbox-cli` have to answer "what state is this session in",
//! and while they each derived it themselves they answered different words for
//! one row — an acknowledged turn was `idle` on screen and `done` on `session
//! get` for the rest of the session's life. `session` is the pure module both
//! may import (`tests/architecture_rules.rs`), so the rules live here and each
//! caller applies the folds whose inputs it can actually observe:
//! [`derive_state`] everywhere (its inputs are all stored columns),
//! [`with_output_quiescence`] and [`with_reachability`] only where there is a
//! live terminal to ask.
//!
//! [`crate::session::HOOK_STATES`] is the agent's half of that vocabulary, and
//! the rest of this module is the honesty around it. `hook_state` is latched:
//! it is whatever was written last, by an agent that may since have crashed,
//! been interrupted, or never have been wired to report at all. A consumer reading the bare word cannot tell
//! `idle` ("the agent says it is at rest") from `idle` ("this agent has no hook
//! coverage and never said anything"), nor `working` ("a turn is running") from
//! `working` ("a turn was running an hour ago and the agent is gone").
//!
//! Three additive answers, none of which overwrite the stored state:
//!
//! - **Age** — [`age_secs`]. A stamp plus a duration lets a consumer apply its
//!   own policy instead of trusting a bare word. Deliberately *not* a built-in
//!   timeout: a turn may legitimately run for an hour, and a guessed bound
//!   would report it finished. An interface has a better signal
//!   ([`with_output_quiescence`]) which needs a live pane and so cannot be
//!   asked headless.
//! - **Coverage** — [`coverage_for`]. Which states an agent's wiring *can*
//!   produce, from the built-in `hooks` extension's payloads. `aider` reports
//!   only `blocked`; a user's own agent reports nothing. Absence of `working`
//!   means one thing for `claude` and nothing at all for `aider`.
//! - **Corroboration** — [`classify_foreground`]. What actually holds the
//!   pane's tty, from the foreground process group. This is the only check that
//!   can contradict a latched state, and the only way to see an agent thurbox
//!   never launched.
//!
//! Pure data and decisions: no process is run here, no file is read, and
//! nothing overwrites the stored state — every rule is read-time. The
//! callers gather the facts (`agent::tmux::pane_state`, the agent registry, the
//! hook columns) and ask this module what they mean.

use super::{AgentRegistry, HOOK_STATES};

/// How the built-in `hooks` extension delivers an agent's status hooks.
///
/// The three mechanisms `extensions/hooks/extension.toml` uses, in the same
/// order `docs/AGENTS.md` documents them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDelivery {
    /// Args appended to the agent's launch command (`[[agent_patches]]`).
    /// Wired at spawn, so it covers only agents thurbox itself launches.
    Args,
    /// A reversible JSON deep-merge into a config file the agent and its user
    /// share (`[[config_merges]]`).
    MergeJson,
    /// A standalone thurbox-managed file dropped into the agent's own config
    /// directory (`[[external_files]]`).
    File,
}

impl HookDelivery {
    /// The stable word this mechanism is reported as.
    pub fn as_str(self) -> &'static str {
        match self {
            HookDelivery::Args => "args",
            HookDelivery::MergeJson => "config-merge",
            HookDelivery::File => "config-file",
        }
    }
}

/// What one agent's shipped hook payload can report, and how it is delivered.
///
/// The table below is the machine-readable half of `docs/AGENTS.md` → *Status
/// hook mechanisms*; each entry's `states` are exactly the states its payload
/// in `extensions/hooks/` signals, which a test asserts against the embedded
/// payloads rather than trusting this list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentHookCoverage {
    /// The built-in agent (also the `hook_schema` family name).
    pub agent: &'static str,
    /// Every state this agent's payload can signal, in `HOOK_STATES` order.
    pub states: &'static [&'static str],
    pub delivery: HookDelivery,
    /// The file the agent reads its hooks from, `~`-anchored — `None` when the
    /// wiring is launch args alone (aider's `--notifications-command`).
    ///
    /// For [`HookDelivery::Args`] agents that *do* have one (claude), the path
    /// is relative to the hooks extension's home rather than `~`; see
    /// [`Self::hook_file_is_in_hooks_home`].
    pub hook_file: Option<&'static str>,
    /// Whether `blocked` is inferred by matching **text** in a notification
    /// body rather than a structured event. Such a match stops working
    /// silently when the agent rewords its notifications, so a consumer should
    /// treat a missing `blocked` from these agents as inconclusive.
    pub blocked_is_heuristic: bool,
}

impl AgentHookCoverage {
    /// Whether [`Self::hook_file`] is relative to the hooks extension's install
    /// home (claude's `claude.json`, passed by `--settings`) rather than to the
    /// user's home directory.
    pub fn hook_file_is_in_hooks_home(&self) -> bool {
        matches!(self.delivery, HookDelivery::Args)
    }

    /// Whether this agent can report every state in [`HOOK_STATES`].
    pub fn is_full(&self) -> bool {
        HOOK_STATES.iter().all(|s| self.states.contains(s))
    }
}

/// Every agent the built-in `hooks` extension wires, and what its payload can
/// say. Kept in step with `extensions/hooks/extension.toml` by tests: the
/// states are checked against the embedded payloads, and the paths against the
/// manifest.
pub const AGENT_HOOK_COVERAGE: &[AgentHookCoverage] = &[
    AgentHookCoverage {
        agent: "claude",
        states: &["working", "blocked", "done", "idle"],
        delivery: HookDelivery::Args,
        hook_file: Some("claude.json"),
        // `Notification` bodies are matched against *permission*/*approval*.
        blocked_is_heuristic: true,
    },
    AgentHookCoverage {
        agent: "aider",
        // Only a "waiting for input" callback exists, so the block edge is the
        // whole of what aider can report.
        states: &["blocked"],
        delivery: HookDelivery::Args,
        hook_file: None,
        blocked_is_heuristic: false,
    },
    AgentHookCoverage {
        agent: "codex",
        states: &["working", "done", "idle"],
        delivery: HookDelivery::MergeJson,
        hook_file: Some("~/.codex/hooks.json"),
        blocked_is_heuristic: false,
    },
    AgentHookCoverage {
        agent: "antigravity",
        states: &["working", "blocked", "done", "idle"],
        delivery: HookDelivery::MergeJson,
        hook_file: Some("~/.gemini/settings.json"),
        // agy adopted claude's schema, including the text match.
        blocked_is_heuristic: true,
    },
    AgentHookCoverage {
        agent: "opencode",
        states: &["working", "blocked", "done", "idle"],
        delivery: HookDelivery::File,
        hook_file: Some("~/.config/opencode/plugin/thurbox-status.js"),
        blocked_is_heuristic: false,
    },
    AgentHookCoverage {
        agent: "copilot",
        states: &["working", "blocked", "done", "idle"],
        delivery: HookDelivery::File,
        hook_file: Some("~/.copilot/hooks/thurbox-status.json"),
        blocked_is_heuristic: false,
    },
    AgentHookCoverage {
        agent: "vibe",
        states: &["working", "done"],
        delivery: HookDelivery::File,
        hook_file: Some("~/.vibe/hooks.toml"),
        blocked_is_heuristic: false,
    },
    AgentHookCoverage {
        agent: "pi",
        states: &["working", "blocked", "done", "idle"],
        delivery: HookDelivery::File,
        hook_file: Some("~/.pi/agent/extensions/thurbox-status.ts"),
        blocked_is_heuristic: false,
    },
    AgentHookCoverage {
        agent: "omp",
        states: &["working", "blocked", "done", "idle"],
        delivery: HookDelivery::File,
        hook_file: Some("~/.omp/agent/extensions/thurbox-status.ts"),
        blocked_is_heuristic: false,
    },
];

/// How an agent came to have (or not have) hook coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageSource {
    /// The agent's own name is one the hooks extension knows.
    ByName,
    /// A custom agent asserted the family with `hook_schema` in agents.toml.
    BySchema,
}

/// The coverage of the agent named `agent`, resolved the way the hooks
/// extension resolves it: by the agent's own name, else by the `hook_schema`
/// family the user asserted for it.
///
/// `None` means **uninstrumented** — thurbox ships no hook payload for this
/// agent and the user asserted no family, so it can report nothing at all. That
/// is a different fact from "idle", and the whole reason this returns an
/// `Option` rather than an empty state list.
pub fn coverage_for(
    registry: &AgentRegistry,
    agent: &str,
) -> Option<(&'static AgentHookCoverage, CoverageSource)> {
    if let Some(found) = AGENT_HOOK_COVERAGE.iter().find(|c| c.agent == agent) {
        return Some((found, CoverageSource::ByName));
    }
    let schema = registry.get(agent)?.hook_schema.as_deref()?;
    AGENT_HOOK_COVERAGE
        .iter()
        .find(|c| c.agent == schema)
        .map(|c| (c, CoverageSource::BySchema))
}

/// The one-word coverage verdict a consumer branches on.
///
/// `Partial` is the honest middle: `aider` reports `blocked` and nothing else,
/// so its silence about `working` carries no information.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Coverage {
    Full,
    Partial,
    #[default]
    None,
}

impl Coverage {
    pub fn as_str(self) -> &'static str {
        match self {
            Coverage::Full => "full",
            Coverage::Partial => "partial",
            Coverage::None => "none",
        }
    }

    /// The verdict for what [`coverage_for`] resolved.
    pub fn of(found: Option<&AgentHookCoverage>) -> Self {
        match found {
            None => Coverage::None,
            Some(c) if c.is_full() => Coverage::Full,
            Some(_) => Coverage::Partial,
        }
    }
}

/// Seconds between `state_at` (epoch ms) and `now` (epoch ms).
///
/// `None` when nothing was ever reported. A stamp in the future clamps to 0
/// rather than wrapping: a mirrored session carries a state a *peer's* clock
/// stamped, and two machines' clocks are never exactly equal.
pub fn age_secs(state_at: Option<i64>, now: i64) -> Option<u64> {
    state_at.map(|at| u64::try_from((now - at).max(0)).unwrap_or(0) / 1000)
}

/// What actually holds a session pane's tty, judged against the agent registry.
///
/// The decisive check the hook state cannot make for itself: `hook_state` is
/// self-reported and latched, while this is observed. It is deliberately
/// coarse — the question is "is an agent there", not "what is it doing".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Corroboration {
    /// The foreground process is the session's own agent.
    Agent,
    /// The foreground process is *a* known agent, but not the one this session
    /// was created with — an agent some driver started inside a pane thurbox
    /// opened for something else (typically a bare shell). The session is
    /// running an agent whether or not it ever signalled.
    ///
    /// Carries the registry **name** of the agent that was found, which is the
    /// whole reason a driver-launched session can be labelled at all: the
    /// answer used to be computed and thrown away, leaving every surface to
    /// say "some agent" about a pane thurbox could name. Not `Copy` for it,
    /// which is the price of the payload and is paid once here.
    ///
    /// `None` when the executable is one **several** registered profiles
    /// share, which is a shape the shipped `agents.toml` walks the user
    /// through: pinning a model means a second entry on the same `command`
    /// (`claude-opus` beside `claude`). A process listing sees the executable,
    /// not the profile, so there is no answer to publish — and the variant
    /// still says an agent is *present*, which is observed, while leaving the
    /// identity blank, which is not. A plausible-but-arbitrary name is worse
    /// than none: it invites exactly the trust it has not earned.
    ForeignAgent(Option<String>),
    /// A bare interactive shell holds the pane. For a session whose agent was
    /// launched into that pane, this means the agent is gone.
    Shell,
    /// Something else runs in the foreground — the agent shelled out, or a
    /// person is running a command. Says nothing either way.
    Other,
    /// The pane's command has exited; `remain-on-exit` kept the frame. Whatever
    /// the row still says, nothing is running.
    Dead,
    /// Nothing could be resolved (no `ps`, a multiplexer that answers no
    /// format, a pane that went away between reads).
    Unknown,
    /// Not checkable from here — a remote session's pane lives on its own
    /// host's multiplexer.
    Unavailable,
}

impl Corroboration {
    pub fn as_str(&self) -> &'static str {
        match self {
            Corroboration::Agent => "agent",
            Corroboration::ForeignAgent(_) => "foreign-agent",
            Corroboration::Shell => "shell",
            Corroboration::Other => "other",
            Corroboration::Dead => "dead",
            Corroboration::Unknown => "unknown",
            Corroboration::Unavailable => "unavailable",
        }
    }

    /// The registered agent found holding the pane, when it is not the one the
    /// session was created with.
    ///
    /// A live observation and nothing more — deliberately **not** written back
    /// onto the row as `reports_as`, which is a durable declaration a driver
    /// makes. Deriving one from the other would make a passing `claude` in the
    /// pane permanent.
    pub fn detected_agent(&self) -> Option<&str> {
        match self {
            Corroboration::ForeignAgent(name) => name.as_deref(),
            _ => None,
        }
    }

    /// Whether an agent process is demonstrably running in the pane.
    pub fn agent_present(&self) -> Option<bool> {
        match self {
            Corroboration::Agent | Corroboration::ForeignAgent(_) => Some(true),
            Corroboration::Shell | Corroboration::Dead => Some(false),
            // `Other` is a command the agent (or the user) is running; it says
            // nothing about whether the agent is still behind it.
            Corroboration::Other | Corroboration::Unknown | Corroboration::Unavailable => None,
        }
    }
}

/// Interactive shells, by executable name. A pane holding one of these — and
/// nothing of the agent in its argv — is a pane whose agent is gone.
const SHELLS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ash",
    "ksh",
    "mksh",
    "csh",
    "tcsh",
    "nu",
    "elvish",
    "xonsh",
    "pwsh",
    "powershell",
    "cmd",
];

/// The executable name of an argv0: no directories, no `.exe`, and no leading
/// `-` (a login shell is spelled `-bash` in its own argv).
fn executable_name(argv0: &str) -> &str {
    let base = argv0
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(argv0)
        .trim_start_matches('-');
    base.strip_suffix(".exe").unwrap_or(base)
}

/// Whether `command_line` runs `program` **in command position** rather than
/// merely mentioning it somewhere in its argv.
///
/// This is what keeps a wrapper honest: a remote session's window command is
/// `/bin/sh -lc 'exec claude --resume …'`, whose argv0 is `sh`. Reading argv0
/// alone would call that pane a shell and declare the agent lost.
///
/// The position matters as much as the token, because a driver-launched agent
/// is handed a multi-kilobyte prose brief as one argv element and `ps` prints
/// argv joined by spaces: matching a bare token *anywhere* reported
/// `perl -e 'sleep 300' claude` as a claude agent holding the pane. Only four
/// places start a command, and all four are checked here:
///
/// - argv0;
/// - after `exec`, `env`, `command` or `nohup`, and past leading `VAR=value`
///   assignments, all of which are prefixes to the command they run;
/// - after a shell's `-c`/`-lc`/`-ic` option, whose operand is a whole new
///   command line — the wrapper case above;
/// - the shell's first operand, which is the script it runs: an agent shipped
///   as a `#!/bin/sh` script is executed as `/bin/sh …/bin/codex`, and codex is
///   what is running there;
///
/// and nowhere else. Everything a shell takes is honoured only while its *own*
/// options are still being scanned, so a `-c` that turns up inside prose argv
/// opens nothing. Anything else is a false negative rather than a false
/// positive: no identity is more honest than a confidently wrong one.
fn runs_program(command_line: &str, program: &str) -> bool {
    // `true` at the start of a line and after every prefix above: the next
    // token that is not itself a prefix is the command being run.
    let mut expect_command = true;
    // Whether the command already resolved is a shell whose own options we are
    // still walking — the only state in which what follows is a command again.
    let mut scanning_shell_options = false;
    for raw in command_line.split_whitespace() {
        let token = raw.trim_matches(['\'', '"']);
        if token.is_empty() {
            continue;
        }
        if expect_command {
            match command_position_token(token, program) {
                CommandPositionToken::Prefix => {}
                CommandPositionToken::Match => return true,
                CommandPositionToken::Other { is_shell } => {
                    expect_command = false;
                    scanning_shell_options = is_shell;
                }
            }
            continue;
        }
        if !scanning_shell_options {
            continue;
        }
        match shell_option_token(token, program) {
            ShellOptionToken::Match => return true,
            ShellOptionToken::EndsOptions { next_is_command } => {
                scanning_shell_options = false;
                expect_command = next_is_command;
            }
            ShellOptionToken::StillScanning => {}
        }
    }
    false
}

/// What a token means while `runs_program` still expects the command itself.
enum CommandPositionToken {
    /// A prefix (`exec`, `env`, `FOO=1`, …): the command is still ahead.
    Prefix,
    /// Resolves to `program` in command position.
    Match,
    /// Some other command; `is_shell` says whether its own options follow.
    Other { is_shell: bool },
}

fn command_position_token(token: &str, program: &str) -> CommandPositionToken {
    if COMMAND_PREFIXES.contains(&token) || is_assignment(token) {
        return CommandPositionToken::Prefix;
    }
    let name = executable_name(token);
    if name == program {
        return CommandPositionToken::Match;
    }
    CommandPositionToken::Other {
        is_shell: SHELLS.contains(&name),
    }
}

/// What a token means while `runs_program` is walking a shell's own options.
enum ShellOptionToken {
    /// The shell's first operand resolves to `program`.
    Match,
    /// The option list ended here; `next_is_command` says whether the token
    /// right after this one is a command (a script operand already consumed
    /// its own answer, so it never is).
    EndsOptions { next_is_command: bool },
    /// Still an option; the shell's own options continue past it.
    StillScanning,
}

fn shell_option_token(token: &str, program: &str) -> ShellOptionToken {
    match token.strip_prefix('-') {
        // `--` ends the option list; the next token is the operand.
        Some("-") => ShellOptionToken::EndsOptions {
            next_is_command: true,
        },
        // A cluster like `-lc`, whose operand is a whole command line.
        Some(flags) if !flags.starts_with('-') => {
            if flags.contains('c') {
                ShellOptionToken::EndsOptions {
                    next_is_command: true,
                }
            } else {
                ShellOptionToken::StillScanning
            }
        }
        // Any other long option: no shell takes one that runs a command.
        Some(_) => ShellOptionToken::StillScanning,
        // The first operand — the script the shell runs, and so a command.
        None if executable_name(token) == program => ShellOptionToken::Match,
        None => ShellOptionToken::EndsOptions {
            next_is_command: false,
        },
    }
}

/// Commands that run another command: whatever follows one is still a command.
const COMMAND_PREFIXES: &[&str] = &["exec", "env", "command", "nohup"];

/// Whether a token is a `VAR=value` assignment, which precedes the command it
/// applies to (`FOO=1 claude …`, `env FOO=1 claude …`) rather than being one.
fn is_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((name, _)) => {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// Judge what holds a pane against the session's agent and every agent the user
/// has defined.
///
/// `agent_command` is the session's own agent binary (`AgentDef::command`, not
/// the agent *name* — `antigravity` runs `agy`). `registry` is every agent the
/// user has defined, which is what makes an externally-launched agent visible:
/// thurbox wires no hooks for a session whose agent is `bash`, but if `claude`
/// is in the registry and `claude` holds the pane, an agent is running. The
/// whole registry rather than a list of commands, so the answer can be
/// reported under the *name* a person configured (`antigravity`, not `agy`).
///
/// `dead` is tmux's `#{pane_dead}`; it is checked first because a dead pane
/// still answers `#{pane_current_command}` with whatever last ran there, which
/// is a plausible wrong answer rather than an honest absence.
pub fn classify_foreground(
    agent_command: &str,
    registry: &AgentRegistry,
    process: Option<&str>,
    command_line: Option<&str>,
    dead: Option<bool>,
) -> Corroboration {
    if dead == Some(true) {
        return Corroboration::Dead;
    }
    let Some(process) = process.filter(|p| !p.is_empty()) else {
        return Corroboration::Unknown;
    };
    let name = executable_name(process);
    let own = executable_name(agent_command);
    let line = command_line.unwrap_or(process);

    // A session whose "agent" is a bare shell is the externally-driven shape:
    // the shell it asked for holding the pane is not an agent running, and
    // calling it one would report an empty terminal as live work.
    let own_is_agent = !own.is_empty() && !SHELLS.contains(&own);
    if own_is_agent && (name == own || runs_program(line, own)) {
        return Corroboration::Agent;
    }
    // Every registry profile whose executable this pane could be running —
    // walked rather than `find`-ed, because the *first* match is not an answer.
    // Several profiles may share one executable, and the shipped `agents.toml`
    // walks the user through creating exactly that: pinning a model means a
    // second entry on the same `command` (`claude-opus` beside `claude`).
    // Taking the first would publish whichever the file happens to list first.
    let mut found = registry
        .agents
        .iter()
        .map(|def| (def, executable_name(&def.command)))
        .filter(|(_, c)| !c.is_empty() && *c != own)
        // A registry entry that *is* a shell (the bare-shell agent an external
        // driver asks for) must not make every shell look like an agent.
        .filter(|(_, c)| !SHELLS.contains(c))
        .filter(|(_, c)| name == *c || runs_program(line, c))
        .map(|(def, _)| def.name.as_str());
    if let Some(first) = found.next() {
        // Presence is observed; identity may not be. `ps` reports the
        // executable, and two profiles that share one are indistinguishable
        // through it — so the presence is reported and the name is left blank
        // rather than guessed. Deliberately not broken by a tie-break on argv:
        // a heuristic that is usually right is the failure this whole change
        // set out to remove, and a confident wrong name is worse on screen
        // than the bare `shell` label it replaced.
        let names_one_agent = found.all(|other| other == first);
        return Corroboration::ForeignAgent(names_one_agent.then(|| first.to_string()));
    }
    if SHELLS.contains(&name) {
        return Corroboration::Shell;
    }
    Corroboration::Other
}

/// Whether the reported state and the pane contradict each other.
///
/// Only the two *active* states can be contradicted: `working` and `blocked`
/// both assert that an agent is there to be working or waiting, and an empty or
/// dead pane says it is not. `done` and `idle` assert nothing about a live
/// process — an agent that finished its turn and exited is not a contradiction.
///
/// A contradiction is **reported, never applied**: the stored `hook_state` is
/// left exactly as the agent wrote it, because overwriting an agent's own
/// report with an inference is how a state becomes unfalsifiable.
pub fn contradicts(state: Option<&str>, corroboration: &Corroboration) -> bool {
    matches!(state, Some("working" | "blocked"))
        && matches!(corroboration, Corroboration::Shell | Corroboration::Dead)
}

/// Where a session's reported state came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateSource {
    /// An agent lifecycle hook called `thurbox-cli session signal`, or the
    /// remote pane-option channel that stands in for it.
    Hook,
    /// Nothing signalled, but the pane's foreground process is an agent. Coarse
    /// — it says an agent is running, never what it is doing.
    Process,
}

impl StateSource {
    pub fn as_str(self) -> &'static str {
        match self {
            StateSource::Hook => "hook",
            StateSource::Process => "process",
        }
    }
}

/// The one word every surface answers with, and the whole vocabulary of them.
///
/// Four of these are [`HOOK_STATES`] — the agent's own report, verbatim. The
/// rest are thurbox's own, and each is spelled apart from `idle` deliberately:
/// collapsing "nothing here is wired to report" or "the host is gone" into
/// "the agent says it is at rest" is the conflation this whole module exists
/// to prevent.
///
/// Every surface derives through this enum — `session get`, `session list`,
/// `thurbox-cli watch`, `thurbox-cli` bare and the interface's own session
/// list — so a driver reconciling two of them never has to reconcile two
/// vocabularies. The read-time rules that produce it are [`derive_state`],
/// [`with_output_quiescence`] and [`with_reachability`]; a caller applies the
/// ones whose inputs it can actually observe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionState {
    /// A turn is running (hook).
    Working,
    /// The agent is waiting on input or approval (hook).
    Blocked,
    /// A turn finished and nobody has looked at it yet (hook).
    Done,
    /// At rest: acknowledged, never active, or a stored word nothing knows.
    Idle,
    /// Parked by `session stop`: the row and its checkout stand, the pane does
    /// not.
    ///
    /// It outranks everything else here for the sharpest version of the reason
    /// the two silences below exist: a parked session has no process at all, so
    /// *every* other word would describe an agent that is not running. It is
    /// also the one state thurbox knows first-hand rather than infers.
    Stopped,
    /// A remote session whose host cannot be reached. The row stands; the
    /// machine behind it does not, and the hook columns just hold its last
    /// word.
    Unreachable,
    /// An agent holds the pane and nothing has signalled.
    ///
    /// Outside [`HOOK_STATES`] on purpose: no amount of process inspection
    /// distinguishes a turn in flight from a prompt waiting for input, and
    /// spelling it `working` would launder an observation into a claim the
    /// observation cannot support.
    Running,
    /// This session's agent is wired to report nothing, so its silence means
    /// nothing. Spelling it `idle` would launder "we cannot know" into "the
    /// agent says it is at rest".
    Uncovered,
    /// The agent *can* report and has not yet.
    Unreported,
}

impl SessionState {
    /// Every word, in the order a reader scans them — so a surface that counts
    /// or documents the vocabulary enumerates it rather than restating it.
    pub const ALL: &[SessionState] = &[
        SessionState::Working,
        SessionState::Blocked,
        SessionState::Done,
        SessionState::Idle,
        SessionState::Stopped,
        SessionState::Unreachable,
        SessionState::Running,
        SessionState::Uncovered,
        SessionState::Unreported,
    ];

    /// The stable lowercase word every surface prints.
    pub fn as_str(self) -> &'static str {
        match self {
            SessionState::Working => "working",
            SessionState::Blocked => "blocked",
            SessionState::Done => "done",
            SessionState::Idle => "idle",
            SessionState::Stopped => "stopped",
            SessionState::Unreachable => "unreachable",
            SessionState::Running => "running",
            SessionState::Uncovered => "uncovered",
            SessionState::Unreported => "unreported",
        }
    }

    /// The state a hook's own word names — `None` for anything outside
    /// [`HOOK_STATES`], which is how a stored word nothing recognises stops
    /// short of being reported as an agent's report.
    pub fn from_hook_state(word: &str) -> Option<Self> {
        match word {
            "working" => Some(SessionState::Working),
            "blocked" => Some(SessionState::Blocked),
            "done" => Some(SessionState::Done),
            "idle" => Some(SessionState::Idle),
            _ => None,
        }
    }
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a finished turn has been acknowledged: the interface stamps
/// `seen_at` when the user moves focus off a `done` session.
///
/// A **stored fact**, not a timeout — which is why every surface can apply it.
/// The CLI has no terminal to ask about quiescence and refuses to guess a
/// staleness bound, but this column is simply there to be read, and until it
/// was a turn the interface had already acknowledged stayed `done` on every
/// headless surface for the rest of the session's life.
fn acknowledged(state_at: Option<i64>, seen_at: Option<i64>) -> bool {
    matches!((seen_at, state_at), (Some(seen), Some(at)) if seen >= at)
}

/// Map the persisted hook columns onto the state a reader should see.
///
/// The stuck-`working` fallback is deliberately **not** here: it is a question
/// about the terminal, not the row, so it belongs to [`with_output_quiescence`].
/// The stored row is never touched — every rule in this module is a read-time
/// decision.
pub fn derive_state(
    hook_state: Option<&str>,
    state_at: Option<i64>,
    seen_at: Option<i64>,
) -> SessionState {
    match hook_state.and_then(SessionState::from_hook_state) {
        Some(SessionState::Done) if acknowledged(state_at, seen_at) => SessionState::Idle,
        Some(state) => state,
        None => SessionState::Idle,
    }
}

/// How long a `working` session may produce **no terminal output** before it is
/// reported as idle. v1 uses the same 10s bound.
///
/// The signal is output, not the age of the hook state, and the difference is
/// the whole point: a turn that runs for a minute is still `working` the whole
/// minute, because the agent is printing throughout. Keying on the hook's
/// timestamp instead ends every turn after ten seconds and starts it again at
/// the next hook — a spinner that stops early and restarts, which is precisely
/// what the fallback exists to avoid.
pub const WORKING_QUIET_MS: u64 = 10_000;

/// Fold terminal quiescence into a `working` session's state.
///
/// Agents fire no hook when a turn is **interrupted** (Esc / Ctrl+C) and none
/// when they return to the idle prompt, so a `working` state can stand forever
/// with nothing running behind it. TUI agents animate their in-progress line
/// while a turn runs (Claude's `(Xs · esc to interrupt)` ticks every second), so
/// a genuinely live turn is never quiet for [`WORKING_QUIET_MS`] and only the
/// stuck state falls through.
///
/// `quiet_for_ms` is `None` when the session has no live pane — nothing can be
/// producing output, so that reads as quiet too. This is where v1's `exited →
/// Idle` branch lands: a pane whose stream ended is dropped from the live set.
/// Only `working` is time-gated; `blocked` is a standing request for input and
/// says nothing about output.
///
/// Only a caller with a live terminal can answer this, which is why it is a
/// fold a surface opts into rather than part of [`derive_state`]: headless,
/// there is nothing to ask, and guessing a bound would report a running turn
/// as finished.
pub fn with_output_quiescence(state: SessionState, quiet_for_ms: Option<u64>) -> SessionState {
    if state == SessionState::Working && quiet_for_ms.unwrap_or(u64::MAX) > WORKING_QUIET_MS {
        return SessionState::Idle;
    }
    state
}

/// Fold what the terminal store knows about the host into a session's state.
///
/// The hook state says what the *agent* is doing; it cannot say that the machine
/// the agent runs on has gone away, because a host that is down reports nothing
/// at all — it just leaves the last state standing. So a remote session with no
/// live pane reads `unreachable` rather than the stale `idle` its row still
/// carries, which is v1's placeholder row by another name.
///
/// Local sessions are never unreachable: this *is* their machine, and a missing
/// pane there means the agent has not been launched, not that it cannot be
/// reached.
pub fn with_reachability(
    state: SessionState,
    backend: &str,
    attach_error: Option<&str>,
) -> SessionState {
    if attach_error.is_some() && super::is_remote_backend(backend) {
        return SessionState::Unreachable;
    }
    state
}

/// The best answer available for a session, and where it came from.
///
/// The hook columns win whenever they hold anything — they are the agent's own
/// report, read through [`derive_state`], and richer than anything observable.
/// Only when nothing ever signalled does the pane get a say, and then only to
/// the extent of [`SessionState::Running`]. `None` is the honest third outcome:
/// no hook, no agent in the pane.
pub fn best_state(
    hook_state: Option<&str>,
    state_at: Option<i64>,
    seen_at: Option<i64>,
    corroboration: Option<&Corroboration>,
) -> Option<(SessionState, StateSource)> {
    if hook_state.is_some() {
        return Some((
            derive_state(hook_state, state_at, seen_at),
            StateSource::Hook,
        ));
    }
    match corroboration {
        Some(Corroboration::Agent | Corroboration::ForeignAgent(_)) => {
            Some((SessionState::Running, StateSource::Process))
        }
        _ => None,
    }
}

/// Everything known about one session's agent state, gathered in one place so
/// every caller reports the same fields and no renderer has to re-derive them.
///
/// Built in two steps because they cost different things: [`Self::from_hooks`]
/// is three database columns and a table lookup, while [`Self::with_pane`]
/// needs a live probe of the pane. A caller that reads many sessions at once
/// takes the first and skips the second, and the fields the probe would have
/// filled stay `None` — *unchecked*, which is a third answer distinct from
/// "checked and found nothing".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assessment {
    /// The stored `hook_state`, verbatim — never adjusted by anything here.
    pub hook_state: Option<String>,
    /// Epoch ms it was stored.
    pub state_at: Option<i64>,
    /// Epoch ms the interface stamped when the user moved focus off a finished
    /// turn — the `done → idle` acknowledgment, folded in by [`derive_state`].
    pub seen_at: Option<i64>,
    pub age_secs: Option<u64>,
    /// Whether any hook has *ever* reported for this session. The distinction
    /// the brief's "explicit uninstrumented state" turns on: a session that has
    /// said nothing is not a session that said `idle`.
    pub reported: bool,
    pub coverage: Coverage,
    pub coverage_source: Option<CoverageSource>,
    /// The row of [`AGENT_HOOK_COVERAGE`] this session's agent resolved to —
    /// held whole rather than copied field by field, so a caller that needs the
    /// hook file (a diagnostic looking for it on disk) reaches the *same* entry
    /// [`Self::coverage`] was decided from and cannot pick a different agent
    /// that happens to report the same states.
    pub covered: Option<&'static AgentHookCoverage>,
    /// `None` = the pane was not looked at.
    pub corroboration: Option<Corroboration>,
    pub foreground_process: Option<String>,
    pub foreground_command: Option<String>,
    /// `None` = not checked; `Some(false)` = checked and consistent.
    pub contradicted: Option<bool>,
    /// The best answer available, and where it came from — see [`best_state`].
    pub state: Option<SessionState>,
    pub state_source: Option<StateSource>,
    /// Parked by `session stop`: the row and its checkout stand, the pane does
    /// not. Set by [`Self::parked`], and the one fact here that is thurbox's
    /// own rather than the agent's or the pane's.
    pub stopped: bool,
    /// The agent every answer above was resolved against.
    ///
    /// Usually the session's own, but a `--command` session whose driver
    /// declared what it launched (`session reports-as`) is judged against
    /// *that* agent — so a reader can see which name the coverage belongs to
    /// rather than assuming it is the row's.
    pub agent: String,
}

impl Assessment {
    /// The one state every surface shows for this session, never absent.
    ///
    /// [`Self::state`] is `None` when nothing signalled and nothing observable
    /// holds the pane, and a bare null leaves a reader unable to tell the two
    /// silences apart. They are different facts and get different words:
    /// [`SessionState::Uncovered`] (this agent is wired to report nothing, so
    /// silence means nothing) and [`SessionState::Unreported`] (it can report
    /// and has not). A session [`parked`](Self::parked) by `session stop`
    /// answers [`SessionState::Stopped`] ahead of both.
    ///
    /// Every rendering goes through here — the human table, the agent-facing
    /// TOON view, the home view and the event stream — so no surface can invent
    /// a third answer for the same row.
    pub fn state(&self) -> SessionState {
        if self.stopped {
            return SessionState::Stopped;
        }
        match self.state {
            Some(state) => state,
            None if self.coverage == Coverage::None => SessionState::Uncovered,
            None => SessionState::Unreported,
        }
    }

    /// [`Self::state`](Self::state) as the word a document carries.
    pub fn state_word(&self) -> &'static str {
        self.state().as_str()
    }

    /// Every state this session's agent can report — empty when it is wired to
    /// report nothing.
    pub fn states_reportable(&self) -> &'static [&'static str] {
        self.covered.map(|c| c.states).unwrap_or(&[])
    }

    /// How this agent's hooks are delivered, when it has any.
    pub fn delivery(&self) -> Option<HookDelivery> {
        self.covered.map(|c| c.delivery)
    }

    /// Whether this agent's `blocked` is a text match on a notification body,
    /// and so stops working silently when the agent rewords one.
    pub fn blocked_is_heuristic(&self) -> bool {
        self.covered.is_some_and(|c| c.blocked_is_heuristic)
    }

    /// Where this agent's hook payload lives, when it has a file at all.
    ///
    /// The path is `~`-anchored for a config-dir agent and relative to the
    /// hooks extension's own install home for an arg-patched one — see
    /// [`AgentHookCoverage::hook_file_is_in_hooks_home`].
    pub fn hook_file(&self) -> Option<&'static str> {
        self.covered.and_then(|c| c.hook_file)
    }

    /// What the stored hook columns and the agent registry alone can say.
    pub fn from_hooks(
        registry: &AgentRegistry,
        agent: &str,
        hook_state: Option<&str>,
        state_at: Option<i64>,
        seen_at: Option<i64>,
        now: i64,
    ) -> Self {
        let found = coverage_for(registry, agent);
        let state = best_state(hook_state, state_at, seen_at, None);
        Self {
            hook_state: hook_state.map(str::to_string),
            state_at,
            seen_at,
            age_secs: age_secs(state_at, now),
            reported: hook_state.is_some(),
            coverage: Coverage::of(found.map(|(c, _)| c)),
            coverage_source: found.map(|(_, source)| source),
            covered: found.map(|(c, _)| c),
            corroboration: None,
            foreground_process: None,
            foreground_command: None,
            contradicted: None,
            state: state.map(|(state, _)| state),
            state_source: state.map(|(_, source)| source),
            stopped: false,
            agent: agent.to_string(),
        }
    }

    /// The agent found holding the pane, when it is not the one the session was
    /// created with — `None` when the pane was not looked at, holds a shell, or
    /// holds this session's own agent.
    ///
    /// The third of three names for one row, and none of them substitutes for
    /// another: [`Self::agent`] is what the row reports *as*, `reports_as` is
    /// what a driver declared, and this is what is observably there right now.
    pub fn detected_agent(&self) -> Option<&str> {
        self.corroboration
            .as_ref()
            .and_then(Corroboration::detected_agent)
    }

    /// Fold in what the pane's foreground process says.
    ///
    /// The stored state is left exactly as it was; the observation only adds
    /// [`Self::contradicted`], and only fills [`Self::state`] when nothing ever
    /// signalled (an agent thurbox did not launch, and so never wired).
    pub fn with_pane(
        mut self,
        agent_command: &str,
        registry: &AgentRegistry,
        process: Option<&str>,
        command_line: Option<&str>,
        dead: Option<bool>,
    ) -> Self {
        self.foreground_process = process.filter(|p| !p.is_empty()).map(str::to_string);
        self.foreground_command = command_line.filter(|c| !c.is_empty()).map(str::to_string);
        self.with_corroboration(classify_foreground(
            agent_command,
            registry,
            process,
            command_line,
            dead,
        ))
    }

    /// Fold in an already-computed pane verdict.
    ///
    /// Split out of [`Self::with_pane`] for the caller that cannot classify
    /// where it folds: the interface probes panes on a worker thread (the
    /// render loop may not shell out), so the verdict arrives on its own and
    /// the argv it was read from is not carried back.
    pub fn with_corroboration(mut self, corroboration: Corroboration) -> Self {
        self.contradicted = Some(contradicts(self.hook_state.as_deref(), &corroboration));
        if let Some((state, source)) = best_state(
            self.hook_state.as_deref(),
            self.state_at,
            self.seen_at,
            Some(&corroboration),
        ) {
            self.state = Some(state);
            self.state_source = Some(source);
        }
        self.corroboration = Some(corroboration);
        self
    }

    /// Record that the pane could not be looked at from here — a remote
    /// session's pane lives on its own host's multiplexer. Distinct from
    /// leaving the corroboration unset, which means nobody tried.
    pub fn pane_unavailable(mut self) -> Self {
        self.corroboration = Some(Corroboration::Unavailable);
        self
    }

    /// Record that the session is parked — `session stop` killed its pane and
    /// kept everything else.
    ///
    /// [`Self::state`] and its source are dropped rather than kept: they are
    /// "the best answer available" for a session that is *running*, and a
    /// parked one is not. `hook_state` stays exactly as stored, because a
    /// consumer reading that column reads the agent's last word verbatim and
    /// nothing here may launder thurbox's own fact into it.
    pub fn parked(mut self) -> Self {
        self.stopped = true;
        self.state = None;
        self.state_source = None;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::AgentDef;

    fn registry(defs: Vec<AgentDef>) -> AgentRegistry {
        AgentRegistry {
            config_version: Some(1),
            default: defs.first().map(|d| d.name.clone()).unwrap_or_default(),
            agents: defs,
        }
    }

    fn agent(name: &str, command: &str, hook_schema: Option<&str>) -> AgentDef {
        AgentDef {
            name: name.into(),
            command: command.into(),
            args: vec![],
            resume_args: vec![],
            fork_args: vec![],
            new_session_args: vec![],
            resume_latest: false,
            hook_schema: hook_schema.map(str::to_string),
        }
    }

    /// A fixed instant, so nothing here depends on a clock.
    const NOW: i64 = 1_700_000_000_000;

    #[test]
    fn an_unreported_session_is_idle() {
        assert_eq!(derive_state(None, None, None), SessionState::Idle);
        // A stored word nothing recognises is not an agent's report.
        assert_eq!(
            derive_state(Some("unknown"), None, None),
            SessionState::Idle
        );
    }

    #[test]
    fn a_long_turn_stays_working_however_old_its_hook_is() {
        // The regression this guards: keyed on the hook's own timestamp, every
        // turn reported itself finished ten seconds in and started again at the
        // next hook — a spinner that stopped early and restarted. How long ago
        // the agent said "working" says nothing about whether it still is.
        assert_eq!(
            derive_state(Some("working"), Some(NOW), None),
            SessionState::Working
        );
        assert_eq!(
            derive_state(Some("working"), Some(NOW - 600_000), None),
            SessionState::Working
        );
    }

    #[test]
    fn done_stays_done_until_acknowledged() {
        assert_eq!(
            derive_state(Some("done"), Some(NOW), None),
            SessionState::Done
        );
        assert_eq!(
            derive_state(Some("done"), Some(NOW), Some(NOW - 1)),
            SessionState::Done
        );
        assert_eq!(
            derive_state(Some("done"), Some(NOW), Some(NOW)),
            SessionState::Idle
        );
    }

    #[test]
    fn a_quiet_working_session_falls_back_to_idle() {
        // Agents fire no hook on interrupt, so without this a cancelled turn
        // would spin forever. v1 guards the same way, on the same signal.
        assert_eq!(
            with_output_quiescence(SessionState::Working, Some(WORKING_QUIET_MS + 1)),
            SessionState::Idle
        );
    }

    #[test]
    fn a_printing_working_session_stays_working() {
        assert_eq!(
            with_output_quiescence(SessionState::Working, Some(0)),
            SessionState::Working
        );
        assert_eq!(
            with_output_quiescence(SessionState::Working, Some(WORKING_QUIET_MS)),
            SessionState::Working
        );
    }

    #[test]
    fn a_working_session_with_no_live_pane_is_idle() {
        // Nothing can be producing output, which is where v1's exited → Idle
        // branch lands: a pane whose stream ended leaves the live set.
        assert_eq!(
            with_output_quiescence(SessionState::Working, None),
            SessionState::Idle
        );
    }

    #[test]
    fn blocked_is_never_time_gated() {
        assert_eq!(
            derive_state(Some("blocked"), Some(NOW - 600_000), None),
            SessionState::Blocked
        );
        // A session waiting on you produces no output while it waits.
        assert_eq!(
            with_output_quiescence(SessionState::Blocked, None),
            SessionState::Blocked
        );
        assert_eq!(
            with_output_quiescence(SessionState::Done, Some(WORKING_QUIET_MS * 10)),
            SessionState::Done
        );
    }

    #[test]
    fn a_reachable_session_keeps_the_state_its_hooks_reported() {
        assert_eq!(
            with_reachability(SessionState::Working, "ssh:devbox", None),
            SessionState::Working
        );
        assert_eq!(
            with_reachability(SessionState::Done, "local-tmux", None),
            SessionState::Done
        );
    }

    #[test]
    fn a_remote_session_with_no_pane_is_unreachable() {
        assert_eq!(
            with_reachability(SessionState::Idle, "ssh:devbox", Some("host unreachable")),
            SessionState::Unreachable
        );
        assert_eq!(
            with_reachability(
                SessionState::Working,
                "wsl:ubuntu",
                Some("connect timed out")
            ),
            SessionState::Unreachable
        );
    }

    #[test]
    fn a_local_session_without_a_pane_is_not_unreachable() {
        // This is the machine it runs on: no pane means the agent has not been
        // launched, which the pane itself says. Calling it unreachable would
        // claim a host problem that does not exist.
        assert_eq!(
            with_reachability(
                SessionState::Idle,
                "local-tmux",
                Some("session has no pane yet")
            ),
            SessionState::Idle
        );
    }

    /// The acknowledgment is a stored fact, so it applies headlessly too — the
    /// divergence that let `session get` report `done` for the rest of a
    /// session's life after the interface had already shown `idle`.
    #[test]
    fn an_assessment_folds_the_acknowledgment_the_snapshot_writes() {
        let reg = registry(vec![agent("claude", "claude", None)]);
        let unseen = Assessment::from_hooks(&reg, "claude", Some("done"), Some(NOW), None, NOW);
        assert_eq!(unseen.state(), SessionState::Done);

        let seen = Assessment::from_hooks(&reg, "claude", Some("done"), Some(NOW), Some(NOW), NOW);
        assert_eq!(seen.state(), SessionState::Idle);
        // The column a consumer applying its own policy reads stays verbatim.
        assert_eq!(seen.hook_state.as_deref(), Some("done"));
    }

    #[test]
    fn every_covered_agent_lists_states_in_the_shared_vocabulary() {
        for c in AGENT_HOOK_COVERAGE {
            assert!(!c.states.is_empty(), "{} covers nothing", c.agent);
            for state in c.states {
                assert!(
                    HOOK_STATES.contains(state),
                    "{} claims to report {state:?}, which is not a hook state",
                    c.agent
                );
            }
        }
    }

    #[test]
    fn coverage_separates_full_partial_and_uninstrumented() {
        let reg = registry(vec![
            agent("claude", "claude", None),
            agent("aider", "aider", None),
            agent("mine", "mine", None),
        ]);
        let claude = coverage_for(&reg, "claude").expect("claude is covered");
        assert_eq!(Coverage::of(Some(claude.0)), Coverage::Full);
        assert_eq!(claude.1, CoverageSource::ByName);

        // aider has only a "waiting for input" callback, so its silence about
        // `working` means nothing — which `partial` is what tells a consumer.
        let aider = coverage_for(&reg, "aider").expect("aider is covered");
        assert_eq!(Coverage::of(Some(aider.0)), Coverage::Partial);
        assert_eq!(aider.0.states, &["blocked"]);

        // The distinction the brief asks for: uninstrumented is not idle.
        assert!(coverage_for(&reg, "mine").is_none());
        assert_eq!(Coverage::of(None), Coverage::None);
    }

    #[test]
    fn a_custom_agent_inherits_the_family_it_asserts() {
        let reg = registry(vec![agent("fleet", "fleet", Some("claude"))]);
        let (coverage, source) = coverage_for(&reg, "fleet").expect("hook_schema is honoured");
        assert_eq!(coverage.agent, "claude");
        assert_eq!(source, CoverageSource::BySchema);

        // An asserted family that thurbox ships no payload for buys nothing.
        let reg = registry(vec![agent("fleet", "fleet", Some("nonesuch"))]);
        assert!(coverage_for(&reg, "fleet").is_none());
    }

    #[test]
    fn claude_and_antigravity_are_flagged_as_matching_notification_text() {
        // Both grep the notification body for *permission*/*approval*, so a
        // reworded upstream notification stops `blocked` silently. A consumer
        // has to be able to see that from the data.
        for name in ["claude", "antigravity"] {
            let c = AGENT_HOOK_COVERAGE
                .iter()
                .find(|c| c.agent == name)
                .expect("covered");
            assert!(c.blocked_is_heuristic, "{name}");
        }
        for name in ["opencode", "copilot", "codex", "vibe"] {
            let c = AGENT_HOOK_COVERAGE
                .iter()
                .find(|c| c.agent == name)
                .expect("covered");
            assert!(!c.blocked_is_heuristic, "{name}");
        }
    }

    #[test]
    fn age_is_seconds_and_never_negative() {
        assert_eq!(age_secs(None, 10_000), None);
        assert_eq!(age_secs(Some(4_000), 10_000), Some(6));
        // Sub-second ages floor to 0 rather than rounding up to a lie.
        assert_eq!(age_secs(Some(9_500), 10_000), Some(0));
        // A peer's clock ahead of ours must not wrap into ~584 million years.
        assert_eq!(age_secs(Some(20_000), 10_000), Some(0));
    }

    #[test]
    fn the_session_agent_in_the_foreground_corroborates() {
        let known = registry(vec![
            agent("claude", "claude", Some("claude")),
            agent("codex", "codex", Some("codex")),
        ]);
        assert_eq!(
            classify_foreground("claude", &known, Some("claude"), None, Some(false)),
            Corroboration::Agent
        );
        // An absolute path and an argv are the same answer.
        assert_eq!(
            classify_foreground(
                "claude",
                &known,
                Some("/home/u/.local/bin/claude"),
                Some("/home/u/.local/bin/claude --resume x"),
                Some(false),
            ),
            Corroboration::Agent
        );
    }

    #[test]
    fn a_login_wrapper_is_not_mistaken_for_a_bare_shell() {
        // The remote window command is `/bin/sh -lc 'exec claude …'`. Judging
        // argv0 alone would report a shell and declare the agent lost.
        let known = registry(vec![agent("claude", "claude", Some("claude"))]);
        assert_eq!(
            classify_foreground(
                "claude",
                &known,
                Some("/bin/sh"),
                Some("/bin/sh -lc 'exec claude --resume x'"),
                Some(false),
            ),
            Corroboration::Agent
        );
    }

    #[test]
    fn a_bare_shell_contradicts_an_active_state() {
        let known = registry(vec![agent("claude", "claude", Some("claude"))]);
        let shell =
            classify_foreground("claude", &known, Some("-bash"), Some("-bash"), Some(false));
        assert_eq!(shell, Corroboration::Shell);
        assert_eq!(shell.agent_present(), Some(false));
        assert!(contradicts(Some("working"), &shell));
        assert!(contradicts(Some("blocked"), &shell));
        // A finished turn asserts nothing about a live process.
        assert!(!contradicts(Some("done"), &shell));
        assert!(!contradicts(Some("idle"), &shell));
        assert!(!contradicts(None, &shell));
    }

    #[test]
    fn a_dead_pane_beats_the_command_name_it_still_answers_with() {
        // `remain-on-exit` keeps the frame, and tmux keeps naming the command
        // that died there — a plausible wrong answer, so deadness wins.
        let known = registry(vec![agent("claude", "claude", Some("claude"))]);
        let dead = classify_foreground("claude", &known, Some("claude"), None, Some(true));
        assert_eq!(dead, Corroboration::Dead);
        assert!(contradicts(Some("working"), &dead));
    }

    #[test]
    fn an_agent_thurbox_did_not_launch_is_still_seen() {
        // The externally-driven shape: the session's agent is a bare shell (so
        // thurbox wired no hooks and nothing ever signalled), and a driver
        // started a real agent inside the pane.
        let known = registry(vec![
            agent("shell", "bash", None),
            agent("claude", "claude", Some("claude")),
        ]);
        let seen = classify_foreground(
            "bash",
            &known,
            Some("claude"),
            Some("claude --permission-mode acceptEdits"),
            Some(false),
        );
        assert_eq!(seen, Corroboration::ForeignAgent(Some("claude".into())));
        assert_eq!(seen.detected_agent(), Some("claude"));
        assert_eq!(seen.agent_present(), Some(true));
        assert_eq!(
            best_state(None, None, None, Some(&seen)),
            Some((SessionState::Running, StateSource::Process))
        );

        // And the shell that pane was asked for is still just a shell — even
        // though it is this session's own "agent". Reporting an empty terminal
        // as an agent running is the failure this branch exists to avoid.
        let idle = classify_foreground("bash", &known, Some("bash"), Some("bash -i"), Some(false));
        assert_eq!(idle, Corroboration::Shell);
        assert_eq!(best_state(None, None, None, Some(&idle)), None);
    }

    /// F6. `runs_program` matched a bare token anywhere in the command line, so
    /// the *shape a foreign driver produces* — a multi-kilobyte prose brief in
    /// argv, printed back by `ps` as one long space-separated line — classified
    /// a pane holding no agent at all as a foreign agent.
    #[test]
    fn a_registry_name_mentioned_in_argv_is_not_an_agent_in_the_pane() {
        let known = registry(vec![
            agent("shell", "zsh", None),
            agent("claude", "claude", Some("claude")),
        ]);
        // The reproduction, verbatim: `perl -e 'sleep 300' claude`.
        let inert = classify_foreground(
            "zsh",
            &known,
            Some("perl"),
            Some("perl -e sleep 300 claude"),
            Some(false),
        );
        assert_eq!(inert, Corroboration::Other);
        assert_eq!(inert.detected_agent(), None);
        assert_eq!(best_state(None, None, None, Some(&inert)), None);

        // The same word inside prose, which is what a driver's brief is.
        let brief = classify_foreground(
            "zsh",
            &known,
            Some("zsh"),
            Some("zsh -i -- read the report and tell claude to fix it"),
            Some(false),
        );
        assert_eq!(brief, Corroboration::Shell);
        // Even a `-c` that turns up mid-prose opens nothing: a shell's own
        // options stop being scanned at its first operand.
        let flag = classify_foreground(
            "zsh",
            &known,
            Some("zsh"),
            Some("zsh -i -- run it with -c claude afterwards"),
            Some(false),
        );
        assert_eq!(flag, Corroboration::Shell);
    }

    /// The other half of F6: constraining the match must not lose the wrapper
    /// shapes `runs_program` exists for.
    #[test]
    fn a_command_position_is_still_matched_through_every_wrapper() {
        let known = registry(vec![
            agent("shell", "zsh", None),
            agent("claude", "claude", Some("claude")),
        ]);
        for line in [
            // argv0, absolute or bare.
            "claude --resume x",
            "/usr/local/bin/claude --resume x",
            // The remote window command: a shell whose `-c` operand is a whole
            // new command line, with `exec` in front of it.
            "/bin/sh -lc 'exec claude --resume x'",
            "bash -c claude",
            // An agent shipped as a `#!/bin/sh` script: the shell's first
            // operand is the program that is running.
            "/bin/sh /opt/agents/bin/claude",
            // Prefixes to the command they run.
            "env CLAUDE_CODE=1 claude --resume x",
            "exec claude",
            "nohup claude --resume x",
            // A leading assignment, with no `env` at all.
            "FOO=1 claude",
        ] {
            let seen = classify_foreground("zsh", &known, Some("sh"), Some(line), Some(false));
            assert_eq!(
                seen,
                Corroboration::ForeignAgent(Some("claude".into())),
                "should have found claude in {line}"
            );
        }
    }

    /// One executable, several registered profiles: an agent is demonstrably
    /// there, and *which* one is not something a process listing can say.
    ///
    /// This is not a theoretical registry. The shipped `agents.toml` walks the
    /// user through building it: pinning a model means a second entry on the
    /// same `command`, `claude-opus` beside `claude`. Taking the first match
    /// published whichever profile the file happened to list first — a
    /// confident name the observation never determined, which is worse on
    /// screen than the bare `shell` label this vocabulary set out to improve
    /// on, because a specific name invites trust.
    #[test]
    fn an_executable_several_profiles_share_names_no_agent() {
        // Exactly the shape the shipped config's "Pin a model" section builds.
        let shared = registry(vec![
            agent("shell", "zsh", None),
            agent("claude", "claude", Some("claude")),
            agent("claude-opus", "claude", Some("claude")),
        ]);
        let seen = classify_foreground(
            "zsh",
            &shared,
            Some("claude"),
            Some("claude --model opus"),
            Some(false),
        );

        // An agent IS running here — that much is observed, and the status
        // must not lose it.
        assert_eq!(seen, Corroboration::ForeignAgent(None));
        assert_eq!(seen.agent_present(), Some(true));
        assert_eq!(
            best_state(None, None, None, Some(&seen)),
            Some((SessionState::Running, StateSource::Process))
        );
        // But nothing may be published about WHICH, in either order — the
        // answer must not depend on how the file happens to be sorted.
        assert_eq!(seen.detected_agent(), None);
        let reversed = registry(vec![
            agent("shell", "zsh", None),
            agent("claude-opus", "claude", Some("claude")),
            agent("claude", "claude", Some("claude")),
        ]);
        assert_eq!(
            classify_foreground("zsh", &reversed, Some("claude"), None, Some(false)),
            Corroboration::ForeignAgent(None)
        );

        // And the blank reaches the assessment as a blank, beside a status
        // that still says something is running.
        let assessment = Assessment::from_hooks(&shared, "shell", None, None, None, NOW)
            .with_corroboration(seen);
        assert_eq!(assessment.detected_agent(), None);
        assert_eq!(assessment.state(), SessionState::Running);

        // The single-profile case is untouched: one profile owning the
        // executable is still named, or the guard would have cost the feature.
        let sole = registry(vec![
            agent("shell", "zsh", None),
            agent("claude", "claude", Some("claude")),
        ]);
        assert_eq!(
            classify_foreground("zsh", &sole, Some("claude"), None, Some(false)).detected_agent(),
            Some("claude")
        );
    }

    /// R3. The detector resolved *which* agent holds the pane and then dropped
    /// the answer on the floor, leaving every surface able to say only that
    /// "some agent" was there.
    #[test]
    fn a_detected_agent_is_named_under_its_registry_name() {
        // `antigravity` runs `agy`: the pane spells the binary, a person spells
        // the agent, and the name is the half worth showing.
        let known = registry(vec![
            agent("shell", "zsh", None),
            agent("antigravity", "agy", Some("antigravity")),
        ]);
        let seen = classify_foreground("zsh", &known, Some("agy"), Some("agy"), Some(false));
        assert_eq!(
            seen,
            Corroboration::ForeignAgent(Some("antigravity".into()))
        );
        assert_eq!(seen.detected_agent(), Some("antigravity"));

        // And it reaches the assessment, beside — never instead of — the agent
        // the row was created as.
        let assessment =
            Assessment::from_hooks(&known, "shell", None, None, None, NOW).with_corroboration(seen);
        assert_eq!(assessment.detected_agent(), Some("antigravity"));
        assert_eq!(assessment.agent, "shell");
        // Named, and still not a claim about the turn: `running`, not `working`.
        assert_eq!(assessment.state(), SessionState::Running);
    }

    #[test]
    fn a_hook_report_always_outranks_the_process_observation() {
        // The agent's own report is richer than anything observable, so the
        // pane never overwrites it — it only ever adds the contradiction flag.
        assert_eq!(
            best_state(Some("blocked"), None, None, Some(&Corroboration::Shell)),
            Some((SessionState::Blocked, StateSource::Hook))
        );
        assert_eq!(
            best_state(Some("idle"), None, None, Some(&Corroboration::Agent)),
            Some((SessionState::Idle, StateSource::Hook))
        );
    }

    #[test]
    fn an_unresolvable_pane_is_unknown_rather_than_guessed() {
        let known = registry(vec![agent("claude", "claude", Some("claude"))]);
        for state in [
            classify_foreground("claude", &known, None, None, None),
            classify_foreground("claude", &known, Some(""), None, Some(false)),
        ] {
            assert_eq!(state, Corroboration::Unknown);
            assert_eq!(state.agent_present(), None);
            assert!(!contradicts(Some("working"), &state));
            assert_eq!(best_state(None, None, None, Some(&state)), None);
        }
        // A remote pane is not unknown but unreadable, and says so.
        assert_eq!(Corroboration::Unavailable.agent_present(), None);
        assert!(!contradicts(Some("working"), &Corroboration::Unavailable));
    }

    #[test]
    fn a_command_the_agent_ran_is_neither_agent_nor_shell() {
        let known = registry(vec![agent("claude", "claude", Some("claude"))]);
        let other = classify_foreground(
            "claude",
            &known,
            Some("git"),
            Some("git rebase -i origin/main"),
            Some(false),
        );
        assert_eq!(other, Corroboration::Other);
        assert_eq!(other.agent_present(), None);
        // The agent is probably still there behind it, so this never
        // contradicts an active state.
        assert!(!contradicts(Some("working"), &other));
    }
}
