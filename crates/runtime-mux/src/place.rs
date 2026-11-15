//! A request's place (docs/runtime/README.md, "Places"): `local`, this
//! machine, or a machine reached over ssh or into a WSL distro. The place's
//! table in Fleet's config is parsed here into the host model the backends
//! are built from ([`HostDef`]). The commands runtime-mux runs on a remote
//! place outside its multiplexer run here too: the readiness probe, the
//! check for a binary on the place's `PATH`, `exec`, `workspace-git`,
//! `browse` and `branches`.
//!
//! ```text
//! request.place ──parse──▶ None (local) ──▶ this machine's backend
//!                     └──▶ Some(HostDef) ──▶ the place's backend (ssh / wsl.exe)
//! ```

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::contract::SessionBackend;
use crate::shell::HostLauncher;
use crate::{HostDef, HostKind, Platform};

/// The place that is this machine. It takes no config.
pub const LOCAL: &str = "local";

/// The bound on one probe of a place: an ssh connect, a login shell and a
/// `command -v`. ssh's own `ConnectTimeout` (5 s) ends an unreachable one
/// first.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The most a command run on a place may print on a stream: a `browse` of
/// a large directory or a `branches` of a repository with many refs fits
/// well inside it. Past it the run is an error, never a cut answer.
const PROBE_OUTPUT: u64 = 8 * 1024 * 1024;

/// How long a place's login `PATH`, read once, is used from the cache on
/// disk before it is read again. Each `runtime-mux` call is a process of its
/// own, and reading it costs an ssh connection and two login shells.
const PATH_CACHE_TTL: Duration = Duration::from_secs(600);

/// Where the place's `PATH` is cached: a directory only this user can write
/// to, and the prefix of the file names in it. Set once by the program.
static CACHE: OnceLock<(PathBuf, String)> = OnceLock::new();

/// Cache each place's login `PATH` in `dir`, under names starting with
/// `prefix`.
pub fn keep_cache_in(dir: PathBuf, prefix: String) {
    let _ = CACHE.set((dir, prefix));
}

/// Where the place's copies of Fleet's files are made, under its home: one
/// directory per key ([`copy_dir`]), for the harness asset files and for
/// the user's harness profiles.
pub const ASSETS_DIR: &str = ".local/state/fleet/assets";
pub const HARNESSES_DIR: &str = ".local/state/fleet/harnesses";

/// A place as a request names it: the name, and its table from Fleet's
/// config.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Place {
    pub name: String,
    #[serde(default)]
    pub config: Map<String, Value>,
}

/// How a remote place is reached.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Ssh,
    Wsl,
}

/// A remote place's table: the keys of a v2 `hosts.toml` entry that still
/// mean something to a runtime. Any other key is refused by name.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    kind: Kind,
    destination: Option<String>,
    distro: Option<String>,
    #[serde(default)]
    ssh_opts: Vec<String>,
    multiplexer: Option<String>,
    platform: Option<Platform>,
    socket: Option<String>,
    session: Option<String>,
    #[serde(default)]
    path_prepend: Vec<String>,
}

/// The place, parsed: `None` for `local`, else the host it names. A place
/// this program cannot serve is refused with why.
pub fn parse(place: &Place) -> Result<Option<HostDef>, String> {
    let name = &place.name;
    if name == LOCAL {
        if !place.config.is_empty() {
            return Err(format!(
                "place {LOCAL:?} is this machine and takes no config; runtime-mux cannot use {:?}",
                place.config.keys().collect::<Vec<_>>()
            ));
        }
        return Ok(None);
    }
    // The name goes into the lock and pause-mark file names, and into a
    // route, where `:` separates the multiplexer.
    let plain = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    if !plain {
        return Err(format!(
            "place {name:?}: use 1 to 64 ASCII letters, digits, '.', '-' or '_'"
        ));
    }
    if place.config.is_empty() {
        return Err(format!(
            "place {name:?} names no kind: give it kind = \"ssh\" and a destination, or kind = \"wsl\""
        ));
    }
    let config: Config = serde_json::from_value(Value::Object(place.config.clone()))
        .map_err(|e| format!("place {name:?}: {e}"))?;
    let kind = match config.kind {
        Kind::Ssh => {
            if config.destination.as_deref().is_none_or(str::is_empty) {
                return Err(format!(
                    "place {name:?}: kind = \"ssh\" needs a destination, such as \"me@devbox\""
                ));
            }
            if config.distro.is_some() {
                return Err(format!(
                    "place {name:?}: distro is for kind = \"wsl\", not \"ssh\""
                ));
            }
            HostKind::Ssh
        }
        Kind::Wsl => {
            if config.destination.is_some() || !config.ssh_opts.is_empty() {
                return Err(format!(
                    "place {name:?}: destination and ssh_opts are for kind = \"ssh\", not \"wsl\""
                ));
            }
            HostKind::Wsl
        }
    };
    if let Some(mux) = &config.multiplexer {
        crate::Multiplexer::parse(mux).map_err(|e| format!("place {name:?}: {e}"))?;
    }
    Ok(Some(HostDef {
        name: name.clone(),
        kind,
        destination: config.destination.unwrap_or_default(),
        distro: config.distro,
        socket: config.socket,
        session: config.session,
        ssh_opts: config.ssh_opts,
        multiplexer: config.multiplexer,
        platform: config.platform,
        path_prepend: config.path_prepend,
        ..HostDef::default()
    }))
}

