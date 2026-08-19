//! Centralized path resolution for application data files.
//!
//! This module provides a unified interface for resolving paths to:
//! - Config files (`~/.config/thurbox[-dev]/config.toml`)
//! - SQLite database (`~/.local/share/thurbox[-dev]/thurbox.db`)
//! - Log directories (`~/.local/share/thurbox[-dev]/`)
//!
//! Dev builds (`0.0.0-dev`) use `thurbox-dev` subdirectories to avoid
//! interfering with an installed release binary.
//!
//! ## Production Behavior
//!
//! By default, uses XDG Base Directory Specification:
//! - Prefers `$XDG_CONFIG_HOME` for config, fallback to `$HOME/.config`
//! - Prefers `$XDG_DATA_HOME` for data, fallback to `$HOME/.local/share`
//!
//! ## Testing Behavior
//!
//! Tests can override path resolution using `TestPathGuard`:
//! ```ignore
//! #[test]
//! fn test_with_custom_paths() {
//!     let temp_dir = tempfile::TempDir::new().unwrap();
//!     let _guard = TestPathGuard::new(temp_dir.path());
//!
//!     // All paths now resolve under temp_dir
//!     let config = config_file().unwrap();
//!     assert_eq!(config, temp_dir.path().join("config.toml"));
//! }
//! ```

use std::cell::RefCell;
use std::path::{Path, PathBuf};

/// Env var pinning the resolved config app dir for a child process (an agent
/// whose hook calls `thurbox-cli`), so it targets the same config the spawning
/// thurbox uses regardless of XDG/binary-flavor/tmux-server-env drift. Injected
/// at spawn ([`crate::session_ops`]); consumed by `config_app_dir`.
pub const CONFIG_DIR_OVERRIDE_ENV: &str = "THURBOX_CONFIG_DIR";
/// Data counterpart of [`CONFIG_DIR_OVERRIDE_ENV`] (`THURBOX_DATA_DIR`).
pub const DATA_DIR_OVERRIDE_ENV: &str = "THURBOX_DATA_DIR";

/// Returns "thurbox-dev" for dev builds, "thurbox" for release builds.
#[cfg_attr(test, allow(dead_code))] // only used by the non-test XDG fallback
fn app_dir_name() -> &'static str {
    if cfg!(dev_build) {
        "thurbox-dev"
    } else {
        "thurbox"
    }
}

/// The user's home directory: `$HOME` on Unix, `%USERPROFILE%` on Windows.
pub fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).map(PathBuf::from)
}

/// Whether `exe` resolves on `PATH`. A minimal lookup that avoids pulling in a
/// `which` crate for a one-off probe (used to detect optional helper binaries
/// like `wsl.exe` / `powershell.exe`); cheap PATH scan, no process spawn.
pub fn which_on_path(exe: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(exe).exists())
}

/// Base directory for config files. `$XDG_CONFIG_HOME` wins on every platform
/// (some users set it on Windows too); otherwise `%APPDATA%` on Windows,
/// `$HOME/.config` on Unix.
#[cfg_attr(test, allow(dead_code))] // only used by the non-test XDG fallback
fn config_base() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(x));
    }
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        home_dir().map(|h| h.join(".config"))
    }
}

/// Base directory for data files. `$XDG_DATA_HOME` wins on every platform;
/// otherwise `%LOCALAPPDATA%` on Windows, `$HOME/.local/share` on Unix.
#[cfg_attr(test, allow(dead_code))] // only used by the non-test XDG fallback
fn data_base() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
        return Some(PathBuf::from(x));
    }
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        home_dir().map(|h| h.join(".local").join("share"))
    }
}

/// Per-process temp sandbox for the XDG fallback in **test builds only**.
///
/// The unit-test harness (`cargo test`/`nextest`) frequently runs *inside* a
/// live thurbox session (the dev shell is itself an agent session), whose env
/// carries `THURBOX_CONFIG_DIR`/`THURBOX_DATA_DIR` pointing at the developer's
/// **real** config/data dirs (injected so an agent's `thurbox-cli` hook targets
/// the same DB — see `session_ops::inject_thurbox_env`). Honoring those in tests
/// — or falling through to the real `$HOME/.config/thurbox` — let any unguarded
/// test that writes config (settings save, hooks install, keybindings) clobber
/// the user's live settings. So in test builds the XDG fallback ignores the
/// override env entirely and resolves under a `<pid>`-scoped temp dir instead;
/// `TestPathGuard`/`set_test_dir` (the `Override` strategy) still wins where a
/// test wants a specific base.
#[cfg(test)]
fn test_sandbox_base() -> PathBuf {
    std::env::temp_dir().join(format!("thurbox-unittest-{}", std::process::id()))
}

