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

use super::{repo_name, SessionRow, SnapshotSource, SourceRead, StatusSource};
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

/// The Task label holding the name a session was given in thurbox, which a
/// Task's own name (a DNS-like name) may not spell.
pub const NAME_LABEL: &str = "thurbox.dev/name";

enum Update {
    Rows(Vec<SessionRow>),
    /// Why the watch is not running, or `None` once it reads again.
    Watch(Option<String>),
    /// Why no manager runs, or `None` once one does.
    Manager(Option<String>),
    /// The harnesses Fleet has profiles for: the agents a session can run.
    Harnesses(Vec<String>),
    /// The hosts Fleet defines: where a session can run.
    Hosts(Vec<String>),
}

/// The Tasks as session rows, followed by `fleet watch` on a worker.
pub struct FleetWatch {
    updates: Receiver<Update>,
    read: SourceRead,
    watch_error: Option<String>,
    manager_error: Option<String>,
    /// The Tasks as `fleet watch` last read them, before the interface's own
    /// order is laid over them.
    tasks: Vec<SessionRow>,
    /// Whether the watch has delivered rows yet.
    rows_seen: bool,
    /// `interface.json` as last read, and the generation it was read at.
    interface: Option<(u64, crate::config::interface_state::InterfaceState)>,
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
        {
            let (program, tx) = (program.clone(), tx.clone());
            std::thread::spawn(move || read_harnesses(&program, &tx));
        }
        {
            let (program, tx) = (program.clone(), tx.clone());
            std::thread::spawn(move || read_hosts(&program, &tx));
        }
        let child = Arc::new(Mutex::new(None));
        let held = Arc::clone(&child);
        std::thread::spawn(move || watch(&program, &tx, &held));
        Self {
            updates,
            read: SourceRead::default(),
            watch_error: None,
            manager_error: None,
            tasks: Vec::new(),
            rows_seen: false,
            interface: None,
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
                Update::Rows(rows) => {
                    self.tasks = rows;
                    self.rows_seen = true;
                }
                Update::Watch(error) => self.watch_error = error,
                Update::Manager(error) => self.manager_error = error,
                Update::Harnesses(harnesses) => self.read.harnesses = Some(harnesses),
                Update::Hosts(hosts) => self.read.hosts = Some(hosts),
            }
        }
        // The interface's own file, read again only when this process wrote
        // it: what it holds is changed from this interface alone.
        let generation = crate::config::interface_state::generation();
        if self
            .interface
            .as_ref()
            .is_none_or(|(seen, _)| *seen != generation)
        {
            self.interface = Some((generation, crate::config::interface_state::load()));
            // Before the first rows there is nothing to lay it over.
            moved |= self.rows_seen;
        }
        if !moved {
            return None;
        }
        let state = self.interface.as_ref().map(|(_, state)| state);
        self.read.sessions = self
            .tasks
            .iter()
            .cloned()
            .map(|mut row| {
                if let Some(state) = state {
                    row.display_order = state
                        .order
                        .iter()
                        .position(|id| *id == row.id)
                        .map(|at| at as i64);
                }
                row
            })
            .collect();
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

/// Ask Fleet which hosts it defines (`fleet runtime hosts --json`), again
/// every [`RETRY`] until it answers: the hosts the creation flow offers are
/// Fleet's. `null` (one runtime serves any host) names none to offer.
fn read_hosts(program: &Path, tx: &Sender<Update>) {
    loop {
        if let Some(answer) = ask(program, &["runtime", "hosts", "--json"]) {
            let hosts = answer["hosts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|h| h.as_str().map(str::to_string))
                .collect();
            let _ = tx.send(Update::Hosts(hosts));
            return;
        }
        std::thread::sleep(RETRY);
    }
}

/// One `fleet <args>` answer as JSON, or `None` when it failed.
fn ask(program: &Path, args: &[&str]) -> Option<Value> {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| serde_json::from_slice::<Value>(&out.stdout).ok())
}