/// How the place is reached, for a message: the command that shows whether
/// it answers.
fn reach(host: &HostDef) -> String {
    match HostLauncher::for_host(host) {
        HostLauncher::Ssh {
            destination,
            ssh_opts,
        } => {
            let opts: String = ssh_opts.iter().map(|o| format!(" {o}")).collect();
            format!("ssh -o BatchMode=yes{opts} {destination} true")
        }
        HostLauncher::Wsl { distro } => format!("wsl.exe -d {distro} true"),
    }
}

/// What a finished probe printed, and how it ended.
struct Ran {
    ok: bool,
    /// The launcher's own failure: ssh exits 255 when it cannot connect.
    unreached: bool,
    stdout: String,
    stderr: String,
}

/// The cache file of `host`'s `PATH`, and the line that says which
/// launcher and `path_prepend` it was read with: a place whose table
/// changed is read again.
fn cache_file(host: &HostDef) -> Option<(PathBuf, String)> {
    let (dir, prefix) = CACHE.get()?;
    let file = dir.join(format!("{prefix}{}.path", host.name));
    let identity = format!("{:?} {:?}", HostLauncher::for_host(host), host.path_prepend);
    Some((file, identity.replace('\n', " ")))
}

/// What the place's probe says ([`crate::host_path`]), within `budget`: the
/// answer this process has, else a fresh one from the cache on disk, else a
/// probe bounded by `budget`, which the cache then keeps.
fn env(host: &HostDef, budget: Duration) -> Option<crate::host_path::HostEnv> {
    let cached = cache_file(host);
    if let Some((file, identity)) = &cached {
        let fresh = std::fs::metadata(file)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| SystemTime::now().duration_since(at).ok())
            .is_some_and(|age| age < PATH_CACHE_TTL);
        let text = std::fs::read_to_string(file).ok().filter(|_| fresh);
        if let Some(env) = text.as_deref().and_then(|text| {
            let (first, rest) = text.split_once('\n')?;
            (first == identity).then(|| crate::host_path::parse_probe(rest))?
        }) {
            crate::host_path::remember(host, env.clone());
            return Some(env);
        }
    }
    let env = crate::host_path::env_within(host, budget)?;
    if let Some((file, identity)) = cached {
        // A cache that cannot be written costs the next call a probe.
        let tmp = file.with_extension("path.tmp");
        let text = format!("{identity}\n{}", crate::host_path::render(&env));
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &file);
        }
    }
    Some(env)
}

/// Read the place's `PATH` now, within the probe's budget, so the backend's
/// own commands find it without a probe of their own.
pub fn warm(host: &HostDef) {
    if host.platform() == Platform::Posix {
        let _ = env(host, PROBE_TIMEOUT);
    }
}

/// The place's `PATH` as an `sh` assignment, read within `budget`:
/// `path_prepend` alone when the login `PATH` was not read in time.
fn assignment(host: &HostDef, budget: Duration) -> String {
    let env = env(host, budget);
    crate::host_path::plan(&host.path_prepend, env.as_ref())
        .map(|plan| crate::host_path::shell_assignment(&plan))
        .unwrap_or_default()
}

/// `script` through `sh -c` on the place, with the place's `PATH`
/// ([`crate::host_path`]), `args` as its positional parameters, all of it
/// by `deadline`. `Err` when it did not finish in time or could not start.
fn run_script(
    host: &HostDef,
    script: &str,
    args: &[&str],
    deadline: Instant,
) -> Result<Ran, String> {
    if host.platform() != Platform::Posix {
        return Err(format!(
            "place {:?} is native Windows, which has no `sh` to run this in",
            host.name
        ));
    }
    let cmd = script_command(host, &assignment(host, left(deadline)), script, args);
    run_bounded(cmd, left(deadline)).map(|(code, stdout, stderr)| Ran {
        ok: code == Some(0),
        unreached: matches!(host.kind, HostKind::Ssh) && code == Some(255),
        stdout,
        stderr,
    })
}

/// A PowerShell `script` on a native-Windows place, after its
/// `path_prepend`, by `deadline`.
fn run_powershell(host: &HostDef, script: &str, deadline: Instant) -> Result<Ran, String> {
    let script = format!(
        "$ProgressPreference = 'SilentlyContinue'\n{}{script}",
        crate::shell::windows_path_prepend(&host.path_prepend)
    );
    let cmd =
        crate::shell::HostShell::PowerShell.run_script(&HostLauncher::for_host(host), &script);
    run_bounded(cmd, left(deadline)).map(|(code, stdout, stderr)| Ran {
        ok: code == Some(0),
        unreached: code == Some(255),
        stdout,
        stderr,
    })
}

