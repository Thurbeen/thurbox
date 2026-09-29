//! Native delivery of a mailbox message into the recipient agent's own inbox.
//!
//! `message send` used to type the word `inbox` into the recipient's pane and
//! hope the agent drained its mailbox. The body never travelled that way — only
//! a nudge did, by keystroke injection — and every consumer had to guess from
//! screen contents whether typing was safe. Both agents thurbox runs most have a
//! real inbox instead, and this module hands the *body* to it:
//!
//! - **Claude Code** binds a Unix socket per session and exports its path to
//!   hooks as `CLAUDE_CODE_MESSAGING_SOCKET`. One JSON line on it is read
//!   between tool calls mid-turn, or starts a new turn when the session is idle.
//! - **Codex** queues a message on a thread through its app-server daemon with
//!   `codex queue --thread <id>`; it starts a turn when idle and runs as the
//!   next turn when one is in flight.
//!
//! Anything else keeps the message in the mailbox only. Nothing here touches the
//! pane: there is no keystroke fallback, by design (`tests/architecture_rules.rs`
//! keeps the multiplexer out of this path).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::session::SessionMessage;
use crate::storage::Database;
use crate::sync::SharedSession;

/// Session-meta key holding the Claude inbox socket, captured from the agent's
/// own hook environment by `session signal` (see [`remember_claude_socket`]).
pub(crate) const CLAUDE_SOCKET_META: &str = "thurbox.claude_messaging_socket";

/// Session-meta key `session bind-codex` records the Codex thread under.
const CODEX_THREAD_META: &str = "thurbox.codex_conversation_id";

/// The env var Claude Code exports to hooks and its Bash tool.
const CLAUDE_SOCKET_ENV: &str = "CLAUDE_CODE_MESSAGING_SOCKET";

/// A `codex queue` that has not returned by now is not going to; the message is
/// still in the mailbox, so giving up costs timeliness, not the message.
const CODEX_QUEUE_TIMEOUT: Duration = Duration::from_secs(20);

/// Claude Code closes a connection without a complete line within 30 s; a
/// local socket write that blocks this long means the reader is wedged.
const SOCKET_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// Which inbox carried a message. Reported as `delivered_via` and stored on the
/// row for the native two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveredVia {
    ClaudeSocket,
    CodexQueue,
    /// Not handed to the agent: the body waits for `message inbox`.
    Mailbox,
}

impl DeliveredVia {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeSocket => "claude-socket",
            Self::CodexQueue => "codex-queue",
            Self::Mailbox => "mailbox",
        }
    }
}

/// One way to reach the recipient's agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Route {
    ClaudeSocket(PathBuf),
    CodexQueue(String),
}

impl Route {
    fn via(&self) -> DeliveredVia {
        match self {
            Self::ClaudeSocket(_) => DeliveredVia::ClaudeSocket,
            Self::CodexQueue(_) => DeliveredVia::CodexQueue,
        }
    }
}

/// What is known about a recipient that could reach its agent.
///
/// The agent is *detected* from this rather than read off the row's agent name:
/// a registry entry is a name (`claude-coder`, `flow`) whose command may be a
/// wrapper script, while a captured socket or a bound Codex thread is the agent
/// having announced itself.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Evidence {
    /// The session runs on another machine, where its agent's inbox is.
    pub remote: bool,
    /// The socket the agent's own hooks reported (`session signal`).
    pub captured_socket: Option<PathBuf>,
    /// Sockets of live interactive Claude sessions registered against the
    /// recipient's pane, newest first.
    pub registry_sockets: Vec<PathBuf>,
    /// The Codex thread `session bind-codex` recorded.
    pub codex_thread: Option<String>,
}

