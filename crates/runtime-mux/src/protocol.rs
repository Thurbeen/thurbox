//! The runtime protocol (docs/runtime/README.md) over one backend: what the
//! `runtime-mux` program answers for each verb.
//!
//! Each call is one request in and one answer out, `{"known": …}` or
//! `{"unknown": {"why": …}}`. A key is found again by its window's name
//! (`name_of`), and its window answers for it while it is there.
//!
//! A process is the key's agent window. A pane kept after its program exits is
//! how `inspect` reports an exit code; a `start` replaces it. Two things are
//! kept outside the server ([`Kept`]): a paused key's mark, since a pause
//! leaves no window to keep it on, and each started key until its `stop`,
//! since a reboot takes the server and its windows with it.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::contract::{Located, Owner, SessionBackend, WindowRole, WindowSpec};
use crate::identity::window_name_for;
use crate::place::Place;
use crate::workspace::{Workspaces, SCHEMES};
use crate::HostDef;

pub use crate::place::LOCAL;

/// The newest protocol version this program speaks, which its manifest
/// names. Version 2 carries a place on each request.
pub const PROTOCOL: u64 = 2;

/// Every version it answers. A version 1 request names no place and runs at
/// `local`, this machine.
pub const PROTOCOLS: &[u64] = &[1, 2];

/// The verbs it answers, `manifest` first.
pub const VERBS: &[&str] = &[
    "manifest",
    "start",
    "inspect",
    "stop",
    "pause",
    "list",
    "input",
    "capture",
    "exec",
    "attach",
    "shell",
    "workspace",
    "browse",
    "branches",
];

/// The tmux socket the program's server runs on when [`SOCKET_ENV`] names
/// none: its own, never the interface's (`thurbox`), which it does not read.
pub const SOCKET: &str = "runtime-mux";

/// Names another socket, for an operator running two Fleets on one machine
/// and for tests.
pub const SOCKET_ENV: &str = "RUNTIME_MUX_SOCKET";

/// The longest key: Fleet's are `<prefix>.<uid>`, well inside it.
const MAX_KEY: usize = 128;

/// Rows `capture` reads when the request names none.
const DEFAULT_CAPTURE_LINES: u32 = 50;

/// The most `exec` keeps of each stream when the request names no cap.
const DEFAULT_OUTPUT_CAP: usize = 1024 * 1024;
const MAX_OUTPUT_CAP: usize = 16 * 1024 * 1024;
const MAX_EXEC_TIMEOUT_MS: u64 = 600_000;

/// How long `exec` waits for its output past the deadline.
const READ_GRACE: Duration = Duration::from_millis(200);

fn known(value: Value) -> Value {
    json!({ "known": value })
}

fn unknown(why: impl std::fmt::Display) -> Value {
    json!({ "unknown": { "why": why.to_string() } })
}

/// The backend serving a place: this machine's for `None`, else the one on
/// the host the place names, or why there is none.
pub type Backends<'a> =
    dyn Fn(Option<&HostDef>) -> Result<std::sync::Arc<dyn SessionBackend>, String> + Sync + 'a;

/// The place a request runs at, parsed: `None` for `local`, for a version 1
/// request, and for a request [`serve_with`] refuses, which it answers
/// Unknown itself. The caller builds the backend and the locks for it.
pub fn place_of(request: &[u8]) -> Option<HostDef> {
    let request: Value = serde_json::from_slice(request).ok()?;
    if request.get("protocol").and_then(Value::as_u64) != Some(2) {
        return None;
    }
    check_place(request.get("place")).ok().flatten()
}

/// The answer to `verb` with `request`, the JSON read from stdin. `backends`
/// gives the backend of the request's place. `view` is the argv that runs
/// this program's `view` against the same server, which `attach` answers
/// with the key after it. `process_done` is called once a releasing stop
/// has stopped the process and before it releases the workspace: the caller
/// lets go of the server's lock there, so a slow teardown holds only its key.
pub fn serve_with(
    backends: &Backends,
    kept: &Kept,
    verb: &str,
    request: &[u8],
    view: &[String],
    process_done: &mut dyn FnMut(),
) -> Value {
    if verb == "manifest" {
        // The files a workspace holds are on the host, so they outlive the
        // process: a stop without release keeps them.
        return known(json!({
            "protocol": PROTOCOL,
            "verbs": VERBS,
            "workspace": { "schemes": SCHEMES, "survivesStop": true },
            "places": places(backends, request),
        }));
    }
    let request: Value = match serde_json::from_slice(request) {
        Ok(value) => value,
        Err(e) => return unknown(format!("the request is not JSON: {e}")),
    };
    let place = match request.get("protocol").and_then(Value::as_u64) {
        Some(1) => None,
        Some(2) => match check_place(request.get("place")) {
            Ok(place) => place,
            Err(why) => return unknown(why),
        },
        Some(other) => {
            return unknown(format!(
                "protocol {other} is not served; this runtime speaks {PROTOCOLS:?}"
            ))
        }
        None => return unknown("the request names no protocol"),
    };
    let backend = match backends(place.as_ref()) {
        Ok(backend) => backend,
        Err(why) => return unknown(why),
    };
    let backend = backend.as_ref();
    let place = place.as_ref();
    // The backend's commands lead with the place's login `PATH`: read it,
    // or take it from the cache, before the first of them. `exec` reads it
    // inside its own deadline.
    if let Some(host) = place.filter(|_| verb != "exec") {
        crate::place::warm(host);
    }
    // A view of a remote place's pane names the place, so the view builds
    // that place's backend and reads the pane over its connection.
    let view = match (place, request.get("place")) {
        (Some(_), Some(named)) => {
            let mut argv = view.to_vec();
            argv.extend(["--place".to_string(), named.to_string()]);
            argv
        }
        _ => view.to_vec(),
    };
    let view = view.as_slice();
    let result = match verb {
        "start" => parse(request).map(|r| start(backend, place, kept, r)),
        "inspect" => parse(request).map(|r: Keyed| inspect(backend, kept, &r.key)),
        "pause" => parse(request).map(|r: Keyed| pause(backend, kept, &r.key)),
        "stop" => parse(request).map(|r: Stop| {
            let stopped = stop(backend, kept, &r.key);
            if let (Some(host), Some(_)) = (place, stopped.get("known")) {
                crate::place::remove_assets(host, &r.key);
            }
            if !r.release || stopped.get("known").is_none() {
                return stopped;
            }
            process_done();
            match Workspaces::load()
                .and_then(|w| w.at(place))
                .and_then(|w| w.release(&r.key))
            {
                Ok(()) => stopped,
                Err(why) => unknown(format!(
                    "the process stopped; the workspace was not released: {why}"
                )),
            }
        }),
        "workspace" => parse(request).map(|r: Workspace| match Workspaces::load() {
            // A place that does not answer may answer the next call.
            Ok(workspaces) => match workspaces.at(place) {
                Ok(workspaces) => workspaces.make(&r.key, &r.urls),
                Err(why) => unknown(why),
            },
            Err(why) => known(json!({ "failed": { "why": why } })),
        }),
        "list" => Ok(list(backend, kept)),
        "input" => parse(request).map(|r| input(backend, r)),
        "capture" => parse(request).map(|r| capture(backend, r)),
        "exec" => parse(request).map(|r: Exec| exec(place, r)),
        "attach" => parse(request).map(|r: Keyed| attach(backend, &r.key, view)),
        "shell" => parse(request).map(|r: Keyed| shell(backend, &r.key, view)),
        "browse" => parse(request).map(|r| browse(place, r)),
        "branches" => parse(request).map(|r| branches(place, r)),
        other => Err(format!("unknown verb {other:?}")),
    };
    result.unwrap_or_else(unknown)
}