/// Ask Fleet which harnesses it has profiles for (`fleet version --json`),
/// again every [`RETRY`] until it answers: the agents the creation flow
/// offers are Fleet's, and thurbox keeps no list of its own.
fn read_harnesses(program: &Path, tx: &Sender<Update>) {
    loop {
        if let Some(answer) = ask(program, &["version", "--json"]) {
            let harnesses = answer["harnesses"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|h| h.as_str().map(str::to_string))
                .collect();
            let _ = tx.send(Update::Harnesses(harnesses));
            return;
        }
        std::thread::sleep(RETRY);
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
    // Drained beside stdout so a chatty stderr cannot stall the watch. A
    // warning (a store it cannot read) is the source's error while the watch
    // runs, until the `synced` that follows the next read that works; the
    // last line is the reason the watch ended.
    let warnings = tx.clone();
    let last_error = std::thread::spawn(move || {
        let mut last = String::new();
        for line in stderr
            .map(BufReader::new)
            .into_iter()
            .flat_map(|r| r.lines())
        {
            match line {
                Ok(line) if !line.trim().is_empty() => {
                    if let Some(warning) = line.strip_prefix("warning: ") {
                        let _ =
                            warnings.send(Update::Watch(Some(format!("fleet watch: {warning}"))));
                    }
                    last = line;
                }
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
        let dir = crate::paths::data_directory();
        let error = match dir
            .ok_or_else(|| "fleet up: no data directory for its log".to_string())
            .and_then(|dir| manage(program, &dir, tx))
        {
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

/// One `fleet up`, waited for. Its standard error goes to a file in `dir`,
/// thurbox's own data directory, rather than a pipe: a pipe would end the
/// manager with the interface, the first time it wrote after thurbox exited.
/// The file is read back and removed when the manager exits; one whose
/// manager outlived the interface stays readable, and a later start removes
/// it once that manager is gone ([`clear_old_logs`]).
fn manage(program: &Path, dir: &Path, tx: &Sender<Update>) -> Result<Option<i32>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("fleet up: log: {e}"))?;
    clear_old_logs(dir);
    let dir = dir.to_path_buf();
    let (log, mut reader, path) = open_log(&dir).map_err(|e| format!("fleet up: log: {e}"))?;
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
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return Err(format!("could not start fleet up: {e}"));
        }
    };
    // Named for the manager from here on, so a later start can tell a log
    // whose manager still runs from one nobody writes.
    let named = dir.join(format!("fleet-up-{}.log", child.id()));
    let path = match std::fs::rename(&path, &named) {
        Ok(()) => named,
        Err(_) => path,
    };
    if tx.send(Update::Manager(None)).is_err() {
        return Ok(Some(LEASE_HELD));
    }
    let status = child.wait().map_err(|e| format!("fleet up: {e}"))?;
    let mut stderr = String::new();
    let _ = reader.rewind();
    let _ = reader.read_to_string(&mut stderr);
    let _ = std::fs::remove_file(&path);
    if status.code() == Some(LEASE_HELD) {
        return Ok(status.code());
    }
    match stderr.lines().find_map(|line| line.strip_prefix("error: ")) {
        Some(why) => Err(format!("fleet up: {why}")),
        None => Ok(status.code()),
    }
}

/// Remove the `fleet up` logs in `dir` whose process is gone: a manager
/// that outlived its interface, and has exited since. A log is named for the
/// manager it holds (`fleet-up-<pid>.log`), or, before the manager started,
/// for the interface (`fleet-up-<pid>-<n>.log`).
fn clear_old_logs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut system = sysinfo::System::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.strip_prefix("fleet-up-"))
            .and_then(|rest| rest.strip_suffix(".log"))
            .and_then(|rest| rest.split('-').next())
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };
        let pid = sysinfo::Pid::from_u32(pid);
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        if system.process(pid).is_none() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The `n`th name this process gives a `fleet up` log: unique to it, so two
/// interfaces sharing a data directory never touch each other's.
fn log_name(n: u64) -> String {
    format!("fleet-up-{}-{n}.log", std::process::id())
}

/// `fleet up`'s standard error: a file in `dir`, a handle to read it back
/// (the same file description: rewind before reading), and its path.
///
/// Created exclusively under a name of this process's own: a name already
/// taken, by a link or anything else, is never opened, and the next one is
/// tried instead.
fn open_log(dir: &Path) -> std::io::Result<(std::fs::File, std::fs::File, PathBuf)> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    loop {
        let path = dir.join(log_name(
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(log) => {
                let reader = log.try_clone()?;
                return Ok((log, reader, path));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
}

/// One Task as a session row.
///
/// The directory a `file` or `git+file` URL names, without a query or a
/// fragment, `%XX` decoded; `None` for any other URL.
fn local_dir(url: &str) -> Option<String> {
    let path = url
        .strip_prefix("file://")
        .or_else(|| url.strip_prefix("git+file://"))?;
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let mut bytes = Vec::with_capacity(path.len());
    let mut rest = path.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        let hex = tail
            .get(..2)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (b, hex) {
            (b'%', Some(decoded)) => {
                bytes.push(decoded);
                rest = &tail[2..];
            }
            _ => {
                bytes.push(b);
                rest = tail;
            }
        }
    }
    String::from_utf8(bytes).ok().filter(|p| !p.is_empty())
}

/// The Task's name is the row's id: every command addresses a Task by name.
/// The row's name is the one the session was given ([`NAME_LABEL`]), or the
/// Task's for a Task made elsewhere. What a row says of git, panes and hook
/// columns is thurbox's own engine's and stays empty here; the status is
/// Fleet's `agentState` (the agent's hooks, else its screen) over its
/// `sessionState`, and `status.activity` says which source decided it.
pub fn row_of(task: &Value) -> SessionRow {
    let text = |value: &Value| value.as_str().map(str::to_string);
    let (spec, status) = (&task["spec"], &task["status"]);
    let id = text(&task["metadata"]["name"]).unwrap_or_default();
    let name = text(&task["metadata"]["labels"][NAME_LABEL]).unwrap_or_else(|| id.clone());
    // Where the session runs once the runtime made its workspace; before,
    // the directory its first URL names on this machine.
    let cwd = text(&status["workspace"]["dirs"][0])
        .or_else(|| spec["workspace"][0].as_str().and_then(local_dir))
        .map(PathBuf::from);
    // Every directory the session spans, in order: the runtime's answer, or
    // before it, the session's own directory.
    let member_dirs: Vec<PathBuf> = status["workspace"]["dirs"]
        .as_array()
        .map(|dirs| {
            dirs.iter()
                .filter_map(Value::as_str)
                .map(PathBuf::from)
                .collect()
        })
        .filter(|dirs: &Vec<PathBuf>| !dirs.is_empty())
        .unwrap_or_else(|| cwd.iter().cloned().collect());
    let session_state = status["sessionState"].as_str();
    let agent_state = status["agentState"].as_str();
    let repo = repo_name(&cwd, None);
    // The source of a status the agent state decided: not of a stopped,
    // lost or gone session's.
    let status_source = match (session_state, &status["activity"]["source"]) {
        (Some("Paused" | "Lost" | "Gone"), _) | (_, Value::Null) => None,
        (_, source) => Some(StatusSource {
            source: text(source).unwrap_or_default(),
            rule: text(&status["activity"]["rule"]),
            since_ms: status["activity"]["at"].as_i64(),
        }),
    };
    SessionRow {
        id,
        name,
        agent: text(&spec["harness"]).unwrap_or_default(),
        // A stopped, lost or gone session is that, whatever its agent last
        // said; otherwise the agent's own report, as v2 showed it, and only
        // with no report what the runtime proved.
        status: match (session_state, agent_state) {
            (Some("Paused"), _) => SessionState::Stopped,
            (Some("Lost" | "Gone"), _) => SessionState::Failed,
            (_, Some("working")) => SessionState::Working,
            (_, Some("attention")) => SessionState::Blocked,
            (_, Some("idle")) => SessionState::Idle,
            (Some("Running"), _) => SessionState::Running,
            (Some("Idle"), _) => SessionState::Idle,
            _ => SessionState::Unreported,
        },
        repos: repo.iter().cloned().collect(),
        repo,
        member_dirs,
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
        status_source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session over several repositories spans every directory the runtime
    /// made, in order: the editor (`Ctrl+O`) opens them all, not the first.
    #[test]
    fn a_row_spans_every_directory_of_its_workspace() {
        let task = serde_json::json!({
            "metadata": {"name": "pair"},
            "spec": {"harness": "claude", "host": "local", "workspace": [
                "git+file:///src/a?branch=pair", "git+file:///src/b?branch=pair"
            ]},
            "status": {"workspace": {"key": "k", "host": "local", "dirs": ["/w/k/a", "/w/k/b"]}},
        });
        let row = row_of(&task);
        assert_eq!(row.cwd, Some(PathBuf::from("/w/k/a")));
        assert_eq!(
            row.member_dirs,
            vec![PathBuf::from("/w/k/a"), PathBuf::from("/w/k/b")]
        );
    }

    /// A row names where its agent state came from, but a paused, lost or
    /// gone session's status is not the agent state's.
    #[test]
    fn a_row_carries_the_source_of_its_agent_state() {
        let task = |session_state: &str| {
            serde_json::json!({
                "metadata": {"name": "t"},
                "spec": {"harness": "codex", "host": "local", "workspace": []},
                "status": {"sessionState": session_state, "agentState": "attention",
                           "activity": {"source": "screen", "rule": "codex/action-required", "at": 7}},
            })
        };
        let row = row_of(&task("Running"));
        assert_eq!(row.status, SessionState::Blocked);
        assert_eq!(
            row.status_source,
            Some(StatusSource {
                source: "screen".into(),
                rule: Some("codex/action-required".into()),
                since_ms: Some(7),
            })
        );
        assert_eq!(row_of(&task("Paused")).status_source, None);
        let mut bare = task("Running");
        bare["status"].as_object_mut().unwrap().remove("activity");
        assert_eq!(row_of(&bare).status_source, None);
    }

    /// The agents a session can run are the harnesses Fleet has profiles
    /// for, read from `fleet version --json`: thurbox keeps no list of its own.
    #[cfg(unix)]
    #[test]
    fn the_agents_are_the_harnesses_fleet_reports() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fleet = dir.path().join("fleet");
        std::fs::write(
            &fleet,
            "#!/bin/sh\n[ \"$1\" = version ] && exec echo '{\"harnesses\":[\"claude-code\",\"codex\"]}'\nexec sleep 30\n",
        )
        .unwrap();
        std::fs::set_permissions(&fleet, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut source = FleetWatch::spawn(fleet);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let harnesses = loop {
            if let Some(harnesses) = source.poll().and_then(|read| read.harnesses) {
                break harnesses;
            }
            assert!(std::time::Instant::now() < deadline, "Fleet never answered");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(harnesses, ["claude-code", "codex"]);
    }

    /// Whatever stands where a log would go, a link included, is never
    /// opened: what it pointed at keeps its contents.
    #[cfg(unix)]
    #[test]
    fn a_planted_link_at_the_log_path_is_not_followed() {
        let dir = tempfile::tempdir().expect("dir");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, "keep").expect("victim");
        for name in (0..64).map(log_name) {
            std::os::unix::fs::symlink(&victim, dir.path().join(name)).expect("link");
        }

        let (mut log, mut reader, _) = open_log(dir.path()).expect("a free name");
        std::io::Write::write_all(&mut log, b"error: written\n").expect("write");
        let mut read = String::new();
        reader.rewind().expect("rewind");
        reader.read_to_string(&mut read).expect("read");
        assert_eq!(read, "error: written\n");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    }

    /// A stand-in `fleet` running `script` as `fleet up`.
    #[cfg(unix)]
    fn fake_fleet(dir: &Path, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.join("fleet");
        std::fs::write(&program, format!("#!/bin/sh\n{script}\n")).expect("fleet");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        program
    }

    #[cfg(unix)]
    fn logs(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("dir")
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|name| name.starts_with("fleet-up-"))
            .collect();
        names.sort();
        names
    }

    /// A manager that outlives the interface keeps writing to a log that is
    /// still there to read, named for its pid.
    #[cfg(unix)]
    #[test]
    fn a_running_managers_log_stays_readable() {
        let bin = tempfile::tempdir().expect("bin");
        let data = tempfile::tempdir().expect("data");
        let program = fake_fleet(bin.path(), "echo 'warning: still here' >&2; exec sleep 30");
        let (tx, _rx) = channel();
        let dir = data.path().to_path_buf();
        std::thread::spawn(move || {
            let _ = manage(&program, &dir, &tx);
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let (name, text) = loop {
            let found = logs(data.path()).into_iter().find_map(|name| {
                let text = std::fs::read_to_string(data.path().join(&name)).ok()?;
                text.contains("still here").then_some((name, text))
            });
            if let Some(found) = found {
                break found;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no readable log: {:?}",
                logs(data.path())
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        let pid: u32 = name
            .trim_start_matches("fleet-up-")
            .trim_end_matches(".log")
            .parse()
            .unwrap_or_else(|_| panic!("named for the manager's pid: {name}"));
        let _ = Command::new("kill").arg(pid.to_string()).status();
        assert!(text.contains("warning: still here"));
    }

    /// A manager that exits is read back, and its log goes.
    #[cfg(unix)]
    #[test]
    fn an_exited_managers_log_is_read_then_removed() {
        let bin = tempfile::tempdir().expect("bin");
        let data = tempfile::tempdir().expect("data");
        let program = fake_fleet(bin.path(), "echo 'error: no starter' >&2; exit 1");
        let (tx, _rx) = channel();
        let why = manage(&program, data.path(), &tx).expect_err("it failed");
        assert_eq!(why, "fleet up: no starter");
        assert_eq!(logs(data.path()), Vec::<String>::new());
    }

    /// A log whose process is gone is cleared at the next start; one whose
    /// process runs is kept.
    #[cfg(unix)]
    #[test]
    fn logs_of_gone_processes_are_cleared() {
        let data = tempfile::tempdir().expect("data");
        let mut gone = Command::new("true").spawn().expect("true");
        let gone_pid = gone.id();
        gone.wait().expect("wait");
        let alive = std::process::id();
        for pid in [gone_pid, alive] {
            std::fs::write(data.path().join(format!("fleet-up-{pid}.log")), "x").expect("log");
        }
        clear_old_logs(data.path());
        assert_eq!(logs(data.path()), vec![format!("fleet-up-{alive}.log")]);
    }

    /// Two interfaces sharing a data directory each get a log of their own.
    #[test]
    fn each_manager_start_has_a_log_of_its_own() {
        let dir = tempfile::tempdir().expect("dir");
        let (mut first, mut first_read, first_path) = open_log(dir.path()).expect("first");
        let (mut second, mut second_read, second_path) = open_log(dir.path()).expect("second");
        assert_ne!(first_path, second_path);
        std::io::Write::write_all(&mut first, b"first").expect("write");
        std::io::Write::write_all(&mut second, b"second").expect("write");
        for (reader, want) in [(&mut first_read, "first"), (&mut second_read, "second")] {
            let mut read = String::new();
            reader.rewind().expect("rewind");
            reader.read_to_string(&mut read).expect("read");
            assert_eq!(read, want);
        }
        assert!(first_path.is_file() && second_path.is_file());
    }
}
