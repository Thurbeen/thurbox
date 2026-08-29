//! What a session's hooks-driven state is *worth* — its age, the coverage of
//! the agent that reports it, and whether the pane agrees.
//!
//! [`crate::session::HOOK_STATES`] is the vocabulary; this module is the
//! honesty around it. `hook_state` is latched: it is whatever was written last,
//! by an agent that may since have crashed, been interrupted, or never have
//! been wired to report at all. A consumer reading the bare word cannot tell
//! `idle` ("the agent says it is at rest") from `idle` ("this agent has no hook
//! coverage and never said anything"), nor `working` ("a turn is running") from
//! `working` ("a turn was running an hour ago and the agent is gone").
//!
//! Three additive answers, none of which overwrite the stored state:
//!
//! - **Age** — [`age_secs`]. A stamp plus a duration lets a consumer apply its
//!   own policy instead of trusting a bare word. Deliberately *not* a built-in
//!   timeout: a turn may legitimately run for an hour, and a guessed bound
//!   would report it finished. The TUI has a better signal (terminal
//!   quiescence, `kernel::snapshot::with_output_quiescence`) which needs a live
//!   pane and so does not exist headless.
//! - **Coverage** — [`coverage_for`]. Which states an agent's wiring *can*
//!   produce, from the built-in `hooks` extension's payloads. `aider` reports
//!   only `blocked`; a user's own agent reports nothing. Absence of `working`
//!   means one thing for `claude` and nothing at all for `aider`.
//! - **Corroboration** — [`classify_foreground`]. What actually holds the
//!   pane's tty, from the foreground process group. This is the only check that
//!   can contradict a latched state, and the only way to see an agent thurbox
//!   never launched.
//!
//! Pure data and decisions: no process is run here, no file is read. The
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corroboration {
    /// The foreground process is the session's own agent.
    Agent,
    /// The foreground process is *a* known agent, but not the one this session
    /// was created with — an agent some driver started inside a pane thurbox
    /// opened for something else (typically a bare shell). The session is
    /// running an agent whether or not it ever signalled.
    ForeignAgent,
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
    pub fn as_str(self) -> &'static str {
        match self {
            Corroboration::Agent => "agent",
            Corroboration::ForeignAgent => "foreign-agent",
            Corroboration::Shell => "shell",
            Corroboration::Other => "other",
            Corroboration::Dead => "dead",
            Corroboration::Unknown => "unknown",
            Corroboration::Unavailable => "unavailable",
        }
    }

    /// Whether an agent process is demonstrably running in the pane.
    pub fn agent_present(self) -> Option<bool> {
        match self {
            Corroboration::Agent | Corroboration::ForeignAgent => Some(true),
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

/// Whether `command_line` runs `program` as a command rather than merely
/// mentioning it — a whole whitespace-separated token whose executable name
/// matches.
///
/// This is what keeps a wrapper honest: a remote session's window command is
/// `/bin/sh -lc 'exec claude --resume …'`, whose argv0 is `sh`. Reading argv0
/// alone would call that pane a shell and declare the agent lost.
fn runs_program(command_line: &str, program: &str) -> bool {
    command_line
        .split_whitespace()
        .any(|token| executable_name(token.trim_matches(['\'', '"'])) == program)
}

/// Judge what holds a pane against the session's agent and every agent the user
/// has defined.
///
/// `agent_command` is the session's own agent binary (`AgentDef::command`, not
/// the agent *name* — `antigravity` runs `agy`). `known_agents` is every
/// command in the registry, which is what makes an externally-launched agent
/// visible: thurbox wires no hooks for a session whose agent is `bash`, but if
/// `claude` is in the registry and `claude` holds the pane, an agent is running.
///
/// `dead` is tmux's `#{pane_dead}`; it is checked first because a dead pane
/// still answers `#{pane_current_command}` with whatever last ran there, which
/// is a plausible wrong answer rather than an honest absence.
pub fn classify_foreground(
    agent_command: &str,
    known_agents: &[String],
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
    let foreign = known_agents
        .iter()
        .map(|c| executable_name(c))
        .filter(|c| !c.is_empty() && *c != own)
        // A registry entry that *is* a shell (the bare-shell agent an external
        // driver asks for) must not make every shell look like an agent.
        .filter(|c| !SHELLS.contains(c))
        .any(|c| name == c || runs_program(line, c));
    if foreign {
        return Corroboration::ForeignAgent;
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
pub fn contradicts(state: Option<&str>, corroboration: Corroboration) -> bool {
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

/// The state word a process observation alone can justify.
///
/// Deliberately outside [`HOOK_STATES`]: an agent holding the pane is running,
/// and no amount of process inspection distinguishes a turn in flight from a
/// prompt waiting for input. Spelling it `working` would launder an
/// observation into a claim the observation cannot support.
pub const STATE_RUNNING: &str = "running";

/// The best answer available for a session, and where it came from.
///
/// The hook state wins whenever there is one — it is the agent's own report,
/// and richer than anything observable. Only when nothing ever signalled does
/// the pane get a say, and then only to the extent of [`STATE_RUNNING`].
/// `None` is the honest third outcome: no hook, no agent in the pane.
pub fn best_state(
    hook_state: Option<&str>,
    corroboration: Option<Corroboration>,
) -> Option<(String, StateSource)> {
    if let Some(state) = hook_state {
        return Some((state.to_string(), StateSource::Hook));
    }
    match corroboration {
        Some(Corroboration::Agent | Corroboration::ForeignAgent) => {
            Some((STATE_RUNNING.to_string(), StateSource::Process))
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
    pub age_secs: Option<u64>,
    /// Whether any hook has *ever* reported for this session. The distinction
    /// the brief's "explicit uninstrumented state" turns on: a session that has
    /// said nothing is not a session that said `idle`.
    pub reported: bool,
    pub coverage: Coverage,
    pub coverage_source: Option<CoverageSource>,
    pub states_reportable: &'static [&'static str],
    pub delivery: Option<HookDelivery>,
    pub blocked_is_heuristic: bool,
    /// `None` = the pane was not looked at.
    pub corroboration: Option<Corroboration>,
    pub foreground_process: Option<String>,
    pub foreground_command: Option<String>,
    /// `None` = not checked; `Some(false)` = checked and consistent.
    pub contradicted: Option<bool>,
    /// The best answer available, and where it came from — see [`best_state`].
    pub state: Option<String>,
    pub state_source: Option<StateSource>,
}

impl Assessment {
    /// What the stored hook columns and the agent registry alone can say.
    pub fn from_hooks(
        registry: &AgentRegistry,
        agent: &str,
        hook_state: Option<&str>,
        state_at: Option<i64>,
        now: i64,
    ) -> Self {
        let found = coverage_for(registry, agent);
        let state = best_state(hook_state, None);
        Self {
            hook_state: hook_state.map(str::to_string),
            state_at,
            age_secs: age_secs(state_at, now),
            reported: hook_state.is_some(),
            coverage: Coverage::of(found.map(|(c, _)| c)),
            coverage_source: found.map(|(_, source)| source),
            states_reportable: found.map(|(c, _)| c.states).unwrap_or(&[]),
            delivery: found.map(|(c, _)| c.delivery),
            blocked_is_heuristic: found.is_some_and(|(c, _)| c.blocked_is_heuristic),
            corroboration: None,
            foreground_process: None,
            foreground_command: None,
            contradicted: None,
            state: state.as_ref().map(|(s, _)| s.clone()),
            state_source: state.map(|(_, source)| source),
        }
    }

    /// Fold in what the pane's foreground process says.
    ///
    /// The stored state is left exactly as it was; the observation only adds
    /// [`Self::contradicted`], and only fills [`Self::state`] when nothing ever
    /// signalled (an agent thurbox did not launch, and so never wired).
    pub fn with_pane(
        mut self,
        agent_command: &str,
        known_agents: &[String],
        process: Option<&str>,
        command_line: Option<&str>,
        dead: Option<bool>,
    ) -> Self {
        let corroboration =
            classify_foreground(agent_command, known_agents, process, command_line, dead);
        self.contradicted = Some(contradicts(self.hook_state.as_deref(), corroboration));
        self.foreground_process = process.filter(|p| !p.is_empty()).map(str::to_string);
        self.foreground_command = command_line.filter(|c| !c.is_empty()).map(str::to_string);
        self.corroboration = Some(corroboration);
        if let Some((state, source)) = best_state(self.hook_state.as_deref(), Some(corroboration)) {
            self.state = Some(state);
            self.state_source = Some(source);
        }
        self
    }

    /// Record that the pane could not be looked at from here — a remote
    /// session's pane lives on its own host's multiplexer. Distinct from
    /// leaving the corroboration unset, which means nobody tried.
    pub fn pane_unavailable(mut self) -> Self {
        self.corroboration = Some(Corroboration::Unavailable);
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
        let known = vec!["claude".to_string(), "codex".to_string()];
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
        let known = vec!["claude".to_string()];
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
        let known = vec!["claude".to_string()];
        let shell =
            classify_foreground("claude", &known, Some("-bash"), Some("-bash"), Some(false));
        assert_eq!(shell, Corroboration::Shell);
        assert_eq!(shell.agent_present(), Some(false));
        assert!(contradicts(Some("working"), shell));
        assert!(contradicts(Some("blocked"), shell));
        // A finished turn asserts nothing about a live process.
        assert!(!contradicts(Some("done"), shell));
        assert!(!contradicts(Some("idle"), shell));
        assert!(!contradicts(None, shell));
    }

    #[test]
    fn a_dead_pane_beats_the_command_name_it_still_answers_with() {
        // `remain-on-exit` keeps the frame, and tmux keeps naming the command
        // that died there — a plausible wrong answer, so deadness wins.
        let known = vec!["claude".to_string()];
        let dead = classify_foreground("claude", &known, Some("claude"), None, Some(true));
        assert_eq!(dead, Corroboration::Dead);
        assert!(contradicts(Some("working"), dead));
    }

    #[test]
    fn an_agent_thurbox_did_not_launch_is_still_seen() {
        // The externally-driven shape: the session's agent is a bare shell (so
        // thurbox wired no hooks and nothing ever signalled), and a driver
        // started a real agent inside the pane.
        let known = vec!["bash".to_string(), "claude".to_string()];
        let seen = classify_foreground(
            "bash",
            &known,
            Some("claude"),
            Some("claude --permission-mode acceptEdits"),
            Some(false),
        );
        assert_eq!(seen, Corroboration::ForeignAgent);
        assert_eq!(seen.agent_present(), Some(true));
        assert_eq!(
            best_state(None, Some(seen)),
            Some((STATE_RUNNING.to_string(), StateSource::Process))
        );

        // And the shell that pane was asked for is still just a shell — even
        // though it is this session's own "agent". Reporting an empty terminal
        // as an agent running is the failure this branch exists to avoid.
        let idle = classify_foreground("bash", &known, Some("bash"), Some("bash -i"), Some(false));
        assert_eq!(idle, Corroboration::Shell);
        assert_eq!(best_state(None, Some(idle)), None);
    }

    #[test]
    fn a_hook_report_always_outranks_the_process_observation() {
        // The agent's own report is richer than anything observable, so the
        // pane never overwrites it — it only ever adds the contradiction flag.
        assert_eq!(
            best_state(Some("blocked"), Some(Corroboration::Shell)),
            Some(("blocked".to_string(), StateSource::Hook))
        );
        assert_eq!(
            best_state(Some("idle"), Some(Corroboration::Agent)),
            Some(("idle".to_string(), StateSource::Hook))
        );
    }

    #[test]
    fn an_unresolvable_pane_is_unknown_rather_than_guessed() {
        let known = vec!["claude".to_string()];
        for state in [
            classify_foreground("claude", &known, None, None, None),
            classify_foreground("claude", &known, Some(""), None, Some(false)),
        ] {
            assert_eq!(state, Corroboration::Unknown);
            assert_eq!(state.agent_present(), None);
            assert!(!contradicts(Some("working"), state));
            assert_eq!(best_state(None, Some(state)), None);
        }
        // A remote pane is not unknown but unreadable, and says so.
        assert_eq!(Corroboration::Unavailable.agent_present(), None);
        assert!(!contradicts(Some("working"), Corroboration::Unavailable));
    }

    #[test]
    fn a_command_the_agent_ran_is_neither_agent_nor_shell() {
        let known = vec!["claude".to_string()];
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
        assert!(!contradicts(Some("working"), other));
    }
}
