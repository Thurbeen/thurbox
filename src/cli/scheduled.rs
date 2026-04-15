//! Scheduled-command CRUD subcommands.

use clap::Subcommand;
use serde_json::{json, Value};

use crate::session::{ScheduledCommand, SessionId};
use crate::storage::Database;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// Schedule a command.
    Create {
        /// Session UUID.
        #[arg(long)]
        session: String,
        /// Unix-millisecond timestamp at which to send.
        #[arg(long)]
        at: u64,
        /// Command text to type into the terminal.
        #[arg(long)]
        command: String,
    },
    /// List pending commands (optionally filtered by session).
    List {
        /// Optional session UUID filter.
        #[arg(long)]
        session: Option<String>,
    },
    /// Get a scheduled command by ID.
    Get {
        /// Command ID.
        id: i64,
    },
    /// Cancel a pending scheduled command.
    Cancel {
        /// Command ID.
        id: i64,
    },
}

pub fn run(action: Action, db: &Database) -> Result<Value, String> {
    match action {
        Action::Create {
            session,
            at,
            command,
        } => {
            let session_id: SessionId = session
                .parse()
                .map_err(|_| format!("Invalid session UUID: {session}"))?;
            let info = db
                .get_session_by_id(session_id)
                .map_err(|e| format!("get_session_by_id: {e}"))?
                .ok_or_else(|| format!("Session not found: {session}"))?;
            if command.is_empty() {
                return Err("command must not be empty".into());
            }
            let now = crate::sync::current_time_millis();
            if at <= now {
                return Err("at must be in the future (unix millis)".into());
            }
            let id = db
                .create_scheduled_command(session_id, &command, at)
                .map_err(|e| format!("create_scheduled_command: {e}"))?;
            let db_path = crate::paths::database_file()
                .ok_or_else(|| "Cannot resolve database path".to_string())?;
            let delay_secs = at.saturating_sub(now) / 1000;
            if let Err(e) = crate::agent::tmux::schedule_tmux_command(
                &info.name, &command, delay_secs, id, &db_path,
            ) {
                eprintln!("warning: failed to set tmux timer for command {id}: {e}");
            }
            let cmd = ScheduledCommand {
                id,
                session_id,
                command_text: command,
                scheduled_at: at,
                created_at: now,
                executed_at: None,
                cancelled_at: None,
            };
            Ok(scheduled_to_json(&cmd))
        }
        Action::List { session } => {
            let cmds = db
                .list_pending_scheduled_commands()
                .map_err(|e| format!("list_pending_scheduled_commands: {e}"))?;
            let filtered: Vec<_> = if let Some(s) = session {
                let id: SessionId = s
                    .parse()
                    .map_err(|_| format!("Invalid session UUID: {s}"))?;
                cmds.into_iter().filter(|c| c.session_id == id).collect()
            } else {
                cmds
            };
            Ok(Value::Array(
                filtered.iter().map(scheduled_to_json).collect(),
            ))
        }
        Action::Get { id } => {
            let cmd = db
                .get_scheduled_command(id)
                .map_err(|e| format!("get_scheduled_command: {e}"))?
                .ok_or_else(|| format!("Scheduled command not found: {id}"))?;
            Ok(scheduled_to_json(&cmd))
        }
        Action::Cancel { id } => match db.cancel_scheduled_command(id) {
            Ok(true) => Ok(json!({ "cancelled": true, "id": id })),
            Ok(false) => Err("Command not found or already executed/cancelled".into()),
            Err(e) => Err(format!("cancel_scheduled_command: {e}")),
        },
    }
}

fn scheduled_to_json(cmd: &ScheduledCommand) -> Value {
    let status = if cmd.cancelled_at.is_some() {
        "cancelled"
    } else if cmd.executed_at.is_some() {
        "executed"
    } else {
        "pending"
    };
    json!({
        "id": cmd.id,
        "session_id": cmd.session_id.to_string(),
        "command_text": cmd.command_text,
        "scheduled_at": cmd.scheduled_at,
        "created_at": cmd.created_at,
        "status": status,
    })
}
