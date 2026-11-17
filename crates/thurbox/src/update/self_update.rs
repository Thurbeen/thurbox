//! Self-update: download, verify, and install a thurbox release beside the
//! running one.
//!
//! This is the auto-update feature (gated behind `[features] auto_update`, on
//! by default for 1.0 — see [`crate::session::settings::FeatureFlags`]). It
//! surfaces two ways, both calling [`perform_update`]:
//!
//! - **TUI** — a silent check on startup (`main::spawn_auto_update`), run on a
//!   background thread so a slow download never blocks the first frame. A newer
//!   release is downloaded and installed beside the running one (the running
//!   process is untouched); the new version applies on the next launch, and the
//!   result is surfaced as a status toast.
//! - **CLI** — `thurbox-cli update` does the same on demand (`--force` bypasses
//!   the up-to-date / dev-build guards).
//!
//! It reuses the version-check plumbing ([`fetch_releases`],
//! [`decide_update`], [`current_version`]) so dev builds (`0.0.0-dev`) never
//! auto-update and **only a release on this binary's own major line is ever
//! installed** ([`crate::update::version_check::RELEASE_LINE`]) — not by the
//! startup update, not by `--force`. Each major so far has been a different
//! program under the same binary name (2.x replaced v1's interface, 3.x
//! replaces its engine), so crossing one is a reinstall the user chooses, never
//! a background download. It installs what
//! `scripts/install.sh` installs — the same release artifacts, the same
//! target-triple mapping, the same digest verified before anything is replaced.
//! It does not reach for the same *tools*: downloads go through the
//! `curl`/`wget` helpers and the unpacker is shelled out to, but the checksum is
//! computed in process, so verification does not depend on what the local
//! machine has on `PATH` (the installer's `sha256sum`/`shasum` do not exist on
//! native Windows — issue #1182).
//!
//! **Windows.** This path used to refuse outright, which made a default-on
//! `auto_update` silently mean nothing there (issue #1172). Two things make
//! Windows different, and both are handled rather than refused:
//!
//! - the release artifact is the `.zip` `install.ps1` extracts, not a tarball,
//!   so `extractor_for` picks the unpacker from the archive's own extension;
//! - a rename cannot replace an executable a process is running from, so
//!   `commit_binary` moves the old one aside first, which Windows allows. Its
//!   doc comment is where the interrupted-swap guarantee is written down.
//!
//! **Releases (Unix).** A release is installed into
//! `<prefix>/lib/thurbox/releases/<version>/bin/`, and `<prefix>/bin` links
//! each program to `<prefix>/lib/thurbox/current/bin/`, the layout
//! `scripts/install.sh` writes. The update flips `current` in one rename,
//! keeps the old release as `previous`, and on Linux runs the new `fleet
//! service restart --expect <version>`. When the manager does not come back
//! it flips `current` back and restarts it again. Releases other than
//! `current` and `previous` are removed once no process runs from them.
//! Windows still replaces the programs in place (issue #1867).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::update::version_check::{current_version, decide_update, fetch_releases, RELEASES_URL};

/// GitHub release-download base for the thurbox repo (same repo as
/// `scripts/install.sh`); the per-release directory is `<base>/v{version}/`.
const RELEASE_BASE: &str = "https://github.com/Thurbeen/thurbox/releases/download";

/// The binaries shipped in a release archive, installed together on update:
/// `BINARIES` in `scripts/ci/release-archive.sh`, which packs the archive (a
/// test holds the two lists equal). All of them, because thurbox runs `fleet`,
/// Fleet runs `runtime-mux` and runtime-mux runs `workspace-git`: an update
/// that left one behind would pair programs from two releases.
///
/// Stems, not file names: the archive and the install directory both spell them
/// with the platform's executable suffix, which [`install_binaries`] appends.
const BINARIES: [&str; 5] = [
    "thurbox",
    "thurbox-cli",
    "fleet",
    "runtime-mux",
    "workspace-git",
];

/// Outcome of an update attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// Already on the newest release of the line; nothing was downloaded.
    /// `next_major` is a later major line's newest release, reported for the
    /// user to take on purpose.
    UpToDate {
        current: String,
        latest: String,
        next_major: Option<String>,
    },
    /// Binaries were replaced; the new version applies on the next launch.
    /// `next_major` as in [`UpdateOutcome::UpToDate`].
    Updated {
        from: String,
        to: String,
        next_major: Option<String>,
    },
    /// A development build — skipped (its version doesn't order against tags).
    /// Only returned when `force` is false.
    SkippedDevBuild { current: String },
}

/// Map an OS/arch pair to the release artifact's Rust target triple. Mirrors
/// `scripts/install.sh`'s `get_target`, and is restricted to the platforms
/// `cd.yml`'s release matrix actually builds an archive for — Linux x86_64 and
/// aarch64 (we pick the portable musl tarballs), Apple-silicon macOS and x86_64
/// Windows — so an update never 404s on a missing artifact. Intel macOS has no
/// build, so it errors cleanly here rather than pointing at a download that
/// isn't there.
/// Pure (takes os/arch) so it's testable; note Rust spells these
/// `"macos"`/`"aarch64"` where `uname` says `darwin`/`arm64`, but they map to
/// the same triples.
pub fn target_triple(os: &str, arch: &str) -> Result<&'static str, String> {
    // Accepts both Rust's spellings (`macos`/`aarch64`) and `uname`'s
    // (`Darwin`/`arm64`), plus PowerShell's `AMD64`, because a *host's* platform
    // arrives as whatever that host's shell printed.
    let os = os.to_ascii_lowercase();
    let arch = arch.to_ascii_lowercase();
    match (os.as_str(), arch.as_str()) {
        ("linux", "x86_64" | "amd64") => Ok("x86_64-unknown-linux-musl"),
        ("linux", "aarch64" | "arm64") => Ok("aarch64-unknown-linux-musl"),
        ("macos" | "darwin", "aarch64" | "arm64") => Ok("aarch64-apple-darwin"),
        ("windows", "x86_64" | "amd64") => Ok("x86_64-pc-windows-msvc"),
        _ => Err(format!(
            "unsupported platform: {os}-{arch} (no release artifact is built \
             for it; see scripts/install.sh)"
        )),
    }
}

/// True for **either** Windows triple.
///
/// `cd.yml` releases `-windows-msvc`, but `-windows-gnu` is the same platform
/// with the same zip artifact and the same mapped-image problem, and a triple
/// reaches this module from outside the running binary — [`fetch_archive`] takes
/// a *peer host's*. The guard used to spell itself `-windows-msvc`, so a gnu
/// target fell through into the tar path and asked the release for an artifact
/// nothing builds (issue #1172); latent while the only release is msvc, and
/// wrong the moment it is not.
fn is_windows_target(target: &str) -> bool {
    target.contains("-windows-")
}

/// The release archive for `target`: a `.tar.gz` everywhere but Windows, whose
/// artifact is the `.zip` `install.ps1` extracts.
pub fn archive_name(version: &str, target: &str) -> String {
    if is_windows_target(target) {
        zip_name(version, target)
    } else {
        tarball_name(version, target)
    }
}

/// A verified release archive on local disk, ready to be shipped somewhere.
#[derive(Debug)]
pub struct FetchedArchive {
    /// The archive file, inside a scratch directory that is removed when this
    /// value drops.
    pub path: PathBuf,
    /// The artifact's file name (`thurbox-v1.2.3-<target>.tar.gz` / `.zip`).
    pub name: String,
    _scratch: ScratchDir,
}

/// Download the release archive of `version` for `target` and verify it
/// against the release checksums, without extracting or installing anything.
///
/// This is [`perform_update`]'s download half, split out so a peer host can be
/// provisioned with a **foreign** target — the host's platform rather than the
/// running one — and with an exact version rather than "latest": a peer must
/// speak this binary's JSON, so it is given this binary's release. A dev build
/// has no release to fetch and is refused here; `host_cli` decides what a dev
/// build ships instead.
pub fn fetch_archive(version: &str, target: &str) -> Result<FetchedArchive, String> {
    if crate::agent::fetch::is_dev_version(version) {
        return Err(format!(
            "no release archive exists for the development build {version}"
        ));
    }
    let scratch = ScratchDir::new()?;
    let checksums_path = scratch.path.join(checksums_name(version));
    crate::agent::fetch::http_get_to_file(&checksums_url(RELEASE_BASE, version), &checksums_path)?;
    let name = archive_name(version, target);
    let path = scratch.path.join(&name);
    crate::agent::fetch::http_get_to_file(&archive_url(RELEASE_BASE, version, target), &path)?;
    let checksums =
        std::fs::read_to_string(&checksums_path).map_err(|e| format!("read checksums: {e}"))?;
    let expected = parse_checksum(&checksums, &name)
        .ok_or_else(|| format!("no checksum for {name} in release checksums"))?;
    verify_sha256(&path, &expected)?;
    Ok(FetchedArchive {
        path,
        name,
        _scratch: scratch,
    })
}

/// The target triple for the running binary's platform.
pub fn current_target() -> Result<&'static str, String> {
    target_triple(std::env::consts::OS, std::env::consts::ARCH)
}

/// Release tarball filename for `version` (no leading `v`) + `target`.
fn tarball_name(version: &str, target: &str) -> String {
    format!("thurbox-v{version}-{target}.tar.gz")
}

/// Release zip filename for `version` (no leading `v`) + `target` — the Windows
/// artifact `cd.yml` builds with `Compress-Archive`.
fn zip_name(version: &str, target: &str) -> String {
    format!("thurbox-v{version}-{target}.zip")
}

