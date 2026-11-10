//! Centralized path resolution for application data files.
//!
//! This module provides a unified interface for resolving paths to:
//! - Config files (`~/.config/thurbox[-dev]/config.toml`)
//! - The interface's data files (`~/.local/share/thurbox[-dev]/`)
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

// The name of this build's directories and its CLI, and the PATH and WSL
// lookups, are the multiplexer runtime's as well: one definition, read here.
pub use runtime_mux::instance::{app_dir_name, resolve_cli_binary};
pub use runtime_mux::path_lookup::{
    current_wsl_distro, is_executable_file, path_dirs, resolve_on_path, which_on_path,
    WSL_DISTRO_NAME_VAR,
};

use std::cell::RefCell;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Env var pinning the resolved config app dir for a child process (an agent
/// whose hook calls `thurbox-cli`), so it targets the same config the spawning
/// thurbox uses regardless of XDG/binary-flavor/tmux-server-env drift. Consumed
/// by `config_app_dir`.
pub const CONFIG_DIR_OVERRIDE_ENV: &str = "THURBOX_CONFIG_DIR";
/// Data counterpart of [`CONFIG_DIR_OVERRIDE_ENV`] (`THURBOX_DATA_DIR`).
pub const DATA_DIR_OVERRIDE_ENV: &str = "THURBOX_DATA_DIR";

/// The user's home directory: `$HOME` on Unix, `%USERPROFILE%` on Windows.
pub fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).map(PathBuf::from)
}

/// Write a file beside its destination, then replace the destination.
///
/// The one writer for every file a user edits or thurbox reads back;
/// `tests/architecture_rules.rs` holds production code outside `paths` to it.
///
/// Keeping the staged file in the same directory makes the final rename a
/// same-filesystem operation: readers see either the previous complete file or
/// the new complete file. A failed staged write leaves the destination alone.
/// A replaced file keeps its permissions — a `chmod 600` config stays private
/// and an executable stays executable — which a bare rename would reset. A
/// read-only one is refused, as `fs::write` refused it: the rename needs only
/// the directory to be writable, and would otherwise undo the user's lock.
/// The rename is `std::fs::rename` on every platform: on Windows it replaces
/// with POSIX semantics, so a reader holding the old file open does not make
/// the save fail, where `MoveFileExW` answers "Access is denied".
/// This does not promise durability across storage or operating-system failure;
/// neither the file nor its parent directory is synced explicitly.
pub(crate) fn write_atomically(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    static SAVES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = write_destination(path)?;
    let staged = path.with_extension(format!(
        "saving-{}-{}",
        std::process::id(),
        SAVES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let kept = match std::fs::metadata(&path) {
        Ok(existing) if existing.permissions().readonly() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{} is read-only", path.display()),
            ));
        }
        Ok(existing) => Some(existing.permissions()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    open_staged(&staged, kept.as_ref())
        .and_then(|mut file| std::io::Write::write_all(&mut file, contents.as_ref()))
        .and_then(|()| kept.map_or(Ok(()), |mode| std::fs::set_permissions(&staged, mode)))
        .and_then(|()| std::fs::rename(&staged, &path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&staged);
        })
}

/// Run `change` holding an exclusive lock on `<path>.lock`, so a
/// read-change-write of `path` by another process (a second interface, or
/// `thurbox-cli`) waits for this one instead of losing its update. The lock is
/// the operating system's (`flock` on Unix, `LockFileEx` on Windows): a
/// process that dies releases it. Locking a sibling file rather than `path`
/// itself is what keeps [`write_atomically`]'s rename possible, since the
/// rename replaces the file a lock on `path` would be held on.
pub(crate) fn with_file_lock<T>(path: &Path, change: impl FnOnce() -> T) -> std::io::Result<T> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(PathBuf::from(name))?;
    lock.lock()?;
    Ok(change())
}

/// Create the staged file, on Unix already no more open than the file it will
/// replace: a crash before [`write_atomically`] applies the final permissions
/// must not leave a readable copy of a private file. The umask can only narrow
/// the mode, so the exact one is applied after the write.
///
/// A file already at the staged name is a leftover of a crashed process that
/// had this pid, so it is removed rather than reopened with its own mode.
fn open_staged(
    staged: &Path,
    kept: Option<&std::fs::Permissions>,
) -> std::io::Result<std::fs::File> {
    match std::fs::remove_file(staged) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if let Some(kept) = kept {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(kept.mode() & 0o7777);
    }
    #[cfg(not(unix))]
    let _ = kept;
    options.open(staged)
}