/// The PowerShell that prints `@missing <name>` for each of `programs` the
/// place's `Path` does not find.
fn windows_lookup(programs: &[&str]) -> String {
    programs
        .iter()
        .map(|p| {
            let q = crate::shell::powershell_quote(p);
            format!("if (-not (Get-Command -Name {q} -CommandType Application -ErrorAction SilentlyContinue)) {{ '@missing ' + {q} }}\n")
        })
        .collect()
}

/// `script` through `sh -c` on the place, after `path`, the place's `PATH`
/// as an assignment, with `args` as its positional parameters.
fn script_command(host: &HostDef, path: &str, script: &str, args: &[&str]) -> Command {
    let mut words = vec!["sh"];
    words.extend_from_slice(args);
    HostLauncher::for_host(host).shell_c_args(&format!("{path}{script}"), &words)
}

/// The probe that `fleet` is on the place's `PATH`, which its agents' hooks
/// run to report.
const FLEET_PROBE: &str = "command -v fleet >/dev/null 2>&1 || echo @missing";

/// On a WSL place, the probe also looks for `flock`, which the waiter that
/// keeps the distro running needs ([`run_detached`]).
const FLOCK_PROBE: &str = "; command -v flock >/dev/null 2>&1 || echo @noflock";

/// The readiness probe of `host`.
fn probe_script(host: &HostDef) -> String {
    let flock = if host.is_wsl() { FLOCK_PROBE } else { "" };
    format!("{FLEET_PROBE}{flock}")
}

/// Why the place cannot run a session now, if it cannot: the launcher does
/// not reach it, its multiplexer is missing or too old, or `fleet` is not on
/// its `PATH`, so its agents' hooks cannot report. Each reason names the
/// command that fixes it.
pub fn readiness(
    host: &HostDef,
    backend: &dyn SessionBackend,
    deadline: Instant,
) -> Result<(), String> {
    let name = &host.name;
    if host.platform() == Platform::Posix {
        let ran = run_script(host, &probe_script(host), &[], deadline).map_err(|why| {
            format!(
                "place {name:?} did not answer: {why}; check `{}`",
                reach(host)
            )
        })?;
        if ran.unreached || !ran.ok {
            return Err(format!(
                "place {name:?} cannot be reached: {}; check that `{}` succeeds without asking anything (for a key, `ssh-copy-id`)",
                last_line(&ran.stderr),
                reach(host)
            ));
        }
        if ran.stdout.contains("@missing") {
            return Err(format!(
                "place {name:?}: `fleet` is not on its PATH, so its agents' hooks cannot report their state; install fleet there, or add its directory to the place's path_prepend"
            ));
        }
        if ran.stdout.contains("@noflock") {
            return Err(format!(
                "place {name:?}: `flock` is not in the distro, so runtime-mux cannot keep it running between calls and WSL would stop its sessions; install util-linux there"
            ));
        }
    } else {
        let ran = run_powershell(host, &windows_lookup(&["fleet"]), deadline).map_err(|why| {
            format!(
                "place {name:?} did not answer: {why}; check `{}`",
                reach(host)
            )
        })?;
        if ran.unreached || !ran.ok {
            return Err(format!(
                "place {name:?} cannot be reached: {}; check that `{}` succeeds without asking anything",
                last_line(&ran.stderr),
                reach(host)
            ));
        }
        if let Some(missing) = ran
            .stdout
            .lines()
            .find_map(|l| l.trim().strip_prefix("@missing "))
        {
            return Err(format!(
                "place {name:?}: `{missing}` is not on its Path; install it there, or add its directory to the place's path_prepend"
            ));
        }
    }
    backend.check_available().map_err(|e| {
        format!(
            "place {name:?}: {e}; {} on the place",
            host.default_route().mux.install_hint()
        )
    })
}

/// Why `program` cannot start on the place, if it is not on its `PATH`.
pub fn missing_program(host: &HostDef, program: &str) -> Option<String> {
    let name = &host.name;
    if host.platform() != Platform::Posix {
        return match run_powershell(host, &windows_lookup(&[program]), Instant::now() + PROBE_TIMEOUT) {
            Err(why) => Some(format!("place {name:?} did not answer: {why}")),
            Ok(ran) if !ran.ok => Some(format!(
                "place {name:?} cannot be reached: {}",
                last_line(&ran.stderr)
            )),
            Ok(ran) if ran.stdout.contains("@missing") => Some(format!(
                "{program} is not on the Path of place {name:?}: install it there, or add its directory to the place's path_prepend"
            )),
            Ok(_) => None,
        };
    }
    match run_script(
        host,
        "command -v \"$1\" >/dev/null 2>&1 || echo @missing",
        &[program],
        Instant::now() + PROBE_TIMEOUT,
    ) {
        Err(why) => Some(format!("place {name:?} did not answer: {why}")),
        Ok(ran) if !ran.ok => Some(format!(
            "place {name:?} cannot be reached: {}",
            last_line(&ran.stderr)
        )),
        Ok(ran) if ran.stdout.contains("@missing") => Some(format!(
            "{program} is not on the PATH of place {name:?}: install it there, or add its directory to the place's path_prepend"
        )),
        Ok(_) => None,
    }
}

