//! Fetching from a source, and the release this binary is: what the plugin
//! installer and the updater share.
//!
//! A source is a base URL, a local directory or a git repository. The official
//! repository's sets are pinned to this binary's release tag, so what they hand
//! over matches the binary asking for it. Downloads go through `curl`, falling
//! back to `wget`, with a timeout.

use std::path::PathBuf;
use std::process::Command;

/// Raw-content root of the thurbox repo (no git ref).
const OFFICIAL_REPO_RAW: &str = "https://raw.githubusercontent.com/Thurbeen/thurbox";

/// The running binary's version string (e.g. `0.113.0`, or `0.0.0-dev` for a
/// development build), injected at compile time by `build.rs`.
pub fn binary_version() -> &'static str {
    env!("THURBOX_VERSION")
}

/// Whether this is an unstable development build (its version doesn't order
/// against release tags, so version checks are skipped for it).
pub fn is_dev_build() -> bool {
    cfg!(dev_build) || is_dev_version(binary_version())
}

/// The git ref to fetch the official sets from: the running binary's release
/// tag (`v0.7.0`), so what is fetched matches the binary that reads it. Dev
/// builds track `main`.
pub fn official_ref() -> String {
    if is_dev_build() {
        "main".to_string()
    } else {
        format!("v{}", binary_version())
    }
}

/// Base URL for one of the repo's officially-distributed sets, pinned to this
/// binary's release tag.
///
/// A pane reads `thurbox.*`, a contract that moves, so what the official source
/// hands over should match the binary asking for it.
pub fn official_set_base(folder: &str) -> String {
    official_set_base_at(folder, &official_ref())
}

/// [`official_set_base`] at an explicit git ref.
///
/// What a lockfile needs: an entry recorded at `v2.0.0` has to keep resolving to
/// `v2.0.0` after the binary moves on, or applying the same spec elsewhere would
/// deliver whatever is newest — which is the property a lock exists to provide.
pub fn official_set_base_at(folder: &str, git_ref: &str) -> String {
    format!("{OFFICIAL_REPO_RAW}/{git_ref}/{folder}")
}

/// Where files come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A base URL (no trailing slash); files fetched via `<base>/<rel>`.
    Remote(String),
    /// A local directory containing `extension.toml` + payload files.
    Local(PathBuf),
    /// A version-control repository, obtained as a working copy rather than
    /// fetched file by file.
    ///
    /// The only form that can deliver bytes: [`fetch_file`] returns a `String` and
    /// decodes remote output lossily, so a binary through it is corrupted rather
    /// than refused. A clone carries whatever the repository holds, preserves its
    /// layout, and identifies exactly what it delivered.
    Git(String),
}

/// Resolve an install target into a source:
/// - a repository (`git+…`, `….git`, `git@host:path`) → a working copy,
/// - `http(s)://…` → that base URL,
/// - a path-like target (`/abs`, `./rel`, `~/x`, anything with a `/`) → local dir,
/// - a bare name → `<name>` in the official repository's `folder`.
pub fn resolve_source_in(target: &str, folder: &str) -> Source {
    let t = target.trim();
    // A repository, recognised EXPLICITLY and never guessed. Three unambiguous
    // forms: a `git+` prefix, a `.git` suffix, or the scp-like `git@host:path`.
    //
    // A bare `https://github.com/owner/repo` deliberately does NOT clone. That
    // spelling already means "a base URL to fetch manifest-named files from", and
    // reinterpreting it by host would change the meaning of every existing install
    // for one forge while leaving it alone for others. `git+` is the escape hatch
    // for a repository URL with no `.git` suffix — cargo's spelling, for this
    // reason.
    if let Some(url) = git_url(t) {
        return Source::Git(url);
    }
    if let Some(rest) = t
        .strip_prefix("https://")
        .or_else(|| t.strip_prefix("http://"))
    {
        let scheme = if t.starts_with("https://") {
            "https://"
        } else {
            "http://"
        };
        return Source::Remote(format!("{scheme}{}", rest.trim_end_matches('/')));
    }
    // Looks like a path (not a bare name)? Detect `/` and `.`/`~`
    // prefixes, plus Windows forms: a `\` separator or an absolute drive path
    // (`C:\…`), which carry neither a `/` nor a leading `.`.
    let looks_local = t.contains('/')
        || t.contains('\\')
        || t.starts_with('.')
        || t.starts_with('~')
        || std::path::Path::new(t).is_absolute();
    if looks_local {
        return Source::Local(expand_tilde(t));
    }
    Source::Remote(format!("{}/{t}", official_set_base(folder)))
}

