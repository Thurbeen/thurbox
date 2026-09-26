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
/// asked again — the **first** time. Keeps what an unreachable host costs the
/// mirror worker to one ssh connect attempt (its own `ConnectTimeout`) per
/// interval rather than per pass; a host that keeps failing is then spaced out
/// further still, up to [`PROBE_RETRY_MAX`].
pub const PROBE_RETRY: Duration = Duration::from_secs(60);

/// The ceiling `retry_after` climbs to after repeated failures.
///
/// A host that cannot be provisioned *at all* — no release artifact for its
/// platform, a remote shell that will not take a payload that size — fails
/// identically every time it is asked, and on the flat [`PROBE_RETRY`] that
/// cost a release-archive download, an ssh connect and a 10 MB stream once a
/// minute for as long as thurbox ran. Backing off to this bounds a permanent
/// failure at a few attempts an hour, while a transient one (a host rebooting,
/// a laptop off the network) is still picked up within the minute because its
/// first success resets the count.
pub const PROBE_RETRY_MAX: Duration = Duration::from_secs(15 * 60);

/// How long to leave a host alone after `failures` consecutive `No`s:
/// [`PROBE_RETRY`] doubled once per failure, capped at [`PROBE_RETRY_MAX`].
///
/// Shared with the teardown sweep's own host backoff
/// ([`super::delete`]), so a host that cannot be reached is spaced out on one
/// curve rather than on two that drift apart.
pub(super) fn retry_after(failures: u32) -> Duration {
    // Capped before the shift rather than after: 20 doublings of a minute is
    // already far past the ceiling, and it keeps the shift in range.
    let doublings = failures.saturating_sub(1).min(20);
    PROBE_RETRY
        .saturating_mul(1 << doublings)
        .min(PROBE_RETRY_MAX)
}

/// A cached probe verdict: what the host said, when it said it, and how many
/// times in a row it has now failed — which is what sets the next retry's
/// distance. A `Yes` carries `failures: 0` and never expires.
struct Verdict {
    usable: Usable,
    at: Instant,
    failures: u32,
}

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
fn verdicts() -> &'static Mutex<HashMap<String, Verdict>> {
    static VERDICTS: OnceLock<Mutex<HashMap<String, Verdict>>> = OnceLock::new();
    VERDICTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether `host` has — or can be given — a `thurbox-cli` this binary can
/// delegate to. Cached per host: a `Yes` for the process lifetime (the host's
/// CLI does not change under us), a `No` for `retry_after` its consecutive
/// failure count — [`PROBE_RETRY`] the first time, doubling towards
/// [`PROBE_RETRY_MAX`] for a host that cannot be made usable at all.
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
    // Carried across the re-probe below, so a host that keeps failing keeps
    // backing off instead of restarting at `PROBE_RETRY` on every attempt.
    let mut failures = 0;
    if let Ok(cache) = verdicts().lock() {
        if let Some(verdict) = cache.get(&key) {
            if is_fresh(verdict) {
                return verdict.usable.clone();
            }
            failures = verdict.failures;
        }
    }
    let verdict = establish(host);
    remember_socket(host, &verdict);
    let failures = match &verdict {
        Usable::Yes(_) => 0,
        Usable::No(reason) => {
            let failures = failures.saturating_add(1);
            tracing::debug!(
                "host '{}' is not usable ({reason}); asking again in {}s",
                host.name,
                retry_after(failures).as_secs()
            );
            failures
        }
    };
    if let Ok(mut cache) = verdicts().lock() {
        cache.insert(
            key,
            Verdict {
                usable: verdict.clone(),
                at: Instant::now(),
                failures,
            },
        );
    }
    verdict
}

/// Whether a cached verdict still stands: a `Yes` for the process lifetime, a
/// `No` until its backoff runs out.
fn is_fresh(verdict: &Verdict) -> bool {
    match &verdict.usable {
        Usable::Yes(_) => true,
        Usable::No(_) => verdict.at.elapsed() < retry_after(verdict.failures),
    }
}

/// Tell the tmux layer the socket a usable host's CLI reported.
fn remember_socket(host: &HostDef, verdict: &Usable) {
    if let Usable::Yes(cli) = verdict {
        if let Some(socket) = &cli.tmux_socket {
            crate::agent::tmux::learn_host_socket(host, socket);
        }
    }
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

/// Put **this** thurbox's CLI where a peer's probe looks — `<data dir>/bin/
/// thurbox-cli`, as a symlink to the running binary's `thurbox-cli` — so a
/// machine that runs thurbox at all is shareable without being provisioned.
///
/// The case that needs it is a development build: a checkout's
/// `target/debug/thurbox-cli` is on nobody's PATH, so a peer probing this
/// machine found only a release install (a different major) and had to
/// provision, which a dev peer can only do onto its own platform. A release
/// build gains nothing it did not have (its CLI is already on PATH) but the
/// link is kept true regardless, so a later dev checkout cannot leave a stale
/// one behind. Refreshed at TUI start and on every CLI invocation; a cheap
/// `readlink` compare when nothing changed. Unix only — Windows symlinks need
/// a privilege, and `install.ps1`'s directory is already a probe candidate.
///
/// It only ever manages a symlink of its own: a real file at that path is the
/// advertisement already (a provisioned host's own CLI lands exactly there),
/// and is left alone.
pub fn advertise_running_cli() {
    #[cfg(unix)]
    {
        let Some(dir) =
            crate::paths::database_file().and_then(|db| db.parent().map(|d| d.join(HOST_BIN_DIR)))
        else {
            return;
        };
        advertise_cli_in(&dir, &crate::agent::tmux::resolve_cli_binary());
    }
}

/// [`advertise_running_cli`] with the directory and the running CLI handed in
/// — all of its logic, and the seam its tests drive: nothing in-process can
/// choose what `resolve_cli_binary` answers, and every case worth pinning is a
/// relation between those two paths.
#[cfg(unix)]
fn advertise_cli_in(dir: &std::path::Path, target: &std::path::Path) {
    let link = dir.join("thurbox-cli");
    // Healed before anything is read, because no guard below can see past a
    // loop: `resolve_cli_binary` looks for a sibling that `exists()`, and a
    // self-link does not, so `target` is then the bare name and the
    // `is_absolute` guard returns with the loop still in place. No migration
    // reaches a WSL distro, so this read side is the only thing that ever
    // repairs a machine already carrying one (issue #1193).
    heal_self_link(&link);
    // The running CLI *is* the path being advertised. That is the ordinary
    // shape on a provisioned host: `resolve_cli_binary` answers with a sibling
    // of the running exe, and there the exe is `<data dir>/bin/thurbox`, so the
    // sibling is this very link. The advertisement is already true — a real
    // binary sits at it — and writing one anyway removed that binary and
    // pointed the path at itself. This has to return *before* the removal
    // below, not merely before the symlink.
    if same_path(target, &link) {
        return;
    }
    if !target.is_absolute() || !target.exists() {
        return;
    }
    if std::fs::read_link(&link).is_ok_and(|current| current == *target) {
        return;
    }
    if let Err(e) = link_cli(dir, &link, target) {
        tracing::debug!("could not advertise thurbox-cli at {}: {e}", link.display());
    }
}

/// Whether two paths name the same file, decided without following either.
///
/// Spelling equality is not enough, and the two sides here are drawn from
/// different places: `resolve_cli_binary` answers from `current_exe`, which the
/// kernel hands back fully resolved, while the advertised directory is built
/// from `THURBOX_DATA_DIR` or `$HOME` and may be relative or reached through a
/// symlinked home. One file spelled two ways reads as two files, and relinking
/// one to the other is exactly the loop (issue #1193).
///
/// The *parents* are resolved rather than the paths: a path being compared here
/// is either the loop, which `canonicalize` refuses outright, or a link this
/// function wrote, which it would resolve to the wrong side of the question.
/// The directories holding them are ordinary directories either way. A parent
/// that cannot be resolved — the advertising directory on a first run does not
/// exist yet — answers "not the same", which advertises rather than skips.
#[cfg(unix)]
fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    if a == b {
        return true;
    }
    if a.file_name() != b.file_name() {
        return false;
    }
    match (a.parent(), b.parent()) {
        (Some(a), Some(b)) => match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        },
        _ => false,
    }
}