/// `argv` on the place, in `dir` when given, with the place's `PATH`: what
/// `exec` runs there. The `PATH` is read by `deadline`, the command's own,
/// and when it is not, only `path_prepend` leads it. A native-Windows place
/// runs it from an encoded PowerShell script, with `path_prepend` first on
/// its `Path` ([`crate::shell::windows_program_script`]).
pub fn exec_command(
    host: &HostDef,
    argv: &[String],
    dir: Option<&Path>,
    deadline: Instant,
) -> Result<Command, String> {
    if host.platform() != Platform::Posix {
        let dir = dir.map(|d| d.to_string_lossy().into_owned());
        let script = crate::shell::windows_program_script(
            &argv[0],
            &argv[1..],
            dir.as_deref(),
            &host.path_prepend,
            "",
        );
        return Ok(
            crate::shell::HostShell::PowerShell.run_script(&HostLauncher::for_host(host), &script)
        );
    }
    let path = assignment(host, left(deadline));
    let dir = dir
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut words: Vec<&str> = vec!["sh", &dir];
    words.extend(argv.iter().map(String::as_str));
    Ok(HostLauncher::for_host(host).shell_c_args(
        &format!("{path}if [ -n \"$1\" ]; then cd \"$1\" || exit 126; fi; shift; exec \"$@\""),
        &words,
    ))
}

/// How long a `workspace-git` call on a place may take: a first clone of a
/// repository can take minutes. Fleet's own bound on a `workspace` call
/// (600 s unless configured) ends it first.
const WORKSPACE_TIMEOUT: Duration = Duration::from_secs(600);

/// `workspace-git <verb>` on the place, with `request` on its stdin: its
/// exit code and what it printed. `Err` when the place did not answer.
pub fn workspace_git(
    host: &HostDef,
    verb: &str,
    request: &[u8],
) -> Result<(Option<i32>, String, String), String> {
    let argv = ["workspace-git".to_string(), verb.to_string()];
    let mut cmd = exec_command(host, &argv, None, Instant::now() + PROBE_TIMEOUT)?;
    cmd.stdin(Stdio::piped());
    run_fed(cmd, request, WORKSPACE_TIMEOUT)
}

/// The directories a workspace of `file://` URLs only names on the place,
/// each checked to be there: what a place without `workspace-git` makes.
/// `None` when a URL is not `file://`, which only `workspace-git` makes.
pub fn file_dirs(host: &HostDef, urls: &[String]) -> Option<Reading<Vec<String>>> {
    let mut dirs = Vec::new();
    for url in urls {
        let path = url.strip_prefix("file://")?;
        match percent_decode(path).filter(|p| p.starts_with('/') && !path.contains(['?', '#'])) {
            Some(dir) => dirs.push(dir),
            None => return Some(Ok(Err(format!("{url} names no absolute path")))),
        }
    }
    let args: Vec<&str> = dirs.iter().map(String::as_str).collect();
    let ran = match read(
        host,
        "for d; do [ -d \"$d\" ] || printf '@missing %s\\n' \"$d\"; done",
        &args,
    ) {
        Ok(Ok(ran)) => ran,
        other => return Some(other.map(|r| r.map(|_| Vec::new()))),
    };
    Some(Ok(
        match ran.lines().find_map(|l| l.strip_prefix("@missing ")) {
            Some(missing) => Err(format!(
                "{missing} is not a directory on place {:?}",
                host.name
            )),
            None => Ok(dirs),
        },
    ))
}

