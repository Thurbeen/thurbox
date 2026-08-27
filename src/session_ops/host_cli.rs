//! Running `thurbox-cli` on a shareable host — and putting one there when the
//! host has none.
//!
//! A shareable host's own database is the record of the sessions on it
//! (`docs/ARCHITECTURE.md` ADR-24), so every write a remote thurbox wants to
//! make there is a `thurbox-cli` command run *on the host*, and every read is
//! `session list --json` read back. This module is the one place that knows
//! how to find that CLI, decide whether it speaks this binary's JSON, install
//! a matching one when it does not, and run it in whichever shell the host
//! has — `sh` over ssh / `wsl.exe`, or PowerShell on a Windows host.
//!
//! Nothing here runs on the render path: the callers are the four
//! `session_ops` pipelines (on a worker or in the CLI) and the mirror worker.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::session::HostDef;

/// Where a provisioned CLI lands, under the host's thurbox data directory —
/// deliberately *not* on PATH: it is thurbox's, and an install the user makes
/// later (`install.sh`) wins as soon as its major matches.
pub const HOST_BIN_DIR: &str = "bin";

/// How long a host that answered "no usable CLI" is left alone before it is
/// asked again. Bounds what an unreachable host costs the mirror worker: one
/// ssh connect attempt (its own `ConnectTimeout`) per this interval, not per
/// pass.
pub const PROBE_RETRY: Duration = Duration::from_secs(60);

/// What a host's `thurbox-cli version --json` said about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliInfo {
    /// How to invoke it on the host: a bare name found on PATH, or the absolute
    /// path of a provisioned copy.
    pub path: String,
    pub version: String,
    /// The tmux socket its sessions live on — what a peer must attach to.
    pub tmux_socket: Option<String>,
    pub data_dir: Option<String>,
    /// Its database schema. `None` for a CLI too old to report one, which is
    /// also too old to share with.
    pub schema_version: Option<u32>,
}

/// Whether a host can be shared with, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Usable {
    Yes(CliInfo),
    /// Why not — the text a session's info shows as `Sharing: off (…)`.
    No(String),
}

/// The probe verdict for each host, so a spawn, a delete and the mirror do not
/// each pay an ssh round trip to learn the same thing. Keyed by backend name.
fn verdicts() -> &'static Mutex<HashMap<String, (Usable, Instant)>> {
    static VERDICTS: OnceLock<Mutex<HashMap<String, (Usable, Instant)>>> = OnceLock::new();
    VERDICTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether `host` has — or can be given — a `thurbox-cli` this binary can
/// delegate to. Cached per host: a `Yes` for the process lifetime (the host's
/// CLI does not change under us), a `No` for [`PROBE_RETRY`].
///
/// A host with `share_sessions = false` is never contacted: it is used
/// exactly as before sharing existed.
pub fn usable(host: &HostDef) -> Usable {
    if !host.shareable() {
        return Usable::No("sharing is off for this host (share_sessions = false)".to_string());
    }
    #[cfg(test)]
    if let Some(forced) = fake::usable_override() {
        return forced;
    }
    let key = host.backend_name();
    if let Ok(cache) = verdicts().lock() {
        if let Some((verdict, at)) = cache.get(&key) {
            let fresh = match verdict {
                Usable::Yes(_) => true,
                Usable::No(_) => at.elapsed() < PROBE_RETRY,
            };
            if fresh {
                return verdict.clone();
            }
        }
    }
    let verdict = establish(host);
    if let Usable::Yes(cli) = &verdict {
        if let Some(socket) = &cli.tmux_socket {
            crate::agent::tmux::learn_host_socket(host, socket);
        }
    }
    if let Ok(mut cache) = verdicts().lock() {
        cache.insert(key, (verdict.clone(), Instant::now()));
    }
    verdict
}

/// The usable CLI for `host`, or `None` (with the reason logged once) when the
/// pipelines must fall back to driving the host from here.
pub fn delegated(host: &HostDef) -> Option<CliInfo> {
    match usable(host) {
        Usable::Yes(cli) => Some(cli),
        Usable::No(reason) => {
            tracing::info!("not delegating to host '{}': {reason}", host.name);
            None
        }
    }
}

