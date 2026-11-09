//! Fleet as the snapshot's source (ADR-41): the Tasks, read through `fleet
//! watch --json`, each published as a session row; and the `fleet up` that
//! keeps them reconciling.
//!
//! Both run on threads of their own, so the store's poll only drains a
//! channel. thurbox reads nothing of Fleet's store directly and links no
//! Fleet crate: the command line and its NDJSON are the contract
//! (`docs/fleet/contracts.md`, the command line).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Seek};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use super::{repo_name, SessionRow, SnapshotSource, SourceRead};
use crate::session::SessionState;

/// How long a watch that ended waits before it is started again, resuming
/// from the last revision it read.
const RETRY: Duration = Duration::from_secs(2);

/// How often a manager that is not ours, or that would not start, is asked
/// about again: each ask is a `fleet up`, which exits at once while another
/// holds the lease.
const MANAGER_CHECK: Duration = Duration::from_secs(10);

/// `fleet up`'s exit code when another manager holds the lease.
const LEASE_HELD: i32 = 6;

/// The backend every Task's row names: no multiplexer of thurbox's, but
/// Fleet, whose runtime keeps the terminal.
pub const BACKEND: &str = "fleet";

enum Update {
    Rows(Vec<SessionRow>),
    /// Why the watch is not running, or `None` once it reads again.
    Watch(Option<String>),
    /// Why no manager runs, or `None` once one does.
    Manager(Option<String>),
}

/// The Tasks as session rows, followed by `fleet watch` on a worker.
pub struct FleetWatch {
    updates: Receiver<Update>,
    read: SourceRead,
    watch_error: Option<String>,
    manager_error: Option<String>,
    /// The running `fleet watch`, killed when the source goes.
    child: Arc<Mutex<Option<Child>>>,
}

impl FleetWatch {
    /// Follow the Tasks of the Fleet `program` runs.
    pub fn spawn(program: PathBuf) -> Self {
        Self::start(program, false)
    }

    /// [`Self::spawn`], and keep a `fleet up` running: one is started
    /// whenever no manager holds the lease. A manager thurbox started
    /// outlives it, in a process group of its own: Tasks keep reconciling
    /// with no interface open.
    pub fn with_manager(program: PathBuf) -> Self {
        Self::start(program, true)
    }

    fn start(program: PathBuf, manager: bool) -> Self {
        let (tx, updates) = channel();
        if manager {
            let (program, tx) = (program.clone(), tx.clone());
            std::thread::spawn(move || keep_manager(&program, &tx));
        }
        let child = Arc::new(Mutex::new(None));
        let held = Arc::clone(&child);
        std::thread::spawn(move || watch(&program, &tx, &held));
        Self {
            updates,
            read: SourceRead::default(),
            watch_error: None,
            manager_error: None,
            child,
        }
    }
}

impl SnapshotSource for FleetWatch {
    fn poll(&mut self) -> Option<SourceRead> {
        let mut moved = false;
        while let Ok(update) = self.updates.try_recv() {
            moved = true;
            match update {
                Update::Rows(rows) => self.read.sessions = rows,
                Update::Watch(error) => self.watch_error = error,
                Update::Manager(error) => self.manager_error = error,
            }
        }
        if !moved {
            return None;
        }
        let errors: Vec<&str> = [&self.watch_error, &self.manager_error]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        self.read.error = (!errors.is_empty()).then(|| errors.join("; "));
        Some(self.read.clone())
    }
}

impl Drop for FleetWatch {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            if let Some(child) = child.as_mut() {
                let _ = child.kill();
            }
        }
    }
}

/// Run `fleet watch` until the source goes, starting it again whenever it
/// ends.
fn watch(program: &Path, tx: &Sender<Update>, held: &Mutex<Option<Child>>) {
    let mut tasks: BTreeMap<String, Value> = BTreeMap::new();
    let mut since = 0u64;
    loop {
        let why = match follow(program, &mut tasks, &mut since, tx, held) {
            Ok(()) => return,
            Err(why) => why,
        };
        if tx.send(Update::Watch(Some(why))).is_err() {
            return;
        }
        std::thread::sleep(RETRY);
    }
}

