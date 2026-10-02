//! Instance-scoped commands for a running local interface.

use clap::Subcommand;
use serde_json::{json, Value};
use std::io::{ErrorKind, Write};

use super::output::CommandOutput;
use super::{CommandError, EXIT_AMBIGUOUS};
use crate::ui_control::{self, Instance, Request};

pub fn stream(chosen: Option<String>, mut since: Option<u64>) -> Result<(), CommandError> {
    let instance = target(chosen)?;
    let mut stdout = std::io::stdout().lock();
    loop {
        let reply = ui_control::send(&instance, &Request::Watch { since }).map_err(|e| {
            CommandError::from(format!("UI instance {} is unavailable: {e}", instance.id))
        })?;
        let result = reply.result;
        if result.get("ok") == Some(&Value::Bool(false)) {
            return Err(result["error"]["message"]
                .as_str()
                .unwrap_or("UI watch failed")
                .to_string()
                .into());
        }
        match result["kind"].as_str() {
            Some("delta") => {
                for event in result["events"].as_array().into_iter().flatten() {
                    if !write_stream_line(&mut stdout, event)? {
                        return Ok(());
                    }
                }
            }
            _ => {
                if !write_stream_line(&mut stdout, &result)? {
                    return Ok(());
                }
            }
        }
        since = result["revision"].as_u64().or(Some(reply.revision));
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn write_stream_line(out: &mut impl Write, value: &Value) -> Result<bool, CommandError> {
    match writeln!(out, "{value}").and_then(|()| out.flush()) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(false),
        Err(error) => Err(error.to_string().into()),
    }
}

#[derive(Subcommand, Debug)]
pub enum Action {
    /// List reachable interface instances in this data profile.
    Instances,
    /// Read focus, selection and search state from the selected instance.
    State,
    /// Stream instance-scoped UI changes as JSON lines.
    Watch {
        /// Resume after this revision; an expired cursor yields a resync snapshot.
        #[arg(long)]
        since: Option<u64>,
        /// Return one batch, useful for polling clients.
        #[arg(long)]
        once: bool,
    },
    /// Apply one typed action and wait for its acknowledgment.
    #[command(name = "action")]
    Apply {
        /// `session.focus` or `search.open`.
        name: String,
        /// Session UUID for `session.focus`.
        #[arg(long)]
        session: Option<String>,
        /// Query to set for `search.open`.
        #[arg(long)]
        query: Option<String>,
    },
}

pub(super) fn target(chosen: Option<String>) -> Result<Instance, CommandError> {
    let instances = ui_control::instances().map_err(CommandError::from)?;
    let chosen = chosen.or_else(|| {
        std::env::var("THURBOX_UI_INSTANCE")
            .ok()
            .filter(|id| !id.is_empty())
    });
    if let Some(id) = chosen {
        return instances
            .into_iter()
            .find(|instance| instance.id == id)
            .ok_or_else(|| format!("UI instance {id} is closed or unavailable").into());
    }
    match instances.len() {
        0 => Err("no running UI instance is reachable".to_string().into()),
        1 => Ok(instances.into_iter().next().unwrap()),
        _ => Err(CommandError {
            message: format!(
                "ambiguous UI instance; select one with --instance: {}",
                instances
                    .iter()
                    .map(|instance| format!(
                        "{} ({}; pid {})",
                        instance.id, instance.label, instance.pid
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            exit_code: EXIT_AMBIGUOUS,
        }),
    }
}

pub fn run(instance: Option<String>, action: Action) -> Result<CommandOutput, CommandError> {
    if matches!(action, Action::Instances) {
        let entries = ui_control::instances().map_err(CommandError::from)?;
        let human = if entries.is_empty() {
            "No running UI instances".into()
        } else {
            entries
                .iter()
                .map(|row| {
                    format!(
                        "{}  {}  pid {}  {}",
                        row.id,
                        row.label,
                        row.pid,
                        row.terminal.as_deref().unwrap_or("terminal unknown")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        return Ok(CommandOutput::new(json!({"instances": entries}), human));
    }
    let instance = target(instance)?;
    let request = match action {
        Action::Instances => unreachable!(),
        Action::State => Request::State,
        Action::Watch { since, .. } => Request::Watch { since },
        Action::Apply {
            name,
            session,
            query,
        } => {
            let args: Value = match name.as_str() {
                "session.focus" if session.is_some() && query.is_none() => {
                    json!({"session_id": session.unwrap()})
                }
                "search.open" if query.is_some() && session.is_none() => {
                    json!({"query": query.unwrap()})
                }
                "session.focus" | "search.open" => {
                    return Err("action arguments do not match its schema"
                        .to_string()
                        .into())
                }
                _ => return Err(format!("unknown UI action {name}").into()),
            };
            Request::Action { name, args }
        }
    };
    let reply = ui_control::send(&instance, &request).map_err(|e| {
        CommandError::from(format!("UI instance {} is unavailable: {e}", instance.id))
    })?;
    if reply.result.get("ok") == Some(&Value::Bool(false)) {
        let message = reply.result["error"]["message"]
            .as_str()
            .unwrap_or("UI action refused");
        return Err(message.to_string().into());
    }
    let output = match request {
        Request::State => reply.result,
        Request::Watch { .. } => reply.result,
        Request::Action { .. } => {
            json!({"instance_id": reply.instance_id, "request_id": reply.request_id, "revision": reply.revision, "result": reply.result})
        }
        Request::Ping => unreachable!(),
    };
    Ok(CommandOutput::new(
        output.clone(),
        serde_json::to_string_pretty(&output).unwrap_or_default(),
    ))
}
