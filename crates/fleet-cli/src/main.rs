//! `fleet`: the command line of the Rust rewrite. It parses arguments, opens
//! the store and the runtime the config names, calls fleet-manager, and
//! prints. Every rule about what may be written lives in fleet-manager and
//! below; nothing here decides.

mod bootstrap;
mod config;

use std::collections::BTreeMap;
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
    receive, record_result, send,
};
use fleet_runtime::{ExecProgram, ExecRuntime, FakeHost, FakeRuntime, FileSignals};
use fleet_store::{FileStore, SystemClock};
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
    /// The hosts a Task may name: the `[hosts]` of the config, or none
    /// (`null` with --json) when one `[runtime]` serves any host.
    Hosts {
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

/// Exit codes, stable for scripts.
mod exit {
    pub const ERROR: u8 = 1;
    pub const NOT_FOUND: u8 = 3;
    pub const UNKNOWN: u8 = 4;
    pub const CONFLICT: u8 = 5;
    pub const LEASE: u8 = 6;
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

fn main() -> ExitCode {
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    let name = matches.subcommand_name().unwrap_or_default().to_string();
    let telemetry = fleet_telemetry::init(VERSION).unwrap_or_else(|why| {
        eprintln!("warning: telemetry is off: {why}");
        fleet_telemetry::Telemetry::default()
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
    let code = if name == "up" {
        command()
    } else {
        telemetry.command(&name, command)
    };
    telemetry.shutdown();
    ExitCode::from(code)
}

fn run(cli: Cli) -> Result<(), Failure> {
    if let Command::Version { json } = &cli.command {
        return version(*json, cli.config.clone());
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
        println!(
            "{} {}; run fleet up (runtime-mux, tmux and an authenticated harness are required)",
            if written { "initialized" } else { "kept" },
            path.display()
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
            Config::parse(&text, p).map_err(fail)?
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
        // Runtime programs serve exactly the hosts named for them, as
        // `open_runtime` wires them.
        let hosts = (!config.runtimes.is_empty()).then(|| config.hosts.keys().collect::<Vec<_>>());
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
    let data_dir = cli
        .data_dir
        .clone()
        .or(config.data.dir.clone())
        .or_else(default_data_dir)
        .ok_or_else(|| {
            fail("no data directory: pass --data-dir or set data.dir in the config file")
        })?;
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
    let file_store = FileStore::open(&data_dir)?.with_plugins(&registrations)?;
    let store: &dyn Store = &file_store;
    match cli.command {
        Command::Up {
            poll_ms,
            lease_ttl_ms,
        } => {
            let path = config_path.ok_or_else(|| fail("no configuration; run fleet init first"))?;
            cmd_up(
                store,
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
            let holder = holder.unwrap_or_else(|| format!("fleet-{}", std::process::id()));
            let manager = Manager {
                store,
                runtime: runtime.as_ref(),
                clock: &SystemClock,
                holder,
                forge: forge.as_ref().map(|f| f as &dyn Forge),
                signals: Some(&signals),
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
        Command::Attach { task, json } => {
            let signals = open_signals(&data_dir)?;
            let runtime = open_runtime(&config, &signals)?;
            cmd_attach(store, runtime.as_ref(), &task, json)
        }
        Command::Watch {
            since,
            once,
            poll_ms,
            json,
        } => cmd_watch(store, since, once, poll_ms, json),
        Command::Version { .. }
        | Command::Signal { .. }
        | Command::Runtime { .. }
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
    fleet_runtime::signals::append(std::path::Path::new(&file), &signal)
        .map_err(|e| fail(format!("{}: {e}", Path::new(&file).display())))
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
    store: &dyn Store,
    (config, data_dir): (&Config, &Path),
    config_path: &Path,
    poll_ms: u64,
    lease_ttl_ms: Option<u64>,
    plugins: (
        &dyn fleet_api::plugin::Plugins,
        &[fleet_api::plugin::Registration],
    ),
) -> Result<(), Failure> {
    let signals = open_signals(data_dir)?;
    let runtime = open_runtime(config, &signals)?;
    let forge = open_forge(config)?;
    let gateway = open_gateway(config)?;
    // Every host the config names, or `local` under one `[runtime]`. One
    // that answers is enough to start; each other is reported, and its
    // Tasks wait in their own status.
    let hosts: Vec<&str> = if config.hosts.is_empty() {
        vec!["local"]
    } else {
        config.hosts.keys().map(String::as_str).collect()
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
    let manager = Manager {
        store,
        runtime: runtime.as_ref(),
        clock: &SystemClock,
        holder: format!("fleet-up-{}", std::process::id()),
        forge: forge.as_ref().map(|f| f as &dyn Forge),
        signals: Some(&signals),
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
    println!(
        "{exe} speaks runtime protocol {}",
        fleet_runtime::exec::PROTOCOL
    );
    Ok(())
}

fn version(json: bool, config: Option<PathBuf>) -> Result<(), Failure> {
    if json {
        // The user's profiles too, so a view offers their harnesses; one
        // that does not parse is left out here and refused by every
        // command that loads the config.
        let user = user_profiles(
            harness_dir(config::discover(config).as_deref()).as_deref(),
            false,
        )
        .unwrap_or_default();
        print_json(&json!({
            "version": VERSION,
            "apiVersions": [API_VERSION],
            "kinds": fleet_api::kinds::BUILT_IN,
            "configVersion": CONFIG_VERSION,
            "storeFormat": {"format": "fleet-store", "version": 1},
            "runtimes": ["exec", "fake"],
            "harnesses": fleet_api::harness::Profiles::bundled().with(user).names(),
            "runtimeProtocol": fleet_runtime::exec::PROTOCOL,
            "forges": ["azure-devops", "bitbucket-cloud", "github", "gitlab", "fake"],
            "gateways": Platform::ALL,
        }));
    } else {
        println!("fleet {VERSION}");
    }
    Ok(())
}

/// The signal files, one per session on this machine, under the store, and
/// beside them the plugin files the harness profiles name: every command
/// that starts a session opens the signals first.
fn open_signals(data_dir: &Path) -> Result<FileSignals, Failure> {
    let assets = data_dir.join(fleet_api::harness::ASSET_DIR);
    fleet_runtime::assets::write(&assets)
        .map_err(|e| fail(format!("{}: {e}", assets.display())))?;
    let dir = data_dir.join("signals");
    FileSignals::open(&dir).map_err(|e| fail(format!("{}: {e}", dir.display())))
}

/// The runtime: the programs under `[runtimes]`, one per host, or the fake.
fn open_runtime(config: &Config, signals: &FileSignals) -> Result<Box<dyn Runtime>, Failure> {
    if !config.runtimes.is_empty() {
        let mut hosts = BTreeMap::new();
        for (host, h) in &config.hosts {
            let p = &config.runtimes[&h.runtime];
            let mut rt = ExecProgram::new(h.runtime.clone(), p.program.clone());
            rt.args = p.args.clone();
            rt.call_timeout = std::time::Duration::from_millis(
                p.call_timeout_ms.unwrap_or(DEFAULT_CALL_TIMEOUT_MS),
            );
            if let Some(ms) = p.workspace_timeout_ms {
                rt.workspace_timeout = std::time::Duration::from_millis(ms);
            }
            if let Some(prefix) = &p.key_prefix {
                rt.key_prefix = prefix.clone();
            }
            hosts.insert(host.clone(), rt);
        }
        let runtime = ExecRuntime::new(hosts).with_signals(signals.clone());
        return Ok(Box::new(runtime));
    }
    let Some(rt) = &config.runtime else {
        return Err(Failure {
            code: exit::ERROR,
            message: "no runtime configured".into(),
            hint: Some(
                "add [runtimes.mux] program = \"runtime-mux\" and [hosts.local] runtime = \"mux\" to the config file, or run fleet init"
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

/// `fleet attach`: the argv that opens the Task's terminal stream. A
/// runtime's `tty` answer is a raw interactive command, which a view never
/// sees: it runs under `fleet term-bridge`, so every view gets a stream.
fn cmd_attach(
    store: &dyn Store,
    runtime: &dyn Runtime,
    task: &str,
    json_out: bool,
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
            hint: Some("an attach waits for the Task's spawn".into()),
        });
    };
    let argv = match runtime.attach(&session) {
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
                    "the runtime serving host {} offers no live view of Task/{task}",
                    session.host
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