/// Release sums filename for `version` (no leading `v`), as
/// `scripts/ci/release-archive.sh --sums` writes it.
fn checksums_name(version: &str) -> String {
    format!("thurbox-{version}-sha256sums.txt")
}

/// The download URL of whichever archive [`archive_name`] picks for `target`.
fn archive_url(base: &str, version: &str, target: &str) -> String {
    format!("{base}/v{version}/{}", archive_name(version, target))
}

fn checksums_url(base: &str, version: &str) -> String {
    format!("{base}/v{version}/{}", checksums_name(version))
}

/// Pull the expected SHA256 digest for `artifact` out of a checksums file.
/// Each line is `<hex>␠␠<filename>`; find the line naming our tarball and take
/// the first whitespace token. Pure, so it's unit-testable. Mirrors
/// `install.sh`'s `get_checksum`.
fn parse_checksum(checksums: &str, artifact: &str) -> Option<String> {
    checksums
        .lines()
        .find(|line| line.contains(artifact))
        .and_then(|line| line.split_whitespace().next())
        .map(str::to_string)
}

/// `file`'s SHA-256, lowercase hex.
///
/// Streamed through the hasher rather than read whole: a release archive is
/// tens of megabytes and nothing here needs its bytes, only its digest.
///
/// Fed by hand rather than by `io::copy`: since `sha2` 0.11 the hasher is no
/// longer an `io::Write` (that moved to the separate `digest-io` crate), and its
/// output is a `hybrid_array::Array`, which has no `LowerHex`.
fn sha256_of(file: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    use std::io::Read;

    let mut handle =
        std::fs::File::open(file).map_err(|e| format!("open {} to hash: {e}", file.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        match handle.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("read {} to hash: {e}", file.display())),
        }
    }
    Ok(hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        }))
}

/// Verify `file`'s SHA256 against `expected`, case-insensitively.
///
/// Hashed **in process**. This shelled out to `sha256sum`, falling back to
/// `shasum -a 256` — the tools `install.sh` uses — which made a checksum depend
/// on what the *local* machine has on `PATH`. Native Windows has neither, so
/// every verification there failed with `could not compute SHA256`, which
/// skipped the WSL host mirror, left the host's socket unlearned, and put the
/// teardown sweep in a 5-second re-probe loop for the life of the process
/// (issue #1182). Hashing here answers identically on every platform, and
/// leaves no tool output to parse — the escaped-line handling that #1168 needed
/// went with it, because coreutils' escaping was the tool's, not the digest's.
fn verify_sha256(file: &Path, expected: &str) -> Result<(), String> {
    let actual = sha256_of(file)?;
    if actual.eq_ignore_ascii_case(expected.trim()) {
        Ok(())
    } else {
        Err(format!(
            "checksum mismatch (expected {expected}, got {actual})"
        ))
    }
}

/// The program and arguments that unpack `archive` into `into`.
///
/// Chosen by the archive's **extension** rather than by the running platform,
/// because the two are not always the same question: [`fetch_archive`] fetches a
/// peer host's artifact, and a zip is a zip wherever it was downloaded.
///
/// A `.zip` goes to PowerShell's built-in `Expand-Archive`, which is what
/// `install.ps1` uses and the only unpacker a native Windows box is guaranteed
/// to have — `tar.exe` only arrived in Windows 10 1803, and `unzip` never did.
/// `-LiteralPath` where `install.ps1` writes `-Path` because these are paths and
/// not wildcards: a `[` in an install directory would otherwise be read as a
/// character class. Progress is silenced because the child shares thurbox's
/// console, and the startup update runs while the interface owns the screen:
/// `Expand-Archive` would draw its progress bar over the frame, where `tar`
/// prints nothing on success. Anything else is the release tarball and goes to
/// `tar`, exactly as before.
fn extractor_for(archive: &Path, into: &Path) -> (&'static str, Vec<std::ffi::OsString>) {
    if archive
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
    {
        let script = format!(
            "$ProgressPreference='SilentlyContinue'; \
             Expand-Archive -LiteralPath {} -DestinationPath {} -Force",
            crate::shell::powershell_quote(&archive.to_string_lossy()),
            crate::shell::powershell_quote(&into.to_string_lossy()),
        );
        (
            "powershell.exe",
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                script.into(),
            ],
        )
    } else {
        (
            "tar",
            vec![
                "-xzf".into(),
                archive.as_os_str().to_os_string(),
                "-C".into(),
                into.as_os_str().to_os_string(),
            ],
        )
    }
}

/// Download staging removed on drop; a later allocation recovers old staging
/// whose owner died without running destructors.
#[derive(Debug)]
struct ScratchDir {
    path: PathBuf,
}

const SCRATCH_RECOVERY_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

impl ScratchDir {
    fn new() -> Result<Self, String> {
        let data_base = crate::paths::log_directory();
        let base = data_base.clone().unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&base)
            .map_err(|e| format!("create update scratch base {}: {e}", base.display()))?;
        // The system temp fallback is shared with other programs.
        if data_base.is_some() {
            recover_scratch(&base);
        }
        loop {
            // A random token avoids adopting an old process's sequence after
            // pid reuse, including while another recovery pass is in flight.
            let path = base.join(format!(
                ".update-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4().as_u128()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("create temp dir {}: {e}", path.display())),
            }
        }
    }
}

/// Canonical decimal ownership only, including older releases' u32 sequences.
fn scratch_owner(name: &std::ffi::OsStr) -> Option<u32> {
    let name = name.to_str()?;
    let (pid, token) = name.strip_prefix(".update-")?.split_once('-')?;
    let pid = pid.parse::<u32>().ok().filter(|pid| *pid != 0)?;
    let token = token.parse::<u128>().ok()?;
    (name == format!(".update-{pid}-{token}")).then_some(pid)
}

fn ordinary_owned_directory(path: &Path) -> Option<std::fs::Metadata> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_dir() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() } {
            return None;
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return None;
        }
    }
    Some(meta)
}

fn recover_scratch(base: &Path) {
    if ordinary_owned_directory(base).is_none() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let Some(pid) = scratch_owner(&entry.file_name()) else {
            continue;
        };
        let path = entry.path();
        let Some(meta) = ordinary_owned_directory(&path) else {
            continue;
        };
        let Some(age) = meta
            .modified()
            .ok()
            .and_then(|mtime| now.duration_since(mtime).ok())
        else {
            continue;
        };
        if age < SCRATCH_RECOVERY_AGE || !scratch_owner_exited(pid) {
            continue;
        }
        // remove_dir_all unlinks nested links without following their targets.
        if let Err(error) = std::fs::remove_dir_all(&path) {
            tracing::debug!(path = %path.display(), %error, "could not recover update scratch");
        }
    }
}

// An unsuccessful probe is not proof of death: permission errors and unknown
// states must preserve a concurrent updater, however old its staging is.
#[cfg(unix)]
fn scratch_owner_exited(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    (unsafe { libc::kill(pid, 0) }) == -1
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(windows)]
fn scratch_owner_exited(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return std::io::Error::last_os_error().raw_os_error()
            == Some(ERROR_INVALID_PARAMETER as i32);
    }
    let state = unsafe { WaitForSingleObject(handle, 0) };
    unsafe { CloseHandle(handle) };
    state == WAIT_OBJECT_0
}

#[cfg(not(any(unix, windows)))]
fn scratch_owner_exited(_pid: u32) -> bool {
    false
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Copy `src` into `dest`'s directory as a hidden `.{name}.new` staging file
/// (same filesystem as `dest`, so the later rename is atomic), made executable.
/// Returns the staged path, ready to commit.
fn stage_binary(src: &Path, dest: &Path) -> Result<PathBuf, String> {
    let dir = dest
        .parent()
        .ok_or_else(|| format!("no parent dir for {}", dest.display()))?;
    let name = dest
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| format!("bad destination name {}", dest.display()))?;
    let staged = dir.join(format!(".{name}.new"));
    std::fs::copy(src, &staged)
        .map_err(|e| format!("copy {} -> {}: {e}", src.display(), staged.display()))?;
    set_executable(&staged)?;
    Ok(staged)
}

