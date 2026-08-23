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

/// Validate that a manifest-supplied path is a safe relative path (no absolute
/// root, no `..` components) — the shared path-traversal guard for payloads
/// delivered under a directory the caller controls (extension installs, and
/// anything else that joins untrusted relative paths onto a base).
///
/// This is the one canonical implementation (see also [`safe_join`]).
/// `session::plugin_spec::validate_destination` deliberately keeps its own,
/// stricter variant: `session` is the pure-data leaf and may reference no
/// crate module (tests/architecture_rules.rs), and its rule set is different
/// (Lua-only, string-level checks on both separators so a spec written on
/// Windows still loads on Linux).
pub fn ensure_safe_relative(rel: &str) -> Result<(), String> {
    let p = Path::new(rel);
    if p.is_absolute() {
        return Err(format!(
            "manifest path '{rel}' must be relative, not absolute"
        ));
    }
    for c in p.components() {
        match c {
            std::path::Component::Normal(_) | std::path::Component::CurDir => {}
            _ => {
                return Err(format!(
                    "manifest path '{rel}' must not contain '..' or a root component"
                ))
            }
        }
    }
    Ok(())
}

/// [`ensure_safe_relative`] + join under `base`.
pub fn safe_join(base: &Path, rel: &str) -> Result<PathBuf, String> {
    ensure_safe_relative(rel)?;
    Ok(base.join(rel))
}

#[cfg(test)]
mod tests;
