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

use serde_json::Value;

use crate::session::SessionMessage;
use crate::storage::Database;
use crate::sync::SharedSession;

/// Session-meta key holding the Claude inbox socket, captured from the agent's
/// own hook environment by `session signal` (see [`remember_claude_socket`]).
pub(crate) const CLAUDE_SOCKET_META: &str = "thurbox.claude_messaging_socket";

/// Session-meta key holding the Claude session registry the recipient's own
/// hook saw, which differs from the sender's when the two run with different
/// `$CLAUDE_CONFIG_DIR`s.
const CLAUDE_REGISTRY_META: &str = "thurbox.claude_registry_dir";

/// Session-meta key `session bind-codex` records the Codex thread under.
const CODEX_THREAD_META: &str = "thurbox.codex_conversation_id";

/// The env var Claude Code exports to hooks and its Bash tool.
const CLAUDE_SOCKET_ENV: &str = "CLAUDE_CODE_MESSAGING_SOCKET";

/// A `codex queue` that has not returned by now is not going to; the message is
/// still in the mailbox, so giving up costs timeliness, not the message.
const CODEX_QUEUE_TIMEOUT: Duration = Duration::from_secs(20);

/// Claude Code closes a connection without a complete line within 30 s; a
/// local socket write that blocks this long means the reader is wedged.
#[cfg(unix)]
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
/// wrapper script, while a proven socket or a bound Codex thread is the agent
/// having announced itself.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Evidence {
    /// The session runs on another machine, where its agent's inbox is.
    pub remote: bool,
    /// Inbox sockets proven to belong to the recipient ([`owned_sockets`]),
    /// the one its hooks last reported first.
    pub claude_sockets: Vec<PathBuf>,
    /// The Codex thread `session bind-codex` recorded.
    pub codex_thread: Option<String>,
}