/// Resolved thurbox config app dir. A `THURBOX_CONFIG_DIR` env override (the
/// already-resolved dir, incl. the `thurbox`/`thurbox-dev` segment) wins — this
/// is how the TUI pins child processes (agent hooks calling `thurbox-cli`) to
/// the *same* config it uses, immune to a stale tmux-server env or which
/// `thurbox-cli` binary is on PATH. Otherwise `<config_base>/<app>`. In test
/// builds the env override is ignored in favor of a temp sandbox — see
/// [`test_sandbox_base`].
#[cfg(not(test))]
fn config_app_dir() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os(CONFIG_DIR_OVERRIDE_ENV).filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(x));
    }
    Some(config_base()?.join(app_dir_name()))
}

/// Test build: pin the config dir to a temp sandbox, ignoring the inherited
/// `THURBOX_CONFIG_DIR` — see [`test_sandbox_base`].
#[cfg(test)]
fn config_app_dir() -> Option<PathBuf> {
    Some(test_sandbox_base().join("config"))
}

/// Resolved thurbox data app dir; see [`config_app_dir`] (`THURBOX_DATA_DIR`).
#[cfg(not(test))]
fn data_app_dir() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os(DATA_DIR_OVERRIDE_ENV).filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(x));
    }
    Some(data_base()?.join(app_dir_name()))
}

/// Test build: pin the data dir to a temp sandbox; see [`config_app_dir`].
#[cfg(test)]
fn data_app_dir() -> Option<PathBuf> {
    Some(test_sandbox_base().join("data"))
}

/// `<config_app_dir>/<filename>`.
fn xdg_config_subpath(filename: &str) -> Option<PathBuf> {
    Some(config_app_dir()?.join(filename))
}

/// `<data_app_dir>/<segments...>`.
fn xdg_data_subpath(segments: &[&str]) -> Option<PathBuf> {
    let mut p = data_app_dir()?;
    for seg in segments {
        p.push(seg);
    }
    Some(p)
}

/// Categories of application paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    /// The config directory's anchor: `~/.config/thurbox/config.toml`.
    ///
    /// The *file* is legacy and read only for migration, but the path is not:
    /// every config location is derived from it by
    /// `with_file_name`/`parent().join(..)` — `agents.toml`, `hosts.toml`,
    /// `settings.toml`, `themes.toml`, `extensions/`, `ui/`, `ui.json`. So this is
    /// the single place the `thurbox` / `thurbox-dev` split enters for all of
    /// them, and calling it "migration only" invited the conclusion that the
    /// interface directory was derived from something vestigial.
    Config,
    /// Log directory: `~/.local/share/thurbox/`
    LogDir,
    /// SQLite database: `~/.local/share/thurbox/thurbox.db`
    Database,
    /// Agent metrics files: `~/.local/share/thurbox/metrics/`
    MetricsDir,
    /// Embedded built-in extensions materialized for install:
    /// `~/.local/share/thurbox/builtin-extensions/`
    BuiltinExtensionsDir,
    /// Git worktrees: `~/.local/share/thurbox/worktrees/`
    WorktreesDir,
    /// Per-session multi-repo symlink workspaces:
    /// `~/.local/share/thurbox/workspaces/`
    WorkspacesDir,
    /// User keybindings JSON file: `~/.config/thurbox/keybindings.json`
    KeybindingsFile,
}

/// Path resolution strategy (thread-local).
#[derive(Debug, PartialEq)]
enum PathStrategy {
    /// Production: Use XDG Base Directory Specification.
    Xdg,
    /// Testing: Use custom base directory for all paths.
    Override(PathBuf),
}

thread_local! {
    static PATH_STRATEGY: RefCell<PathStrategy> = const { RefCell::new(PathStrategy::Xdg) };
}

/// Resolve a path based on the current strategy.
///
/// # Returns
///
/// - `Some(path)` - Successfully resolved path
/// - `None` - Could not resolve path (e.g., HOME not set in XDG mode)
pub fn resolve(kind: PathKind) -> Option<PathBuf> {
    PATH_STRATEGY.with(|strategy| {
        let s = strategy.borrow();
        match *s {
            PathStrategy::Xdg => resolve_xdg(kind),
            PathStrategy::Override(ref base) => Some(resolve_override(base, kind)),
        }
    })
}

/// Resolve a path using XDG Base Directory Specification.
fn resolve_xdg(kind: PathKind) -> Option<PathBuf> {
    match kind {
        PathKind::Config => xdg_config_subpath("config.toml"),
        PathKind::Database => xdg_data_subpath(&["thurbox.db"]),
        PathKind::LogDir => xdg_data_subpath(&[]),
        PathKind::MetricsDir => xdg_data_subpath(&["metrics"]),
        PathKind::BuiltinExtensionsDir => xdg_data_subpath(&["builtin-extensions"]),
        PathKind::WorktreesDir => xdg_data_subpath(&["worktrees"]),
        PathKind::WorkspacesDir => xdg_data_subpath(&["workspaces"]),
        PathKind::KeybindingsFile => xdg_config_subpath("keybindings.json"),
    }
}