/// Resolve the file itself separately from its parent so an existing config
/// symlink keeps pointing at the file its manager owns. Canonicalising the
/// parent for a new file also gives Windows APIs the extended-length path Rust
/// uses beyond `MAX_PATH`.
fn write_destination(path: &Path) -> std::io::Result<PathBuf> {
    const MAX_LINKS: usize = 40;

    let mut destination = path.to_path_buf();
    for _ in 0..MAX_LINKS {
        match std::fs::symlink_metadata(&destination) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = std::fs::read_link(&destination)?;
                destination = if target.is_absolute() {
                    target
                } else {
                    destination
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(target)
                };
            }
            Ok(_) => return std::fs::canonicalize(destination),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let name = destination.file_name().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "config path has no file name",
                    )
                })?;
                let parent = destination.parent().unwrap_or_else(|| Path::new("."));
                return Ok(std::fs::canonicalize(parent)?.join(name));
            }
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("too many config file symlinks"))
}

/// Run `f` with [`WSL_DISTRO_NAME_VAR`] set to `distro` (or unset, for
/// `None`), restoring what was there before.
///
/// Serialized on a process-wide lock, and the **only** way a test may set it,
/// for the reason [`with_path`] gives.
#[cfg(test)]
pub(crate) fn with_wsl_distro<T>(distro: Option<&str>, f: impl FnOnce() -> T) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var_os(WSL_DISTRO_NAME_VAR);
    match distro {
        Some(d) => std::env::set_var(WSL_DISTRO_NAME_VAR, d),
        None => std::env::remove_var(WSL_DISTRO_NAME_VAR),
    }
    let out = f();
    match saved {
        Some(v) => std::env::set_var(WSL_DISTRO_NAME_VAR, v),
        None => std::env::remove_var(WSL_DISTRO_NAME_VAR),
    }
    out
}