/// The repository URL a target names, or `None` when it does not name one.
///
/// The `git+` prefix is stripped: it is thurbox's marker, not part of the URL git
/// is handed.
pub fn git_url(target: &str) -> Option<String> {
    let t = target.trim();
    if let Some(rest) = t.strip_prefix("git+") {
        return (!rest.is_empty()).then(|| rest.to_string());
    }
    if t.ends_with(".git") {
        return Some(t.to_string());
    }
    // scp-like: `git@host:path`. Required to have both a host and a path, so a bare
    // `git@host` is not mistaken for one.
    if let Some(rest) = t.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            if !host.is_empty() && !path.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// Expand a leading `~/` (or bare `~`) to the user's home directory. Shared by
/// the source resolver and the installer so both agree on home expansion.
/// Delegates to [`crate::paths::expand_tilde`] — the single implementation —
/// which additionally honours `~\` on Windows.
pub fn expand_tilde(path: &str) -> PathBuf {
    crate::paths::expand_tilde(path)
}

/// Fetch one file's text content from a source (`<source>/<rel>`).
pub fn fetch_file(source: &Source, rel: &str) -> Result<String, String> {
    match source {
        Source::Local(dir) => {
            let p = dir.join(rel);
            std::fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))
        }
        Source::Remote(base) => http_get(&format!("{base}/{rel}")),
        // A repository is obtained as a working copy, not read file by file — that
        // is the whole point of the form. Reaching here means a caller took the
        // fetch path for a git source, which is a bug at that call site.
        Source::Git(url) => Err(format!(
            "{url} is a repository: it is cloned, not fetched file by file"
        )),
    }
}

/// Run the first of `attempts` (a `(binary, args)` list) that succeeds,
/// returning its raw stdout. On failure the aggregated cause is surfaced: a
/// missing tool is distinguished from a tool that ran but failed (e.g. HTTP 404
/// for a misspelled name), whose last stderr line is included. Shared by the
/// `curl`-then-`wget` downloaders below; the caller adds the URL context.
fn run_first_ok(attempts: &[(&str, Vec<&str>)]) -> Result<Vec<u8>, String> {
    let mut errors = Vec::new();
    for (bin, args) in attempts {
        match Command::new(bin).args(args).output() {
            Ok(out) if out.status.success() => return Ok(out.stdout),
            Ok(out) => {
                // The tool ran but failed (404, DNS, refused…) — keep its stderr.
                let stderr = String::from_utf8_lossy(&out.stderr);
                let detail = stderr.trim();
                let detail = if detail.is_empty() {
                    format!("exit {}", out.status)
                } else {
                    detail.lines().last().unwrap_or(detail).to_string()
                };
                errors.push(format!("{bin}: {detail}"));
            }
            Err(_) => errors.push(format!("{bin}: not found")),
        }
    }
    Err(errors.join("; "))
}

/// HTTP GET to stdout via `curl` (falling back to `wget`) — same dependency-free
/// approach as the shell installer, both run with a timeout.
///
/// Note: the body is decoded as UTF-8 (lossy); what is fetched this way is
/// expected to be text, not binaries.
pub fn http_get(url: &str) -> Result<String, String> {
    let attempts: [(&str, Vec<&str>); 2] = [
        ("curl", vec!["-fsSL", "--max-time", "30", url]),
        ("wget", vec!["--timeout=30", "-O", "-", url]),
    ];
    run_first_ok(&attempts)
        .map(|stdout| String::from_utf8_lossy(&stdout).into_owned())
        .map_err(|e| format!("failed to fetch {url} ({e})"))
}