/// Resolve a path using a custom base directory (for testing).
fn resolve_override(base: &Path, kind: PathKind) -> PathBuf {
    match kind {
        PathKind::Config => base.join("config.toml"),
        PathKind::LogDir => base.to_path_buf(),
        PathKind::Database => base.join("thurbox.db"),
        PathKind::MetricsDir => base.join("metrics"),
        PathKind::BuiltinExtensionsDir => base.join("builtin-extensions"),
        PathKind::WorktreesDir => base.join("worktrees"),
        PathKind::WorkspacesDir => base.join("workspaces"),
        PathKind::KeybindingsFile => base.join("keybindings.json"),
    }
}

/// Resolve the config file path.
///
/// Returns: `$XDG_CONFIG_HOME/thurbox/config.toml` or `$HOME/.config/thurbox/config.toml`
pub fn config_file() -> Option<PathBuf> {
    resolve(PathKind::Config)
}

/// Resolve the log directory path.
///
/// Returns: `$XDG_DATA_HOME/thurbox/` or `$HOME/.local/share/thurbox/`
pub fn log_directory() -> Option<PathBuf> {
    resolve(PathKind::LogDir)
}

/// Resolve the database file path.
///
/// Returns: `$XDG_DATA_HOME/thurbox/thurbox.db` or `$HOME/.local/share/thurbox/thurbox.db`
pub fn database_file() -> Option<PathBuf> {
    resolve(PathKind::Database)
}

/// Validate that `name` is a safe single-segment identifier — non-empty,
/// no dot-prefix, no slashes / backslashes / `..`, max 64 chars. Used by
/// `session_ops::spawn` to guard names that become on-disk paths.
pub fn validate_safe_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Name cannot be empty".into());
    }
    if name.len() > 64 {
        return Err("Name too long (max 64 characters)".into());
    }
    if name.starts_with('.') {
        return Err("Name cannot start with '.'".into());
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err("Name contains invalid characters".into());
    }
    Ok(())
}

/// Resolve the agent metrics directory path.
///
/// Returns: `$XDG_DATA_HOME/thurbox/metrics/` or `$HOME/.local/share/thurbox/metrics/`
pub fn metrics_directory() -> Option<PathBuf> {
    resolve(PathKind::MetricsDir)
}

/// Directory where embedded built-in extensions are materialized so the
/// extension installer can treat them as a local source.
///
/// Returns: `$XDG_DATA_HOME/thurbox/builtin-extensions/` or
/// `$HOME/.local/share/thurbox/builtin-extensions/`
pub fn builtin_extensions_directory() -> Option<PathBuf> {
    resolve(PathKind::BuiltinExtensionsDir)
}

/// Resolve the worktrees directory path.
///
/// Returns: `$XDG_DATA_HOME/thurbox/worktrees/` or `$HOME/.local/share/thurbox/worktrees/`
pub fn worktrees_directory() -> Option<PathBuf> {
    resolve(PathKind::WorktreesDir)
}

/// Resolve the multi-repo workspaces directory path.
///
/// Returns: `$XDG_DATA_HOME/thurbox/workspaces/` or
/// `$HOME/.local/share/thurbox/workspaces/`
pub fn workspaces_directory() -> Option<PathBuf> {
    resolve(PathKind::WorkspacesDir)
}

/// Resolve the user keybindings file path.
///
/// Returns: `$XDG_CONFIG_HOME/thurbox/keybindings.json` or
/// `$HOME/.config/thurbox/keybindings.json`.
pub fn keybindings_file() -> Option<PathBuf> {
    resolve(PathKind::KeybindingsFile)
}

/// Returns true if a Claude transcript file `<agent_session_id>.jsonl` exists
/// under `<root>/projects/*/`.
///
/// Root resolution: `config_dir_override` → `$CLAUDE_CONFIG_DIR` → `~/.claude`.
/// Used by restart paths to decide between `--resume` (transcript exists) and
/// `--session-id` (fresh start with same id).
pub fn claude_transcript_exists(
    agent_session_id: &str,
    config_dir_override: Option<&Path>,
) -> bool {
    let root = if let Some(p) = config_dir_override {
        p.to_path_buf()
    } else if let Some(env) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        PathBuf::from(env)
    } else {
        match home_dir() {
            Some(h) => h.join(".claude"),
            None => return false,
        }
    };

    let projects = root.join("projects");
    let Ok(entries) = std::fs::read_dir(&projects) else {
        return false;
    };
    let target = format!("{agent_session_id}.jsonl");
    for entry in entries.flatten() {
        if entry.path().join(&target).is_file() {
            return true;
        }
    }
    false
}

/// Override path resolution for all paths to use a custom base directory.
///
/// This is primarily intended for testing. All paths will resolve under the given base:
/// - `config_file()` → `base/config.toml`
/// - `log_directory()` → `base/`
/// - `database_file()` → `base/thurbox.db`
///
/// # Note
///
/// This change is thread-local and affects only the current thread.
/// Use `reset_to_xdg()` or `TestPathGuard` to restore XDG behavior.
pub fn set_test_dir(base: impl Into<PathBuf>) {
    PATH_STRATEGY.with(|strategy| {
        *strategy.borrow_mut() = PathStrategy::Override(base.into());
    });
}

