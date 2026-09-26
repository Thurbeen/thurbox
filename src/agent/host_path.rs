//! The `PATH` an agent gets on an SSH host or inside a WSL distro.
//!
//! Every command thurbox runs on a host goes through a launcher that hands it
//! a **non-login** environment: `wsl.exe -e sh -c …` bypasses the user's shell
//! altogether (the distro's default `PATH` plus the translated Windows one),
//! and sshd runs a command with its own compiled-in `PATH`. Neither reads
//! `~/.profile`, `~/.zprofile` or — for `wsl.exe -e` — even `~/.zshenv`, which
//! is where `~/.local/bin`, `~/.cargo/bin`, `~/.bun/bin` and the nvm/fnm shims
//! go. A delegated `thurbox-cli session create` (ADR-24) inherited exactly that
//! environment and pinned it on the pane (`agent::tmux::path_prefix_args`), so
//! an agent installed under `~/.local/bin` died with `env: 'claude': No such
//! file or directory`.
//!
//! So the host's own login `PATH` is read once per host — the user's `$SHELL
//! -l` and `/bin/sh -l`, stdin closed, each under `timeout` — cached for the
//! process, and spliced in front of what the launcher gives:
//! `path_prepend` (hosts.toml), then the login shells', then the launcher's,
//! de-duplicated in that order. A host whose shells cannot be read keeps
//! today's `PATH` with a warning; nothing here can fail a session.

use std::collections::HashMap;
use std::io::Read;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use crate::session::HostDef;
use crate::shell::{posix_quote, HostLauncher};

/// Each login shell's own budget on the host, enforced there by `timeout(1)`
/// when the host has it.
const SHELL_TIMEOUT_SECS: u32 = 5;

/// The whole probe's budget on this side — launcher start, two login shells,
/// and an ssh connection — after which the launcher is killed. Covers a host
/// with no `timeout(1)`, where only this bounds an rc file waiting on a lock.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How much of the probe's output is kept — far more than three `PATH`s.
const MAX_PROBE_OUTPUT: u64 = 64 * 1024;

/// What the probe read on a host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostEnv {
    /// `$HOME`, for expanding `~` in `path_prepend`.
    pub home: Option<String>,
    /// The launcher's own `PATH` — what a delegated CLI inherits today.
    pub base: Vec<String>,
    /// `$SHELL -lc` (the passwd shell when `$SHELL` is unset). `None` when it
    /// could not be read.
    pub shell_login: Option<Vec<String>>,
    /// `/bin/sh -lc`: `~/.profile`, the one login file every POSIX shell's
    /// user is likely to have, and the fallback for an unreadable `$SHELL`.
    pub sh_login: Option<Vec<String>>,
}

/// The probe, as one `sh -c` script.
///
/// Each login shell prints its `PATH` behind a sentinel and the part after the
/// **last** sentinel is kept, so an rc file that echoes something on a login
/// shell cannot end up as a `PATH` component. `-l` without `-i` and stdin on
/// `/dev/null`: nothing here can print a prompt or wait for input.
pub fn probe_script() -> String {
    format!(
        "exec </dev/null; t=; command -v timeout >/dev/null 2>&1 && t='timeout {SHELL_TIMEOUT_SECS}'; \
         printf '@home %s\\n' \"$HOME\"; printf '@base %s\\n' \"$PATH\"; \
         r() {{ p=$($t \"$1\" -lc 'printf \"\\n@@PATH=%s\" \"$PATH\"' 2>/dev/null); \
         case $p in *@@PATH=*) printf '@%s %s\\n' \"$2\" \"${{p##*@@PATH=}}\";; esac; }}; \
         r /bin/sh sh; s=${{SHELL:-}}; \
         [ -n \"$s\" ] || s=$(getent passwd \"$(id -un)\" 2>/dev/null | cut -d: -f7); \
         [ -n \"$s\" ] && [ -x \"$s\" ] && r \"$s\" shell; exit 0"
    )
}

/// Parse [`probe_script`]'s output. `None` when there is no `@base` line —
/// the script never ran, so nothing it would have said is known.
pub fn parse_probe(stdout: &str) -> Option<HostEnv> {
    let mut env = HostEnv::default();
    let mut saw_base = false;
    for line in stdout.lines() {
        let line = line.trim_end_matches('\r');
        let Some((tag, value)) = line.split_once(' ') else {
            continue;
        };
        match tag {
            "@home" => env.home = Some(value.to_string()).filter(|h| h.starts_with('/')),
            "@base" => {
                env.base = split_path(value);
                saw_base = true;
            }
            "@shell" => env.shell_login = Some(split_path(value)),
            "@sh" => env.sh_login = Some(split_path(value)),
            _ => {}
        }
    }
    saw_base.then_some(env)
}

