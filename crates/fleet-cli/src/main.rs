//! `fleet`: the command line of the Rust rewrite. It parses arguments, opens
//! the store and the runtime the config names, calls fleet-manager, and
//! prints. Every rule about what may be written lives in fleet-manager and
//! below; nothing here decides.

mod bootstrap;
mod config;
mod service;
mod supervisor;

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use fleet_api::change_request::ChangeRequestDefaults;
use fleet_api::follow::ForgeDefaults;
use fleet_api::forge::Forge;
use fleet_api::gateway::Gateway;
use fleet_api::result::{Outcome, TaskResult};
use fleet_api::runtime::Runtime;
use fleet_api::{API_VERSION, Object, Observation, Store, StoreError};
use fleet_forge::{ForgeHosts, HostConfig, HostKind};
use fleet_gateway::{GatewayAccounts, Platform, account_digest, listener_account};
use fleet_manager::{
    AmendRequest, DeclareOutcome, GatewaySide, Manager, RunOptions, amend, apply,
    apply_change_request, apply_conversation, beat, declare, declare_change_request, delete, label,
    list_events, publish_event, receive, record_result, send, wake,
};
use fleet_runtime::{ExecProgram, ExecRuntime, FakeHost, FakeRuntime, FileSignals};
use fleet_store::manager::{ManagerLock, ManagerRecord, ManagerState};
use fleet_store::{FileStore, SystemClock};
use otel_env::KeyValue;
use serde_json::{Value, json};

use crate::config::{CONFIG_VERSION, Config, default_data_dir};