/// Drop the cached verdict for `host`, so the next question asks the host
/// again — what `session sync` does, since a user running it by hand has
/// usually just fixed something.
pub fn forget(host: &HostDef) {
    if let Ok(mut cache) = verdicts().lock() {
        cache.remove(&host.backend_name());
    }
}

/// The reason text a session created without delegation carries.
pub fn sharing_off_note(host: &HostDef, reason: &str) -> String {
    format!("sharing off for host '{}': {reason}", host.name)
}

fn establish(host: &HostDef) -> Usable {
    let found = match probe(host) {
        Ok(found) => found,
        Err(e) => return Usable::No(format!("host not answering: {e}")),
    };
    if let Some(cli) = &found {
        if let Err(mismatch) = compatible(cli) {
            tracing::info!(
                "host '{}' has thurbox-cli {} at {}, but {mismatch}; provisioning a matching one",
                host.name,
                cli.version,
                cli.path
            );
        } else {
            return Usable::Yes(cli.clone());
        }
    }
    let path = match provision(host) {
        Ok(path) => path,
        Err(e) => return Usable::No(e),
    };
    match probe_at(host, &path) {
        Ok(Some(cli)) => match compatible(&cli) {
            Ok(()) => Usable::Yes(cli),
            Err(mismatch) => Usable::No(format!("provisioned thurbox-cli at {path} {mismatch}")),
        },
        Ok(None) => Usable::No(format!("provisioned thurbox-cli at {path} does not run")),
        Err(e) => Usable::No(format!(
            "provisioned thurbox-cli at {path} failed to answer: {e}"
        )),
    }
}

/// Whether a host CLI speaks this binary's JSON and database: same major
/// version, same schema. `Err` names the mismatch.
pub fn compatible(cli: &CliInfo) -> Result<(), String> {
    let ours = crate::agent::version_check::current_version();
    let (ours_major, theirs_major) = (major_of(ours), major_of(&cli.version));
    if ours_major != theirs_major {
        return Err(format!(
            "is major {theirs_major} where this thurbox is major {ours_major}"
        ));
    }
    match cli.schema_version {
        Some(schema) if schema == crate::storage::SCHEMA_VERSION => Ok(()),
        Some(schema) => Err(format!(
            "uses database schema v{schema} where this thurbox uses v{}",
            crate::storage::SCHEMA_VERSION
        )),
        None => Err("predates session sharing (reports no schema version)".to_string()),
    }
}

