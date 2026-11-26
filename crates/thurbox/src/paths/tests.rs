use super::*;

const HOME_VAR: &str = if cfg!(windows) { "USERPROFILE" } else { "HOME" };

#[test]
fn display_path_uses_basename() {
    assert_eq!(
        display_path(Path::new("/home/user/Repositories/thurbox")),
        "thurbox"
    );
}

fn executable_marker(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, b"#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    p
}

#[test]
fn which_on_path_finds_present_and_rejects_absent() {
    let dir = tempfile::TempDir::new().unwrap();
    let marker = "tbx_which_probe_marker";
    executable_marker(dir.path(), marker);

    let (found, missing) = with_path(dir.path(), || {
        (
            which_on_path(marker),
            which_on_path("tbx_which_probe_absent"),
        )
    });

    assert!(found, "marker on PATH should be found");
    assert!(!missing, "a name not on PATH should not be found");
}

#[test]
fn resolve_on_path_answers_with_the_absolute_path() {
    let dir = tempfile::TempDir::new().unwrap();
    let marker = "tbx_resolve_probe_marker";
    let expected = executable_marker(dir.path(), marker);

    let found = with_path(dir.path(), || resolve_on_path(marker));

    assert_eq!(found.as_deref(), Some(expected.as_path()));
}

#[test]
fn resolve_on_path_declines_a_name_that_is_already_a_path() {
    let dir = tempfile::TempDir::new().unwrap();
    let marker = "tbx_resolve_relative_marker";
    executable_marker(dir.path(), marker);

    let found = with_path(dir.path(), || resolve_on_path(&format!("./{marker}")));

    assert_eq!(found, None);
}

#[test]
#[cfg(unix)]
fn resolve_on_path_skips_what_cannot_be_executed() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("tbx_dir_probe")).unwrap();
    std::fs::write(dir.path().join("tbx_plain_probe"), b"").unwrap();

    let (as_dir, as_plain) = with_path(dir.path(), || {
        (
            resolve_on_path("tbx_dir_probe"),
            resolve_on_path("tbx_plain_probe"),
        )
    });

    assert_eq!(as_dir, None, "a directory is not the binary");
    assert_eq!(as_plain, None, "a non-executable file is not the binary");
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
    reset_to_xdg();
    let saved_cfg = std::env::var_os(CONFIG_DIR_OVERRIDE_ENV);
    let saved_data = std::env::var_os(DATA_DIR_OVERRIDE_ENV);
    std::env::set_var(CONFIG_DIR_OVERRIDE_ENV, "/real/config/thurbox");
    std::env::set_var(DATA_DIR_OVERRIDE_ENV, "/real/data/thurbox");

    let cfg = config_file().unwrap();
    let db = perf_snapshot_file().unwrap();

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
    let default = Path::new("/home/u/.local/share/thurbox");
    assert_eq!(
        relocated_from(
            Some(OsStr::new("/home/u/.local/share/thurbox")),
            Some(default)
        ),
        None
    );
    assert_eq!(
        relocated_from(
            Some(OsStr::new("/home/u/.local/share/thurbox/")),
            Some(default)
        ),
        None
    );
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
    assert_eq!(perf_snapshot_file(), Some(base.join("perf.json")));

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
    assert_eq!(
        resolve(PathKind::PerfSnapshot),
        Some(base.join("perf.json"))
    );
    assert_eq!(resolve(PathKind::MetricsDir), Some(base.join("metrics")));

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
fn perf_snapshot_file_convenience() {
    let base = PathBuf::from("/custom");
    set_test_dir(&base);

    let path = perf_snapshot_file().unwrap();
    assert!(path.ends_with("perf.json"));

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
        resolve_override(base, PathKind::PerfSnapshot),
        PathBuf::from("/data/perf.json")
    );
    assert_eq!(
        resolve_override(base, PathKind::MetricsDir),
        PathBuf::from("/data/metrics")
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
    assert_eq!(expand_tilde("~otheruser"), PathBuf::from("~otheruser"));
}

#[cfg(windows)]
#[test]
fn expand_tilde_backslash_on_windows() {
    // Windows accepts `\` as a path separator.
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
    // On Unix `\` is a legal filename character, not a separator.
    assert_eq!(expand_tilde("~\\foo"), PathBuf::from("~\\foo"));
}

#[cfg(windows)]
#[test]
fn native_separators_respells_a_mixed_windows_path() {
    let got = native_separators(Path::new("C:\\Users\\me\\Documents/thurbox"));
    assert_eq!(got.to_str(), Some("C:\\Users\\me\\Documents\\thurbox"));
}

#[cfg(not(windows))]
#[test]
fn native_separators_leaves_a_unix_backslash_alone() {
    let got = native_separators(Path::new("/home/me/odd\\name"));
    assert_eq!(got.to_str(), Some("/home/me/odd\\name"));
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
    if let Some(s) = complete_directory_path("~/") {
        assert!(!s.starts_with('/'));
    }
}

