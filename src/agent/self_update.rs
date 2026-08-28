//! Self-update: download, verify, and replace the installed thurbox binaries.
//!
//! This is the auto-update feature (gated behind `[features] auto_update`, on
//! by default for 1.0 — see [`crate::session::settings::FeatureFlags`]). It
//! surfaces two ways, both calling [`perform_update`]:
//!
//! - **TUI** — a silent check on startup (`main::spawn_auto_update`), run on a
//!   background thread so a slow download never blocks the first frame. A newer
//!   release is downloaded + installed in place (atomic renames against the
//!   install dir — the running process is untouched); the new version applies on
//!   the next launch, and the result is surfaced as a status toast.
//! - **CLI** — `thurbox-cli update` does the same on demand (`--force` bypasses
//!   the up-to-date / dev-build guards).
//!
//! It reuses the version-check plumbing ([`fetch_latest_release`],
//! [`decide_update`], [`crosses_major`], [`current_version`]) so dev builds
//! (`0.0.0-dev`) never auto-update and **a new major is never installed
//! automatically** — 2.x replaced v1's whole interface, so crossing that line is
//! the user's decision, not a background download's. It mirrors
//! `scripts/install.sh` exactly: the same release artifacts, target-triple
//! mapping, and SHA256 verification. Like the rest of
//! thurbox it adds no new crate dependency — downloads go through the
//! `curl`/`wget` helpers and `tar` / `sha256sum`/`shasum` are shelled out to.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::agent::version_check::{
    crosses_major, current_version, decide_update, fetch_latest_release,
};

/// GitHub release-download base for the thurbox repo (same repo as
/// `scripts/install.sh`); the per-release directory is `<base>/v{version}/`.
const RELEASE_BASE: &str = "https://github.com/Thurbeen/thurbox/releases/download";

/// The binaries shipped in a release tarball, replaced in place on update.
const BINARIES: [&str; 2] = ["thurbox", "thurbox-cli"];

/// Outcome of an update attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// Already on the latest release; nothing was downloaded.
    UpToDate { current: String, latest: String },
    /// Binaries were replaced; the new version applies on the next launch.
    Updated { from: String, to: String },
    /// A development build — skipped (its version doesn't order against tags).
    /// Only returned when `force` is false.
    SkippedDevBuild { current: String },
    /// A newer release exists but it is a new **major**, so it was reported and
    /// not installed. Only returned when `force` is false.
    SkippedMajor { current: String, latest: String },
}

/// Map an OS/arch pair to the release artifact's Rust target triple. Mirrors
/// `scripts/install.sh`'s `get_target`, and is restricted to the platforms
/// `cd.yml`'s release matrix actually builds a `.tar.gz` for — Linux x86_64 (we
/// pick the portable musl tarball) and Apple-silicon macOS — so an update never
/// 404s on a missing artifact. Linux aarch64 and Intel macOS have no build, so
/// they error cleanly here rather than pointing at a download that isn't there.
/// Pure (takes os/arch) so it's testable; note Rust spells these
/// `"macos"`/`"aarch64"` where `uname` says `darwin`/`arm64`, but they map to
/// the same triples.
pub fn target_triple(os: &str, arch: &str) -> Result<&'static str, String> {
    // Accepts both Rust's spellings (`macos`/`aarch64`) and `uname`'s
    // (`Darwin`/`arm64`), plus PowerShell's `AMD64`, because a *host's* platform
    // arrives as whatever that host's shell printed (`session_ops::host_cli`).
    let os = os.to_ascii_lowercase();
    let arch = arch.to_ascii_lowercase();
    match (os.as_str(), arch.as_str()) {
        ("linux", "x86_64" | "amd64") => Ok("x86_64-unknown-linux-musl"),
        ("macos" | "darwin", "aarch64" | "arm64") => Ok("aarch64-apple-darwin"),
        ("windows", "x86_64" | "amd64") => Ok("x86_64-pc-windows-msvc"),
        _ => Err(format!(
            "unsupported platform: {os}-{arch} (no release artifact is built \
             for it; see scripts/install.sh)"
        )),
    }
}