/// Remove `link` when it is a symlink to its own path — `ELOOP` on every use,
/// and never correct however it got there.
///
/// Read back rather than resolved: a loop has no `metadata`, and `read_link`
/// is the one call that answers what it was pointed at. The target is joined
/// onto the parent so a relative spelling of the same mistake is caught too;
/// `Path::join` leaves an absolute one alone, which is the spelling actually
/// measured. [`same_path`] then settles it, so a loop written under one
/// spelling of the directory is removed when it is reached by another.
#[cfg(unix)]
fn heal_self_link(link: &std::path::Path) {
    let Some(dir) = link.parent() else { return };
    if !std::fs::read_link(link).is_ok_and(|to| same_path(&dir.join(to), link)) {
        return;
    }
    if let Err(e) = std::fs::remove_file(link) {
        tracing::debug!(
            "could not remove the self-referential thurbox-cli at {}: {e}",
            link.display()
        );
    }
}

/// Point `link` at `target`, replacing an advertisement of this function's own
/// making.
///
/// Only ever a symlink is replaced. A regular file there is somebody else's —
/// the CLI a provisioner just extracted, or an installer wrote — and removing
/// it is what destroyed a freshly provisioned host, so it is left and the
/// advertisement is skipped.
#[cfg(unix)]
fn link_cli(
    dir: &std::path::Path,
    link: &std::path::Path,
    target: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    match std::fs::symlink_metadata(link) {
        Ok(meta) if !meta.file_type().is_symlink() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "a file that is not ours is already there",
            ))
        }
        Ok(_) => std::fs::remove_file(link)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    std::os::unix::fs::symlink(target, link)
}

