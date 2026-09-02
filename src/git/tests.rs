//! `git`'s tests, kept together.
//!
//! They were one module before the file was split and they stay one: many
//! exercise a parser against the *pair* of scripts that feed it (POSIX and
//! PowerShell), which is a claim about two files agreeing, and splitting them
//! by which file happens to hold the parser would hide that.

use super::*;
use crate::paths::TestPathGuard;

#[test]
fn git_program_scrubs_inherited_location_env() {
    // Git exports these to hook processes; git_program must mark every one
    // for removal so an inherited hook environment can't redirect a
    // path-targeted git call to the wrong repo (the bug that corrupted the
    // index when the suite ran under the pre-commit `cargo nextest` hook).
    let cmd = git_program();
    let removed: std::collections::HashSet<&str> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.to_str().unwrap())
        .collect();
    for var in GIT_LOCATION_ENV {
        assert!(removed.contains(var), "git_program must scrub {var}");
    }
}

/// A throwaway repository with one commit, to clone from.
fn seed_repo(dir: &Path, file: &str, contents: &str) -> String {
    std::fs::create_dir_all(dir.parent().unwrap_or(dir)).ok();
    run_git(
        {
            let mut c = git_program();
            c.arg("init").arg("--initial-branch=main").arg(dir);
            c
        },
        "git init",
    )
    .expect("init");
    let path = dir.join(file);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, contents).expect("write");
    for args in [
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "user.name", "T"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["add", "-A"],
        vec!["commit", "-m", "seed"],
    ] {
        run_git(git_command(None, dir, &args), "git").expect("seed");
    }
    head_commit(dir).expect("head")
}

/// A clone delivers whatever the repository holds — including bytes no text
/// path could carry — and reports the commit, not the ref.
#[test]
fn cloning_a_plugin_delivers_its_files_and_reports_the_commit() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let origin = tmp.path().join("origin");
    // A pane in a nested directory, and a non-UTF-8 payload beside it: the exact
    // pair the text fetch path corrupts.
    let commit = seed_repo(&origin, "plugins/40_x.lua", "return {}\n");
    std::fs::write(origin.join("payload.bin"), [0x00u8, 0xff, 0xfe, 0x01]).expect("write");
    for args in [vec!["add", "-A"], vec!["commit", "-m", "payload"]] {
        run_git(git_command(None, &origin, &args), "git").expect("commit");
    }
    let commit = {
        let _ = commit;
        head_commit(&origin).expect("head")
    };

    let dest = tmp.path().join("ui").join("x");
    clone_plugin(&origin.to_string_lossy(), &dest, None).expect("clone");

    assert_eq!(
        std::fs::read(dest.join("payload.bin")).expect("read"),
        [0x00u8, 0xff, 0xfe, 0x01],
        "the bytes survive, which is the whole point of a clone"
    );
    assert!(dest.join("plugins/40_x.lua").is_file());
    assert!(is_working_copy(&dest), "the clone keeps its .git");
    assert_eq!(head_commit(&dest).expect("head"), commit);
    assert!(!is_dirty(&dest).expect("status"));
}

#[test]
fn cloning_over_something_that_exists_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path().join("taken");
    std::fs::create_dir_all(&dest).expect("mkdir");
    std::fs::write(dest.join("mine.lua"), "return {}").expect("write");
    let error = clone_plugin("https://example.com/x.git", &dest, None).expect_err("should refuse");
    assert!(error.to_string().contains("already exists"), "{error}");
    assert!(dest.join("mine.lua").is_file(), "and leaves it alone");
}

/// The property that makes git the right owner of "your edits are yours".
#[test]
fn an_edited_working_copy_reports_itself_dirty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let origin = tmp.path().join("origin");
    seed_repo(&origin, "plugins/40_x.lua", "return {}\n");
    let dest = tmp.path().join("clone");
    clone_plugin(&origin.to_string_lossy(), &dest, None).expect("clone");

    assert!(!is_dirty(&dest).expect("clean"));
    std::fs::write(dest.join("plugins/40_x.lua"), "-- mine\n").expect("edit");
    assert!(
        is_dirty(&dest).expect("dirty"),
        "an edit has to be visible, or nothing can refuse to overwrite it"
    );
}

/// A clone must never be able to sit waiting for a passphrase: an install runs
/// from the command drain, so a prompt is a frozen interface.
#[test]
fn git_operations_refuse_to_prompt() {
    let mut cmd = git_program();
    non_interactive(&mut cmd);
    let envs: std::collections::HashMap<String, String> = cmd
        .get_envs()
        .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
        .collect();
    assert_eq!(
        envs.get("GIT_TERMINAL_PROMPT").map(String::as_str),
        Some("0")
    );
    let ssh = envs.get("GIT_SSH_COMMAND").expect("GIT_SSH_COMMAND");
    assert!(ssh.contains("BatchMode=yes"), "{ssh}");
    assert!(ssh.contains("ConnectTimeout"), "{ssh}");
}

#[test]
fn parse_numstat_sums_changes() {
    let (files, ins, dels) = parse_numstat("1\t2\tfile.rs\n3\t4\tother.rs\n-\t-\tbin.png\n");
    assert_eq!(files, 3);
    assert_eq!(ins, 4);
    assert_eq!(dels, 6);
}

#[test]
fn parse_numstat_empty_is_zero() {
    assert_eq!(parse_numstat(""), (0, 0, 0));
}

#[test]
fn split_untracked_diff_separates_numstat_from_patch() {
    let combined = "2\t0\tnotes.md\0diff --git a/notes.md b/notes.md\n\
                    new file mode 100644\n--- /dev/null\n+++ b/notes.md\n\
                    @@ -0,0 +1,2 @@\n+one\n+two\n";
    let (counts, patch) = split_untracked_diff(combined);
    assert_eq!(counts, "2\t0\tnotes.md\0");
    assert!(patch.starts_with("diff --git a/notes.md"));
    assert!(patch.ends_with("+two\n"));
}

#[test]
fn split_untracked_diff_ignores_the_marker_inside_a_path() {
    // A path containing the literal header text must not split the output
    // early: only a `diff --git` at the start or after a terminator counts.
    let combined = "1\t0\tdiff --git trap.txt\0diff --git a/x b/x\n+hi\n";
    let (counts, patch) = split_untracked_diff(combined);
    assert_eq!(counts, "1\t0\tdiff --git trap.txt\0");
    assert_eq!(patch, "diff --git a/x b/x\n+hi\n");
}