/// The routes to try, in order — or why there are none.
///
/// Claude comes first because its evidence is proof of a live process (a
/// socket exists only while its session runs), where a Codex thread id outlives
/// the process that opened it. Both Claude sockets are kept: a captured one can
/// be stale after a restart that has not yet re-run a hook, and the registry is
/// the fallback for sessions started before capture existed.
pub(crate) fn routes(evidence: &Evidence) -> Result<Vec<Route>, String> {
    if evidence.remote {
        return Err(
            "the session runs on a remote host, whose agent inbox is not \
                    reachable from here"
                .into(),
        );
    }
    let mut routes: Vec<Route> = Vec::new();
    for socket in evidence
        .captured_socket
        .iter()
        .chain(&evidence.registry_sockets)
    {
        let route = Route::ClaudeSocket(socket.clone());
        if !routes.contains(&route) {
            routes.push(route);
        }
    }
    if let Some(thread) = &evidence.codex_thread {
        routes.push(Route::CodexQueue(thread.clone()));
    }
    if routes.is_empty() {
        return Err(
            "no agent-native inbox is known for this session: no Claude inbox \
             socket, no bound Codex thread"
                .into(),
        );
    }
    Ok(routes)
}

/// Collect [`Evidence`] for `recipient` from the database and Claude's session
/// registry.
pub(crate) fn gather(db: &Database, recipient: &SharedSession) -> Evidence {
    if crate::session::is_remote_backend(&recipient.backend_type) {
        return Evidence {
            remote: true,
            ..Evidence::default()
        };
    }
    let meta = |key| db.get_session_meta(recipient.id, key).ok().flatten();
    Evidence {
        remote: false,
        captured_socket: meta(CLAUDE_SOCKET_META)
            .map(PathBuf::from)
            .filter(|p| is_socket(p)),
        registry_sockets: claude_registry_dir()
            .map(|dir| registry_sockets(&dir, &recipient.backend_id))
            .unwrap_or_default(),
        codex_thread: meta(CODEX_THREAD_META).filter(|id| uuid::Uuid::parse_str(id).is_ok()),
    }
}

/// `~/.claude/sessions`, or under `$CLAUDE_CONFIG_DIR` when Claude Code was
/// pointed elsewhere.
fn claude_registry_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => crate::paths::home_dir()?.join(".claude"),
    };
    Some(base.join("sessions"))
}

/// Inbox sockets of the interactive Claude sessions whose registry entry names
/// `pane` (thurbox's `backend_id`, `%N`), newest first.
///
/// Each running Claude Code writes `<pid>.json` with a `tmux` field of
/// `<session>:@<window>.%<pane>`. The pane id is matched rather than the
/// conversation id, because thurbox's `agent_session_id` drifts from Claude's
/// after a resume. An entry outlives a crashed process, so only a path that is
/// still a socket counts — and a stale one fails the connect and is skipped.
fn registry_sockets(dir: &Path, pane: &str) -> Vec<PathBuf> {
    if !pane.starts_with('%') {
        return Vec::new();
    }
    let suffix = format!(".{pane}");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(i64, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|raw| serde_json::from_str::<Value>(&raw).ok())
        .filter(|v| v["tmux"].as_str().is_some_and(|t| t.ends_with(&suffix)))
        // A `claude -p` run from inside the pane registers against it too;
        // only the interactive session is the one the pane shows.
        // `map_or(true, ..)`: `is_none_or` postdates the 1.75 MSRV.
        .filter(|v| v["kind"].as_str().map_or(true, |k| k == "interactive"))
        .filter_map(|v| {
            let socket = PathBuf::from(v["messagingSocketPath"].as_str()?);
            is_socket(&socket).then(|| (v["startedAt"].as_i64().unwrap_or(0), socket))
        })
        .collect();
    found.sort_by_key(|(started, _)| std::cmp::Reverse(*started));
    found.into_iter().map(|(_, socket)| socket).collect()
}

#[cfg(unix)]
fn is_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(path).is_ok_and(|m| m.file_type().is_socket())
}

#[cfg(not(unix))]
fn is_socket(_path: &Path) -> bool {
    false
}

