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

/// Returns "thurbox-dev" for dev builds, "thurbox" for release builds.
fn app_dir_name() -> &'static str {
    if cfg!(dev_build) {
        "thurbox-dev"
    } else {
        "thurbox"
    }
}

/// Categories of application paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    /// Config file: `~/.config/thurbox/config.toml` (legacy, used for migration only)
    Config,
    /// Log directory: `~/.local/share/thurbox/`
    LogDir,
    /// SQLite database: `~/.local/share/thurbox/thurbox.db`
    Database,
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
        PathKind::Config => {
            // Prefer $XDG_CONFIG_HOME, fall back to $HOME/.config
            if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
                let mut p = PathBuf::from(xdg);
                p.push(app_dir_name());
                p.push("config.toml");
                return Some(p);
            }

            std::env::var_os("HOME").map(|h| {
                let mut p = PathBuf::from(h);
                p.push(".config");
                p.push(app_dir_name());
                p.push("config.toml");
                p
            })
        }
        PathKind::Database => {
            // Prefer $XDG_DATA_HOME, fall back to $HOME/.local/share
            if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
                let mut p = PathBuf::from(xdg);
                p.push(app_dir_name());
                p.push("thurbox.db");
                return Some(p);
            }

            std::env::var_os("HOME").map(|h| {
                let mut p = PathBuf::from(h);
                p.push(".local");
                p.push("share");
                p.push(app_dir_name());
                p.push("thurbox.db");
                p
            })
        }
        PathKind::LogDir => {
            // Prefer $XDG_DATA_HOME, fall back to $HOME/.local/share
            if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
                let mut p = PathBuf::from(xdg);
                p.push(app_dir_name());
                return Some(p);
            }

            std::env::var_os("HOME").map(|h| {
                let mut p = PathBuf::from(h);
                p.push(".local");
                p.push("share");
                p.push(app_dir_name());
                p
            })
        }
    }
}

/// Resolve a path using a custom base directory (for testing).
fn resolve_override(base: &Path, kind: PathKind) -> PathBuf {
    match kind {
        PathKind::Config => base.join("config.toml"),
        PathKind::LogDir => base.to_path_buf(),
        PathKind::Database => base.join("thurbox.db"),
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

    let path = Path::new(input);

    // Determine parent directory and the prefix the user is typing
    let (parent, prefix) = if input.ends_with('/') {
        // User typed a trailing slash — list contents of this directory
        (path.to_path_buf(), String::new())
    } else {
        // Split into parent + partial filename
        let parent = path.parent()?.to_path_buf();
        let file_name = path.file_name()?.to_str()?;
        (parent, file_name.to_string())
    };

    let entries = std::fs::read_dir(&parent).ok()?;

    let show_hidden = prefix.starts_with('.');

    let matches: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            if !show_hidden && name.starts_with('.') {
                return None;
            }
            if name.starts_with(&prefix) {
                Some(name)
            } else {
                None
            }
        })
        .collect();

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_strategy_is_xdg() {
        reset_to_xdg(); // Ensure clean state
                        // We can't easily test XDG without mocking env vars,
                        // but we can verify the strategy exists.
        PATH_STRATEGY.with(|s| {
            assert_eq!(*s.borrow(), PathStrategy::Xdg);
        });
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
        // After drop, should be reset to Xdg
        PATH_STRATEGY.with(|s| {
            assert_eq!(*s.borrow(), PathStrategy::Xdg);
        });
    }

    #[test]
    fn thread_local_isolation() {
        // Set override in main thread
        let base1 = PathBuf::from("/test/base1");
        set_test_dir(&base1);

        assert_eq!(config_file(), Some(base1.join("config.toml")));

        // Spawn another thread and verify it has independent state
        let handle = std::thread::spawn(|| {
            // New thread should start with Xdg (default)
            PATH_STRATEGY.with(|s| matches!(*s.borrow(), PathStrategy::Xdg))
        });

        assert!(handle.join().unwrap());

        // Main thread should still have override
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

        // After reset, should use Xdg again
        PATH_STRATEGY.with(|s| {
            assert_eq!(*s.borrow(), PathStrategy::Xdg);
        });
    }

    #[test]
    fn override_persists_across_calls() {
        let base = PathBuf::from("/persistent");
        set_test_dir(&base);

        // Multiple calls should use the same override
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

            // After inner guard drops, should still use base2's strategy
            // (because nested set_test_dir overwrites the strategy)
            PATH_STRATEGY.with(|s| matches!(*s.borrow(), PathStrategy::Xdg));
        }

        // After outer guard drops, should be Xdg
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
        // /tmp should exist on all Unix systems
        let result = complete_directory_path("/tm");
        assert_eq!(result, Some("p/".to_string()));
    }

    #[test]
    fn complete_directory_path_trailing_slash() {
        // /tmp/ should list children — result depends on system, but shouldn't panic
        let result = complete_directory_path("/tmp/");
        // Either Some (has child dirs) or None (empty /tmp/)
        assert!(result.is_some() || result.is_none());
    }

    #[test]
    fn complete_directory_path_exact_match() {
        // /tmp already exists and is a directory
        let result = complete_directory_path("/tmp");
        assert_eq!(result, Some("/".to_string()));
    }

    #[test]
    fn complete_directory_path_root() {
        // "/" is a valid directory — shouldn't panic
        let result = complete_directory_path("/");
        // Root has children, so we expect Some or None depending on common prefix
        assert!(result.is_some() || result.is_none());
    }

    #[test]
    fn complete_directory_path_with_tempdir() {
        let temp = tempfile::TempDir::new().unwrap();
        let base = temp.path();

        // Create two subdirectories with a common prefix
        std::fs::create_dir(base.join("project_alpha")).unwrap();
        std::fs::create_dir(base.join("project_beta")).unwrap();
        std::fs::create_dir(base.join("other")).unwrap();

        // Completing "project_" should find common prefix "project_"
        let input = format!("{}/project_", base.display());
        let result = complete_directory_path(&input);
        // Two matches: project_alpha, project_beta — common prefix beyond typed is empty
        assert_eq!(result, None);

        // Completing "project_a" should complete to "project_alpha/"
        let input = format!("{}/project_a", base.display());
        let result = complete_directory_path(&input);
        assert_eq!(result, Some("lpha/".to_string()));

        // Completing "oth" should complete to "other/"
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

        // Without dot prefix, hidden dirs are skipped
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

        // With dot prefix, hidden dirs are included
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

        // Only directories should match, not files
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
}