/// `browse`: a directory of the place, its home when the request names none.
#[derive(Deserialize)]
struct Browse {
    dir: Option<PathBuf>,
}

impl Validate for Browse {
    /// The directory is the place's: [`browse`] checks it by the place's
    /// rules ([`absolute_on`]).
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

/// `browse`: the subdirectories of `dir`, by name, each marked when it is a
/// git checkout, so a creation flow can pick a repository on the place.
/// Files are left out. A directory that cannot be read is a failed browse.
/// A remote place is read over its launcher ([`crate::place::browse`]).
fn browse(place: Option<&HostDef>, request: Browse) -> Value {
    let platform = place.map_or(crate::Platform::local(), HostDef::platform);
    if let Some(dir) = &request.dir {
        if !absolute_on(&dir.to_string_lossy(), platform) {
            return unknown("dir must be an absolute path");
        }
    }
    if let Some(host) = place {
        let dir = request.dir.as_ref().map(|d| d.to_string_lossy());
        return match crate::place::browse(host, dir.as_deref()) {
            Err(why) => unknown(why),
            Ok(Err(why)) => known(json!({ "failed": { "why": why } })),
            Ok(Ok((dir, entries))) => listing(dir, entries),
        };
    }
    let dir = match request.dir.or_else(home) {
        Some(dir) => dir,
        None => return known(json!({ "failed": { "why": "this place has no home directory" } })),
    };
    let read = match std::fs::read_dir(&dir) {
        Ok(read) => read,
        Err(e) => return known(json!({ "failed": { "why": format!("{}: {e}", dir.display()) } })),
    };
    let entries: Vec<(String, bool)> = read
        .filter_map(Result::ok)
        // `path().is_dir()` follows a symlink to a directory.
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            Some((name, is_repository(&entry.path())))
        })
        .collect();
    listing(dir.display().to_string(), entries)
}

/// A `browse` answer: `dir`, and its subdirectories sorted by name, each
/// with whether it is a git work tree.
fn listing(dir: String, mut entries: Vec<(String, bool)>) -> Value {
    entries.sort();
    let entries: Vec<Value> = entries
        .into_iter()
        .map(|(name, repo)| json!({ "name": name, "repo": repo }))
        .collect();
    known(json!({ "listing": { "dir": dir, "entries": entries } }))
}

/// The home directory of the user this program runs as.
fn home() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// Whether `dir` is the top of a git work tree, as git reads one, from the
/// filesystem: a `.git` directory holding `HEAD`, `objects` and `refs`, or a
/// `.git` file naming one (a linked worktree, a submodule). A `git` that
/// cannot run never passes for none, and an empty `.git` is no work tree.
fn is_repository(dir: &std::path::Path) -> bool {
    let dot = dir.join(".git");
    if dot.is_dir() {
        return dot.join("HEAD").is_file()
            && dot.join("objects").is_dir()
            && dot.join("refs").is_dir();
    }
    std::fs::read_to_string(&dot).is_ok_and(|text| text.starts_with("gitdir: "))
}

/// `branches`: a repository of the place.
#[derive(Deserialize)]
struct Branches {
    repo: PathBuf,
}