/// `rwxr-xr-x`: only the owner may write, but everyone keeps read + execute,
/// because a system-wide install (e.g. `/usr/local/bin`, updated as root) is
/// run by other accounts. `chmod` is not masked by the umask, so this is the
/// exact mode the binary ends up with.
#[cfg(unix)]
const INSTALLED_BINARY_MODE: u32 = 0o755;

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;
    let perms = Permissions::from_mode(INSTALLED_BINARY_MODE); // NOSONAR: world r-x is required
    std::fs::set_permissions(path, perms).map_err(|e| format!("chmod {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Where to send someone whose install directory refused the replace. On
/// Windows a refusal is as likely to be another thurbox holding the file as a
/// permission problem, and only the reader can tell which, so the hint names
/// both.
#[cfg(windows)]
const REINSTALL_HINT: &str =
    "the binary may be in use — close thurbox and try again, or re-run install.ps1";
#[cfg(not(windows))]
const REINSTALL_HINT: &str = "reinstall with scripts/install.sh";

fn replace_failed(dest: &Path, e: impl std::fmt::Display) -> String {
    format!(
        "replace {} failed: {e}. Update may be partial — {REINSTALL_HINT}",
        dest.display()
    )
}

/// Rename `staged` over `dest`.
///
/// Unix replaces a running binary happily — the running process keeps the inode
/// it already opened — so this is the plain rename it has always been, and it
/// keeps no backup to roll back to.
#[cfg(not(windows))]
fn commit_binary(staged: &Path, dest: &Path) -> Result<Option<PathBuf>, String> {
    std::fs::rename(staged, dest).map_err(|e| replace_failed(dest, e))?;
    Ok(None)
}

/// Move `dest` aside to a free backup name, then move `staged` in. Returns the
/// backup, so [`install_binaries`] can put it back if a later binary fails.
///
/// **Why two renames.** Windows will not delete an image a process has mapped,
/// and a rename *over* `dest` has to delete it. Renaming the mapped image
/// itself is allowed, so it is moved aside first and the running process keeps
/// its old image, as it keeps its inode on Unix — the same swap `install.ps1`
/// and the host-CLI install use.
///
/// **Why a free name.** A process started before the last update is still
/// running from that update's backup, and nothing can delete it until it
/// exits. With one fixed backup name the swap then had nowhere to put `dest`,
/// and the update stopped part-way (issue #1432); [`backup_path`] picks a name
/// nothing holds instead.
///
/// **What an interrupted swap leaves.** Download, verification and staging all
/// happen before this, so an interruption up to here leaves the installed
/// binary untouched. Between the two renames `dest` is briefly absent; a failed
/// second rename moves the backup straight back.
#[cfg(windows)]
fn commit_binary(staged: &Path, dest: &Path) -> Result<Option<PathBuf>, String> {
    // A program the install lacked has nothing to move aside.
    if !dest.exists() {
        std::fs::rename(staged, dest).map_err(|e| replace_failed(dest, e))?;
        return Ok(None);
    }
    let backup = backup_path(dest)?;
    std::fs::rename(dest, &backup).map_err(|e| replace_failed(dest, e))?;
    if let Err(e) = std::fs::rename(staged, dest) {
        let _ = std::fs::rename(&backup, dest);
        return Err(replace_failed(dest, e));
    }
    Ok(Some(backup))
}

/// How many backups of one binary may be held by processes still running from
/// them before an update gives up.
#[cfg(windows)]
const BACKUP_SLOTS: usize = 16;

/// The first free name of `.{name}.old`, `.{name}.1.old`, … beside `dest`,
/// deleting every backup no process still runs from on the way, so old ones do
/// not pile up.
#[cfg(windows)]
fn backup_path(dest: &Path) -> Result<PathBuf, String> {
    let name = dest.file_name().unwrap_or_default().to_string_lossy();
    let mut free = None;
    for slot in 0..BACKUP_SLOTS {
        let candidate = if slot == 0 {
            dest.with_file_name(format!(".{name}.old"))
        } else {
            dest.with_file_name(format!(".{name}.{slot}.old"))
        };
        let _ = std::fs::remove_file(&candidate);
        if free.is_none() && !candidate.exists() {
            free = Some(candidate);
        }
    }
    free.ok_or_else(|| replace_failed(dest, "every backup name is held by a running thurbox"))
}

/// Replace the installed binaries in `install_dir` with the ones extracted into
/// `extract_dir`. Two-phase to minimise the version-mismatch window: every
/// binary is staged + verified first, then the renames run back-to-back.
///
/// `exe_suffix` is the platform's executable suffix appended to each
/// [`BINARIES`] stem — `std::env::consts::EXE_SUFFIX`, passed in rather than
/// read here so the Windows shape is reachable from a test on any platform.
/// Windows spells the binaries `thurbox.exe` in both the zip and the install
/// directory, and a suffix-blind version looked for a `thurbox` that is in
/// neither, reporting the archive as missing it (issue #1172).
///
/// A program the install directory lacks is added, not skipped: the archive is
/// the same verified one, and thurbox cannot start a session without `fleet`
/// and the programs it runs. Returns the names installed. Unit-testable: takes
/// plain dirs, no network.
fn install_binaries(
    extract_dir: &Path,
    install_dir: &Path,
    exe_suffix: &str,
) -> Result<Vec<String>, String> {
    // Phase 1: stage every binary the archive ships.
    let mut staged: Vec<(PathBuf, PathBuf, String)> = Vec::new(); // (staged, dest, name)
    for stem in BINARIES {
        let name = format!("{stem}{exe_suffix}");
        let src = extract_dir.join(&name);
        let dest = install_dir.join(&name);
        if !src.exists() {
            return Err(format!("release archive is missing `{name}`"));
        }
        if std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0) == 0 {
            return Err(format!("extracted `{name}` is empty"));
        }
        let s = match stage_binary(&src, &dest) {
            Ok(s) => s,
            Err(e) => {
                discard_staged(&staged);
                return Err(e);
            }
        };
        staged.push((s, dest, name));
    }
    // Phase 2: commit staged files back-to-back (`commit_binary` says how). A
    // failure puts back every binary already swapped and removes every one it
    // added, so the install is never left on two versions at once.
    let mut committed: Vec<(&Path, Option<PathBuf>, bool)> = Vec::new();
    for (i, (s, dest, _)) in staged.iter().enumerate() {
        let added = !dest.exists();
        match commit_binary(s, dest) {
            Ok(backup) => committed.push((dest, backup, added)),
            Err(e) => {
                for (dest, backup, added) in committed.into_iter().rev() {
                    if let Some(backup) = backup {
                        let _ = std::fs::rename(backup, dest);
                    } else if added {
                        let _ = std::fs::remove_file(dest);
                    }
                }
                discard_staged(&staged[i..]);
                return Err(e);
            }
        }
    }
    Ok(staged.into_iter().map(|(_, _, name)| name).collect())
}

/// Remove staged `.new` files an update will not commit, so the next one does
/// not find them.
fn discard_staged(staged: &[(PathBuf, PathBuf, String)]) {
    for (s, _, _) in staged {
        let _ = std::fs::remove_file(s);
    }
}

/// Download, verify, extract, and install the newest release of this binary's
/// major line in place.
///
/// `force` bypasses the up-to-date and dev-build guards (re-downloads + replaces
/// regardless) but never the line: no flag installs another major. Staging may
/// be recovered before verification; any failure before the swap leaves
/// installed binaries untouched.
pub fn perform_update(force: bool) -> Result<UpdateOutcome, String> {
    let current = current_version().to_string();

    // Dev builds never auto-update (their version doesn't order against tags).
    if !force && crate::agent::fetch::is_dev_build() {
        return Ok(UpdateOutcome::SkippedDevBuild { current });
    }

    perform_update_from(&ReleaseFeed::github(), &current, &install_dir()?, force)
}

/// The directory an update installs into: the one the programs are run
/// from, `<prefix>/bin` for a release under `<prefix>/lib/thurbox/releases/`,
/// otherwise the running executable's.
fn install_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("resolve current executable: {e}"))?;
    bin_dir_of(&exe).ok_or_else(|| "running executable has no parent directory".to_string())
}

fn bin_dir_of(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let release_bin = || {
        let releases = dir.parent()?.parent()?;
        let home = releases.parent()?;
        let lib = home.parent()?;
        (dir.file_name()? == "bin"
            && releases.file_name()? == RELEASES
            && home.file_name()? == "thurbox"
            && lib.file_name()? == "lib")
            .then(|| lib.parent().map(|prefix| prefix.join("bin")))
            .flatten()
    };
    Some(release_bin().unwrap_or_else(|| dir.to_path_buf()))
}

const RELEASES: &str = "releases";

/// `<prefix>/lib/thurbox` for the programs linked from `<prefix>/bin`: it
/// holds `releases/<version>/bin/` and the `current` and `previous` links.
#[cfg(unix)]
fn release_home(bin: &Path) -> PathBuf {
    bin.parent().unwrap_or(bin).join("lib/thurbox")
}

/// Install release `to` beside the running one and flip `current` to it.
/// An install whose programs are still files in `bin` first moves them into
/// a release `from` of their own. `bin` then links each program to
/// `current/bin`.
#[cfg(unix)]
fn install_release(extract: &Path, bin: &Path, from: &str, to: &str) -> Result<(), String> {
    let home = release_home(bin);
    adopt_unreleased(bin, &home, from)?;
    let release = home.join(RELEASES).join(to);
    let release_bin = release.join("bin");
    std::fs::create_dir_all(&release_bin)
        .map_err(|e| format!("create {}: {e}", release_bin.display()))?;
    install_binaries(extract, &release_bin, "")?;
    let link = Path::new(RELEASES).join(to);
    if let Some(current) = std::fs::read_link(home.join("current"))
        .ok()
        .filter(|c| *c != link)
    {
        relink(&home.join("previous"), &current)?;
    }
    relink(&home.join("current"), &link)?;
    link_programs(bin, &home)
}

#[cfg(unix)]
fn adopt_unreleased(bin: &Path, home: &Path, from: &str) -> Result<(), String> {
    if std::fs::symlink_metadata(home.join("current")).is_ok() {
        return Ok(());
    }
    let files: Vec<&str> = BINARIES
        .into_iter()
        .filter(|name| std::fs::symlink_metadata(bin.join(name)).is_ok_and(|m| m.is_file()))
        .collect();
    if files.is_empty() {
        return Ok(());
    }
    let release_bin = home.join(RELEASES).join(from).join("bin");
    std::fs::create_dir_all(&release_bin)
        .map_err(|e| format!("create {}: {e}", release_bin.display()))?;
    for name in files {
        let (src, dest) = (bin.join(name), release_bin.join(name));
        std::fs::copy(&src, &dest)
            .map_err(|e| format!("copy {} -> {}: {e}", src.display(), dest.display()))?;
    }
    relink(&home.join("current"), &Path::new(RELEASES).join(from))
}