/// The released version when CI stamps one, else the manifest's.
const VERSION: &str = match option_env!("FLEET_RELEASE_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Debug, Parser)]
#[command(name = "fleet", version = VERSION, about = "Fleet: one agent session per Task, reconciled from a file store.")]
struct Cli {
    /// Config file (else FLEET_CONFIG, then the native per-user config).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// The data directory (overrides the config file's data.dir).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create the local config, offline; an existing config is kept.
    Init,
    /// Keep every due Task reconciling until Ctrl-C.
    Up {
        /// Interval between bounded sweeps (100..=10000 ms).
        #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u64).range(100..=10000))]
        poll_ms: u64,
        /// Manager lease; must outlast both runtime operations and poll interval.
        #[arg(long)]
        lease_ttl_ms: Option<u64>,
    },
    /// Create or update resources from a JSON file (an object, an array, or {"items": [...]}); `-` reads stdin.
    Apply {
        #[arg(short = 'f', long = "filename")]
        file: PathBuf,
        #[arg(long)]
        json: bool,
        /// Create each object, or refuse one whose name is taken (exit 5),
        /// leaving it as it is; never update.
        #[arg(long)]
        create_only: bool,
    },
    /// Print one resource as JSON.
    Get { kind: String, name: String },
    /// List every resource of a kind.
    List {
        kind: String,
        #[arg(long)]
        json: bool,
    },
    /// Show what a resource's status says, and whether it is current.
    Status {
        kind: String,
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// The worker's answer: record the result of a briefed Task.
    Result {
        task: String,
        /// The generation the brief was delivered for.
        #[arg(long)]
        generation: Option<u64>,
        /// shipped, stuck, failed or not-applicable.
        #[arg(long)]
        outcome: Option<String>,
        #[arg(long, default_value = "")]
        summary: String,
        #[arg(long = "artifact")]
        artifacts: Vec<String>,
        /// A structured output as NAME=JSON, repeatable.
        #[arg(long = "output")]
        outputs: Vec<String>,
        /// The whole result as a JSON file instead of the flags above.
        #[arg(long, conflicts_with_all = ["generation", "outcome", "summary", "artifacts", "outputs"])]
        file: Option<PathBuf>,
    },
    /// The worker's declaration: the change it opened, or any fact as a JSON object with a `kind`.
    Declare {
        task: String,
        /// The URL of the change the worker opened; one ChangeRequest is ensured for it and followed.
        #[arg(long, value_name = "URL", conflicts_with = "json")]
        change_request: Option<String>,
        /// Any JSON object with a string `kind`, appended as is.
        #[arg(long, value_name = "OBJECT")]
        json: Option<String>,
    },
    /// Queue a message for a Task's session; the controller delivers it on its next pass.
    Send {
        task: String,
        /// The message, delivered as typed; it may start with a dash.
        #[arg(allow_hyphen_values = true)]
        text: String,
        /// A declared Question this message answers, by id; repeatable.
        /// Recorded in the Task's status.answered, first answer wins.
        #[arg(long = "answers", value_name = "ID")]
        answers: Vec<String>,
        /// Print the appended inbox entry as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Add to a Task's brief: the amendment is kept in its spec, delivered to the same session, and re-opens a Concluded Task.
    Amend {
        task: String,
        /// What the amendment adds.
        #[arg(
            long,
            allow_hyphen_values = true,
            required_unless_present = "file",
            conflicts_with = "file"
        )]
        text: Option<String>,
        /// Read the amendment from a file, or from stdin with `-`.
        #[arg(long, value_name = "PATH")]
        file: Option<PathBuf>,
        /// Why, for whoever reads the Task later.
        #[arg(long)]
        reason: Option<String>,
        /// A new output a result must carry, as NAME=TYPE; a trailing `!`
        /// makes it required. Repeatable.
        #[arg(long = "output", value_name = "NAME=TYPE[!]")]
        outputs: Vec<String>,
        /// Refuse unless the Task is at this generation.
        #[arg(long, value_name = "N")]
        if_generation: Option<u64>,
        /// Print what was appended as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Set labels on a Task, KEY=VALUE, and change nothing else of it.
    Label {
        /// `task`: the only kind this sets labels on.
        kind: String,
        name: String,
        /// `KEY=VALUE`; a label Fleet alone writes is refused.
        #[arg(required = true, value_parser = parse_label)]
        labels: Vec<(String, String)>,
    },
    /// Delete a resource; a Task is released by its controller first.
    Delete { kind: String, name: String },
    /// The command that opens a Task's terminal stream: as JSON with
    /// --json, else one argument per line.
    Attach {
        task: String,
        #[arg(long)]
        json: bool,
        /// Resume a paused Task first, and wait for it to run again.
        #[arg(long)]
        wake: bool,
    },
    /// The command that opens an interactive shell beside a Task's session,
    /// on its place, as a terminal stream: as JSON with --json, else one
    /// argument per line.
    Shell {
        task: String,
        #[arg(long)]
        json: bool,
        /// Resume a paused Task first, and wait for it to run again.
        #[arg(long)]
        wake: bool,
    },
    /// Follow the Tasks: one event per changed or deleted Task, as NDJSON
    /// with --json, until Ctrl-C or --once.
    Watch {
        /// Report only writes after this store revision (a `revision` an
        /// earlier watch printed).
        #[arg(long, default_value_t = 0)]
        since: u64,
        /// Stop after the first `synced` event.
        #[arg(long)]
        once: bool,
        /// Interval between reads of the store (50..=10000 ms).
        #[arg(long, default_value_t = 500, value_parser = clap::value_parser!(u64).range(50..=10000))]
        poll_ms: u64,
        #[arg(long)]
        json: bool,
    },
    /// Run the Task controller under the manager lease until idle, or for at most --max-passes passes.
    Reconcile {
        #[arg(long, default_value_t = 50)]
        max_passes: u32,
        /// The lease holder name; defaults to one unique to this process.
        #[arg(long)]
        holder: Option<String>,
        /// The manager lease; by default 30 s, raised to twice the longest
        /// single runtime operation.
        #[arg(long)]
        lease_ttl_ms: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    /// Events on a Board: publish one, or list, wait for or watch what it holds.
    Event {
        #[command(subcommand)]
        command: EventCommand,
    },
    /// Chat accounts: what a listener runs per event and per beat, and what it starts from.
    Gateway {
        #[command(subcommand)]
        command: GatewayCommand,
    },
    /// The binary's version and every schema it reads and writes.
    Version {
        #[arg(long)]
        json: bool,
    },
    /// Report the agent's state from its own hook: appends to the file
    /// Fleet passed the session as FLEET_SIGNAL_FILE, and does nothing
    /// outside a session Fleet started.
    Signal {
        state: SignalState,
        /// The harness whose profile names the payload's fields (else
        /// FLEET_HARNESS, which Fleet sets at spawn).
        #[arg(long)]
        harness: Option<String>,
        /// The hook's JSON, when it passes it as an argument; otherwise it
        /// is read from stdin when stdin is not a terminal.
        payload: Option<String>,
    },
    /// Print a session's signal file on this machine, for the Fleet that
    /// reads it through a runtime's exec: the end of what it held before
    /// SINCE, a split line, then the end of what came after. Exits 3 when
    /// the file is not there.
    #[command(hide = true)]
    SignalRead {
        /// Print only the file's length in bytes, 0 when it is not there.
        #[arg(long)]
        length: bool,
        /// The file, `~`-rooted on another host than Fleet's.
        file: PathBuf,
        /// The byte offset a restart left the file at.
        since: Option<u64>,
        /// Read a session's outbox instead: print the offset the read
        /// starts at on a line, then the bytes from this offset on (from
        /// the start when the file is shorter).
        #[arg(long, value_name = "OFFSET", conflicts_with_all = ["length", "since"])]
        from: Option<u64>,
    },
    /// A raw interactive command as a terminal stream on stdin and stdout:
    /// what `fleet attach` wraps a runtime's `tty` answer in.
    TermBridge {
        /// The command and its arguments.
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
    /// Runtimes: programs that speak the runtime protocol.
    Runtime {
        #[command(subcommand)]
        command: RuntimeCommand,
    },
    /// The hooks Fleet installs into an agent's own config directory, on
    /// this machine, for an agent that takes none at launch.
    Integration {
        #[command(subcommand)]
        command: IntegrationCommand,
    },
    /// The systemd user unit that runs `fleet up` for this store: Fleet
    /// writes it and drives it.
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    /// Check this machine: each installed integration, each harness's
    /// program on PATH, and each host's runtime. Exits 1 only on a fail.
    Doctor {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    /// Write the unit for the config, adopting a unit of the same name and
    /// keeping its drop-ins, and enable it. Refuses a second unit for the
    /// same store (exit 5).
    Install {
        /// The unit's name, without `.service`.
        #[arg(long, default_value = service::DEFAULT_NAME)]
        name: String,
        /// Start it now too.
        #[arg(long)]
        now: bool,
    },
    /// Start the store's unit.
    Start {
        #[arg(long)]
        name: Option<String>,
    },
    /// Stop the store's unit; the sessions keep running.
    Stop {
        #[arg(long)]
        name: Option<String>,
    },
    /// Restart the store's unit.
    Restart {
        #[arg(long)]
        name: Option<String>,
    },
    /// What systemd says of the store's unit; exits as `systemctl status`
    /// does (0 active, 3 not running).
    Status {
        #[arg(long)]
        name: Option<String>,
    },
    /// Stop and disable the store's unit and remove its file; its drop-ins
    /// stay.
    Uninstall {
        #[arg(long)]
        name: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum IntegrationCommand {
    /// Each agent's integration: not-needed, not-installed, current,
    /// outdated, modified or agent-missing. Every agent without one.
    Status {
        agent: Option<String>,
        #[arg(long, conflicts_with = "agent")]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Write the agent's hooks into its config directory, replacing an
    /// older version Fleet wrote.
    Install { agent: String },
    /// Remove exactly what Fleet wrote for the agent, or for every agent.
    Uninstall {
        #[arg(required_unless_present = "all")]
        agent: Option<String>,
        #[arg(long, conflicts_with = "agent")]
        all: bool,
    },
}

#[derive(Debug, Subcommand)]
enum RuntimeCommand {
    /// Run the conformance suite against a runtime program: `fleet runtime check -- <program> [args]`.
    Check {
        /// The bound on each call, in milliseconds.
        #[arg(long, default_value_t = DEFAULT_CALL_TIMEOUT_MS)]
        call_timeout_ms: u64,
        /// The program and the arguments it takes before the verb.
        #[arg(last = true, required = true)]
        program: Vec<String>,
    },
    /// The hosts a Task may name: `local` and each `<runtime>/<place>` it
    /// does not stand for, or none (`null` with --json) when one `[runtime]`
    /// serves any host.
    Hosts {
        #[arg(long)]
        json: bool,
    },
    /// Every place of every runtime, with what the runtime's manifest says
    /// of it: ready, not ready and why, or unknown when it did not answer.
    Places {
        #[arg(long)]
        json: bool,
    },
    /// The folders of a place, read through its runtime's `browse`: each
    /// subdirectory of `dir` (the place's home when omitted), marked when it
    /// is a git work tree. Read-only.
    Browse {
        /// The place: `local`, or `<runtime>/<place>`.
        place: String,
        /// An absolute directory on the place.
        dir: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// The branches of a repository on a place, read through its runtime's
    /// `branches`, with the one a new branch starts from by default.
    /// Read-only.
    Branches {
        /// The place: `local`, or `<runtime>/<place>`.
        place: String,
        /// The absolute path of the repository on the place.
        repo: String,
        #[arg(long)]
        json: bool,
    },
}

/// What a hook reports (`fleet_api::signal::State`).
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum SignalState {
    Working,
    Blocked,
    Idle,
    Prompt,
}

#[derive(Debug, Subcommand)]
enum GatewayCommand {
    /// Read one delivery envelope from stdin (or, with --beat, one heartbeat) and record it on the thread's Conversation (or the account's Listener).
    Receive {
        /// The `[gateway.accounts]` entry the listener delivers for.
        #[arg(long)]
        account: String,
        /// Stdin holds a heartbeat, not an envelope.
        #[arg(long)]
        beat: bool,
        #[arg(long)]
        json: bool,
    },
    /// What a listener needs to connect to an account, with the digest its beats carry.
    Account {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Each account's listener: accepted, resolved, inbound, last delivery.
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum EventCommand {
    /// Append one event to a Board as cli/USER, and print it with its seq.
    ///
    /// Inside a Fleet session (FLEET_EVENT_FILE set), append it to the
    /// session's outbox instead, and print the line's outbox offset: Fleet
    /// ingests it into the Board as the session's Task. A session with
    /// FLEET_SIGNAL_FILE but no FLEET_EVENT_FILE refuses.
    Publish {
        /// The Board to publish to.
        #[arg(long, default_value = fleet_api::board::SHARED)]
        board: String,
        /// The event type: dot-separated segments of a-z, 0-9, '_' and '-';
        /// the fleet. prefix is Fleet's own.
        #[arg(long = "type", value_name = "TYPE")]
        event_type: String,
        /// What the event is about, such as a repository or a path.
        #[arg(long)]
        subject: Option<String>,
        /// A key that makes a retried publish the same event: the board
        /// answers the first event published under it, and appends nothing.
        #[arg(long)]
        key: Option<String>,
        /// An id that ties the events of one piece of work together.
        #[arg(long)]
        correlation: Option<String>,
        /// The event's data, as JSON.
        #[arg(long, value_name = "JSON", allow_hyphen_values = true)]
        data: String,
        /// Inside a session, wait until Fleet has read the line into the
        /// Board, then print the event with its seq; exit 1 when Fleet
        /// skipped the line, and 7 with the outbox offset on a timeout.
        /// Outside a session the publish is already done when it prints.
        #[arg(long)]
        wait: bool,
        /// How long --wait waits.
        #[arg(long, value_name = "MS", default_value_t = 60_000, requires = "wait")]
        timeout_ms: u64,
        /// Print the event as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List a Board's events after a cursor, oldest first.
    List {
        /// Only events with a seq above this cursor; 0 lists from the start.
        #[arg(long, value_name = "SEQ", default_value_t = 0)]
        after: u64,
        #[command(flatten)]
        filter: EventFilter,
        /// The most events to print.
        #[arg(long, value_name = "N", default_value_t = 100)]
        limit: usize,
        /// Print the events as a JSON array.
        #[arg(long)]
        json: bool,
    },
    /// Wait until a Board holds an event after a cursor that matches, then
    /// print the matches as list does; exit 7, printing nothing, when none
    /// comes before the timeout.
    Wait {
        /// Only events with a seq above this cursor: the last seq read.
        #[arg(long, value_name = "SEQ")]
        after: u64,
        #[command(flatten)]
        filter: EventFilter,
        /// How long to wait for a match.
        #[arg(long, value_name = "MS")]
        timeout_ms: u64,
        /// Interval between reads of the Board (50..=10000 ms).
        #[arg(long, default_value_t = 500, value_parser = clap::value_parser!(u64).range(50..=10000))]
        poll_ms: u64,
        /// The most events to print.
        #[arg(long, value_name = "N", default_value_t = 100)]
        limit: usize,
        /// Print the events as a JSON array.
        #[arg(long)]
        json: bool,
    },
    /// Print each event after a cursor that matches, oldest first, then
    /// each new one as it is published, until stdout closes.
    Watch {
        /// Only events with a seq above this cursor: the last seq read.
        #[arg(long, value_name = "SEQ")]
        after: u64,
        #[command(flatten)]
        filter: EventFilter,
        /// Interval between reads of the Board (50..=10000 ms).
        #[arg(long, default_value_t = 500, value_parser = clap::value_parser!(u64).range(50..=10000))]
        poll_ms: u64,
        /// Print one event per line as JSON (NDJSON).
        #[arg(long)]
        json: bool,
    },
    /// Subscribe a Task to a Board: when events after its cursor match,
    /// Fleet queues one notice in the Task's inbox with their count and seq
    /// range, and the session reads them with list. While that notice is
    /// still to deliver, no other notice is queued for the subscription.
    Subscribe {
        /// The Task whose session hears of the events.
        task: String,
        #[command(flatten)]
        filter: EventFilter,
        /// The seq of the last event the Task knows of already; without it,
        /// the Board's head when Fleet first reads the subscription.
        #[arg(long, value_name = "SEQ")]
        after: Option<u64>,
    },
    /// Remove a Task's subscription to a Board with the same filter.
    Unsubscribe {
        /// The Task that subscribes.
        task: String,
        #[command(flatten)]
        filter: EventFilter,
    },
}

#[derive(Debug, clap::Args)]
struct EventFilter {
    /// The Board to read.
    #[arg(long, default_value = fleet_api::board::SHARED)]
    board: String,
    /// Only this event type; a trailing * matches a prefix.
    #[arg(long = "type", value_name = "TYPE")]
    event_type: Option<String>,
    /// Only this subject; a trailing * matches a prefix.
    #[arg(long)]
    subject: Option<String>,
    /// Only events from this source, such as cli/USER or task/NAME.
    #[arg(long)]
    source: Option<String>,
    /// Only events with this correlation id.
    #[arg(long)]
    correlation: Option<String>,
}

impl EventFilter {
    fn cursor(self, after: u64) -> fleet_manager::verbs::EventCursor {
        let (board, filter) = self.split();
        fleet_manager::verbs::EventCursor {
            board,
            after,
            filter,
        }
    }

    fn split(self) -> (String, fleet_api::event::Filter) {
        use fleet_api::event::{Filter, Pattern};
        let filter = Filter {
            event_type: self.event_type.as_deref().map(Pattern::new),
            subject: self.subject.as_deref().map(Pattern::new),
            source: self.source,
            correlation: self.correlation,
        };
        (self.board, filter)
    }
}

/// Exit codes, stable for scripts.
mod exit {
    pub const ERROR: u8 = 1;
    pub const NOT_FOUND: u8 = 3;
    pub const UNKNOWN: u8 = 4;
    pub const CONFLICT: u8 = 5;
    pub const LEASE: u8 = 6;
    pub const TIMEOUT: u8 = 7;
    pub const CONFIG: u8 = 78;
}

struct Failure {
    code: u8,
    message: String,
    hint: Option<String>,
}

impl From<StoreError> for Failure {
    fn from(e: StoreError) -> Failure {
        let (code, hint) = match &e {
            StoreError::NotFound(_) => (exit::NOT_FOUND, None),
            StoreError::Unknown(_) => (
                exit::UNKNOWN,
                Some(
                    "fleet could not tell; nothing was assumed. Check the path's permissions and contents.",
                ),
            ),
            StoreError::Conflict { .. } | StoreError::Busy(_) => (
                exit::CONFLICT,
                Some("another writer got there first; run the command again"),
            ),
            StoreError::Precondition(_) => (
                exit::CONFLICT,
                Some("read the resource again and decide whether the command still applies"),
            ),
            StoreError::LeaseHeld { .. } | StoreError::LeaseLost(_) => (
                exit::LEASE,
                Some("one manager runs at a time; wait for it or for its lease to expire"),
            ),
            StoreError::Unsupported(_) => (
                exit::ERROR,
                Some("see docs/fleet/contracts.md for what this build serves"),
            ),
            _ => (exit::ERROR, None),
        };
        Failure {
            code,
            message: e.to_string(),
            hint: hint.map(str::to_string),
        }
    }
}

/// `KEY=VALUE`, a label to set; the key is not empty.
fn parse_label(text: &str) -> Result<(String, String), String> {
    match text.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.to_string(), value.to_string())),
        _ => Err(format!("{text:?}: expected KEY=VALUE")),
    }
}

/// `NAME=TYPE`, with a trailing `!` for a required output.
fn parse_output_decl(text: &str) -> Result<(String, fleet_api::result::OutputDecl), Failure> {
    let usage = || {
        fail(format!(
            "--output {text:?}: use NAME=TYPE[!], TYPE one of string, integer, number, boolean, object, array"
        ))
    };
    let (name, ty) = text.split_once('=').ok_or_else(usage)?;
    let (ty, required) = match ty.strip_suffix('!') {
        Some(ty) => (ty, true),
        None => (ty, false),
    };
    let name = name.trim();
    if name.is_empty() {
        return Err(usage());
    }
    let type_ =
        serde_json::from_value(Value::String(ty.trim().to_string())).map_err(|_| usage())?;
    Ok((
        name.to_string(),
        fleet_api::result::OutputDecl { type_, required },
    ))
}

fn fail(message: impl Into<String>) -> Failure {
    Failure {
        code: exit::ERROR,
        message: message.into(),
        hint: None,
    }
}

fn refusal_no_retry_fixes(command: &Command, message: String) -> Failure {
    match command {
        Command::Up { .. } => Failure {
            code: exit::CONFIG,
            message,
            hint: Some("fix the config or the binary, then start fleet up again".into()),
        },
        _ => fail(message),
    }
}

fn main() -> ExitCode {
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    let name = matches.subcommand_name().unwrap_or_default().to_string();
    let adopted = match config::discover(cli.config.clone()).map(|p| config::telemetry(&p)) {
        Some(Ok(Some(table))) => otel_env::adopt(&table),
        Some(Err(why)) => {
            otel_env::disable();
            Err(why)
        }
        _ => Ok(()),
    };
    if let Err(why) = adopted {
        eprintln!("warning: telemetry is off: {why}");
    }
    let telemetry = otel_env::init("fleet", VERSION, &[], false).unwrap_or_else(|why| {
        eprintln!("warning: telemetry is off: {why}");
        otel_env::Telemetry::default()
    });
    let command = || match run(cli) {
        Ok(()) => 0,
        Err(f) => {
            eprintln!("error: {}", f.message);
            if let Some(h) = f.hint {
                eprintln!("hint: {h}");
            }
            f.code
        }
    };
    // `up` runs until Ctrl-C: each of its sweeps is a trace of its own.
    // Any other command is the root span `fleet NAME`, under the span its
    // caller named in TRACEPARENT.
    let code = if name == "up" {
        command()
    } else {
        let root = otel_env::root(
            "fleet",
            format!("fleet {name}"),
            vec![KeyValue::new("fleet.command", name.clone())],
        );
        let code = command();
        root.set(KeyValue::new("process.exit.code", i64::from(code)));
        if code != 0 {
            root.fail(format!("exit {code}"));
        }
        code
    };
    // A one-shot command answers whoever ran it (thurbox's probes, an agent's
    // hook), often under a deadline: its output is written, and its flush
    // stays inside the call bound. Only `up`, stopped by Ctrl-C, waits the
    // longer bound for its last sweeps (docs/observability.md).
    if name == "up" {
        telemetry.shutdown();
    } else {
        telemetry.shutdown_within(otel_env::CALL_SHUTDOWN_BOUND);
    }
    ExitCode::from(code)
}

fn run(cli: Cli) -> Result<(), Failure> {
    if let Command::Version { json } = &cli.command {
        return version(*json, cli.config.clone(), cli.data_dir.clone());
    }
    if let Command::Signal {
        state,
        harness,
        payload,
    } = &cli.command
    {
        return cmd_signal(
            *state,
            harness.clone(),
            payload.as_deref(),
            cli.config.clone(),
        );
    }
    if let Command::SignalRead {
        length,
        file,
        since,
        from,
    } = &cli.command
    {
        return match from {
            Some(from) => cmd_outbox_read(file, *from),
            None => cmd_signal_read(*length, file, *since),
        };
    }
    let mut queued = None;
    if let Command::Event {
        command:
            EventCommand::Publish {
                board,
                event_type,
                subject,
                key,
                correlation,
                data,
                wait,
                timeout_ms,
                json,
            },
    } = &cli.command
        && let Some(outbox) = session_outbox()?
    {
        let publish = publish_of(event_type, subject, key, correlation, data)?;
        let line = fleet_api::event::OutboxLine::new(board, publish);
        if !*wait {
            let (offset, _) = session_append(&outbox, &line)?;
            print_queued(&line.board, &line.event_type, offset, *json);
            return Ok(());
        }
        line.encode().map_err(fail)?;
        queued = Some(Queued {
            session_key: outbox_session_key(&outbox)?,
            outbox,
            line,
            timeout_ms: *timeout_ms,
            json: *json,
        });
    }
    if let Command::TermBridge { argv } = &cli.command {
        return fleet_runtime::bridge::serve(argv, std::io::stdin(), std::io::stdout())
            .map_err(fail);
    }
    if let Command::Runtime {
        command: RuntimeCommand::Check {
            call_timeout_ms,
            program,
        },
    } = &cli.command
    {
        return runtime_check(program, *call_timeout_ms);
    }
    let explicit_config = cli.config.is_some() || std::env::var_os(config::CONFIG_ENV).is_some();
    let config_path = config::discover(cli.config.clone())
        .map(|p| bootstrap::absolute(&p))
        .transpose()
        .map_err(fail)?;
    if let Command::Init = cli.command {
        let path = config_path
            .ok_or_else(|| fail("no config location: pass --config or set HOME/APPDATA"))?;
        let root = default_data_dir()
            .unwrap_or_else(|| path.parent().unwrap_or(Path::new(".")).join("fleet-data"));
        let written = bootstrap::init(&path, &root, cli.data_dir.as_deref()).map_err(fail)?;
        let config = Config::load(&path).map_err(fail)?;
        let data_dir = cli
            .data_dir
            .clone()
            .or(config.data.dir.clone())
            .or_else(default_data_dir)
            .ok_or_else(|| fail("no data directory: set data.dir in the config file"))?;
        let store = FileStore::open(&data_dir)?;
        default_task_config(&store)?;
        shared_board(&store)?;
        // The profiles `fleet doctor` checks: the bundled ones, then the
        // user's beside this config.
        let harnesses = harness_dir(Some(&path));
        let user = user_profiles(harnesses.as_deref(), true).map_err(fail)?;
        let profiles = fleet_api::harness::Profiles::bundled().with(user);
        let named = explicit_config.then_some(path.as_path());
        print!(
            "{}",
            init_summary(written, &path, &config, &profiles, named)
        );
        return Ok(());
    }
    let config = match &config_path {
        Some(p) if !explicit_config && !p.try_exists().map_err(|e| fail(e.to_string()))? => {
            Config::empty()
        }
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .map_err(|e| fail(format!("cannot read config {}: {e}", p.display())))?;
            let mut config = Config::parse_settings(&text, p)
                .map_err(|why| refusal_no_retry_fixes(&cli.command, why))?;
            config.read_briefs(p).map_err(fail)?;
            config
        }
        None => Config::empty(),
    };
    let harnesses = harness_dir(config_path.as_deref());
    let user = user_profiles(harnesses.as_deref(), true).map_err(fail)?;
    let mut profiles = fleet_api::harness::Profiles::bundled().with(user);
    if let Some(dir) = harnesses {
        profiles = profiles.from_dir(dir.to_string_lossy().into_owned());
    }
    if let Command::Runtime {
        command: RuntimeCommand::Hosts { json },
    } = &cli.command
    {
        // Runtime programs serve exactly their places, as `open_runtime`
        // wires them; `local` stands for the default runtime's own.
        let hosts = (!config.runtimes.is_empty()).then(|| {
            let places = config.places();
            let mut hosts: Vec<String> = places
                .iter()
                .filter(|p| p.local)
                .map(|_| config::LOCAL.to_string())
                .collect();
            hosts.extend(places.into_iter().filter(|p| !p.local).map(|p| p.id));
            hosts
        });
        if *json {
            print_json(&json!({ "hosts": hosts }));
        } else {
            match hosts {
                Some(hosts) => hosts.iter().for_each(|host| println!("{host}")),
                None => println!("any host: one [runtime] serves them all"),
            }
        }
        return Ok(());
    }
    if let Command::Runtime {
        command: RuntimeCommand::Browse { place, dir, json },
    } = &cli.command
    {
        let program = place_program(&config, place)?;
        return match program.browse(dir.as_deref()) {
            Observation::Known(fleet_runtime::exec::Browsed::Listing { dir, entries }) => {
                if *json {
                    print_json(&json!({ "dir": dir, "entries": entries }));
                } else {
                    for entry in entries {
                        let git = if entry.repo { "  (git)" } else { "" };
                        println!("{}/{git}", entry.name);
                    }
                }
                Ok(())
            }
            Observation::Known(fleet_runtime::exec::Browsed::Failed { why })
            | Observation::Unknown { why } => Err(fail(why)),
        };
    }
    if let Command::Runtime {
        command: RuntimeCommand::Branches { place, repo, json },
    } = &cli.command
    {
        let program = place_program(&config, place)?;
        return match program.branches(repo) {
            Observation::Known(fleet_runtime::exec::BranchesRead::Branches { names, default }) => {
                if *json {
                    print_json(&json!({ "names": names, "default": default }));
                } else {
                    for name in &names {
                        let mark = if Some(name) == default.as_ref() {
                            "* "
                        } else {
                            "  "
                        };
                        println!("{mark}{name}");
                    }
                }
                Ok(())
            }
            Observation::Known(fleet_runtime::exec::BranchesRead::Failed { why })
            | Observation::Unknown { why } => Err(fail(why)),
        };
    }
    if let Command::Runtime {
        command: RuntimeCommand::Places { json },
    } = &cli.command
    {
        let places = (!config.runtimes.is_empty()).then(|| runtime_places(&config));
        let local = config.places().into_iter().find(|p| p.local).map(|p| p.id);
        let no_local = config.no_local();
        if *json {
            print_json(&json!({ "places": places, "local": local, "localWhy": no_local }));
        } else {
            if let Some(why) = &no_local {
                println!("{why}");
            }
            match places {
                Some(places) => {
                    for p in places {
                        let state = match p["ready"].as_bool() {
                            Some(true) => "ready",
                            Some(false) => "not ready",
                            None => "unknown",
                        };
                        let why = p["why"]
                            .as_str()
                            .map(|w| format!(": {w}"))
                            .unwrap_or_default();
                        println!(
                            "{:<24} {state}{why}",
                            p["place"].as_str().unwrap_or_default()
                        );
                    }
                }
                None => println!("any place: one [runtime] serves them all"),
            }
        }
        return Ok(());
    }
    let data_dir = cli
        .data_dir
        .clone()
        .or(config.data.dir.clone())
        .or_else(default_data_dir)
        .ok_or_else(|| {
            fail("no data directory: pass --data-dir or set data.dir in the config file")
        })?;
    // Before the store opens: a doctor writes nothing.
    if let Command::Service { command } = &cli.command {
        if cli.data_dir.is_some() {
            return Err(fail(
                "fleet service runs fleet up with the config alone: persist data.dir in the config instead of passing --data-dir",
            ));
        }
        let data_dir = bootstrap::absolute(&data_dir).map_err(fail)?;
        return cmd_service(command, config_path.as_deref(), &config, &data_dir);
    }
    if let Command::Doctor { json } = &cli.command {
        return cmd_doctor(&config, &profiles, &data_dir, *json);
    }
    // The plugin files a profile names sit under the data dir, where
    // `open_signals` writes them.
    fleet_api::harness::install(
        profiles.with_assets(
            bootstrap::absolute(&data_dir.join(fleet_api::harness::ASSET_DIR))
                .map_err(fail)?
                .to_string_lossy()
                .into_owned(),
        ),
    );
    if matches!(cli.command, Command::Up { .. })
        && let Some(override_dir) = &cli.data_dir
    {
        let configured = config
            .data
            .dir
            .clone()
            .or_else(default_data_dir)
            .ok_or_else(|| fail("no configured data directory"))?;
        if bootstrap::absolute(override_dir).map_err(fail)?
            != bootstrap::absolute(&configured).map_err(fail)?
        {
            return Err(fail(
                "fleet up requires workers to share its config: persist data.dir in that config instead of using a differing --data-dir override",
            ));
        }
    }
    let plugins = fleet_plugin::StdioPlugins::new(config.plugins.clone());
    let registrations = if matches!(
        cli.command,
        Command::Up { .. } | Command::Reconcile { .. } | Command::Apply { .. }
    ) || matches!(&cli.command,Command::List{kind,..} if kind=="plugins")
    {
        fleet_manager::plugins::register(&plugins, &config.plugins)?
    } else {
        Vec::new()
    };
    if let Command::List { kind, json } = &cli.command
        && kind == "plugins"
    {
        let rows:Vec<_>=registrations.iter().map(|r|json!({"name":r.manifest.name,"protocol":r.manifest.protocol,"kinds":r.manifest.kinds.iter().map(|k|&k.kind).collect::<Vec<_>>(),"available":true})).collect();
        if *json {
            print_json(&json!(rows));
        } else {
            for row in rows {
                println!("{} {}", row["name"], row["kinds"]);
            }
        }
        return Ok(());
    }
    let file_store = FileStore::open(&data_dir)
        .map_err(|e| match e {
            StoreError::Unsupported(why) => refusal_no_retry_fixes(&cli.command, why),
            e => e.into(),
        })?
        .with_plugins(&registrations)?;
    // The ledger sits in the store's directory, which the store must hold
    // first: it refuses a directory that holds anything else.
    if let Command::Integration { command } = &cli.command {
        return cmd_integration(command, &data_dir);
    }
    let store: &dyn Store = &file_store;
    match cli.command {
        Command::Up {
            poll_ms,
            lease_ttl_ms,
        } => {
            let path = config_path.ok_or_else(|| fail("no configuration; run fleet init first"))?;
            cmd_up(
                &file_store,
                (&config, &data_dir),
                &path,
                poll_ms,
                lease_ttl_ms,
                (&plugins, &registrations),
            )
        }
        Command::Init => unreachable_version(),
        Command::Apply {
            file,
            json,
            create_only,
        } => cmd_apply(store, &config, &file, json, create_only),
        Command::Get { kind, name } => {
            let obj = get(store, &served_kind(store, &kind)?, &name)?;
            print_json(&serde_json::to_value(&obj).map_err(|e| fail(e.to_string()))?);
            Ok(())
        }
        Command::List { kind, json } => cmd_list(store, &served_kind(store, &kind)?, json),
        Command::Status { kind, name, json } => {
            let obj = get(store, &served_kind(store, &kind)?, &name)?;
            let summary = status_summary(&obj);
            if json {
                print_json(&summary);
            } else {
                print_status(&summary);
            }
            Ok(())
        }
        Command::Result {
            task,
            generation,
            outcome,
            summary,
            artifacts,
            outputs,
            file,
        } => {
            let result = match file {
                Some(f) => serde_json::from_slice(&read_input(&f)?)
                    .map_err(|e| fail(format!("{}: {e}", f.display())))?,
                None => result_from_flags(generation, outcome, summary, artifacts, outputs)?,
            };
            let outcome = record_result(store, &task, result)?;
            println!(
                "Task/{task} result {}",
                if outcome == fleet_manager::ResultOutcome::Recorded {
                    "recorded"
                } else {
                    "unchanged"
                }
            );
            Ok(())
        }
        Command::Declare {
            task,
            change_request,
            json,
        } => cmd_declare(store, &config, &task, change_request, json),
        Command::Send {
            task,
            text,
            answers,
            json,
        } => {
            let entry = send(store, &SystemClock, &task, &current_user(), &text, &answers)?;
            if json {
                print_json(&serde_json::to_value(&entry).map_err(|e| fail(e.to_string()))?);
            } else {
                println!("Task/{task} inbox {} queued from {}", entry.key, entry.from);
            }
            Ok(())
        }
        Command::Amend {
            task,
            text,
            file,
            reason,
            outputs,
            if_generation,
            json,
        } => {
            let text = match (text, file) {
                (Some(text), _) => text,
                (None, Some(path)) if path.as_os_str() == "-" => {
                    let mut text = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
                        .map_err(|e| fail(format!("stdin: {e}")))?;
                    text
                }
                (None, Some(path)) => std::fs::read_to_string(&path)
                    .map_err(|e| fail(format!("{}: {e}", path.display())))?,
                (None, None) => return Err(fail("give --text or --file")),
            };
            let request = AmendRequest {
                text,
                reason,
                outputs: outputs
                    .iter()
                    .map(|o| parse_output_decl(o))
                    .collect::<Result<_, _>>()?,
                if_generation,
            };
            let done = amend(store, &SystemClock, &task, &current_user(), &request)?;
            if json {
                print_json(&serde_json::to_value(&done).map_err(|e| fail(e.to_string()))?);
            } else {
                println!(
                    "Task/{task} amended: amendment {} appended, generation {} → {}",
                    done.amendment, done.previous_generation, done.generation
                );
            }
            Ok(())
        }
        Command::Label { kind, name, labels } => {
            let kind = served_kind(store, &kind)?;
            if kind != fleet_api::task::KIND {
                return Err(fail(format!(
                    "fleet label sets a Task's labels, not a {kind}'s"
                )));
            }
            label(store, &name, &labels.into_iter().collect())?;
            println!("{kind}/{name} labelled");
            Ok(())
        }
        Command::Delete { kind, name } => {
            let kind = served_kind(store, &kind)?;
            let outcome = delete(store, &kind, &name)?;
            let word = match outcome {
                fleet_api::DeleteOutcome::Marked => {
                    "marked for deletion (its controller releases it)"
                }
                fleet_api::DeleteOutcome::Removed => "deleted",
            };
            println!("{kind}/{name} {word}");
            Ok(())
        }
        Command::Reconcile {
            max_passes,
            holder,
            lease_ttl_ms,
            json,
        } => {
            let signals = open_signals(&data_dir)?;
            let runtime = open_runtime(&config, &signals)?;
            let forge = open_forge(&config)?;
            let gateway = open_gateway(&config)?;
            let bound = runtime
                .operation_bound()
                .max(forge.as_ref().map_or(0, |f| f.operation_bound()))
                .max(gateway.as_ref().map_or(0, |g| g.operation_bound()))
                .max(fleet_api::plugin::Plugins::operation_bound(&plugins));
            let lease_ttl_ms = lease_for(bound, lease_ttl_ms)?;
            if let Some(named) = holder
                .as_deref()
                .filter(|h| fleet_store::holder::is_reserved(h))
            {
                return Err(fail(format!(
                    "--holder {named}: names of the shape fleet-<pid>@<boot id> are Fleet's own"
                )));
            }
            let holder = holder.unwrap_or_else(|| fleet_store::holder::local(false));
            record_children(&data_dir, &holder)?;
            let manager = Manager {
                store,
                runtime: runtime.as_ref(),
                clock: &SystemClock,
                holder,
                forge: forge.as_ref().map(|f| f as &dyn Forge),
                signals: Some(&signals),
                agent_telemetry: agent_collector(),
                change_request_defaults: forge_defaults(&config),
                forge_defaults: relay_defaults(&config),
                plugins: Some((&plugins, &registrations)),
                gateway: gateway_side(&config, gateway.as_ref()),
            };
            let report = manager.run(RunOptions {
                max_passes,
                lease_ttl: lease_ttl_ms,
            })?;
            if json {
                print_json(&serde_json::to_value(&report).map_err(|e| fail(e.to_string()))?);
            } else {
                println!(
                    "reconciled: {} passes, {} status writes, {} effects, {}",
                    report.passes,
                    report.status_writes,
                    report.effects,
                    if report.idle { "idle" } else { "work remains" }
                );
                for u in &report.unknowns {
                    println!("could not tell: {u}");
                }
                for e in &report.errors {
                    println!("error: {e}");
                }
            }
            if report.errors.is_empty() {
                Ok(())
            } else {
                Err(fail(format!(
                    "{} objects failed to reconcile",
                    report.errors.len()
                )))
            }
        }
        Command::Gateway { command } => cmd_gateway(store, &config, command),
        Command::Event { command } => cmd_event(store, command, queued),
        Command::Attach { task, json, wake } => {
            let signals = open_signals(&data_dir)?;
            let runtime = open_runtime(&config, &signals)?;
            cmd_open(store, &task, json, wake, Opens::View, runtime.as_ref())
        }
        Command::Shell { task, json, wake } => {
            let signals = open_signals(&data_dir)?;
            let runtime = open_runtime(&config, &signals)?;
            cmd_open(store, &task, json, wake, Opens::Shell, runtime.as_ref())
        }
        Command::Watch {
            since,
            once,
            poll_ms,
            json,
        } => cmd_watch(store, since, once, poll_ms, json),
        Command::Version { .. }
        | Command::Signal { .. }
        | Command::SignalRead { .. }
        | Command::Runtime { .. }
        | Command::Integration { .. }
        | Command::Doctor { .. }
        | Command::Service { .. }
        | Command::TermBridge { .. } => unreachable_version(),
    }
}

/// The `harnesses` directory beside the config file.
fn harness_dir(config: Option<&Path>) -> Option<PathBuf> {
    config
        .and_then(Path::parent)
        .map(|d| d.join(fleet_api::harness::USER_DIR))
}

/// The user's harness profiles: each `*.toml` in `dir`, in file-name order.
/// A missing directory is none. `strict`: a file that cannot be read or
/// parsed is an error; otherwise it alone is left out, so a hook or a view
/// keeps every other profile.
fn user_profiles(
    dir: Option<&Path>,
    strict: bool,
) -> Result<Vec<fleet_api::harness::Profile>, String> {
    let Some(dir) = dir else {
        return Ok(Vec::new());
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("harness profiles {}: {e}", dir.display())),
    };
    let mut paths = entries
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("harness profiles {}: {e}", dir.display()))?;
    paths.retain(|p| p.extension().is_some_and(|x| x == "toml"));
    paths.sort();
    let mut profiles = Vec::new();
    for p in &paths {
        let parsed = std::fs::read_to_string(p)
            .map_err(|e| e.to_string())
            .and_then(|text| fleet_api::harness::parse(&text));
        match parsed {
            Ok(profile) => profiles.push(profile),
            Err(e) if strict => return Err(format!("harness profile {}: {e}", p.display())),
            Err(_) => {}
        }
    }
    Ok(profiles)
}

/// `fleet signal`: one line appended to the session's signal file. It needs
/// no store, and reads the config dir only for a named harness's user
/// profile, so a hook costs one short process.
fn cmd_signal(
    state: SignalState,
    harness: Option<String>,
    payload: Option<&str>,
    config: Option<PathBuf>,
) -> Result<(), Failure> {
    use fleet_api::harness::{HARNESS_DIR_ENV, HARNESS_ENV, Payload, Profiles};
    use fleet_api::signal::{SIGNAL_FILE_ENV, Signal, State};
    use std::io::{IsTerminal, Read};
    let Some(file) = std::env::var_os(SIGNAL_FILE_ENV).filter(|f| !f.is_empty()) else {
        return Ok(());
    };
    let state = match state {
        SignalState::Working => State::Working,
        SignalState::Blocked => State::Blocked,
        SignalState::Idle => State::Idle,
        SignalState::Prompt => State::Prompt,
    };
    let text = match payload {
        Some(p) => p.to_string(),
        None if !std::io::stdin().is_terminal() => {
            let mut text = String::new();
            std::io::stdin()
                .take(4 * 1024 * 1024)
                .read_to_string(&mut text)
                .map_err(|e| fail(format!("stdin: {e}")))?;
            text
        }
        None => String::new(),
    };
    let payload = serde_json::from_str::<serde_json::Value>(&text).ok();
    let now = fleet_api::store::Clock::now(&SystemClock);
    // The profiles Fleet launched the session with: the directory it names
    // at spawn, else the one beside the config this `fleet` finds. A user
    // profile that cannot be read never fails the agent's turn: the bundled
    // profile, or the default fields, read the payload.
    let fields = harness
        .or_else(|| std::env::var(HARNESS_ENV).ok())
        .filter(|h| !h.is_empty())
        .map_or_else(Payload::default, |h| {
            let dir = std::env::var_os(HARNESS_DIR_ENV)
                .filter(|d| !d.is_empty())
                .map(PathBuf::from)
                .or_else(|| harness_dir(config::discover(config).as_deref()));
            let user = user_profiles(dir.as_deref(), false).unwrap_or_default();
            Profiles::bundled().with(user).get(&h).payload
        });
    let signal = Signal::from_hook_with(now, state, payload.as_ref(), &fields);
    // A session on another host than Fleet's reports to a file under that
    // host's home, `~/…`, whose directory the first report makes.
    let on_place = file.to_string_lossy().starts_with("~/");
    let file = fleet_runtime::signals::expand_home(Path::new(&file));
    if on_place && let Some(dir) = file.parent() {
        fleet_runtime::events::create_private_dir_all(dir)
            .map_err(|e| fail(format!("{}: {e}", dir.display())))?;
    }
    fleet_runtime::signals::append(&file, &signal)
        .map_err(|e| fail(format!("{}: {e}", file.display())))
}

/// `fleet signal-read`: what Fleet reads of a session's signal file on
/// another host, through the runtime's exec there.
fn cmd_signal_read(length: bool, file: &Path, since: Option<u64>) -> Result<(), Failure> {
    use std::io::Write;
    let shown = || file.display().to_string();
    let bytes = if length {
        let n = fleet_runtime::signals::file_length(file)
            .map_err(|e| fail(format!("{}: {e}", shown())))?;
        format!("{n}\n").into_bytes()
    } else {
        match fleet_runtime::signals::read_split(file, since)
            .map_err(|e| fail(format!("{}: {e}", shown())))?
        {
            Some(bytes) => bytes,
            None => {
                return Err(Failure {
                    code: exit::NOT_FOUND,
                    message: format!("{} is not there", shown()),
                    hint: None,
                });
            }
        }
    };
    std::io::stdout()
        .write_all(&bytes)
        .map_err(|e| fail(format!("stdout: {e}")))
}

/// The `fleet gateway` verbs: each reads the `[gateway]` section and calls
/// the manager's verb; the receive verb runs the adapter's pure event
/// reading through the port the section names.
fn cmd_gateway(store: &dyn Store, config: &Config, command: GatewayCommand) -> Result<(), Failure> {
    let Some(section) = &config.gateway else {
        return Err(Failure {
            code: exit::ERROR,
            message: "no [gateway] section is configured".into(),
            hint: Some("add [gateway.accounts] and [gateway.routes] to the config file".into()),
        });
    };
    match command {
        GatewayCommand::Receive {
            account,
            beat: is_beat,
            json,
        } => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|e| fail(format!("stdin: {e}")))?;
            let received = if is_beat {
                beat(store, &SystemClock, section, &account, &text)?
            } else {
                let gateway = open_gateway(config)?.ok_or_else(|| fail("no [gateway] section"))?;
                receive(store, &SystemClock, &gateway, section, &account, &text)?
            };
            if json {
                print_json(&serde_json::to_value(&received).map_err(|e| fail(e.to_string()))?);
            } else {
                println!(
                    "{}",
                    match &received {
                        fleet_manager::Received::Written {
                            conversation,
                            delivery,
                        } => format!("Conversation/{conversation} received {delivery}"),
                        fleet_manager::Received::Duplicate {
                            conversation,
                            delivery,
                        } => format!("Conversation/{conversation} already holds {delivery}"),
                        fleet_manager::Received::Ignored { why } => format!("ignored: {why}"),
                        fleet_manager::Received::Beat { listener, instance } =>
                            format!("Listener/{listener} beat from {instance}"),
                    }
                );
            }
            Ok(())
        }
        GatewayCommand::Account { name, json } => {
            let Some(account) = section.accounts.get(&name) else {
                return Err(fail(format!(
                    "account {name:?} is not configured under [gateway.accounts]"
                )));
            };
            let la = listener_account(&name, account);
            if json {
                print_json(&serde_json::to_value(&la).map_err(|e| fail(e.to_string()))?);
            } else {
                println!(
                    "{} platform {} bot {} inbound {} digest {}",
                    la.name,
                    la.platform,
                    la.bot,
                    la.inbound.name(),
                    la.digest
                );
            }
            Ok(())
        }
        GatewayCommand::Status { json } => {
            let mut rows = Vec::new();
            for name in section.accounts.keys() {
                let listener = fleet_api::listener::listener_name(name);
                let (status, last_delivery) = match store.get(fleet_api::listener::KIND, &listener)
                {
                    Observation::Known(Some(o)) => {
                        let st: fleet_api::listener::ListenerStatus = if o.status.is_null() {
                            Default::default()
                        } else {
                            serde_json::from_value(o.status.clone())
                                .map_err(|e| fail(format!("{listener}: status: {e}")))?
                        };
                        let last = st.last_delivery_at();
                        (Some(st), last)
                    }
                    Observation::Known(None) => (None, None),
                    Observation::Unknown { why } => return Err(StoreError::Unknown(why).into()),
                };
                let cond = |t: &str| -> Value {
                    match status
                        .as_ref()
                        .and_then(|st| fleet_api::condition::condition(&st.conditions, t))
                    {
                        Some(c) => {
                            json!({"status": c.status, "reason": c.reason, "message": c.message})
                        }
                        None => {
                            json!({"status": "Unknown", "reason": "NotObserved", "message": ""})
                        }
                    }
                };
                rows.push(json!({
                    "account": name,
                    "listener": listener,
                    "accepted": cond("Accepted"),
                    "resolved": cond("Resolved"),
                    "inbound": cond("InboundReady"),
                    "lastDeliveryAt": last_delivery,
                }));
            }
            if json {
                print_json(&Value::Array(rows));
            } else {
                for row in rows {
                    let c = |k: &str| {
                        format!(
                            "{} {} {}",
                            row[k]["status"].as_str().unwrap_or("-"),
                            row[k]["reason"].as_str().unwrap_or("-"),
                            row[k]["message"].as_str().unwrap_or("")
                        )
                        .trim_end()
                        .to_string()
                    };
                    println!(
                        "{}  Accepted {}  Resolved {}  InboundReady {}  last delivery {}",
                        row["account"].as_str().unwrap_or("-"),
                        c("accepted"),
                        c("resolved"),
                        c("inbound"),
                        row["lastDeliveryAt"]
                            .as_u64()
                            .map_or("never".to_string(), |t| t.to_string())
                    );
                }
            }
            Ok(())
        }
    }
}

/// The manager's chat side: the port the section opened, the section, and
/// each account's digest.
fn gateway_side<'a>(
    config: &Config,
    gateway: Option<&'a GatewayAccounts>,
) -> Option<GatewaySide<'a>> {
    let (section, port) = (config.gateway.as_ref()?, gateway?);
    let digests = section
        .accounts
        .iter()
        .map(|(name, account)| (name.clone(), account_digest(name, account)))
        .collect();
    Some(GatewaySide {
        port,
        config: section.clone(),
        digests,
        briefs: config.gateway_briefs.clone(),
    })
}

fn cmd_up(
    file_store: &FileStore,
    (config, data_dir): (&Config, &Path),
    config_path: &Path,
    poll_ms: u64,
    lease_ttl_ms: Option<u64>,
    plugins: (
        &dyn fleet_api::plugin::Plugins,
        &[fleet_api::plugin::Registration],
    ),
) -> Result<(), Failure> {
    let lock = file_store.wait_for_manager_lock(|| {
        let holder = manager_holder(data_dir);
        eprintln!("waiting for {holder} to exit");
        supervisor::tell(supervisor::Phase::Waiting(&holder));
    })?;
    let mut record = ManagerRecord {
        pid: std::process::id(),
        boot: fleet_store::holder::boot_id(),
        version: VERSION.into(),
        exe: std::env::current_exe().unwrap_or_default(),
        config: bootstrap::absolute(config_path).map_err(fail)?,
        started_at: fleet_api::Clock::now(&SystemClock),
        state: ManagerState::Starting,
    };
    file_store.record_manager(&lock, &record)?;
    let outcome = serve_up(
        file_store,
        (config, data_dir),
        config_path,
        poll_ms,
        lease_ttl_ms,
        plugins,
        (&lock, &mut record),
    );
    supervisor::tell(supervisor::Phase::Stopping);
    record.state = ManagerState::Stopped;
    report_manager(file_store, &lock, &record);
    outcome
}

fn record_children(data_dir: &Path, holder: &str) -> Result<(), Failure> {
    stop_what_gone_holders_left(data_dir);
    if let Some(dir) = fleet_store::holder::children_dir(data_dir, holder) {
        if let Err(e) = fleet_exec::children::stop_recorded(&dir) {
            eprintln!(
                "could not stop what an earlier process with this PID left in {}: {e}",
                dir.display()
            );
        }
        fleet_exec::children::record_in(&dir)
            .map_err(|e| fail(format!("{}: {e}", dir.display())))?;
    }
    Ok(())
}

fn stop_what_gone_holders_left(data_dir: &Path) {
    let lease = FileStore::peek_lease(data_dir)
        .ok()
        .flatten()
        .map(|l| l.holder);
    for left in fleet_store::holder::left_behind(data_dir, lease.as_deref()) {
        if left.this_boot
            && let Err(e) = fleet_exec::children::stop_recorded(&left.dir)
        {
            eprintln!(
                "could not stop what {} left running: {e}; its lease waits for expiry",
                left.dir.display()
            );
            continue;
        }
        if !left.holds_lease
            && let Err(e) = fleet_store::holder::forget(&left.dir)
        {
            eprintln!("could not remove {}: {e}", left.dir.display());
        }
    }
}

fn manager_holder(data_dir: &Path) -> String {
    let lock = data_dir.join(fleet_store::manager::LOCK);
    let pid = FileStore::peek_manager(data_dir)
        .ok()
        .flatten()
        .map_or(String::new(), |r| format!(" (pid {})", r.pid));
    format!("the manager{pid} that holds {}", lock.display())
}

fn report_manager(store: &FileStore, lock: &ManagerLock, record: &ManagerRecord) {
    if let Err(e) = store.record_manager(lock, record) {
        eprintln!("could not record the manager's state: {e}");
    }
}

fn serve_up(
    file_store: &FileStore,
    (config, data_dir): (&Config, &Path),
    config_path: &Path,
    poll_ms: u64,
    lease_ttl_ms: Option<u64>,
    plugins: (
        &dyn fleet_api::plugin::Plugins,
        &[fleet_api::plugin::Registration],
    ),
    (lock, record): (&ManagerLock, &mut ManagerRecord),
) -> Result<(), Failure> {
    let store: &dyn Store = file_store;
    let signals = open_signals(data_dir)?;
    let runtime = open_runtime(config, &signals)?;
    let forge = open_forge(config)?;
    let gateway = open_gateway(config)?;
    // Every host the config names, or `local` under one `[runtime]`. One
    // that answers is enough to start; each other is reported, and its
    // Tasks wait in their own status.
    let hosts: Vec<String> = if config.runtimes.is_empty() {
        vec![config::LOCAL.to_string()]
    } else {
        config.places().into_iter().map(|p| p.id).collect()
    };
    let mut down = Vec::new();
    for host in &hosts {
        match runtime.host(host) {
            Observation::Known(true) => {}
            Observation::Known(false) => down.push(format!(
                "host {host} is unavailable: no runtime configured for it answers; install and configure its runtime (runtime-mux by default)"
            )),
            Observation::Unknown { why } => down.push(format!(
                "the runtime of host {host} could not be checked: {why}; install and configure it (runtime-mux by default)"
            )),
        }
    }
    if down.len() == hosts.len() {
        return Err(fail(format!("{}; then retry fleet up", down.join("; "))));
    }
    for line in &down {
        eprintln!("{line}");
    }
    let ttl = lease_for(
        runtime
            .operation_bound()
            .max(forge.as_ref().map_or(0, |f| f.operation_bound()))
            .max(gateway.as_ref().map_or(0, |g| g.operation_bound()))
            .max(poll_ms.saturating_mul(2))
            .max(plugins.0.operation_bound()),
        lease_ttl_ms,
    )?;
    let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let signal = stopped.clone();
    ctrlc::set_handler(move || signal.store(true, std::sync::atomic::Ordering::Relaxed))
        .map_err(|e| fail(format!("shutdown handler: {e}")))?;
    let holder = fleet_store::holder::local(true);
    record_children(data_dir, &holder)?;
    let manager = Manager {
        store,
        runtime: runtime.as_ref(),
        clock: &SystemClock,
        holder,
        forge: forge.as_ref().map(|f| f as &dyn Forge),
        signals: Some(&signals),
        agent_telemetry: agent_collector(),
        change_request_defaults: forge_defaults(config),
        forge_defaults: relay_defaults(config),
        plugins: Some(plugins),
        gateway: gateway_side(config, gateway.as_ref()),
    };
    println!(
        "reconciling all Tasks; config {}; Ctrl-C stops Fleet and retains sessions",
        config_path.display()
    );
    let mut last = Vec::new();
    manager.serve(
        RunOptions {
            max_passes: 50,
            lease_ttl: ttl,
        },
        |report| {
            if record.state != ManagerState::Serving {
                record.state = ManagerState::Serving;
                report_manager(file_store, lock, record);
                supervisor::tell(supervisor::Phase::Ready);
            }
            let mut diagnostics: Vec<_> = report
                .errors
                .iter()
                .chain(&report.unknowns)
                .cloned()
                .collect();
            // A Task blocked at startup, or needing attention, is reported
            // here, so no separate status query is needed to see why.
            match store.list("Task") {
                Observation::Known(tasks) => {
                    for task in tasks {
                        let name = &task.metadata.name;
                        for condition in task.status["conditions"].as_array().into_iter().flatten() {
                            let status = condition["status"].as_str().unwrap_or("");
                            let startup = matches!(condition["type"].as_str(), Some("HostReachable" | "Spawned" | "Briefed"));
                            let attention = matches!(condition["type"].as_str(), Some("NeedsAttention" | "Stalled" | "WorkerGone"));
                            if (startup && matches!(status, "False" | "Unknown")) || (attention && status == "True") {
                                diagnostics.push(format!("Task/{name} {}: {} {}. Inspect the session and authenticated harness if readiness or startup is blocked", condition["type"].as_str().unwrap_or(""), condition["reason"].as_str().unwrap_or(""), condition["message"].as_str().unwrap_or("")));
                            }
                        }
                    }
                }
                Observation::Unknown { why } => diagnostics.push(format!("the Tasks are unknown: {why}")),
            }
            if diagnostics != last {
                for line in &diagnostics {
                    eprintln!("could not reconcile: {line}");
                }
                last = diagnostics;
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(poll_ms);
            while !stopped.load(std::sync::atomic::Ordering::Relaxed)
                && std::time::Instant::now() < deadline
                && !manager.reported(report)
            {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            !stopped.load(std::sync::atomic::Ordering::Relaxed)
        },
    )?;
    println!("Fleet stopped; Tasks and sessions retained");
    Ok(())
}

/// Who `fleet send` speaks as: the login name the environment gives, else
/// `unknown`. A name with a path separator would read as another source.
fn current_user() -> String {
    ["USER", "USERNAME", "LOGNAME"]
        .iter()
        .find_map(|v| std::env::var(v).ok())
        .map(|u| u.trim().replace('/', "-"))
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn unreachable_version() -> Result<(), Failure> {
    Err(fail(
        "version, signal and runtime are handled before the store opens",
    ))
}

/// `fleet runtime check`: one line per case, and a failure when any fails.
fn runtime_check(program: &[String], call_timeout_ms: u64) -> Result<(), Failure> {
    let (exe, args) = program
        .split_first()
        .ok_or_else(|| fail("name the runtime program after --"))?;
    let mut rt = ExecProgram::new(exe.clone(), exe.clone());
    rt.args = args.to_vec();
    rt.call_timeout = std::time::Duration::from_millis(call_timeout_ms);
    let cases = fleet_runtime::exec::check(&rt);
    let mut failed = 0;
    for case in &cases {
        match &case.result {
            Ok(()) => println!("ok   {}", case.name),
            Err(why) => {
                failed += 1;
                println!("FAIL {}: {why}", case.name);
            }
        }
    }
    if failed > 0 {
        return Err(fail(format!(
            "{failed} of {} conformance cases failed for {exe}",
            cases.len()
        )));
    }
    if let Some(protocol) = rt.spoken() {
        println!("{exe} speaks runtime protocol {protocol}");
    }
    Ok(())
}

fn version(json: bool, config: Option<PathBuf>, data_dir: Option<PathBuf>) -> Result<(), Failure> {
    if json {
        let explicit = config.is_some() || std::env::var_os(config::CONFIG_ENV).is_some();
        let config = config::discover(config);
        // The user's profiles too, so a view offers their harnesses; one
        // that does not parse is left out here and refused by every
        // command that loads the config.
        let user =
            user_profiles(harness_dir(config.as_deref()).as_deref(), false).unwrap_or_default();
        let profiles = fleet_api::harness::Profiles::bundled().with(user);
        let manager = manager_lease(config.as_deref(), explicit, data_dir.clone());
        let outdated = outdated_integrations(&profiles, config.as_deref(), data_dir);
        print_json(&json!({
            "version": VERSION,
            "cli": {"version": VERSION},
            "apiVersions": [API_VERSION],
            "kinds": fleet_api::kinds::BUILT_IN,
            "configVersion": CONFIG_VERSION,
            "storeFormat": {"format": "fleet-store", "version": 1},
            "runtimes": ["exec", "fake"],
            "harnesses": profiles.names(),
            "outdatedIntegrations": outdated,
            "runtimeProtocol": fleet_runtime::exec::PROTOCOL,
            "forges": ["azure-devops", "bitbucket-cloud", "github", "gitlab", "fake"],
            "gateways": Platform::ALL,
            "manager": manager,
        }));
    } else {
        println!("fleet {VERSION}");
    }
    Ok(())
}

fn cmd_service(
    command: &ServiceCommand,
    config_path: Option<&Path>,
    config: &Config,
    data_dir: &Path,
) -> Result<(), Failure> {
    let stop_timeout = std::time::Duration::from_millis(service_stop_ms(config)?);
    let stop_bound = stop_timeout + std::time::Duration::from_secs(30);
    let target = |name: &Option<String>| service::target(name.as_deref(), data_dir);
    let outcome = match command {
        ServiceCommand::Install { name, now } => {
            let config_path = config_path
                .filter(|p| p.is_file())
                .ok_or_else(|| fail("no config file; run fleet init first"))?;
            let exe = std::env::current_exe().map_err(|e| fail(e.to_string()))?;
            if exe.to_string_lossy().ends_with(" (deleted)") {
                return Err(fail(format!(
                    "{}: this fleet binary was replaced; run the installed one",
                    exe.display()
                )));
            }
            service::install(&service::Install {
                name,
                exe: &exe,
                config: config_path,
                data_dir,
                path_env: std::env::var_os("PATH").unwrap_or_default(),
                stop_timeout,
                now: *now,
            })
            .map(|unit| {
                let verb = if *now {
                    "enabled and started"
                } else {
                    "enabled"
                };
                println!("{} {verb} ({})", unit.name, unit.path.display());
            })
        }
        ServiceCommand::Start { name } => {
            target(name).and_then(|u| service::drive(&u, service::Verb::Start, stop_bound))
        }
        ServiceCommand::Stop { name } => {
            target(name).and_then(|u| service::drive(&u, service::Verb::Stop, stop_bound))
        }
        ServiceCommand::Restart { name } => {
            target(name).and_then(|u| service::drive(&u, service::Verb::Restart, stop_bound))
        }
        ServiceCommand::Status { name } => {
            let unit = target(name).map_err(service_failure)?;
            let (text, code) = service::status(&unit).map_err(service_failure)?;
            print!("{text}");
            return match code {
                0 => Ok(()),
                code => Err(Failure {
                    code: u8::try_from(code).unwrap_or(exit::ERROR),
                    message: format!("{} is not running", unit.name),
                    hint: None,
                }),
            };
        }
        ServiceCommand::Uninstall { name } => target(name).and_then(|u| {
            service::uninstall(&u, stop_bound)
                .map(|()| println!("{} removed ({})", u.name, u.path.display()))
        }),
    };
    outcome.map_err(service_failure)
}

fn service_stop_ms(config: &Config) -> Result<u64, Failure> {
    let runtime = if config.runtimes.is_empty() {
        0
    } else {
        let mut hosts = BTreeMap::new();
        for place in config.places() {
            hosts.insert(place.id, exec_program(config, &place.runtime));
        }
        ExecRuntime::new(hosts).operation_bound()
    };
    let plugins = fleet_plugin::StdioPlugins::new(config.plugins.clone());
    let bound = runtime
        .max(open_forge(config)?.map_or(0, |f| f.operation_bound()))
        .max(open_gateway(config)?.map_or(0, |g| g.operation_bound()))
        .max(fleet_api::plugin::Plugins::operation_bound(&plugins));
    lease_for(bound, None)
}

fn service_failure(refusal: service::Refusal) -> Failure {
    use service::Refusal;
    let (code, message, hint) = match refusal {
        Refusal::Unsupported => (
            exit::ERROR,
            "fleet service drives systemd user units, on Linux only".to_string(),
            Some("run fleet up under this system's own service manager".to_string()),
        ),
        Refusal::Invalid(why) | Refusal::Io(why) => (exit::ERROR, why, None),
        Refusal::Taken(unit) => (
            exit::CONFLICT,
            format!(
                "{} already runs this store ({})",
                unit.name,
                unit.path.display()
            ),
            Some(format!(
                "install with --name {} to rewrite it, or fleet service uninstall first",
                unit.name.trim_end_matches(".service")
            )),
        ),
        Refusal::NotInstalled => (
            exit::NOT_FOUND,
            "no unit runs this store".to_string(),
            Some("fleet service install".to_string()),
        ),
        Refusal::Systemctl(why) => (exit::ERROR, format!("systemctl --user {why}"), None),
        Refusal::StillRunning(why) => (
            exit::TIMEOUT,
            why,
            Some("fleet service status says where it is".to_string()),
        ),
    };
    Failure {
        code,
        message,
        hint,
    }
}

/// The unit that runs `fleet up` for the store, for `version`, or `null`.
fn supervisor(data_dir: &Path) -> Value {
    match service::store_unit(data_dir) {
        Ok(Some(unit)) => json!({"kind": "systemd", "unit": unit.name}),
        _ => Value::Null,
    }
}

/// The manager lease, for `version`: `held` while it is unexpired and its
/// holder is not proven dead, `false` when none is recorded, it expired or
/// its holder is dead, `null` with `why` when it cannot be
/// read. `state`, `version`, `exe` and `config` come from the last
/// manager's record, `null` when none ran; `why` says when it cannot be
/// read. Only read: the lease is never taken or renewed, and no store is
/// created.
fn manager_lease(config: Option<&Path>, explicit: bool, data_dir: Option<PathBuf>) -> Value {
    let unknown = |why: String| {
        json!({"held": null, "holder": null, "expiresAt": null, "why": why,
            "state": null, "version": null, "exe": null, "config": null, "supervisor": null})
    };
    let data_dir = match data_dir {
        Some(dir) => Some(dir),
        None => {
            let loaded = match config {
                Some(p) => match p.try_exists() {
                    Ok(true) => Config::load(p).map(Some),
                    Ok(false) if explicit => Err(format!("config {} does not exist", p.display())),
                    Ok(false) => Ok(None),
                    Err(e) => Err(format!("cannot read config {}: {e}", p.display())),
                },
                None => Ok(None),
            };
            match loaded {
                Ok(c) => c.and_then(|c| c.data.dir).or_else(default_data_dir),
                Err(why) => return unknown(why),
            }
        }
    };
    let Some(data_dir) = data_dir else {
        return unknown(
            "no data directory: pass --data-dir or set data.dir in the config file".into(),
        );
    };
    let mut manager = match FileStore::peek_lease(&data_dir) {
        Ok(None) => json!({"held": false, "holder": null, "expiresAt": null, "why": null}),
        Ok(Some(l)) => json!({
            "held": fleet_api::Clock::now(&SystemClock) < l.expires_at
                && !fleet_store::holder::is_takeable(&l.holder, &data_dir),
            "holder": l.holder,
            "expiresAt": l.expires_at,
            "why": null,
        }),
        Err(e) => return unknown(e.to_string()),
    };
    let record = FileStore::peek_manager(&data_dir);
    let known = record.as_ref().ok().cloned().flatten();
    manager["state"] = json!(known.as_ref().map(|r| r.state));
    manager["version"] = json!(known.as_ref().map(|r| &r.version));
    manager["exe"] = json!(known.as_ref().map(|r| &r.exe));
    manager["config"] = json!(known.as_ref().map(|r| &r.config));
    manager["supervisor"] = supervisor(&data_dir);
    if let Err(e) = record {
        manager["why"] = json!(e.to_string());
    }
    manager
}

/// The integrations installed on this machine that are outdated, by agent:
/// what `fleet integration install <agent>` brings up to date. A config or
/// a ledger that cannot be read lists none: a version never fails.
fn outdated_integrations(
    profiles: &fleet_api::harness::Profiles,
    config: Option<&Path>,
    data_dir: Option<PathBuf>,
) -> Vec<String> {
    let data_dir = data_dir
        .or_else(|| {
            config
                .and_then(|p| Config::load(p).ok())
                .and_then(|c| c.data.dir)
        })
        .or_else(default_data_dir);
    let Some(data_dir) = data_dir else {
        return Vec::new();
    };
    let env = |v: &str| std::env::var(v).ok();
    fleet_runtime::integration::installed(&data_dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|name| {
            fleet_runtime::integration::status(&profiles.get(name), &data_dir, &env)
                .is_ok_and(|r| r.state == fleet_api::integration::State::Outdated)
        })
        .collect()
}

/// `fleet integration`: each verb on this machine, with the profiles the
/// config installed and the ledger under the data directory.
fn cmd_integration(command: &IntegrationCommand, data_dir: &Path) -> Result<(), Failure> {
    use fleet_runtime::integration as io;
    let profiles = fleet_api::harness::installed();
    let env = |v: &str| std::env::var(v).ok();
    let known = |agent: &str| {
        if profiles.names().iter().any(|n| n == agent) {
            Ok(profiles.get(agent))
        } else {
            Err(fail(format!(
                "no harness profile names {agent:?}; `fleet version --json` lists them"
            )))
        }
    };
    match command {
        IntegrationCommand::Status { agent, json, .. } => {
            let chosen = match agent {
                Some(a) => vec![known(a)?],
                None => profiles.list().to_vec(),
            };
            let mut rows = Vec::new();
            for p in &chosen {
                let report = io::status(p, data_dir, &env).map_err(|e| fail(e.to_string()))?;
                rows.push(json!({
                    "agent": p.name,
                    "state": report.state.as_str(),
                    "dir": report.dir.map(|d| d.to_string_lossy().into_owned()),
                    "installed": report.installed,
                    "version": p.install.as_ref().map(|i| i.version),
                }));
            }
            if *json {
                print_json(&json!(rows));
            } else {
                for r in &rows {
                    let detail = match r["state"].as_str() {
                        Some("not-needed") => "launch-time hooks".to_string(),
                        Some("unsupported") => "no hooks Fleet knows".to_string(),
                        _ => format!(
                            "{} (version {})",
                            r["dir"].as_str().unwrap_or("no config directory"),
                            r["version"]
                        ),
                    };
                    println!(
                        "{:<12} {:<14} {detail}",
                        r["agent"].as_str().unwrap_or_default(),
                        r["state"].as_str().unwrap_or_default()
                    );
                }
            }
            Ok(())
        }
        IntegrationCommand::Install { agent } => {
            let p = known(agent)?;
            match io::install(&p, data_dir, &env).map_err(fail)? {
                io::Installed::Written => println!("{} installed", p.name),
                io::Installed::Unchanged => println!("{} current", p.name),
            }
            Ok(())
        }
        IntegrationCommand::Uninstall { agent, all } => {
            let installed = io::installed(data_dir).map_err(|e| fail(e.to_string()))?;
            let agent = agent.as_deref().unwrap_or_default();
            // The ledger's own name first: an integration outlives a user
            // profile removed since.
            let names = if *all {
                installed
            } else if installed.iter().any(|n| n == agent) {
                vec![agent.to_string()]
            } else {
                vec![known(agent)?.name]
            };
            for name in names {
                if io::uninstall(&name, data_dir).map_err(fail)? {
                    println!("{name} uninstalled");
                } else {
                    println!("{name} not installed");
                }
            }
            Ok(())
        }
    }
}

/// The signal files, one per session on this machine, under the store, and
/// beside them the plugin files the harness profiles name: every command
/// that starts a session opens the signals first.
fn open_signals(data_dir: &Path) -> Result<FileSignals, Failure> {
    let assets = data_dir.join(fleet_api::harness::ASSET_DIR);
    // A profile names a plugin file inside a JSON string, as opencode's
    // config does, where a quote or a control character cannot stand as
    // it is.
    if assets
        .to_string_lossy()
        .chars()
        .any(|c| c == '"' || c.is_control())
    {
        return Err(fail(format!(
            "{}: a data directory whose path holds a double quote or a control character cannot be named in a harness's config; move it",
            assets.display()
        )));
    }
    fleet_runtime::assets::write(&assets)
        .map_err(|e| fail(format!("{}: {e}", assets.display())))?;
    let dir = data_dir.join("signals");
    FileSignals::open(&dir).map_err(|e| fail(format!("{}: {e}", dir.display())))
}

/// The runtime: the programs under `[runtimes]`, one per place, or the
/// fake. `local` is the default runtime's `local` place under a second name.
fn open_runtime(config: &Config, signals: &FileSignals) -> Result<Box<dyn Runtime>, Failure> {
    if !config.runtimes.is_empty() {
        let mut hosts = BTreeMap::new();
        for place in config.places() {
            let mut rt = exec_program(config, &place.runtime);
            // Its manifest asks about its own place only: a Task on one
            // place never waits on another's probe.
            rt.places = vec![place.place.clone()];
            rt.place = place.place;
            if place.local {
                hosts.insert(config::LOCAL.to_string(), rt.clone());
            }
            hosts.insert(place.id, rt);
        }
        let runtime = ExecRuntime::new(hosts).with_signals(signals.clone());
        return Ok(Box::new(runtime));
    }
    let Some(rt) = &config.runtime else {
        return Err(Failure {
            code: exit::ERROR,
            message: "no runtime configured".into(),
            hint: Some(
                "add [runtimes.mux] program = \"runtime-mux\" to the config file, or run fleet init"
                    .into(),
            ),
        });
    };
    match rt.kind.as_str() {
        "fake" => {
            let dir = rt
                .dir
                .clone()
                .ok_or_else(|| fail("runtime.dir is required for the fake runtime"))?;
            let host = FakeHost::open(&dir)
                .map_err(|e| fail(format!("fake host {}: {e}", dir.display())))?;
            Ok(Box::new(FakeRuntime::new(host)))
        }
        other => Err(fail(format!(
            "unknown runtime kind {other:?}; [runtime] is the fake, and a real runtime is a program under [runtimes]"
        ))),
    }
}

fn exec_program(config: &Config, name: &str) -> ExecProgram {
    let p = &config.runtimes[name];
    let program = std::env::current_exe()
        .ok()
        .and_then(|exe| fleet_runtime::program::beside(&p.program, &exe))
        .map_or_else(|| p.program.clone(), |f| f.to_string_lossy().into_owned());
    let mut rt = ExecProgram::new(name.to_string(), program);
    rt.args = p.args.clone();
    rt.call_timeout =
        std::time::Duration::from_millis(p.call_timeout_ms.unwrap_or(DEFAULT_CALL_TIMEOUT_MS));
    if let Some(ms) = p.workspace_timeout_ms {
        rt.workspace_timeout = std::time::Duration::from_millis(ms);
    }
    if let Some(prefix) = &p.key_prefix {
        rt.key_prefix = prefix.clone();
    }
    rt.places = config
        .places()
        .into_iter()
        .filter(|place| place.runtime == name)
        .map(|place| place.place)
        .collect();
    rt
}

/// The program of the place `id` names (`local`, or `<runtime>/<place>`),
/// its requests naming that place: how `fleet runtime browse` and
/// `branches` reach it. A place the config does not define is refused by
/// name.
fn place_program(config: &Config, id: &str) -> Result<ExecProgram, Failure> {
    if config.runtimes.is_empty() {
        return Err(fail(
            "the fake [runtime] has no places to read; a place belongs to a runtime under [runtimes]",
        ));
    }
    let places = config.places();
    let found = places.iter().find(|p| {
        if id == config::LOCAL {
            p.local
        } else {
            p.id == id
        }
    });
    let Some(found) = found else {
        if id == config::LOCAL
            && let Some(why) = config.no_local()
        {
            return Err(fail(why));
        }
        let known: Vec<&str> = places.iter().map(|p| p.id.as_str()).collect();
        return Err(fail(format!(
            "no place {id}; the places are {}",
            known.join(", ")
        )));
    };
    let mut program = exec_program(config, &found.runtime);
    program.place = found.place.clone();
    Ok(program)
}

/// `fleet runtime places`: each place, with its runtime's manifest answer.
/// A runtime that does not answer leaves each of its places unknown, and one
/// of protocol 1 serves only `local`.
fn runtime_places(config: &Config) -> Vec<Value> {
    let places = config.places();
    let mut rows = Vec::new();
    for runtime in config.runtimes.keys() {
        let program = exec_program(config, runtime);
        let manifest = program.manifest();
        for place in places.iter().filter(|p| &p.runtime == runtime) {
            let r = fleet_runtime::exec::readiness(&program, &place.place, &manifest);
            let row = json!({
                "place": place.id,
                "runtime": runtime,
                "name": place.place.name,
                "alias": place.local.then_some(config::LOCAL),
                "platform": r.report.as_ref().map(|r| r.platform),
                "dynamic": r.report.as_ref().map(|r| r.dynamic),
                "ready": r.ready,
                "why": r.why,
            });
            rows.push(row);
        }
    }
    rows
}

/// The collector Fleet exports to, for the agents it starts to export to
/// as well (docs/observability.md, *Coding agents*).
fn agent_collector() -> Option<fleet_api::harness::Collector> {
    otel_env::collector().map(|c| fleet_api::harness::Collector {
        endpoint: c.endpoint,
        protocol: c.protocol.to_string(),
        headers: c.headers,
    })
}

fn shared_board(store: &dyn Store) -> Result<(), Failure> {
    use fleet_api::board::{BoardSpec, KIND, SHARED};
    let obj: Object = serde_json::from_value(json!({
        "apiVersion": fleet_api::API_VERSION,
        "kind": KIND,
        "metadata": {"name": SHARED},
        "spec": BoardSpec::default(),
    }))
    .map_err(|e| fail(e.to_string()))?;
    match fleet_manager::verbs::apply(store, obj, &BTreeSet::new(), true) {
        Ok(_) | Err(StoreError::AlreadyExists(_)) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn publish_of(
    event_type: &str,
    subject: &Option<String>,
    key: &Option<String>,
    correlation: &Option<String>,
    data: &str,
) -> Result<fleet_api::event::Publish, Failure> {
    let data = serde_json::from_str(data).map_err(|e| fail(format!("--data: not JSON: {e}")))?;
    Ok(fleet_api::event::Publish {
        event_type: event_type.to_string(),
        subject: subject.clone(),
        key: key.clone(),
        correlation: correlation.clone(),
        data,
    })
}

/// The outbox of the Fleet session this runs in; `None` outside one. A
/// session Fleet gave a signal file but no outbox cannot publish: Fleet
/// would never read what it wrote, and publishing as the user instead
/// would name the wrong source.
fn session_outbox() -> Result<Option<PathBuf>, Failure> {
    use fleet_api::event::EVENT_FILE_ENV;
    use fleet_api::signal::SIGNAL_FILE_ENV;
    let set = |var: &str| std::env::var_os(var).filter(|v| !v.is_empty());
    match (set(EVENT_FILE_ENV), set(SIGNAL_FILE_ENV)) {
        (Some(outbox), _) => Ok(Some(PathBuf::from(outbox))),
        (None, Some(_)) => Err(Failure {
            code: exit::ERROR,
            message: format!(
                "this Fleet session has {SIGNAL_FILE_ENV} but no {EVENT_FILE_ENV}: Fleet reads no events from it"
            ),
            hint: Some("restart the session under a Fleet that passes FLEET_EVENT_FILE".into()),
        }),
        (None, None) => Ok(None),
    }
}

fn outbox_session_key(outbox: &Path) -> Result<String, Failure> {
    fleet_runtime::events::outbox_session_key(outbox).ok_or_else(|| Failure {
        code: exit::ERROR,
        message: format!(
            "{}: {} names no outbox Fleet made, so --wait cannot tell which Task reads it",
            outbox.display(),
            fleet_api::event::EVENT_FILE_ENV
        ),
        hint: Some("publish without --wait".into()),
    })
}

fn session_append(
    outbox: &Path,
    line: &fleet_api::event::OutboxLine,
) -> Result<(u64, u64), Failure> {
    let length = line.encode().map_err(fail)?.len() as u64;
    let offset = fleet_runtime::events::append(outbox, line)
        .map_err(|e| fail(format!("{}: {e}", outbox.display())))?;
    Ok((offset, offset + length))
}

fn print_queued(board: &str, event_type: &str, offset: u64, json: bool) {
    if json {
        print_json(&json!({"board": board, "type": event_type, "offset": offset}));
    } else {
        println!("{board} {event_type} queued at outbox offset {offset}");
    }
}

struct Queued {
    session_key: String,
    outbox: PathBuf,
    line: fleet_api::event::OutboxLine,
    timeout_ms: u64,
    json: bool,
}

const PUBLISH_WAIT_POLL_MS: u64 = 200;

fn sleep_ms(ms: u64) {
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

fn warn_and_retry(e: &StoreError) {
    eprintln!("warning: could not read the store; reading again: {e}");
}

fn with_poll<T>(every_ms: u64, f: impl FnOnce(&mut fleet_manager::verbs::Poll<'_>) -> T) -> T {
    let mut pause = sleep_ms;
    let mut warn = warn_and_retry;
    f(&mut fleet_manager::verbs::Poll {
        every_ms,
        pause: &mut pause,
        warn: &mut warn,
    })
}

fn cmd_publish_wait(store: &dyn Store, queued: Queued) -> Result<(), Failure> {
    use fleet_manager::verbs::{Ingest, before_append, wait_ingested};
    let before = before_append(store, &queued.session_key, &queued.line.board)?;
    let (offset, end) = session_append(&queued.outbox, &queued.line)?;
    let line = before.queued(offset, end, &queued.line.board, queued.line.key.clone());
    let deadline = fleet_api::Clock::now(&SystemClock) + queued.timeout_ms;
    let ingest = with_poll(PUBLISH_WAIT_POLL_MS, |poll| {
        wait_ingested(store, &SystemClock, &line, deadline, poll)
    })?;
    match ingest {
        Ingest::Appended { event, duplicate } => print_published(&event, duplicate, queued.json),
        Ingest::Skipped(why) => Err(fail(format!(
            "the line at outbox offset {offset} was skipped or is not on the Board: {why}"
        ))),
        Ingest::Pending => {
            print_queued(&line.board, &queued.line.event_type, offset, queued.json);
            Err(Failure {
                code: exit::TIMEOUT,
                message: format!(
                    "Fleet has not read the line at outbox offset {offset} in within {} ms",
                    queued.timeout_ms
                ),
                hint: Some("Fleet may still read it in, or the outbox may have been made again; check that fleet up runs".into()),
            })
        }
    }
}

fn print_published(
    e: &fleet_api::event::Event,
    duplicate: bool,
    json: bool,
) -> Result<(), Failure> {
    if json {
        print_json(&serde_json::to_value(e).map_err(|e| fail(e.to_string()))?);
    } else {
        println!(
            "{}/{} {} from {}{}",
            e.board,
            e.seq,
            e.event_type,
            e.source.name(),
            if duplicate {
                " (already published under this key)"
            } else {
                ""
            }
        );
    }
    Ok(())
}

fn cmd_outbox_read(file: &Path, from: u64) -> Result<(), Failure> {
    use std::io::Write;
    let read = fleet_runtime::events::read_from(file, from)
        .map_err(|e| fail(format!("{}: {e}", file.display())))?
        .ok_or_else(|| Failure {
            code: exit::NOT_FOUND,
            message: format!("{} is not there", file.display()),
            hint: None,
        })?;
    std::io::stdout()
        .write_all(&fleet_runtime::events::encode_place_read(&read))
        .map_err(|e| fail(format!("stdout: {e}")))
}

fn cmd_event(
    store: &dyn Store,
    command: EventCommand,
    queued: Option<Queued>,
) -> Result<(), Failure> {
    match command {
        EventCommand::Publish { .. } if queued.is_some() => {
            cmd_publish_wait(store, queued.expect("queued"))
        }
        EventCommand::Publish {
            board,
            event_type,
            subject,
            key,
            correlation,
            data,
            json,
            ..
        } => {
            let publish = publish_of(&event_type, &subject, &key, &correlation, &data)?;
            let appended = publish_event(store, &board, &current_user(), publish)?;
            print_published(&appended.event, appended.duplicate, json)
        }
        EventCommand::List {
            after,
            filter,
            limit,
            json,
        } => {
            let cursor = filter.cursor(after);
            let page = list_events(store, &cursor.board, after, &cursor.filter, limit)?;
            print_page(&cursor.board, after, page, json)
        }
        EventCommand::Wait {
            after,
            filter,
            timeout_ms,
            poll_ms,
            limit,
            json,
        } => {
            let mut cursor = filter.cursor(after);
            let deadline = fleet_api::Clock::now(&SystemClock) + timeout_ms;
            let page = with_poll(poll_ms, |poll| {
                fleet_manager::verbs::wait_events(
                    store,
                    &SystemClock,
                    &mut cursor,
                    limit,
                    deadline,
                    poll,
                )
            })?;
            match page {
                Some(page) => print_page(&cursor.board, cursor.after, page, json),
                None => Err(Failure {
                    code: exit::TIMEOUT,
                    message: format!(
                        "Board/{}: no event after {after} matched within {timeout_ms} ms",
                        cursor.board
                    ),
                    hint: None,
                }),
            }
        }
        EventCommand::Watch {
            after,
            filter,
            poll_ms,
            json,
        } => cmd_event_watch(store, filter.cursor(after), poll_ms, json),
        EventCommand::Subscribe {
            task,
            filter,
            after,
        } => {
            let (board, filter) = filter.split();
            let subscription = fleet_api::subscription::Subscription {
                board: board.clone(),
                filter,
                after,
            };
            fleet_manager::verbs::subscribe(store, &task, subscription)?;
            println!("Task/{task} subscribes to Board/{board}");
            Ok(())
        }
        EventCommand::Unsubscribe { task, filter } => {
            let (board, filter) = filter.split();
            fleet_manager::verbs::unsubscribe(store, &task, &board, &filter)?;
            println!("Task/{task} no longer subscribes to Board/{board} with this filter");
            Ok(())
        }
    }
}

fn expired_failure(board: &str, after: u64, oldest: u64) -> Failure {
    Failure {
        code: exit::CONFLICT,
        message: format!(
            "Board/{board}: the events after {after} are gone; the oldest {oldest} is the first it holds"
        ),
        hint: Some(format!(
            "read again with --after {} and accept the gap",
            oldest - 1
        )),
    }
}

fn print_page(
    board: &str,
    after: u64,
    page: fleet_api::event::Page,
    json: bool,
) -> Result<(), Failure> {
    use fleet_api::event::Page;
    match page {
        Page::Expired { oldest } => {
            if json {
                print_json(&json!({"expired": {"oldest": oldest}}));
            }
            Err(expired_failure(board, after, oldest))
        }
        Page::Events(events) if json => {
            print_json(&serde_json::to_value(&events).map_err(|e| fail(e.to_string()))?);
            Ok(())
        }
        Page::Events(events) => {
            println!(
                "{:<8} {:<14} {:<32} {:<24} SUBJECT",
                "SEQ", "TIME", "TYPE", "SOURCE"
            );
            for e in &events {
                println!("{}", event_row(e));
            }
            Ok(())
        }
    }
}

fn event_row(e: &fleet_api::event::Event) -> String {
    format!(
        "{:<8} {:<14} {:<32} {:<24} {}",
        e.seq,
        e.time,
        e.event_type,
        e.source.name(),
        e.subject.as_deref().unwrap_or("-")
    )
}

fn cmd_event_watch(
    store: &dyn Store,
    mut cursor: fleet_manager::verbs::EventCursor,
    poll_ms: u64,
    json: bool,
) -> Result<(), Failure> {
    use fleet_api::event::Page;
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    loop {
        let from = cursor.after;
        let events = match cursor.next(store, fleet_manager::verbs::MAX_EVENT_LIST) {
            Ok(Page::Events(events)) => events,
            Ok(Page::Expired { oldest }) => {
                if json {
                    let _ = writeln!(out, "{}", json!({"expired": {"oldest": oldest}}));
                }
                return Err(expired_failure(&cursor.board, cursor.after, oldest));
            }
            Err(e) if fleet_manager::verbs::is_transient(&e) => {
                warn_and_retry(&e);
                Vec::new()
            }
            Err(e) => return Err(e.into()),
        };
        if events.is_empty() && cursor.after == from {
            sleep_ms(poll_ms);
            continue;
        }
        for e in &events {
            let line = if json {
                serde_json::to_string(e).map_err(|e| fail(e.to_string()))?
            } else {
                event_row(e)
            };
            if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
                return Ok(());
            }
        }
    }
}

/// Write the TaskConfig a Task with no `configRef` takes, when the store
/// holds none: an idle session pauses after 30 minutes. One that is there is
/// the user's, and stays as it is.
fn default_task_config(store: &dyn Store) -> Result<(), Failure> {
    use fleet_api::config::{DEFAULT_PAUSE_AFTER_MS, DEFAULT_TASK_CONFIG, TASK_CONFIG};
    let obj: Object = serde_json::from_value(json!({
        "apiVersion": "fleet/v1alpha1",
        "kind": TASK_CONFIG,
        "metadata": {"name": DEFAULT_TASK_CONFIG},
        "spec": {"pauseAfterMs": DEFAULT_PAUSE_AFTER_MS},
    }))
    .map_err(|e| fail(e.to_string()))?;
    match fleet_manager::verbs::apply(store, obj, &BTreeSet::new(), true) {
        Ok(_) | Err(StoreError::AlreadyExists(_)) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// What `fleet init` set up and what comes next, in a few lines: the
/// config, the places sessions run in, the coding agents on PATH, and the
/// commands that follow. A first-time user reads this, so it names the
/// next command rather than the concepts. `named`: a config chosen with
/// `--config` or `FLEET_CONFIG`, which every printed command then carries.
fn init_summary(
    written: bool,
    path: &Path,
    config: &Config,
    profiles: &fleet_api::harness::Profiles,
    named: Option<&Path>,
) -> String {
    let mut out = format!(
        "{} {}\n",
        if written { "initialized" } else { "kept" },
        path.display()
    );
    let places: Vec<String> = config.places().into_iter().map(|p| p.id).collect();
    if !places.is_empty() {
        out.push_str(&format!("  sessions run in: {}\n", places.join(", ")));
    }
    let path_var = std::env::var_os("PATH");
    let mut agents: Vec<String> = Vec::new();
    for p in profiles.list() {
        let found = fleet_runtime::program::find(&p.program, path_var.as_deref()).is_some();
        if found && !agents.contains(&p.program) {
            agents.push(p.program.clone());
        }
    }
    if agents.is_empty() {
        out.push_str(
            "  no coding agent found on PATH: install one (claude, codex, opencode, ...)\n",
        );
    } else {
        out.push_str(&format!("  coding agents found: {}\n", agents.join(", ")));
    }
    // Fleet runs without thurbox, so both ways to start are named.
    let fleet = match named {
        Some(p) => format!("fleet --config {}", p.display()),
        None => "fleet".to_string(),
    };
    out.push_str("next: run `thurbox` and press ctrl+n to start a session,\n");
    if let Some(p) = named {
        out.push_str(&format!(
            "      once settings.toml names this config: [fleets.<name>] config = \"{}\"\n",
            p.display()
        ));
    }
    out.push_str(&format!(
        "      or run `{fleet} up` and `{fleet} apply` without it\n"
    ));
    out.push_str(&format!("      `{fleet} doctor` checks this machine\n"));
    out
}

/// `fleet doctor`: each check's finding, classified by
/// `fleet_api::doctor`, and a failure when any check failed.
fn cmd_doctor(
    config: &Config,
    profiles: &fleet_api::harness::Profiles,
    data_dir: &Path,
    json: bool,
) -> Result<(), Failure> {
    use fleet_api::doctor;
    let env = |v: &str| std::env::var(v).ok();
    let mut checks = Vec::new();
    for p in profiles.list().iter().filter(|p| p.install.is_some()) {
        checks.push(
            match fleet_runtime::integration::status(p, data_dir, &env) {
                Ok(report) => doctor::integration(&p.name, report.state),
                Err(e) => doctor::integration_unknown(&p.name, &e.to_string()),
            },
        );
    }
    match fleet_runtime::integration::read_ledger(data_dir) {
        Ok(ledger) => checks.extend(doctor::left_behind(&ledger, profiles)),
        // Each profile with an `[install]` table already failed on it above.
        Err(e) if !profiles.list().iter().any(|p| p.install.is_some()) => {
            checks.push(doctor::integration_unknown(
                fleet_api::integration::LEDGER,
                &e.to_string(),
            ));
        }
        Err(_) => {}
    }
    let path = std::env::var_os("PATH");
    for p in profiles.list() {
        let found = fleet_runtime::program::find(&p.program, path.as_deref());
        let found = found.as_ref().map(|f| f.to_string_lossy());
        checks.push(doctor::harness(&p.name, &p.program, found.as_deref()));
    }
    if !config.runtimes.is_empty() {
        if let Some(why) = config.no_local() {
            checks.push(doctor::no_local_place(&why));
        }
        for place in config.places() {
            let mut program = exec_program(config, &place.runtime);
            program.places = vec![place.place.clone()];
            program.place = place.place;
            let answer = fleet_runtime::exec::reach(&program);
            checks.push(doctor::runtime(&place.id, &place.runtime, answer));
        }
    } else if let Some(rt) = &config.runtime {
        checks.push(doctor::fake_runtime(&rt.kind, rt.dir.is_some()));
    } else {
        checks.push(doctor::no_runtime());
    }
    checks.push(doctor::telemetry(otel_env::describe(
        &config.telemetry.clone().unwrap_or_default(),
        false,
    )));
    let failed = doctor::failed(&checks);
    if json {
        print_json(&json!({ "checks": checks, "failed": failed }));
    } else {
        for c in &checks {
            let hint = c
                .hint
                .as_deref()
                .map(|h| format!(": {h}"))
                .unwrap_or_default();
            println!(
                "{:<5} {:<12} {:<12} {}{hint}",
                c.level.as_str(),
                c.area,
                c.name,
                c.detail
            );
        }
    }
    if failed > 0 {
        return Err(fail(format!("{failed} of {} checks failed", checks.len())));
    }
    Ok(())
}

/// The configured forges, one adapter per `[forge.hosts]` entry, or `None`
/// when the config has no `[forge]` section.
fn open_forge(config: &Config) -> Result<Option<ForgeHosts>, Failure> {
    let Some(cfg) = &config.forge else {
        return Ok(None);
    };
    let mut hosts = std::collections::BTreeMap::new();
    for (host, h) in &cfg.hosts {
        let kind = HostKind::parse(&h.kind).ok_or_else(|| {
            fail(format!(
                "forge.hosts.{host:?}: unknown kind {:?}; this build has azure-devops, bitbucket-cloud, github, gitlab and fake",
                h.kind
            ))
        })?;
        hosts.insert(
            host.clone(),
            HostConfig {
                kind,
                cli: h.cli.clone().map(Into::into),
                dir: h.dir.clone(),
            },
        );
    }
    let timeout =
        std::time::Duration::from_millis(cfg.call_timeout_ms.unwrap_or(DEFAULT_CALL_TIMEOUT_MS));
    ForgeHosts::new(hosts, timeout).map(Some).map_err(fail)
}

/// The configured chat accounts, one adapter per `[gateway.accounts]`
/// entry, or `None` when the config has no `[gateway]` section. The section's
/// own rules ran at load; this is where a platform this build lacks, an
/// `inbound` its listener cannot deliver by, a fake without a dir, or a
/// `gateway.logs` channel id the account's adapter cannot read is refused.
fn open_gateway(config: &Config) -> Result<Option<GatewayAccounts>, Failure> {
    let Some(cfg) = &config.gateway else {
        return Ok(None);
    };
    let timeout = std::time::Duration::from_millis(cfg.defaults().call_timeout_ms);
    let accounts = GatewayAccounts::new(cfg.accounts.clone(), timeout).map_err(fail)?;
    if let Some(logs) = &cfg.logs {
        accounts
            .check_channel(&logs.account, &logs.channel)
            .map_err(|e| fail(format!("gateway.logs: channel {:?}: {e}", logs.channel)))?;
    }
    Ok(Some(accounts))
}

/// The retention defaults a ChangeRequest spec may override.
fn forge_defaults(config: &Config) -> ChangeRequestDefaults {
    let base = ChangeRequestDefaults::default();
    match &config.forge {
        Some(f) => ChangeRequestDefaults {
            closed_grace_ms: f.closed_grace_ms.unwrap_or(base.closed_grace_ms),
            settled_ttl_ms: f.settled_ttl_ms.unwrap_or(base.settled_ttl_ms),
        },
        None => base,
    }
}

/// The relay and draft defaults a Task's `spec.forge` may override, and the
/// ready policies it may select.
fn relay_defaults(config: &Config) -> ForgeDefaults {
    let base = ForgeDefaults::default();
    match &config.forge {
        Some(f) => ForgeDefaults {
            relay_notes: f.relay_notes.unwrap_or(base.relay_notes),
            relay_checks: f.relay_checks.unwrap_or(base.relay_checks),
            relay_bots: f.relay_bots.unwrap_or(base.relay_bots),
            no_check_grace_ms: f.no_check_grace_ms.unwrap_or(base.no_check_grace_ms),
            draft_policy: f.draft_policy.clone(),
            ready_policies: f.ready_policies.clone(),
        },
        None => base,
    }
}

/// The names a Task's `spec.forge.draft.policy` may select.
fn policy_names(config: &Config) -> std::collections::BTreeSet<String> {
    config
        .forge
        .as_ref()
        .map(|f| f.ready_policies.keys().cloned().collect())
        .unwrap_or_default()
}

/// `fleet declare`: one of the two forms, through the manager's verbs.
fn cmd_declare(
    store: &dyn Store,
    config: &Config,
    task: &str,
    change_request: Option<String>,
    json: Option<String>,
) -> Result<(), Failure> {
    match (change_request, json) {
        (Some(url), None) => {
            let forge = open_forge(config)?.ok_or_else(|| Failure {
                code: exit::ERROR,
                message: "a change needs a [forge] section naming its host".into(),
                hint: Some("add [forge.hosts] with the change's host to the config file".into()),
            })?;
            let declared = declare_change_request(store, &forge, task, &url)?;
            println!(
                "Task/{task} declared ChangeRequest/{} ({}; {})",
                declared.change_request,
                if declared.created {
                    "created"
                } else {
                    "existing"
                },
                if declared.outcome == DeclareOutcome::Declared {
                    "declared"
                } else {
                    "already declared"
                }
            );
            Ok(())
        }
        (None, Some(text)) => {
            let item: Value = serde_json::from_str(&text)
                .map_err(|e| fail(format!("--json: not a JSON object: {e}")))?;
            let kind = item
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            let outcome = declare(store, task, item)?;
            println!(
                "Task/{task} declared {kind} ({})",
                if outcome == DeclareOutcome::Declared {
                    "declared"
                } else {
                    "already declared"
                }
            );
            Ok(())
        }
        _ => Err(fail(
            "declare takes exactly one of --change-request URL or --json OBJECT",
        )),
    }
}

/// The lease a run holds: the one asked for, refused when it cannot outlast
/// the longest operation, else the default, raised to twice that operation.
/// The lease is renewed before every probe and every effect, so one runtime
/// operation, bounds and grace included, must fit inside it.
fn lease_for(bound: u64, asked: Option<u64>) -> Result<u64, Failure> {
    match asked {
        Some(ms) if ms <= bound => Err(Failure {
            code: exit::ERROR,
            message: format!(
                "the manager lease ({ms} ms) cannot outlast one runtime operation ({bound} ms): it would expire mid-operation"
            ),
            hint: Some("raise --lease-ttl-ms or lower runtime.callTimeoutMs".into()),
        }),
        Some(ms) => Ok(ms),
        None => Ok(DEFAULT_LEASE_MS.max(bound.saturating_mul(2))),
    }
}

const DEFAULT_LEASE_MS: u64 = 30_000;

/// Runtime program and forge CLI calls are bounded at half the default lease.
const DEFAULT_CALL_TIMEOUT_MS: u64 = 15_000;

/// `task`, `tasks` and `Task` all name Task. Unsupported kinds are refused
/// here, as on write, never reported as unreadable.
fn served_kind(store: &dyn Store, kind: &str) -> Result<String, Failure> {
    let lower = kind.to_ascii_lowercase();
    let singular = lower.strip_suffix('s').unwrap_or(&lower);
    let kind = match singular {
        "task" => fleet_api::task::KIND.to_string(),
        "taskconfig" => fleet_api::config::TASK_CONFIG.to_string(),
        "envprovider" => fleet_api::config::ENV_PROVIDER.to_string(),
        "changerequest" => fleet_api::change_request::KIND.to_string(),
        "conversation" => fleet_api::conversation::KIND.to_string(),
        "listener" => fleet_api::listener::KIND.to_string(),
        "crontask" => fleet_api::cron_task::KIND.to_string(),
        "board" => fleet_api::board::KIND.to_string(),
        _ => kind.to_string(),
    };
    if let Some(served) = store
        .served_kinds()
        .into_iter()
        .find(|s| s.eq_ignore_ascii_case(&kind) || s.eq_ignore_ascii_case(singular))
    {
        return Ok(served);
    }
    Err(StoreError::Invalid(format!("unknown kind {kind:?}")).into())
}

fn get(store: &dyn Store, kind: &str, name: &str) -> Result<Object, Failure> {
    match store.get(kind, name) {
        Observation::Known(Some(o)) => Ok(o),
        Observation::Known(None) => Err(StoreError::NotFound(format!("{kind}/{name}")).into()),
        Observation::Unknown { why } => Err(StoreError::Unknown(why).into()),
    }
}

fn read_input(path: &Path) -> Result<Vec<u8>, Failure> {
    if path == Path::new("-") {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .map_err(|e| fail(format!("stdin: {e}")))?;
        return Ok(buf);
    }
    std::fs::read(path).map_err(|e| fail(format!("{}: {e}", path.display())))
}

fn cmd_apply(
    store: &dyn Store,
    config: &Config,
    file: &Path,
    json_out: bool,
    create_only: bool,
) -> Result<(), Failure> {
    let bytes = read_input(file)?;
    let doc: Value = serde_json::from_slice(&bytes)
        .map_err(|e| fail(format!("{}: not JSON: {e}", file.display())))?;
    let items = match doc {
        Value::Array(a) => a,
        Value::Object(mut o) if o.contains_key("items") && !o.contains_key("kind") => {
            match o.remove("items") {
                Some(Value::Array(a)) => a,
                _ => return Err(fail("items must be an array")),
            }
        }
        other => vec![other],
    };
    let mut objs = Vec::new();
    for (i, item) in items.into_iter().enumerate() {
        let obj: Object =
            serde_json::from_value(item).map_err(|e| fail(format!("item {i}: {e}")))?;
        objs.push(obj);
    }
    // The TaskConfig a Task takes its defaults from, as the manager
    // resolves it: the one `configRef` names, else `default`. Of several of
    // one name in the batch, the last is the one stored; else the store's.
    let task_config = |obj: &Object| {
        let name = obj.spec["configRef"]["name"]
            .as_str()
            .unwrap_or(fleet_api::config::DEFAULT_TASK_CONFIG);
        objs.iter()
            .rfind(|o| o.kind == fleet_api::config::TASK_CONFIG && o.metadata.name == name)
            .cloned()
            .or_else(|| match store.get(fleet_api::config::TASK_CONFIG, name) {
                Observation::Known(found) => found.filter(|o| !o.is_deleting()),
                Observation::Unknown { .. } => None,
            })
    };
    // A Task with no harness and no `configRef` takes `default`'s, or the
    // one its saved launch froze. One whose `configRef` names a TaskConfig
    // not there yet is checked at its launch.
    let launched = |obj: &Object| match store.get(fleet_api::task::KIND, &obj.metadata.name) {
        Observation::Known(Some(stored)) => store.launch(&stored).is_ok_and(|l| l.is_some()),
        _ => false,
    };
    if let Some(obj) = objs.iter().find(|obj| {
        obj.kind == fleet_api::task::KIND
            && obj.spec.get("harness").is_none()
            && obj.spec.get("configRef").is_none()
            && task_config(obj).is_none_or(|c| c.spec["harness"].as_str().is_none())
            && !launched(obj)
    }) {
        return Err(Failure {
            code: exit::ERROR,
            message: format!(
                "{}: spec.harness is missing, and the TaskConfig default gives none",
                obj.key()
            ),
            hint: Some("give the Task a harness, or a harness to the TaskConfig default".into()),
        });
    }
    // `local` naming no place is refused here, by name: the runtime would
    // only read the place as unreachable. The host is the manager's rule
    // (`effective_host`), from the TaskConfig the Task takes. A named
    // TaskConfig not there yet leaves it untold; with no `default`, the
    // host is `local`.
    let on_local = |obj: &Object| {
        if obj.kind != fleet_api::task::KIND {
            return false;
        }
        let config_host = if obj.spec.get("host").is_some() {
            None
        } else {
            match task_config(obj) {
                Some(config) => config.spec["host"].as_str().map(str::to_string),
                None if obj.spec.get("configRef").is_some() => return false,
                None => None,
            }
        };
        fleet_api::task::effective_host(&obj.spec, config_host.as_deref())
            == fleet_api::task::LOCAL_HOST
    };
    if let Some(why) = config.no_local()
        && objs.iter().any(on_local)
    {
        return Err(Failure {
            code: exit::ERROR,
            message: why,
            hint: Some("name a place `fleet runtime places` lists in spec.host".into()),
        });
    }
    let mut out = Vec::new();
    let mut forge: Option<ForgeHosts> = None;
    let mut gateway: Option<GatewayAccounts> = None;
    let policies = policy_names(config);
    // A taken name under --create-only is a conflict, whatever the kind.
    let refused = |e: StoreError| match e {
        StoreError::AlreadyExists(_) if create_only => Failure {
            code: exit::CONFLICT,
            message: e.to_string(),
            hint: Some("the name is taken; choose another, or apply without --create-only".into()),
        },
        e => e.into(),
    };
    for obj in objs {
        let (outcome, stored) = if obj.kind == fleet_api::conversation::KIND {
            // The account's adapter parses the address and the name derives from it.
            let Some(section) = &config.gateway else {
                return Err(Failure {
                    code: exit::ERROR,
                    message: "a Conversation needs a [gateway] section naming its account".into(),
                    hint: Some(
                        "add [gateway.accounts] and [gateway.routes] to the config file".into(),
                    ),
                });
            };
            if gateway.is_none() {
                gateway = Some(open_gateway(config)?.ok_or_else(|| fail("no [gateway] section"))?);
            }
            apply_conversation(
                store,
                gateway.as_ref().expect("opened"),
                section,
                obj,
                create_only,
            )
            .map_err(refused)?
        } else if obj.kind == fleet_api::change_request::KIND {
            // The forge adapter parses the URL and the name derives from it.
            if forge.is_none() {
                forge = Some(open_forge(config)?.ok_or_else(|| Failure {
                    code: exit::ERROR,
                    message: "a ChangeRequest needs a [forge] section naming its host".into(),
                    hint: Some(
                        "add [forge.hosts] with the change's host to the config file".into(),
                    ),
                })?);
            }
            apply_change_request(store, forge.as_ref().expect("opened"), obj, create_only)
                .map_err(refused)?
        } else {
            apply(store, obj, &policies, create_only).map_err(refused)?
        };
        let key = stored.key();
        if json_out {
            out.push(json!({"object": key, "outcome": outcome, "generation": stored.metadata.generation}));
        } else {
            println!(
                "{key} {}",
                serde_json::to_value(outcome)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_lowercase))
                    .unwrap_or_default()
            );
        }
    }
    if json_out {
        print_json(&Value::Array(out));
    }
    Ok(())
}

/// What a stream that [`cmd_open`] answers opens.
#[derive(Clone, Copy)]
enum Opens {
    /// The session's own terminal: `fleet attach`.
    View,
    /// A shell beside the session: `fleet shell`.
    Shell,
}

const RESUME_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

fn paused(obj: &Object) -> bool {
    serde_json::from_value::<fleet_api::task::TaskStatus>(obj.status.clone())
        .is_ok_and(|st| fleet_api::condition::is_true(&st.conditions, "Paused"))
}

fn refuse_unresumable(obj: &Object, task: &str, stopped: Stopped) -> Result<(), Failure> {
    let held = obj.spec.get("dispatch") != Some(&json!(true));
    let stalled = obj.status.get("phase") == Some(&json!("Stalled"));
    if !held && !stalled {
        return Ok(());
    }
    let (state, hint) = if held {
        ("held", "set spec.dispatch to true to let Fleet resume it")
    } else {
        (
            "stalled",
            "Stalled is terminal: delete the Task and apply it again",
        )
    };
    Err(Failure {
        code: exit::NOT_FOUND,
        message: match stopped {
            Stopped::Paused => {
                format!("Task/{task} is paused and {state}, so Fleet resumes nothing for it")
            }
            Stopped::Lost => format!(
                "Task/{task} lost its session and is {state}, so Fleet revives nothing for it"
            ),
        },
        hint: Some(hint.into()),
    })
}

/// Why the Task's session cannot resume, when TaskController said so:
/// `SessionLost` True with the reason `CannotResume`.
fn cannot_resume(obj: &Object) -> Option<String> {
    obj.status["conditions"]
        .as_array()?
        .iter()
        .find(|c| {
            c["type"] == "SessionLost" && c["status"] == "True" && c["reason"] == "CannotResume"
        })
        .map(|c| c["message"].as_str().unwrap_or_default().to_string())
}

fn cannot_resume_failure(task: &str, why: &str) -> Failure {
    Failure {
        code: exit::NOT_FOUND,
        message: format!("Task/{task} cannot resume its session: {why}"),
        hint: Some(format!(
            "nothing would run in its window; see why with `fleet get task {task}`"
        )),
    }
}

enum Stopped {
    Paused,
    Lost,
}

fn wait_for_revived_view(
    store: &dyn Store,
    task: &str,
    ask: impl Fn() -> Observation<Option<fleet_api::runtime::Attach>>,
) -> Result<Observation<Option<fleet_api::runtime::Attach>>, Failure> {
    wake(store, &SystemClock, task)?;
    let wait = std::time::Duration::from_millis(fleet_api::task::VIEW_WAIT_MS);
    let deadline = std::time::Instant::now() + wait;
    let mut last = Observation::Known(None);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(500));
        if let Some(why) = cannot_resume(&get(store, fleet_api::task::KIND, task)?) {
            return Err(cannot_resume_failure(task, &why));
        }
        last = ask();
        if matches!(last, Observation::Known(Some(_))) {
            return Ok(last);
        }
    }
    if matches!(last, Observation::Unknown { .. }) {
        return Ok(last);
    }
    Err(Failure {
        code: exit::NOT_FOUND,
        message: format!(
            "Task/{task} lost its session, and Fleet did not revive it within {}s",
            wait.as_secs()
        ),
        hint: Some(format!(
            "the Fleet engine revives it on its next pass; check that it runs, then \
             try again, or see why with `fleet get task {task}`"
        )),
    })
}

fn wait_for_resume(store: &dyn Store, task: &str) -> Result<(), Failure> {
    wake(store, &SystemClock, task)?;
    let deadline = std::time::Instant::now() + RESUME_WAIT;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(250));
        if !paused(&get(store, fleet_api::task::KIND, task)?) {
            return Ok(());
        }
    }
    Err(Failure {
        code: exit::NOT_FOUND,
        message: format!(
            "Task/{task} is paused, and Fleet did not resume it within {}s",
            RESUME_WAIT.as_secs()
        ),
        hint: Some(format!(
            "the Fleet engine resumes it on its next pass; check that it runs, then \
             try again, or see why with `fleet get task {task}`"
        )),
    })
}

/// `fleet attach` and `fleet shell`: the argv that opens a terminal stream
/// on the Task's session, as `ask` answers it. A runtime's `tty` answer is
/// a raw interactive command, which a view never sees: it runs under
/// `fleet term-bridge`, so every view gets a stream.
fn cmd_open(
    store: &dyn Store,
    task: &str,
    json_out: bool,
    wake_paused: bool,
    opens: Opens,
    runtime: &dyn fleet_api::runtime::Runtime,
) -> Result<(), Failure> {
    use fleet_api::runtime::Attach;
    let obj = get(store, fleet_api::task::KIND, task)?;
    let session: Option<fleet_api::task::SessionRef> = obj
        .status
        .get("session")
        .filter(|s| !s.is_null())
        .map(|s| serde_json::from_value(s.clone()))
        .transpose()
        .map_err(|e| fail(format!("Task/{task} status.session: {e}")))?;
    let Some(session) = session else {
        return Err(Failure {
            code: exit::NOT_FOUND,
            message: format!("Task/{task} has no session yet"),
            hint: Some("a stream waits for the Task's spawn".into()),
        });
    };
    if paused(&obj) {
        if !wake_paused {
            return Err(Failure {
                code: exit::NOT_FOUND,
                message: format!("Task/{task} is paused: its agent is stopped"),
                hint: Some(format!(
                    "add --wake to resume it and open it, or send it a message with \
                     `fleet send {task} TEXT`"
                )),
            });
        }
        refuse_unresumable(&obj, task, Stopped::Paused)?;
        wait_for_resume(store, task)?;
    }
    if let Some(why) = cannot_resume(&obj) {
        return Err(cannot_resume_failure(task, &why));
    }
    let ask = |session: &fleet_api::task::SessionRef| match opens {
        Opens::View => runtime.attach(session),
        Opens::Shell => runtime.shell(session),
    };
    let mut answer = ask(&session);
    if matches!(answer, Observation::Known(None))
        && matches!(runtime.lost(&session), Observation::Known(true))
    {
        refuse_unresumable(&obj, task, Stopped::Lost)?;
        answer = wait_for_revived_view(store, task, || ask(&session))?;
    }
    let argv = match answer {
        Observation::Known(Some(Attach::Stream { argv })) => argv,
        Observation::Known(Some(Attach::Tty { argv })) => {
            let fleet = std::env::current_exe()
                .map_err(|e| fail(format!("cannot name this program for the bridge: {e}")))?;
            [
                fleet.to_string_lossy().into_owned(),
                "term-bridge".into(),
                "--".into(),
            ]
            .into_iter()
            .chain(argv)
            .collect()
        }
        Observation::Known(None) => {
            return Err(Failure {
                code: exit::NOT_FOUND,
                message: format!(
                    "the runtime serving place {} offers no {} of Task/{task}",
                    session.host,
                    match opens {
                        Opens::View => "live view",
                        Opens::Shell => "shell",
                    }
                ),
                hint: None,
            });
        }
        Observation::Unknown { why } => return Err(StoreError::Unknown(why).into()),
    };
    if json_out {
        println!("{}", json!({ "stream": { "argv": argv } }));
        return Ok(());
    }
    for arg in argv {
        println!("{arg}");
    }
    Ok(())
}

/// `fleet watch`: reads the Tasks every `poll_ms` and prints what moved
/// ([`Watch`] decides what).
fn cmd_watch(
    store: &dyn Store,
    since: u64,
    once: bool,
    poll_ms: u64,
    json_out: bool,
) -> Result<(), Failure> {
    use std::io::Write;
    let mut watch = Watch::new(since);
    let mut out = std::io::stdout().lock();
    loop {
        let tasks = match store.list(fleet_api::task::KIND) {
            Observation::Known(tasks) => tasks,
            Observation::Unknown { why } if once => return Err(StoreError::Unknown(why).into()),
            Observation::Unknown { why } => {
                eprintln!("warning: could not read the Tasks; nothing assumed: {why}");
                // The next read that works is `synced` again: a view that
                // showed this warning learns its rows are current once more.
                watch.synced = false;
                std::thread::sleep(std::time::Duration::from_millis(poll_ms));
                continue;
            }
        };
        let Some(events) = watch.read(&tasks).map_err(fail)? else {
            // A write landed between this read and the last: read again now.
            std::thread::sleep(std::time::Duration::from_millis(5));
            continue;
        };
        for event in events {
            let line = if json_out {
                event.to_string()
            } else {
                watch_line(&event)
            };
            // A reader that went away ends the watch.
            if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
                return Ok(());
            }
        }
        if once && watch.synced {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(poll_ms));
    }
}

/// What `fleet watch` reports from each read of the Tasks.
///
/// The cursor is the store's write counter: every write gives the object a
/// higher `resourceVersion`, so a Task above the cursor changed since. A
/// deletion leaves nothing to compare, so the first report ends with
/// `synced`, the names present then: a view resuming with `--since` drops
/// any other it holds. An unreadable store reports nothing, and the first
/// read that works after it ends with `synced` again.
///
/// A list reads each Task's file on its own, so one read can hold a later
/// write and miss an earlier one, and a revision reported from it would let
/// a resume skip the missed write. A read is reported only when the read
/// before it was the same: the store's writes are serialized, so every
/// write below the highest revision either read holds was already in place
/// when the second read began.
#[derive(Debug)]
struct Watch {
    since: u64,
    cursor: u64,
    synced: bool,
    seen: BTreeMap<String, (String, u64)>,
    last: Option<BTreeMap<String, (String, u64)>>,
}

impl Watch {
    fn new(since: u64) -> Watch {
        Watch {
            since,
            cursor: since,
            synced: false,
            seen: BTreeMap::new(),
            last: None,
        }
    }

    /// The events one read gives, or `None` while it may be torn.
    fn read(&mut self, tasks: &[Object]) -> Result<Option<Vec<Value>>, String> {
        let state: BTreeMap<String, (String, u64)> = tasks
            .iter()
            .map(|t| {
                (
                    t.metadata.name.clone(),
                    (t.metadata.uid.clone(), t.metadata.resource_version),
                )
            })
            .collect();
        if self.last.as_ref() != Some(&state) {
            self.last = Some(state);
            return Ok(None);
        }
        let mut events = Vec::new();
        for (name, (uid, _)) in &self.seen {
            if state.get(name).is_none_or(|(now, _)| now != uid) {
                events.push(
                    json!({"type": "deleted", "revision": self.cursor, "name": name, "uid": uid}),
                );
            }
        }
        let mut changed: Vec<&Object> = tasks
            .iter()
            .filter(|t| match self.seen.get(&t.metadata.name) {
                Some(seen) => *seen != (t.metadata.uid.clone(), t.metadata.resource_version),
                None => t.metadata.resource_version > self.since,
            })
            .collect();
        changed.sort_by_key(|t| t.metadata.resource_version);
        for t in changed {
            self.cursor = self.cursor.max(t.metadata.resource_version);
            events.push(json!({
                "type": "changed",
                "revision": t.metadata.resource_version,
                "object": serde_json::to_value(t).map_err(|e| e.to_string())?,
            }));
        }
        if !self.synced {
            self.synced = true;
            events.push(json!({"type": "synced", "revision": self.cursor, "names": state.keys().collect::<Vec<_>>()}));
        }
        self.seen = state;
        Ok(Some(events))
    }
}

fn watch_line(event: &Value) -> String {
    let revision = &event["revision"];
    match event["type"].as_str() {
        Some("changed") => {
            let o = &event["object"];
            format!(
                "{revision} changed Task/{} {}",
                o["metadata"]["name"].as_str().unwrap_or("?"),
                o["status"]["phase"].as_str().unwrap_or("-")
            )
        }
        Some("deleted") => format!(
            "{revision} deleted Task/{}",
            event["name"].as_str().unwrap_or("?")
        ),
        _ => format!("{revision} synced"),
    }
}

fn cmd_list(store: &dyn Store, kind: &str, json_out: bool) -> Result<(), Failure> {
    let objs = match store.list(kind) {
        Observation::Known(o) => o,
        Observation::Unknown { why } => return Err(StoreError::Unknown(why).into()),
    };
    if json_out {
        print_json(&serde_json::to_value(&objs).map_err(|e| fail(e.to_string()))?);
        return Ok(());
    }
    println!("{:<40} {:<11} GENERATION", "NAME", "PHASE");
    for o in &objs {
        let phase = o.status.get("phase").and_then(Value::as_str).unwrap_or("-");
        println!(
            "{:<40} {:<11} {}",
            o.metadata.name, phase, o.metadata.generation
        );
    }
    Ok(())
}

fn status_summary(obj: &Object) -> Value {
    let observed = if obj.kind != fleet_api::task::KIND {
        obj.metadata.generation
    } else {
        obj.status
            .get("observedGeneration")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let field = |k: &str| obj.status.get(k).cloned().unwrap_or(Value::Null);
    let mut summary = json!({
        "schema": "fleet.status/v1",
        "kind": obj.kind,
        "name": obj.metadata.name,
        "generation": obj.metadata.generation,
        "observedGeneration": observed,
        "current": observed == obj.metadata.generation,
        "deleting": obj.is_deleting(),
        "finalizers": obj.metadata.finalizers,
        "phase": field("phase"),
        "conditions": obj.status.get("conditions").cloned().unwrap_or_else(|| json!([])),
    });
    let own = summary.as_object_mut().expect("object");
    if obj.kind == fleet_api::change_request::KIND {
        // The bookkeeping and the whole numbered snapshot.
        for k in [
            "ref",
            "observedAt",
            "notBefore",
            "closedAt",
            "settledAt",
            "revision",
            "state",
            "everReady",
            "head",
            "reviewDecision",
            "mergeable",
            "threads",
            "reviews",
            "notes",
            "droppedBefore",
        ] {
            own.insert(k.to_string(), field(k));
        }
    } else if obj.kind == fleet_api::conversation::KIND {
        // The tie, the numbered mirror and the spool.
        for k in [
            "ref",
            "task",
            "revision",
            "judged",
            "starter",
            "droppedBefore",
            "observedAt",
            "lastMessageAt",
            "notBefore",
            "dir",
            "retiring",
            "reactions",
        ] {
            own.insert(k.to_string(), field(k));
        }
        for k in ["messages", "received", "seenEvents", "refusals"] {
            own.insert(
                k.to_string(),
                field(k).as_array().map_or_else(|| json!([]), |a| json!(a)),
            );
        }
        own.insert(
            "effects".to_string(),
            field("effects")
                .as_object()
                .map_or_else(|| json!({}), |o| json!(o)),
        );
    } else if obj.kind == fleet_api::listener::KIND {
        for k in [
            "accepted",
            "refused",
            "beatRevision",
            "seen",
            "observedAt",
            "notBefore",
        ] {
            own.insert(k.to_string(), field(k));
        }
        own.insert(
            "beats".to_string(),
            field("beats")
                .as_object()
                .map_or_else(|| json!({}), |o| json!(o)),
        );
    } else if !fleet_api::kinds::is_served(&obj.kind) {
        own.insert("status".into(), obj.status.clone());
    } else {
        for k in [
            "session",
            "sessionState",
            "workspace",
            "execution",
            "brief",
            "result",
            "forge",
            "gateway",
            "answered",
        ] {
            own.insert(k.to_string(), field(k));
        }
        own.insert(
            "declared".to_string(),
            obj.status
                .get("declared")
                .cloned()
                .unwrap_or_else(|| json!([])),
        );
        own.insert(
            "inbox".to_string(),
            obj.status
                .get("inbox")
                .cloned()
                .unwrap_or_else(|| json!([])),
        );
        own.insert(
            "deliveries".to_string(),
            obj.status
                .get("deliveries")
                .cloned()
                .unwrap_or_else(|| json!({})),
        );
    }
    summary
}

fn print_status(s: &Value) {
    let text = |k: &str| {
        s.get(k)
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
            .unwrap_or_default()
    };
    println!("{}/{}  phase {}", text("kind"), text("name"), text("phase"));
    let current = if s["current"].as_bool() == Some(true) {
        "current"
    } else {
        "STALE: status lags the spec"
    };
    println!(
        "generation {} observed {} ({current})",
        text("generation"),
        text("observedGeneration")
    );
    if s["deleting"].as_bool() == Some(true) {
        println!("being deleted; finalizers {}", s["finalizers"]);
    }
    for c in s["conditions"].as_array().into_iter().flatten() {
        let field = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("");
        println!(
            "  {:<16} {:<8} {:<26} {}",
            field("type"),
            field("status"),
            field("reason"),
            field("message")
        );
    }
    if let Some(id) = s["session"]["id"].as_str() {
        println!(
            "session {id} on {} {}",
            s["session"]["host"].as_str().unwrap_or("-"),
            s["sessionState"].as_str().unwrap_or("(not yet observed)")
        );
    }
    if let Some(dir) = s["workspace"]["dirs"][0].as_str() {
        println!(
            "workspace {} on {} at {dir}",
            s["workspace"]["key"].as_str().unwrap_or("-"),
            s["workspace"]["host"].as_str().unwrap_or("-")
        );
    }
    if !s["result"].is_null() {
        println!("result {}", s["result"]);
    }
    if s["kind"] == fleet_api::change_request::KIND && !s["revision"].is_null() {
        println!(
            "revision {} state {} head {} checks {} notes {} unresolved {}",
            s["revision"],
            s["state"]["value"].as_str().unwrap_or("-"),
            s["head"]["id"].as_str().unwrap_or("-"),
            s["head"]["checks"].as_str().unwrap_or("-"),
            s["notes"].as_array().map_or(0, Vec::len),
            s["threads"]["unresolved"]
        );
    }
    if s["kind"] == fleet_api::conversation::KIND && !s["revision"].is_null() {
        println!(
            "revision {} task {} {} message(s) {} spooled",
            s["revision"],
            s["task"]["name"].as_str().unwrap_or("-"),
            s["messages"].as_array().map_or(0, Vec::len),
            s["received"].as_array().map_or(0, Vec::len)
        );
    }
    if s["kind"] == fleet_api::listener::KIND && !s["beatRevision"].is_null() {
        for (instance, b) in s["beats"].as_object().into_iter().flatten() {
            println!(
                "beat {instance} {} digest {} at {}",
                b["state"].as_str().unwrap_or("-"),
                b["digest"].as_str().unwrap_or("-"),
                b["at"]
            );
        }
    }
    let queued = s["inbox"].as_array().map_or(0, Vec::len);
    let delivered = s["deliveries"]["receipts"].as_array().map_or(0, Vec::len);
    if queued + delivered > 0 {
        println!("inbox {queued} queued, {delivered} delivered");
    }
    if let Some(changes) = s["forge"]["changes"].as_object() {
        for (name, e) in changes {
            println!(
                "following {name} {} cursor {} gaps {} ready {}",
                e["state"].as_str().unwrap_or("-"),
                e["cursor"],
                e["gaps"],
                e["markReady"]["state"].as_str().unwrap_or("-")
            );
        }
        for c in s["forge"]["conditions"].as_array().into_iter().flatten() {
            let field = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("");
            println!(
                "  forge {:<10} {:<8} {:<26} {}",
                field("type"),
                field("status"),
                field("reason"),
                field("message").lines().next().unwrap_or("")
            );
        }
    }
    if let Some(gw) = s.get("gateway").filter(|g| g.is_object()) {
        if let Some(tie) = gw.get("conversation").filter(|t| t.is_object()) {
            println!(
                "gateway {} {} cursor {} seen {} gaps {}",
                tie["state"].as_str().unwrap_or("-"),
                tie["name"].as_str().unwrap_or("-"),
                gw["cursor"],
                gw["seen"],
                gw["gaps"]
            );
        }
        let children = gw["children"].as_object().map_or(0, |c| c.len());
        if children > 0 {
            println!("gateway children {children}");
        }
        for c in gw["conditions"].as_array().into_iter().flatten() {
            let field = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("");
            println!(
                "  gateway {:<8} {:<8} {:<26} {}",
                field("type"),
                field("status"),
                field("reason"),
                field("message").lines().next().unwrap_or("")
            );
        }
    }
    for (id, a) in s["answered"].as_object().into_iter().flatten() {
        println!(
            "answered {id} by {} ({})",
            a["from"].as_str().unwrap_or("-"),
            a["key"].as_str().unwrap_or("-")
        );
    }
}

fn result_from_flags(
    generation: Option<u64>,
    outcome: Option<String>,
    summary: String,
    artifacts: Vec<String>,
    outputs: Vec<String>,
) -> Result<TaskResult, Failure> {
    let generation = generation.ok_or_else(|| {
        fail("--generation is required: name the generation you were briefed for")
    })?;
    let outcome = outcome.ok_or_else(|| fail("--outcome is required"))?;
    let outcome: Outcome =
        serde_json::from_value(Value::String(outcome.clone())).map_err(|_| {
            fail(format!(
                "--outcome {outcome:?}: use shipped, stuck, failed or not-applicable"
            ))
        })?;
    let mut map = std::collections::BTreeMap::new();
    for o in outputs {
        let (name, raw) = o
            .split_once('=')
            .ok_or_else(|| fail(format!("--output {o:?}: use NAME=JSON")))?;
        let value: Value = serde_json::from_str(raw)
            .map_err(|e| fail(format!("--output {name}: not JSON: {e}")))?;
        map.insert(name.to_string(), value);
    }
    Ok(TaskResult {
        outcome,
        generation,
        summary,
        artifacts,
        outputs: map,
    })
}

fn print_json(v: &Value) {
    match serde_json::to_string_pretty(v) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("error: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(name: &str, rv: u64) -> Object {
        serde_json::from_value(json!({
            "apiVersion": "fleet/v1alpha1",
            "kind": "Task",
            "metadata": {"name": name, "uid": format!("uid-{name}"), "resourceVersion": rv},
            "spec": {},
        }))
        .expect("an object")
    }

    /// The events of `reads`, one after another, as the watch reports them.
    fn report(watch: &mut Watch, reads: &[Vec<Object>]) -> Vec<Value> {
        reads
            .iter()
            .filter_map(|r| watch.read(r).unwrap())
            .flatten()
            .collect()
    }

    #[test]
    fn a_torn_read_never_moves_the_cursor_past_a_write_it_missed() {
        // The first read took A before a writer moved it to 3 and B after it
        // moved B to 4: reporting it would say "synced at 4" with A at 1.
        let torn = vec![task("a", 1), task("b", 4)];
        let whole = vec![task("a", 3), task("b", 4)];
        let mut watch = Watch::new(0);
        let events = report(&mut watch, &[torn, whole.clone(), whole]);
        let a: Vec<u64> = events
            .iter()
            .filter(|e| e["object"]["metadata"]["name"] == "a")
            .map(|e| e["revision"].as_u64().unwrap())
            .collect();
        assert_eq!(a, [3], "{events:#?}");
        assert_eq!(events.last().unwrap()["type"], "synced");
        assert_eq!(events.last().unwrap()["revision"], 4);
    }
}