impl Validate for Branches {
    /// The repository is the place's: [`branches`] checks it by the place's
    /// rules ([`absolute_on`]).
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

/// `branches`: the repository `repo` is (not one it is inside of): its local
/// branches, then its `origin/*` ones,
/// as they are (no fetch), and the one a new branch starts from by default:
/// `origin`'s default branch (`origin/HEAD`, else `origin/main`, else
/// `origin/master`), else the branch checked out, else the first.
fn branches(place: Option<&HostDef>, request: Branches) -> Value {
    let repo = &request.repo;
    let platform = place.map_or(crate::Platform::local(), HostDef::platform);
    if !absolute_on(&repo.to_string_lossy(), platform) {
        return unknown("repo must be an absolute path");
    }
    let failed = |why: String| known(json!({ "failed": { "why": why } }));
    // A remote place's repository is read there, by the same rules, in one
    // call ([`crate::place::git_refs`]).
    if let Some(host) = place {
        return match crate::place::git_refs(host, &repo.to_string_lossy()) {
            Err(why) => unknown(why),
            Ok(Err(why)) => failed(why),
            Ok(Ok([refs, upstream, checked_out])) => {
                branch_list(&refs, Some(upstream), Some(checked_out))
            }
        };
    }
    // The repository itself, as `browse` marks one: a directory inside a
    // checkout is not its repository.
    if !is_repository(repo) {
        return failed(format!("{} is no git repository", repo.display()));
    }
    let refs = match git_out(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads",
            "refs/remotes/origin",
        ],
    ) {
        Ok(refs) => refs,
        Err(why) => return failed(why),
    };
    let upstream = git_out(repo, &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"]).ok();
    let checked_out = git_out(repo, &["symbolic-ref", "-q", "--short", "HEAD"]).ok();
    branch_list(&refs, upstream, checked_out)
}

/// The `branches` answer from what git printed: `refs`, the repository's
/// refs under `refs/heads` and `refs/remotes/origin`; `upstream`,
/// `origin/HEAD`'s target; `checked_out`, the branch checked out.
fn branch_list(refs: &str, upstream: Option<String>, checked_out: Option<String>) -> Value {
    let mut local = Vec::new();
    let mut remote = Vec::new();
    for line in refs.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix("refs/heads/") {
            local.push(name.to_string());
        } else if let Some(name) = line.strip_prefix("refs/remotes/origin/") {
            if name != "HEAD" {
                remote.push(format!("origin/{name}"));
            }
        }
    }
    // The rule thurbox's `git::discovery::branches` reads a repository on
    // its own machine by, kept the same: no crate both may depend on holds
    // it (crates/architecture.toml). `origin`'s branches and the local ones
    // are told apart by their refs, never by name: a local branch may be
    // called `origin/main`.
    let in_remote = |name: &str| remote.iter().any(|n| n == name);
    let in_local = |name: &str| local.iter().any(|n| n == name);
    let upstream =
        upstream.and_then(|r| r.trim().strip_prefix("refs/remotes/").map(str::to_string));
    let checked_out = checked_out.map(|b| b.trim().to_string());
    let default = upstream
        .filter(|name| in_remote(name))
        .or_else(|| {
            ["origin/main", "origin/master"]
                .into_iter()
                .find(|name| in_remote(name))
                .map(str::to_string)
        })
        .or_else(|| checked_out.filter(|name| in_local(name)))
        .or_else(|| local.first().cloned());
    let names: Vec<String> = local.iter().chain(&remote).cloned().collect();
    known(json!({ "branches": { "names": names, "default": default } }))
}

/// `git <args>` in `repo`, its stdout, with the variables that would point
/// git at another repository cleared.
fn git_out(repo: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let mut git = Command::new("git");
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_NAMESPACE",
    ] {
        git.env_remove(var);
    }
    // Nor may git look for a repository above `repo`.
    if let Some(parent) = repo.parent() {
        git.env("GIT_CEILING_DIRECTORIES", parent);
    }
    let out = git
        .args(args)
        .current_dir(repo)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A version 2 request's `place`, checked and parsed: every request but
/// `manifest` names one. `None` is `local`.
pub fn check_place(place: Option<&Value>) -> Result<Option<HostDef>, String> {
    let place = place.ok_or("a version 2 request names its place")?;
    let place: Place =
        serde_json::from_value(place.clone()).map_err(|e| format!("bad place: {e}"))?;
    crate::place::parse(&place)
}

/// How long `manifest` waits for the places' readiness, all of them
/// together: well inside Fleet's bound on a call (15 s unless configured),
/// so one place that does not answer leaves the others their report.
const READINESS_BUDGET: Duration = Duration::from_secs(8);

/// The manifest's `places`: each place the request asks about, or `local`
/// when it asks about none, with whether this program can run a process
/// there now. Each place is probed on a thread of its own, and every probe
/// shares [`READINESS_BUDGET`]: a place whose probe has not answered by
/// then is not ready, and its probe is left to end on its own.
fn places(backends: &Backends, request: &[u8]) -> Vec<Value> {
    let asked: Vec<Value> = serde_json::from_slice::<Value>(request)
        .ok()
        .and_then(|r| r.get("places").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    let asked = if asked.is_empty() {
        vec![json!({ "name": LOCAL, "config": {} })]
    } else {
        asked
    };
    let deadline = Instant::now() + READINESS_BUDGET;
    let local = if cfg!(windows) { "windows" } else { "posix" };
    let (tx, rx) = mpsc::channel();
    let mut reports: Vec<Value> = asked
        .into_iter()
        .enumerate()
        .map(|(i, place)| {
            let name = place
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let parsed = serde_json::from_value::<Place>(place)
                .map_err(|e| format!("bad place: {e}"))
                .and_then(|place| crate::place::parse(&place));
            let platform = match &parsed {
                Ok(Some(host)) => host.platform().name(),
                _ => local,
            };
            match parsed.and_then(|host| Ok((backends(host.as_ref())?, host))) {
                Ok((backend, host)) => {
                    let tx = tx.clone();
                    std::thread::spawn(move || {
                        let ready = match &host {
                            Some(host) => {
                                crate::place::readiness(host, backend.as_ref(), deadline)
                            }
                            None => backend.check_available().map_err(|e| e.to_string()),
                        };
                        let _ = tx.send((i, ready));
                    });
                    json!({ "name": name, "platform": platform, "dynamic": false, "ready": false,
                            "why": format!("place {name:?}: its readiness probe timed out after {} s", READINESS_BUDGET.as_secs()) })
                }
                Err(why) => json!({ "name": name, "platform": platform, "dynamic": false,
                                    "ready": false, "why": why }),
            }
        })
        .collect();
    drop(tx);
    while let Ok((i, ready)) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        let report = &mut reports[i];
        match ready {
            Ok(()) => {
                report["ready"] = json!(true);
                if let Some(map) = report.as_object_mut() {
                    map.remove("why");
                }
            }
            Err(why) => report["why"] = json!(why),
        }
    }
    reports
}