/// Run `f` with `PATH` set to `path`, restoring what was there before.
///
/// Serialized on a process-wide lock, and the **only** way a test may set
/// `PATH`: it is process state, and under plain `cargo test` the unit tests
/// that need it run concurrently in one process, where interleaved writes make
/// one test observe another's directory or restore a stale value. (`nextest`,
/// the repo's gate, gives each test its own process — this is what keeps the
/// other entry point honest.)
#[cfg(test)]
pub(crate) fn with_path<T>(path: impl AsRef<OsStr>, f: impl FnOnce() -> T) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // A test that panicked while holding it poisoned the lock; the value is
    // `()`, so there is nothing to protect against and the next test may run.
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let saved = std::env::var_os("PATH");
    std::env::set_var("PATH", path.as_ref());
    let out = f();
    match saved {
        Some(v) => std::env::set_var("PATH", v),
        None => std::env::remove_var("PATH"),
    }
    out
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
/// **real** config/data dirs. Honoring those in tests
/// — or falling through to the real `$HOME/.config/thurbox` — let any unguarded
/// test that writes config (settings save, hooks install, keybindings) clobber
/// the user's live settings. So in test builds the XDG fallback ignores the
/// override env entirely and resolves under a temp sandbox instead;
/// `TestPathGuard`/`set_test_dir` (the `Override` strategy) still wins where a
/// test wants a specific base.
///
/// The sandbox is one directory per process, shared by every thread, and it is
/// removed when that process exits. It has to be process-wide: a test that
/// fans work out to threads keeps its `Override` to itself (`PATH_STRATEGY` is
/// thread-local), so its workers land here, and a sandbox scoped any tighter
/// than the process would take their output with it while the test still
/// wants it.
///
/// That rules out letting a `Drop` do the cleanup — nothing owned by a
/// `static` is ever dropped — which is why the removal hangs off `atexit`
/// instead. It used to hang off nothing at all: the sandbox was a `<pid>`
/// path with no owner, so every run left a directory behind for good, and
/// nextest — one process per test — left one per test. The system temp dir is
/// tmpfs on many machines, where that is a slow leak of RAM.
#[cfg(test)]
fn test_sandbox_base() -> PathBuf {
    /// The `atexit` handler: `SANDBOX` is initialized by the time the process
    /// can reach an exit, and reading a `OnceLock` needs no lock.
    extern "C" fn remove_sandbox() {
        if let Some(dir) = SANDBOX.get() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    static SANDBOX: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

    SANDBOX
        .get_or_init(|| {
            let dir = tempfile::Builder::new()
                .prefix("thurbox-unittest-")
                .tempdir()
                .expect("temp sandbox for the unit-test config/data dirs");
            // SAFETY: registered once, from inside `get_or_init`, with a
            // plain function pointer that outlives the process. `keep()`
            // disarms `TempDir`'s own `Drop` so the two never race.
            unsafe { libc::atexit(remove_sandbox) };
            dir.keep()
        })
        .clone()
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

/// The data directory when this instance has been **relocated** — i.e.
/// [`DATA_DIR_OVERRIDE_ENV`] names a directory other than the XDG default.
/// `None` for a default instance, *including* one whose `THURBOX_DATA_DIR`
/// merely restates the default: a session started from inside thurbox can
/// inherit that variable, so "the variable is set" is a different question
/// from "this instance was moved".
///
/// A relocated instance keeps its own files; `backend::mux_settings` hands this to the runtime to put
/// those sessions on a tmux socket of their own instead of the operator's
/// shared server.
#[cfg(not(test))]
pub fn relocated_data_dir() -> Option<PathBuf> {
    let over = std::env::var_os(DATA_DIR_OVERRIDE_ENV);
    let default = data_base().map(|b| b.join(app_dir_name()));
    relocated_from(over.as_deref(), default.as_deref())
}

/// Test build: never relocated. The data dir is a temp sandbox (see
/// [`test_sandbox_base`]), so deriving from it would make every unit test's
/// socket a function of where that sandbox landed; [`relocated_from`] carries
/// the behaviour under test.
#[cfg(test)]
pub fn relocated_data_dir() -> Option<PathBuf> {
    None
}

/// The pure half of [`relocated_data_dir`]: `override_dir` when it names a
/// directory other than `default_dir`. Compared as paths, so a trailing
/// separator is not a relocation. An unresolvable default (no `HOME`, no XDG)
/// counts as one — there is then no default instance to share a server with.
fn relocated_from(override_dir: Option<&OsStr>, default_dir: Option<&Path>) -> Option<PathBuf> {
    let dir = PathBuf::from(override_dir.filter(|s| !s.is_empty())?);
    (Some(dir.as_path()) != default_dir).then_some(dir)
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
    /// `settings.toml`, `themes.toml`, `ui/`, `ui.json`,
    /// `plugin-state.json`. So this is
    /// the single place the `thurbox` / `thurbox-dev` split enters for all of
    /// them, and calling it "migration only" invited the conclusion that the
    /// interface directory was derived from something vestigial.
    Config,
    /// Log directory: `~/.local/share/thurbox/`
    LogDir,
    /// The perf snapshot a running interface publishes:
    /// `~/.local/share/thurbox/perf.json`
    PerfSnapshot,
    /// The repositories the creation flow remembers:
    /// `~/.local/share/thurbox/bookmarks.json`
    Bookmarks,
    /// The dedupe keys `notify send` holds:
    /// `~/.local/share/thurbox/notify-dedupe.json`
    NotifyDedupe,
    /// Agent metrics files: `~/.local/share/thurbox/metrics/`
    MetricsDir,
    /// Per-session multi-repo symlink workspaces:
    /// `~/.local/share/thurbox/workspaces/`
    WorkspacesDir,
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
        PathKind::PerfSnapshot => xdg_data_subpath(&["perf.json"]),
        PathKind::Bookmarks => xdg_data_subpath(&["bookmarks.json"]),
        PathKind::NotifyDedupe => xdg_data_subpath(&["notify-dedupe.json"]),
        PathKind::LogDir => xdg_data_subpath(&[]),
        PathKind::MetricsDir => xdg_data_subpath(&["metrics"]),
        PathKind::WorkspacesDir => xdg_data_subpath(&["workspaces"]),
    }
}

/// Resolve a path using a custom base directory (for testing).
fn resolve_override(base: &Path, kind: PathKind) -> PathBuf {
    match kind {
        PathKind::Config => base.join("config.toml"),
        PathKind::LogDir => base.to_path_buf(),
        PathKind::PerfSnapshot => base.join("perf.json"),
        PathKind::Bookmarks => base.join("bookmarks.json"),
        PathKind::NotifyDedupe => base.join("notify-dedupe.json"),
        PathKind::MetricsDir => base.join("metrics"),
        PathKind::WorkspacesDir => base.join("workspaces"),
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

/// The perf snapshot a running interface publishes and `thurbox-cli perf`
/// reads.
///
/// Returns: `$XDG_DATA_HOME/thurbox/perf.json` or `$HOME/.local/share/thurbox/perf.json`
pub fn perf_snapshot_file() -> Option<PathBuf> {
    resolve(PathKind::PerfSnapshot)
}

/// The dedupe keys `notify send` holds, and when each hold ends.
pub fn notify_dedupe_file() -> Option<PathBuf> {
    resolve(PathKind::NotifyDedupe)
}

/// The repositories the creation flow remembers.
///
/// Returns: `$XDG_DATA_HOME/thurbox/bookmarks.json` or `$HOME/.local/share/thurbox/bookmarks.json`
pub fn bookmarks_file() -> Option<PathBuf> {
    resolve(PathKind::Bookmarks)
}

/// Validate that `name` is a safe single-segment identifier — non-empty,
/// no dot-prefix, no slashes / backslashes / `..`, max 64 chars: a name that
/// becomes an on-disk path.
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

/// The data directory this instance actually resolved — the override when one
/// is set, the XDG default otherwise.
///
/// [`relocated_data_dir`] answers the narrower "was this instance *moved*";
/// this answers "where is it", which is what a consumer comparing against
/// somebody else's recorded data dir needs (runtime-mux's `instance::socket_for`, deciding
/// whether an inherited socket still belongs to this instance).
pub fn data_directory() -> Option<PathBuf> {
    data_app_dir()
}

/// Resolve the multi-repo workspaces directory path.
///
/// Returns: `$XDG_DATA_HOME/thurbox/workspaces/` or
/// `$HOME/.local/share/thurbox/workspaces/`
pub fn workspaces_directory() -> Option<PathBuf> {
    resolve(PathKind::WorkspacesDir)
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
    claude_transcript_path(agent_session_id, config_dir_override).is_some()
}

/// The existing Claude transcript, if this host and account can see it.
pub fn claude_transcript_path(
    agent_session_id: &str,
    config_dir_override: Option<&Path>,
) -> Option<PathBuf> {
    if agent_session_id.contains(['/', '\\']) || agent_session_id.is_empty() {
        return None;
    }
    let root = if let Some(p) = config_dir_override {
        p.to_path_buf()
    } else if let Some(env) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        PathBuf::from(env)
    } else {
        home_dir()?.join(".claude")
    };

    let projects = root.join("projects");
    let Ok(entries) = std::fs::read_dir(&projects) else {
        return None;
    };
    let target = format!("{agent_session_id}.jsonl");
    for entry in entries.flatten() {
        if entry.path().join(&target).is_file() {
            return Some(entry.path().join(&target));
        }
    }
    None
}

/// Override path resolution for all paths to use a custom base directory.
///
/// This is primarily intended for testing. All paths will resolve under the given base:
/// - `config_file()` → `base/config.toml`
/// - `log_directory()` → `base/`
/// - `perf_snapshot_file()` → `base/perf.json`
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

/// The marker that opens an `{env:NAME:-default}` token.
pub const ENV_TOKEN_OPEN: &str = "{env:";

/// `path` with each `{env:NAME:-default}` replaced by `lookup(NAME)`, or by
/// `default` when that is unset or empty — the shell's `${NAME:-default}`.
///
/// An agent can move its config dir with a variable (`CODEX_HOME`), and an
/// account is exactly such a dir (issue #1526), so a hook file's location is
/// only known against the environment the agent runs in. The caller says which:
/// the installer asks this process, a launch asks the session's saved env.
/// Text that is not a complete token is left as written.
pub fn resolve_env_default(path: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(start) = rest.find(ENV_TOKEN_OPEN) {
        let body = &rest[start + ENV_TOKEN_OPEN.len()..];
        let Some(end) = body.find('}') else { break };
        let Some((name, default)) = body[..end].split_once(":-") else {
            break;
        };
        out.push_str(&rest[..start]);
        match lookup(name).filter(|value| !value.is_empty()) {
            Some(value) => out.push_str(&value),
            None => out.push_str(default),
        }
        rest = &body[end + 1..];
    }
    out.push_str(rest);
    out
}

/// The variable each `{env:NAME:-…}` token in `path` reads, in order.
pub fn env_token_names(path: &str) -> Vec<&str> {
    path.match_indices(ENV_TOKEN_OPEN)
        .filter_map(|(start, _)| {
            let body = &path[start + ENV_TOKEN_OPEN.len()..];
            let token = &body[..body.find('}')?];
            Some(token.split_once(":-")?.0)
        })
        .collect()
}

/// `path` respelled with this platform's own separator. On Windows both `/` and
/// `\` separate, so `C:\Users\me\Documents/thurbox` (a `~/…` path expanded onto
/// a Windows home) names the same directory as its all-`\` spelling but stores
/// as different text. Elsewhere the path is returned untouched: a `\` there is a
/// filename character, not a separator.
pub fn native_separators(path: &Path) -> PathBuf {
    if cfg!(windows) {
        path.components().collect()
    } else {
        path.to_path_buf()
    }
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
/// delivered under a directory the caller controls (anything that joins
/// untrusted relative paths onto a base).
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