/// Point `link` at `target` in one rename.
#[cfg(unix)]
fn relink(link: &Path, target: &Path) -> Result<(), String> {
    let name = link.file_name().unwrap_or_default().to_string_lossy();
    let staged = link.with_file_name(format!(".{name}.{}.link", std::process::id()));
    let _ = std::fs::remove_file(&staged);
    std::os::unix::fs::symlink(target, &staged)
        .and_then(|()| std::fs::rename(&staged, link))
        .map_err(|e| {
            let _ = std::fs::remove_file(&staged);
            format!("link {} -> {}: {e}", link.display(), target.display())
        })
}

#[cfg(unix)]
fn link_programs(bin: &Path, home: &Path) -> Result<(), String> {
    for name in BINARIES {
        let target = home.join("current/bin").join(name);
        let path = bin.join(name);
        if std::fs::read_link(&path).ok().as_deref() != Some(target.as_path()) {
            relink(&path, &target).map_err(|e| replace_failed(&path, e))?;
        }
    }
    Ok(())
}

/// `fleet service restart --expect <to>` with the new release's `fleet`.
/// Exit 3 is "no unit runs this store": nothing to restart. Any other
/// failure flips `current` back, restarts the manager on the old release
/// and says why.
#[cfg(target_os = "linux")]
fn restart_manager(bin: &Path, to: &str) -> Result<(), String> {
    const NO_UNIT: i32 = 3;
    let why = match fleet_service(bin, &["restart", "--expect", to]) {
        Ok(()) => return Ok(()),
        Err((Some(NO_UNIT), _)) => return Ok(()),
        Err((_, why)) => why,
    };
    let home = release_home(bin);
    let Some(previous) = std::fs::read_link(home.join("previous")).ok() else {
        return Err(format!(
            "the manager did not come back on {to}: {why}; no previous release to roll back to"
        ));
    };
    relink(&home.join("current"), &previous)?;
    relink(&home.join("previous"), &Path::new(RELEASES).join(to))?;
    let old = previous.file_name().unwrap_or_default().to_string_lossy();
    let restarted = match fleet_service(bin, &["restart"]) {
        Ok(()) => String::new(),
        Err((_, again)) => format!("; restarting it on {old} failed too: {again}"),
    };
    Err(format!(
        "the manager did not come back on {to}: {why}; rolled back to {old}{restarted}"
    ))
}

#[cfg(target_os = "linux")]
fn fleet_service(bin: &Path, args: &[&str]) -> Result<(), (Option<i32>, String)> {
    let fleet = bin.join("fleet");
    let output = Command::new(&fleet)
        .arg("service")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| (None, format!("run {}: {e}", fleet.display())))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let why = match stderr.trim() {
        "" => format!("fleet service {} exited {}", args.join(" "), output.status),
        text => text.to_string(),
    };
    Err((output.status.code(), why))
}

/// Remove every release but `current` and `previous` that no running
/// process's executable lives in.
#[cfg(unix)]
fn collect_releases(home: &Path) {
    let keep: Vec<PathBuf> = ["current", "previous"]
        .iter()
        .filter_map(|l| std::fs::read_link(home.join(l)).ok())
        .filter_map(|t| t.file_name().map(PathBuf::from))
        .collect();
    let Ok(entries) = std::fs::read_dir(home.join(RELEASES)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if keep.iter().any(|k| k.as_os_str() == entry.file_name()) || runs_from(&path) {
            continue;
        }
        if let Err(e) = std::fs::remove_dir_all(&path) {
            tracing::warn!("could not remove release {}: {e}", path.display());
        }
    }
}

/// Whether a running process's executable lives under `dir`. Off Linux
/// there is no `/proc` to tell, and a release is kept.
#[cfg(unix)]
fn runs_from(dir: &Path) -> bool {
    if !cfg!(target_os = "linux") {
        return true;
    }
    let Ok(dir) = std::fs::canonicalize(dir) else {
        return false;
    };
    let Ok(processes) = std::fs::read_dir("/proc") else {
        return true;
    };
    processes
        .flatten()
        .filter_map(|p| std::fs::read_link(p.path().join("exe")).ok())
        .any(|exe| exe.starts_with(&dir))
}

/// The `fleet` an update installs, beside the running executable.
pub fn installed_fleet() -> Result<PathBuf, String> {
    Ok(install_dir()?.join(format!("fleet{}", std::env::consts::EXE_SUFFIX)))
}

/// Where releases are listed and downloaded from: GitHub in production, a
/// directory of fixture files under `file://` in the updater's own tests, which
/// is how they drive the whole list-download-verify-install path offline.
pub(crate) struct ReleaseFeed {
    releases: String,
    download: String,
}

impl ReleaseFeed {
    fn github() -> Self {
        Self {
            releases: RELEASES_URL.to_string(),
            download: RELEASE_BASE.to_string(),
        }
    }
}