/// A `PATH` value's components, keeping only absolute ones. An empty or
/// relative component resolves against the pane's working directory — the
/// session's own worktree — for the reason `paths::resolve_on_path` skips them.
fn split_path(value: &str) -> Vec<String> {
    value
        .split(':')
        .filter(|c| c.starts_with('/'))
        .map(str::to_string)
        .collect()
}

/// One component of the `PATH` being written.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Component {
    Literal(String),
    /// `~` or `~/<suffix>` from `path_prepend` on a host whose `$HOME` is not
    /// known here: spelled `"$HOME"<suffix>` for the host's shell to expand.
    Home(String),
}

/// What to assign `PATH` to on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathPlan {
    /// The whole `PATH`, from a probe that read the launcher's own.
    Full(Vec<Component>),
    /// Components to put in front of whatever `PATH` the host command already
    /// has — the probe did not answer, but `path_prepend` still applies.
    Prepend(Vec<Component>),
}

/// Merge `prepend` (hosts.toml `path_prepend`) with what the probe read.
///
/// Order: `prepend`, the user's login shell, `/bin/sh -l`, the launcher's
/// `PATH`; each directory kept at its first position. `None` means "change
/// nothing": no probe answer and nothing to prepend.
pub fn plan(prepend: &[String], env: Option<&HostEnv>) -> Option<PathPlan> {
    let home = env.and_then(|e| e.home.as_deref());
    let mut components: Vec<Component> = prepend
        .iter()
        .filter_map(|entry| expand(entry, home))
        .collect();
    let Some(env) = env else {
        dedup(&mut components);
        return (!components.is_empty()).then_some(PathPlan::Prepend(components));
    };
    let login = env.shell_login.iter().chain(env.sh_login.iter()).flatten();
    components.extend(
        login
            .chain(env.base.iter())
            .map(|c| Component::Literal(c.clone())),
    );
    dedup(&mut components);
    Some(PathPlan::Full(components))
}

/// One `path_prepend` entry as a component: `~` expanded against `home` when
/// it is known, deferred to the host's `$HOME` when not. Anything neither
/// absolute nor `~`-rooted is dropped (see [`split_path`]).
fn expand(entry: &str, home: Option<&str>) -> Option<Component> {
    let entry = entry.trim();
    let suffix = match entry.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => rest,
        _ => {
            return entry
                .starts_with('/')
                .then(|| Component::Literal(entry.to_string()))
        }
    };
    Some(match home {
        Some(home) => Component::Literal(format!("{}{suffix}", home.trim_end_matches('/'))),
        None => Component::Home(suffix.to_string()),
    })
}

fn dedup(components: &mut Vec<Component>) {
    let mut seen = std::collections::HashSet::new();
    components.retain(|c| seen.insert(c.clone()));
}

/// `plan` as `sh` text — `PATH=…; export PATH; ` — for the front of a script
/// or a window command. `:` is not special to `sh`, so each component is
/// quoted on its own and the pieces concatenate into one word.
pub fn shell_assignment(plan: &PathPlan) -> String {
    let (components, keep_inherited) = match plan {
        PathPlan::Full(c) => (c, false),
        PathPlan::Prepend(c) => (c, true),
    };
    let mut value = components
        .iter()
        .map(|c| match c {
            Component::Literal(dir) => posix_quote(dir),
            Component::Home(suffix) if suffix.is_empty() => "\"$HOME\"".to_string(),
            Component::Home(suffix) => format!("\"$HOME\"{}", posix_quote(suffix)),
        })
        .collect::<Vec<_>>()
        .join(":");
    if keep_inherited {
        // `${PATH:+…}`: an unset inherited `PATH` must not leave a trailing
        // `:`, which POSIX reads as the current directory.
        value.push_str("\"${PATH:+:$PATH}\"");
    }
    format!("PATH={value}; export PATH; ")
}

/// The assignment for a command on `host`, or `None` to leave its `PATH`
/// alone: a native-Windows host (no `sh`), or nothing learned and nothing
/// configured.
///
/// The probe runs on the first call per host and is cached for the life of
/// the process, a failure included — a host whose shell hangs costs one
/// probe timeout, not one per mirror tick.
pub fn assignment_for(host: &HostDef) -> Option<String> {
    if host.is_windows() {
        return None;
    }
    let env = cached_env(host);
    plan(&host.path_prepend, env.as_ref()).map(|p| shell_assignment(&p))
}

fn cache() -> &'static Mutex<HashMap<String, Option<HostEnv>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<HostEnv>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_env(host: &HostDef) -> Option<HostEnv> {
    let key = host.backend_name();
    if let Some(hit) = cache().lock().ok().and_then(|c| c.get(&key).cloned()) {
        return hit;
    }
    // Tests never reach a real host: a host a test did not seed is one whose
    // probe did not answer.
    if cfg!(test) {
        return None;
    }
    // Probed without the lock held, so one slow host does not stall another;
    // two racing first calls for the same host only probe it twice.
    let env = probe(host);
    if let Ok(mut c) = cache().lock() {
        c.insert(key, env.clone());
    }
    env
}