#[test]
fn split_untracked_diff_empty_and_patchless_inputs() {
    // A vanished file produces no output at all.
    assert_eq!(split_untracked_diff(""), ("", ""));
    // Stat records with no patch stay whole on the numstat side.
    assert_eq!(split_untracked_diff("1\t0\ta.txt\0"), ("1\t0\ta.txt\0", ""));
    // A bare patch (no --numstat) is all patch.
    let (counts, patch) = split_untracked_diff("diff --git a/x b/x\n+hi\n");
    assert_eq!(counts, "");
    assert!(patch.starts_with("diff --git"));
}

#[test]
fn parse_status_v2_reads_ab_header_and_counts_entries() {
    let out = "# branch.oid 1234abcd\n\
               # branch.head feat/x\n\
               # branch.upstream origin/feat/x\n\
               # branch.ab +3 -1\n\
               1 .M N... 100644 100644 100644 aaaa bbbb src/lib.rs\n\
               2 R. N... 100644 100644 100644 aaaa bbbb R100 new.rs\told.rs\n\
               u UU N... 100644 100644 100644 100644 aaaa bbbb cccc conflicted.rs\n\
               ? scratch.txt\n\
               ? notes.md\n";
    let status = parse_status_v2(out);
    assert!(status.dirty);
    assert_eq!(status.untracked, 2);
    assert_eq!(status.ahead_behind, Some((3, 1)));
}

#[test]
fn parse_status_v2_clean_repo_with_upstream_in_sync() {
    let out = "# branch.oid 1234abcd\n\
               # branch.head main\n\
               # branch.upstream origin/main\n\
               # branch.ab +0 -0\n";
    let status = parse_status_v2(out);
    // Headers alone are not dirt.
    assert!(!status.dirty);
    assert_eq!(status.untracked, 0);
    assert_eq!(status.ahead_behind, Some((0, 0)));
}

#[test]
fn parse_status_v2_without_upstream_reports_no_ab() {
    // No upstream configured (or its ref gone): git omits `# branch.ab`, and
    // the caller must fall back to the resolve_base_ref probe chain.
    let out = "# branch.oid 1234abcd\n\
               # branch.head detached\n\
               ? scratch.txt\n";
    let status = parse_status_v2(out);
    assert!(status.dirty);
    assert_eq!(status.untracked, 1);
    assert_eq!(status.ahead_behind, None);
}

#[test]
fn scan_child_repos_finds_git_subdirs_sorted_skipping_others() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Two git repos (.git dir), one plain dir, one hidden git dir.
    for name in ["beta", "alpha"] {
        std::fs::create_dir_all(root.join(name).join(".git")).unwrap();
    }
    std::fs::create_dir_all(root.join("plain")).unwrap();
    std::fs::create_dir_all(root.join(".hidden").join(".git")).unwrap();

    let repos = scan_child_repos(root);
    assert_eq!(repos, vec![root.join("alpha"), root.join("beta")]);
}

#[test]
fn scan_child_repos_detects_git_file_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let wt = root.join("worktree");
    std::fs::create_dir_all(&wt).unwrap();
    // Worktree checkouts use a `.git` *file*, not a directory.
    std::fs::write(wt.join(".git"), "gitdir: /somewhere\n").unwrap();

    assert!(is_git_repo(&wt));
    assert_eq!(scan_child_repos(root), vec![wt]);
}

#[test]
fn scan_child_repos_missing_parent_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(scan_child_repos(&tmp.path().join("nope")).is_empty());
}

#[test]
fn create_or_attach_worktree_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        let ok = git_program()
            .args(args)
            .current_dir(&repo)
            .output()
            .expect("run git")
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("file.txt"), "hi").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);
    let base = String::from_utf8(
        git_program()
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    let _guard = TestPathGuard::new(tmp.path().join("data"));

    let p1 = create_or_attach_worktree(&repo, "feat/x", &base).expect("first creates");
    assert!(p1.exists());
    let p2 = create_or_attach_worktree(&repo, "feat/x", &base).expect("second reuses");
    assert_eq!(p1, p2);
    // Worktree dir gone but branch remains: re-attach a worktree to it.
    remove_worktree(&repo, &p1).expect("remove worktree");
    let p3 = create_or_attach_worktree(&repo, "feat/x", &base).expect("third reattaches");
    assert_eq!(p1, p3);
    assert!(p3.exists());
}

/// Compute the 16-char hex repo hash used in worktree paths.
fn repo_hash(repo_path: &Path) -> String {
    stable_repo_hash(&repo_path.display().to_string())
}

#[test]
fn worktree_path_simple_branch() {
    let base = PathBuf::from("/test/data");
    let _guard = TestPathGuard::new(&base);
    let repo = Path::new("/home/user/repo");
    let result = worktree_path(repo, "main").unwrap();
    let hash = repo_hash(repo);
    assert_eq!(result, base.join("worktrees").join(&hash).join("main"));
}

#[test]
fn worktree_path_slash_branch() {
    let base = PathBuf::from("/test/data");
    let _guard = TestPathGuard::new(&base);
    let repo = Path::new("/home/user/repo");
    let result = worktree_path(repo, "feature/foo").unwrap();
    let hash = repo_hash(repo);
    assert_eq!(
        result,
        base.join("worktrees").join(&hash).join("feature-foo")
    );
}

#[test]
fn worktree_path_nested_slashes() {
    let base = PathBuf::from("/test/data");
    let _guard = TestPathGuard::new(&base);
    let repo = Path::new("/home/user/repo");
    let result = worktree_path(repo, "feature/team/task").unwrap();
    let hash = repo_hash(repo);
    assert_eq!(
        result,
        base.join("worktrees").join(&hash).join("feature-team-task")
    );
}

#[test]
fn worktree_path_no_slashes_unchanged() {
    let base = PathBuf::from("/test/data");
    let _guard = TestPathGuard::new(&base);
    let repo = Path::new("/repo");
    let result = worktree_path(repo, "my-branch").unwrap();
    let hash = repo_hash(repo);
    assert_eq!(result, base.join("worktrees").join(&hash).join("my-branch"));
}

