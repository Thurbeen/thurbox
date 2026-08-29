//! The home view: what `thurbox-cli` prints when it is given no subcommand.
//!
//! AXI principle 8 ("content first") asks a bare invocation for live,
//! actionable data rather than a usage manual — an agent that runs a tool to
//! find out what it can do should learn the state of the world in the same
//! breath. So this answers *what is going on right now*: every session with the
//! status its hooks last reported, the calling session's unread mail, and the
//! counts that would otherwise cost three more invocations to assemble
//! (principle 4, "pre-computed aggregates").
//!
//! It is deliberately cheap and read-only — four SQLite reads, no tmux, no
//! network, no writes. A command an agent runs to orient itself must not be
//! one it has to think twice about running.

use serde_json::{json, Value};

use crate::cli::output::CommandOutput;
use crate::storage::Database;

/// One sentence on what this binary is for, printed above the state (AXI
/// principle 8 asks for the executable's path and its purpose, so that an
/// agent handed only the output knows what produced it).
const DESCRIPTION: &str =
    "orchestrates coding-agent CLI sessions in persistent tmux panels — sessions, \
tasks, automations and the interface, without the TUI";

/// Build the home view.
pub fn run(db: &Database) -> Result<CommandOutput, String> {
    let sessions = db
        .list_active_sessions()
        .map_err(|e| format!("list_active_sessions: {e}"))?;
    let states = db.load_hook_states().unwrap_or_default();

    // The rows are trimmed to the four fields that let an agent decide what to
    // look at next (principle 2). `session list --json` is still the full
    // record; this view has no compatibility surface to preserve, so it carries
    // only what it is for.
    let rows: Vec<Value> = sessions
        .iter()
        .map(|s| {
            let status = states
                .get(&s.id)
                .and_then(|r| r.state.as_deref())
                .unwrap_or("idle");
            json!({
                "name": s.name,
                "agent": s.agent,
                "status": status,
                "id": s.id.to_string(),
            })
        })
        .collect();

    let mut totals = serde_json::Map::new();
    totals.insert("sessions".into(), json!(sessions.len()));
    for state in ["working", "blocked", "done"] {
        let n = rows.iter().filter(|r| r["status"] == json!(state)).count();
        if n > 0 {
            totals.insert(state.into(), json!(n));
        }
    }

    let tasks = db.list_tasks().unwrap_or_default();
    let open = tasks
        .iter()
        .filter(|t| !matches!(t.status, crate::session::TaskStatus::Done))
        .count();
    if open > 0 {
        totals.insert("tasks_open".into(), json!(open));
    }

    let automations = db.list_automations().unwrap_or_default();
    let enabled = automations.iter().filter(|a| a.enabled).count();
    if enabled > 0 {
        totals.insert("automations_enabled".into(), json!(enabled));
    }

    // Mail is addressed to a session, so it is only a fact when this
    // invocation is running inside one.
    let calling = crate::cli::identity::calling_session(db);
    let unread = calling
        .as_ref()
        .and_then(|s| db.count_unread_messages(s.id).ok())
        .unwrap_or(0);
    if unread > 0 {
        totals.insert("unread_messages".into(), json!(unread));
    }

    let mut document = serde_json::Map::new();
    document.insert("bin".into(), json!(executable()));
    document.insert("description".into(), json!(DESCRIPTION));
    document.insert("sessions".into(), Value::Array(rows.clone()));
    document.insert("totals".into(), Value::Object(totals));
    if let Some(session) = &calling {
        document.insert("calling_session".into(), json!(session.name));
    }

    Ok(
        CommandOutput::new(Value::Object(document), human(&rows, unread))
            .help(suggestions(&rows, unread, calling.is_some()))
            // The document is an object and so never empty, but a run with nothing
            // in it still has to say so rather than print a lone header.
            .empty("0 sessions on this machine — `thurbox-cli session create` starts one"),
    )
}

/// This binary's path, for an agent that has to tell someone how to rerun it.
/// Falls back to the bare name when the platform will not say (`argv[0]` is not
/// guaranteed to be a path).
fn executable() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "thurbox-cli".to_string())
}

/// The next steps this state makes sensible (AXI principle 9). Runtime values
/// stay as `<id>` placeholders rather than being guessed at.
fn suggestions(rows: &[Value], unread: usize, inside_session: bool) -> Vec<String> {
    let mut help = Vec::new();
    if unread > 0 {
        help.push(format!(
            "thurbox-cli message inbox --claim   read and claim your {unread} unread message(s)"
        ));
    }
    if rows.iter().any(|r| r["status"] == json!("blocked")) {
        help.push(
            "thurbox-cli session capture <id>   see what a blocked agent is waiting on".to_string(),
        );
    }
    if rows.is_empty() {
        help.push(
            "thurbox-cli session create --name <name> --repo-path <path>   start a session"
                .to_string(),
        );
    } else {
        help.push("thurbox-cli session list   every session, with branch and cwd".to_string());
    }
    if inside_session {
        help.push(
            "thurbox-cli message send --to <id> --kind result --body <text>   hand work back"
                .to_string(),
        );
    }
    help.push("thurbox-cli <command> --help   flags and examples for one command".to_string());
    help
}

/// The terminal rendering: the same facts, laid out for a person.
fn human(rows: &[Value], unread: usize) -> String {
    if rows.is_empty() {
        return "No active sessions. `thurbox-cli session create --name <name> --repo-path <path>` starts one.".to_string();
    }
    let table = crate::cli::output::table(
        &["NAME", "AGENT", "STATUS", "ID"],
        &rows
            .iter()
            .map(|r| {
                ["name", "agent", "status", "id"]
                    .iter()
                    .map(|k| r[*k].as_str().unwrap_or("-").to_string())
                    .collect()
            })
            .collect::<Vec<_>>(),
    );
    let mut out = format!("{} session(s)\n{table}", rows.len());
    if unread > 0 {
        out.push_str(&format!(
            "\n\n{unread} unread message(s) — `thurbox-cli message inbox --claim`"
        ));
    }
    out
}