/// Record what a probe of `host` found, for tests that must not reach one.
#[cfg(test)]
pub fn seed(host: &HostDef, env: Option<HostEnv>) {
    cache().lock().unwrap().insert(host.backend_name(), env);
}

fn probe(host: &HostDef) -> Option<HostEnv> {
    let launcher = if host.is_wsl() {
        HostLauncher::Wsl {
            distro: host.distro_name(),
        }
    } else {
        HostLauncher::Ssh {
            destination: &host.destination,
            ssh_opts: &host.ssh_opts,
        }
    };
    let env = run_bounded(launcher.shell_c(&probe_script()), PROBE_TIMEOUT)
        .and_then(|out| parse_probe(&out));
    match &env {
        None => warn!(
            "host '{}': could not read the host's PATH; agents there keep the default PATH \
             (set `path_prepend` in hosts.toml to add directories)",
            host.name
        ),
        Some(e) if e.shell_login.is_none() && e.sh_login.is_none() => warn!(
            "host '{}': the login shell did not report a PATH; agents there keep the default \
             PATH (set `path_prepend` in hosts.toml to add directories)",
            host.name
        ),
        Some(e) => debug!("host '{}': login PATH read: {e:?}", host.name),
    }
    env
}

/// Run `cmd` with stdin closed, giving up after `timeout`. `None` when it
/// could not start, timed out, or printed nothing readable.
///
/// The deadline covers the **output** as well as the process: a login shell
/// can leave a background job holding the pipe after the launcher exits, and
/// waiting for EOF there would outlive any timeout. Only the first
/// [`MAX_PROBE_OUTPUT`] bytes are kept — a `PATH` or three — and the rest is
/// drained unread, so a chatty rc file costs neither memory nor a blocked
/// writer.
fn run_bounded(mut cmd: std::process::Command, timeout: Duration) -> Option<String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    // Detached: if the deadline passes it is left blocked on a pipe someone
    // else still holds, and ends when they close it.
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut capped = stdout.take(MAX_PROBE_OUTPUT);
        let _ = capped.read_to_end(&mut out);
        let _ = tx.send(out);
        let _ = std::io::copy(&mut capped.into_inner(), &mut std::io::sink());
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(_) => return None,
        }
    }
    let out = rx
        .recv_timeout(timeout.saturating_sub(started.elapsed()))
        .ok()?;
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn env(base: &[&str], shell: Option<&[&str]>, sh: Option<&[&str]>) -> HostEnv {
        HostEnv {
            home: Some("/home/me".into()),
            base: s(base),
            shell_login: shell.map(s),
            sh_login: sh.map(s),
        }
    }

    fn full(plan: Option<PathPlan>) -> Vec<String> {
        match plan {
            Some(PathPlan::Full(c)) => c
                .into_iter()
                .map(|c| match c {
                    Component::Literal(d) => d,
                    Component::Home(s) => format!("$HOME{s}"),
                })
                .collect(),
            other => panic!("expected a full PATH, got {other:?}"),
        }
    }

    #[test]
    fn the_user_path_goes_before_the_launchers_and_duplicates_collapse() {
        let e = env(
            &["/usr/bin", "/bin", "/mnt/c/git/bin", "/mnt/c/git/bin"],
            Some(&["/home/me/.cargo/bin", "/home/me/.local/bin", "/usr/bin"]),
            Some(&["/home/me/.local/bin", "/usr/local/bin", "/usr/bin"]),
        );
        assert_eq!(
            full(plan(&[], Some(&e))),
            [
                "/home/me/.cargo/bin",
                "/home/me/.local/bin",
                "/usr/bin",
                "/usr/local/bin",
                "/bin",
                "/mnt/c/git/bin",
            ]
        );
    }

    #[test]
    fn path_prepend_leads_and_expands_tilde_against_the_hosts_home() {
        let e = env(&["/usr/bin"], Some(&["/home/me/.local/bin"]), None);
        assert_eq!(
            full(plan(&s(&["~/.local/bin", "/opt/x/bin", "~"]), Some(&e))),
            ["/home/me/.local/bin", "/opt/x/bin", "/home/me", "/usr/bin"]
        );
    }

    #[test]
    fn relative_and_empty_components_never_reach_the_path() {
        let e = parse_probe("@home /home/me\n@base /usr/bin::.\n@shell bin:/a:\n").unwrap();
        assert_eq!(
            full(plan(&s(&["rel/bin", "", "~user/bin"]), Some(&e))),
            ["/a", "/usr/bin"]
        );
    }

    #[test]
    fn a_failed_login_shell_keeps_the_launchers_path() {
        let e = env(&["/usr/bin", "/bin"], None, None);
        assert_eq!(full(plan(&[], Some(&e))), ["/usr/bin", "/bin"]);
    }

    #[test]
    fn an_unanswered_probe_changes_nothing_without_an_override() {
        assert_eq!(plan(&[], None), None);
    }

    #[test]
    fn an_unanswered_probe_still_prepends_the_override_before_the_inherited_path() {
        let p = plan(&s(&["~/.local/bin", "/opt/bin", "/opt/bin"]), None).unwrap();
        assert_eq!(
            shell_assignment(&p),
            "PATH=\"$HOME\"/.local/bin:/opt/bin\"${PATH:+:$PATH}\"; export PATH; "
        );
    }

    #[test]
    fn the_assignment_quotes_each_component() {
        let e = env(&["/mnt/c/Program Files/Git/bin", "/usr/bin"], None, None);
        let p = plan(&[], Some(&e)).unwrap();
        assert_eq!(
            shell_assignment(&p),
            "PATH='/mnt/c/Program Files/Git/bin':/usr/bin; export PATH; "
        );
    }

    #[test]
    fn the_probe_output_parses_and_ignores_noise() {
        let out = "motd line\n@home /home/me\r\n@base /usr/bin:/bin\n\
                   @sh /home/me/.local/bin:/usr/bin\n@shell /home/me/.bun/bin:/usr/bin\n";
        assert_eq!(
            parse_probe(out),
            Some(HostEnv {
                home: Some("/home/me".into()),
                base: s(&["/usr/bin", "/bin"]),
                shell_login: Some(s(&["/home/me/.bun/bin", "/usr/bin"])),
                sh_login: Some(s(&["/home/me/.local/bin", "/usr/bin"])),
            })
        );
    }

    #[test]
    fn a_probe_that_never_ran_is_no_answer() {
        assert_eq!(parse_probe(""), None);
        assert_eq!(parse_probe("sh: 1: not found\n"), None);
    }

    #[test]
    fn a_windows_host_is_never_touched() {
        let host = HostDef {
            name: "win".into(),
            destination: "me@win".into(),
            multiplexer: Some("psmux".into()),
            path_prepend: s(&["/opt/bin"]),
            ..HostDef::default()
        };
        assert_eq!(assignment_for(&host), None);
    }

    #[test]
    fn a_seeded_host_gets_its_merged_path() {
        let host = HostDef {
            name: "host-path-seeded".into(),
            destination: "me@box".into(),
            ..HostDef::default()
        };
        seed(
            &host,
            Some(env(&["/usr/bin"], Some(&["/home/me/.local/bin"]), None)),
        );
        assert_eq!(
            assignment_for(&host).as_deref(),
            Some("PATH=/home/me/.local/bin:/usr/bin; export PATH; ")
        );
    }

    /// A background job keeping the pipe open after the launcher exits must
    /// not hold the probe past its deadline.
    #[cfg(unix)]
    #[test]
    fn a_pipe_held_open_after_exit_does_not_outlive_the_timeout() {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg("sleep 30 & echo early");
        let started = Instant::now();
        assert_eq!(run_bounded(cmd, Duration::from_millis(500)), None);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn probe_output_is_capped() {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg("yes 0123456789 | head -c 1000000");
        let out = run_bounded(cmd, Duration::from_secs(10)).expect("answered");
        assert_eq!(out.len() as u64, MAX_PROBE_OUTPUT);
    }

    #[cfg(unix)]
    #[test]
    fn a_process_that_outruns_the_timeout_is_killed() {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg("exec sleep 30");
        let started = Instant::now();
        assert_eq!(run_bounded(cmd, Duration::from_millis(300)), None);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// The script itself, against a real `sh` with a login shell whose rc file
    /// prints noise and whose profile adds a directory.
    #[cfg(unix)]
    #[test]
    fn the_probe_script_reads_a_login_shells_path_through_rc_noise() {
        let home = tempfile::TempDir::new().unwrap();
        let shell = home.path().join("fake-shell");
        // A "login shell" that echoes before printing, as a chatty rc would.
        std::fs::write(
            &shell,
            "#!/bin/sh\necho 'welcome!'\nPATH=/opt/user/bin:$PATH\nexport PATH\nshift\nexec /bin/sh -c \"$1\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(probe_script())
            .env("HOME", home.path())
            .env("SHELL", &shell)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        let env = parse_probe(&String::from_utf8_lossy(&out.stdout)).expect("probe answered");
        assert_eq!(env.base, s(&["/usr/bin", "/bin"]));
        let login = env.shell_login.expect("login shell read");
        assert_eq!(login.first().map(String::as_str), Some("/opt/user/bin"));
        assert!(env.sh_login.is_some());
    }
}