#[test]
fn worktree_path_trailing_slash() {
    let base = PathBuf::from("/test/data");
    let _guard = TestPathGuard::new(&base);
    let repo = Path::new("/repo");
    let result = worktree_path(repo, "branch/").unwrap();
    let hash = repo_hash(repo);
    assert_eq!(result, base.join("worktrees").join(&hash).join("branch-"));
}

#[test]
fn worktree_path_leading_slash() {
    let base = PathBuf::from("/test/data");
    let _guard = TestPathGuard::new(&base);
    let repo = Path::new("/repo");
    let result = worktree_path(repo, "/branch").unwrap();
    let hash = repo_hash(repo);
    assert_eq!(result, base.join("worktrees").join(&hash).join("-branch"));
}

#[test]
fn worktree_path_different_repos_produce_different_hashes() {
    let base = PathBuf::from("/test/data");
    let _guard = TestPathGuard::new(&base);
    let path_a = worktree_path(Path::new("/repo/a"), "main").unwrap();
    let path_b = worktree_path(Path::new("/repo/b"), "main").unwrap();
    assert_ne!(path_a, path_b);
    assert_eq!(path_a.file_name(), path_b.file_name());
}

#[test]
fn worktree_path_same_repo_is_deterministic() {
    let base = PathBuf::from("/test/data");
    let _guard = TestPathGuard::new(&base);
    let repo = Path::new("/home/user/repo");
    let first = worktree_path(repo, "main").unwrap();
    let second = worktree_path(repo, "main").unwrap();
    assert_eq!(first, second);
}

#[test]
fn stable_repo_hash_is_pinned_fnv1a() {
    // Pin the exact output so a future swap back to a non-stable hasher
    // (e.g. DefaultHasher/SipHash, whose digest varies across builds) is
    // caught: these are the canonical FNV-1a 64-bit hashes for the inputs.
    assert_eq!(stable_repo_hash(""), "cbf29ce484222325");
    assert_eq!(stable_repo_hash("a"), "af63dc4c8601ec8c");
    assert_eq!(stable_repo_hash("/home/user/repo"), "96e5ae60e8caf52a");
}

#[test]
fn resolve_base_ref_none_without_remote() {
    // A repo with no upstream and no remote refs resolves to no base ref,
    // so sync surfaces an error rather than rebasing onto a missing
    // `origin/main`.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let git = |args: &[&str]| {
        let out = git_program()
            .args(args)
            .current_dir(repo)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("file.txt"), "hi").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);

    assert_eq!(resolve_base_ref(None, repo), None);
}