/// Download a URL's raw bytes to `dest` via `curl` (falling back to `wget`) —
/// the bytes sibling of [`http_get`], for binary artifacts (release tarballs)
/// that must not be UTF-8-decoded. Same approach, but a longer timeout (a
/// tarball is larger than a text file).
pub fn http_get_to_file(url: &str, dest: &std::path::Path) -> Result<(), String> {
    let dest_str = dest
        .to_str()
        .ok_or_else(|| format!("non-UTF-8 destination path: {}", dest.display()))?;
    let attempts: [(&str, Vec<&str>); 2] = [
        (
            "curl",
            vec!["-fsSL", "--max-time", "120", "-o", dest_str, url],
        ),
        ("wget", vec!["--timeout=120", "-O", dest_str, url]),
    ];
    run_first_ok(&attempts)
        .map(|_| ())
        .map_err(|e| format!("failed to download {url} ({e})"))
}

/// Whether a version string denotes an unstable dev build (`0.0.0-dev`, or any
/// version carrying a `-dev`/pre-release suffix). Such builds skip update checks
/// because their version doesn't order against releases.
pub fn is_dev_version(v: &str) -> bool {
    v.contains("-dev") || v.trim_start_matches('v').starts_with("0.0.0")
}

/// The major component of a dotted version string (optional leading `v`, any
/// `-suffix` ignored), or `None` when there is no leading number to read.
///
/// Split out from [`compare_versions`] because a *major* difference is not just
/// a bigger ordering: thurbox's 2.x line changed the whole interface, so
/// crossing that boundary is a decision rather than a newer patch.
pub fn major_version(v: &str) -> Option<u64> {
    v.trim()
        .trim_start_matches('v')
        .split('-')
        .next()?
        .split('.')
        .next()?
        .parse::<u64>()
        .ok()
}