/// Record the Claude inbox socket of the calling agent, from the environment
/// Claude Code gives its hooks. Called by `session signal`, which every thurbox
/// Claude hook runs — so a session started before this existed is captured on
/// its next tool call, and a restarted one on its `SessionStart`.
///
/// A `claude -p` launched from inside the pane would report its own socket
/// here; that socket disappears when it exits, [`gather`] drops a path that is
/// no longer a socket, and the interactive session's next hook writes its own
/// back.
pub(crate) fn remember_claude_socket(db: &Database, session: &SharedSession) {
    let Some(socket) = std::env::var(CLAUDE_SOCKET_ENV)
        .ok()
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    let stored = db.get_session_meta(session.id, CLAUDE_SOCKET_META);
    if matches!(stored, Ok(Some(ref s)) if *s == socket) {
        return;
    }
    if let Err(e) = db.set_session_meta(session.id, CLAUDE_SOCKET_META, &socket) {
        tracing::warn!("could not record the Claude inbox socket: {e}");
    }
}

/// The side effects of delivery, separated so tests can observe them.
pub(crate) trait Transport {
    fn post_to_claude(&self, socket: &Path, text: &str) -> Result<(), String>;
    fn queue_to_codex(&self, thread: &str, text: &str) -> Result<(), String>;
}

/// The real transports: a Unix socket write and a `codex queue` process.
pub(crate) struct Native;

impl Transport for Native {
    #[cfg(unix)]
    fn post_to_claude(&self, socket: &Path, text: &str) -> Result<(), String> {
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        // No auth line: the token is optional on macOS/Linux, where the socket
        // is already mode 0600 to this user. Opened only now, with the payload
        // ready, because Claude Code drops a connection idle for 30 s.
        let mut stream = UnixStream::connect(socket).map_err(|e| format!("connect: {e}"))?;
        stream
            .set_write_timeout(Some(SOCKET_WRITE_TIMEOUT))
            .map_err(|e| format!("set timeout: {e}"))?;
        stream
            .write_all(claude_envelope(text).as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|e| format!("write: {e}"))?;
        let _ = stream.shutdown(std::net::Shutdown::Write);
        Ok(())
    }

    #[cfg(not(unix))]
    fn post_to_claude(&self, _socket: &Path, _text: &str) -> Result<(), String> {
        Err("Claude's inbox is a named pipe on this platform, which is not supported".into())
    }

    fn queue_to_codex(&self, thread: &str, text: &str) -> Result<(), String> {
        use std::process::{Command, Stdio};

        let mut child = Command::new("codex")
            .args(["queue", "--thread", thread, "--message", text])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn codex: {e}"))?;
        let deadline = std::time::Instant::now() + CODEX_QUEUE_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "codex queue did not return within {}s",
                        CODEX_QUEUE_TIMEOUT.as_secs()
                    ));
                }
                Err(e) => return Err(format!("wait for codex: {e}")),
            }
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("codex output: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(format!(
            "codex queue exited {}: {}",
            out.status
                .code()
                .map_or_else(|| "on a signal".to_string(), |c| c.to_string()),
            stderr.lines().next().unwrap_or("").trim()
        ))
    }
}

/// The two-line-shaped stream-json message Claude Code's inbox reads: a user
/// turn, newline-terminated.
fn claude_envelope(text: &str) -> String {
    let line = json!({
        "type": "user",
        "message": { "role": "user", "content": text },
    });
    format!("{line}\n")
}

/// The text handed to the agent: one provenance line, then the body verbatim.
///
/// Deliberately no instruction to run `message inbox`: the body *is* the
/// delivery, and the row is marked read so a drain does not repeat it.
pub(crate) fn delivery_text(message: &SessionMessage, sender: Option<&str>) -> String {
    format!(
        "[thurbox message #{} · kind: {} · from: {}]\n\n{}",
        message.id,
        message.kind,
        sender.unwrap_or("unknown sender"),
        message.body
    )
}

/// How a delivery went, for the command output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub via: DeliveredVia,
    /// Why the message stayed in the mailbox; `None` once delivered natively.
    pub note: Option<String>,
}

impl Outcome {
    fn mailbox(why: impl Into<String>) -> Self {
        Self {
            via: DeliveredVia::Mailbox,
            note: Some(why.into()),
        }
    }
}