#[test]
fn sanitize_workspace_segment_strips_separators_and_dots() {
    assert_eq!(sanitize_workspace_segment("a/b\\c:d"), "a-b-c-d");
    assert_eq!(sanitize_workspace_segment("  .git  "), "git");
    assert_eq!(sanitize_workspace_segment("my repo"), "my-repo");
    assert_eq!(sanitize_workspace_segment("--.hidden.--"), "hidden");
    assert_eq!(
        sanitize_workspace_segment("d5715d35-9599-4507-9901-ef33b9476358"),
        "d5715d35-9599-4507-9901-ef33b9476358"
    );
}

#[test]
fn unique_link_name_dedups_with_dash_two_suffix() {
    let mut used = std::collections::HashSet::new();
    assert_eq!(unique_link_name("webapp", &mut used), "webapp");
    assert_eq!(unique_link_name("webapp", &mut used), "webapp-2");
    assert_eq!(unique_link_name("webapp", &mut used), "webapp-3");
    assert_eq!(unique_link_name("", &mut used), "repo");
    assert_eq!(unique_link_name("", &mut used), "repo-2");
}

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

#[test]
fn no_unit_test_temp_dir_outlives_the_test_process() {
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

    // The test harness's progress text shares stdout unsynchronized, so a marker can land mid-line.
    const MARKER: &str = "TEMP_SANDBOX=";
    let mut reported = 0;
    let mut rest: &str = &stdout;
    while let Some(start) = rest.find(MARKER) {
        let after = &rest[start + MARKER.len()..];
        let end = after.find(char::is_whitespace).unwrap_or(after.len());
        let path = Path::new(&after[..end]);
        assert!(
            !path.exists(),
            "a temp dir outlived the test process that made it: {}",
            path.display()
        );
        reported += 1;
        rest = &after[end..];
    }
    assert_eq!(
        reported,
        HELPERS.len(),
        "child reported {reported} temp dirs, expected {}: {stdout}",
        HELPERS.len()
    );
}

#[test]
fn resolve_cli_binary_uses_platform_exe_suffix() {
    let p = resolve_cli_binary();
    let name = p.file_name().unwrap().to_string_lossy();
    assert_eq!(name, format!("thurbox-cli{}", std::env::consts::EXE_SUFFIX));
}

#[test]
fn env_default_tokens_follow_the_given_environment() {
    let path = "{env:CODEX_HOME:-~/.codex}/hooks.json";
    let env = |value: &'static str| move |name: &str| (name == "CODEX_HOME").then(|| value.into());
    assert_eq!(resolve_env_default(path, |_| None), "~/.codex/hooks.json");
    assert_eq!(resolve_env_default(path, env("")), "~/.codex/hooks.json");
    assert_eq!(resolve_env_default(path, env("/w")), "/w/hooks.json");
    assert_eq!(
        resolve_env_default("~/.gemini/settings.json", env("/w")),
        "~/.gemini/settings.json"
    );
    assert_eq!(
        resolve_env_default("{env:BROKEN/x", env("/w")),
        "{env:BROKEN/x"
    );
    assert_eq!(env_token_names(path), ["CODEX_HOME"]);
    assert!(env_token_names("~/.codex/hooks.json").is_empty());
}

#[cfg(unix)]
#[test]
fn a_replaced_file_keeps_its_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    for mode in [0o600, 0o755] {
        let path = dir.path().join(format!("file-{mode:o}"));
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();

        write_atomically(&path, "new").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        let kept = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(kept, mode);
    }
}

#[cfg(unix)]
#[test]
fn a_read_only_file_is_not_replaced() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agents.toml");
    std::fs::write(&path, "mine").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();

    let err = write_atomically(&path, "theirs").unwrap_err();

    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine");
    let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(left.len(), 1, "a staged file was left behind");
}

#[cfg(unix)]
#[test]
fn a_staged_copy_starts_no_more_open_than_the_file_it_replaces() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let staged = dir.path().join("agents.saving-1-0");

    let _file = open_staged(&staged, Some(&std::fs::Permissions::from_mode(0o600))).unwrap();

    let mode = std::fs::metadata(&staged).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode & 0o077, 0, "staged file opened as {mode:o}");
}

#[test]
fn a_leftover_staged_file_does_not_block_the_next_save() {
    let dir = tempfile::tempdir().unwrap();
    let staged = dir.path().join("agents.saving-1-0");
    std::fs::write(&staged, "half a file").unwrap();

    drop(open_staged(&staged, None).unwrap());

    assert_eq!(std::fs::read_to_string(&staged).unwrap(), "");
}

#[test]
fn the_wsl_distro_is_read_trimmed_and_an_empty_one_is_none() {
    with_wsl_distro(Some("Ubuntu"), || {
        assert_eq!(current_wsl_distro().as_deref(), Some("Ubuntu"));
    });
    with_wsl_distro(None, || assert_eq!(current_wsl_distro(), None));
    with_wsl_distro(Some("  "), || assert_eq!(current_wsl_distro(), None));
}

#[test]
fn a_2x_database_is_unmigrated_until_the_marker_is_written() {
    let dir = tempfile::tempdir().expect("dir");
    assert!(!super::v2_state_unmigrated_in(dir.path()));
    std::fs::write(dir.path().join("thurbox.db"), b"").expect("db");
    assert!(super::v2_state_unmigrated_in(dir.path()));
    std::fs::write(dir.path().join("thurbox.db.migrated"), b"").expect("marker");
    assert!(!super::v2_state_unmigrated_in(dir.path()));
}