/// A request as its verb's shape, with every key it names checked.
fn parse<T: for<'de> Deserialize<'de> + Validate>(request: Value) -> Result<T, String> {
    let parsed: T = serde_json::from_value(request).map_err(|e| format!("bad request: {e}"))?;
    parsed.validate()?;
    Ok(parsed)
}

trait Validate {
    fn validate(&self) -> Result<(), String>;
}

/// A key the program accepts: what Fleet makes, and nothing tmux would read
/// as a target or a format.
fn check_key(key: &str) -> Result<(), String> {
    if key.is_empty()
        || key.len() > MAX_KEY
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err(format!(
            "key {key:?}: use 1 to {MAX_KEY} ASCII letters, digits, '.', '-' or '_'"
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct Keyed {
    key: String,
}

impl Validate for Keyed {
    fn validate(&self) -> Result<(), String> {
        check_key(&self.key)
    }
}

#[derive(Deserialize)]
struct Stop {
    key: String,
    /// Releases the key's workspace too, after the process.
    #[serde(default)]
    release: bool,
}

impl Validate for Stop {
    fn validate(&self) -> Result<(), String> {
        check_key(&self.key)
    }
}

#[derive(Deserialize)]
struct Workspace {
    key: String,
    urls: Vec<String>,
}

impl Validate for Workspace {
    fn validate(&self) -> Result<(), String> {
        check_key(&self.key)
    }
}

#[derive(Deserialize)]
struct Start {
    key: String,
    argv: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    dir: Option<PathBuf>,
}

impl Validate for Start {
    fn validate(&self) -> Result<(), String> {
        check_key(&self.key)?;
        if self.argv.first().is_none_or(|program| program.is_empty()) {
            return Err("argv names no program".into());
        }
        // The directory is the place's: `start` checks it by the place's
        // rules ([`absolute_on`]).
        Ok(())
    }
}

#[derive(Deserialize)]
struct Input {
    key: String,
    text: String,
    #[serde(default)]
    submit: bool,
}

impl Validate for Input {
    fn validate(&self) -> Result<(), String> {
        check_key(&self.key)
    }
}

#[derive(Deserialize)]
struct Capture {
    key: String,
    lines: Option<u32>,
}

impl Validate for Capture {
    fn validate(&self) -> Result<(), String> {
        check_key(&self.key)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Exec {
    argv: Vec<String>,
    #[serde(rename = "in", default = "host")]
    place: String,
    dir: Option<PathBuf>,
    timeout_ms: u64,
    output_cap: Option<usize>,
}

fn host() -> String {
    "host".into()
}

impl Validate for Exec {
    fn validate(&self) -> Result<(), String> {
        if self.place != "host" {
            return Err(format!(
                "exec in {:?}: this runtime runs exec only `in: host`",
                self.place
            ));
        }
        if self.argv.first().is_none_or(|program| program.is_empty()) {
            return Err("argv names no program".into());
        }
        if !(1..=MAX_EXEC_TIMEOUT_MS).contains(&self.timeout_ms) {
            return Err(format!("timeoutMs must be 1 to {MAX_EXEC_TIMEOUT_MS}"));
        }
        Ok(())
    }
}

/// The name a key's window is found by. A window name keeps only
/// `[A-Za-z0-9_-]` and folds the rest to `_` ([`crate::identity`]), which
/// would make `a.b` and `a_b` one window; escaping `_` and `.` keeps every key
/// its own name, and [`key_of`] reads it back. Nothing but this program makes
/// windows on its socket, so the name alone is the identity: no stamp.
fn name_of(key: &str) -> String {
    let mut name = String::with_capacity(key.len() + 4);
    for c in key.chars() {
        match c {
            '_' => name.push_str("_u"),
            '.' => name.push_str("_d"),
            c => name.push(c),
        }
    }
    name
}

/// The key a window name carries, or `None` for a name [`name_of`] never
/// makes.
fn key_of(name: &str) -> Option<String> {
    let mut key = String::with_capacity(name.len());
    let mut chars = name.chars();
    while let Some(c) = chars.next() {
        match c {
            '_' => match chars.next()? {
                'u' => key.push('_'),
                'd' => key.push('.'),
                _ => return None,
            },
            c => key.push(c),
        }
    }
    check_key(&key).ok().map(|()| key)
}

fn owner(name: &str) -> Owner<'_> {
    Owner::new("", name)
}

/// The key's agent window, where one listing places it.
fn locate(backend: &dyn SessionBackend, key: &str) -> Result<Located, String> {
    backend
        .locate(owner(&name_of(key)))
        .map(|placed| placed.agent)
        .map_err(|e| e.to_string())
}

/// Whether the pane's program runs: `Some(code)` once it exited (`None` inside
/// for a signal), `None` while it runs, `Err` when the server cannot tell.
fn exited(backend: &dyn SessionBackend, pane: &str) -> Result<Option<Option<i64>>, String> {
    let mut state = backend.pane_state(pane).map_err(|e| e.to_string())?;
    if state.dead == Some(true) && state.reaped == Some(false) {
        // tmux before 3.7 reads a pane dead before it reaps the program.
        backend.reap_exited();
        state = backend.pane_state(pane).map_err(|e| e.to_string())?;
    }
    match (state.dead, state.reaped) {
        (Some(false), _) => Ok(None),
        (Some(true), Some(false)) => Err("the process exited and is not reaped yet".into()),
        (Some(true), _) => Ok(Some(state.exit_status)),
        (None, _) => Err("the multiplexer did not say whether the process runs".into()),
    }
}

/// Files kept outside the server, one per key, named `<prefix><key>`, in a
/// directory only this user can write to. `what` names them in an error.
/// `dir` is the reason none could be found, which makes every call that
/// reads one Unknown.
pub struct Marks {
    dir: Result<PathBuf, String>,
    prefix: String,
    what: &'static str,
}

impl Marks {
    pub fn new(dir: Result<PathBuf, String>, prefix: String, what: &'static str) -> Marks {
        Marks { dir, prefix, what }
    }

    fn path(&self, key: &str) -> Result<PathBuf, String> {
        Ok(self.dir.clone()?.join(format!("{}{key}", self.prefix)))
    }

    fn holds(&self, key: &str) -> Result<bool, String> {
        self.read(key).map(|text| text.is_some())
    }

    fn read(&self, key: &str) -> Result<Option<String>, String> {
        let path = self.path(key)?;
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!(
                "cannot read the {} {}: {e}",
                self.what,
                path.display()
            )),
        }
    }

    fn write(&self, key: &str, text: &str) -> Result<(), String> {
        let path = self.path(key)?;
        let fail =
            |e: std::io::Error| format!("cannot write the {} {}: {e}", self.what, path.display());
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(fail)?;
        }
        std::fs::write(&path, text).map_err(fail)
    }

    fn clear(&self, key: &str) -> Result<(), String> {
        let path = self.path(key)?;
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!(
                "cannot remove the {} {}: {e}",
                self.what,
                path.display()
            )),
            _ => Ok(()),
        }
    }

    fn keys(&self) -> Result<Vec<String>, String> {
        let dir = self.dir.clone()?;
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(format!(
                    "cannot list the {}s in {}: {e}",
                    self.what,
                    dir.display()
                ))
            }
        };
        Ok(entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let key = name.strip_prefix(&self.prefix)?;
                check_key(key).ok().map(|()| key.to_string())
            })
            .collect())
    }
}