/// The release archive for `target`: a `.tar.gz` everywhere but Windows, whose
/// artifact is the `.zip` `install.ps1` extracts.
pub fn archive_name(version: &str, target: &str) -> String {
    if target.ends_with("-windows-msvc") {
        format!("thurbox-v{version}-{target}.zip")
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
    if crate::session::extension_def::is_dev_version(version) {
        return Err(format!(
            "no release archive exists for the development build {version}"
        ));
    }
    let scratch = ScratchDir::new()?;
    let checksums_path = scratch.path.join(checksums_name(version));
    crate::agent::extension_config::http_get_to_file(&checksums_url(version), &checksums_path)?;
    let name = archive_name(version, target);
    let path = scratch.path.join(&name);
    crate::agent::extension_config::http_get_to_file(
        &format!("{RELEASE_BASE}/v{version}/{name}"),
        &path,
    )?;
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

/// Release checksums filename for `version` (no leading `v`).
fn checksums_name(version: &str) -> String {
    format!("thurbox-v{version}-checksums.txt")
}

fn tarball_url(version: &str, target: &str) -> String {
    format!(
        "{RELEASE_BASE}/v{version}/{}",
        tarball_name(version, target)
    )
}

fn checksums_url(version: &str) -> String {
    format!("{RELEASE_BASE}/v{version}/{}", checksums_name(version))
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

/// Verify `file`'s SHA256 against `expected`, shelling out to `sha256sum`
/// (falling back to `shasum -a 256`) — same tools as `install.sh`.
fn verify_sha256(file: &Path, expected: &str) -> Result<(), String> {
    let attempts: [(&str, &[&str]); 2] = [("sha256sum", &[]), ("shasum", &["-a", "256"])];
    let mut errors = Vec::new();
    for (bin, extra) in attempts {
        let mut cmd = Command::new(bin);
        cmd.args(extra).arg(file);
        match cmd.output() {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let actual = stdout.split_whitespace().next().unwrap_or("");
                return if actual.eq_ignore_ascii_case(expected.trim()) {
                    Ok(())
                } else {
                    Err(format!(
                        "checksum mismatch (expected {expected}, got {actual})"
                    ))
                };
            }
            Ok(out) => errors.push(format!("{bin}: exit {}", out.status)),
            Err(_) => errors.push(format!("{bin}: not found")),
        }
    }
    Err(format!("could not compute SHA256 ({})", errors.join("; ")))
}

/// A temp dir removed (best-effort) when dropped, so a failed/early-returned
/// update leaves nothing behind. Avoids the `tempfile` crate (dev-only).
#[derive(Debug)]
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new() -> Result<Self, String> {
        // Unique per call, not per process: a peer host being provisioned
        // (`fetch_archive`) can overlap the TUI's own startup update check.
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let base = crate::paths::log_directory().unwrap_or_else(std::env::temp_dir);
        let path = base.join(format!(".update-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&path)
            .map_err(|e| format!("create temp dir {}: {e}", path.display()))?;
        Ok(Self { path })
    }
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

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("chmod {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Replace the installed binaries in `install_dir` with the ones extracted into
/// `extract_dir`. Two-phase to minimise the version-mismatch window: every
/// binary is staged + verified first, then the renames run back-to-back.
/// Returns the names actually replaced (binaries absent from either side are
/// skipped). Unit-testable: takes plain dirs, no network.
fn install_binaries(extract_dir: &Path, install_dir: &Path) -> Result<Vec<String>, String> {
    // Phase 1: stage every binary present both in the tarball and on disk.
    let mut staged: Vec<(PathBuf, PathBuf, String)> = Vec::new(); // (staged, dest, name)
    let mut skipped: Vec<String> = Vec::new();
    for name in BINARIES {
        let src = extract_dir.join(name);
        let dest = install_dir.join(name);
        if !src.exists() {
            return Err(format!("release tarball is missing `{name}`"));
        }
        if std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0) == 0 {
            return Err(format!("extracted `{name}` is empty"));
        }
        if !dest.exists() {
            // e.g. thurbox-cli not co-located next to the running thurbox.
            skipped.push(name.to_string());
            continue;
        }
        let s = stage_binary(&src, &dest)?;
        staged.push((s, dest, name.to_string()));
    }
    if staged.is_empty() {
        return Err("no installed binaries to replace in the install directory".to_string());
    }
    // Phase 2: commit staged files with atomic renames, back-to-back.
    let mut replaced = Vec::new();
    for (s, dest, name) in &staged {
        std::fs::rename(s, dest).map_err(|e| {
            format!(
                "replace {} failed: {e}. Update may be partial — reinstall with scripts/install.sh",
                dest.display()
            )
        })?;
        replaced.push(name.clone());
    }
    if !skipped.is_empty() {
        tracing::warn!(
            "auto-update: skipped {} (not in install dir)",
            skipped.join(", ")
        );
    }
    Ok(replaced)
}

/// Download, verify, extract, and install the latest release in place.
///
/// `force` bypasses the up-to-date, dev-build and major-version guards
/// (re-downloads + replaces regardless). Best-effort and side-effect-free until
/// the checksum passes — any failure before the swap leaves the installed
/// binaries untouched.
pub fn perform_update(force: bool) -> Result<UpdateOutcome, String> {
    let current = current_version().to_string();

    // Dev builds never auto-update (their version doesn't order against tags).
    if !force && crate::agent::extension_config::is_dev_build() {
        return Ok(UpdateOutcome::SkippedDevBuild { current });
    }

    let latest = fetch_latest_release()?;
    if !force && decide_update(&current, &latest).is_none() {
        return Ok(UpdateOutcome::UpToDate { current, latest });
    }

    // A new major is a different program under the same binary name — 2.x
    // replaced v1's compiled-in interface with the plugin kernel — so it is
    // reported and left for the user to take deliberately. Without this a 1.x
    // install silently woke up running 2.x.
    if !force && crosses_major(&current, &latest) {
        return Ok(UpdateOutcome::SkippedMajor { current, latest });
    }

    let target = current_target()?;
    // The Windows artifact is a zip that `install.ps1` extracts; this tar path
    // has never served it, and resolving the triple for a *peer* (shared
    // sessions) must not make it start pretending to.
    if target.ends_with("-windows-msvc") {
        return Err("self-update is not supported on Windows; re-run install.ps1".to_string());
    }
    let scratch = ScratchDir::new()?;

    // Download checksums + tarball.
    let checksums_path = scratch.path.join(checksums_name(&latest));
    crate::agent::extension_config::http_get_to_file(&checksums_url(&latest), &checksums_path)?;
    let tarball = tarball_name(&latest, target);
    let tarball_path = scratch.path.join(&tarball);
    crate::agent::extension_config::http_get_to_file(&tarball_url(&latest, target), &tarball_path)?;

    // Verify the download BEFORE touching anything installed.
    let checksums =
        std::fs::read_to_string(&checksums_path).map_err(|e| format!("read checksums: {e}"))?;
    let expected = parse_checksum(&checksums, &tarball)
        .ok_or_else(|| format!("no checksum for {tarball} in release checksums"))?;
    verify_sha256(&tarball_path, &expected)?;

    // Extract and install.
    let extract_dir = scratch.path.join("extract");
    std::fs::create_dir_all(&extract_dir).map_err(|e| format!("create extract dir: {e}"))?;
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball_path)
        .arg("-C")
        .arg(&extract_dir)
        .status()
        .map_err(|e| format!("run tar: {e}"))?;
    if !status.success() {
        return Err(format!("tar extraction failed (exit {status})"));
    }

    let install_dir = std::env::current_exe()
        .map_err(|e| format!("resolve current executable: {e}"))?
        .parent()
        .ok_or("running executable has no parent directory")?
        .to_path_buf();
    install_binaries(&extract_dir, &install_dir)?;

    Ok(UpdateOutcome::Updated {
        from: current,
        to: latest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The triples an update or a peer provisioning may download — the
    /// artifacts `cd.yml` builds that `target_triple` actually selects (it
    /// prefers musl over the also-built gnu tarball for Linux x86_64; the
    /// Windows artifact is a `.zip`, served by `archive_name`). `target_triple`
    /// / `current_target` must only ever resolve to one of these — anything
    /// else would 404 on download.
    const SHIPPED_TRIPLES: [&str; 3] = [
        "x86_64-unknown-linux-musl",
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

    #[test]
    fn target_triple_rejects_unshipped_platforms() {
        // Platforms `cd.yml` does NOT build an artifact for must error cleanly
        // rather than resolve to a non-existent one.
        assert!(target_triple("windows", "arm64").is_err());
        assert!(target_triple("linux", "riscv64").is_err());
        // aarch64 Linux and Intel macOS are intentionally not shipped.
        assert!(target_triple("linux", "aarch64").is_err());
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
        assert_eq!(checksums_name("0.114.0"), "thurbox-v0.114.0-checksums.txt");
        let url = tarball_url("0.114.0", "aarch64-apple-darwin");
        assert!(url.starts_with(RELEASE_BASE), "got: {url}");
        assert!(url.contains("/v0.114.0/"), "got: {url}");
        assert!(url.ends_with(".tar.gz"), "got: {url}");
        assert!(checksums_url("0.114.0").contains("/v0.114.0/"));
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
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
            // New extracted binaries with fresh contents.
            std::fs::write(extract.path().join(name), format!("NEW-{name}")).unwrap();
        }

        let replaced = install_binaries(extract.path(), install.path()).unwrap();
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
    #[test]
    fn install_binaries_skips_binaries_absent_from_install_dir() {
        let install = tempfile::TempDir::new().unwrap();
        let extract = tempfile::TempDir::new().unwrap();
        for name in BINARIES {
            std::fs::write(extract.path().join(name), b"NEW").unwrap();
        }
        // Only `thurbox` is installed; `thurbox-cli` is not co-located.
        std::fs::write(install.path().join("thurbox"), b"OLD").unwrap();

        let replaced = install_binaries(extract.path(), install.path()).unwrap();
        assert_eq!(replaced, vec!["thurbox".to_string()]);
        assert!(!install.path().join("thurbox-cli").exists());
    }

    #[test]
    fn install_binaries_errors_when_tarball_missing_a_binary() {
        let install = tempfile::TempDir::new().unwrap();
        let extract = tempfile::TempDir::new().unwrap();
        std::fs::write(install.path().join("thurbox"), b"OLD").unwrap();
        // extract dir has neither binary
        let err = install_binaries(extract.path(), install.path()).unwrap_err();
        assert!(err.contains("missing"), "got: {err}");
    }

    // Unix-only: `verify_sha256` shells out to `sha256sum`/`shasum` (like
    // `install.sh`). On a native Windows runner those Unix coreutils behave
    // inconsistently (a Git-for-Windows `sha256sum` mis-hashes a Windows path),
    // so this exercises a Unix-tool path. Windows self-update is opt-in and
    // shells the same Unix tooling — covered by the dockur VM, not this CI job.
    #[cfg(unix)]
    #[test]
    fn verify_sha256_matches_and_detects_mismatch() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("empty");
        std::fs::write(&file, b"").unwrap();
        // SHA256 of empty input — the canonical value.
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        match verify_sha256(&file, empty) {
            Ok(()) => {
                // Compare is case-insensitive.
                assert!(verify_sha256(&file, &empty.to_uppercase()).is_ok());
                let err = verify_sha256(&file, &"0".repeat(64)).unwrap_err();
                assert!(err.contains("mismatch"), "got: {err}");
            }
            // No sha256sum/shasum on this machine — only the tool-missing path
            // is reachable, so assert that and stop.
            Err(e) => assert!(e.contains("could not compute"), "unexpected: {e}"),
        }
    }

    #[test]
    fn perform_update_skips_dev_build_without_force() {
        // The dev-build guard must short-circuit before any network IO. Only
        // assert when the test binary is itself a dev build (the normal case);
        // a release-versioned test build would legitimately hit the network.
        if !crate::agent::extension_config::is_dev_build() {
            return;
        }
        let outcome = perform_update(false).expect("dev build short-circuits, no network");
        assert!(matches!(outcome, UpdateOutcome::SkippedDevBuild { .. }));
    }
}
