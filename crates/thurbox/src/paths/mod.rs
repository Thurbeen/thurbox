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

pub const CONFIG_DIR_OVERRIDE_ENV: &str = "THURBOX_CONFIG_DIR";
pub const DATA_DIR_OVERRIDE_ENV: &str = "THURBOX_DATA_DIR";

pub fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).map(PathBuf::from)
}

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

#[cfg(test)]
fn test_sandbox_base() -> PathBuf {
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

#[cfg(not(test))]
fn config_app_dir() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os(CONFIG_DIR_OVERRIDE_ENV).filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(x));
    }
    Some(config_base()?.join(app_dir_name()))
}

#[cfg(test)]
fn config_app_dir() -> Option<PathBuf> {
    Some(test_sandbox_base().join("config"))
}

#[cfg(not(test))]
fn data_app_dir() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os(DATA_DIR_OVERRIDE_ENV).filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(x));
    }
    Some(data_base()?.join(app_dir_name()))
}

#[cfg(test)]
fn data_app_dir() -> Option<PathBuf> {
    Some(test_sandbox_base().join("data"))
}

#[cfg(not(test))]
pub fn relocated_data_dir() -> Option<PathBuf> {
    let over = std::env::var_os(DATA_DIR_OVERRIDE_ENV);
    let default = data_base().map(|b| b.join(app_dir_name()));
    relocated_from(over.as_deref(), default.as_deref())
}

#[cfg(test)]
pub fn relocated_data_dir() -> Option<PathBuf> {
    None
}

fn relocated_from(override_dir: Option<&OsStr>, default_dir: Option<&Path>) -> Option<PathBuf> {
    let dir = PathBuf::from(override_dir.filter(|s| !s.is_empty())?);
    (Some(dir.as_path()) != default_dir).then_some(dir)
}

fn xdg_config_subpath(filename: &str) -> Option<PathBuf> {
    Some(config_app_dir()?.join(filename))
}

fn xdg_data_subpath(segments: &[&str]) -> Option<PathBuf> {
    let mut p = data_app_dir()?;
    for seg in segments {
        p.push(seg);
    }
    Some(p)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    Config,
    LogDir,
    PerfSnapshot,
    Bookmarks,
    NotifyDedupe,
    MetricsDir,
    WorkspacesDir,
}

#[derive(Debug, PartialEq)]
enum PathStrategy {
    Xdg,
    Override(PathBuf),
}

thread_local! {
    static PATH_STRATEGY: RefCell<PathStrategy> = const { RefCell::new(PathStrategy::Xdg) };
}

pub fn resolve(kind: PathKind) -> Option<PathBuf> {
    PATH_STRATEGY.with(|strategy| {
        let s = strategy.borrow();
        match *s {
            PathStrategy::Xdg => resolve_xdg(kind),
            PathStrategy::Override(ref base) => Some(resolve_override(base, kind)),
        }
    })
}

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

pub fn config_file() -> Option<PathBuf> {
    resolve(PathKind::Config)
}

pub fn log_directory() -> Option<PathBuf> {
    resolve(PathKind::LogDir)
}

pub fn perf_snapshot_file() -> Option<PathBuf> {
    resolve(PathKind::PerfSnapshot)
}

pub fn notify_dedupe_file() -> Option<PathBuf> {
    resolve(PathKind::NotifyDedupe)
}

pub fn bookmarks_file() -> Option<PathBuf> {
    resolve(PathKind::Bookmarks)
}

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

pub fn metrics_directory() -> Option<PathBuf> {
    resolve(PathKind::MetricsDir)
}

pub fn data_directory() -> Option<PathBuf> {
    data_app_dir()
}

pub fn v2_migrated_marker() -> Option<PathBuf> {
    data_directory().map(|dir| dir.join("thurbox.db.migrated"))
}

pub fn v2_state_unmigrated() -> bool {
    data_directory().is_some_and(|dir| v2_state_unmigrated_in(&dir))
}

pub fn v2_state_unmigrated_in(dir: &Path) -> bool {
    dir.join("thurbox.db").is_file() && !dir.join("thurbox.db.migrated").exists()
}

pub fn workspaces_directory() -> Option<PathBuf> {
    resolve(PathKind::WorkspacesDir)
}

pub fn claude_transcript_exists(
    agent_session_id: &str,
    config_dir_override: Option<&Path>,
) -> bool {
    claude_transcript_path(agent_session_id, config_dir_override).is_some()
}

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

pub fn set_test_dir(base: impl Into<PathBuf>) {
    PATH_STRATEGY.with(|strategy| {
        *strategy.borrow_mut() = PathStrategy::Override(base.into());
    });
}

pub fn reset_to_xdg() {
    PATH_STRATEGY.with(|strategy| {
        *strategy.borrow_mut() = PathStrategy::Xdg;
    });
}

pub struct TestPathGuard;

impl TestPathGuard {
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

pub const ENV_TOKEN_OPEN: &str = "{env:";

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

pub fn env_token_names(path: &str) -> Vec<&str> {
    path.match_indices(ENV_TOKEN_OPEN)
        .filter_map(|(start, _)| {
            let body = &path[start + ENV_TOKEN_OPEN.len()..];
            let token = &body[..body.find('}')?];
            Some(token.split_once(":-")?.0)
        })
        .collect()
}

pub fn native_separators(path: &Path) -> PathBuf {
    if cfg!(windows) {
        path.components().collect()
    } else {
        path.to_path_buf()
    }
}

fn strip_tilde_prefix(path: &str) -> Option<&str> {
    if let Some(rest) = path.strip_prefix("~/") {
        return Some(rest);
    }
    if cfg!(windows) {
        return path.strip_prefix("~\\");
    }
    None
}

pub fn display_path(path: &Path) -> String {
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => path.display().to_string(),
    }
}

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

pub fn safe_join(base: &Path, rel: &str) -> Result<PathBuf, String> {
    ensure_safe_relative(rel)?;
    Ok(base.join(rel))
}

#[cfg(test)]
mod tests;