/// What this program keeps of a key outside the server. `paused` marks a
/// paused key ([`pause`]). `started` holds the pane of each key a `start`
/// made until a `stop`, where a reboot leaves it: a key with no window reads
/// as exited, never as stopped, so Fleet revives its session.
pub struct Kept {
    pub paused: Marks,
    pub started: Marks,
}

fn start(
    backend: &dyn SessionBackend,
    place: Option<&HostDef>,
    kept: &Kept,
    request: Start,
) -> Value {
    // The one Known failure: nothing can have started without the binary.
    if let Err(e) = backend.check_available() {
        let why = match place {
            Some(host) => format!("place {:?}: {e}", host.name),
            None => e.to_string(),
        };
        return known(json!({ "failed": { "why": why } }));
    }
    match locate(backend, &request.key) {
        Err(why) => return unknown(why),
        Ok(Located::Unknown) => {
            return unknown("several windows answer to the key and none is stamped with it")
        }
        Ok(Located::At(pane)) => match exited(backend, &pane) {
            Err(why) => return unknown(why),
            Ok(None) => {
                return match kept.started.write(&request.key, &pane) {
                    Ok(()) => known(json!({ "started": { "id": pane, "adopted": true } })),
                    Err(why) => unknown(why),
                }
            }
            Ok(Some(_)) => {
                if let Err(e) = backend.kill(&pane) {
                    return unknown(e);
                }
            }
        },
        Ok(Located::Absent) => {}
    }
    // On a remote place the agent is looked for on the place's `PATH` before
    // its window is made: a window whose program is missing dies at once,
    // and its pane says less than this does.
    if let Some(why) = place.and_then(|host| crate::place::missing_program(host, &request.argv[0]))
    {
        return known(json!({ "failed": { "why": why } }));
    }
    let platform = place.map_or(crate::Platform::local(), HostDef::platform);
    if let Some(dir) = &request.dir {
        if !absolute_on(&dir.to_string_lossy(), platform) {
            return unknown("dir must be an absolute path");
        }
    }
    let mut request = request;
    if let Some(host) = place {
        if let Err(why) = place_assets(host, &mut request) {
            return known(json!({ "failed": { "why": why } }));
        }
    }
    let name = name_of(&request.key);
    let spec = WindowSpec {
        owner: owner(&name),
        role: WindowRole::Agent,
        command: &request.argv[0],
        args: &request.argv[1..],
        cwd: request.dir.as_deref(),
        env: &request.env,
    };
    // An error after the create went out may still have made the window, so
    // it is Unknown: the next start adopts it.
    let pane = match backend.create_window(&spec) {
        Ok(pane) => pane,
        Err(e) => return unknown(e),
    };
    if let Err(e) = backend.set_pane_retention(&pane, true) {
        return unknown(e);
    }
    if let Err(why) = kept.started.write(&request.key, pane.as_str()) {
        return unknown(why);
    }
    if let Err(why) = kept.paused.clear(&request.key) {
        return unknown(why);
    }
    known(json!({ "started": { "id": pane.as_str(), "adopted": false } }))
}

/// Whether `dir` is absolute on a machine of `platform`, read as a string:
/// this machine's own path rules are not the place's. On POSIX a path
/// starts at `/`; on Windows at a drive (`C:\`, `C:/`) or a UNC share.
fn absolute_on(dir: &str, platform: crate::Platform) -> bool {
    match platform {
        crate::Platform::Posix => dir.starts_with('/'),
        crate::Platform::Windows => {
            let b = dir.as_bytes();
            let drive = b.len() >= 3
                && b[0].is_ascii_alphabetic()
                && b[1] == b':'
                && matches!(b[2], b'\\' | b'/');
            drive || dir.starts_with("\\\\") || dir.starts_with("//")
        }
    }
}