/// A URL path's `%XX` escapes decoded; `None` when one is malformed or the
/// result is not UTF-8.
fn percent_decode(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The place's home directory, as its login shell reads it.
pub fn home(host: &HostDef) -> Result<String, String> {
    if host.platform() != Platform::Posix {
        // Spelled with `/`, which Windows takes, so a path built under it
        // is one spelling.
        return run_powershell(
            host,
            "[Console]::Out.Write($env:USERPROFILE)",
            Instant::now() + PROBE_TIMEOUT,
        )
        .ok()
        .filter(|ran| ran.ok && !ran.stdout.trim().is_empty())
        .map(|ran| ran.stdout.trim().replace('\\', "/"))
        .ok_or_else(|| {
            format!(
                "place {:?}: its home could not be read; check `{}`",
                host.name,
                reach(host)
            )
        });
    }
    env(host, PROBE_TIMEOUT)
        .and_then(|env| env.home)
        .ok_or_else(|| {
            format!(
                "place {:?}: its home could not be read; check `{}`",
                host.name,
                reach(host)
            )
        })
}

/// `command` through `sh -c` in `dir` on the place, with the place's `PATH`:
/// a workspace step. `Err` names why it did not succeed.
pub fn run_in(host: &HostDef, dir: &str, command: &str) -> Result<(), String> {
    let ran = run_script(
        host,
        "cd \"$1\" || exit 126; sh -c \"$2\"",
        &[dir, command],
        Instant::now() + WORKSPACE_TIMEOUT,
    )?;
    if ran.ok {
        return Ok(());
    }
    Err(format!(
        "`{command}` in {dir} on place {:?} failed: {}",
        host.name,
        last_line(&ran.stderr)
    ))
}

/// Whether `path` is a file on the place; `Err` when the place cannot
/// tell.
pub fn is_file(host: &HostDef, path: &str) -> Result<bool, String> {
    test_path(host, "-f", path)
}

/// Whether anything is at `path` on the place; `Err` when the place cannot
/// tell.
pub fn exists(host: &HostDef, path: &str) -> Result<bool, String> {
    test_path(host, "-e", path)
}

/// `test <flag> path` on the place.
fn test_path(host: &HostDef, flag: &str, path: &str) -> Result<bool, String> {
    let ran = run_script(
        host,
        &format!("if [ {flag} \"$1\" ]; then echo @file; fi"),
        &[path],
        Instant::now() + PROBE_TIMEOUT,
    )
    .map_err(|why| format!("place {:?} did not answer: {why}", host.name))?;
    if !ran.ok {
        return Err(format!(
            "place {:?} cannot be reached: {}",
            host.name,
            last_line(&ran.stderr)
        ));
    }
    Ok(ran.stdout.contains("@file"))
}

/// An empty file at `path` on the place. Best effort.
pub fn touch(host: &HostDef, path: &str) {
    let _ = run_script(host, ": > \"$1\"", &[path], Instant::now() + PROBE_TIMEOUT);
}

/// What a read of the place answers: `Ok(Ok(_))` the reading, `Ok(Err(why))`
/// a reading that failed there, `Err(why)` a place that did not answer.
pub type Reading<T> = Result<Result<T, String>, String>;

/// The subdirectories of `dir`, or of the place's home when `None`, each
/// with whether it is a git work tree by the rule `browse` reads one with
/// on this machine: a `.git` directory holding `HEAD`, `objects` and
/// `refs`, or a `.git` file naming one. The directory as read, and its
/// entries unsorted. Each entry is one NUL-ended record, so any name
/// arrives whole.
pub fn browse(host: &HostDef, dir: Option<&str>) -> Reading<(String, Vec<(String, bool)>)> {
    if let Some(why) = posix_only(host, "browse") {
        return Ok(Err(why));
    }
    const SCRIPT: &str = "d=${1:-$HOME}; printf '%s\\0' \"$d\"; \
        if ! [ -d \"$d\" ]; then printf '@failed: no such directory\\0'; exit 0; fi; \
        if ! [ -r \"$d\" ] || ! [ -x \"$d\" ]; then printf '@failed: permission denied\\0'; exit 0; fi; \
        for e in \"$d\"/* \"$d\"/.[!.]* \"$d\"/..?*; do \
          [ -d \"$e\" ] || continue; r=0; \
          if [ -d \"$e/.git\" ]; then \
            [ -f \"$e/.git/HEAD\" ] && [ -d \"$e/.git/objects\" ] && [ -d \"$e/.git/refs\" ] && r=1; \
          elif [ -f \"$e/.git\" ] && [ \"$(head -c 8 \"$e/.git\")\" = 'gitdir: ' ]; then r=1; fi; \
          printf '%s%s\\0' \"$r\" \"${e##*/}\"; \
        done";
    let ran = match read(host, SCRIPT, &[dir.unwrap_or_default()])? {
        Ok(ran) => ran,
        Err(why) => return Ok(Err(why)),
    };
    let mut records = ran.split('\0');
    let dir = records.next().unwrap_or_default().to_string();
    let mut entries = Vec::new();
    for record in records.filter(|r| !r.is_empty()) {
        if let Some(why) = record.strip_prefix("@failed") {
            return Ok(Err(format!("{dir}{why}")));
        }
        let (repo, name) = record.split_at(1);
        entries.push((name.to_string(), repo == "1"));
    }
    Ok(Ok((dir, entries)))
}

/// What `branches` reads of a repository on the place: its refs under
/// `refs/heads` and `refs/remotes/origin`, `origin/HEAD`'s target and the
/// branch checked out, each as git prints it, or why there is none.
pub fn git_refs(host: &HostDef, repo: &str) -> Reading<[String; 3]> {
    if let Some(why) = posix_only(host, "branches") {
        return Ok(Err(why));
    }
    const SCRIPT: &str = "r=$1; \
        if [ -d \"$r/.git\" ]; then \
          [ -f \"$r/.git/HEAD\" ] && [ -d \"$r/.git/objects\" ] && [ -d \"$r/.git/refs\" ] || r=; \
        elif ! { [ -f \"$r/.git\" ] && [ \"$(head -c 8 \"$r/.git\")\" = 'gitdir: ' ]; }; then r=; fi; \
        if [ -z \"$r\" ]; then printf '@norepo'; exit 0; fi; \
        unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR GIT_OBJECT_DIRECTORY \
          GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_NAMESPACE; \
        GIT_CEILING_DIRECTORIES=${r%/*}; export GIT_CEILING_DIRECTORIES; \
        cd \"$r\" || exit 1; \
        git for-each-ref --format='%(refname)' refs/heads refs/remotes/origin || exit 1; \
        printf '\\0'; git symbolic-ref -q refs/remotes/origin/HEAD; \
        printf '\\0'; git symbolic-ref -q --short HEAD; exit 0";
    let ran = match read(host, SCRIPT, &[repo])? {
        Ok(ran) => ran,
        Err(why) => return Ok(Err(why)),
    };
    if ran == "@norepo" {
        return Ok(Err(format!("{repo} is no git repository")));
    }
    let mut parts = ran.splitn(3, '\0').map(str::to_string);
    let mut next = || parts.next().unwrap_or_default();
    Ok(Ok([next(), next(), next()]))
}

/// Why `verb` cannot run on `host`, a native-Windows place: it reads the
/// place with an `sh` script.
fn posix_only(host: &HostDef, verb: &str) -> Option<String> {
    (host.platform() != Platform::Posix).then(|| {
        format!(
            "{verb} is not supported on place {:?}, a native-Windows place: it reads a place with `sh`",
            host.name
        )
    })
}

/// A read-only script's stdout on the place: `Ok(Err(_))` when it failed
/// there, `Err(_)` when the place did not answer.
fn read(host: &HostDef, script: &str, args: &[&str]) -> Reading<String> {
    let name = &host.name;
    let ran = run_script(host, script, args, Instant::now() + PROBE_TIMEOUT)
        .map_err(|why| format!("place {name:?} did not answer: {why}"))?;
    if ran.unreached {
        return Err(format!(
            "place {name:?} cannot be reached: {}",
            last_line(&ran.stderr)
        ));
    }
    if !ran.ok {
        return Ok(Err(last_line(&ran.stderr)));
    }
    Ok(Ok(ran.stdout))
}

/// Run `cmd`, a server's waiter ([`WindowLifecycle::server_waiter`](crate::contract::WindowLifecycle::server_waiter)), left
/// to run after this program exits: on a WSL place it is the `wsl.exe`
/// client that keeps the distro, and so the server, running. WSL stops a
/// distro, and every process in it, soon after its last `wsl.exe` client
/// exits, and each `runtime-mux` call is one such client that exits at once.
/// Best effort: a waiter that cannot start leaves the server to WSL's idle
/// stop.
pub fn run_detached(mut cmd: Command) {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn GetStdHandle(which: u32) -> *mut std::ffi::c_void;
            fn SetHandleInformation(handle: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
        }
        /// `STD_INPUT_HANDLE`, `STD_OUTPUT_HANDLE`, `STD_ERROR_HANDLE`.
        const STD_HANDLES: [u32; 3] = [-10i32 as u32, -11i32 as u32, -12i32 as u32];
        const HANDLE_FLAG_INHERIT: u32 = 1;
        // A child inherits every inheritable handle of its parent, and this
        // program's own stdout is its caller's pipe: a waiter holding it
        // open would keep the caller from ever reading the answer's end.
        for which in STD_HANDLES {
            // SAFETY: plain calls on this process's own standard handles; a
            // missing one is null or invalid, which the call refuses.
            unsafe {
                SetHandleInformation(GetStdHandle(which), HANDLE_FLAG_INHERIT, 0);
            }
        }
        /// No window, a process group of its own, and out of the caller's
        /// job, which a caller may kill whole when it ends.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        if cmd.spawn().is_ok() {
            return;
        }
        // A job that refuses a breakaway: the waiter stays in it.
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
    let _ = cmd.spawn();
}