/// Deliver an enqueued `message` to its recipient's agent-native inbox.
///
/// Never fails the send: the row is already durable, so every failure path
/// ends at [`DeliveredVia::Mailbox`] with the reason, logged at `warn` when a
/// native attempt was made and did not land.
pub(crate) fn deliver(
    db: &Database,
    message: &SessionMessage,
    text: &str,
    evidence: &Evidence,
    transport: &dyn Transport,
) -> Outcome {
    let routes = match routes(evidence) {
        Ok(routes) => routes,
        Err(why) => return Outcome::mailbox(why),
    };
    let mut failures = Vec::new();
    for route in routes {
        let via = route.via().as_str();
        match db.reserve_message_delivery(message.id, via) {
            Ok(true) => {}
            Ok(false) => {
                return Outcome::mailbox(
                    "the recipient drained it from the mailbox before native delivery",
                )
            }
            Err(e) => {
                tracing::warn!("message #{}: reserve for {via}: {e}", message.id);
                return Outcome::mailbox(format!("could not mark the row for delivery: {e}"));
            }
        }
        let sent = match &route {
            Route::ClaudeSocket(socket) => transport.post_to_claude(socket, text),
            Route::CodexQueue(thread) => transport.queue_to_codex(thread, text),
        };
        match sent {
            Ok(()) => {
                return Outcome {
                    via: route.via(),
                    note: None,
                }
            }
            Err(e) => {
                let target = match &route {
                    Route::ClaudeSocket(socket) => socket.display().to_string(),
                    Route::CodexQueue(thread) => format!("thread {thread}"),
                };
                tracing::warn!(
                    "message #{}: {via} delivery to {target} failed: {e}",
                    message.id
                );
                failures.push(format!("{via} ({target}): {e}"));
                if let Err(e) = db.release_message_delivery(message.id, via) {
                    tracing::warn!("message #{}: release after failed {via}: {e}", message.id);
                }
            }
        }
    }
    Outcome::mailbox(format!("native delivery failed: {}", failures.join("; ")))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::session::SessionId;
    use crate::storage::messages::NewMessage;

    /// Records every send and fails the routes it is told to.
    #[derive(Default)]
    struct Fake {
        fail_claude: bool,
        fail_codex: bool,
        sent: RefCell<Vec<(String, String)>>,
    }

    impl Transport for Fake {
        fn post_to_claude(&self, socket: &Path, text: &str) -> Result<(), String> {
            self.sent
                .borrow_mut()
                .push((format!("claude:{}", socket.display()), text.into()));
            if self.fail_claude {
                Err("connect: refused".into())
            } else {
                Ok(())
            }
        }
        fn queue_to_codex(&self, thread: &str, text: &str) -> Result<(), String> {
            self.sent
                .borrow_mut()
                .push((format!("codex:{thread}"), text.into()));
            if self.fail_codex {
                Err("codex queue exited 1".into())
            } else {
                Ok(())
            }
        }
    }

    fn enqueued(db: &Database) -> SessionMessage {
        let id = db
            .enqueue_message(&NewMessage {
                to_session_id: SessionId::default(),
                from_session_id: None,
                from_task_id: None,
                kind: "result".into(),
                body: "the body".into(),
            })
            .unwrap();
        db.get_message(id).unwrap().unwrap()
    }

    fn claude(path: &str) -> Evidence {
        Evidence {
            captured_socket: Some(path.into()),
            ..Evidence::default()
        }
    }

    const THREAD: &str = "01a0eeb5-c968-7f03-aed6-37b54c6586e9";

    fn codex() -> Evidence {
        Evidence {
            codex_thread: Some(THREAD.into()),
            ..Evidence::default()
        }
    }

    #[test]
    fn claude_evidence_selects_the_socket() {
        assert_eq!(
            routes(&claude("/tmp/cc-socks/1.sock")).unwrap(),
            vec![Route::ClaudeSocket("/tmp/cc-socks/1.sock".into())]
        );
    }

    #[test]
    fn codex_evidence_selects_the_queue() {
        assert_eq!(
            routes(&codex()).unwrap(),
            vec![Route::CodexQueue(THREAD.into())]
        );
    }

    #[test]
    fn captured_socket_is_tried_before_the_registry_and_not_twice() {
        let evidence = Evidence {
            captured_socket: Some("/s/a.sock".into()),
            registry_sockets: vec!["/s/b.sock".into(), "/s/a.sock".into()],
            ..Evidence::default()
        };
        assert_eq!(
            routes(&evidence).unwrap(),
            vec![
                Route::ClaudeSocket("/s/a.sock".into()),
                Route::ClaudeSocket("/s/b.sock".into()),
            ]
        );
    }

    #[test]
    fn no_evidence_or_a_remote_session_has_no_route() {
        assert!(routes(&Evidence::default())
            .unwrap_err()
            .contains("no agent-native inbox"));
        let remote = Evidence {
            remote: true,
            ..claude("/s/a.sock")
        };
        assert!(routes(&remote).unwrap_err().contains("remote host"));
    }

    #[test]
    fn success_marks_the_row_delivered_and_read() {
        let db = Database::open_in_memory().unwrap();
        let msg = enqueued(&db);
        let fake = Fake::default();
        let out = deliver(&db, &msg, "text", &claude("/s/a.sock"), &fake);
        assert_eq!(out.via, DeliveredVia::ClaudeSocket);
        assert_eq!(out.note, None);
        let row = db.get_message(msg.id).unwrap().unwrap();
        assert_eq!(row.delivered_via.as_deref(), Some("claude-socket"));
        assert!(!row.is_unread(), "a drain must not hand it over again");
        assert_eq!(fake.sent.borrow().len(), 1);
    }

    #[test]
    fn failure_leaves_the_row_unread_and_undelivered() {
        let db = Database::open_in_memory().unwrap();
        let msg = enqueued(&db);
        let fake = Fake {
            fail_codex: true,
            ..Fake::default()
        };
        let out = deliver(&db, &msg, "text", &codex(), &fake);
        assert_eq!(out.via, DeliveredVia::Mailbox);
        assert!(out.note.unwrap().contains("codex queue exited 1"));
        let row = db.get_message(msg.id).unwrap().unwrap();
        assert_eq!(row.delivered_via, None);
        assert!(row.is_unread());
    }

    #[test]
    fn a_stale_socket_falls_through_to_the_next_route() {
        let db = Database::open_in_memory().unwrap();
        let msg = enqueued(&db);
        let fake = Fake {
            fail_claude: true,
            ..Fake::default()
        };
        let evidence = Evidence {
            codex_thread: Some(THREAD.into()),
            ..claude("/s/stale.sock")
        };
        let out = deliver(&db, &msg, "text", &evidence, &fake);
        assert_eq!(out.via, DeliveredVia::CodexQueue);
        let row = db.get_message(msg.id).unwrap().unwrap();
        assert_eq!(row.delivered_via.as_deref(), Some("codex-queue"));
    }

    #[test]
    fn no_route_sends_nothing() {
        let db = Database::open_in_memory().unwrap();
        let msg = enqueued(&db);
        let fake = Fake::default();
        let out = deliver(&db, &msg, "text", &Evidence::default(), &fake);
        assert_eq!(out.via, DeliveredVia::Mailbox);
        assert!(fake.sent.borrow().is_empty());
        assert!(db.get_message(msg.id).unwrap().unwrap().is_unread());
    }

    #[test]
    fn an_already_drained_message_is_not_delivered_again() {
        let db = Database::open_in_memory().unwrap();
        let msg = enqueued(&db);
        db.claim_messages(msg.to_session_id, None).unwrap();
        let fake = Fake::default();
        let out = deliver(&db, &msg, "text", &claude("/s/a.sock"), &fake);
        assert_eq!(out.via, DeliveredVia::Mailbox);
        assert!(fake.sent.borrow().is_empty());
    }

    #[test]
    fn delivery_text_is_provenance_then_the_body_verbatim() {
        let db = Database::open_in_memory().unwrap();
        let msg = enqueued(&db);
        let text = delivery_text(&msg, Some("coder-x"));
        let (head, body) = text.split_once("\n\n").unwrap();
        assert_eq!(
            head,
            format!(
                "[thurbox message #{} · kind: result · from: coder-x]",
                msg.id
            )
        );
        assert_eq!(body, "the body");
        assert!(!text.contains("message inbox"));
    }

    #[test]
    fn claude_envelope_is_one_user_line() {
        let line = claude_envelope("a \"quoted\"\nbody");
        assert!(line.ends_with('\n') && line.matches('\n').count() == 1);
        let v: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"], "a \"quoted\"\nbody");
    }

    #[cfg(unix)]
    #[test]
    fn native_post_writes_the_envelope_to_the_socket() {
        use std::io::Read;
        use std::os::unix::net::UnixListener;

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("in.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let reader = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut got = String::new();
            conn.read_to_string(&mut got).unwrap();
            got
        });
        Native.post_to_claude(&path, "hello").unwrap();
        assert_eq!(reader.join().unwrap(), claude_envelope("hello"));
    }

    #[cfg(unix)]
    #[test]
    fn native_post_to_a_dead_socket_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("gone.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(Native.post_to_claude(&path, "hello").is_err());
    }

    #[test]
    fn remember_records_the_hook_socket() {
        let db = Database::open_in_memory().unwrap();
        let session = SharedSession {
            id: SessionId::default(),
            name: "worker".into(),
            agent: "claude".into(),
            backend_id: "%7".into(),
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
        };
        db.upsert_session(&session).unwrap();
        let stored = || db.get_session_meta(session.id, CLAUDE_SOCKET_META).unwrap();

        std::env::set_var(CLAUDE_SOCKET_ENV, "");
        remember_claude_socket(&db, &session);
        assert_eq!(stored(), None, "an empty variable is no socket");

        std::env::set_var(CLAUDE_SOCKET_ENV, "/tmp/cc-socks/9.sock");
        remember_claude_socket(&db, &session);
        assert_eq!(stored().as_deref(), Some("/tmp/cc-socks/9.sock"));

        // A restart binds a new socket; the next hook replaces the old path.
        std::env::set_var(CLAUDE_SOCKET_ENV, "/tmp/cc-socks/10.sock");
        remember_claude_socket(&db, &session);
        assert_eq!(stored().as_deref(), Some("/tmp/cc-socks/10.sock"));
        std::env::remove_var(CLAUDE_SOCKET_ENV);
    }

    #[cfg(unix)]
    #[test]
    fn registry_matches_the_interactive_session_on_the_pane() {
        use std::os::unix::net::UnixListener;

        let dir = tempfile::TempDir::new().unwrap();
        let socks = dir.path().join("socks");
        std::fs::create_dir(&socks).unwrap();
        let _old = UnixListener::bind(socks.join("1.sock")).unwrap();
        let _new = UnixListener::bind(socks.join("2.sock")).unwrap();
        let _print = UnixListener::bind(socks.join("3.sock")).unwrap();
        let _other = UnixListener::bind(socks.join("4.sock")).unwrap();
        let entry = |pid: u32, pane: &str, kind: &str, started: i64, sock: &str| {
            let v = json!({
                "pid": pid, "kind": kind, "startedAt": started,
                "tmux": format!("thurbox:@7.{pane}"),
                "messagingSocketPath": socks.join(sock),
            });
            std::fs::write(dir.path().join(format!("{pid}.json")), v.to_string()).unwrap();
        };
        entry(1, "%7", "interactive", 100, "1.sock");
        entry(2, "%7", "interactive", 200, "2.sock");
        entry(3, "%7", "print", 300, "3.sock");
        entry(4, "%77", "interactive", 400, "4.sock");
        // Registered against the pane, but its socket is gone.
        entry(5, "%7", "interactive", 500, "5.sock");
        std::fs::write(dir.path().join("junk.json"), "not json").unwrap();

        assert_eq!(
            registry_sockets(dir.path(), "%7"),
            vec![socks.join("2.sock"), socks.join("1.sock")]
        );
        assert!(registry_sockets(dir.path(), "").is_empty());
    }
}
