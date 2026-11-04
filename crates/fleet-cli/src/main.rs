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
    apply_change_request, apply_conversation, beat, declare, declare_change_request, delete,
    receive, record_result, send,
};
use fleet_runtime::{
    Cleanup, FakeHost, FakeRuntime, Repositories, ThurboxConfig, ThurboxRuntime, TrustInterface,
};
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
    /// Create local config and the bundled editable VEGA workspace, offline.
    Init {
        /// Offer bundled updates, preserving edited files with conflict copies.
        #[arg(long)]
        update: bool,
        /// The coding agent for VEGA and, by default, its workers, such as
        /// claude or codex. Without it, a terminal prompts on first init.
        #[arg(long, value_name = "NAME")]
        harness: Option<String>,
    },
    /// Start/reconnect VEGA and keep all due Tasks reconciling until Ctrl-C.
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
    /// Delete a resource; a Task is released by its controller first.
    Delete { kind: String, name: String },
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
                Some("see crates/CONTRACTS.md for what this build serves"),
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
        return version(*json);
    }
    let explicit_config = cli.config.is_some() || std::env::var_os(config::CONFIG_ENV).is_some();
    let config_path = config::discover(cli.config.clone())
        .map(|p| bootstrap::absolute(&p))
        .transpose()
        .map_err(fail)?;
    if let Command::Init { update, harness } = cli.command {
        let path = config_path
            .ok_or_else(|| fail("no config location: pass --config or set HOME/APPDATA"))?;
        let root = default_data_dir()
            .unwrap_or_else(|| path.parent().unwrap_or(Path::new(".")).join("fleet-data"));
        let done = bootstrap::init(
            &path,
            &root,
            cli.data_dir.as_deref(),
            update,
            harness.as_deref(),
            &ask_harness,
        )
        .map_err(fail)?;
        for notice in done.notices {
            println!("{notice}");
        }
        match done.harness {
            Some(h) => println!(
                "initialized {}; task.json chooses harness {h} for VEGA and its workers; run fleet up (Thurbox and an authenticated {h} are required)",
                path.display()
            ),
            None => println!(
                "initialized {}; run fleet up (Thurbox and an authenticated harness are required)",
                path.display()
            ),
        }
        return Ok(());
    }
    // The text is kept: `fleet up` compares later edits against it.
    let (config, config_text) = match &config_path {
        Some(p) if !explicit_config && !p.try_exists().map_err(|e| fail(e.to_string()))? => {
            (Config::empty(), String::new())
        }
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .map_err(|e| fail(format!("cannot read config {}: {e}", p.display())))?;
            (Config::parse(&text, p).map_err(fail)?, text)
        }
        None => (Config::empty(), String::new()),
    };
    let data_dir = cli
        .data_dir
        .clone()
        .or(config.data.dir.clone())
        .or_else(default_data_dir)
        .ok_or_else(|| {
            fail("no data directory: pass --data-dir or set data.dir in the config file")
        })?;
    if matches!(cli.command, Command::Up { .. }) {
        if config.starter.is_none() {
            return Err(fail(
                "no starter workspace configured; run fleet init first",
            ));
        }
        if let Some(override_dir) = &cli.data_dir {
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
                (&config, &config_text),
                &path,
                poll_ms,
                lease_ttl_ms,
                (&plugins, &registrations),
            )
        }
        Command::Init { .. } => unreachable_version(),
        Command::Apply { file, json } => cmd_apply(store, &config, &file, json),
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
            let runtime = open_runtime(&config)?;
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
        Command::Version { .. } => unreachable_version(),
    }
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

/// Ask for the coding agent, only when a person is at a terminal. An empty
/// answer keeps `default`.
fn ask_harness(default: &str) -> Result<Option<String>, String> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Ok(None);
    }
    eprint!("Coding agent for VEGA and its workers, such as claude or codex [{default}]: ");
    std::io::stderr().flush().map_err(|e| e.to_string())?;
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|e| e.to_string())?;
    let answer = answer.trim();
    Ok((!answer.is_empty()).then(|| answer.to_string()))
}