/// Reset path resolution back to XDG Base Directory Specification.
pub fn reset_to_xdg() {
    PATH_STRATEGY.with(|strategy| {
        *strategy.borrow_mut() = PathStrategy::Xdg;
    });
}

/// RAII guard for test path overrides.
///
/// Automatically resets to XDG behavior when dropped.
/// Simplifies test setup/teardown:
///
/// ```ignore
/// #[test]
/// fn test_with_override() {
///     let temp_dir = tempfile::TempDir::new().unwrap();
///     let _guard = TestPathGuard::new(temp_dir.path());
///
///     // Paths are overridden in this scope...
///     let config = config_file();
///
///     // Automatically reset on drop
/// }
/// ```
pub struct TestPathGuard;

impl TestPathGuard {
    /// Create a new test path guard with the given base directory.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        set_test_dir(base_dir);
        TestPathGuard
    }
}

impl Drop for TestPathGuard {
    fn drop(&mut self) {
        reset_to_xdg();
    }
}

/// Expand a leading `~` followed by a path separator to the user's home
/// directory. On Windows both separators are accepted (`~/` and `~\`); on Unix
/// only `~/` is (a backslash is a legal filename character there).
///
/// - `"~/foo"` → `"/home/user/foo"`
/// - `"~\\foo"` → `"C:\\Users\\user\\foo"` (Windows)
/// - `"~"` → `"/home/user"`
/// - `"/absolute/path"` → unchanged
/// - `"relative/path"` → unchanged
pub fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    } else if let Some(rest) = strip_tilde_prefix(path) {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

/// Strip a leading `~` + path separator, returning the remainder. Accepts `~/`
/// everywhere and `~\` on Windows (where `\` is a path separator).
fn strip_tilde_prefix(path: &str) -> Option<&str> {
    if let Some(rest) = path.strip_prefix("~/") {
        return Some(rest);
    }
    if cfg!(windows) {
        return path.strip_prefix("~\\");
    }
    None
}

/// Short display label for a repo/dir path: the final path component,
/// falling back to the full path when there is no file name (e.g. `/`).
///
/// - `/home/user/Repositories/thurbox` → `thurbox`
/// - `/home/user/Repositories/thurbox/` → `thurbox` (trailing slash ignored)
/// - `/` → `/`
pub fn display_path(path: &Path) -> String {
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => path.display().to_string(),
    }
}

/// Find the longest common prefix among a slice of strings.
fn longest_common_prefix(strings: &[String]) -> String {
    if strings.is_empty() {
        return String::new();
    }
    let first = &strings[0];
    let mut prefix_len = first.len();
    for s in &strings[1..] {
        prefix_len = prefix_len.min(s.len());
        for (i, (a, b)) in first.bytes().zip(s.bytes()).enumerate() {
            if a != b {
                prefix_len = prefix_len.min(i);
                break;
            }
        }
    }
    first[..prefix_len].to_string()
}

/// Directory names directly under `parent` that start with `prefix`. Hidden
/// entries (`.`-prefixed) are included only when `prefix` itself is hidden.
/// Returns an empty vec when `parent` can't be read.
fn matching_dir_names(parent: &Path, prefix: &str) -> Vec<String> {
    let show_hidden = prefix.starts_with('.');
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            if !show_hidden && name.starts_with('.') {
                return None;
            }
            name.starts_with(prefix).then_some(name)
        })
        .collect()
}

/// Fish-style directory path completion.
///
/// Given a partial path input, returns the suffix to complete it.
/// Only considers directories. Hidden entries (starting with `.`) are
/// included only when the user's prefix starts with `.`.
///
/// # Examples
///
/// - Input `"/home/us"` with `/home/user/` existing → `Some("er/")`
/// - Input `"/home/user/"` → suggests first common prefix of children
/// - Input `"/nonexistent"` → `None`
pub fn complete_directory_path(input: &str) -> Option<String> {
    if input.is_empty() {
        return None;
    }

    let expanded = expand_tilde(input);
    let expanded_str = expanded.to_str().unwrap_or(input);
    let path = Path::new(expanded_str);

    // Determine parent directory and the prefix the user is typing. A trailing
    // path separator (`/` everywhere, plus `\` on Windows — tilde expansion
    // yields `C:\Users\me\`) means "list this directory's contents".
    let ends_with_sep = expanded_str
        .chars()
        .next_back()
        .is_some_and(std::path::is_separator);
    let (parent, prefix) = if ends_with_sep {
        (path.to_path_buf(), String::new())
    } else {
        let parent = path.parent()?.to_path_buf();
        let file_name = path.file_name()?.to_str()?;
        (parent, file_name.to_string())
    };

    let matches = matching_dir_names(&parent, &prefix);

    if matches.is_empty() {
        return None;
    }

    let common = longest_common_prefix(&matches);
    let beyond_typed = &common[prefix.len()..];
    if beyond_typed.is_empty() && matches.len() > 1 {
        return None;
    }

    let completed = parent.join(&common);
    let suffix = if completed.is_dir() {
        format!("{beyond_typed}/")
    } else {
        beyond_typed.to_string()
    };

    if suffix.is_empty() {
        None
    } else {
        Some(suffix)
    }
}