/// The routes to try, in order — or why there are none.
///
/// Claude comes first because its evidence is proof of a live process (a
/// socket exists only while its session runs), where a Codex thread id outlives
/// the process that opened it.
pub(crate) fn routes(evidence: &Evidence) -> Result<Vec<Route>, String> {
    if evidence.remote {
        return Err(
            "the session runs on a remote host, whose agent inbox is not reachable from here"
                .into(),
        );
    }
    let mut routes: Vec<Route> = Vec::new();
    for socket in &evidence.claude_sockets {
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
    // This process's registry, and the one the recipient's own hook reported:
    // they differ when the two sessions run with different `CLAUDE_CONFIG_DIR`s.
    let mut dirs: Vec<PathBuf> = claude_registry_dir().into_iter().collect();
    if let Some(theirs) = meta(CLAUDE_REGISTRY_META).map(PathBuf::from) {
        if !dirs.contains(&theirs) {
            dirs.push(theirs);
        }
    }
    let id = recipient.id.to_string();
    let mut claude_sockets: Vec<PathBuf> = Vec::new();
    for socket in dirs.iter().flat_map(|dir| owned_sockets(dir, &id)) {
        if !claude_sockets.contains(&socket) {
            claude_sockets.push(socket);
        }
    }
    // The captured socket is only an ordering hint: it is used when, and only
    // when, it is also one of the proven ones.
    if let Some(captured) = meta(CLAUDE_SOCKET_META).map(PathBuf::from) {
        if let Some(at) = claude_sockets.iter().position(|s| *s == captured) {
            let socket = claude_sockets.remove(at);
            claude_sockets.insert(0, socket);
        }
    }
    Evidence {
        remote: false,
        claude_sockets,
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

/// Inbox sockets of the interactive Claude sessions running *as* thurbox
/// session `session_id`, newest first.
///
/// Each running Claude Code writes `<pid>.json` naming its pid, its `kind` and
/// its `messagingSocketPath`. A socket counts only when all of these hold,
/// because anything weaker delivers a message into somebody else's
/// conversation:
///
/// - **`kind` is `interactive`.** A `claude -p` run from inside the pane
///   inherits the pane's identity, so only the kind tells it apart from the
///   session the pane shows.
/// - **The process's own environment carries `THURBOX_SESSION=<session_id>`.**
///   thurbox injects that into every pane it spawns, so it is the recipient's
///   identity read off the process that owns the socket. The weaker signals
///   are each wrong somewhere: the registry's `tmux` field names a pane id,
///   which another tmux server reuses; and `$CLAUDE_CODE_MESSAGING_SOCKET` in a
///   hook is inherited by every pane of a tmux server started from inside some
///   other Claude session.
/// - **The path is still a socket.** An entry outlives a crashed process.
///
/// A process whose environment cannot be read proves nothing, and its socket
/// is not used: the message then waits in the mailbox rather than risking the
/// wrong recipient. The pane id is not consulted at all, and neither is
/// thurbox's `agent_session_id`, which drifts from Claude's after a resume.
fn owned_sockets(dir: &Path, session_id: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(i64, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|raw| serde_json::from_str::<Value>(&raw).ok())
        .filter(|v| v["kind"].as_str() == Some("interactive"))
        .filter_map(|v| {
            let socket = PathBuf::from(v["messagingSocketPath"].as_str()?);
            let pid = u32::try_from(v["pid"].as_u64()?).ok()?;
            let owned = is_socket(&socket)
                && process_env_var(pid, "THURBOX_SESSION").as_deref() == Some(session_id);
            owned.then(|| (v["startedAt"].as_i64().unwrap_or(0), socket))
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

/// `name` from the environment process `pid` started with, if it is readable.
///
/// Denied, exited and foreign-user processes all read as `None`, which
/// [`owned_sockets`] treats as unproven.
#[cfg(target_os = "linux")]
fn process_env_var(pid: u32, name: &str) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    env_value(raw.split(|b| *b == 0), name)
}

#[cfg(target_os = "macos")]
fn process_env_var(pid: u32, name: &str) -> Option<String> {
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROCARGS2,
        libc::c_int::try_from(pid).ok()?,
    ];
    let mut size: libc::size_t = 0;
    // SAFETY: a size query — a null buffer with a valid length out-pointer.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size];
    // SAFETY: `buf` is `size` bytes long and `size` says so; the kernel writes
    // at most that much and reports what it wrote back through `size`.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(size);
    procargs_env_var(&buf, name)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_env_var(_pid: u32, _name: &str) -> Option<String> {
    None
}

/// `name` from a `KERN_PROCARGS2` buffer: a native-endian `argc`, the exec
/// path, NUL padding, `argc` argument strings, then the environment.
#[cfg(any(target_os = "macos", test))]
fn procargs_env_var(buf: &[u8], name: &str) -> Option<String> {
    let argc = usize::try_from(i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?)).ok()?;
    let rest = buf.get(4..)?;
    let rest = &rest[rest.iter().position(|b| *b == 0)?..];
    let rest = &rest[rest.iter().position(|b| *b != 0)?..];
    let mut fields = rest.split(|b| *b == 0);
    for _ in 0..argc {
        fields.next()?;
    }
    env_value(fields, name)
}

/// The value of `name` among `NAME=value` fields, stopping at the first empty
/// one (the end of an environment block).
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn env_value<'a>(fields: impl Iterator<Item = &'a [u8]>, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    fields
        .take_while(|f| !f.is_empty())
        .find_map(|f| f.strip_prefix(prefix.as_bytes()))
        .map(|v| String::from_utf8_lossy(v).into_owned())
}

/// Record the Claude inbox socket of the calling agent, from the environment
/// Claude Code gives its hooks. Called by `session signal`, which every thurbox
/// Claude hook runs, so a restarted session is recorded on its `SessionStart`.
///
/// Recorded only once [`owned_sockets`] proves the socket belongs to
/// `session`: the variable is inherited by panes that are not that Claude's
/// (a nested `claude -p`, or every pane of a tmux server started from inside
/// another Claude session). [`gather`] proves it again before use, since the
/// process may have gone and its pid been reused.
pub(crate) fn remember_claude_socket(db: &Database, session: &SharedSession) {
    let Some(socket) = std::env::var(CLAUDE_SOCKET_ENV)
        .ok()
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    // The hook runs in the agent's environment, so this is *its* registry.
    let Some(dir) = claude_registry_dir() else {
        return;
    };
    let dir_str = dir.to_string_lossy().into_owned();
    let recorded = |key| db.get_session_meta(session.id, key).ok().flatten();
    if recorded(CLAUDE_SOCKET_META).as_deref() == Some(socket.as_str())
        && recorded(CLAUDE_REGISTRY_META).as_deref() == Some(dir_str.as_str())
    {
        return;
    }
    if !owned_sockets(&dir, &session.id.to_string()).contains(&PathBuf::from(&socket)) {
        tracing::debug!(
            "not recording {socket}: it does not belong to '{}'",
            session.name
        );
        return;
    }
    for (key, value) in [
        (CLAUDE_SOCKET_META, &socket),
        (CLAUDE_REGISTRY_META, &dir_str),
    ] {
        if let Err(e) = db.set_session_meta(session.id, key, value) {
            tracing::warn!("could not record {key}: {e}");
        }
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

/// The stream-json message Claude Code's inbox reads: one user turn,
/// newline-terminated.
#[cfg(unix)]
fn claude_envelope(text: &str) -> String {
    let line = serde_json::json!({
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
///
/// The send runs under a delivery lease
/// ([`Database::lease_message_delivery`]): a drain racing it skips the row, so
/// the body reaches the agent or the mailbox consumer, not both. The lease
/// lapses rather than marking the row read, so a sender killed mid-send delays
/// the message by the lease but never hides it. It is renewed before each
/// attempt, and each attempt is bounded well inside it (a 5 s socket write, a
/// 20 s `codex queue`), so a live send does not outlive its lease; one that
/// has lost it does not start. The one duplicate left is a sender killed after
/// a successful send and before it records it — once the lease lapses, a drain
/// hands that body over again.
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
    let lease = match db.lease_message_delivery(message.id) {
        Ok(Some(lease)) => lease,
        Ok(None) => {
            return Outcome::mailbox(
                "it was drained from the mailbox, or is being delivered, before this send",
            )
        }
        Err(e) => {
            tracing::warn!("message #{}: take the delivery lease: {e}", message.id);
            return Outcome::mailbox(format!("could not take the delivery lease: {e}"));
        }
    };
    let mut failures = Vec::new();
    for route in routes {
        let via = route.via().as_str();
        // Each attempt starts with a full lease, and none starts without one:
        // a lease that lapsed may already be another sender's or a drain's.
        match db.renew_message_delivery(message.id, &lease) {
            Ok(true) => {}
            Ok(false) => {
                failures.push("the delivery lease was lost; another reader took it".into());
                break;
            }
            Err(e) => {
                failures.push(format!("renew the delivery lease: {e}"));
                break;
            }
        }
        let sent = match &route {
            Route::ClaudeSocket(socket) => transport.post_to_claude(socket, text),
            Route::CodexQueue(thread) => transport.queue_to_codex(thread, text),
        };
        let target = match &route {
            Route::ClaudeSocket(socket) => socket.display().to_string(),
            Route::CodexQueue(thread) => format!("thread {thread}"),
        };
        match sent {
            Ok(()) => {
                match db.complete_message_delivery(message.id, &lease, via) {
                    Ok(true) => {}
                    // The send outran its lease and a drain took the row.
                    Ok(false) => tracing::warn!(
                        "message #{}: {via} delivered after the lease was lost",
                        message.id
                    ),
                    // Delivered; once the lease lapses a drain repeats it.
                    Err(e) => {
                        tracing::warn!("message #{}: record {via} delivery: {e}", message.id)
                    }
                }
                return Outcome {
                    via: route.via(),
                    note: None,
                };
            }
            Err(e) => {
                tracing::warn!(
                    "message #{}: {via} delivery to {target} failed: {e}",
                    message.id
                );
                failures.push(format!("{via} ({target}): {e}"));
            }
        }
    }
    if let Err(e) = db.release_message_delivery(message.id, &lease) {
        tracing::warn!("message #{}: release the delivery lease: {e}", message.id);
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
            claude_sockets: vec![path.into()],
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
    fn each_socket_is_tried_once_in_order() {
        let evidence = Evidence {
            claude_sockets: vec!["/s/a.sock".into(), "/s/b.sock".into(), "/s/a.sock".into()],
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
        // The lease is given back: the next drain takes it straight away.
        assert_eq!(db.claim_messages(msg.to_session_id, None).unwrap().len(), 1);
    }

    /// Drains the recipient's inbox from inside the send, the race a
    /// concurrent `inbox --claim` would make.
    struct DrainsMidSend<'a> {
        db: &'a Database,
        drained: RefCell<usize>,
    }

    impl Transport for DrainsMidSend<'_> {
        fn post_to_claude(&self, _socket: &Path, _text: &str) -> Result<(), String> {
            let to = self.db.get_message(1).unwrap().unwrap().to_session_id;
            *self.drained.borrow_mut() += self.db.claim_messages(to, None).unwrap().len();
            Ok(())
        }
        fn queue_to_codex(&self, _thread: &str, _text: &str) -> Result<(), String> {
            unreachable!()
        }
    }

    #[test]
    fn a_drain_racing_the_send_does_not_also_hand_it_over() {
        let db = Database::open_in_memory().unwrap();
        let msg = enqueued(&db);
        assert_eq!(msg.id, 1);
        let racing = DrainsMidSend {
            db: &db,
            drained: RefCell::new(0),
        };
        let out = deliver(&db, &msg, "text", &claude("/s/a.sock"), &racing);
        assert_eq!(out.via, DeliveredVia::ClaudeSocket);
        assert_eq!(*racing.drained.borrow(), 0, "the body went one way only");
        assert!(db
            .claim_messages(msg.to_session_id, None)
            .unwrap()
            .is_empty());
    }

    /// Fails the Claude attempt slowly enough that the lease lapses and
    /// another sender takes it, then records whether Codex was tried anyway.
    struct LosesTheLease<'a> {
        db: &'a Database,
        codex_tried: RefCell<bool>,
    }

    impl Transport for LosesTheLease<'_> {
        fn post_to_claude(&self, _socket: &Path, _text: &str) -> Result<(), String> {
            let stale = crate::sync::current_time_millis() as i64
                - crate::storage::messages::DELIVERY_LEASE_MS
                - 1;
            self.db
                .conn_ref()
                .execute(
                    "UPDATE session_messages SET delivering_at = ?1 WHERE id = 1",
                    [stale],
                )
                .unwrap();
            self.db.lease_message_delivery(1).unwrap().unwrap();
            Err("connect: timed out".into())
        }
        fn queue_to_codex(&self, _thread: &str, _text: &str) -> Result<(), String> {
            *self.codex_tried.borrow_mut() = true;
            Ok(())
        }
    }

    #[test]
    fn a_sender_that_lost_its_lease_tries_no_further_route() {
        let db = Database::open_in_memory().unwrap();
        let msg = enqueued(&db);
        assert_eq!(msg.id, 1);
        let transport = LosesTheLease {
            db: &db,
            codex_tried: RefCell::new(false),
        };
        let evidence = Evidence {
            codex_thread: Some(THREAD.into()),
            ..claude("/s/slow.sock")
        };
        let out = deliver(&db, &msg, "text", &evidence, &transport);
        assert_eq!(out.via, DeliveredVia::Mailbox);
        assert!(out.note.unwrap().contains("lease was lost"));
        assert!(
            !*transport.codex_tried.borrow(),
            "the new holder sends, not us"
        );
        // Its release did not free the new holder's lease.
        assert!(db
            .claim_messages(msg.to_session_id, None)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_second_sender_leaves_a_message_in_flight_alone() {
        let db = Database::open_in_memory().unwrap();
        let msg = enqueued(&db);
        db.lease_message_delivery(msg.id).unwrap().unwrap();
        let fake = Fake::default();
        let out = deliver(&db, &msg, "text", &claude("/s/a.sock"), &fake);
        assert_eq!(out.via, DeliveredVia::Mailbox);
        assert!(fake.sent.borrow().is_empty());
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

    #[cfg(unix)]
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
    fn procargs_env_is_read_past_the_arguments() {
        let mut buf = 2i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/bin/claude\0\0\0claude\0THURBOX_SESSION=arg\0");
        buf.extend_from_slice(b"HOME=/h\0THURBOX_SESSION=abc\0\0junk=1\0");
        // The argument that merely looks like the variable is skipped.
        assert_eq!(
            procargs_env_var(&buf, "THURBOX_SESSION").as_deref(),
            Some("abc")
        );
        assert_eq!(procargs_env_var(&buf, "HOME").as_deref(), Some("/h"));
        // Past the environment's terminating empty field is not environment.
        assert_eq!(procargs_env_var(&buf, "junk"), None);
        assert_eq!(procargs_env_var(&buf[..3], "HOME"), None);
    }

    /// A live process started with `THURBOX_SESSION=<session>`, standing in for
    /// the Claude Code a thurbox pane runs. It is this test binary running
    /// [`idle_as_a_claude_stand_in`]: macOS will not read the arguments of an
    /// Apple platform binary such as `/bin/sleep`, even a copy of one.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn process_as(session: &str) -> std::process::Child {
        // `spawn` returns once the exec happened, so the environment is final.
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cli::delivery::tests::idle_as_a_claude_stand_in",
                "--ignored",
            ])
            .env("THURBOX_SESSION", session)
            .env(STAND_IN_ENV, "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    const STAND_IN_ENV: &str = "THURBOX_DELIVERY_TEST_STAND_IN";

    /// Not a test: the body of [`process_as`]'s child. Idles only when that
    /// child is what runs it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    #[ignore = "helper process for the ownership tests"]
    fn idle_as_a_claude_stand_in() {
        if std::env::var_os(STAND_IN_ENV).is_some() {
            std::thread::sleep(Duration::from_secs(30));
        }
    }

    /// Write a Claude registry entry for `pid` into `dir`, with a live socket
    /// under `socks` unless `live` is false. Returns the socket path and the
    /// listener keeping it live.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn register(
        dir: &Path,
        name: &str,
        pid: u32,
        kind: &str,
        started: i64,
        live: bool,
    ) -> (PathBuf, Option<std::os::unix::net::UnixListener>) {
        let socket = dir.join(format!("{name}.sock"));
        let listener = live.then(|| std::os::unix::net::UnixListener::bind(&socket).unwrap());
        let entry = serde_json::json!({
            "pid": pid, "kind": kind, "startedAt": started,
            "tmux": "thurbox:@7.%7", "messagingSocketPath": socket,
        });
        std::fs::write(dir.join(format!("{name}.json")), entry.to_string()).unwrap();
        (socket, listener)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_socket_counts_only_when_its_process_is_the_session() {
        let dir = tempfile::TempDir::new().unwrap();
        let (me, other) = ("11111111-1111-4111-8111-111111111111", "other-session");
        let mut mine = process_as(me);
        let mut older = process_as(me);
        let mut theirs = process_as(other);
        let (newest, _a) = register(dir.path(), "a", mine.id(), "interactive", 200, true);
        let (oldest, _b) = register(dir.path(), "b", older.id(), "interactive", 100, true);
        // Same pane id, another server's (or an inherited) session: refused.
        let _c = register(dir.path(), "c", theirs.id(), "interactive", 300, true);
        // A `claude -p` in the recipient's own pane: same identity, refused.
        let _d = register(dir.path(), "d", mine.id(), "print", 400, true);
        // The recipient's process, but its socket is gone.
        let _e = register(dir.path(), "e", mine.id(), "interactive", 500, false);
        std::fs::write(dir.path().join("junk.json"), "not json").unwrap();

        assert_eq!(owned_sockets(dir.path(), me), vec![newest, oldest]);
        for child in [&mut mine, &mut older, &mut theirs] {
            let _ = child.kill();
            let _ = child.wait();
        }
        // The processes are gone, so nothing is proven any more.
        assert!(owned_sockets(dir.path(), me).is_empty());
    }

    /// One test, because every step points `CLAUDE_CONFIG_DIR` at a fixture.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn only_a_proven_socket_is_recorded_and_it_is_tried_first() {
        // The recipient's Claude runs under its own config dir; the sender,
        // later, under another one with an empty registry.
        let config = tempfile::TempDir::new().unwrap();
        let dir = config.path().join("sessions");
        std::fs::create_dir(&dir).unwrap();
        let senders = tempfile::TempDir::new().unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", config.path());

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
        let id = session.id.to_string();
        let mut a = process_as(&id);
        let mut b = process_as(&id);
        let mut lead = process_as("the-lead");
        let (older, _a) = register(&dir, "a", a.id(), "interactive", 100, true);
        let (newer, _b) = register(&dir, "b", b.id(), "interactive", 200, true);
        let (leads, _l) = register(&dir, "lead", lead.id(), "interactive", 300, true);
        let stored = || db.get_session_meta(session.id, CLAUDE_SOCKET_META).unwrap();

        // A lead's socket inherited through the tmux server's environment.
        std::env::set_var(CLAUDE_SOCKET_ENV, &leads);
        remember_claude_socket(&db, &session);
        assert_eq!(stored(), None);

        std::env::set_var(CLAUDE_SOCKET_ENV, &older);
        remember_claude_socket(&db, &session);
        assert_eq!(stored().map(PathBuf::from), Some(older.clone()));

        // A sender with a different `CLAUDE_CONFIG_DIR` still finds them,
        // through the registry the recipient's hook reported. Newest first,
        // except the one the hooks reported, which leads.
        std::env::set_var("CLAUDE_CONFIG_DIR", senders.path());
        assert_eq!(gather(&db, &session).claude_sockets, vec![older, newer]);

        std::env::remove_var(CLAUDE_SOCKET_ENV);
        std::env::remove_var("CLAUDE_CONFIG_DIR");
        for child in [&mut a, &mut b, &mut lead] {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