/// The time left until `deadline`.
fn left(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// The place's own copy of the directory `local`, for the session under
/// `key`: `local` copied, whole, to `~/<under>/<key>` on the place
/// ([`ASSETS_DIR`], [`HARNESSES_DIR`]), replacing what an earlier start left
/// there. Its absolute path on the place, which the caller names in place
/// of `local`.
pub fn copy_dir(host: &HostDef, key: &str, local: &Path, under: &str) -> Result<String, String> {
    let name = &host.name;
    let home = home(host)?;
    let remote = format!("{}/{under}/{key}", home.trim_end_matches('/'));
    let mut tar = Command::new("tar")
        .arg("-C")
        .arg(local)
        .args(["-cf", "-", "."])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run tar: {e}"))?;
    // Windows has had `tar.exe` since Windows 10 1803; the script makes the
    // directory afresh, then starts it on the stream it inherits.
    let mut copy = if host.platform() == Platform::Posix {
        HostLauncher::for_host(host).shell_c_args(
            "rm -rf \"$1\" && mkdir -p \"$1\" && tar -xf - -C \"$1\"",
            &["sh", &remote],
        )
    } else {
        let q = crate::shell::powershell_quote(&remote);
        let prelude = format!(
            "Remove-Item -LiteralPath {q} -Recurse -Force -ErrorAction SilentlyContinue\n\
             New-Item -ItemType Directory -Force -Path {q} | Out-Null\n"
        );
        let script = crate::shell::windows_program_script(
            "tar.exe",
            &["-xf", "-", "-C", &remote],
            None,
            &[],
            &prelude,
        );
        crate::shell::HostShell::PowerShell.run_script(&HostLauncher::for_host(host), &script)
    };
    copy.stdin(tar.stdout.take().map_or_else(Stdio::null, Stdio::from));
    let ran = run_piped(copy, PROBE_TIMEOUT);
    let _ = tar.wait();
    match ran {
        Ok((Some(0), _, _)) => Ok(remote),
        Ok((_, _, stderr)) => Err(format!(
            "place {name:?}: the harness's files were not copied to {remote}: {}",
            last_line(&stderr)
        )),
        Err(why) => Err(format!(
            "place {name:?}: copying the harness's files: {why}"
        )),
    }
}

/// Remove the key's copies of Fleet's files on the place, if any are
/// there. Best effort: a copy left behind is replaced by the key's next
/// start.
pub fn remove_assets(host: &HostDef, key: &str) {
    if host.platform() != Platform::Posix {
        let dirs = [ASSETS_DIR, HARNESSES_DIR]
            .map(|under| crate::shell::powershell_quote(&format!("/{under}/{key}")))
            .map(|rest| format!("Remove-Item -LiteralPath ($env:USERPROFILE + {rest}) -Recurse -Force -ErrorAction SilentlyContinue\n"))
            .concat();
        let _ = run_powershell(host, &dirs, Instant::now() + PROBE_TIMEOUT);
        return;
    }
    let _ = run_script(
        host,
        &format!("rm -rf \"$HOME/{ASSETS_DIR}/$1\" \"$HOME/{HARNESSES_DIR}/$1\""),
        &[key],
        Instant::now() + PROBE_TIMEOUT,
    );
}

fn last_line(text: &str) -> String {
    let line = text
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    if line.is_empty() {
        "the launcher failed and said nothing".into()
    } else {
        line.trim().to_string()
    }
}

/// `cmd` run to its end or killed at `timeout`: its exit code and what it
/// printed.
fn run_bounded(
    mut cmd: Command,
    timeout: Duration,
) -> Result<(Option<i32>, String, String), String> {
    cmd.stdin(Stdio::null());
    run_piped(cmd, timeout)
}

/// [`run_piped`] with `input` written to `cmd`'s piped stdin, from a thread
/// of its own so a command that answers before it reads all of it never
/// blocks the writer against the reader.
fn run_fed(
    mut cmd: Command,
    input: &[u8],
    timeout: Duration,
) -> Result<(Option<i32>, String, String), String> {
    use std::io::Write;
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot run {:?}: {e}", cmd.get_program()))?;
    if let Some(mut stdin) = child.stdin.take() {
        let input = input.to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    wait_bounded(child, timeout)
}

/// [`run_bounded`] with the stdin `cmd` already has.
fn run_piped(mut cmd: Command, timeout: Duration) -> Result<(Option<i32>, String, String), String> {
    let child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {:?}: {e}", cmd.get_program()))?;
    wait_bounded(child, timeout)
}

/// `child` waited for to its end or killed at `timeout`: its exit code and
/// what it printed on its piped stdout and stderr.
fn wait_bounded(
    mut child: std::process::Child,
    timeout: Duration,
) -> Result<(Option<i32>, String, String), String> {
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(pipe) = pipe {
                let mut capped = pipe.take(PROBE_OUTPUT + 1);
                let _ = capped.read_to_end(&mut out);
                let _ = std::io::copy(&mut capped.into_inner(), &mut std::io::sink());
            }
            let _ = tx.send(out);
        });
        rx
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
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("no answer within {} s", timeout.as_secs()));
            }
        }
    };
    let text = |rx: std::sync::mpsc::Receiver<Vec<u8>>| {
        let out = rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default();
        if out.len() as u64 > PROBE_OUTPUT {
            return Err(format!("it printed more than {PROBE_OUTPUT} bytes"));
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    };
    Ok((status.code(), text(stdout)?, text(stderr)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn place(name: &str, config: Value) -> Place {
        serde_json::from_value(json!({ "name": name, "config": config })).unwrap()
    }

    /// A WSL place's readiness looks for `fleet` inside the distro, on the
    /// `PATH` its sessions get: `wsl.exe` enters it and runs the probe after
    /// the place's `PATH` assignment.
    #[test]
    fn a_wsl_place_looks_for_fleet_on_its_own_path() {
        let host = parse(&place("dev", json!({"kind": "wsl", "distro": "Ubuntu"})))
            .unwrap()
            .unwrap();
        let path = "PATH=/home/me/.cargo/bin:/usr/bin; export PATH; ";
        let probe = probe_script(&host);
        assert!(probe.ends_with(FLOCK_PROBE), "{probe}");
        let cmd = script_command(&host, path, &probe, &[]);
        let argv: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            cmd.get_program().to_string_lossy().ends_with("wsl.exe"),
            "{:?}",
            cmd.get_program()
        );
        // `wsl.exe` is given a neutral `--cd /` only from a Unix machine
        // (`shell::wsl_command`).
        let mut want = vec!["-d".to_string(), "Ubuntu".to_string()];
        if cfg!(unix) {
            want.extend(["--cd".to_string(), "/".to_string()]);
        }
        want.extend(["-e", "sh", "-c", &format!("{path}{probe}"), "sh"].map(String::from));
        assert_eq!(argv, want);
    }

    /// Output past the cap is an error, never a cut answer passed as whole.
    #[cfg(unix)]
    #[test]
    fn output_past_the_cap_is_an_error_not_a_cut_answer() {
        let run = |bytes: u64| {
            let mut cmd = Command::new("head");
            cmd.args(["-c", &bytes.to_string(), "/dev/zero"]);
            run_bounded(cmd, Duration::from_secs(30))
        };
        assert_eq!(
            run(PROBE_OUTPUT).map(|(_, out, _)| out.len() as u64),
            Ok(PROBE_OUTPUT)
        );
        let why = run(PROBE_OUTPUT + 1).unwrap_err();
        assert!(why.contains("more than"), "{why}");
    }

    #[test]
    fn local_takes_no_config() {
        assert_eq!(parse(&place("local", json!({}))), Ok(None));
        let why = parse(&place("local", json!({"kind": "ssh"}))).unwrap_err();
        assert!(why.contains("takes no config"), "{why}");
    }

    #[test]
    fn an_ssh_place_becomes_the_host_it_names() {
        let host = parse(&place(
            "devbox",
            json!({
                "kind": "ssh",
                "destination": "me@devbox",
                "ssh_opts": ["-p", "2222"],
                "multiplexer": "tmux",
                "platform": "posix",
                "socket": "fleet",
                "session": "agents",
                "path_prepend": ["~/.local/bin"],
            }),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(host.name, "devbox");
        assert_eq!(host.kind, HostKind::Ssh);
        assert_eq!(host.destination, "me@devbox");
        assert_eq!(host.ssh_opts, ["-p", "2222"]);
        assert_eq!(host.socket.as_deref(), Some("fleet"));
        assert_eq!(host.session.as_deref(), Some("agents"));
        assert_eq!(host.path_prepend, ["~/.local/bin"]);
        assert_eq!(host.platform(), Platform::Posix);
    }

    #[test]
    fn a_wsl_place_takes_an_optional_distro() {
        let host = parse(&place("ubuntu", json!({"kind": "wsl"})))
            .unwrap()
            .unwrap();
        assert!(host.is_wsl());
        assert_eq!(host.distro_name(), "ubuntu");
        let host = parse(&place(
            "dev",
            json!({"kind": "wsl", "distro": "Ubuntu-24.04"}),
        ))
        .unwrap()
        .unwrap();
        assert_eq!(host.distro_name(), "Ubuntu-24.04");
    }

    #[test]
    fn a_place_that_cannot_be_served_is_refused_by_what_is_wrong() {
        for (name, config, why) in [
            ("devbox", json!({}), "names no kind"),
            ("devbox", json!({"kind": "ssh"}), "needs a destination"),
            ("devbox", json!({"kind": "vm"}), "unknown variant `vm`"),
            (
                "devbox",
                json!({"kind": "ssh", "destination": "d", "runtime": "mux"}),
                "unknown field `runtime`",
            ),
            (
                "devbox",
                json!({"kind": "ssh", "destination": "d", "worktrees_dir": "/w"}),
                "unknown field `worktrees_dir`",
            ),
            (
                "ubuntu",
                json!({"kind": "wsl", "destination": "d"}),
                "for kind = \"ssh\"",
            ),
            (
                "devbox",
                json!({"kind": "ssh", "destination": "d", "distro": "u"}),
                "for kind = \"wsl\"",
            ),
            (
                "devbox",
                json!({"kind": "ssh", "destination": "d", "multiplexer": "screen"}),
                "screen",
            ),
            (
                "dev:box",
                json!({"kind": "ssh", "destination": "d"}),
                "letters, digits",
            ),
        ] {
            let got = parse(&place(name, config.clone())).unwrap_err();
            assert!(got.contains(why), "{config}: {got}");
        }
    }
}
