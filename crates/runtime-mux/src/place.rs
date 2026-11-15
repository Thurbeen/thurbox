//! A request's place (docs/runtime/README.md, "Places"): `local`, this
//! machine, or a machine reached over ssh or into a WSL distro. The place's
//! table in Fleet's config is parsed here into the host model the backends
//! are built from ([`HostDef`]). The commands runtime-mux runs on a remote
//! place outside its multiplexer run here too: the readiness probe, the
//! check for a binary on the place's `PATH`, `exec`, and the check of a
//! `file://` workspace.
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

/// What a probe keeps of a stream.
const PROBE_OUTPUT: u64 = 64 * 1024;

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

/// Where the place's assets are copied, under its home: one directory per
/// key ([`copy_assets`]).
const ASSETS_DIR: &str = ".local/state/fleet/assets";

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
    let cmd = script_command(host, &assignment(host, left(deadline)), script, args);
    run_bounded(cmd, left(deadline)).map(|(code, stdout, stderr)| Ran {
        ok: code == Some(0),
        unreached: matches!(host.kind, HostKind::Ssh) && code == Some(255),
        stdout,
        stderr,
    })
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
        let ran = run_script(host, FLEET_PROBE, &[], deadline).map_err(|why| {
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
    if host.platform() != Platform::Posix {
        return None;
    }
    let name = &host.name;
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
/// and when it is not, only `path_prepend` leads it. Only a POSIX place has
/// the `sh` it runs through.
pub fn exec_command(
    host: &HostDef,
    argv: &[String],
    dir: Option<&Path>,
    deadline: Instant,
) -> Result<Command, String> {
    if host.platform() != Platform::Posix {
        return Err(format!(
            "exec on place {:?}: a native-Windows place is not supported yet",
            host.name
        ));
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

/// What a `workspace` call on a remote place answers.
pub enum Workspace {
    /// One directory per URL, each a directory on the place.
    Dirs(Vec<String>),
    /// Nothing can be made: why.
    Failed(String),
    /// The place did not answer.
    Unknown(String),
}

/// A remote place's workspace: `file://` paths already on the place, used
/// as they are. Nothing is made there yet, so a `git+…` URL is refused, and
/// nothing is removed at a release.
pub fn workspace(host: &HostDef, urls: &[String]) -> Workspace {
    let name = &host.name;
    let mut dirs = Vec::new();
    for url in urls {
        let path = match url.strip_prefix("file://") {
            Some(path) if !path.contains(['?', '#']) => percent_decode(path),
            Some(_) => {
                return Workspace::Failed(format!(
                    "{url}: a file:// URL takes no query or fragment"
                ))
            }
            None => {
                let scheme = url.split("://").next().unwrap_or(url);
                return Workspace::Failed(format!(
                    "{url}: a {scheme} workspace is not yet supported on a remote place ({name:?}); name a directory already on the place as a file:// URL"
                ));
            }
        };
        match path {
            Some(path) if path.starts_with('/') => dirs.push(path),
            _ => return Workspace::Failed(format!("{url} names no absolute path")),
        }
    }
    if dirs.is_empty() {
        return Workspace::Dirs(dirs);
    }
    let args: Vec<&str> = dirs.iter().map(String::as_str).collect();
    let ran = match run_script(
        host,
        "for d; do [ -d \"$d\" ] || printf '@missing %s\\n' \"$d\"; done",
        &args,
        Instant::now() + PROBE_TIMEOUT,
    ) {
        Ok(ran) => ran,
        Err(why) => return Workspace::Unknown(format!("place {name:?} did not answer: {why}")),
    };
    if !ran.ok {
        return Workspace::Unknown(format!(
            "place {name:?} cannot be reached: {}",
            last_line(&ran.stderr)
        ));
    }
    if let Some(missing) = ran.stdout.lines().find_map(|l| l.strip_prefix("@missing ")) {
        return Workspace::Failed(format!("{missing} is not a directory on place {name:?}"));
    }
    Workspace::Dirs(dirs)
}

/// The time left until `deadline`.
fn left(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// The place's own copy of the asset directory `local`, for the session
/// under `key`: `local` copied, whole, to `~/.local/state/fleet/assets/<key>`
/// on the place, replacing what an earlier start left there. Its absolute
/// path on the place, which the caller names in place of `local`.
pub fn copy_assets(host: &HostDef, key: &str, local: &Path) -> Result<String, String> {
    let name = &host.name;
    if host.platform() != Platform::Posix {
        return Err(format!(
            "place {name:?}: this harness's hook files cannot be copied to a native-Windows place yet; run it on a POSIX place, or use a harness whose hooks are inline, such as claude or codex"
        ));
    }
    let home = env(host, PROBE_TIMEOUT)
        .and_then(|env| env.home)
        .ok_or_else(|| format!("place {name:?}: its home could not be read"))?;
    let remote = format!("{}/{ASSETS_DIR}/{key}", home.trim_end_matches('/'));
    let mut tar = Command::new("tar")
        .arg("-C")
        .arg(local)
        .args(["-cf", "-", "."])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run tar: {e}"))?;
    let mut copy = HostLauncher::for_host(host).shell_c_args(
        "rm -rf \"$1\" && mkdir -p \"$1\" && tar -xf - -C \"$1\"",
        &["sh", &remote],
    );
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

/// Remove the key's copy of the assets on the place, if one is there. Best
/// effort: a copy left behind is replaced by the key's next start.
pub fn remove_assets(host: &HostDef, key: &str) {
    if host.platform() != Platform::Posix {
        return;
    }
    let _ = run_script(
        host,
        &format!("rm -rf \"$HOME/{ASSETS_DIR}/$1\""),
        &[key],
        Instant::now() + PROBE_TIMEOUT,
    );
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

/// [`run_bounded`] with the stdin `cmd` already has.
fn run_piped(mut cmd: Command, timeout: Duration) -> Result<(Option<i32>, String, String), String> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {:?}: {e}", cmd.get_program()))?;
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(pipe) = pipe {
                let mut capped = pipe.take(PROBE_OUTPUT);
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
        String::from_utf8_lossy(&out).into_owned()
    };
    Ok((status.code(), text(stdout), text(stderr)))
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
        let cmd = script_command(&host, path, FLEET_PROBE, &[]);
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
        want.extend(["-e", "sh", "-c", &format!("{path}{FLEET_PROBE}"), "sh"].map(String::from));
        assert_eq!(argv, want);
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

    #[test]
    fn only_a_file_url_is_a_remote_workspace() {
        let host = parse(&place("devbox", json!({"kind": "ssh", "destination": "d"})))
            .unwrap()
            .unwrap();
        match workspace(&host, &["git+https://example.test/app.git".into()]) {
            Workspace::Failed(why) => {
                assert!(why.contains("not yet supported on a remote place"), "{why}")
            }
            _ => panic!("a git+ URL is refused"),
        }
        match workspace(&host, &["file://relative".into()]) {
            Workspace::Failed(why) => assert!(why.contains("no absolute path"), "{why}"),
            _ => panic!("a relative path is refused"),
        }
        assert!(matches!(workspace(&host, &[]), Workspace::Dirs(d) if d.is_empty()));
        assert_eq!(percent_decode("/a%20b").as_deref(), Some("/a b"));
        assert_eq!(percent_decode("/a%2"), None);
    }
}