/// Compare two dotted version strings (`a.b.c`, optional leading `v`, any
/// `-suffix` ignored) numerically, component by component. Missing trailing
/// components count as `0` (so `1.2` == `1.2.0`). Non-numeric components sort as
/// `0`. A dependency-free stand-in for the `semver` crate, sufficient for the
/// `major.minor.patch` tags thurbox ships.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parts = |s: &str| -> Vec<u64> {
        s.trim_start_matches('v')
            .split('-')
            .next()
            .unwrap_or("")
            .split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    };
    let (pa, pb) = (parts(a), parts(b));
    let n = pa.len().max(pb.len());
    for i in 0..n {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compare_versions_orders_numerically() {
        use std::cmp::Ordering;
        assert_eq!(compare_versions("0.113.0", "0.113.0"), Ordering::Equal);
        assert_eq!(compare_versions("0.114.0", "0.113.0"), Ordering::Greater);
        assert_eq!(compare_versions("0.113.0", "0.114.0"), Ordering::Less);
        // Numeric, not lexical: 0.20 > 0.9.
        assert_eq!(compare_versions("0.20.0", "0.9.0"), Ordering::Greater);
        // Leading `v` and missing trailing components are tolerated.
        assert_eq!(compare_versions("v1.2", "1.2.0"), Ordering::Equal);
        assert_eq!(compare_versions("2.0.0", "1.99.99"), Ordering::Greater);
        // Pre-release suffix is ignored for ordering.
        assert_eq!(compare_versions("0.113.0-rc1", "0.113.0"), Ordering::Equal);
    }

    #[test]
    fn major_version_reads_the_leading_component() {
        assert_eq!(major_version("1.8.7"), Some(1));
        assert_eq!(major_version("v2.2.2"), Some(2));
        assert_eq!(major_version("2"), Some(2));
        assert_eq!(major_version("2.0.0-rc1"), Some(2));
        assert_eq!(major_version("  v10.1.0 "), Some(10));
        // Nothing numeric to read: report that rather than guessing a `0`,
        // which `compare_versions` may do because ordering needs a number and a
        // major-boundary decision needs a fact.
        assert_eq!(major_version(""), None);
        assert_eq!(major_version("main"), None);
    }

    #[test]
    fn is_dev_version_detects_unstable_builds() {
        assert!(is_dev_version("0.0.0-dev"));
        assert!(is_dev_version("0.113.0-dev"));
        assert!(is_dev_version("0.0.0"));
        assert!(!is_dev_version("0.113.0"));
        assert!(!is_dev_version("v1.2.3"));
    }

    /// The three repository forms, and — the load-bearing half — that nothing else
    /// became one.
    ///
    /// A bare `https://…` URL silently reinterpreted as a clone would change the
    /// meaning of every install that works today, which is why recognition is
    /// explicit rather than guessed from the host.
    #[test]
    fn a_repository_source_is_recognised_only_in_its_explicit_forms() {
        for (target, expected) in [
            // `git+` prefix, stripped from what git is handed.
            (
                "git+https://github.com/you/thurbox-widget",
                "https://github.com/you/thurbox-widget",
            ),
            // A `.git` suffix speaks for itself.
            (
                "https://github.com/you/thurbox-widget.git",
                "https://github.com/you/thurbox-widget.git",
            ),
            // scp-like.
            (
                "git@github.com:you/thurbox-widget.git",
                "git@github.com:you/thurbox-widget.git",
            ),
        ] {
            match resolve_source_in(target, "examples/panes") {
                Source::Git(url) => assert_eq!(url, expected, "{target}"),
                other => panic!("{target} should be a repository, got {other:?}"),
            }
        }

        // And these must keep the meanings they already have.
        assert!(matches!(
            resolve_source_in("https://github.com/you/thurbox-widget", "examples/panes"),
            Source::Remote(_)
        ));
        assert!(matches!(
            resolve_source_in("./local/pane", "examples/panes"),
            Source::Local(_)
        ));
        assert!(matches!(
            resolve_source_in("tasks", "examples/panes"),
            Source::Remote(_)
        ));
    }

    #[test]
    fn a_near_miss_is_not_a_repository() {
        // `git@host` with no path, and a bare `git+`, are not sources.
        assert_eq!(git_url("git@github.com"), None);
        assert_eq!(git_url("git+"), None);
        // A directory merely named `.github` is not a `.git` suffix.
        assert_eq!(git_url("./.github"), None);
        // A path that happens to contain the word.
        assert_eq!(git_url("./my-git-panes"), None);
    }

    /// A repository must never be read through the text fetch path: that is where a
    /// binary is corrupted rather than refused, so the mistake is made loud.
    #[test]
    fn fetching_a_file_from_a_repository_is_refused() {
        let error = fetch_file(
            &Source::Git("https://example.com/x.git".into()),
            "plugin.toml",
        )
        .expect_err("should refuse");
        assert!(error.contains("cloned"), "{error}");
    }

    #[test]
    fn resolve_source_distinguishes_name_url_and_path() {
        let base = official_set_base("examples/panes");
        assert!(base.starts_with(OFFICIAL_REPO_RAW));
        assert_eq!(
            resolve_source_in("tasks", "examples/panes"),
            Source::Remote(format!("{base}/tasks"))
        );
        assert_eq!(
            resolve_source_in("https://example.com/panes/foo/", "examples/panes"),
            Source::Remote("https://example.com/panes/foo".into())
        );
        assert_eq!(
            resolve_source_in("./panes/foo", "examples/panes"),
            Source::Local(PathBuf::from("./panes/foo"))
        );
        assert_eq!(
            resolve_source_in("/abs/foo", "examples/panes"),
            Source::Local(PathBuf::from("/abs/foo"))
        );
    }
}