fn major_of(version: &str) -> u64 {
    version
        .trim_start_matches('v')
        .split(['.', '-'])
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// The shell script that looks for a `thurbox-cli` on the host and, finding
/// one, prints `@cli <path>` followed by its `version --json`; `@none` when
/// there is none. The provisioned copy of **this flavour** is looked at first
/// — a dev build's lives under `thurbox-dev`, a release's under `thurbox` —
/// so a dev laptop finds its own copy again on the next start rather than
/// the release CLI on PATH (a different major) and a fresh provisioning.
/// Then PATH, then the installer's default, which a non-interactive ssh
/// shell rarely has on PATH.
pub(crate) fn probe_script_posix() -> String {
    let flavour = crate::paths::app_dir_name();
    format!(
        "for c in \"$HOME/.local/share/{flavour}/{HOST_BIN_DIR}/thurbox-cli\" thurbox-cli \
         \"$HOME/.local/bin/thurbox-cli\" /usr/local/bin/thurbox-cli; do \
         p=$(command -v \"$c\" 2>/dev/null) && [ -n \"$p\" ] && \
         {{ echo \"@cli $p\"; \"$p\" version --json; exit 0; }}; done; echo @none"
    )
}

/// [`probe_script_posix`] for a Windows host: the same line protocol out of
/// PowerShell, looking at this flavour's provisioned directory, PATH, and
/// `install.ps1`'s default.
pub(crate) fn probe_script_windows() -> String {
    let flavour = crate::paths::app_dir_name();
    format!(
        "$c = @(\"$env:LOCALAPPDATA\\{flavour}\\{HOST_BIN_DIR}\\thurbox-cli.exe\", 'thurbox-cli', \
         \"$env:LOCALAPPDATA\\Programs\\thurbox\\thurbox-cli.exe\"); \
         foreach ($p in $c) {{ $g = Get-Command $p -ErrorAction SilentlyContinue; \
         if ($g) {{ Write-Output \"@cli $($g.Source)\"; & $g.Source version --json; exit 0 }} }}; \
         Write-Output '@none'"
    )
}

/// Ask the host whether it has a `thurbox-cli`, and what version.
pub fn probe(host: &HostDef) -> Result<Option<CliInfo>, String> {
    let script = if host.is_windows() {
        probe_script_windows()
    } else {
        probe_script_posix()
    };
    let stdout = run_script(host, &script, "thurbox-cli probe")?;
    parse_probe(&stdout)
}

/// [`probe`] for a known path — what a freshly provisioned copy is checked
/// with, since PATH would not find it.
fn probe_at(host: &HostDef, path: &str) -> Result<Option<CliInfo>, String> {
    let script = if host.is_windows() {
        format!(
            "Write-Output \"@cli {path}\"; & {} version --json; exit $LASTEXITCODE",
            crate::shell::powershell_quote(path)
        )
    } else {
        format!(
            "echo \"@cli {path}\"; {} version --json",
            crate::shell::posix_quote(path)
        )
    };
    let stdout = run_script(host, &script, "thurbox-cli probe")?;
    parse_probe(&stdout)
}

/// Parse the probe's line protocol: `@cli <path>` then a JSON document, or
/// `@none`.
pub(crate) fn parse_probe(stdout: &str) -> Result<Option<CliInfo>, String> {
    let mut lines = stdout.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(first) = lines.next() else {
        return Err("probe printed nothing".to_string());
    };
    if first == "@none" {
        return Ok(None);
    }
    let Some(path) = first.strip_prefix("@cli ") else {
        return Err(format!("unexpected probe output: {first}"));
    };
    let body: String = lines.collect::<Vec<_>>().join("\n");
    let json: Value = serde_json::from_str(&body)
        .map_err(|e| format!("thurbox-cli at {path} printed no JSON version ({e})"))?;
    let version = json
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("thurbox-cli at {path} reported no version"))?
        .to_string();
    Ok(Some(CliInfo {
        path: path.trim().to_string(),
        version,
        tmux_socket: json
            .get("tmux_socket")
            .and_then(Value::as_str)
            .map(str::to_string),
        data_dir: json
            .get("data_dir")
            .and_then(Value::as_str)
            .map(str::to_string),
        schema_version: json
            .get("schema_version")
            .and_then(Value::as_u64)
            .map(|v| v as u32),
    }))
}

/// Run `thurbox-cli <args> --json` on `host` and return the parsed answer.
///
/// A non-zero exit is the host CLI's own error (`error: …` on stderr), passed
/// on verbatim: it names what went wrong *there*, which is what the caller
/// needs to show.
pub fn run(host: &HostDef, cli: &CliInfo, args: &[&str]) -> Result<Value, String> {
    #[cfg(test)]
    if let Some(answer) = fake::run_override(host, args) {
        return answer;
    }
    let script = if host.is_windows() {
        cli_script_windows(&cli.path, args)
    } else {
        cli_script_posix(&cli.path, args)
    };
    let stdout = run_script(host, &script, "thurbox-cli")?;
    serde_json::from_str(&stdout).map_err(|e| {
        format!(
            "thurbox-cli on '{}' printed no JSON for `{}` ({e}): {}",
            host.name,
            args.join(" "),
            stdout.trim()
        )
    })
}

/// The `sh` line for one CLI invocation. Every argument is POSIX-quoted, and
/// `--json` is forced so the answer is parseable whether or not stdout is a
/// pipe on the host.
pub(crate) fn cli_script_posix(cli: &str, args: &[&str]) -> String {
    let mut words = vec![crate::shell::posix_quote(cli)];
    words.extend(args.iter().map(|a| crate::shell::posix_quote(a)));
    words.push("--json".to_string());
    words.join(" ")
}

/// The PowerShell line for one CLI invocation: `& 'cli' 'arg' … --json`, then
/// the CLI's exit code handed back — PowerShell's own would be 0 regardless.
pub(crate) fn cli_script_windows(cli: &str, args: &[&str]) -> String {
    let mut words = vec![format!("& {}", crate::shell::powershell_quote(cli))];
    words.extend(args.iter().map(|a| crate::shell::powershell_quote(a)));
    words.push("--json".to_string());
    format!("{}; exit $LASTEXITCODE", words.join(" "))
}