/// Reduce a display name / session id to a safe single path segment for a
/// symlink-workspace link or directory name. Shared by the local
/// (`workspace`) and remote (`git`) workspace builders so their layouts match
/// by construction — neither may depend on the other.
pub(crate) fn sanitize_workspace_segment(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' => '-',
            c if c.is_whitespace() => '-',
            c => c,
        })
        .collect();
    cleaned.trim_matches(['.', '-']).to_string()
}

/// Sanitize `name` and make it unique within `used` by appending `-2`, `-3`,
/// …; an empty sanitized name falls back to `repo`. Same sharing rationale as
/// [`sanitize_workspace_segment`].
pub(crate) fn unique_link_name(name: &str, used: &mut std::collections::HashSet<String>) -> String {
    let sanitized = sanitize_workspace_segment(name);
    let base = if sanitized.is_empty() {
        "repo".to_string()
    } else {
        sanitized
    };
    if used.insert(base.clone()) {
        return base;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The env var `home_dir()` reads on this platform: `USERPROFILE` on
    /// Windows, `HOME` elsewhere. Tests that exercise tilde expansion source the
    /// home directory from the same var so they pass on every target.
    const HOME_VAR: &str = if cfg!(windows) { "USERPROFILE" } else { "HOME" };

    #[test]
    fn display_path_uses_basename() {
        assert_eq!(
            display_path(Path::new("/home/user/Repositories/thurbox")),
            "thurbox"
        );
    }

    #[test]
    fn which_on_path_finds_present_and_rejects_absent() {
        let dir = tempfile::TempDir::new().unwrap();
        // `which_on_path` checks for the file verbatim (no `.exe` munging), so a
        // plain marker filename resolves identically on every platform.
        let marker = "tbx_which_probe_marker";
        std::fs::write(dir.path().join(marker), b"").unwrap();

        let saved = std::env::var_os("PATH");
        std::env::set_var("PATH", dir.path());
        let found = which_on_path(marker);
        let missing = which_on_path("tbx_which_probe_absent");
        match saved {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }

        assert!(found, "marker on PATH should be found");
        assert!(!missing, "a name not on PATH should not be found");
    }

    #[test]
    fn display_path_ignores_trailing_slash() {
        assert_eq!(
            display_path(Path::new("/home/user/Repositories/thurbox/")),
            "thurbox"
        );
    }

    #[test]
    fn display_path_falls_back_to_full_path_without_file_name() {
        assert_eq!(display_path(Path::new("/")), "/");
    }

    #[test]
    fn transcript_exists_detects_file_under_any_project_slug() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("projects").join("-some-slug");
        std::fs::create_dir_all(&proj).unwrap();
        let sid = "11111111-2222-3333-4444-555555555555";
        std::fs::write(proj.join(format!("{sid}.jsonl")), b"").unwrap();

        assert!(claude_transcript_exists(sid, Some(tmp.path())));
        assert!(!claude_transcript_exists("not-present", Some(tmp.path())));
    }

    #[test]
    fn transcript_exists_returns_false_when_projects_dir_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!claude_transcript_exists("any-id", Some(tmp.path())));
    }

    #[test]
    fn default_strategy_is_xdg() {
        reset_to_xdg();
        PATH_STRATEGY.with(|s| {
            assert_eq!(*s.borrow(), PathStrategy::Xdg);
        });
    }

    #[test]
    fn test_build_ignores_config_and_data_dir_override_env() {
        // Regression: the unit-test harness often runs *inside* a live thurbox
        // session whose env carries THURBOX_CONFIG_DIR/THURBOX_DATA_DIR pointing
        // at the developer's real config/data. On the XDG strategy a test build
        // must ignore those and stay under the per-process temp sandbox, so an
        // unguarded config write can never clobber the user's live settings.
        reset_to_xdg();
        let saved_cfg = std::env::var_os(CONFIG_DIR_OVERRIDE_ENV);
        let saved_data = std::env::var_os(DATA_DIR_OVERRIDE_ENV);
        std::env::set_var(CONFIG_DIR_OVERRIDE_ENV, "/real/config/thurbox");
        std::env::set_var(DATA_DIR_OVERRIDE_ENV, "/real/data/thurbox");

        let cfg = config_file().unwrap();
        let db = database_file().unwrap();

        match saved_cfg {
            Some(v) => std::env::set_var(CONFIG_DIR_OVERRIDE_ENV, v),
            None => std::env::remove_var(CONFIG_DIR_OVERRIDE_ENV),
        }
        match saved_data {
            Some(v) => std::env::set_var(DATA_DIR_OVERRIDE_ENV, v),
            None => std::env::remove_var(DATA_DIR_OVERRIDE_ENV),
        }

        assert!(cfg.starts_with(test_sandbox_base()), "config: {cfg:?}");
        assert!(db.starts_with(test_sandbox_base()), "db: {db:?}");
        assert!(!cfg.starts_with("/real/config/thurbox"), "config: {cfg:?}");
        assert!(!db.starts_with("/real/data/thurbox"), "db: {db:?}");
    }

    #[test]
    fn override_isolates_paths() {
        let base = PathBuf::from("/test/base");
        set_test_dir(&base);

        assert_eq!(config_file(), Some(base.join("config.toml")));
        assert_eq!(log_directory(), Some(base.clone()));
        assert_eq!(database_file(), Some(base.join("thurbox.db")));

        reset_to_xdg();
    }

    #[test]
    fn guard_resets_on_drop() {
        let base = PathBuf::from("/test/base");
        {
            let _guard = TestPathGuard::new(&base);
            assert_eq!(config_file(), Some(base.join("config.toml")));
        }
        PATH_STRATEGY.with(|s| {
            assert_eq!(*s.borrow(), PathStrategy::Xdg);
        });
    }

    #[test]
    fn thread_local_isolation() {
        let base1 = PathBuf::from("/test/base1");
        set_test_dir(&base1);

        assert_eq!(config_file(), Some(base1.join("config.toml")));

        // A fresh thread starts with the Xdg default, unaffected by this one.
        let handle =
            std::thread::spawn(|| PATH_STRATEGY.with(|s| matches!(*s.borrow(), PathStrategy::Xdg)));

        assert!(handle.join().unwrap());

        assert_eq!(config_file(), Some(base1.join("config.toml")));

        reset_to_xdg();
    }

    #[test]
    fn all_path_kinds_resolve_in_override() {
        let base = PathBuf::from("/test/override");
        set_test_dir(&base);

        assert_eq!(resolve(PathKind::Config), Some(base.join("config.toml")));
        assert_eq!(resolve(PathKind::LogDir), Some(base.clone()));
        assert_eq!(resolve(PathKind::Database), Some(base.join("thurbox.db")));
        assert_eq!(resolve(PathKind::MetricsDir), Some(base.join("metrics")));
        assert_eq!(
            resolve(PathKind::WorktreesDir),
            Some(base.join("worktrees"))
        );
        assert_eq!(
            resolve(PathKind::KeybindingsFile),
            Some(base.join("keybindings.json"))
        );

        reset_to_xdg();
    }

    #[test]
    fn config_file_convenience() {
        let base = PathBuf::from("/custom");
        set_test_dir(&base);

        let path = config_file().unwrap();
        assert!(path.ends_with("config.toml"));

        reset_to_xdg();
    }

    #[test]
    fn log_directory_convenience() {
        let base = PathBuf::from("/custom");
        set_test_dir(&base);

        let path = log_directory().unwrap();
        assert_eq!(path, base);

        reset_to_xdg();
    }

    #[test]
    fn database_file_convenience() {
        let base = PathBuf::from("/custom");
        set_test_dir(&base);

        let path = database_file().unwrap();
        assert!(path.ends_with("thurbox.db"));

        reset_to_xdg();
    }

    #[test]
    fn set_test_dir_explicit() {
        reset_to_xdg();

        let base = PathBuf::from("/test/explicit");
        set_test_dir(&base);

        assert_eq!(config_file(), Some(base.join("config.toml")));

        reset_to_xdg();

        PATH_STRATEGY.with(|s| {
            assert_eq!(*s.borrow(), PathStrategy::Xdg);
        });
    }

    #[test]
    fn override_persists_across_calls() {
        let base = PathBuf::from("/persistent");
        set_test_dir(&base);

        for _ in 0..3 {
            assert_eq!(config_file(), Some(base.join("config.toml")));
        }

        reset_to_xdg();
    }

    #[test]
    fn multiple_guards_reset_correctly() {
        let base1 = PathBuf::from("/base1");
        let base2 = PathBuf::from("/base2");

        {
            let _guard1 = TestPathGuard::new(&base1);
            assert_eq!(config_file(), Some(base1.join("config.toml")));

            {
                let _guard2 = TestPathGuard::new(&base2);
                assert_eq!(config_file(), Some(base2.join("config.toml")));
            }

            PATH_STRATEGY.with(|s| matches!(*s.borrow(), PathStrategy::Xdg));
        }

        PATH_STRATEGY.with(|s| {
            assert_eq!(*s.borrow(), PathStrategy::Xdg);
        });
    }

    #[test]
    fn resolve_override_all_kinds() {
        let base = Path::new("/data");

        assert_eq!(
            resolve_override(base, PathKind::Config),
            PathBuf::from("/data/config.toml")
        );
        assert_eq!(
            resolve_override(base, PathKind::LogDir),
            PathBuf::from("/data")
        );
        assert_eq!(
            resolve_override(base, PathKind::Database),
            PathBuf::from("/data/thurbox.db")
        );
        assert_eq!(
            resolve_override(base, PathKind::MetricsDir),
            PathBuf::from("/data/metrics")
        );
        assert_eq!(
            resolve_override(base, PathKind::WorktreesDir),
            PathBuf::from("/data/worktrees")
        );
    }

    #[test]
    fn metrics_directory_convenience() {
        let base = PathBuf::from("/custom");
        set_test_dir(&base);

        let path = metrics_directory().unwrap();
        assert!(path.ends_with("metrics"));

        reset_to_xdg();
    }

    #[test]
    fn worktrees_directory_convenience() {
        let base = PathBuf::from("/custom");
        set_test_dir(&base);

        let path = worktrees_directory().unwrap();
        assert!(path.ends_with("worktrees"));

        reset_to_xdg();
    }

    #[test]
    fn longest_common_prefix_empty() {
        assert_eq!(longest_common_prefix(&[]), "");
    }

    #[test]
    fn longest_common_prefix_single() {
        assert_eq!(longest_common_prefix(&["hello".to_string()]), "hello");
    }

    #[test]
    fn longest_common_prefix_multiple() {
        assert_eq!(
            longest_common_prefix(&[
                "foobar".to_string(),
                "foobaz".to_string(),
                "fooqux".to_string(),
            ]),
            "foo"
        );
    }

    #[test]
    fn longest_common_prefix_identical() {
        assert_eq!(
            longest_common_prefix(&["abc".to_string(), "abc".to_string()]),
            "abc"
        );
    }

    #[test]
    fn longest_common_prefix_no_common() {
        assert_eq!(
            longest_common_prefix(&["abc".to_string(), "xyz".to_string()]),
            ""
        );
    }

    #[test]
    fn complete_directory_path_empty_input() {
        assert_eq!(complete_directory_path(""), None);
    }

    #[test]
    fn complete_directory_path_nonexistent() {
        assert_eq!(complete_directory_path("/nonexistent_dir_xyz_123"), None);
    }

    #[test]
    fn complete_directory_path_with_real_dir() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(temp.path().join("uniquedir")).unwrap();
        let input = format!("{}/uniqued", temp.path().display());
        assert_eq!(complete_directory_path(&input), Some("ir/".to_string()));
    }

    #[test]
    fn complete_directory_path_trailing_slash() {
        // A real directory with a trailing slash lists children — must not panic.
        let temp = tempfile::TempDir::new().unwrap();
        let result = complete_directory_path(&format!("{}/", temp.path().display()));
        assert!(result.is_some() || result.is_none());
    }

    #[test]
    fn complete_directory_path_exact_match() {
        let temp = tempfile::TempDir::new().unwrap();
        let inner = temp.path().join("exact");
        std::fs::create_dir(&inner).unwrap();
        let result = complete_directory_path(&inner.display().to_string());
        assert_eq!(result, Some("/".to_string()));
    }

    #[test]
    fn complete_directory_path_root() {
        // "/" is a valid directory — shouldn't panic.
        let result = complete_directory_path("/");
        assert!(result.is_some() || result.is_none());
    }

    #[test]
    fn complete_directory_path_with_tempdir() {
        let temp = tempfile::TempDir::new().unwrap();
        let base = temp.path();

        std::fs::create_dir(base.join("project_alpha")).unwrap();
        std::fs::create_dir(base.join("project_beta")).unwrap();
        std::fs::create_dir(base.join("other")).unwrap();

        // Two matches, no completion beyond the typed prefix → None.
        let input = format!("{}/project_", base.display());
        let result = complete_directory_path(&input);
        assert_eq!(result, None);

        let input = format!("{}/project_a", base.display());
        let result = complete_directory_path(&input);
        assert_eq!(result, Some("lpha/".to_string()));

        let input = format!("{}/oth", base.display());
        let result = complete_directory_path(&input);
        assert_eq!(result, Some("er/".to_string()));
    }

    #[test]
    fn complete_directory_path_skips_hidden_by_default() {
        let temp = tempfile::TempDir::new().unwrap();
        let base = temp.path();

        std::fs::create_dir(base.join(".hidden")).unwrap();
        std::fs::create_dir(base.join("visible")).unwrap();

        let input = format!("{}/", base.display());
        let result = complete_directory_path(&input);
        assert_eq!(result, Some("visible/".to_string()));
    }

    #[test]
    fn complete_directory_path_shows_hidden_with_dot_prefix() {
        let temp = tempfile::TempDir::new().unwrap();
        let base = temp.path();

        std::fs::create_dir(base.join(".hidden")).unwrap();
        std::fs::create_dir(base.join("visible")).unwrap();

        let input = format!("{}/.hid", base.display());
        let result = complete_directory_path(&input);
        assert_eq!(result, Some("den/".to_string()));
    }

    #[test]
    fn complete_directory_path_ignores_files() {
        let temp = tempfile::TempDir::new().unwrap();
        let base = temp.path();

        std::fs::write(base.join("readme.md"), "content").unwrap();
        std::fs::create_dir(base.join("src")).unwrap();

        let input = format!("{}/rea", base.display());
        let result = complete_directory_path(&input);
        assert_eq!(result, None);

        let input = format!("{}/sr", base.display());
        let result = complete_directory_path(&input);
        assert_eq!(result, Some("c/".to_string()));
    }

    #[test]
    fn longest_common_prefix_different_lengths() {
        assert_eq!(
            longest_common_prefix(&["ab".to_string(), "abcdef".to_string()]),
            "ab"
        );
    }

    #[test]
    fn expand_tilde_home() {
        let home = std::env::var(HOME_VAR).unwrap();
        assert_eq!(expand_tilde("~/foo"), PathBuf::from(&home).join("foo"));
    }

    #[test]
    fn expand_tilde_bare() {
        let home = std::env::var(HOME_VAR).unwrap();
        assert_eq!(expand_tilde("~"), PathBuf::from(&home));
    }

    #[test]
    fn expand_tilde_absolute() {
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
    }

    #[test]
    fn expand_tilde_relative() {
        assert_eq!(expand_tilde("rel/path"), PathBuf::from("rel/path"));
    }

    #[test]
    fn expand_tilde_no_home() {
        // Temporarily unset the home var — use a thread to avoid interfering with
        // other tests.
        let result = std::thread::spawn(|| {
            let orig = std::env::var_os(HOME_VAR);
            std::env::remove_var(HOME_VAR);
            let p = expand_tilde("~/foo");
            if let Some(home) = orig {
                std::env::set_var(HOME_VAR, home);
            }
            p
        })
        .join()
        .unwrap();
        assert_eq!(result, PathBuf::from("~/foo"));
    }

    #[test]
    fn expand_tilde_nested_path() {
        let home = std::env::var(HOME_VAR).unwrap();
        assert_eq!(
            expand_tilde("~/a/b/c"),
            PathBuf::from(&home).join("a").join("b").join("c")
        );
    }

    #[test]
    fn expand_tilde_empty() {
        assert_eq!(expand_tilde(""), PathBuf::from(""));
    }

    #[test]
    fn expand_tilde_other_user() {
        // ~otheruser is NOT expanded — only ~ and ~/ are handled.
        assert_eq!(expand_tilde("~otheruser"), PathBuf::from("~otheruser"));
    }

    #[cfg(windows)]
    #[test]
    fn expand_tilde_backslash_on_windows() {
        // On Windows `~\foo` uses the native separator and must expand too.
        let home = std::env::var(HOME_VAR).unwrap();
        assert_eq!(expand_tilde("~\\foo"), PathBuf::from(&home).join("foo"));
        assert_eq!(
            expand_tilde("~\\a\\b"),
            PathBuf::from(&home).join("a").join("b")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn expand_tilde_backslash_literal_on_unix() {
        // On Unix `\` is a legal filename character, not a separator, so
        // `~\foo` is left untouched.
        assert_eq!(expand_tilde("~\\foo"), PathBuf::from("~\\foo"));
    }

    #[test]
    fn complete_directory_path_tilde() {
        let temp = tempfile::TempDir::new().unwrap();
        let base = temp.path();
        std::fs::create_dir(base.join("mydir")).unwrap();

        let input = format!("{}/my", base.display());
        let result = complete_directory_path(&input);
        assert_eq!(result, Some("dir/".to_string()));
    }

    #[test]
    fn complete_directory_path_tilde_trailing_slash() {
        // "~/" completions are relative suffixes, never absolute.
        if let Some(s) = complete_directory_path("~/") {
            assert!(!s.starts_with('/'));
        }
    }

    #[test]
    fn sanitize_workspace_segment_strips_separators_and_dots() {
        // Slashes/backslashes/colons and whitespace → `-`; leading/trailing
        // `.`/`-` trimmed.
        assert_eq!(sanitize_workspace_segment("a/b\\c:d"), "a-b-c-d");
        assert_eq!(sanitize_workspace_segment("  .git  "), "git");
        assert_eq!(sanitize_workspace_segment("my repo"), "my-repo");
        assert_eq!(sanitize_workspace_segment("--.hidden.--"), "hidden");
        // A session-id UUID (the real workspace-dir input) is unchanged.
        assert_eq!(
            sanitize_workspace_segment("d5715d35-9599-4507-9901-ef33b9476358"),
            "d5715d35-9599-4507-9901-ef33b9476358"
        );
    }

    #[test]
    fn unique_link_name_dedups_with_dash_two_suffix() {
        // First collision is `-2`, then `-3`; an empty label falls back to
        // `repo`.
        let mut used = std::collections::HashSet::new();
        assert_eq!(unique_link_name("webapp", &mut used), "webapp");
        assert_eq!(unique_link_name("webapp", &mut used), "webapp-2");
        assert_eq!(unique_link_name("webapp", &mut used), "webapp-3");
        assert_eq!(unique_link_name("", &mut used), "repo");
        assert_eq!(unique_link_name("", &mut used), "repo-2");
    }
}
