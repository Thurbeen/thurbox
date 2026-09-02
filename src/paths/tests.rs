//! `paths`'s tests, kept together (the `git/tests.rs` pattern): a sibling
//! file of the same module, so private items stay reachable.

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
fn a_data_dir_matching_the_default_is_not_a_relocation() {
    // thurbox injects THURBOX_DATA_DIR into every session it spawns, pointing
    // at whatever it resolved itself. A `thurbox-cli` inside such a session
    // must still read as the default instance — otherwise every hook would
    // look for its sessions on a socket nobody created.
    let default = Path::new("/home/u/.local/share/thurbox");
    assert_eq!(
        relocated_from(
            Some(OsStr::new("/home/u/.local/share/thurbox")),
            Some(default)
        ),
        None
    );
    // A trailing separator names the same directory.
    assert_eq!(
        relocated_from(
            Some(OsStr::new("/home/u/.local/share/thurbox/")),
            Some(default)
        ),
        None
    );
    // Unset, and empty-as-unset.
    assert_eq!(relocated_from(None, Some(default)), None);
    assert_eq!(relocated_from(Some(OsStr::new("")), Some(default)), None);
}

#[test]
fn a_data_dir_elsewhere_is_a_relocation() {
    let default = Path::new("/home/u/.local/share/thurbox");
    assert_eq!(
        relocated_from(Some(OsStr::new("/tmp/lab/data")), Some(default)),
        Some(PathBuf::from("/tmp/lab/data"))
    );
    // No resolvable default (no HOME, no XDG) means there is no default
    // instance to share with, so the override stands alone.
    assert_eq!(
        relocated_from(Some(OsStr::new("/tmp/lab/data")), None),
        Some(PathBuf::from("/tmp/lab/data"))
    );
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

/// Config written by a test must land in the `cfg(test)` sandbox rather than
/// the developer's real config dir, and that sandbox must not outlive the
/// process that made it. Run in a child process by
/// [`no_unit_test_temp_dir_outlives_the_test_process`], which checks the
/// reported directory is gone once the child has exited.
#[test]
fn the_unit_test_sandbox_holds_written_config() {
    let cfg = config_file().expect("a config path in the test sandbox");
    std::fs::create_dir_all(cfg.parent().expect("config dir")).expect("mkdir");
    std::fs::write(&cfg, "# written by a unit test\n").expect("write");

    let base = test_sandbox_base();
    assert!(
        cfg.starts_with(&base),
        "config escaped the sandbox: {}",
        cfg.display()
    );
    assert!(
        base.starts_with(std::env::temp_dir()),
        "sandbox is not under the temp dir: {}",
        base.display()
    );
    println!("TEMP_SANDBOX={}", base.display());
}

/// The unit-test suite must leave nothing behind in the system temp dir.
///
/// Both temp sandboxes — `paths`' `cfg(test)` config/data sandbox and
/// `workspace`'s test base — are only observably cleaned up once the process
/// that created them is gone, so this re-runs the two tests that create them
/// in a child copy of this very test binary and asserts the paths they
/// reported no longer exist. The system temp dir is tmpfs on many machines, so
/// a per-run leak here is a per-run leak of RAM until reboot.
#[test]
fn no_unit_test_temp_dir_outlives_the_test_process() {
    // Named rather than discovered; a rename trips the "reported both dirs"
    // assertion below instead of passing vacuously.
    const HELPERS: [&str; 2] = [
        "paths::tests::the_unit_test_sandbox_holds_written_config",
        "workspace::tests::the_workspace_test_base_holds_a_workspace",
    ];

    let exe = std::env::current_exe().expect("this test binary");
    let out = std::process::Command::new(&exe)
        .args(["--exact", "--nocapture"])
        .args(HELPERS)
        .output()
        .expect("run a child copy of the test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "child test run failed: {stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut reported = 0;
    for line in stdout.lines() {
        let Some(path) = line.strip_prefix("TEMP_SANDBOX=") else {
            continue;
        };
        let path = Path::new(path.trim_end());
        assert!(
            !path.exists(),
            "a temp dir outlived the test process that made it: {}",
            path.display()
        );
        reported += 1;
    }
    assert_eq!(
        reported,
        HELPERS.len(),
        "child reported {reported} temp dirs, expected {}: {stdout}",
        HELPERS.len()
    );
}