/// A start on a remote place whose launch names a file of the directory
/// Fleet passes as `FLEET_ASSET_DIR`, such as pi's hook extension: the
/// directory is copied to the place, and every word of `argv` and every
/// value of `env` that names it names the copy instead. A launch that names
/// none of its files copies nothing, so it starts on any place. A directory
/// that is not on this machine is left as it is: it may already be the
/// place's.
fn place_assets(host: &HostDef, request: &mut Start) -> Result<(), String> {
    place_profiles(host, request)?;
    let Some(local) = request
        .env
        .get(ASSET_DIR_ENV)
        .filter(|dir| !dir.is_empty() && std::path::Path::new(dir.as_str()).is_dir())
        .cloned()
    else {
        return Ok(());
    };
    let named = request.argv.iter().any(|word| word.contains(&local))
        || request
            .env
            .iter()
            .any(|(name, value)| name != ASSET_DIR_ENV && value.contains(&local));
    if !named {
        return Ok(());
    }
    let remote = crate::place::copy_dir(
        host,
        &request.key,
        std::path::Path::new(&local),
        crate::place::ASSETS_DIR,
    )?;
    for word in request.argv.iter_mut().chain(request.env.values_mut()) {
        if word.contains(&local) {
            *word = word.replace(&local, &remote);
        }
    }
    Ok(())
}

/// The variable Fleet names its harness asset directory in.
const ASSET_DIR_ENV: &str = "FLEET_ASSET_DIR";

/// The variable Fleet names the user's harness profiles directory in, which
/// `fleet signal` reads a profile's payload fields from.
const HARNESS_DIR_ENV: &str = "FLEET_HARNESS_DIR";

/// A start on a remote place with `FLEET_HARNESS_DIR` naming a
/// directory on this machine: the user's profiles are copied to the place
/// and the variable names the copy, so `fleet signal` there reads the
/// profile's own payload fields (its conversation, its prompt).
fn place_profiles(host: &HostDef, request: &mut Start) -> Result<(), String> {
    let Some(local) = request
        .env
        .get(HARNESS_DIR_ENV)
        .filter(|dir| !dir.is_empty() && std::path::Path::new(dir.as_str()).is_dir())
        .cloned()
    else {
        return Ok(());
    };
    let remote = crate::place::copy_dir(
        host,
        &request.key,
        std::path::Path::new(&local),
        crate::place::HARNESSES_DIR,
    )?;
    request.env.insert(HARNESS_DIR_ENV.to_string(), remote);
    Ok(())
}

fn inspect(backend: &dyn SessionBackend, kept: &Kept, key: &str) -> Value {
    let placed = match backend.locate(owner(&name_of(key))) {
        Ok(placed) => placed,
        Err(e) => return unknown(e),
    };
    match placed.agent {
        Located::Unknown => unknown("several windows answer to the key"),
        Located::Absent => match kept.paused.holds(key) {
            Ok(true) => paused(&placed.shell),
            Ok(false) => match kept.started.read(key) {
                Ok(Some(id)) => known(json!({ "exited": { "id": id, "code": null } })),
                Ok(None) => known(json!("none")),
                Err(why) => unknown(why),
            },
            Err(why) => unknown(why),
        },
        Located::At(pane) => match exited(backend, &pane) {
            Err(why) => unknown(why),
            Ok(None) => known(json!({ "running": { "id": pane } })),
            Ok(Some(code)) => known(json!({ "exited": { "id": pane, "code": code } })),
        },
    }
}

/// Stops the key's process and closes its [`shell`], which belongs to the
/// key and would otherwise outlive it: `list` names agents only. Every
/// window named as the key's shell goes, so two of them, which `shell`
/// answers Unknown for, never keep a stop from concluding: nothing but this
/// program makes windows on its socket, so the name alone is the key's.
fn stop(backend: &dyn SessionBackend, kept: &Kept, key: &str) -> Value {
    let stopped = end(backend, key);
    if stopped.get("known").is_none() {
        return stopped;
    }
    match kept
        .paused
        .clear(key)
        .and_then(|()| kept.started.clear(key))
    {
        Ok(()) => stopped,
        Err(why) => unknown(why),
    }
}

/// A marked key with no agent window: paused once no shell of the key is
/// left, else a pause cut short, which the next `pause` finishes.
fn paused(shell: &Located) -> Value {
    match shell {
        Located::Absent => known(json!("paused")),
        _ => unknown("a pause has not finished: the key's shell still runs"),
    }
}

/// Stops the key's process as [`stop`] does, and keeps the key. The shells
/// close first and the agent's window last, with the mark between them: a
/// pause cut short at any step leaves either the agent's window, which
/// answers for the key while it is there, or a finished pause. A mark with
/// a shell left, as an older order could leave, is finished here.
fn pause(backend: &dyn SessionBackend, kept: &Kept, key: &str) -> Value {
    let agent = match locate(backend, key) {
        Err(why) => return unknown(why),
        Ok(Located::Unknown) => return unknown("several windows answer to the key"),
        Ok(Located::Absent) => match kept.paused.holds(key) {
            Ok(true) => None,
            Ok(false) => return known(json!("none")),
            Err(why) => return unknown(why),
        },
        Ok(Located::At(pane)) => Some(pane),
    };
    if let Err(e) = close_shells(backend, key) {
        return unknown(e);
    }
    if let Some(pane) = agent {
        if let Err(why) = kept.paused.write(key, "") {
            return unknown(why);
        }
        if let Err(e) = backend.kill(&pane) {
            return unknown(e);
        }
    }
    known(json!("paused"))
}