/// [`perform_update`] against an explicit feed, running version and install
/// directory.
fn perform_update_from(
    feed: &ReleaseFeed,
    current: &str,
    install_dir: &Path,
    force: bool,
) -> Result<UpdateOutcome, String> {
    let current = current.to_string();
    let releases = fetch_releases(&feed.releases)?;
    let latest = releases.line.unwrap_or_else(|| current.clone());
    if !force && decide_update(&current, &latest).is_none() {
        return Ok(UpdateOutcome::UpToDate {
            current,
            latest,
            next_major: releases.next_major,
        });
    }

    let target = current_target()?;
    let scratch = ScratchDir::new()?;

    // Download checksums + this platform's archive (a tarball, or Windows' zip).
    let checksums_path = scratch.path.join(checksums_name(&latest));
    crate::agent::fetch::http_get_to_file(
        &checksums_url(&feed.download, &latest),
        &checksums_path,
    )?;
    let archive = archive_name(&latest, target);
    let archive_path = scratch.path.join(&archive);
    crate::agent::fetch::http_get_to_file(
        &archive_url(&feed.download, &latest, target),
        &archive_path,
    )?;

    // Verify the download BEFORE touching anything installed.
    let checksums =
        std::fs::read_to_string(&checksums_path).map_err(|e| format!("read checksums: {e}"))?;
    let expected = parse_checksum(&checksums, &archive)
        .ok_or_else(|| format!("no checksum for {archive} in release checksums"))?;
    verify_sha256(&archive_path, &expected)?;

    // Extract and install.
    let extract_dir = scratch.path.join("extract");
    std::fs::create_dir_all(&extract_dir).map_err(|e| format!("create extract dir: {e}"))?;
    let (program, args) = extractor_for(&archive_path, &extract_dir);
    let status = Command::new(program)
        .args(&args)
        .status()
        .map_err(|e| format!("run {program}: {e}"))?;
    if !status.success() {
        return Err(format!(
            "{program} could not unpack {archive} (exit {status})"
        ));
    }

    #[cfg(unix)]
    {
        install_release(&extract_dir, install_dir, &current, &latest)?;
        #[cfg(target_os = "linux")]
        let restarted = restart_manager(install_dir, &latest);
        #[cfg(not(target_os = "linux"))]
        let restarted: Result<(), String> = Ok(());
        collect_releases(&release_home(install_dir));
        restarted?;
    }
    #[cfg(not(unix))]
    install_binaries(&extract_dir, install_dir, std::env::consts::EXE_SUFFIX)?;

    Ok(UpdateOutcome::Updated {
        from: current,
        to: latest,
        next_major: releases.next_major,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRATCH_CHILD: &str = "update::self_update::tests::scratch_lifecycle_child";

    #[test]
    fn scratch_lifecycle_child() {
        let Some(root) = std::env::var_os("THURBOX_UPDATE_TEST_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let _paths = crate::paths::TestPathGuard::new(&root);
        let scratch = ScratchDir::new().unwrap();
        std::fs::write(scratch.path.join("partial.zip"), b"partial download").unwrap();
        std::fs::write(root.join("ready.pending"), scratch.path.to_str().unwrap()).unwrap();
        std::fs::rename(root.join("ready.pending"), root.join("ready")).unwrap();
        if std::env::var_os("THURBOX_UPDATE_TEST_HOLD").is_some() {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            while !root.join("release").exists() {
                assert!(std::time::Instant::now() < deadline, "child bounded wait");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        drop(scratch);
    }

    struct ScratchChild(std::process::Child);

    impl Drop for ScratchChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn start_scratch_child(root: &Path, hold: bool) -> (ScratchChild, PathBuf) {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", SCRATCH_CHILD, "--nocapture"])
            .env("THURBOX_UPDATE_TEST_ROOT", root)
            .env_remove("THURBOX_UPDATE_TEST_HOLD");
        if hold {
            cmd.env("THURBOX_UPDATE_TEST_HOLD", "1");
        }
        let mut child = ScratchChild(cmd.spawn().unwrap());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Ok(path) = std::fs::read_to_string(root.join("ready")) {
                return (child, PathBuf::from(path));
            }
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "child exited before ready"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "child never became ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn set_scratch_modified(path: &Path, modified: std::time::SystemTime) {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_FLAG_BACKUP_SEMANTICS, FILE_WRITE_ATTRIBUTES,
            };
            options
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .access_mode(FILE_WRITE_ATTRIBUTES);
        }
        options
            .open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
    }

    fn age_scratch(path: &Path) {
        set_scratch_modified(
            path,
            std::time::SystemTime::now() - std::time::Duration::from_secs(7200),
        );
    }

    #[test]
    fn killed_update_scratch_is_recovered_by_the_next_update() {
        let root = tempfile::tempdir().unwrap();
        let _paths = crate::paths::TestPathGuard::new(root.path());
        let (mut child, abandoned) = start_scratch_child(root.path(), true);
        child.0.kill().unwrap();
        assert!(!child.0.wait().unwrap().success());
        assert!(
            abandoned.join("partial.zip").exists(),
            "kill must bypass Drop"
        );
        age_scratch(&abandoned);
        let next = ScratchDir::new().unwrap();
        assert!(
            !abandoned.exists(),
            "next update must recover abandoned scratch"
        );
        assert!(next.path.is_dir());
    }

    #[test]
    fn normally_exiting_update_removes_its_scratch() {
        let root = tempfile::tempdir().unwrap();
        let (mut child, scratch) = start_scratch_child(root.path(), false);
        assert!(child.0.wait().unwrap().success());
        assert!(!scratch.exists());
    }

    #[test]
    fn recovery_spares_live_and_recent_updates() {
        let root = tempfile::tempdir().unwrap();
        let _paths = crate::paths::TestPathGuard::new(root.path());
        let (mut child, live) = start_scratch_child(root.path(), true);
        age_scratch(&live);
        let next = ScratchDir::new().unwrap();
        assert!(
            live.join("partial.zip").exists(),
            "old live work must survive"
        );
        drop(next);
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        set_scratch_modified(&live, std::time::SystemTime::now());
        let next = ScratchDir::new().unwrap();
        assert!(live.exists(), "recent dead work must survive");
        age_scratch(&live);
        drop(next);
        let _next = ScratchDir::new().unwrap();
        assert!(!live.exists());
    }

    #[test]
    fn scratch_creation_does_not_reuse_an_existing_directory() {
        let root = tempfile::tempdir().unwrap();
        let _paths = crate::paths::TestPathGuard::new(root.path());
        // Reserve several sequences so this also holds under the shared-process runner.
        for seq in 0..100 {
            let path = root
                .path()
                .join(format!(".update-{}-{seq}", std::process::id()));
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("keep"), b"existing work").unwrap();
        }
        let scratch = ScratchDir::new().unwrap();
        assert!(
            !scratch.path.join("keep").exists(),
            "never adopt existing work"
        );
        drop(scratch);
        for seq in 0..100 {
            assert!(root
                .path()
                .join(format!(".update-{}-{seq}/keep", std::process::id()))
                .exists());
        }
    }

    #[test]
    fn recovery_spares_malformed_ownership_and_unrelated_paths() {
        let root = tempfile::tempdir().unwrap();
        let _paths = crate::paths::TestPathGuard::new(root.path());
        let (mut child, abandoned) = start_scratch_child(root.path(), true);
        let pid = child.0.id();
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        age_scratch(&abandoned);
        let legacy = root.path().join(format!(".update-{pid}-21"));
        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(legacy.join("partial.zip"), b"older release download").unwrap();
        age_scratch(&legacy);
        let names = [
            "unrelated".to_string(),
            ".update-0-1".to_string(),
            format!(".update-{pid}"),
            format!(".update-{pid}-"),
            format!(".update-{pid}-1-extra"),
            format!(".update-{pid}-01"),
            format!(".update-0{pid}-1"),
            format!(".update-{pid}-+1"),
            ".update-4294967296-1".to_string(),
        ];
        for name in &names {
            let path = root.path().join(name);
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("keep"), b"unrelated").unwrap();
            age_scratch(&path);
        }
        let file = root.path().join(format!(".update-{pid}-17"));
        std::fs::write(&file, b"not a directory").unwrap();
        let future = root.path().join(format!(".update-{pid}-18"));
        std::fs::create_dir(&future).unwrap();
        set_scratch_modified(
            &future,
            std::time::SystemTime::now() + std::time::Duration::from_secs(7200),
        );
        let _next = ScratchDir::new().unwrap();
        assert!(!abandoned.exists());
        assert!(
            !legacy.exists(),
            "legacy sequence names must still be recovered"
        );
        for name in names {
            assert!(root.path().join(name).join("keep").exists());
        }
        assert!(file.is_file());
        assert!(future.is_dir());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn recovery_does_not_follow_directory_links() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep"), b"outside scratch").unwrap();
        age_scratch(outside.path());
        let _paths = crate::paths::TestPathGuard::new(root.path());
        let (mut child, abandoned) = start_scratch_child(root.path(), true);
        let pid = child.0.id();
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        let link = root.path().join(format!(".update-{pid}-19"));
        let nested = abandoned.join("outside");
        for path in [&link, &nested] {
            #[cfg(unix)]
            std::os::unix::fs::symlink(outside.path(), path).unwrap();
            #[cfg(windows)]
            assert!(Command::new("cmd.exe")
                .args(["/C", "mklink", "/J"])
                .arg(path)
                .arg(outside.path())
                .status()
                .unwrap()
                .success());
        }
        age_scratch(&abandoned);
        let _next = ScratchDir::new().unwrap();
        assert!(!abandoned.exists());
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "linked candidate must survive"
        );
        assert_eq!(
            std::fs::read(outside.path().join("keep")).unwrap(),
            b"outside scratch"
        );
    }

    /// The triples an update or a peer provisioning may download — the
    /// artifacts `cd.yml` builds that `target_triple` actually selects (it
    /// prefers musl over the also-built gnu tarball for Linux x86_64; the
    /// Windows artifact is a `.zip`, served by `archive_name`). `target_triple`
    /// / `current_target` must only ever resolve to one of these — anything
    /// else would 404 on download.
    const SHIPPED_TRIPLES: [&str; 4] = [
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
    ];

    #[test]
    fn target_triple_maps_supported_platforms() {
        assert_eq!(
            target_triple("linux", "x86_64").unwrap(),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(
            target_triple("macos", "aarch64").unwrap(),
            "aarch64-apple-darwin"
        );
        // A host's platform arrives as its shell spells it.
        assert_eq!(
            target_triple("Darwin", "arm64").unwrap(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            target_triple("Linux", "x86_64").unwrap(),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(
            target_triple("linux", "aarch64").unwrap(),
            "aarch64-unknown-linux-musl"
        );
        assert_eq!(
            target_triple("Linux", "arm64").unwrap(),
            "aarch64-unknown-linux-musl"
        );
        assert_eq!(
            target_triple("windows", "AMD64").unwrap(),
            "x86_64-pc-windows-msvc"
        );
        assert_eq!(
            archive_name("1.2.3", "x86_64-pc-windows-msvc"),
            "thurbox-v1.2.3-x86_64-pc-windows-msvc.zip"
        );
        assert_eq!(
            archive_name("1.2.3", "x86_64-unknown-linux-musl"),
            "thurbox-v1.2.3-x86_64-unknown-linux-musl.tar.gz"
        );
    }

    /// The artifact shape is a property of Windows, not of one of its two
    /// ABIs. The guard used to spell itself `-windows-msvc`, so a
    /// `-windows-gnu` target fell through into the tar path and asked the
    /// release for an artifact `cd.yml` does not build (issue #1172). Reachable
    /// through [`fetch_archive`], which takes a *peer's* triple rather than
    /// this binary's.
    #[test]
    fn archive_name_names_windows_not_one_triple() {
        for target in [
            "x86_64-pc-windows-msvc",
            "x86_64-pc-windows-gnu",
            "aarch64-pc-windows-msvc",
        ] {
            assert_eq!(
                archive_name("1.2.3", target),
                format!("thurbox-v1.2.3-{target}.zip"),
                "{target} must ask for the zip"
            );
        }
    }

    #[test]
    fn target_triple_rejects_unshipped_platforms() {
        // Platforms `cd.yml` does NOT build an artifact for must error cleanly
        // rather than resolve to a non-existent one.
        assert!(target_triple("windows", "arm64").is_err());
        assert!(target_triple("linux", "riscv64").is_err());
        // Intel macOS is intentionally not shipped.
        assert!(target_triple("macos", "x86_64").is_err());
    }

    #[test]
    fn a_dev_build_has_no_archive_to_fetch() {
        let err = fetch_archive("0.0.0-dev", "x86_64-unknown-linux-musl").unwrap_err();
        assert!(err.contains("development build"), "{err}");
    }

    #[test]
    fn current_target_resolves_to_a_shipped_triple_or_errors() {
        // On a release-built host `current_target` must name a shipped triple;
        // on any other host it must error (never point at a missing download).
        match current_target() {
            Ok(triple) => assert!(
                SHIPPED_TRIPLES.contains(&triple),
                "current_target() -> {triple} is not a release-built triple"
            ),
            Err(e) => assert!(e.contains("unsupported platform"), "got: {e}"),
        }
    }

    #[test]
    fn artifact_names_and_urls_match_install_sh() {
        assert_eq!(
            tarball_name("0.114.0", "x86_64-unknown-linux-musl"),
            "thurbox-v0.114.0-x86_64-unknown-linux-musl.tar.gz"
        );
        // Never `thurbox-v<V>-checksums.txt`: every 2.x updater fetches that
        // name first, and a 3.x release leaves it out so they fail safe
        // (docs/thurbox/releasing.md, "What a 3.x release must do").
        assert_eq!(checksums_name("3.0.0"), "thurbox-3.0.0-sha256sums.txt");
        let url = archive_url(RELEASE_BASE, "0.114.0", "aarch64-apple-darwin");
        assert!(url.starts_with(RELEASE_BASE), "got: {url}");
        assert!(url.contains("/v0.114.0/"), "got: {url}");
        assert!(url.ends_with(".tar.gz"), "got: {url}");
        assert!(checksums_url(RELEASE_BASE, "0.114.0").contains("/v0.114.0/"));
        // The URL follows the artifact, so Windows asks for the zip it is sent.
        let url = archive_url(RELEASE_BASE, "0.114.0", "x86_64-pc-windows-msvc");
        assert!(
            url.ends_with("/v0.114.0/thurbox-v0.114.0-x86_64-pc-windows-msvc.zip"),
            "got: {url}"
        );
    }

    /// The zip is unpacked by the one thing a native Windows box is guaranteed
    /// to have. Decided by the archive's extension rather than the running
    /// platform because `fetch_archive` fetches a *peer's* artifact, which is
    /// also why this is asserted on Linux.
    #[test]
    fn extractor_for_picks_expand_archive_for_a_zip() {
        let into = Path::new("/install/dir");

        let (program, args) = extractor_for(Path::new("/tmp/thurbox-v1.2.3-win.zip"), into);
        assert_eq!(program, "powershell.exe");
        let script = args.last().unwrap().to_string_lossy().into_owned();
        assert!(script.contains("Expand-Archive"), "{script}");
        assert!(
            script.contains("-LiteralPath '/tmp/thurbox-v1.2.3-win.zip'"),
            "{script}"
        );
        assert!(
            script.contains("-DestinationPath '/install/dir'"),
            "{script}"
        );
        // An update overwrites what the last one extracted.
        assert!(script.contains("-Force"), "{script}");
        // No progress bar drawn over the interface that shares the console.
        assert!(
            script.starts_with("$ProgressPreference='SilentlyContinue'; Expand-Archive"),
            "{script}"
        );
        // No profile to source and nothing to prompt with: this runs headless.
        let flags: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(flags.contains(&"-NoProfile".to_string()), "{flags:?}");
        assert!(flags.contains(&"-NonInteractive".to_string()), "{flags:?}");

        // Everything else is the release tarball, and goes where it always did.
        let (program, args) = extractor_for(Path::new("/tmp/thurbox-v1.2.3-musl.tar.gz"), into);
        assert_eq!(program, "tar");
        let args: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec![
                "-xzf",
                "/tmp/thurbox-v1.2.3-musl.tar.gz",
                "-C",
                "/install/dir"
            ]
        );

        // The extension decides, not the case somebody wrote it in.
        assert_eq!(extractor_for(Path::new("a.ZIP"), into).0, "powershell.exe");
    }

    #[test]
    fn parse_checksum_picks_the_matching_line() {
        let body = "\
aaaa1111  thurbox-v0.114.0-x86_64-apple-darwin.tar.gz
bbbb2222  thurbox-v0.114.0-x86_64-unknown-linux-musl.tar.gz
cccc3333  thurbox-v0.114.0-aarch64-apple-darwin.tar.gz
";
        assert_eq!(
            parse_checksum(body, "thurbox-v0.114.0-x86_64-unknown-linux-musl.tar.gz").as_deref(),
            Some("bbbb2222")
        );
        assert_eq!(
            parse_checksum(body, "thurbox-v0.114.0-aarch64-apple-darwin.tar.gz").as_deref(),
            Some("cccc3333")
        );
    }

    #[test]
    fn parse_checksum_missing_entry_is_none() {
        let body = "aaaa1111  thurbox-v0.114.0-x86_64-apple-darwin.tar.gz\n";
        assert!(parse_checksum(body, "thurbox-v9.9.9-x86_64-unknown-linux-musl.tar.gz").is_none());
        assert!(parse_checksum("", "anything").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn install_binaries_replaces_contents_and_sets_mode() {
        use std::os::unix::fs::PermissionsExt;

        let install = tempfile::TempDir::new().unwrap();
        let extract = tempfile::TempDir::new().unwrap();

        // Old installed binaries (non-executable, old contents).
        for name in BINARIES {
            let p = install.path().join(name);
            std::fs::write(&p, b"OLD").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
            // New extracted binaries with fresh contents.
            std::fs::write(extract.path().join(name), format!("NEW-{name}")).unwrap();
        }

        let replaced = install_binaries(extract.path(), install.path(), "").unwrap();
        assert_eq!(replaced.len(), BINARIES.len());

        for name in BINARIES {
            let dest = install.path().join(name);
            assert_eq!(
                std::fs::read_to_string(&dest).unwrap(),
                format!("NEW-{name}")
            );
            let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755, "binary should be executable");
            // No staging files left behind.
            assert!(!install.path().join(format!(".{name}.new")).exists());
        }
    }

    #[cfg(unix)]
    /// An install that predates a program (only `thurbox` and `thurbox-cli`
    /// beside each other) gains every program the release ships: thurbox
    /// cannot start a session without `fleet` and the two it runs.
    #[test]
    fn install_binaries_adds_the_programs_the_install_lacks() {
        let install = tempfile::TempDir::new().unwrap();
        let extract = tempfile::TempDir::new().unwrap();
        for name in BINARIES {
            std::fs::write(extract.path().join(name), b"NEW").unwrap();
        }
        for name in ["thurbox", "thurbox-cli"] {
            std::fs::write(install.path().join(name), b"OLD").unwrap();
        }

        let replaced = install_binaries(extract.path(), install.path(), "").unwrap();
        assert_eq!(replaced.len(), BINARIES.len(), "{replaced:?}");
        for name in BINARIES {
            assert_eq!(
                std::fs::read(install.path().join(name)).unwrap(),
                b"NEW",
                "{name}"
            );
        }
    }

    #[test]
    fn install_binaries_errors_when_tarball_missing_a_binary() {
        let install = tempfile::TempDir::new().unwrap();
        let extract = tempfile::TempDir::new().unwrap();
        std::fs::write(install.path().join("thurbox"), b"OLD").unwrap();
        // extract dir has neither binary
        let err = install_binaries(extract.path(), install.path(), "").unwrap_err();
        assert!(err.contains("missing"), "got: {err}");
    }

    /// Windows spells both the archive entries and the installed binaries
    /// `thurbox.exe`, so a suffix-blind install looked for a `thurbox` that is in
    /// neither and called the archive incomplete (issue #1172). Driven by the
    /// suffix rather than `cfg(windows)`, so the Windows shape is covered on the
    /// platform CI actually runs.
    #[test]
    fn install_binaries_appends_the_platform_exe_suffix() {
        let install = tempfile::TempDir::new().unwrap();
        let extract = tempfile::TempDir::new().unwrap();
        for stem in BINARIES {
            std::fs::write(extract.path().join(format!("{stem}.exe")), b"NEW").unwrap();
            std::fs::write(install.path().join(format!("{stem}.exe")), b"OLD").unwrap();
        }

        let replaced = install_binaries(extract.path(), install.path(), ".exe").unwrap();
        assert_eq!(replaced.len(), BINARIES.len());
        for stem in BINARIES {
            let name = format!("{stem}.exe");
            assert_eq!(
                std::fs::read_to_string(install.path().join(&name)).unwrap(),
                "NEW"
            );
            // No staging files left behind.
            assert!(!install.path().join(format!(".{name}.new")).exists());
            // On Windows the commit moves what it replaced aside rather than
            // deleting it; this is where that path runs for real.
            #[cfg(windows)]
            assert_eq!(
                std::fs::read_to_string(install.path().join(format!(".{name}.old"))).unwrap(),
                "OLD"
            );
        }
    }

    /// A process killed on drop, so a failed assertion leaves nothing running
    /// from the test's install directory.
    #[cfg(windows)]
    struct Running(std::process::Child);

    #[cfg(windows)]
    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Start `exe` and keep it running, so Windows holds its image mapped the
    /// way it does for a `thurbox-cli.exe` a hook started.
    #[cfg(windows)]
    fn run_from(exe: &Path) -> Running {
        let child = Command::new(exe)
            .args(["-n", "60", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // Give the loader time to map the image.
        std::thread::sleep(std::time::Duration::from_millis(500));
        Running(child)
    }

    /// Issue #1432: the first update moves a running `thurbox-cli.exe` aside
    /// to `.thurbox-cli.exe.old`, and the process keeps running from there.
    /// The next update could not delete that backup, `ReplaceFile` refused to
    /// reuse its name, and the update stopped after `thurbox.exe` — a TUI and
    /// CLI on different versions and a stale `.new` behind. A real process
    /// holds the image, because only the loader's mapping reproduces it.
    #[cfg(windows)]
    #[test]
    fn update_succeeds_while_a_process_runs_from_the_last_backup() {
        let install = tempfile::TempDir::new().unwrap();
        let ping = Path::new(&std::env::var("SystemRoot").unwrap()).join(r"System32\PING.EXE");
        for stem in BINARIES {
            std::fs::copy(&ping, install.path().join(format!("{stem}.exe"))).unwrap();
        }
        let _cli = run_from(&install.path().join("thurbox-cli.exe"));

        for version in ["V2", "V3"] {
            let extract = tempfile::TempDir::new().unwrap();
            for stem in BINARIES {
                std::fs::write(extract.path().join(format!("{stem}.exe")), version).unwrap();
            }
            let replaced = install_binaries(extract.path(), install.path(), ".exe")
                .unwrap_or_else(|e| panic!("update to {version}: {e}"));
            assert_eq!(replaced.len(), BINARIES.len());
            for stem in BINARIES {
                let name = format!("{stem}.exe");
                assert_eq!(
                    std::fs::read_to_string(install.path().join(&name)).unwrap(),
                    version,
                    "{name} after the update to {version}"
                );
                assert!(!install.path().join(format!(".{name}.new")).exists());
            }
        }
    }

    /// When one binary cannot be committed the other is put back, so a failed
    /// update leaves the install on one version rather than split across two,
    /// and leaves no staged `.new` for the next update to trip over.
    #[cfg(windows)]
    #[test]
    fn a_failed_commit_rolls_back_the_binaries_already_swapped() {
        use std::os::windows::fs::OpenOptionsExt;

        let install = tempfile::TempDir::new().unwrap();
        let extract = tempfile::TempDir::new().unwrap();
        for stem in BINARIES {
            std::fs::write(install.path().join(format!("{stem}.exe")), "OLD").unwrap();
            std::fs::write(extract.path().join(format!("{stem}.exe")), "NEW").unwrap();
        }
        // Opened without FILE_SHARE_DELETE, so no rename can move it.
        let _lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1) // FILE_SHARE_READ
            .open(install.path().join("thurbox-cli.exe"))
            .unwrap();

        install_binaries(extract.path(), install.path(), ".exe").unwrap_err();
        for stem in BINARIES {
            let name = format!("{stem}.exe");
            assert_eq!(
                std::fs::read_to_string(install.path().join(&name)).unwrap(),
                "OLD",
                "{name} after the failed update"
            );
            assert!(!install.path().join(format!(".{name}.new")).exists());
        }
    }

    /// A failed update removes the programs it had added, so the install is
    /// not left with `fleet` from the new release beside the old thurbox.
    #[cfg(unix)]
    #[test]
    fn a_failed_commit_removes_the_programs_it_added() {
        let install = tempfile::TempDir::new().unwrap();
        let extract = tempfile::TempDir::new().unwrap();
        for name in BINARIES {
            std::fs::write(extract.path().join(name), b"NEW").unwrap();
        }
        for name in ["thurbox", "thurbox-cli"] {
            std::fs::write(install.path().join(name), b"OLD").unwrap();
        }
        // A non-empty directory where the last program goes: no rename lands.
        let blocked = install.path().join(BINARIES[BINARIES.len() - 1]);
        std::fs::create_dir_all(blocked.join("x")).unwrap();

        install_binaries(extract.path(), install.path(), "").unwrap_err();
        for name in &BINARIES[2..BINARIES.len() - 1] {
            assert!(
                !install.path().join(name).exists(),
                "{name} stayed after the failed update"
            );
        }
    }

    /// The Windows shape of the same: a later program held open, and the
    /// programs the install lacked are gone again after the failure.
    #[cfg(windows)]
    #[test]
    fn a_failed_commit_removes_the_programs_it_added_on_windows() {
        use std::os::windows::fs::OpenOptionsExt;

        let install = tempfile::TempDir::new().unwrap();
        let extract = tempfile::TempDir::new().unwrap();
        for stem in BINARIES {
            std::fs::write(extract.path().join(format!("{stem}.exe")), "NEW").unwrap();
        }
        let last = format!("{}.exe", BINARIES[BINARIES.len() - 1]);
        for name in ["thurbox.exe", "thurbox-cli.exe", last.as_str()] {
            std::fs::write(install.path().join(name), "OLD").unwrap();
        }
        let _lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1) // FILE_SHARE_READ
            .open(install.path().join(&last))
            .unwrap();

        install_binaries(extract.path(), install.path(), ".exe").unwrap_err();
        for stem in &BINARIES[2..BINARIES.len() - 1] {
            let name = format!("{stem}.exe");
            assert!(
                !install.path().join(&name).exists(),
                "{name} stayed after the failed update"
            );
        }
    }

    /// Hashing is in process, so an empty `PATH` — a machine with neither
    /// `sha256sum` nor `shasum`, which is every native Windows one — must still
    /// verify. Emptying `PATH` is how the platform that shipped issue #1182
    /// gets reproduced on the platform CI runs, and it is why this test carries
    /// no `cfg` gate: the version that did skipped the only platform it broke on.
    #[test]
    fn verify_sha256_needs_no_tool_on_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("empty");
        std::fs::write(&file, b"").unwrap();
        // SHA256 of empty input — the canonical value.
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        crate::paths::with_path("", || {
            verify_sha256(&file, empty).expect("hashing must not depend on PATH");
            // Compare is case-insensitive.
            assert!(verify_sha256(&file, &empty.to_uppercase()).is_ok());
            let err = verify_sha256(&file, &"0".repeat(64)).unwrap_err();
            assert!(err.contains("mismatch"), "got: {err}");
        });
    }

    /// A digest of real content, not just the empty one, so a wrong hasher
    /// cannot pass by returning a constant.
    #[test]
    fn verify_sha256_hashes_the_file_contents() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("abc");
        std::fs::write(&file, b"abc").unwrap();
        verify_sha256(
            &file,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        )
        .unwrap();
    }

    /// A file larger than the read buffer is hashed whole, not just its first
    /// chunk: the FIPS 180-2 one-million-`a` vector.
    #[test]
    fn verify_sha256_hashes_past_the_read_buffer() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("million-a");
        std::fs::write(&file, vec![b'a'; 1_000_000]).unwrap();
        verify_sha256(
            &file,
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
        )
        .unwrap();
    }

    /// The programs a release archive ships: `BINARIES` in
    /// `scripts/ci/release-archive.sh`, which packs every archive.
    fn shipped() -> Vec<String> {
        let packer = include_str!("../../../../scripts/ci/release-archive.sh");
        let list = packer
            .lines()
            .find_map(|l| l.strip_prefix("BINARIES=("))
            .and_then(|l| l.strip_suffix(')'))
            .expect("release-archive.sh declares BINARIES=(...)");
        list.split_whitespace().map(str::to_string).collect()
    }

    /// The updater replaces what the release ships, so a v3 install's `fleet`,
    /// `runtime-mux` and `workspace-git` move with thurbox.
    #[test]
    fn the_updater_replaces_every_program_the_release_ships() {
        assert_eq!(BINARIES.to_vec(), shipped());
    }

    /// An update on an install of every shipped program leaves none of them on
    /// the old version.
    #[cfg(unix)]
    #[test]
    fn an_update_moves_every_shipped_program_to_the_new_release() {
        let feed = Feed::new(&["3.1.0", "3.0.1"], "3.0.1");
        let outcome = feed.update("3.0.1", false).unwrap();
        assert!(
            matches!(outcome, UpdateOutcome::Updated { .. }),
            "{outcome:?}"
        );
        for name in shipped() {
            let installed = Feed::version_of(&feed.bin().join(&name));
            assert_eq!(installed, "3.1.0", "{name} stayed on {installed}");
        }
    }

    /// A release feed on disk, served to the real updater over `file://`.
    ///
    /// Each release gets the archive for this platform and a checksums file, the
    /// same assets `cd.yml` publishes. The binaries inside are scripts that name
    /// their version and record each call in `calls`; a file `exit-<version>`
    /// holding `<code> <message>` makes that release's programs fail with it.
    /// The install is a prefix whose `bin` holds the programs.
    #[cfg(unix)]
    struct Feed {
        root: tempfile::TempDir,
        prefix: tempfile::TempDir,
        _paths: crate::paths::TestPathGuard,
    }

    #[cfg(unix)]
    impl Feed {
        /// `releases` newest first, as GitHub lists them.
        fn new(releases: &[&str], installed: &str) -> Self {
            let root = tempfile::TempDir::new().unwrap();
            let prefix = tempfile::TempDir::new().unwrap();
            let paths = crate::paths::TestPathGuard::new(root.path().join("data"));
            let target = current_target().unwrap();
            for version in releases {
                let dir = root.path().join("download").join(format!("v{version}"));
                let stage = root.path().join("stage").join(version);
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::create_dir_all(&stage).unwrap();
                for name in shipped() {
                    Self::write_program(root.path(), &stage.join(&name), version);
                }
                let archive = archive_name(version, target);
                let ok = Command::new("tar")
                    .arg("czf")
                    .arg(dir.join(&archive))
                    .arg("-C")
                    .arg(&stage)
                    .args(shipped())
                    .status()
                    .unwrap()
                    .success();
                assert!(ok, "tar the fixture archive");
                let digest = sha256_of(&dir.join(&archive)).unwrap();
                std::fs::write(
                    dir.join(checksums_name(version)),
                    format!("{digest}  {archive}\n"),
                )
                .unwrap();
            }
            let list: Vec<_> = releases
                .iter()
                .map(|v| serde_json::json!({"tag_name": format!("v{v}"), "draft": false, "prerelease": false}))
                .collect();
            std::fs::write(
                root.path().join("list.json"),
                serde_json::to_string(&list).unwrap(),
            )
            .unwrap();
            std::fs::create_dir_all(prefix.path().join("bin")).unwrap();
            for name in shipped() {
                Self::write_program(
                    root.path(),
                    &prefix.path().join("bin").join(&name),
                    installed,
                );
            }
            Self {
                root,
                prefix,
                _paths: paths,
            }
        }

        fn write_program(root: &Path, path: &Path, version: &str) {
            use std::os::unix::fs::PermissionsExt;
            let root = root.display();
            std::fs::write(
                path,
                format!(
                    "#!/bin/sh\n\
                     echo '{version}' \"$*\" >> '{root}/calls'\n\
                     if [ -f '{root}/exit-{version}' ]; then read -r c m < '{root}/exit-{version}'; echo \"$m\" >&2; exit \"$c\"; fi\n"
                ),
            )
            .unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        fn version_of(path: &Path) -> String {
            let text = std::fs::read_to_string(path).unwrap();
            text.split('\'').nth(1).unwrap().to_string()
        }

        fn feed(&self) -> ReleaseFeed {
            let url = |p: &str| format!("file://{}", self.root.path().join(p).display());
            ReleaseFeed {
                releases: url("list.json"),
                download: url("download"),
            }
        }

        fn bin(&self) -> PathBuf {
            self.prefix.path().join("bin")
        }

        fn home(&self) -> PathBuf {
            self.prefix.path().join("lib/thurbox")
        }

        fn update(&self, current: &str, force: bool) -> Result<UpdateOutcome, String> {
            perform_update_from(&self.feed(), current, &self.bin(), force)
        }

        fn fail(&self, version: &str, code: u8, message: &str) {
            std::fs::write(
                self.root.path().join(format!("exit-{version}")),
                format!("{code} {message}\n"),
            )
            .unwrap();
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.root.path().join("calls"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }

        fn link(&self, name: &str) -> Option<PathBuf> {
            std::fs::read_link(self.home().join(name)).ok()
        }

        /// The version every installed program now names, or `None` when
        /// they disagree.
        fn installed(&self) -> Option<String> {
            let versions: std::collections::BTreeSet<String> = shipped()
                .iter()
                .map(|n| Self::version_of(&self.bin().join(n)))
                .collect();
            (versions.len() == 1).then(|| versions.into_iter().next().unwrap())
        }
    }

    /// A first update moves the files an older install left in `bin` into
    /// a release of their own, installs the new release beside it, and
    /// leaves `bin` holding links that follow `current`.
    #[cfg(unix)]
    #[test]
    fn an_update_installs_the_release_beside_the_running_one() {
        let feed = Feed::new(&["3.0.1", "3.0.0"], "3.0.0");
        feed.update("3.0.0", false).unwrap();

        assert_eq!(feed.link("current"), Some(PathBuf::from("releases/3.0.1")));
        assert_eq!(feed.link("previous"), Some(PathBuf::from("releases/3.0.0")));
        for name in shipped() {
            let release = |v: &str| feed.home().join("releases").join(v).join("bin").join(&name);
            assert_eq!(Feed::version_of(&release("3.0.1")), "3.0.1");
            assert_eq!(Feed::version_of(&release("3.0.0")), "3.0.0");
            assert_eq!(
                std::fs::read_link(feed.bin().join(&name)).unwrap(),
                feed.home().join("current/bin").join(&name),
            );
        }
        assert_eq!(feed.installed().as_deref(), Some("3.0.1"));
    }

    /// After the flip the new release's `fleet` restarts the manager through
    /// its supervisor and waits for it to serve the new version.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_update_restarts_the_manager_on_the_new_release() {
        let feed = Feed::new(&["3.0.1", "3.0.0"], "3.0.0");
        feed.update("3.0.0", false).unwrap();
        assert_eq!(feed.calls(), ["3.0.1 service restart --expect 3.0.1"]);
    }

    /// A machine with no supervisor has no manager to restart, so the
    /// update stands.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_update_without_a_supervisor_keeps_the_new_release() {
        let feed = Feed::new(&["3.0.1", "3.0.0"], "3.0.0");
        feed.fail("3.0.1", 3, "no unit runs this store");
        feed.update("3.0.0", false).unwrap();
        assert_eq!(feed.link("current"), Some(PathBuf::from("releases/3.0.1")));
    }

    /// A manager that does not come back on the new release puts `current`
    /// back, restarts the manager on the old one, and says why.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_manager_that_does_not_come_back_rolls_the_release_back() {
        let feed = Feed::new(&["3.0.1", "3.0.0"], "3.0.0");
        feed.fail("3.0.1", 7, "fleet.service did not serve 3.0.1 within 120s");

        let error = feed.update("3.0.0", false).unwrap_err();

        assert!(error.contains("rolled back to 3.0.0"), "{error}");
        assert!(error.contains("did not serve 3.0.1 within 120s"), "{error}");
        assert_eq!(feed.link("current"), Some(PathBuf::from("releases/3.0.0")));
        assert_eq!(feed.installed().as_deref(), Some("3.0.0"));
        assert_eq!(
            feed.calls(),
            [
                "3.0.1 service restart --expect 3.0.1",
                "3.0.0 service restart"
            ]
        );
    }

    /// Each update keeps `current` and `previous` and removes an older
    /// release.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_update_keeps_two_releases() {
        let feed = Feed::new(&["3.0.2", "3.0.1", "3.0.0"], "3.0.0");
        feed.update("3.0.0", false).unwrap();
        std::fs::write(
            feed.root.path().join("list.json"),
            r#"[{"tag_name": "v3.0.1", "draft": false, "prerelease": false}]"#,
        )
        .unwrap();
        feed.update("3.0.2", true).unwrap();

        let mut releases: Vec<String> = std::fs::read_dir(feed.home().join("releases"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        releases.sort();
        assert_eq!(releases, ["3.0.1", "3.0.2"]);
        assert_eq!(feed.link("previous"), Some(PathBuf::from("releases/3.0.2")));
    }

    #[test]
    fn a_release_runs_its_programs_from_the_prefix_bin() {
        let release = Path::new("/p/lib/thurbox/releases/3.0.0/bin/thurbox");
        assert_eq!(bin_dir_of(release), Some(PathBuf::from("/p/bin")));
        let plain = Path::new("/p/.local/bin/thurbox");
        assert_eq!(bin_dir_of(plain), Some(PathBuf::from("/p/.local/bin")));
    }

    /// A release a process still runs from stays until it exits.
    #[cfg(target_os = "linux")]
    #[test]
    fn collection_spares_a_release_a_process_runs_from() {
        let home = tempfile::TempDir::new().unwrap();
        let old = home.path().join("releases/1.0.0/bin");
        std::fs::create_dir_all(&old).unwrap();
        let sleep = crate::paths::resolve_on_path("sleep").unwrap();
        std::fs::copy(&sleep, old.join("sleep")).unwrap();
        for v in ["2.0.0", "3.0.0"] {
            std::fs::create_dir_all(home.path().join("releases").join(v).join("bin")).unwrap();
        }
        std::os::unix::fs::symlink("releases/3.0.0", home.path().join("current")).unwrap();
        std::os::unix::fs::symlink("releases/2.0.0", home.path().join("previous")).unwrap();
        let mut running = Command::new(old.join("sleep")).arg("30").spawn().unwrap();

        collect_releases(home.path());
        assert!(old.exists(), "a running release is kept");

        running.kill().unwrap();
        running.wait().unwrap();
        collect_releases(home.path());
        assert!(!home.path().join("releases/1.0.0").exists());
        assert!(home.path().join("releases/2.0.0").exists());
        assert!(home.path().join("releases/3.0.0").exists());
    }

    /// A major boundary: 4.0.0 is GitHub's "Latest", and nothing a 3.x
    /// updater does — the silent startup update or `thurbox-cli update --force`
    /// — may put it on disk.
    #[cfg(unix)]
    #[test]
    fn an_install_offered_the_next_major_never_installs_it() {
        let feed = Feed::new(&["4.0.0", "3.0.0"], "3.0.0");
        for force in [false, true] {
            let outcome = feed.update("3.0.0", force).unwrap();
            // `--force` reinstalls the newest 3.x — here the running one.
            if let UpdateOutcome::Updated { to, .. } = &outcome {
                assert_eq!(to, "3.0.0", "force={force}");
            }
            assert_eq!(feed.installed().as_deref(), Some("3.0.0"), "force={force}");
        }
        assert_eq!(
            feed.update("3.0.0", false).unwrap(),
            UpdateOutcome::UpToDate {
                current: "3.0.0".into(),
                latest: "3.0.0".into(),
                next_major: Some("4.0.0".into()),
            },
            "3.0.0 is reported, not offered"
        );
    }

    /// A maintenance release cut after the next major is not GitHub's
    /// "Latest" (`cd.yml` publishes it with `make_latest: false`), and an
    /// install on its line must still find and take it.
    #[cfg(unix)]
    #[test]
    fn an_install_still_takes_a_release_on_its_line_cut_after_the_next_major() {
        let feed = Feed::new(&["3.0.1", "4.0.0", "3.0.0"], "3.0.0");
        let outcome = feed.update("3.0.0", false).unwrap();
        assert_eq!(
            outcome,
            UpdateOutcome::Updated {
                from: "3.0.0".into(),
                to: "3.0.1".into(),
                next_major: Some("4.0.0".into()),
            },
            "the 3.x is taken and 4.x still reported"
        );
        assert_eq!(feed.installed().as_deref(), Some("3.0.1"));
    }

    #[cfg(unix)]
    #[test]
    fn an_install_takes_a_newer_release_on_its_line() {
        let feed = Feed::new(&["3.0.1", "3.0.0"], "3.0.0");
        let outcome = feed.update("3.0.0", false).unwrap();
        assert!(
            matches!(outcome, UpdateOutcome::Updated { .. }),
            "{outcome:?}"
        );
        assert_eq!(feed.installed().as_deref(), Some("3.0.1"));
    }

    #[test]
    fn perform_update_skips_dev_build_without_force() {
        // The dev-build guard must short-circuit before any network IO. Only
        // assert when the test binary is itself a dev build (the normal case);
        // a release-versioned test build would legitimately hit the network.
        if !crate::agent::fetch::is_dev_build() {
            return;
        }
        let outcome = perform_update(false).expect("dev build short-circuits, no network");
        assert!(matches!(outcome, UpdateOutcome::SkippedDevBuild { .. }));
    }
}