#[test]
fn resolve_base_ref_prefers_upstream() {
    // A branch tracking an upstream resolves to `@{upstream}`, ahead of the
    // origin/HEAD and origin/main fallbacks.
    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("remote.git");
    let work = tmp.path().join("work");
    let run = |dir: &Path, args: &[&str]| {
        let out = git_program()
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    // Bare remote to push to.
    std::fs::create_dir_all(&remote).unwrap();
    run(&remote, &["init", "-q", "--bare"]);

    // Working repo with one commit, pushed with upstream tracking (`-u`).
    std::fs::create_dir_all(&work).unwrap();
    run(&work, &["init", "-q"]);
    run(&work, &["config", "user.email", "t@example.com"]);
    run(&work, &["config", "user.name", "t"]);
    run(&work, &["config", "commit.gpgsign", "false"]);
    std::fs::write(work.join("file.txt"), "hi").unwrap();
    run(&work, &["add", "."]);
    run(&work, &["commit", "-qm", "init"]);
    run(
        &work,
        &["remote", "add", "origin", &remote.display().to_string()],
    );
    run(&work, &["push", "-q", "-u", "origin", "HEAD"]);

    assert_eq!(
        resolve_base_ref(None, &work),
        Some("@{upstream}".to_string())
    );
}

#[test]
fn default_branch_prefers_main_over_master() {
    let branches = vec![
        "develop".to_string(),
        "master".to_string(),
        "main".to_string(),
    ];
    // Uses a non-existent path so the git command fails, exercising the fallback.
    let result = default_branch(Path::new("/nonexistent"), &branches);
    assert_eq!(result, Some("main".to_string()));
}

#[test]
fn default_branch_falls_back_to_master() {
    let branches = vec!["develop".to_string(), "master".to_string()];
    let result = default_branch(Path::new("/nonexistent"), &branches);
    assert_eq!(result, Some("master".to_string()));
}

#[test]
fn default_branch_returns_none_when_no_candidates() {
    let branches = vec!["develop".to_string(), "feature".to_string()];
    let result = default_branch(Path::new("/nonexistent"), &branches);
    assert_eq!(result, None);
}

#[test]
fn default_branch_returns_none_for_empty_branches() {
    let result = default_branch(Path::new("/nonexistent"), &[]);
    assert_eq!(result, None);
}

#[test]
fn transient_error_detects_could_not_write_index() {
    assert!(is_transient_error("error: could not write index"));
}

#[test]
fn transient_error_detects_unable_to_write_new_index() {
    assert!(is_transient_error("fatal: Unable to write new index file"));
}

#[test]
fn transient_error_detects_index_lock_exists() {
    assert!(is_transient_error(
        "fatal: Unable to create '/repo/.git/index.lock': File exists."
    ));
}

#[test]
fn transient_error_detects_another_git_process() {
    assert!(is_transient_error(
        "Another git process seems to be running in this repository"
    ));
}

#[test]
fn transient_error_rejects_auth_failure() {
    assert!(!is_transient_error(
        "fatal: Authentication failed for 'https://github.com/repo.git'"
    ));
}

#[test]
fn transient_error_rejects_merge_conflict() {
    assert!(!is_transient_error(
        "CONFLICT (content): Merge conflict in src/main.rs"
    ));
}

#[test]
fn transient_error_rejects_empty_string() {
    assert!(!is_transient_error(""));
}

#[test]
fn transient_error_matches_within_anyhow_chain() {
    // is_transient_error is called with format!("{e:#}") which includes anyhow context
    assert!(is_transient_error(
        "git stash failed: could not write index"
    ));
    assert!(is_transient_error(
        "git stash failed: fatal: Unable to create '/repo/.git/index.lock': File exists."
    ));
}

#[test]
fn try_remove_by_age_removes_old_lock() {
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join("index.lock");
    std::fs::write(&lock, "").unwrap();

    // Backdate the file's mtime to exceed STALE_LOCK_AGE
    let old_time = std::time::SystemTime::now() - Duration::from_secs(120);
    let times = std::fs::FileTimes::new().set_modified(old_time);
    let file = std::fs::File::options().write(true).open(&lock).unwrap();
    file.set_times(times).unwrap();

    try_remove_by_age(&lock);
    assert!(!lock.exists(), "old lock should have been removed");
}

#[test]
fn try_remove_by_age_preserves_fresh_lock() {
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join("index.lock");
    std::fs::write(&lock, "").unwrap();

    try_remove_by_age(&lock);
    assert!(lock.exists(), "fresh lock should be preserved");
}

// ── remote / ssh helpers ────────────────────────────────────────

fn host(dest: &str, wt_dir: Option<&str>) -> HostDef {
    HostDef {
        name: "h".into(),
        destination: dest.into(),
        ssh_opts: vec!["-o".into(), "ControlMaster=auto".into()],
        worktrees_dir: wt_dir.map(|s| s.to_string()),
        ..Default::default()
    }
}

fn program_and_args(cmd: &Command) -> (String, Vec<String>) {
    (
        cmd.get_program().to_string_lossy().into_owned(),
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect(),
    )
}

#[test]
fn git_command_local_uses_git_with_args() {
    let cmd = git_command(None, Path::new("/repo"), &["branch", "--list"]);
    let (prog, args) = program_and_args(&cmd);
    assert_eq!(prog, "git");
    assert_eq!(args, ["branch", "--list"]);
    assert_eq!(cmd.get_current_dir(), Some(Path::new("/repo")));
}

#[test]
fn git_command_remote_wraps_in_ssh() {
    let h = host("me@box", None);
    let cmd = git_command(
        Some(&h),
        Path::new("/srv/repo"),
        &["worktree", "add", "-b", "x"],
    );
    let (prog, args) = program_and_args(&cmd);
    assert_eq!(prog, "ssh");
    // User ssh_opts first, then the always-appended set (fail-fast hardening
    // plus multiplexing when the machine has an `~/.ssh` —
    // crate::shell::ssh_appended_opts), then destination + remote git.
    let mut expected: Vec<String> = vec!["-o".into(), "ControlMaster=auto".into()];
    expected.extend(
        crate::shell::ssh_appended_opts()
            .iter()
            .map(|s| s.to_string()),
    );
    expected.extend(
        [
            "me@box",
            "git",
            "-C",
            "/srv/repo",
            "worktree",
            "add",
            "-b",
            "x",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    assert_eq!(args, expected);
    // No local current_dir is set for the remote variant.
    assert_eq!(cmd.get_current_dir(), None);
}

#[test]
fn git_command_wsl_wraps_in_wsl_exe() {
    let h = HostDef::wsl("Ubuntu");
    let cmd = git_command(
        Some(&h),
        Path::new("/home/me/repo"),
        &["worktree", "add", "-b", "x"],
    );
    let (prog, args) = program_and_args(&cmd);
    assert_eq!(prog, "wsl.exe");
    // A Unix caller passes `--cd /` so wsl.exe doesn't inherit (or mangle,
    // via a prefix-sibling distro name) a caller cwd missing from the
    // target distro — see `shell::wsl_command`.
    #[cfg(unix)]
    let prefix: &[&str] = &["-d", "Ubuntu", "--cd", "/"];
    #[cfg(not(unix))]
    let prefix: &[&str] = &["-d", "Ubuntu"];
    let expected: Vec<&str> = prefix
        .iter()
        .copied()
        .chain(["git", "-C", "/home/me/repo", "worktree", "add", "-b", "x"])
        .collect();
    assert_eq!(args, expected);
    // The child cwd is left to the OS default; `--cd` is the control.
    assert_eq!(cmd.get_current_dir(), None);
}

#[test]
fn host_shell_c_wsl_passes_script_unquoted_via_exec() {
    // WSL: `--exec` hands argv to the in-distro process verbatim, so the
    // multi-statement script travels as a single *unquoted* arg. Without
    // it, wsl.exe substitutes `$…` inside the script and a pre-quoted
    // script arrives as one literal command word ("not found").
    let h = HostDef::wsl("Ubuntu");
    let cmd = host_shell_c(&h, "mkdir -p /a && ln -s /b /a/b");
    let (prog, args) = program_and_args(&cmd);
    assert_eq!(prog, "wsl.exe");
    #[cfg(unix)]
    let prefix: &[&str] = &["-d", "Ubuntu", "--cd", "/"];
    #[cfg(not(unix))]
    let prefix: &[&str] = &["-d", "Ubuntu"];
    let expected: Vec<&str> = prefix
        .iter()
        .copied()
        .chain(["-e", "sh", "-c", "mkdir -p /a && ln -s /b /a/b"])
        .collect();
    assert_eq!(args, expected);
}

/// A Windows host, declared the only way thurbox declares one.
fn windows_host() -> HostDef {
    HostDef {
        name: "winbox".into(),
        destination: "me@winbox".into(),
        multiplexer: Some("psmux".into()),
        ..Default::default()
    }
}

/// The advisory OpenSSH ≥ 10 prints on stderr for every non-PQ connection.
const PQ_ADVISORY: &str = "\
** WARNING: connection is not using a post-quantum key exchange algorithm.
** This session may be vulnerable to \"store now, decrypt later\" attacks.
** The server may need to be upgraded. See https://openssh.com/pq.html";

#[test]
fn the_post_quantum_advisory_is_not_reported_as_the_error() {
    // Verbatim, this block is FIRST in the buffer, so every remote failure
    // read as "connection is not using a post-quantum key…" and the real
    // cause was pushed below the fold — or off the end of a narrow band.
    let stderr = format!("{PQ_ADVISORY}\nfatal: not a git repository");
    assert_eq!(
        reportable_stderr(stderr.as_bytes()),
        "fatal: not a git repository"
    );
}

#[test]
fn an_advisory_alone_leaves_nothing_to_report() {
    // Not the advisory again as a consolation prize: empty, so the caller
    // reports the exit status, which at least concerns the failure.
    assert_eq!(reportable_stderr(PQ_ADVISORY.as_bytes()), "");
}

#[test]
fn powershell_clixml_stderr_is_decoded_to_its_message() {
    // Captured verbatim from a real Windows SSH host: `powershell.exe` does
    // not write error records as text when stderr is redirected, so this
    // markup is what a Windows failure actually arrives as.
    let stderr = format!(
        "{PQ_ADVISORY}\n#< CLIXML\n<Objs Version=\"1.1.0.1\" \
         xmlns=\"http://schemas.microsoft.com/powershell/2004/04\">\
         <Obj S=\"progress\" RefId=\"0\"><TN RefId=\"0\">\
         <T>System.Management.Automation.PSCustomObject</T></TN><MS>\
         <I64 N=\"SourceId\">1</I64><PR N=\"Record\">\
         <AV>Preparing modules for first use.</AV></PR></MS></Obj>\
         <S S=\"Error\">Get-ChildItem : Access to the path is \
         denied._x000D__x000A_</S>\
         <S S=\"Error\">    + CategoryInfo : PermissionDenied_x000D__x000A_</S>\
         </Objs>"
    );
    let decoded = reportable_stderr(stderr.as_bytes());
    assert_eq!(
        decoded,
        "Get-ChildItem : Access to the path is denied.\n    + CategoryInfo : PermissionDenied"
    );
    assert!(!decoded.contains("CLIXML"), "envelope survived: {decoded}");
    assert!(!decoded.contains("<Objs"), "envelope survived: {decoded}");
    assert!(
        !decoded.contains("Preparing modules"),
        "a progress record is not an error: {decoded}"
    );
}

#[test]
fn clixml_carrying_no_error_reports_the_exit_status_instead() {
    // The shape a script that exits non-zero *silently* produces: an
    // envelope with only a progress record. Reporting it verbatim is how
    // `#< CLIXML <Objs Version=…` reached the user.
    let stderr = "#< CLIXML\n<Objs Version=\"1.1.0.1\"><Obj S=\"progress\" \
                  RefId=\"0\"><AV>Preparing modules for first use.</AV></Obj></Objs>";
    assert_eq!(reportable_stderr(stderr.as_bytes()), "");
}

#[test]
fn a_native_commands_own_stderr_survives_the_envelope() {
    // `[Console]::Error.WriteLine` and any native command write raw text,
    // which can be interleaved with an envelope. Both halves are kept.
    let stderr = "#< CLIXML\nno such directory: C:/Users/me/nope\n\
                  <Objs Version=\"1.1.0.1\"><Obj S=\"progress\" RefId=\"0\" /></Objs>";
    assert_eq!(
        reportable_stderr(stderr.as_bytes()),
        "no such directory: C:/Users/me/nope"
    );
}

#[test]
fn posix_stderr_is_passed_through_untouched() {
    // No advisory, no envelope: the POSIX path must be unaffected.
    assert_eq!(
        reportable_stderr(b"sh: 1: cd: can't cd to /nope"),
        "sh: 1: cd: can't cd to /nope"
    );
}

#[test]
fn the_advisory_is_filtered_off_crlf_output_too() {
    // ssh's real output is CRLF-terminated, which is how it arrives from a
    // Windows host — the filter must not depend on bare LF.
    let stderr = "** WARNING: connection is not using a post-quantum \
                  key exchange algorithm.\r\n\
                  ** The server may need to be upgraded.\r\n\
                  fatal: repository '/nope' does not exist\r\n";
    assert_eq!(
        reportable_stderr(stderr.as_bytes()),
        "fatal: repository '/nope' does not exist"
    );
}

#[test]
fn a_line_merely_containing_asterisks_is_not_an_advisory() {
    // Only a LEADING `**` marks one; git's own output must survive.
    let stderr = "error: pathspec '**/*.rs' did not match any file";
    assert_eq!(reportable_stderr(stderr.as_bytes()), stderr);
}

#[test]
fn a_malformed_underscore_x_is_literal_text() {
    // `_x` only opens an escape when four hex digits and a `_` follow.
    // Guessing otherwise would corrupt a message that merely contains `_x`.
    for text in [
        "_x",
        "_xZZZZ_",
        "_x00_",
        "_x000D",
        "path_x_thing",
        "_x000Dtail",
    ] {
        assert_eq!(unescape_clixml(text), text, "{text} must pass through");
    }
    // `from_str_radix` accepts a leading sign, so this once decoded to a
    // character: four *hex digits* are required, not four parseable chars.
    assert_eq!(unescape_clixml("_x+12_"), "_x+12_");
}

#[test]
fn a_well_formed_underscore_x_decodes_and_keeps_its_surroundings() {
    assert_eq!(unescape_clixml("a_x000D__x000A_b"), "a\r\nb");
    assert_eq!(unescape_clixml("_x0041_"), "A");
    // Lower-case hex is valid too.
    assert_eq!(unescape_clixml("_x000d_"), "\r");
}

#[test]
fn xml_entities_decode_with_the_ampersand_last() {
    assert_eq!(
        unescape_clixml("&lt;Objs&gt; &quot;x&quot; &apos;y&apos;"),
        "<Objs> \"x\" 'y'"
    );
    // `&amp;lt;` is a literal `&lt;`, which only holds if `&amp;` is
    // substituted after the others rather than before.
    assert_eq!(unescape_clixml("&amp;lt;"), "&lt;");
    assert_eq!(unescape_clixml("a &amp; b"), "a & b");
}

#[test]
fn describe_exit_names_sshs_reserved_code_without_claiming_ssh() {
    // 255 means the command never ran, which is a different sentence from
    // "the command failed" — but the same helper serves `wsl.exe`, which
    // has no such convention, so it must not say ssh.
    let described = describe_exit(Some(255));
    assert!(described.contains("255"), "{described}");
    assert!(described.contains("never reached"), "{described}");
    assert!(!described.contains("ssh"), "{described}");

    assert_eq!(describe_exit(Some(1)), "exit 1, no error output");
    assert_eq!(describe_exit(None), "terminated without an exit code");
}

#[test]
fn powershell_quote_doubles_only_the_single_quote() {
    assert_eq!(powershell_quote("C:/Users/me"), "'C:/Users/me'");
    assert_eq!(powershell_quote("o'brien"), "'o''brien'");
    // `\` and `$` are literal inside a single-quoted PowerShell string,
    // which is exactly why it is the right quote for a Windows path.
    assert_eq!(powershell_quote(r"C:\Users\$env:X"), r"'C:\Users\$env:X'");
    assert_eq!(powershell_quote(""), "''");
}

#[test]
fn a_psmux_host_is_a_windows_host_and_a_wsl_distro_is_not() {
    assert!(windows_host().is_windows());
    assert!(!host("me@box", None).is_windows());
    // A distro runs `tmux` inside Linux, whatever the machine hosting it.
    assert!(!HostDef::wsl("Ubuntu").is_windows());
}

#[test]
fn a_windows_host_is_probed_with_powershell_not_sh() {
    // `sh -c …` on a native-Windows host fails with PowerShell's
    // CommandNotFoundException — which is what stopped the repo picker
    // listing a directory there at all.
    let cmd = host_probe(&windows_host(), "test -d /a", "Test-Path /a");
    let (prog, args) = program_and_args(&cmd);
    assert_eq!(prog, "ssh");
    assert!(
        args.iter().any(|a| a == "powershell"),
        "expected the powershell transport; got {args:?}"
    );
    assert!(
        !args.iter().any(|a| a.contains("test -d")),
        "the posix script must not be sent to a windows host: {args:?}"
    );
    // And a POSIX host still gets `sh`, untouched.
    let (_, posix) = program_and_args(&host_probe(&host("me@box", None), "test -d /a", "x"));
    assert!(
        posix.iter().any(|a| a == posix_quote("sh").as_str()),
        "{posix:?}"
    );
}

#[test]
fn the_powershell_payload_is_utf16le_base64_so_no_shell_can_rewrite_it() {
    use base64::Engine as _;

    // ssh space-joins its args for the host's default sshd shell to parse,
    // and that shell is commonly PowerShell, which expands `$…` inside
    // double quotes: a probe reading `$PSVersionTable` came back with the
    // OUTER shell's expansion substituted in. Base64 has nothing to expand.
    let script = "$d='C:/a'; Write-Output \"$d & 'x'\"";
    let cmd = host_powershell_c(&windows_host(), script);
    let (_, args) = program_and_args(&cmd);
    let encoded = args.last().expect("payload");
    assert!(
        encoded
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'='),
        "payload must be inert base64: {encoded}"
    );
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("valid base64");
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    assert_eq!(
        String::from_utf16(&units).expect("utf-16le"),
        script,
        "-EncodedCommand decodes UTF-16LE; the script must arrive byte-exact"
    );
    assert!(args.iter().any(|a| a == "-NoProfile"), "{args:?}");
}

#[test]
fn the_windows_probe_scripts_quote_a_path_holding_a_quote() {
    // `'` is the only character special inside a PowerShell single-quoted
    // literal — `\` and `$` are not, which is what makes it right for a
    // Windows path.
    let path = "C:/Users/o'brien/repos";
    for script in [
        list_dir_entries_script_windows(path),
        classify_path_script_windows(path),
        scan_child_repos_script_windows(path),
    ] {
        assert!(
            script.contains("'C:/Users/o''brien/repos'"),
            "quote not doubled: {script}"
        );
    }
}

#[test]
fn the_windows_probe_scripts_emit_the_same_protocol_as_the_posix_ones() {
    // The point of the pair: every parser below stays transport-neutral.
    assert_eq!(
        parse_dir_listing("!missing"),
        DirListing::Missing,
        "the windows listing writes the same sentinel"
    );
    let listing = list_dir_entries_script_windows("C:/a");
    assert!(listing.contains("'!missing'"), "{listing}");
    assert!(listing.contains("'g '"), "{listing}");
    assert!(listing.contains("'d '"), "{listing}");
    // Hidden entries are included, matching the POSIX loop's `* .*`.
    assert!(listing.contains("-Force"), "{listing}");

    let classify = classify_path_script_windows("C:/a");
    for word in ["'missing'", "'git'", "'dir'"] {
        assert!(classify.contains(word), "{word} missing from {classify}");
    }

    // The scan skips `.`-prefixed names (the POSIX `*` glob's rule) and says
    // why it failed rather than exiting mute.
    let scan = scan_child_repos_script_windows("C:/a");
    assert!(scan.contains("StartsWith('.')"), "{scan}");
    assert!(scan.contains("[Console]::Error.WriteLine"), "{scan}");
    assert!(!scan.contains("-Force"), "hidden must stay skipped: {scan}");
}

#[test]
fn host_shell_c_ssh_posix_quotes_script() {
    // SSH space-joins its trailing args, so the script must be POSIX-quoted
    // to survive as a single `sh -c` argument.
    let h = host("me@box", None);
    let cmd = host_shell_c(&h, "mkdir -p /a && ln -s /b /a/b");
    let (prog, args) = program_and_args(&cmd);
    assert_eq!(prog, "ssh");
    // The script arg is single-quoted as a whole.
    assert!(
        args.iter().any(|a| a == "'mkdir -p /a && ln -s /b /a/b'"),
        "script should be posix-quoted for ssh; got {args:?}"
    );
}

#[test]
fn browse_scripts_posix_quote_the_user_typed_path() {
    // The dir/path is user-typed and embedded in a `sh -c` script — a
    // single quote or `$` must arrive literally, never as shell syntax.
    let tricky = "/srv/it's $HOME";
    for script in [
        list_dir_entries_script(tricky),
        classify_path_script(tricky),
        scan_child_repos_script(tricky),
    ] {
        assert!(
            script.contains(r#"'/srv/it'\''s $HOME'"#),
            "path must be posix-quoted in: {script}"
        );
    }
    // The listing script's protocol pieces are present.
    let script = list_dir_entries_script("/srv");
    assert!(script.contains("!missing"));
    assert!(script.contains("printf 'g %s\\n'"));
    assert!(script.contains("printf 'd %s\\n'"));
}

#[test]
fn collect_scanned_children_joins_and_sorts() {
    let parent = Path::new("/srv/projects");
    let repos = collect_scanned_children(parent, "web\napi\n\n*\n").unwrap();
    assert_eq!(
        repos,
        vec![
            PathBuf::from("/srv/projects/api"),
            PathBuf::from("/srv/projects/web"),
        ]
    );
}

#[test]
fn parse_dir_listing_reads_the_line_protocol() {
    // `g`/`d` tagged lines, sorted; unknown lines skipped, not fatal.
    let listing = parse_dir_listing("g thurbox\nd scratch\nnoise\ng api server\n");
    assert_eq!(
        listing,
        DirListing::Entries(vec![
            ("api server".into(), true),
            ("scratch".into(), false),
            ("thurbox".into(), true),
        ])
    );
    assert_eq!(parse_dir_listing("!missing\n"), DirListing::Missing);
    assert_eq!(parse_dir_listing(""), DirListing::Entries(Vec::new()));
}

#[test]
fn parse_path_class_reads_the_single_word() {
    assert_eq!(parse_path_class("git\n").unwrap(), PathClass::Git);
    assert_eq!(parse_path_class("dir\n").unwrap(), PathClass::Dir);
    assert_eq!(parse_path_class("missing\n").unwrap(), PathClass::Missing);
    assert!(parse_path_class("garbage").is_err());
}

#[test]
fn dir_file_probe_script_quotes_and_carries_the_protocol() {
    // Both paths are embedded in a `sh -c` script — a single quote or `$`
    // must arrive literally, never as shell syntax.
    let script = dir_file_probe_script("/srv/it's $HOME", "/srv/it's $HOME/hooks.json");
    assert!(
        script.contains(r#"'/srv/it'\''s $HOME'"#),
        "dir must be posix-quoted in: {script}"
    );
    assert!(
        script.contains(r#"'/srv/it'\''s $HOME/hooks.json'"#),
        "file must be posix-quoted in: {script}"
    );
    for sentinel in ["'@nodir'", "'@file'", "'@notfile'", "'@nofile'"] {
        assert!(
            script.contains(sentinel),
            "{sentinel} missing from {script}"
        );
    }
    // A missing guard dir answers alone — the file must not be consulted.
    assert!(script.contains("echo '@nodir'; exit 0"), "{script}");
}

#[test]
fn parse_dir_file_probe_reads_each_sentinel() {
    assert_eq!(
        parse_dir_file_probe("@nodir\n").unwrap(),
        DirFileProbe::NoDir
    );
    assert_eq!(
        parse_dir_file_probe("@nofile\n").unwrap(),
        DirFileProbe::NoFile
    );
    assert_eq!(
        parse_dir_file_probe("@notfile\n").unwrap(),
        DirFileProbe::NotFile
    );
    assert!(parse_dir_file_probe("garbage\n").is_err());
    assert!(parse_dir_file_probe("").is_err());
}

#[test]
fn parse_dir_file_probe_returns_content_verbatim() {
    // Content is everything after the `@file` line, byte-exact — including a
    // line that *looks* like a sentinel (file content is remote-controlled
    // text and may legitimately start with `@nodir`) and a body with no
    // trailing newline.
    assert_eq!(
        parse_dir_file_probe("@file\n{\n  \"a\": 1\n}\n").unwrap(),
        DirFileProbe::File("{\n  \"a\": 1\n}\n".into())
    );
    assert_eq!(
        parse_dir_file_probe("@file\n@nodir\nno newline at end").unwrap(),
        DirFileProbe::File("@nodir\nno newline at end".into())
    );
    // An empty file is `@file` with nothing after its newline.
    assert_eq!(
        parse_dir_file_probe("@file\n").unwrap(),
        DirFileProbe::File(String::new())
    );
}

#[test]
fn list_dir_entries_local_flags_git_repos_and_follows_symlinks() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("repo").join(".git")).unwrap();
    std::fs::create_dir(root.join("plain")).unwrap();
    std::fs::create_dir(root.join(".hidden")).unwrap();
    std::fs::write(root.join("file.txt"), "x").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("plain"), root.join("link")).unwrap();

    let DirListing::Entries(entries) =
        list_dir_entries_on(None, &root.display().to_string()).unwrap()
    else {
        panic!("existing dir must not be Missing");
    };
    // Hidden entries included (the picker filters); files excluded; a
    // symlink to a dir is listed (the `ls -p` gap this fixes).
    assert!(entries.contains(&(".hidden".to_string(), false)));
    assert!(entries.contains(&("repo".to_string(), true)));
    assert!(entries.contains(&("plain".to_string(), false)));
    assert!(!entries.iter().any(|(n, _)| n == "file.txt"));
    #[cfg(unix)]
    assert!(entries.contains(&("link".to_string(), false)));

    assert_eq!(
        list_dir_entries_on(None, &root.join("nope").display().to_string()).unwrap(),
        DirListing::Missing
    );
}

#[test]
fn expand_remote_tilde_passes_plain_paths_through() {
    // No `~` prefix → no remote round-trip, byte-identical passthrough
    // (including a mid-path `~`, which is not a home reference).
    let h = HostDef::wsl("Ubuntu");
    assert_eq!(
        expand_remote_tilde(&h, "/home/me/repos").unwrap(),
        "/home/me/repos"
    );
    assert_eq!(expand_remote_tilde(&h, "/data/~x").unwrap(), "/data/~x");
}

#[test]
fn remote_workspace_dir_derives_base_from_worktrees_dir() {
    // With a configured worktrees_dir the path is pure (no host round-trip):
    // base = its parent, mirroring the local `<data root>/workspaces` layout.
    let h = host("me@box", Some("/data/wt"));
    let ws = remote_workspace_dir(&h, "abc-123").unwrap();
    assert_eq!(ws, "/data/workspaces/abc-123");
}

#[test]
fn remote_workspace_dir_rejects_empty_id() {
    // An empty sanitized segment would make ensure/remove `rm -rf` the
    // workspaces *root* — must error like the local builder.
    let h = host("me@box", Some("/data/wt"));
    assert!(remote_workspace_dir(&h, "").is_err());
    assert!(remote_workspace_dir(&h, " .- ").is_err());
}

#[test]
fn worktree_path_for_remote_uses_configured_dir() {
    let h = host("me@box", Some("/data/wt"));
    let path = worktree_path_for(Some(&h), Path::new("/srv/repo"), "feature/foo").unwrap();
    let s = path.display().to_string();
    assert!(s.starts_with("/data/wt/"), "got {s}");
    assert!(s.ends_with("/feature-foo"), "got {s}");
}

#[test]
fn worktree_path_for_local_matches_worktree_path() {
    let base = PathBuf::from("/test/data");
    let _guard = TestPathGuard::new(&base);
    let repo = Path::new("/home/user/repo");
    let via_for = worktree_path_for(None, repo, "main").unwrap();
    let direct = worktree_path(repo, "main").unwrap();
    assert_eq!(via_for, direct);
}

// ── parse_repo_name_from_url ────────────────────────────────────

#[test]
fn parse_ssh_url() {
    assert_eq!(
        parse_repo_name_from_url("git@github.com:user/thurbox.git"),
        Some("thurbox".to_string())
    );
}

#[test]
fn parse_https_url_with_git_suffix() {
    assert_eq!(
        parse_repo_name_from_url("https://github.com/org/api-server.git"),
        Some("api-server".to_string())
    );
}

#[test]
fn parse_https_url_without_git_suffix() {
    assert_eq!(
        parse_repo_name_from_url("https://github.com/org/api-server"),
        Some("api-server".to_string())
    );
}

#[test]
fn parse_empty_url() {
    assert_eq!(parse_repo_name_from_url(""), None);
}

#[test]
fn parse_ssh_url_no_user_path() {
    assert_eq!(
        parse_repo_name_from_url("git@host:repo.git"),
        Some("repo".to_string())
    );
}

#[test]
fn parse_url_trailing_slash() {
    // Trailing slash produces empty last segment — rsplit('/').next() = ""
    assert_eq!(
        parse_repo_name_from_url("https://github.com/org/repo/"),
        None
    );
}

/// A peer that reads the whole payload is the ordinary path, and one that dies
/// holding the pipe open is the path that used to leak an `ssh` per attempt:
/// `write_all` returned `EPIPE`, the `?` skipped the wait, and `Child`'s `Drop`
/// neither killed nor reaped it. Unix-only because both need a POSIX shell.
#[cfg(unix)]
mod stream_into_child {
    use std::process::{Command, Stdio};

    fn peer(script: &str) -> std::process::Child {
        Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn the test peer")
    }

    #[test]
    fn a_payload_the_peer_reads_is_delivered() {
        let child = peer("cat > /dev/null; echo done");
        let out = super::super::stream_into_child(child, &vec![b'x'; 1 << 20], "test-copy")
            .expect("a peer that reads the payload succeeds");
        assert_eq!(String::from_utf8_lossy(&out).trim(), "done");
    }

    #[test]
    fn a_peer_that_dies_mid_payload_reports_its_own_stderr() {
        // Big enough to outrun the pipe buffer, so the write is still going
        // when the peer exits and `write_all` really does see `EPIPE`.
        let child = peer("echo 'no room on device' >&2; exit 1");
        let error = super::super::stream_into_child(child, &vec![b'x'; 8 << 20], "test-copy")
            .expect_err("a peer that exits mid-payload fails");
        // The remote's reason, not our end noticing a broken pipe.
        assert!(
            format!("{error:#}").contains("no room on device"),
            "{error:#}"
        );
    }

    #[test]
    fn a_peer_that_dies_silently_still_returns() {
        // Nothing on stderr to explain it: the write error is the fallback, and
        // the point is that this returns at all rather than waiting forever.
        let child = peer("exit 1");
        let error = super::super::stream_into_child(child, &vec![b'x'; 8 << 20], "test-copy")
            .expect_err("a silent peer still fails");
        assert!(format!("{error:#}").contains("test-copy"), "{error:#}");
    }
}

// ── listing the worktrees a repo already has ───────────────────────────────

#[test]
fn parse_worktree_list_keeps_only_linked_named_branches() {
    // The first `worktree` stanza is always the main checkout, which is the
    // repo itself rather than something to open a session on.
    let porcelain = "\
worktree /repo
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /repo/.worktrees/tooltips
HEAD 2222222222222222222222222222222222222222
branch refs/heads/feat/dynamic-tooltips

worktree /elsewhere/detached
HEAD 3333333333333333333333333333333333333333
detached

worktree /repo/.worktrees/stale
HEAD 4444444444444444444444444444444444444444
branch refs/heads/gone
prunable gitdir file points to non-existent location

worktree /repo/bare
bare
";
    let found = parse_worktree_list(porcelain);
    assert_eq!(
        found,
        vec![ExistingWorktree {
            path: PathBuf::from("/repo/.worktrees/tooltips"),
            branch: "feat/dynamic-tooltips".to_string(),
        }]
    );
}

#[test]
fn parse_worktree_list_handles_empty_and_trailing_output() {
    assert!(parse_worktree_list("").is_empty());
    // No trailing blank line after the last stanza.
    let one = "worktree /repo\nHEAD 1\nbranch refs/heads/main\n\nworktree /wt\nHEAD 2\nbranch refs/heads/x";
    assert_eq!(parse_worktree_list(one).len(), 1);
    assert_eq!(parse_worktree_list(one)[0].branch, "x");
}

#[test]
fn parse_worktree_list_keeps_a_path_that_contains_spaces() {
    // Only the FIRST space separates the key from its value, so a checkout
    // under a directory with a space in it is not truncated.
    let porcelain = "worktree /repo\nHEAD 1\nbranch refs/heads/main\n\n\
worktree /My Projects/repo/.worktrees/a b\nHEAD 2\nbranch refs/heads/x\n";
    let found = parse_worktree_list(porcelain);
    assert_eq!(
        found[0].path,
        PathBuf::from("/My Projects/repo/.worktrees/a b")
    );
}

#[test]
fn parse_worktree_list_keeps_a_locked_worktree() {
    // Locked is not prunable: the checkout is on disk and openable.
    let porcelain = "worktree /repo\nHEAD 1\nbranch refs/heads/main\n\n\
worktree /wt\nHEAD 2\nbranch refs/heads/x\nlocked in use\n";
    assert_eq!(parse_worktree_list(porcelain).len(), 1);
}

#[test]
fn list_worktrees_reports_a_worktree_at_a_foreign_path() {
    // The point of the feature: a worktree the user (or their agent) made at
    // an arbitrary location is found, not just ones under thurbox's own dir.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    seed_repo(&repo, "file.txt", "hi");

    assert!(list_worktrees(&repo).unwrap().is_empty());

    let foreign = repo.join(".worktrees").join("tooltips");
    run_git(
        {
            let mut c = git_program();
            c.args(["worktree", "add", "-b", "feat/tooltips"])
                .arg(&foreign)
                .current_dir(&repo);
            c
        },
        "git worktree add",
    )
    .expect("add worktree");

    let found = list_worktrees(&repo).unwrap();
    assert_eq!(found.len(), 1, "expected exactly one linked worktree");
    assert_eq!(found[0].branch, "feat/tooltips");
    assert_eq!(
        found[0].path.canonicalize().unwrap(),
        foreign.canonicalize().unwrap()
    );
}