/// One `fleet watch`, from `since`. `Ok` when the source went; otherwise why
/// the watch ended.
fn follow(
    program: &Path,
    tasks: &mut BTreeMap<String, Value>,
    since: &mut u64,
    tx: &Sender<Update>,
    held: &Mutex<Option<Child>>,
) -> Result<(), String> {
    let mut child = Command::new(program)
        .args(["watch", "--json", "--since", &since.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start fleet watch: {e}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    // Drained beside stdout so a chatty stderr cannot stall the watch; its
    // last line is the reason the watch ended.
    let last_error = std::thread::spawn(move || {
        let mut last = String::new();
        for line in stderr
            .map(BufReader::new)
            .into_iter()
            .flat_map(|r| r.lines())
        {
            match line {
                Ok(line) if !line.trim().is_empty() => last = line,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        last
    });
    *held.lock().map_err(|_| "fleet watch: poisoned")? = Some(child);
    let mut synced = false;
    for line in stdout
        .map(BufReader::new)
        .into_iter()
        .flat_map(|r| r.lines())
    {
        let Ok(line) = line else { break };
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some(revision) = event["revision"].as_u64() {
            *since = (*since).max(revision);
        }
        match event["type"].as_str() {
            Some("changed") => {
                if let Some(name) = event["object"]["metadata"]["name"].as_str() {
                    tasks.insert(name.to_string(), event["object"].clone());
                }
            }
            Some("deleted") => {
                if let Some(name) = event["name"].as_str() {
                    tasks.remove(name);
                }
            }
            // A resumed watch reports only what moved: a Task deleted while
            // it was down is one `synced` does not name.
            Some("synced") => {
                let names: Vec<&str> = event["names"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                tasks.retain(|name, _| names.contains(&name.as_str()));
                synced = true;
                if tx.send(Update::Watch(None)).is_err() {
                    return stop(held);
                }
            }
            _ => continue,
        }
        if synced
            && tx
                .send(Update::Rows(tasks.values().map(row_of).collect()))
                .is_err()
        {
            return stop(held);
        }
    }
    let status = held
        .lock()
        .ok()
        .and_then(|mut child| child.take())
        .map(|mut child| child.wait());
    let why = last_error.join().unwrap_or_default();
    Err(match status {
        Some(Ok(status)) => format!("fleet watch exited ({status}): {why}"),
        _ => format!("fleet watch ended: {why}"),
    })
}

fn stop(held: &Mutex<Option<Child>>) -> Result<(), String> {
    if let Some(mut child) = held.lock().ok().and_then(|mut child| child.take()) {
        let _ = child.kill();
        let _ = child.wait();
    }
    Ok(())
}

/// Keep a `fleet up` running while the source does: start one, and when it
/// exits, say why and start another after [`MANAGER_CHECK`]. One that
/// exits for a held lease is no error: another manager runs.
fn keep_manager(program: &Path, tx: &Sender<Update>) {
    loop {
        let error = match manage(program, tx) {
            Ok(Some(LEASE_HELD)) => None,
            Ok(code) => Some(format!(
                "fleet up exited{}",
                code.map(|c| format!(" with {c}")).unwrap_or_default()
            )),
            Err(why) => Some(why),
        };
        if tx.send(Update::Manager(error)).is_err() {
            return;
        }
        std::thread::sleep(MANAGER_CHECK);
    }
}

/// One `fleet up`, waited for. Its standard error goes to a file in
/// thurbox's own data directory, unlinked once the manager has it, rather
/// than a pipe: a pipe would end the manager with the interface, the first
/// time it wrote after thurbox exited.
fn manage(program: &Path, tx: &Sender<Update>) -> Result<Option<i32>, String> {
    let dir = crate::paths::data_directory().ok_or("fleet up: no data directory for its log")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("fleet up: log: {e}"))?;
    let (log, mut reader) = open_log(&dir).map_err(|e| format!("fleet up: log: {e}"))?;
    let path = dir.join(LOG);
    let mut command = Command::new(program);
    command
        .arg("up")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let spawned = command.spawn();
    let _ = std::fs::remove_file(&path);
    let mut child = spawned.map_err(|e| format!("could not start fleet up: {e}"))?;
    if tx.send(Update::Manager(None)).is_err() {
        return Ok(Some(LEASE_HELD));
    }
    let status = child.wait().map_err(|e| format!("fleet up: {e}"))?;
    if status.code() == Some(LEASE_HELD) {
        return Ok(status.code());
    }
    let mut stderr = String::new();
    let _ = reader.rewind();
    let _ = reader.read_to_string(&mut stderr);
    match stderr.lines().find_map(|line| line.strip_prefix("error: ")) {
        Some(why) => Err(format!("fleet up: {why}")),
        None => Ok(status.code()),
    }
}

/// The name of `fleet up`'s standard error file.
const LOG: &str = "fleet-up.log";

/// `fleet up`'s standard error: a file in `dir`, and a handle to read it back.
///
/// Created afresh and exclusively: whatever stands at the path, a link
/// included, is removed rather than opened, so nothing it points at is
/// written.
fn open_log(dir: &Path) -> std::io::Result<(std::fs::File, std::fs::File)> {
    let path = dir.join(LOG);
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let log = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let reader = std::fs::File::open(&path)?;
    Ok((log, reader))
}

/// One Task as a session row.
///
/// The Task's name is the row's id and name: every command addresses a Task
/// by name. What a row says of git, panes and hook columns is thurbox's own
/// engine's and stays empty here; Fleet's `sessionState` is the status.
pub fn row_of(task: &Value) -> SessionRow {
    let text = |value: &Value| value.as_str().map(str::to_string);
    let (spec, status) = (&task["spec"], &task["status"]);
    let name = text(&task["metadata"]["name"]).unwrap_or_default();
    let cwd = text(&status["workspace"]["dir"])
        .or_else(|| text(&spec["workspace"]["dir"]))
        .map(PathBuf::from);
    let session_state = status["sessionState"].as_str();
    let repo = repo_name(&cwd, None);
    SessionRow {
        id: name.clone(),
        name,
        agent: text(&spec["harness"]).unwrap_or_default(),
        status: match session_state {
            Some("Running") => SessionState::Running,
            Some("Idle") => SessionState::Idle,
            Some("Paused") => SessionState::Stopped,
            Some("Lost" | "Gone") => SessionState::Failed,
            _ => SessionState::Unreported,
        },
        repos: repo.iter().cloned().collect(),
        repo,
        member_dirs: cwd.iter().cloned().collect(),
        cwd,
        branch: None,
        base_branch: None,
        backend: BACKEND.to_string(),
        // The terminal is the stream `fleet attach` names, not a pane of
        // thurbox's backends.
        backend_id: None,
        remote_host: text(&spec["host"]).filter(|host| host != "local"),
        agent_session_id: text(&status["conversation"]),
        parent_id: text(&task["metadata"]["labels"]["gateway.fleet.dev/parent"]),
        display_order: None,
        worktree_count: 0,
        git: None,
        shell_backend_id: None,
        stopped: session_state == Some("Paused"),
        hook_state: None,
        key: text(&status["session"]["key"]),
        reports_as: None,
        detected_agent: None,
        meta: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link planted where the log goes is replaced, never followed: what
    /// it pointed at keeps its contents.
    #[cfg(unix)]
    #[test]
    fn a_planted_link_at_the_log_path_is_not_followed() {
        let dir = tempfile::tempdir().expect("dir");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep").expect("victim");
        std::os::unix::fs::symlink(&victim, dir.path().join(LOG)).expect("link");

        let (mut log, mut reader) = open_log(dir.path()).expect("the log opens");
        std::io::Write::write_all(&mut log, b"error: written\n").expect("write");
        let mut read = String::new();
        reader.read_to_string(&mut read).expect("read");

        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(read, "error: written\n");
    }
}