/// The key's agent window and every shell window of the key, gone.
fn end(backend: &dyn SessionBackend, key: &str) -> Value {
    let stopped = match locate(backend, key) {
        Err(why) => return unknown(why),
        Ok(Located::Unknown) => return unknown("several windows answer to the key"),
        Ok(Located::Absent) => known(json!("stopped")),
        Ok(Located::At(pane)) => match backend.kill(&pane) {
            Ok(()) => known(json!("stopped")),
            Err(e) => return unknown(e),
        },
    };
    match close_shells(backend, key) {
        Ok(()) => stopped,
        Err(e) => unknown(e),
    }
}

/// Every window named as the key's shell, gone.
fn close_shells(backend: &dyn SessionBackend, key: &str) -> Result<(), String> {
    let shell = window_name_for(WindowRole::Shell, &name_of(key));
    let windows = backend.discover().map_err(|e| e.to_string())?;
    for window in windows.iter().filter(|w| w.name == shell) {
        backend
            .kill(&window.backend_id)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The pane holding `key`: `None` when none does.
pub fn pane_of(backend: &dyn SessionBackend, key: &str) -> Result<Option<String>, String> {
    check_key(key)?;
    match locate(backend, key)? {
        Located::At(pane) => Ok(Some(pane)),
        Located::Absent => Ok(None),
        Located::Unknown => Err("several windows answer to the key".into()),
    }
}

/// The stream that views the key's pane: this program's `view`, the server
/// keeping the screen.
fn attach(backend: &dyn SessionBackend, key: &str, view: &[String]) -> Value {
    match pane_of(backend, key) {
        Err(why) => unknown(why),
        Ok(None) => known(json!("none")),
        Ok(Some(_)) => {
            let argv: Vec<&str> = view.iter().map(String::as_str).chain([key]).collect();
            known(json!({ "stream": { "argv": argv } }))
        }
    }
}

/// A shell in the directory the key's process is in, viewed like
/// [`attach`]: the key's shell window, opened on the first call and found
/// again by the next, so a view that leaves and comes back finds what it
/// typed. Its program is the machine's shell; its directory, the agent's
/// current one as the server reads it, which the server itself falls back
/// from (to its home) when it is gone: nothing here reads this machine's
/// disk, which need not be the place's. It closes with its program, or at
/// the key's [`stop`].
fn shell(backend: &dyn SessionBackend, key: &str, view: &[String]) -> Value {
    let placed = match backend.locate(owner(&name_of(key))) {
        Ok(placed) => placed,
        Err(e) => return unknown(e),
    };
    let agent = match placed.agent {
        Located::Unknown => return unknown("several windows answer to the key"),
        Located::Absent => return known(json!("none")),
        Located::At(pane) => pane,
    };
    let pane = match placed.shell {
        Located::Unknown => return unknown("several shell windows answer to the key"),
        Located::At(pane) => pane,
        Located::Absent => {
            let cwd = backend
                .pane_state(&agent)
                .ok()
                .and_then(|state| state.foreground_cwd)
                .map(PathBuf::from);
            let name = name_of(key);
            let program = backend.default_shell();
            let spec = WindowSpec {
                owner: owner(&name),
                role: WindowRole::Shell,
                command: &program,
                args: &[],
                cwd: cwd.as_deref(),
                env: &HashMap::new(),
            };
            match backend.create_window(&spec) {
                Ok(pane) => pane.as_str().to_string(),
                // The window may have been made: the next call finds it.
                Err(e) => return unknown(e),
            }
        }
    };
    let argv: Vec<&str> = view
        .iter()
        .map(String::as_str)
        .chain(["--pane", &pane])
        .collect();
    known(json!({ "stream": { "argv": argv } }))
}

fn list(backend: &dyn SessionBackend, kept: &Kept) -> Value {
    let kept = match (kept.paused.keys(), kept.started.keys()) {
        (Ok(paused), Ok(started)) => paused.into_iter().chain(started),
        (Err(why), _) | (_, Err(why)) => return unknown(why),
    };
    match backend.discover() {
        Err(e) => unknown(e),
        Ok(windows) => {
            let prefix = window_name_for(WindowRole::Agent, "");
            let mut keys: Vec<String> = windows
                .iter()
                .filter(|w| w.role == WindowRole::Agent)
                .filter_map(|w| key_of(w.name.strip_prefix(&prefix)?))
                .chain(kept)
                .collect();
            keys.sort();
            keys.dedup();
            known(json!({ "keys": keys }))
        }
    }
}

fn input(backend: &dyn SessionBackend, request: Input) -> Value {
    match locate(backend, &request.key) {
        Err(why) => unknown(why),
        Ok(Located::Unknown) => unknown("several windows answer to the key"),
        Ok(Located::Absent) => known(json!("none")),
        Ok(Located::At(pane)) => match backend.send_text(&pane, &request.text, request.submit) {
            Ok(()) => known(json!("sent")),
            Err(e) => unknown(e),
        },
    }
}

fn capture(backend: &dyn SessionBackend, request: Capture) -> Value {
    let pane = match locate(backend, &request.key) {
        Err(why) => return unknown(why),
        Ok(Located::Unknown) => return unknown("several windows answer to the key"),
        Ok(Located::Absent) => return known(json!("none")),
        Ok(Located::At(pane)) => pane,
    };
    let lines = request.lines.unwrap_or(DEFAULT_CAPTURE_LINES);
    let text = match backend.capture(&pane, lines, false) {
        Ok(text) => text,
        Err(e) => return unknown(e),
    };
    let state = backend.pane_state(&pane).unwrap_or_default();
    known(json!({ "screen": {
        "text": text,
        "cursorRow": state.cursor_row,
        "cursorCol": state.cursor_col,
        "foreground": state.foreground_process,
        "title": state.title,
    }}))
}

/// One bounded command at the place, on this machine or through the
/// place's launcher (ssh, `wsl.exe`): its process group here is killed past
/// the deadline, and once it exits, and each stream is kept up to the cap.
/// The answer comes by the deadline even when a child that left the group
/// holds a stream open: what was read by then is the output. On a remote
/// place the group killed is the launcher's: a command there that ignores
/// its closed connection can outlive the deadline.
fn exec(place: Option<&HostDef>, request: Exec) -> Value {
    // The deadline starts here: on a remote place, reading its `PATH` is
    // part of the command's time.
    let deadline = Instant::now() + Duration::from_millis(request.timeout_ms);
    let cap = request
        .output_cap
        .unwrap_or(DEFAULT_OUTPUT_CAP)
        .min(MAX_OUTPUT_CAP);
    let mut command = match place {
        Some(host) => {
            match crate::place::exec_command(host, &request.argv, request.dir.as_deref(), deadline)
            {
                Ok(command) => command,
                Err(why) => return known(json!({ "failed": { "why": why } })),
            }
        }
        None => {
            let mut command = Command::new(&request.argv[0]);
            command.args(&request.argv[1..]);
            if let Some(dir) = &request.dir {
                command.current_dir(dir);
            }
            command
        }
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return known(json!({ "failed": { "why": e.to_string() } })),
    };
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        let kept = Arc::new(Mutex::new(Vec::new()));
        let (done, finished) = mpsc::channel::<()>();
        let into = Arc::clone(&kept);
        std::thread::spawn(move || {
            let Some(mut pipe) = pipe else { return };
            // The pipe is drained to its end, past the cap: a command that
            // writes more than is kept still finishes as it would.
            let mut chunk = [0u8; 64 * 1024];
            while let Ok(n @ 1..) = pipe.read(&mut chunk) {
                if let Ok(mut kept) = into.lock() {
                    let room = cap.saturating_sub(kept.len());
                    kept.extend_from_slice(&chunk[..n.min(room)]);
                }
            }
            let _ = done.send(());
        });
        (kept, finished)
    };
    let stdout = read(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = read(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => break None,
        }
    };
    // Whatever the command left in its group goes with it, so no grandchild
    // holds a pipe open past the bound.
    kill_tree(&mut child);
    let text = |(kept, finished): (Arc<Mutex<Vec<u8>>>, mpsc::Receiver<()>)| {
        // A killed command's pipes close at once: the grace only lets a
        // reader take what was already written.
        let until = deadline.max(Instant::now() + READ_GRACE);
        let _ = finished.recv_timeout(until.saturating_duration_since(Instant::now()));
        let kept = kept.lock().map(|k| k.clone()).unwrap_or_default();
        String::from_utf8_lossy(&kept).into_owned()
    };
    let (stdout, stderr) = (text(stdout), text(stderr));
    match status {
        Some(status) => known(json!({ "exited": {
            "code": status.code(),
            "stdout": stdout,
            "stderr": stderr,
        }})),
        None => known(json!({ "timedOut": { "stdout": stdout, "stderr": stderr } })),
    }
}

/// Kill `child`, if it still runs, and the process group it leads.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: a plain syscall on a group this process created for the
        // child (`process_group(0)`); a group already gone is ESRCH.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A start's directory is checked by the rules of the place it runs
    /// on, whatever machine this program runs on: a POSIX path for a POSIX
    /// place (an ssh host, a WSL distro), a drive or UNC path for Windows.
    #[test]
    fn a_directory_is_absolute_by_its_places_rules() {
        use crate::Platform::{Posix, Windows};
        for (dir, platform, absolute) in [
            ("/home/me/project", Posix, true),
            ("home/me", Posix, false),
            ("C:\\Users\\me", Posix, false),
            ("C:\\Users\\me", Windows, true),
            ("C:/Users/me", Windows, true),
            ("\\\\server\\share\\x", Windows, true),
            ("/home/me", Windows, false),
            ("Users\\me", Windows, false),
        ] {
            assert_eq!(
                absolute_on(dir, platform),
                absolute,
                "{dir} on {platform:?}"
            );
        }
    }

    /// A launch that names no file of Fleet's asset directory copies
    /// nothing, on any place, a native-Windows one included.
    #[test]
    fn a_launch_that_names_no_asset_file_copies_nothing() {
        let assets = tempfile::tempdir().expect("an asset dir");
        let dir = assets.path().to_string_lossy().into_owned();
        let windows = HostDef {
            name: "winbox".into(),
            destination: "me@winbox".into(),
            multiplexer: Some("psmux".into()),
            ..HostDef::default()
        };
        let mut bare = Start {
            key: "k.one".into(),
            argv: vec!["codex".into()],
            env: HashMap::from([(ASSET_DIR_ENV.to_string(), dir.clone())]),
            dir: None,
        };
        assert_eq!(place_assets(&windows, &mut bare), Ok(()));
        assert_eq!(bare.argv, ["codex"]);
    }

    #[test]
    fn every_key_has_a_window_name_of_its_own_and_reads_back() {
        for key in ["fleet.0f3a-uid", "a.b", "a_b", "a__d", "_u.", "x"] {
            let name = name_of(key);
            assert_eq!(
                crate::identity::sanitize_window_name(&name),
                name,
                "a window keeps the name as given"
            );
            assert_eq!(key_of(&name).as_deref(), Some(key));
        }
        assert_ne!(name_of("a.b"), name_of("a_b"));
        assert_eq!(key_of("a_x"), None);
        assert_eq!(key_of("trailing_"), None);
    }
}
