//! The home view: what `thurbox-cli` prints when it is given no subcommand.
//!
//! AXI principle 8 ("content first") asks a bare invocation for live,
//! actionable data rather than a usage manual — an agent that runs a tool to
//! find out what it can do should learn the state of the world in the same
//! breath. So this answers *what is going on right now*: every session with the
//! `state` its hooks last reported, the calling session's unread mail, and the
//! counts that would otherwise cost three more invocations to assemble
//! (principle 4, "pre-computed aggregates").
//!
//! It is deliberately cheap — six SQLite reads, no tmux, no network. It is
//! not quite write-free: reading the agent registry seeds `agents.toml` (and
//! its directory) when a machine has none, the same first-run seeding every
//! other entrypoint performs. A command an agent runs to orient itself must
//! not be one it has to think twice about running.

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
    let parked = db.load_stopped_sessions().unwrap_or_default();
    let registry = crate::agent::agent_config::load_or_seed();

    // The rows are trimmed to the four fields that let an agent decide what to
    // look at next (principle 2). `session list --json` is still the full
    // record; this view has no compatibility surface to preserve, so it carries
    // only what it is for.
    //
    // `state` is `Assessment::state_word`, under the same key and from the same
    // owner `session list` publishes — so the two surfaces cannot disagree
    // about a row, or make an agent learn two names for one fact, and a session
    // that has never said anything reads `uncovered`/`unreported` rather than
    // being laundered into `idle`. The pane is deliberately not probed
    // (`probe = false`): a bare invocation touches no multiplexer.
    let rows: Vec<Value> = sessions
        .iter()
        .map(|s| {
            let hook = crate::cli::sessions::assess(&registry, s, &states, &parked, false);
            json!({
                "name": s.name,
                "agent": s.agent,
                "state": hook.state_word(),
                "id": s.id.to_string(),
            })
        })
        .collect();

    let mut totals = serde_json::Map::new();
    totals.insert("sessions".into(), json!(sessions.len()));
    // Every state present is counted, the two silences included: a session
    // nothing can report for is exactly the one an agent orienting itself needs
    // told about, and rolling it into an unlisted `idle` hid it.
    for state in [
        "working",
        "blocked",
        "done",
        "idle",
        crate::session::STATE_RUNNING,
        crate::session::STATE_UNCOVERED,
        crate::session::STATE_UNREPORTED,
        crate::session::STATE_STOPPED,
    ] {
        let n = rows.iter().filter(|r| r["state"] == json!(state)).count();
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
            // The note is measured against `sessions`, not the document: the
            // document always carries `bin`/`description`/`totals` and so is
            // never empty, but a machine with no sessions still has to say so
            // rather than print a lone header.
            .collection("sessions")
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
    if rows.iter().any(|r| r["state"] == json!("blocked")) {
        help.push(
            "thurbox-cli session capture <id>   see what a blocked agent is waiting on".to_string(),
        );
    }
    // A row nothing can report for looks calm and is simply unknown. Naming the
    // diagnostic is the difference between reading that as "at rest" and asking.
    if rows
        .iter()
        .any(|r| r["state"] == json!(crate::session::STATE_UNCOVERED))
    {
        help.push("thurbox-cli session doctor   why a session reports no state at all".to_string());
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
        &["NAME", "AGENT", "STATE", "ID"],
        &rows
            .iter()
            .map(|r| {
                ["name", "agent", "state", "id"]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionId, STATE_UNCOVERED, STATE_UNREPORTED};
    use crate::sync::SharedSession;

    fn session(name: &str, agent: &str) -> SharedSession {
        SharedSession {
            id: SessionId::default(),
            name: name.into(),
            agent: agent.into(),
            backend_id: String::new(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        }
    }

    /// What `session list` publishes as this session's `state`.
    fn listed_state(db: &Database, name: &str) -> String {
        let out = crate::cli::sessions::run(
            crate::cli::sessions::Action::List {
                parent: None,
                deleted: false,
                verify: false,
            },
            db,
        )
        .unwrap();
        out.json
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == json!(name))
            .unwrap_or_else(|| panic!("{name} not listed"))["state"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// What the bare home view publishes as the same session's `state` — the
    /// same key `session list` uses, which is half of the two surfaces
    /// agreeing.
    fn home_state(db: &Database, name: &str) -> String {
        let out = run(db).unwrap();
        out.json["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == json!(name))
            .unwrap_or_else(|| panic!("{name} not in the home view"))["state"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn home_and_session_list_agree_on_every_shape_of_silence() {
        let tmp = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());
        let db = Database::open_in_memory().unwrap();

        // Never reported, but its agent's hooks could have: `unreported`.
        let quiet = session("quiet", "claude");
        db.upsert_session(&quiet).unwrap();
        // An agent thurbox ships no hooks for, and nothing has signalled:
        // `uncovered`. Reading this as `idle` is the conflation the assessment
        // exists to remove, and it is what the home view used to print.
        let foreign = session("foreign", "mine-own-cli");
        db.upsert_session(&foreign).unwrap();
        // An agent that has actually reported.
        let busy = session("busy", "claude");
        db.upsert_session(&busy).unwrap();
        db.set_hook_state(busy.id, "blocked").unwrap();

        for (name, want) in [
            ("quiet", STATE_UNREPORTED),
            ("foreign", STATE_UNCOVERED),
            ("busy", "blocked"),
        ] {
            assert_eq!(home_state(&db, name), want, "home view for {name}");
            assert_eq!(listed_state(&db, name), want, "session list for {name}");
        }
    }

    #[test]
    fn a_never_reported_session_is_counted_rather_than_folded_into_idle() {
        let tmp = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());
        let db = Database::open_in_memory().unwrap();
        db.upsert_session(&session("foreign", "mine-own-cli"))
            .unwrap();

        let out = run(&db).unwrap();
        assert_eq!(out.json["totals"][STATE_UNCOVERED], json!(1));
        assert_eq!(out.json["totals"]["idle"], Value::Null);
        // And the help names the diagnostic, since the row looks calm and is
        // simply unknown.
        assert!(
            out.agent.help.iter().any(|h| h.contains("session doctor")),
            "{:?}",
            out.agent.help
        );
    }

    #[test]
    fn a_machine_with_no_sessions_says_so_rather_than_printing_a_lone_header() {
        let tmp = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(tmp.path());
        let db = Database::open_in_memory().unwrap();
        let out = run(&db).unwrap();
        let rendered = crate::cli::output::Format::Toon.render(&out);
        assert!(
            rendered.contains("note: 0 sessions on this machine"),
            "{rendered}"
        );
    }
}