/// Run a script on the host in its own dialect and return stdout, with a
/// failure carrying the host's cleaned stderr.
fn run_script(host: &HostDef, script: &str, action: &str) -> Result<String, String> {
    let mut command = if host.is_windows() {
        crate::git::host_powershell_c(host, script)
    } else {
        crate::git::host_shell_c(host, script)
    };
    let output = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("could not start {action} on '{}': {e}", host.name))?;
    if !output.status.success() {
        let stderr = crate::git::reportable_stderr(&output.stderr);
        let stderr = stderr
            .trim()
            .strip_prefix("error: ")
            .unwrap_or(stderr.trim());
        return Err(if stderr.is_empty() {
            format!(
                "{action} on '{}' failed (exit {})",
                host.name,
                output
                    .status
                    .code()
                    .map_or("?".to_string(), |c| c.to_string())
            )
        } else {
            stderr.to_string()
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `(os, arch)` as the host's shell spells them (`uname -sm`, or
/// `windows <PROCESSOR_ARCHITECTURE>`), for [`crate::agent::self_update::target_triple`].
fn host_platform(host: &HostDef) -> Result<(String, String), String> {
    let script = if host.is_windows() {
        "Write-Output \"windows $env:PROCESSOR_ARCHITECTURE\"".to_string()
    } else {
        "uname -sm".to_string()
    };
    let out = run_script(host, &script, "platform probe")?;
    let mut words = out.split_whitespace();
    match (words.next(), words.next()) {
        (Some(os), Some(arch)) => Ok((os.to_string(), arch.to_string())),
        _ => Err(format!("could not read the host platform from {out:?}")),
    }
}

/// The host's thurbox data directory for **this flavour** — `thurbox` for a
/// release build, which is where a full install on the host looks, so the
/// database a provisioned CLI creates is the one a later `install.sh` finds;
/// `thurbox-dev` for a dev build, so it never touches the host's release copy.
pub fn host_data_dir(host: &HostDef) -> Result<String, String> {
    let home = crate::git::remote_home(host).map_err(|e| format!("{e:#}"))?;
    let flavour = crate::paths::app_dir_name();
    Ok(if host.is_windows() {
        format!("{home}/AppData/Local/{flavour}")
    } else {
        format!("{home}/.local/share/{flavour}")
    })
}

/// Put a `thurbox-cli` of this binary's version on the host, under
/// `<data dir>/bin/`, and return its path.
///
/// A release build fetches the release archive for the host's platform,
/// verified against the release checksums, and extracts it on the host. A dev
/// build has no release: it ships its own sibling `thurbox-cli` when the host
/// is the same platform, and refuses otherwise — the refusal is what
/// `Sharing: off` shows, and the legacy path takes over.
pub fn provision(host: &HostDef) -> Result<String, String> {
    let (os, arch) = host_platform(host)?;
    let target = crate::agent::self_update::target_triple(&os, &arch)?;
    let bin_dir = format!("{}/{HOST_BIN_DIR}", host_data_dir(host)?);
    let cli_name = if host.is_windows() {
        "thurbox-cli.exe"
    } else {
        "thurbox-cli"
    };
    let dest = format!("{bin_dir}/{cli_name}");

    if crate::agent::extension_config::is_dev_build() {
        let ours = crate::agent::self_update::current_target()?;
        if ours != target {
            return Err(format!(
                "development build: no release archive to provision a {target} host with \
                 (this machine is {ours}); install thurbox on the host"
            ));
        }
        let local = crate::agent::tmux::resolve_cli_binary();
        let bytes = std::fs::read(&local)
            .map_err(|e| format!("read {} to ship it: {e}", local.display()))?;
        ship(host, &bytes, &dest)?;
        if !host.is_windows() {
            run_script(
                host,
                &format!("chmod +x {}", crate::shell::posix_quote(&dest)),
                "chmod",
            )?;
        }
        tracing::info!(
            "shipped the development thurbox-cli to '{}' at {dest}",
            host.name
        );
        return Ok(dest);
    }

    let version = crate::agent::version_check::current_version();
    let archive = crate::agent::self_update::fetch_archive(version, target)?;
    let bytes = std::fs::read(&archive.path)
        .map_err(|e| format!("read {}: {e}", archive.path.display()))?;
    let remote_archive = format!("{bin_dir}/{}", archive.name);
    ship(host, &bytes, &remote_archive)?;
    let extract = if host.is_windows() {
        format!(
            "Expand-Archive -Force -LiteralPath {a} -DestinationPath {d}; Remove-Item -Force {a}",
            a = crate::shell::powershell_quote(&remote_archive),
            d = crate::shell::powershell_quote(&bin_dir)
        )
    } else {
        format!(
            "cd {d} && tar -xzf {a} && rm -f {a} && chmod +x thurbox-cli",
            d = crate::shell::posix_quote(&bin_dir),
            a = crate::shell::posix_quote(&archive.name)
        )
    };
    run_script(host, &extract, "thurbox-cli extraction")?;
    tracing::info!(
        "provisioned thurbox-cli {version} on '{}' at {dest}",
        host.name
    );
    Ok(dest)
}

fn ship(host: &HostDef, bytes: &[u8], dest: &str) -> Result<(), String> {
    let shipped = if host.is_windows() {
        crate::git::copy_stream_to_remote_windows(host, bytes, dest)
    } else {
        crate::git::copy_bytes_to_remote(host, bytes, dest)
    };
    shipped.map_err(|e| format!("could not copy thurbox-cli to '{}': {e:#}", host.name))
}

/// Test doubles: a forced verdict and a scripted runner, so the pipelines can
/// be exercised without a host. Thread-local because each pipeline runs on the
/// thread that called it.
#[cfg(test)]
pub(crate) mod fake {
    use std::cell::RefCell;

    use serde_json::Value;

    use super::Usable;
    use crate::session::HostDef;

    type Runner = Box<dyn Fn(&HostDef, &[String]) -> Result<Value, String>>;

    thread_local! {
        static USABLE: RefCell<Option<Usable>> = const { RefCell::new(None) };
        static RUNNER: RefCell<Option<Runner>> = const { RefCell::new(None) };
        static CALLS: RefCell<Vec<Vec<String>>> = const { RefCell::new(Vec::new()) };
    }

    pub fn force_usable(verdict: Usable) {
        USABLE.with(|u| *u.borrow_mut() = Some(verdict));
    }

    pub fn install_runner(runner: Runner) {
        RUNNER.with(|r| *r.borrow_mut() = Some(runner));
        CALLS.with(|c| c.borrow_mut().clear());
    }

    pub fn clear() {
        USABLE.with(|u| *u.borrow_mut() = None);
        RUNNER.with(|r| *r.borrow_mut() = None);
        CALLS.with(|c| c.borrow_mut().clear());
    }

    /// Every argument list the scripted runner was asked to run, in order.
    pub fn calls() -> Vec<Vec<String>> {
        CALLS.with(|c| c.borrow().clone())
    }

    pub(super) fn usable_override() -> Option<Usable> {
        USABLE.with(|u| u.borrow().clone())
    }

    pub(super) fn run_override(host: &HostDef, args: &[&str]) -> Option<Result<Value, String>> {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        RUNNER.with(|r| {
            let runner = r.borrow();
            let runner = runner.as_ref()?;
            CALLS.with(|c| c.borrow_mut().push(args.clone()));
            Some(runner(host, &args))
        })
    }

    /// A usable CLI as a test would see it.
    pub fn cli() -> super::CliInfo {
        super::CliInfo {
            path: "/home/me/.local/share/thurbox/bin/thurbox-cli".into(),
            version: crate::agent::version_check::current_version().into(),
            tmux_socket: Some("thurbox".into()),
            data_dir: Some("/home/me/.local/share/thurbox".into()),
            schema_version: Some(crate::storage::SCHEMA_VERSION),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(version: &str, schema: Option<u32>) -> CliInfo {
        CliInfo {
            path: "thurbox-cli".into(),
            version: version.into(),
            tmux_socket: None,
            data_dir: None,
            schema_version: schema,
        }
    }

    #[test]
    fn a_posix_invocation_quotes_every_argument_and_forces_json() {
        let script = cli_script_posix(
            "/home/me/.local/share/thurbox/bin/thurbox-cli",
            &[
                "session",
                "create",
                "--name",
                "my session",
                "--repo-path",
                "/srv/it's",
            ],
        );
        assert_eq!(
            script,
            "/home/me/.local/share/thurbox/bin/thurbox-cli session create --name 'my session' \
             --repo-path '/srv/it'\\''s' --json"
        );
    }

    #[test]
    fn a_windows_invocation_is_single_quoted_and_hands_back_the_exit_code() {
        let script = cli_script_windows(
            "C:/Users/me/AppData/Local/thurbox/bin/thurbox-cli.exe",
            &["session", "list", "--parent", "$x'y"],
        );
        assert_eq!(
            script,
            "& 'C:/Users/me/AppData/Local/thurbox/bin/thurbox-cli.exe' 'session' 'list' \
             '--parent' '$x''y' --json; exit $LASTEXITCODE"
        );
    }

    #[test]
    fn the_probe_protocol_round_trips() {
        let found = parse_probe(
            "@cli /usr/local/bin/thurbox-cli\n{\"version\":\"1.4.0\",\"tmux_socket\":\"thurbox\",\
             \"data_dir\":\"/home/me/.local/share/thurbox\",\"schema_version\":40}\n",
        )
        .unwrap()
        .unwrap();
        assert_eq!(found.path, "/usr/local/bin/thurbox-cli");
        assert_eq!(found.version, "1.4.0");
        assert_eq!(found.tmux_socket.as_deref(), Some("thurbox"));
        assert_eq!(found.schema_version, Some(40));
        assert_eq!(parse_probe("@none\n").unwrap(), None);
        assert!(parse_probe("").is_err());
        assert!(parse_probe("bash: no such thing\n").is_err());
        // An old CLI prints only its version.
        let old = parse_probe("@cli thurbox-cli\n{\"version\":\"1.1.0\"}")
            .unwrap()
            .unwrap();
        assert_eq!(old.schema_version, None);
    }

    #[test]
    fn compatibility_needs_the_same_major_and_the_same_schema() {
        let ours = crate::agent::version_check::current_version();
        let schema = crate::storage::SCHEMA_VERSION;
        assert!(compatible(&cli(ours, Some(schema))).is_ok());
        let other_major = format!("{}.0.0", major_of(ours) + 1);
        let err = compatible(&cli(&other_major, Some(schema))).unwrap_err();
        assert!(err.contains("major"), "{err}");
        let err = compatible(&cli(ours, Some(schema + 1))).unwrap_err();
        assert!(err.contains("schema"), "{err}");
        let err = compatible(&cli(ours, None)).unwrap_err();
        assert!(err.contains("predates"), "{err}");
    }

    #[test]
    fn major_is_read_from_release_and_dev_spellings() {
        assert_eq!(major_of("1.4.2"), 1);
        assert_eq!(major_of("v2.0.0"), 2);
        assert_eq!(major_of("0.0.0-dev"), 0);
        assert_eq!(major_of("garbage"), 0);
    }

    #[test]
    fn a_host_with_sharing_off_is_never_contacted() {
        let host = HostDef {
            name: "quiet".into(),
            destination: "me@quiet".into(),
            share_sessions: false,
            ..HostDef::default()
        };
        assert!(matches!(usable(&host), Usable::No(reason) if reason.contains("share_sessions")));
    }

    #[test]
    fn the_probe_scripts_look_in_this_flavours_provisioned_directory_first() {
        let flavour = crate::paths::app_dir_name();
        let posix = probe_script_posix();
        let own = format!(".local/share/{flavour}/bin/thurbox-cli");
        assert!(posix.contains(&own), "{posix}");
        assert!(
            posix.find(&own) < posix.find(" thurbox-cli "),
            "the flavour's own copy is tried before PATH"
        );
        assert!(posix.contains("version --json"));
        let windows = probe_script_windows();
        assert!(
            windows.contains(&format!("\\{flavour}\\bin\\thurbox-cli.exe")),
            "{windows}"
        );
        assert!(windows.contains("version --json"));
    }
}