fn establish(host: &HostDef) -> Usable {
    let found = match probe(host) {
        Ok(found) => found,
        // The host answered, and what it answered with is broken. Returning
        // `No` here is what made that permanent: the verdict is cached behind
        // a doubling backoff, an unusable host's mirror pass is skipped, and
        // that mirror is the only caller that ever reaches `provision` — so
        // the corrupt binary disabled the one mechanism that would have
        // replaced it, and the backoff made it quieter rather than better.
        // Provisioning *is* the repair for this state, so fall through to it
        // exactly as a host with no CLI at all does.
        Err(e) if e.broken_cli => {
            tracing::info!(
                "host '{}' has a broken thurbox-cli ({e}); provisioning a replacement",
                host.name
            );
            None
        }
        // A host that did not answer is the other state, and the only one
        // waiting helps. It keeps its backoff untouched.
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
    // `provision` has already made the binary answer for itself, so what
    // comes back is what the host's own CLI said about itself.
    match provision(host) {
        Ok(cli) => match compatible(&cli) {
            Ok(()) => Usable::Yes(cli),
            Err(mismatch) => Usable::No(format!(
                "provisioned thurbox-cli at {} {mismatch}",
                cli.path
            )),
        },
        Err(e) => Usable::No(e),
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
/// one, prints `@cli <path>`, then `@status <n>` — what that binary exited
/// with — then its `version --json`; `@none` when there is none. The
/// provisioned copy of **this flavour** is looked at first — a dev build's
/// lives under `thurbox-dev`, a release's under `thurbox` — so a dev laptop
/// finds its own copy again on the next start rather than the release CLI on
/// PATH (a different major) and a fresh provisioning. Then PATH, then the
/// installer's default, which a non-interactive ssh shell rarely has on PATH.
///
/// The status line is the whole reason a half-written binary can be told from
/// a version mismatch. The script used to run the CLI as its own last
/// command, so a segfault reached this side as nothing at all and read as
/// "printed no JSON version" — which sends an operator to look at versions
/// and protocols for a file that was 54% of itself. The output is captured
/// and re-echoed rather than streamed so that the status can be read at all.
pub(crate) fn probe_script_posix() -> String {
    let flavour = crate::paths::app_dir_name();
    format!(
        "for c in \"$HOME/.local/share/{flavour}/{HOST_BIN_DIR}/thurbox-cli\" thurbox-cli \
         \"$HOME/.local/bin/thurbox-cli\" /usr/local/bin/thurbox-cli; do \
         p=$(command -v \"$c\" 2>/dev/null) && [ -n \"$p\" ] && \
         {{ echo \"@cli $p\"; {run}; exit 0; }}; done; echo @none",
        run = probe_run_posix("\"$p\"")
    )
}

/// The `sh` fragment that runs one candidate and reports both halves of what
/// it did: `@status <n>` then whatever it printed. `$?` is read off the
/// assignment, so it is the CLI's own status and not the `echo`'s.
fn probe_run_posix(cli: &str) -> String {
    format!(
        "v=$({cli} version --json 2>/dev/null); echo \"@status $?\"; \
         if [ -n \"$v\" ]; then echo \"$v\"; fi"
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
         if ($g) {{ Write-Output \"@cli $($g.Source)\"; {run}; exit 0 }} }}; \
         Write-Output '@none'",
        run = probe_run_windows("$g.Source")
    )
}

/// [`probe_run_posix`] in PowerShell. An unhandled exception on Windows lands
/// in `$LASTEXITCODE` as its NTSTATUS (an access violation is `0xC0000005`),
/// which [`describe_status`] reads back as a crash rather than an exit code.
///
/// The native command's stderr is left to flow to PowerShell's own, as it
/// always did — `run_script_classified` reads that separately and ignores it
/// on success, and `2>$null` on a native command is the redirection
/// PowerShell handles least predictably across its versions.
fn probe_run_windows(cli: &str) -> String {
    format!(
        "$v = & {cli} version --json; Write-Output \"@status $LASTEXITCODE\"; \
         if ($v) {{ Write-Output $v }}"
    )
}

/// Why a probe produced no usable CLI — and, the part that decides what
/// happens next, whether the host answered at all.
///
/// The two are not degrees of the same failure. A host that is down is helped
/// by waiting, and [`usable`] backs it off. A host that answered with a
/// `thurbox-cli` that does not run is helped by nothing but replacing that
/// binary, and waiting is how it stays broken: the cached `No` skips the
/// mirror, and the mirror is the only caller that reaches [`provision`].
#[derive(Debug, Clone)]
pub struct ProbeFailure {
    pub message: String,
    /// The host ran the probe and the `thurbox-cli` it found there is broken.
    pub broken_cli: bool,
}

impl ProbeFailure {
    /// The probe itself did not come back in a shape this side can read — no
    /// shell, no output, a line that is not the protocol. Nothing here says a
    /// CLI was even found, so nothing here justifies re-provisioning.
    fn unreadable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            broken_cli: false,
        }
    }

    /// A `thurbox-cli` was found on the host and it does not work.
    fn broken(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            broken_cli: true,
        }
    }
}

impl std::fmt::Display for ProbeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Ask the host whether it has a `thurbox-cli`, and what version.
pub fn probe(host: &HostDef) -> Result<Option<CliInfo>, ProbeFailure> {
    let script = if host.is_windows() {
        probe_script_windows()
    } else {
        probe_script_posix()
    };
    let stdout =
        run_script(host, &script, "thurbox-cli probe").map_err(ProbeFailure::unreadable)?;
    parse_probe(&stdout)
}

/// [`probe`] for a known path — what a freshly provisioned copy is checked
/// with, since PATH would not find it.
fn probe_at(host: &HostDef, path: &str) -> Result<Option<CliInfo>, ProbeFailure> {
    let script = if host.is_windows() {
        format!(
            "Write-Output \"@cli {path}\"; {run}",
            run = probe_run_windows(&crate::shell::powershell_quote(path))
        )
    } else {
        format!(
            "echo \"@cli {path}\"; {run}",
            run = probe_run_posix(&crate::shell::posix_quote(path))
        )
    };
    let stdout =
        run_script(host, &script, "thurbox-cli probe").map_err(ProbeFailure::unreadable)?;
    parse_probe(&stdout)
}

/// How the host's shell reported an exit, in the words an operator can act
/// on.
///
/// A POSIX shell reports a signal death as `128 + n`, so `139` is `SIGSEGV` —
/// which is what a truncated binary does, and what was being reported as
/// "printed no JSON version". Windows has no such convention and instead
/// hands back the NTSTATUS of an unhandled exception, which arrives here as a
/// large negative number.
fn describe_status(status: i64) -> String {
    if (129..=192).contains(&status) {
        let signal = status - 128;
        return match signal_name(signal) {
            Some(name) => format!("died on signal {signal} ({name})"),
            None => format!("died on signal {signal}"),
        };
    }
    // Windows hands back the NTSTATUS of an unhandled exception, which reaches
    // here as a negative number or as the same bits unsigned depending on how
    // the shell printed it. `exited 3221225477` is that value read as an exit
    // code, which tells an operator nothing; both spellings are one value.
    if !(0..=0xFFFF).contains(&status) {
        return format!("crashed (0x{:08X})", status as i32 as u32);
    }
    format!("exited {status}")
}

/// The signals worth naming: the ones a broken or killed binary dies of.
/// Anything else is reported by number, which is still the truth.
fn signal_name(signal: i64) -> Option<&'static str> {
    match signal {
        4 => Some("SIGILL"),
        6 => Some("SIGABRT"),
        7 => Some("SIGBUS"),
        8 => Some("SIGFPE"),
        9 => Some("SIGKILL"),
        11 => Some("SIGSEGV"),
        15 => Some("SIGTERM"),
        _ => None,
    }
}

/// Parse the probe's line protocol: `@cli <path>`, `@status <n>`, then a JSON
/// document — or `@none`.
///
/// Three outcomes an operator must be able to tell apart, because each one
/// asks for something different: no CLI on the host (`@none` — provision
/// one), a CLI that died on a signal (replace the binary), and a CLI that ran
/// and said something unreadable (look at what it is). They all used to read
/// as the last one.
pub(crate) fn parse_probe(stdout: &str) -> Result<Option<CliInfo>, ProbeFailure> {
    let mut lines = stdout.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(first) = lines.next() else {
        return Err(ProbeFailure::unreadable("probe printed nothing"));
    };
    if first == "@none" {
        return Ok(None);
    }
    let Some(path) = first.strip_prefix("@cli ") else {
        return Err(ProbeFailure::unreadable(format!(
            "unexpected probe output: {first}"
        )));
    };
    let path = path.trim();
    let mut rest: Vec<&str> = lines.collect();
    // Written by every script above, and those ship with this parser, so its
    // absence is a malformed answer rather than an older host. Read as a
    // clean exit: the lines after it still decide.
    let status = rest
        .first()
        .and_then(|line| line.strip_prefix("@status "))
        .and_then(|code| code.trim().parse::<i64>().ok());
    if status.is_some() {
        rest.remove(0);
    }
    if let Some(status) = status.filter(|status| *status != 0) {
        return Err(ProbeFailure::broken(format!(
            "thurbox-cli at {path} {} instead of reporting a version",
            describe_status(status)
        )));
    }
    let body: String = rest.join("\n");
    if body.is_empty() {
        return Err(ProbeFailure::broken(format!(
            "thurbox-cli at {path} ran and printed nothing"
        )));
    }
    let json: Value = serde_json::from_str(&body).map_err(|e| {
        ProbeFailure::broken(format!(
            "thurbox-cli at {path} printed no JSON version ({e})"
        ))
    })?;
    let version = json
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| ProbeFailure::broken(format!("thurbox-cli at {path} reported no version")))?
        .to_string();
    Ok(Some(CliInfo {
        path: path.to_string(),
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
/// A non-zero exit is passed on verbatim, since it names what went wrong
/// *there*, which is what the caller needs to show: the host's stderr when it
/// has any (a transport failure that never reached the CLI), otherwise the
/// message from the structured error document the CLI prints on stdout.
/// How far a failed host CLI call actually got.
///
/// Decided by **which layer failed** — the launcher, the transport, or the CLI
/// itself — never by what the message says. A message is written for a person
/// and can say anything; a delete that has to choose between "nothing ran
/// there" and "the host refused" cannot be deciding it by looking for
/// substrings, because the first unanticipated wording lands in the wrong
/// branch silently and in whichever direction happens to be worse.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reach {
    /// The question never arrived: the launcher would not start, or `ssh`
    /// failed on its own account (exit 255 — its documented "an error
    /// occurred", distinct from the remote command's status, which it passes
    /// through). Nothing ran on the host, so nothing there acted on it.
    Unreached,
    /// `thurbox-cli` ran on the host and answered with an error of its own,
    /// as the structured document every remote invocation asks for.
    Answered,
    /// Something in between failed and the layer cannot be told: no shell on
    /// the host, a binary that is not there, output in a shape nothing
    /// recognises. **Not** a synonym for either of the others — a caller must
    /// treat it as the unanswered question it is, and pick whichever branch
    /// destroys nothing.
    Undetermined,
}

/// A failed [`run`], with the layer that failed alongside the message.
#[derive(Clone, Debug)]
pub struct RunFailure {
    pub message: String,
    pub reach: Reach,
}

impl RunFailure {
    fn new(message: String, reach: Reach) -> Self {
        Self { message, reach }
    }
}

impl std::fmt::Display for RunFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<RunFailure> for String {
    fn from(failure: RunFailure) -> Self {
        failure.message
    }
}

/// [`run`], keeping the layer that failed instead of flattening it to a
/// message.
///
/// Only the delete path needs it, and it needs it badly: falling back to a
/// local teardown is destructive when the host was in fact fine, and aborting
/// leaves nothing recorded when the host was in fact gone. Which of those is
/// safe depends entirely on whether the question arrived — see [`Reach`].
pub fn run_classified(host: &HostDef, cli: &CliInfo, args: &[&str]) -> Result<Value, RunFailure> {
    #[cfg(test)]
    if let Some(answer) = fake::run_override(host, args) {
        return answer;
    }
    let script = if host.is_windows() {
        cli_script_windows(&cli.path, args)
    } else {
        cli_script_posix(&cli.path, args)
    };
    let stdout = run_script_classified(host, &script, "thurbox-cli")?;
    serde_json::from_str(&stdout).map_err(|e| {
        // It ran and said something; that something is not what this build
        // knows how to read. The host may well have done the thing.
        RunFailure::new(
            format!(
                "thurbox-cli on '{}' printed no JSON for `{}` ({e}): {}",
                host.name,
                args.join(" "),
                stdout.trim()
            ),
            Reach::Undetermined,
        )
    })
}

pub fn run(host: &HostDef, cli: &CliInfo, args: &[&str]) -> Result<Value, String> {
    run_classified(host, cli, args).map_err(String::from)
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
    run_script_classified(host, script, action).map_err(String::from)
}

/// [`run_script`] keeping the layer that failed. See [`Reach`].
///
/// The classification is entirely structural:
///
/// - the launcher would not start at all — no `ssh`/`wsl.exe` on this machine,
///   or it could not be executed — so nothing left this machine: `Unreached`.
/// - `ssh` exited **255**, which is its documented code for "an error
///   occurred" *in ssh*. It passes a remote command's own status through
///   untouched (a remote `exit 7` exits 7), and `thurbox-cli` only ever exits
///   1, 2 or 3 ([`crate::cli::EXIT_ERROR`] and friends), so 255 cannot be the
///   host CLI answering: `Unreached`.
/// - the host CLI answered on stdout with the structured `{"error": …}` every
///   remote invocation asks for: `Answered`.
/// - anything else — a shell that could not find the binary (127), a host
///   running something that is not thurbox, stderr from a layer nobody here
///   owns: `Undetermined`.
///
/// `wsl.exe` has no 255 convention of its own, so a WSL host is never
/// classified `Unreached` by exit status — only by a launcher that would not
/// start. That is the honest limit rather than a guess, and it costs little:
/// `wsl.exe` runs on this machine, so "could not reach it" is a far narrower
/// condition there than it is over a network.
fn run_script_classified(host: &HostDef, script: &str, action: &str) -> Result<String, RunFailure> {
    #[cfg(test)]
    if let Some(answer) = fake::script_override(host, script) {
        return answer;
    }
    let mut command = if host.is_windows() {
        crate::git::host_powershell_c(host, script)
    } else {
        // The launcher's environment is a non-login one, and a `session
        // create` run here pins whatever `PATH` it inherits on the agent's
        // pane — so the host's login `PATH` goes in first (see
        // `agent::host_path`).
        let script = match crate::agent::host_path::assignment_for(host) {
            Some(path) => format!("{path}{script}"),
            None => script.to_string(),
        };
        crate::git::host_shell_c(host, &script)
    };
    let output = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| {
            RunFailure::new(
                format!("could not start {action} on '{}': {e}", host.name),
                Reach::Unreached,
            )
        })?;
    if !output.status.success() {
        let stderr = crate::git::reportable_stderr(&output.stderr);
        let stderr = stderr
            .trim()
            .strip_prefix("error: ")
            .unwrap_or(stderr.trim());
        // The host CLI reports its failures on *stdout* now, as a structured
        // document (AXI principle 6), so an empty stderr no longer means it
        // said nothing. Reading only stderr turned every remote error into a
        // bare "failed (exit 1)" and threw away the reason, which is the whole
        // value of delegating to the host in the first place. stderr is still
        // read first: a transport failure — ssh could not connect, the shell
        // could not find the binary — never reaches the CLI at all.
        let answered = reported_error(&output.stdout);
        let reported = if stderr.is_empty() {
            answered.clone()
        } else {
            Some(stderr.to_string())
        };
        let code = output.status.code();
        let reach = classify_failure(host.is_wsl(), code, answered.is_some());
        return Err(RunFailure::new(
            reported.unwrap_or_else(|| {
                format!(
                    "{action} on '{}' failed (exit {})",
                    host.name,
                    code.map_or("?".to_string(), |c| c.to_string())
                )
            }),
            reach,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `ssh`'s own failure code. ssh(1): "exits with the exit status of the remote
/// command or with 255 if an error occurred" — so this value, and only this
/// value, is ssh saying the failure was its own rather than the host's.
const SSH_ERROR_EXIT: i32 = 255;

/// Which layer a failed remote invocation failed at, from the exit status and
/// whether the host CLI wrote its structured answer. See [`Reach`].
///
/// Structural, in this order:
///
/// 1. only `ssh` owns 255, and `wsl.exe` has no such convention, so a WSL host
///    is never called `Unreached` on a status alone;
/// 2. the `{"error": …}` document is positive proof `thurbox-cli` ran;
/// 3. failing that, an exit code that is one of the CLI's *own*
///    ([`CLI_EXIT_CODES`]) still says something thurbox-shaped ran and refused
///    — which matters because a host on an older build reported its failures
///    on stderr rather than as that document, and reading such a refusal as
///    "nothing answered" is what would let it be overridden.
///
/// Anything left over — 127 from a shell that could not find the binary, a
/// host running something else entirely, no status at all — is
/// [`Reach::Undetermined`]: its own answer, never rounded to the nearest of
/// the other two.
fn classify_failure(is_wsl: bool, code: Option<i32>, answered: bool) -> Reach {
    if !is_wsl && code == Some(SSH_ERROR_EXIT) {
        return Reach::Unreached;
    }
    if answered || code.is_some_and(|c| CLI_EXIT_CODES.contains(&c)) {
        return Reach::Answered;
    }
    Reach::Undetermined
}

/// Every code `thurbox-cli` exits with of its own accord. A status outside
/// this set did not come from the host's thurbox.
///
/// Spelled out rather than imported: `session_ops` may not reference `cli`
/// (`tests/architecture_rules.rs`). These are `cli::EXIT_ERROR`,
/// `cli::EXIT_USAGE` and `cli::EXIT_AMBIGUOUS`, and
/// `cli::tests::host_cli_knows_every_exit_code_this_binary_uses` fails if they
/// ever drift apart.
pub(crate) const CLI_EXIT_CODES: [i32; 3] = [1, 2, 3];

/// Pull the message out of a failed host CLI's stdout.
///
/// Every remote invocation passes `--json` (see [`cli_script_posix`]), so a
/// failure is `{"error": …, "suggestion": …}`. A host running an older thurbox
/// wrote nothing to stdout on failure and a host running something else
/// entirely could write anything, so both fall back to the caller's generic
/// message rather than surfacing a stray line as if it were a diagnosis.
fn reported_error(stdout: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stdout);
    let value: Value = serde_json::from_str(text.trim()).ok()?;
    let message = value.get("error")?.as_str()?.trim();
    if message.is_empty() {
        return None;
    }
    match value.get("suggestion").and_then(Value::as_str) {
        Some(hint) if !hint.trim().is_empty() => Some(format!("{message} ({})", hint.trim())),
        _ => Some(message.to_string()),
    }
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
/// `<data dir>/bin/`, ask it what it is, and return that.
///
/// A release build fetches the release archive for the host's platform,
/// verified against the release checksums, and extracts it on the host. A dev
/// build has no release: it ships its own sibling `thurbox-cli` when the host
/// is the same platform, and refuses otherwise — the refusal is what
/// `Sharing: off` shows, and the legacy path takes over.
///
/// The installed binary is asked for its version before this returns, because
/// a success nobody checked is what let a broken host hide: the archive is
/// checksummed on this machine, nothing checksums what lands on the host, and
/// a `thurbox-cli` that was 54% of itself was installed, logged as
/// provisioned and left to segfault under every later probe.
pub fn provision(host: &HostDef) -> Result<CliInfo, String> {
    let dest = install(host)?;
    let cli = verify_provisioned(host, &dest)?;
    tracing::info!(
        "provisioned thurbox-cli {} on '{}' at {dest}",
        cli.version,
        host.name
    );
    Ok(cli)
}

/// Ask the binary just installed at `dest` for its version, and refuse to
/// call the provisioning a success when it will not answer.
///
/// `fetch_archive` verifies the download against the release checksums on
/// **this** machine; nothing verified what landed on the host, and nothing
/// asked the installed file whether it ran. Measured: a 6,815,232-byte
/// `thurbox-cli` — 54% of itself, its ELF header still declaring section
/// headers at 12,627,792 — installed, logged as `provisioned thurbox-cli
/// <version>`, and segfaulting on every later probe.
///
/// Checking here rather than after the copy is deliberate: the measured file
/// was written *two hours after* the extraction that wrote its siblings, so a
/// checksum taken at install time would have passed and the binary would
/// still have been broken. The question worth asking is not "did the bytes
/// arrive" but "does the thing there work", and it is asked of whatever is
/// there now.
fn verify_provisioned(host: &HostDef, dest: &str) -> Result<CliInfo, String> {
    match probe_at(host, dest) {
        Ok(Some(cli)) => Ok(cli),
        Ok(None) => Err(format!(
            "the provisioned thurbox-cli at {dest} is not there"
        )),
        // A broken-CLI message already names the binary it is about. Anything
        // else is the host failing to answer at all, and says nothing about
        // `dest`, so it is not worth reading as though it did.
        Err(e) if e.broken_cli => Err(format!("the provisioned {e}")),
        Err(e) => Err(format!(
            "could not ask the provisioned thurbox-cli at {dest} for its version: {e}"
        )),
    }
}

/// Put the bytes on the host and answer with where they landed. Says nothing
/// about whether what landed runs.
fn install(host: &HostDef) -> Result<String, String> {
    #[cfg(test)]
    if let Some(dest) = fake::install_override(host) {
        return dest;
    }
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

    type Runner = Box<dyn Fn(&HostDef, &[String]) -> Result<Value, super::RunFailure>>;

    /// Stands in for the host's shell: every script this module would have
    /// sent over ssh or `wsl.exe` is handed here instead.
    type ScriptRunner = Box<dyn Fn(&HostDef, &str) -> Result<String, super::RunFailure>>;

    /// Stands in for the fetch-ship-extract half of [`super::provision`],
    /// answering with the path the CLI landed at — so a test drives the half
    /// that matters, the verification, against a binary it planted itself.
    type Installer = Box<dyn Fn(&HostDef) -> Result<String, String>>;

    thread_local! {
        static USABLE: RefCell<Option<Usable>> = const { RefCell::new(None) };
        static RUNNER: RefCell<Option<Runner>> = const { RefCell::new(None) };
        static CALLS: RefCell<Vec<Vec<String>>> = const { RefCell::new(Vec::new()) };
        static SCRIPTS: RefCell<Option<ScriptRunner>> = const { RefCell::new(None) };
        static INSTALLER: RefCell<Option<Installer>> = const { RefCell::new(None) };
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
        SCRIPTS.with(|s| *s.borrow_mut() = None);
        INSTALLER.with(|i| *i.borrow_mut() = None);
    }

    pub(super) fn script_override(
        host: &HostDef,
        script: &str,
    ) -> Option<Result<String, super::RunFailure>> {
        SCRIPTS.with(|s| s.borrow().as_ref().map(|run| run(host, script)))
    }

    pub(super) fn install_override(host: &HostDef) -> Option<Result<String, String>> {
        INSTALLER.with(|i| i.borrow().as_ref().map(|install| install(host)))
    }

    /// The two seams the planted-binary tests drive, with the local shell
    /// that makes them worth driving. Unix-only because those tests run a real
    /// `/bin/sh` against a file they wrote, and an ungated setter nothing calls
    /// is dead code under the Windows clippy job.
    #[cfg(unix)]
    pub fn install_script_runner(runner: ScriptRunner) {
        SCRIPTS.with(|s| *s.borrow_mut() = Some(runner));
    }

    #[cfg(unix)]
    pub fn install_installer(installer: Installer) {
        INSTALLER.with(|i| *i.borrow_mut() = Some(installer));
    }

    /// A script runner that runs the host's own script through **this**
    /// machine's `sh`, with `home` standing in for the host's `$HOME` and a
    /// `PATH` that resolves nothing — so the script text under test is the
    /// script text that ships, and a binary the test planted is really
    /// executed, really segfaults, and is really reported by a real shell.
    #[cfg(unix)]
    pub fn local_shell(home: &std::path::Path) -> ScriptRunner {
        let home = home.to_path_buf();
        Box::new(move |_host, script| {
            let out = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(script)
                .env("HOME", &home)
                .env("PATH", home.join("no-path"))
                .output()
                .map_err(|e| super::RunFailure::new(e.to_string(), super::Reach::Unreached))?;
            if !out.status.success() {
                return Err(super::RunFailure::new(
                    String::from_utf8_lossy(&out.stderr).trim().to_string(),
                    super::Reach::Undetermined,
                ));
            }
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        })
    }

    /// Every argument list the scripted runner was asked to run, in order.
    pub fn calls() -> Vec<Vec<String>> {
        CALLS.with(|c| c.borrow().clone())
    }

    pub(super) fn usable_override() -> Option<Usable> {
        USABLE.with(|u| u.borrow().clone())
    }

    /// A failure that never reached the host, as a test would script it.
    pub fn unreached(message: &str) -> super::RunFailure {
        super::RunFailure::new(message.to_string(), super::Reach::Unreached)
    }

    /// A failure the host itself answered with.
    pub fn answered(message: &str) -> super::RunFailure {
        super::RunFailure::new(message.to_string(), super::Reach::Answered)
    }

    /// A failure whose layer could not be told — the case a caller must never
    /// quietly round to one of the other two.
    pub fn undetermined(message: &str) -> super::RunFailure {
        super::RunFailure::new(message.to_string(), super::Reach::Undetermined)
    }

    pub(super) fn run_override(
        host: &HostDef,
        args: &[&str],
    ) -> Option<Result<Value, super::RunFailure>> {
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

    /// Which layer failed, decided from the exit status and whether the host
    /// CLI wrote its own structured answer — never from the message. The two
    /// wrong answers cost differently and both are silent: a transport failure
    /// read as a reply leaves an orphaned agent nobody looks for again, and a
    /// reply read as a transport failure authorises a destructive local
    /// teardown against a host that was perfectly fine.
    #[test]
    fn a_failed_remote_call_is_classified_by_layer_not_by_message() {
        // ssh's own code, and only ssh's: it passes a remote command's status
        // through untouched, and thurbox-cli only ever exits 1, 2 or 3.
        assert_eq!(
            classify_failure(false, Some(SSH_ERROR_EXIT), false),
            Reach::Unreached
        );
        // Even when the host CLI would have had something to say: nothing ran
        // there to say it.
        assert_eq!(
            classify_failure(false, Some(SSH_ERROR_EXIT), true),
            Reach::Unreached
        );
        // `wsl.exe` has no 255 convention, so the same status proves nothing.
        assert_eq!(
            classify_failure(true, Some(SSH_ERROR_EXIT), false),
            Reach::Undetermined
        );
        // The structured `{"error": …}` document is positive proof the CLI ran.
        assert_eq!(classify_failure(false, Some(1), true), Reach::Answered);
        // An exit code of the CLI's own still says something thurbox-shaped
        // refused, even on a build too old to write the structured document.
        for code in [Some(1), Some(2), Some(3)] {
            assert_eq!(
                classify_failure(false, code, false),
                Reach::Answered,
                "exit {code:?} is one thurbox-cli gives of its own accord"
            );
        }
        // A shell that could not find the binary (127), a host running
        // something else, a signal: reached or not, nothing here can tell.
        for code in [Some(127), Some(126), Some(9), None] {
            assert_eq!(
                classify_failure(false, code, false),
                Reach::Undetermined,
                "exit {code:?} says nothing about which layer failed"
            );
        }
    }

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
            "@cli /usr/local/bin/thurbox-cli\n@status 0\n{\"version\":\"1.4.0\",\
             \"tmux_socket\":\"thurbox\",\"data_dir\":\"/home/me/.local/share/thurbox\",\
             \"schema_version\":40}\n",
        )
        .unwrap()
        .unwrap();
        assert_eq!(found.path, "/usr/local/bin/thurbox-cli");
        assert_eq!(found.version, "1.4.0");
        assert_eq!(found.tmux_socket.as_deref(), Some("thurbox"));
        assert_eq!(found.schema_version, Some(40));
        assert_eq!(parse_probe("@none\n").unwrap(), None);
        // Neither of these says a CLI was found, so neither justifies
        // re-provisioning; both are the host failing to answer the protocol.
        for stdout in ["", "bash: no such thing\n"] {
            let e = parse_probe(stdout).unwrap_err();
            assert!(!e.broken_cli, "{}", e.message);
        }
        // An old CLI prints only its version.
        let old = parse_probe("@cli thurbox-cli\n@status 0\n{\"version\":\"1.1.0\"}")
            .unwrap()
            .unwrap();
        assert_eq!(old.schema_version, None);
    }

    /// The three answers an operator has to be able to tell apart. Every one
    /// of them used to read as the last: "printed no JSON version", which
    /// sends them looking at versions and protocols for a file that was half
    /// a binary.
    #[test]
    fn a_broken_host_cli_is_named_by_what_it_did() {
        let died = parse_probe("@cli /x/thurbox-cli\n@status 139\n").unwrap_err();
        assert_eq!(
            died.message,
            "thurbox-cli at /x/thurbox-cli died on signal 11 (SIGSEGV) \
             instead of reporting a version"
        );
        assert!(died.broken_cli);
        // Windows has no 128+n convention: an unhandled exception arrives as
        // the NTSTATUS itself (0xC0000005 is an access violation), in whichever
        // of its two spellings the shell printed it. Read as an exit code, the
        // unsigned one would say `exited 3221225477` and mean nothing.
        for status in ["-1073741819", "3221225477"] {
            let crashed =
                parse_probe(&format!("@cli C:/x/thurbox-cli.exe\n@status {status}\n")).unwrap_err();
            assert!(
                crashed.message.contains("crashed (0xC0000005)"),
                "{crashed}"
            );
            assert!(crashed.broken_cli);
        }
        let silent = parse_probe("@cli /x/thurbox-cli\n@status 0\n").unwrap_err();
        assert!(
            silent.message.contains("ran and printed nothing"),
            "{silent}"
        );
        let gibberish = parse_probe("@cli /x/thurbox-cli\n@status 0\nnot json\n").unwrap_err();
        assert!(
            gibberish.message.contains("printed no JSON version"),
            "{gibberish}"
        );
        // And "there is none" stays its own answer, which provisioning
        // already handled.
        assert_eq!(parse_probe("@none\n").unwrap(), None);
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

    #[cfg(unix)]
    #[test]
    fn advertising_links_the_running_cli_where_a_peer_probes() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        advertise_running_cli();
        let link = crate::paths::database_file()
            .unwrap()
            .parent()
            .unwrap()
            .join(HOST_BIN_DIR)
            .join("thurbox-cli");
        let target = crate::agent::tmux::resolve_cli_binary();
        if target.is_absolute() && target.exists() {
            assert_eq!(std::fs::read_link(&link).unwrap(), target);
            // A stale link is replaced, a true one left alone.
            std::fs::remove_file(&link).unwrap();
            std::os::unix::fs::symlink("/nowhere/thurbox-cli", &link).unwrap();
            advertise_running_cli();
            assert_eq!(std::fs::read_link(&link).unwrap(), target);
        } else {
            // A test binary with no `thurbox-cli` beside it advertises nothing.
            assert!(std::fs::symlink_metadata(&link).is_err());
        }
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

    #[test]
    fn a_failing_host_is_asked_less_and_less_often() {
        // The first failure keeps the flat interval, so a host that is merely
        // rebooting is picked up as promptly as it always was.
        assert_eq!(retry_after(0), PROBE_RETRY);
        assert_eq!(retry_after(1), PROBE_RETRY);
        assert_eq!(retry_after(2), PROBE_RETRY * 2);
        assert_eq!(retry_after(3), PROBE_RETRY * 4);
        // And a host that can never be provisioned settles at the ceiling
        // rather than costing an archive download a minute forever.
        assert_eq!(retry_after(10), PROBE_RETRY_MAX);
        assert_eq!(retry_after(u32::MAX), PROBE_RETRY_MAX);
    }

    #[test]
    fn a_host_cli_failure_is_read_off_stdout() {
        // The host CLI reports failures as a document on stdout now, so this
        // is the whole reason a delegated create says what went wrong instead
        // of "exit 1".
        let stdout = br#"{"error":"Session not found: abc","suggestion":"run session list"}"#;
        assert_eq!(
            reported_error(stdout).as_deref(),
            Some("Session not found: abc (run session list)")
        );
    }

    #[test]
    fn a_host_cli_failure_without_a_suggestion_is_just_the_message() {
        assert_eq!(
            reported_error(br#"{"error":"boom"}"#).as_deref(),
            Some("boom")
        );
    }

    #[test]
    fn output_that_is_not_a_thurbox_error_is_left_to_the_generic_message() {
        // An older host wrote nothing; something that is not thurbox at all
        // could write anything. Neither is a diagnosis worth surfacing as one.
        assert_eq!(reported_error(b""), None);
        assert_eq!(reported_error(b"command not found"), None);
        assert_eq!(reported_error(br#"{"ok":true}"#), None);
        assert_eq!(reported_error(br#"{"error":"  "}"#), None);
    }

    #[cfg(unix)]
    fn provisioned_bin_dir(root: &std::path::Path) -> std::path::PathBuf {
        let bin = root.join(HOST_BIN_DIR);
        std::fs::create_dir_all(&bin).unwrap();
        bin
    }

    /// On a provisioned host the running CLI *is* the path being advertised —
    /// `resolve_cli_binary` answers with a sibling of the running exe, and
    /// there that exe is `<data dir>/bin/thurbox`. Advertising over it removed
    /// the binary the provisioner had just extracted and left an `ELOOP` in its
    /// place (issue #1193).
    #[cfg(unix)]
    #[test]
    fn the_cli_is_never_advertised_as_a_link_to_its_own_path() {
        let root = tempfile::TempDir::new().unwrap();
        let bin = provisioned_bin_dir(root.path());
        let cli = bin.join("thurbox-cli");
        std::fs::write(&cli, b"#!/bin/sh\nexit 0\n").unwrap();

        advertise_cli_in(&bin, &cli);

        let kind = std::fs::symlink_metadata(&cli).unwrap().file_type();
        assert!(
            kind.is_file(),
            "the provisioned CLI is the advertisement; it must be left as the \
             real file it is rather than replaced by a link to itself"
        );
        assert_eq!(std::fs::read(&cli).unwrap(), b"#!/bin/sh\nexit 0\n");
    }

    /// A machine already carrying the loop is only ever fixed by thurbox
    /// noticing — no migration reaches a WSL distro — and every exit the
    /// function had preserved it instead.
    #[cfg(unix)]
    #[test]
    fn an_existing_self_referential_link_is_removed_rather_than_kept() {
        let root = tempfile::TempDir::new().unwrap();
        let bin = provisioned_bin_dir(root.path());
        let link = bin.join("thurbox-cli");
        std::os::unix::fs::symlink(&link, &link).unwrap();
        // What `resolve_cli_binary` answers once the loop is there: it looks
        // for a sibling that `exists()`, and a loop does not, so it falls back
        // to the bare name — which is why no guard below the heal can see it.
        let target = std::path::PathBuf::from("thurbox-cli");

        advertise_cli_in(&bin, &target);

        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "a link whose target is its own path is never correct, however it \
             got there, and leaving it is what made the break permanent"
        );
    }

    /// The case the function exists for, unchanged: a checkout's
    /// `target/debug/thurbox-cli` is on nobody's PATH, so a peer probing this
    /// machine needs the advertisement to find it.
    #[cfg(unix)]
    #[test]
    fn a_dev_checkout_is_still_advertised() {
        let root = tempfile::TempDir::new().unwrap();
        let bin = provisioned_bin_dir(root.path());
        let debug = root.path().join("target/debug");
        std::fs::create_dir_all(&debug).unwrap();
        let target = debug.join("thurbox-cli");
        std::fs::write(&target, b"#!/bin/sh\n").unwrap();

        advertise_cli_in(&bin, &target);

        assert_eq!(std::fs::read_link(bin.join("thurbox-cli")).unwrap(), target);
    }

    /// And a stale advertisement is still replaced — the reason the link is
    /// refreshed on every start rather than written once.
    #[cfg(unix)]
    #[test]
    fn a_stale_advertisement_is_replaced() {
        let root = tempfile::TempDir::new().unwrap();
        let bin = provisioned_bin_dir(root.path());
        let link = bin.join("thurbox-cli");
        std::os::unix::fs::symlink(root.path().join("gone/thurbox-cli"), &link).unwrap();
        let target = root.path().join("thurbox-cli");
        std::fs::write(&target, b"#!/bin/sh\n").unwrap();

        advertise_cli_in(&bin, &target);

        assert_eq!(std::fs::read_link(&link).unwrap(), target);
    }

    /// The same rule where the two paths differ: a real file there is somebody
    /// else's, and this function manages only a symlink of its own making.
    #[cfg(unix)]
    #[test]
    fn a_real_file_at_the_advertised_path_is_never_removed() {
        let root = tempfile::TempDir::new().unwrap();
        let bin = provisioned_bin_dir(root.path());
        let installed = bin.join("thurbox-cli");
        std::fs::write(&installed, b"#!/bin/sh\nexit 0\n").unwrap();
        let target = root.path().join("checkout/thurbox-cli");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"#!/bin/sh\n").unwrap();

        advertise_cli_in(&bin, &target);

        assert_eq!(std::fs::read(&installed).unwrap(), b"#!/bin/sh\nexit 0\n");
    }

    /// One `bin` directory reached two ways, as itself and through a symlinked
    /// parent — the shape a resolved `current_exe` and a `$HOME`-built data
    /// directory are in on an ordinary machine.
    #[cfg(unix)]
    fn aliased_bin_dirs(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
        let real = root.join("real");
        let bin = real.join(HOST_BIN_DIR);
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(&real, root.join("home")).unwrap();
        (bin, root.join("home").join(HOST_BIN_DIR))
    }

    /// Compared as strings, two spellings of one path read as two paths, and
    /// `symlink(target, link)` writes the loop all over again.
    #[cfg(unix)]
    #[test]
    fn an_aliased_spelling_of_the_advertised_path_never_becomes_a_loop() {
        let root = tempfile::TempDir::new().unwrap();
        let (bin, aliased) = aliased_bin_dirs(root.path());
        // A true advertisement already there, which the function is entitled
        // to replace — so only the target comparison can stop it.
        let elsewhere = root.path().join("thurbox-cli");
        std::fs::write(&elsewhere, b"#!/bin/sh\n").unwrap();
        std::os::unix::fs::symlink(&elsewhere, aliased.join("thurbox-cli")).unwrap();

        advertise_cli_in(&aliased, &bin.join("thurbox-cli"));

        assert_eq!(
            std::fs::read_link(bin.join("thurbox-cli")).unwrap(),
            elsewhere,
            "the running CLI and the advertised path are one file under two \
             names; relinking one to the other is the loop"
        );
    }

    /// A `thurbox-cli` that dies the moment it is asked anything — the shape a
    /// truncated delivery takes. Measured on a host as a 6,815,232-byte
    /// executable whose ELF header declared its section headers at 12,627,792:
    /// 54% of itself, and a segfault on every invocation.
    #[cfg(unix)]
    const SEGFAULTS: &str = "#!/bin/sh\nkill -SEGV $$\n";

    /// A `thurbox-cli` this binary would accept: same major, same schema.
    #[cfg(unix)]
    fn working_cli() -> String {
        format!(
            "#!/bin/sh\necho '{{\"version\":\"{}\",\"tmux_socket\":\"thurbox\",\
             \"schema_version\":{}}}'\n",
            crate::agent::version_check::current_version(),
            crate::storage::SCHEMA_VERSION
        )
    }

    /// Write `body` as the host's provisioned CLI, at the path
    /// [`probe_script_posix`] looks in first.
    #[cfg(unix)]
    fn plant_cli(home: &std::path::Path, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let dir = home
            .join(".local/share")
            .join(crate::paths::app_dir_name())
            .join(HOST_BIN_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let cli = dir.join("thurbox-cli");
        std::fs::write(&cli, body).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        cli.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    fn probe_host(name: &str) -> HostDef {
        HostDef {
            name: name.into(),
            destination: format!("me@{name}"),
            ..HostDef::default()
        }
    }

    /// The thread-locals the fakes live in, cleared however the test ends.
    #[cfg(unix)]
    struct FakeGuard;

    #[cfg(unix)]
    impl Drop for FakeGuard {
        fn drop(&mut self) {
            fake::clear();
        }
    }

    /// `provision` never asked the thing it had just installed whether it
    /// worked: it fetched, shipped, extracted, logged `provisioned thurbox-cli
    /// <version>` and returned `Ok`. `fetch_archive` checksums the download on
    /// *this* machine, and nothing checksummed what landed on the host — so a
    /// binary that segfaults was reported as a success, which is what let it
    /// hide.
    #[cfg(unix)]
    #[test]
    fn provisioning_a_binary_that_does_not_run_is_not_a_success() {
        let temp = tempfile::TempDir::new().unwrap();
        let home = temp.path().to_path_buf();
        let _guard = FakeGuard;
        fake::install_script_runner(fake::local_shell(&home));
        fake::install_installer(Box::new(move |_| Ok(plant_cli(&home, SEGFAULTS))));

        let err = provision(&probe_host("bad-install")).unwrap_err();

        assert!(
            err.contains("the provisioned") && err.contains("SIGSEGV"),
            "the refusal must name the binary it is about and what it did: {err}"
        );
    }

    /// The deadlock, end to end. The corrupt CLI made the probe fail, the
    /// failed probe marked the host unusable, an unusable host skips the
    /// mirror, and the mirror is the only thing that reaches `provision` — so
    /// the bad binary disabled the one mechanism that would have replaced it,
    /// and the backoff only made it quieter. "Its CLI does not answer" is not
    /// "the host is unreachable", and only the second is helped by waiting.
    #[cfg(unix)]
    #[test]
    fn a_host_whose_cli_segfaults_is_reprovisioned_rather_than_locked_out() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let temp = tempfile::TempDir::new().unwrap();
        let home = temp.path().to_path_buf();
        let cli = plant_cli(&home, SEGFAULTS);
        let _guard = FakeGuard;
        fake::install_script_runner(fake::local_shell(&home));
        let installs = std::sync::Arc::new(AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&installs);
        fake::install_installer(Box::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(plant_cli(&home, &working_cli()))
        }));
        let host = probe_host("segfaulting-cli");
        forget(&host);

        let verdict = usable(&host);

        assert_eq!(
            installs.load(Ordering::SeqCst),
            1,
            "a host that answers with a broken CLI must be re-provisioned, not \
             left to back off against the very binary that broke it"
        );
        match verdict {
            Usable::Yes(found) => {
                assert_eq!(
                    found.version,
                    crate::agent::version_check::current_version()
                )
            }
            Usable::No(reason) => panic!("locked out by its own broken CLI: {reason}"),
        }
        assert_eq!(
            std::fs::read_to_string(&cli).unwrap(),
            working_cli(),
            "the half-written binary is what the next pass has to replace"
        );
    }

    /// And the refusal that has to survive all of this: a host that did not
    /// answer says nothing about any binary, so it keeps its backoff rather
    /// than paying for a release archive every time the backoff expires.
    #[cfg(unix)]
    #[test]
    fn a_host_that_does_not_answer_is_not_re_provisioned() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let _guard = FakeGuard;
        fake::install_script_runner(Box::new(|_, _| {
            Err(fake::unreached("ssh: connect to host port 22: timed out"))
        }));
        let installs = std::sync::Arc::new(AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&installs);
        fake::install_installer(Box::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            Err("an unreachable host must never get this far".to_string())
        }));
        let host = probe_host("off-the-network");
        forget(&host);

        let verdict = usable(&host);

        assert!(
            matches!(&verdict, Usable::No(reason) if reason.contains("host not answering")),
            "{verdict:?}"
        );
        assert_eq!(
            installs.load(Ordering::SeqCst),
            0,
            "nothing about a host that did not answer says its CLI needs replacing"
        );
    }

    /// And the message an operator reads has to name the cause. "printed no
    /// JSON version" sends them to look at versions and protocols; the truth
    /// was a signal death from a half-written file.
    #[cfg(unix)]
    #[test]
    fn the_probe_names_a_signal_death_rather_than_calling_it_bad_json() {
        let temp = tempfile::TempDir::new().unwrap();
        let home = temp.path().to_path_buf();
        let cli = plant_cli(&home, SEGFAULTS);
        let _guard = FakeGuard;
        fake::install_script_runner(fake::local_shell(&home));

        let err = probe(&probe_host("says-nothing")).unwrap_err();

        assert!(err.message.contains(&cli), "{}", err.message);
        assert!(
            err.message.contains("signal 11 (SIGSEGV)"),
            "a binary that dies on a signal is not a version mismatch: {}",
            err.message
        );
        assert!(
            err.broken_cli,
            "the host answered; it is its CLI that is broken, and only that \
             state is repaired by provisioning rather than by waiting"
        );
    }

    /// And the heal the same way round, since nothing else ever repairs one.
    #[cfg(unix)]
    #[test]
    fn an_aliased_self_referential_link_is_still_removed() {
        let root = tempfile::TempDir::new().unwrap();
        let (bin, aliased) = aliased_bin_dirs(root.path());
        let link = bin.join("thurbox-cli");
        std::os::unix::fs::symlink(&link, &link).unwrap();

        advertise_cli_in(&aliased, &std::path::PathBuf::from("thurbox-cli"));

        assert!(std::fs::symlink_metadata(&link).is_err());
    }
}