fn cmd_up(
    store: &dyn Store,
    (config, config_text): (&Config, &str),
    config_path: &Path,
    poll_ms: u64,
    lease_ttl_ms: Option<u64>,
    plugins: (
        &dyn fleet_api::plugin::Plugins,
        &[fleet_api::plugin::Registration],
    ),
) -> Result<(), Failure> {
    let starter = config
        .starter
        .as_ref()
        .ok_or_else(|| fail("no starter workspace configured; run fleet init first"))?;
    let (runtime, repositories) = open_reloadable_runtime(config)?;
    let mut reload = repositories.map(|map| Reload::new(config_path, config_text, map));
    let forge = open_forge(config)?;
    let gateway = open_gateway(config)?;
    let task = match store.get("Task", "vega") {
        Observation::Known(Some(task)) => task,
        Observation::Known(None) => {
            let mut tasks: Vec<Object> = serde_json::from_slice(
                &std::fs::read(starter.workspace.join("task.json"))
                    .map_err(|e| fail(format!("starter task.json: {e}; run fleet init")))?,
            )
            .map_err(|e| fail(format!("starter task.json: {e}")))?;
            if tasks.len() != 1 || tasks[0].kind != "Task" || tasks[0].metadata.name != "vega" {
                return Err(fail("starter task.json must contain exactly Task/vega"));
            }
            fleet_manager::verbs::ensure_task(store, tasks.remove(0))?
        }
        Observation::Unknown { why } => return Err(StoreError::Unknown(why).into()),
    };
    let host = task.spec["host"].as_str().unwrap_or("local");
    match runtime.host(host) {
        Observation::Known(true) => {}
        Observation::Known(false) => {
            return Err(fail(
                "Thurbox host/multiplexer unavailable; install/configure Thurbox and an authenticated agent harness, then retry fleet up",
            ));
        }
        Observation::Unknown { why } => {
            return Err(fail(format!(
                "thurbox prerequisite could not be checked: {why}; install/configure Thurbox and authenticate the agent harness before fleet up"
            )));
        }
    }
    if task.metadata.deletion_timestamp.is_some() || !task.status["result"].is_null() {
        return Err(fail(
            "Task/vega is deleting or has a worker result; inspect fleet status task vega before deliberately creating a new session",
        ));
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
        change_request_defaults: forge_defaults(config),
        forge_defaults: relay_defaults(config),
        plugins: Some(plugins),
        gateway: gateway_side(config, gateway.as_ref()),
    };
    println!(
        "Task/vega {}: reconciling all Tasks; config {}; Ctrl-C stops Fleet and retains sessions",
        task.metadata.uid,
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
            match store.get("Task", "vega") {
                Observation::Known(Some(task)) => {
                    if let Some(conditions) = task.status["conditions"].as_array() {
                        for condition in conditions {
                            let status = condition["status"].as_str().unwrap_or("");
                            let startup = matches!(condition["type"].as_str(), Some("HostReachable" | "Spawned" | "Briefed"));
                            let attention = matches!(condition["type"].as_str(), Some("NeedsAttention" | "Stalled" | "WorkerGone"));
                            if (startup && matches!(status, "False" | "Unknown")) || (attention && status == "True") {
                                diagnostics.push(format!("Task/vega {}: {} {}. Inspect the Thurbox session and authenticated harness if readiness or startup is blocked", condition["type"].as_str().unwrap_or(""), condition["reason"].as_str().unwrap_or(""), condition["message"].as_str().unwrap_or("")));
                            }
                        }
                    }
                },
                Observation::Unknown { why } => diagnostics.push(format!("Task/vega status unknown: {why}")),
                Observation::Known(None) => diagnostics.push("Task/vega is absent; inspect its deletion before restarting Mission Control".into()),
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
            if let Some(reload) = reload.as_mut() {
                reload.poll();
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
    Err(fail("version is handled before the store opens"))
}

fn version(json: bool) -> Result<(), Failure> {
    if json {
        print_json(&json!({
            "version": VERSION,
            "apiVersions": [API_VERSION],
            "kinds": fleet_api::kinds::BUILT_IN,
            "configVersion": CONFIG_VERSION,
            "storeFormat": {"format": "fleet-store", "version": 1},
            "runtimes": ["thurbox", "fake"],
            "forges": ["azure-devops", "bitbucket-cloud", "github", "gitlab", "fake"],
            "gateways": Platform::ALL,
        }));
    } else {
        println!("fleet {VERSION}");
    }
    Ok(())
}

/// What `fleet up` re-reads from its config file between sweeps:
/// `runtime.repositories`, and nothing else. Every other setting, the store
/// and the runtime's identity among them, stays as the process started.
struct Reload<'a> {
    path: &'a Path,
    repositories: Repositories,
    /// The file as it started, without `runtime.repositories`.
    started: Option<toml::Value>,
    /// The last notice printed, so a lasting state is reported once.
    notice: String,
}

impl<'a> Reload<'a> {
    /// `text` is the file the process started from, as it was read then.
    fn new(path: &'a Path, text: &str, repositories: Repositories) -> Reload<'a> {
        Reload {
            path,
            repositories,
            started: without_repositories(text),
            notice: String::new(),
        }
    }

    /// Apply a valid file's `runtime.repositories`; keep the last valid map
    /// when the file cannot be read, parsed or validated.
    fn poll(&mut self) {
        let notice = match self.read() {
            Err(why) => {
                format!("config reload refused: {why}; keeping the last valid runtime.repositories")
            }
            Ok((map, others)) => {
                let count = map.len();
                if self.repositories.replace(map) {
                    println!("config reloaded: runtime.repositories maps {count} repositories");
                }
                if others == self.started {
                    String::new()
                } else {
                    "config changed outside runtime.repositories; restart fleet up to apply it"
                        .into()
                }
            }
        };
        if notice != self.notice {
            if !notice.is_empty() {
                eprintln!("{notice}");
            }
            self.notice = notice;
        }
    }

    /// The file's valid map, and the file without it.
    fn read(&self) -> Result<(BTreeMap<String, String>, Option<toml::Value>), String> {
        let text = std::fs::read_to_string(self.path)
            .map_err(|e| format!("cannot read config {}: {e}", self.path.display()))?;
        let config = Config::parse(&text, self.path)?;
        let map = match &config.runtime {
            Some(rt) if rt.kind == "thurbox" => repositories(rt)?,
            _ => return Err("the runtime is no longer thurbox".into()),
        };
        Ok((map, without_repositories(&text)))
    }
}

fn without_repositories(text: &str) -> Option<toml::Value> {
    let mut value: toml::Value = toml::from_str(text).ok()?;
    if let Some(rt) = value.get_mut("runtime").and_then(toml::Value::as_table_mut) {
        rt.remove("repositories");
    }
    Some(value)
}

/// `runtime.repositories`, each source an absolute path.
fn repositories(rt: &config::RuntimeConfig) -> Result<BTreeMap<String, String>, String> {
    for (url, source) in &rt.repositories {
        if !fleet_api::validate::is_absolute_path(source) {
            return Err(format!(
                "runtime.repositories: the source {source:?} of {url} must be an absolute path"
            ));
        }
    }
    Ok(rt.repositories.clone())
}

fn open_runtime(config: &Config) -> Result<Box<dyn Runtime>, Failure> {
    open_reloadable_runtime(config).map(|(runtime, _)| runtime)
}

/// The runtime, and for thurbox the handle to its repository map.
fn open_reloadable_runtime(
    config: &Config,
) -> Result<(Box<dyn Runtime>, Option<Repositories>), Failure> {
    let Some(rt) = &config.runtime else {
        return Err(Failure {
            code: exit::ERROR,
            message: "no runtime configured".into(),
            hint: Some(
                "set [runtime] kind = \"thurbox\" (or \"fake\" with a dir) in the config file"
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
            Ok((Box::new(FakeRuntime::new(host)), None))
        }
        "thurbox" => {
            let default_cli = if cfg!(windows) {
                "thurbox-cli.exe"
            } else {
                "thurbox-cli"
            };
            let mut cfg = ThurboxConfig::new(rt.cli.clone().unwrap_or_else(|| default_cli.into()));
            cfg.call_timeout = std::time::Duration::from_millis(
                rt.call_timeout_ms.unwrap_or(DEFAULT_CALL_TIMEOUT_MS),
            );
            if let Some(p) = &rt.key_prefix {
                if p.len() > 32 || fleet_api::validate::validate_name(p).is_err() || p.contains('.')
                {
                    return Err(fail(format!(
                        "runtime.keyPrefix {p:?}: use at most 32 lowercase letters, digits and '-'"
                    )));
                }
                cfg.key_prefix = p.clone();
            }
            cfg.model_env.extend(rt.model_env.clone());
            cfg.repositories = Repositories::new(repositories(rt).map_err(fail)?);
            cfg.cleanup = match rt.workspace_cleanup.as_deref() {
                None | Some("delete") => Cleanup::Delete,
                Some("keep") => Cleanup::Keep,
                Some(other) => {
                    return Err(fail(format!(
                        "runtime.workspaceCleanup {other:?}: use delete or keep"
                    )));
                }
            };
            for (harness, name) in &rt.workspace_trust {
                let Some(interface) = TrustInterface::parse(name) else {
                    return Err(fail(format!(
                        "runtime.workspaceTrust.{harness} {name:?}: use codex, claude or none"
                    )));
                };
                cfg.workspace_trust.insert(harness.clone(), interface);
            }
            let handle = cfg.repositories.clone();
            Ok((Box::new(ThurboxRuntime::new(cfg)), Some(handle)))
        }
        other => Err(fail(format!(
            "unknown runtime kind {other:?}; this build has thurbox and fake"
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

/// thurbox-cli and forge CLI calls are bounded at half the default lease.
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
            apply_conversation(store, gateway.as_ref().expect("opened"), section, obj)?
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
            apply_change_request(store, forge.as_ref().expect("opened"), obj)?
        } else {
            apply(store, obj, &policies)?
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
