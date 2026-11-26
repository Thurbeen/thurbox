use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::session::HostDef;

#[test]
fn git_program_scrubs_inherited_location_env() {
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
        run_git(git_command(dir, &args), "git").expect("seed");
    }
    head_commit(dir).expect("head")
}

#[test]
fn cloning_a_mod_delivers_its_files_and_reports_the_commit() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let origin = tmp.path().join("origin");
    let commit = seed_repo(&origin, "mods/40_x.lua", "return {}\n");
    std::fs::write(origin.join("payload.bin"), [0x00u8, 0xff, 0xfe, 0x01]).expect("write");
    for args in [vec!["add", "-A"], vec!["commit", "-m", "payload"]] {
        run_git(git_command(&origin, &args), "git").expect("commit");
    }
    let commit = {
        let _ = commit;
        head_commit(&origin).expect("head")
    };

    let dest = tmp.path().join("ui").join("x");
    clone_mod(&origin.to_string_lossy(), &dest, None).expect("clone");

    assert_eq!(
        std::fs::read(dest.join("payload.bin")).expect("read"),
        [0x00u8, 0xff, 0xfe, 0x01],
        "the bytes survive, which is the whole point of a clone"
    );
    assert!(dest.join("mods/40_x.lua").is_file());
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
    let error = clone_mod("https://example.com/x.git", &dest, None).expect_err("should refuse");
    assert!(error.to_string().contains("already exists"), "{error}");
    assert!(dest.join("mine.lua").is_file(), "and leaves it alone");
}

#[test]
fn a_pin_is_routed_by_length_and_explained_by_shape() {
    let sha1 = "0123456789abcdef0123456789abcdef01234567";
    let sha256 = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    for id in [sha1, sha256] {
        assert!(names_a_commit(id), "{id}");
        assert!(!abbreviated_object_id(id), "{id}");
    }

    for prefix in [
        &sha1[..7],
        &sha1[..8],
        &sha1[..39],
        &sha256[..41],
        &sha256[..63],
    ] {
        assert!(names_a_commit(prefix), "{prefix}");
        assert!(abbreviated_object_id(prefix), "{prefix}");
    }

    for name in ["main", "v1.2.3", "feature/x", "decade", &sha256.repeat(2)] {
        assert!(!names_a_commit(name), "{name}");
        assert!(!abbreviated_object_id(name), "{name}");
    }
}

#[test]
fn an_edited_working_copy_reports_itself_dirty() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let origin = tmp.path().join("origin");
    seed_repo(&origin, "mods/40_x.lua", "return {}\n");
    let dest = tmp.path().join("clone");
    clone_mod(&origin.to_string_lossy(), &dest, None).expect("clone");

    assert!(!is_dirty(&dest).expect("clean"));
    std::fs::write(dest.join("mods/40_x.lua"), "-- mine\n").expect("edit");
    assert!(
        is_dirty(&dest).expect("dirty"),
        "an edit has to be visible, or nothing can refuse to overwrite it"
    );
}

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
    let combined = "1\t0\tdiff --git trap.txt\0diff --git a/x b/x\n+hi\n";
    let (counts, patch) = split_untracked_diff(combined);
    assert_eq!(counts, "1\t0\tdiff --git trap.txt\0");
    assert_eq!(patch, "diff --git a/x b/x\n+hi\n");
}

#[test]
fn split_untracked_diff_empty_and_patchless_inputs() {
    assert_eq!(split_untracked_diff(""), ("", ""));
    assert_eq!(split_untracked_diff("1\t0\ta.txt\0"), ("1\t0\ta.txt\0", ""));
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
    assert!(!status.dirty);
    assert_eq!(status.untracked, 0);
    assert_eq!(status.ahead_behind, Some((0, 0)));
}

#[test]
fn parse_status_v2_without_upstream_reports_no_ab() {
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
    // git worktree checkouts use a `.git` file, not a directory.
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
fn resolve_base_ref_none_without_remote() {
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

    assert_eq!(resolve_base_ref(repo), None);
}

#[test]
fn resolve_base_ref_prefers_upstream() {
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

    std::fs::create_dir_all(&remote).unwrap();
    run(&remote, &["init", "-q", "--bare"]);

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

    assert_eq!(resolve_base_ref(&work), Some("@{upstream}".to_string()));
}

fn host(dest: &str, wt_dir: Option<&str>) -> HostDef {
    HostDef {
        name: "h".into(),
        destination: dest.into(),
        ssh_opts: vec!["-o".into(), "ControlMaster=auto".into()],
        worktrees_dir: wt_dir.map(str::to_string),
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
    let cmd = git_command(Path::new("/repo"), &["branch", "--list"]);
    let (prog, args) = program_and_args(&cmd);
    assert_eq!(prog, "git");
    assert_eq!(args, ["branch", "--list"]);
    assert_eq!(cmd.get_current_dir(), Some(Path::new("/repo")));
}

fn windows_host() -> HostDef {
    HostDef {
        name: "winbox".into(),
        destination: "me@winbox".into(),
        multiplexer: Some("psmux".into()),
        ..Default::default()
    }
}

const PQ_ADVISORY: &str = "\
** WARNING: connection is not using a post-quantum key exchange algorithm.
** This session may be vulnerable to \"store now, decrypt later\" attacks.
** The server may need to be upgraded. See https://openssh.com/pq.html";

#[test]
fn the_post_quantum_advisory_is_not_reported_as_the_error() {
    let stderr = format!("{PQ_ADVISORY}\nfatal: not a git repository");
    assert_eq!(
        reportable_stderr(stderr.as_bytes()),
        "fatal: not a git repository"
    );
}

#[test]
fn an_advisory_alone_leaves_nothing_to_report() {
    assert_eq!(reportable_stderr(PQ_ADVISORY.as_bytes()), "");
}

#[test]
fn posix_stderr_is_passed_through_untouched() {
    assert_eq!(
        reportable_stderr(b"sh: 1: cd: can't cd to /nope"),
        "sh: 1: cd: can't cd to /nope"
    );
}

#[test]
fn the_advisory_is_filtered_off_crlf_output_too() {
    // ssh output from a Windows host is CRLF-terminated.
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
    let stderr = "error: pathspec '**/*.rs' did not match any file";
    assert_eq!(reportable_stderr(stderr.as_bytes()), stderr);
}

#[test]
fn a_psmux_host_is_a_windows_host_and_a_wsl_distro_is_not() {
    assert!(windows_host().is_windows());
    assert!(!host("me@box", None).is_windows());
    assert!(!HostDef::wsl("Ubuntu").is_windows());
}

#[test]
fn the_powershell_payload_is_utf16le_base64_so_no_shell_can_rewrite_it() {
    use base64::Engine as _;

    // sshd hands the joined args to PowerShell, which expands `$` inside double quotes.
    let script = "$d='C:/a'; Write-Output \"$d & 'x'\"";
    let cmd = crate::shell::HostShell::run_on_host(&windows_host(), script);
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
        .as_chunks::<2>()
        .0
        .iter()
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
fn list_dir_entries_flags_git_repos_and_follows_symlinks() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("repo").join(".git")).unwrap();
    std::fs::create_dir(root.join("plain")).unwrap();
    std::fs::create_dir(root.join(".hidden")).unwrap();
    std::fs::write(root.join("file.txt"), "x").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("plain"), root.join("link")).unwrap();

    let DirListing::Entries(entries) = list_dir_entries(&root.display().to_string()) else {
        panic!("existing dir must not be Missing");
    };
    assert!(entries.contains(&(".hidden".to_string(), false)));
    assert!(entries.contains(&("repo".to_string(), true)));
    assert!(entries.contains(&("plain".to_string(), false)));
    assert!(!entries.iter().any(|(n, _)| n == "file.txt"));
    #[cfg(unix)]
    assert!(entries.contains(&("link".to_string(), false)));

    assert_eq!(
        list_dir_entries(&root.join("nope").display().to_string()),
        DirListing::Missing
    );
}

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
    assert_eq!(
        parse_repo_name_from_url("https://github.com/org/repo/"),
        None
    );
}

fn git_in(dir: &Path, args: &[&str]) {
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
}

fn commit_file(dir: &Path, file: &str, contents: &str, msg: &str) {
    std::fs::write(dir.join(file), contents).unwrap();
    git_in(dir, &["add", "-A"]);
    git_in(dir, &["commit", "-qm", msg]);
}

fn pr_repo(tmp: &Path) -> PathBuf {
    let remote = tmp.join("remote.git");
    let work = tmp.join("work");

    std::fs::create_dir_all(&remote).unwrap();
    git_in(&remote, &["init", "-q", "--bare", "--initial-branch=main"]);

    std::fs::create_dir_all(&work).unwrap();
    git_in(&work, &["init", "-q", "--initial-branch=main"]);
    git_in(&work, &["config", "user.email", "t@example.com"]);
    git_in(&work, &["config", "user.name", "t"]);
    git_in(&work, &["config", "commit.gpgsign", "false"]);
    git_in(
        &work,
        &["remote", "add", "origin", &remote.display().to_string()],
    );
    commit_file(&work, "base.txt", "base", "seed");
    git_in(&work, &["push", "-q", "-u", "origin", "main"]);

    git_in(&work, &["checkout", "-q", "-b", "feature"]);
    for file in ["one.txt", "two.txt", "three.txt"] {
        commit_file(&work, file, file, file);
    }
    git_in(&work, &["push", "-q", "-u", "origin", "feature"]);
    work
}

fn land(work: &Path) {
    git_in(work, &["push", "-q", "origin", "main"]);
    git_in(work, &["push", "-q", "origin", "--delete", "feature"]);
    git_in(work, &["checkout", "-q", "feature"]);
    git_in(work, &["fetch", "-q", "--prune", "origin"]);
}

fn squash_merged_repo(tmp: &Path) -> PathBuf {
    let work = pr_repo(tmp);
    git_in(&work, &["checkout", "-q", "main"]);
    git_in(&work, &["merge", "-q", "--squash", "feature"]);
    git_in(&work, &["commit", "-qm", "feat: the squashed pull request"]);
    commit_file(&work, "unrelated.txt", "unrelated", "another PR");
    land(&work);
    work
}

#[test]
fn a_squash_merged_branch_reports_itself_merged() {
    let tmp = tempfile::tempdir().unwrap();
    let work = squash_merged_repo(tmp.path());

    assert_eq!(merged_into_default(&work), Some(true));
}

#[test]
fn an_unmerged_branch_reports_itself_unmerged() {
    let tmp = tempfile::tempdir().unwrap();
    let work = squash_merged_repo(tmp.path());
    git_in(&work, &["checkout", "-q", "-b", "wip", "origin/main"]);
    commit_file(&work, "wip.txt", "wip", "work in progress");

    assert_eq!(merged_into_default(&work), Some(false));
}

fn loose_objects(dir: &Path) -> usize {
    let out = run_git_capture(&["count-objects"], dir).expect("count-objects");
    out.split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .expect("count-objects prints the loose count first")
}

#[test]
fn re_statting_an_unmerged_worktree_writes_no_further_objects() {
    let tmp = tempfile::tempdir().unwrap();
    let work = squash_merged_repo(tmp.path());
    git_in(&work, &["checkout", "-q", "-b", "wip", "origin/main"]);
    commit_file(&work, "wip.txt", "wip", "work in progress");

    let stats = worktree_stats(&work, None).expect("a worktree");
    assert_eq!(
        stats.merged,
        Some(false),
        "the stat has to reach the squash check for this test to mean anything"
    );
    let settled = loose_objects(&work);

    // git commit timestamps are whole seconds, so the probe must be re-asked across a tick.
    sleep_past_the_second();
    assert_eq!(
        worktree_stats(&work, None).and_then(|s| s.merged),
        Some(false)
    );

    assert_eq!(
        loose_objects(&work),
        settled,
        "the same question must reuse the same probe commit"
    );
}

fn sleep_past_the_second() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch");
    let to_the_tick = Duration::from_secs(1) - Duration::from_nanos(now.subsec_nanos().into());
    std::thread::sleep(to_the_tick + Duration::from_millis(20));
}

#[test]
fn a_branch_already_on_the_default_reports_itself_merged() {
    let tmp = tempfile::tempdir().unwrap();
    let work = squash_merged_repo(tmp.path());
    git_in(&work, &["checkout", "-q", "-b", "spike", "origin/main"]);

    assert_eq!(merged_into_default(&work), Some(true));
}

#[test]
fn a_merge_commit_branch_reports_itself_merged() {
    let tmp = tempfile::tempdir().unwrap();
    let work = pr_repo(tmp.path());
    assert_eq!(
        merged_into_default(&work),
        Some(false),
        "nothing landed yet"
    );

    git_in(&work, &["checkout", "-q", "main"]);
    git_in(
        &work,
        &["merge", "-q", "--no-ff", "-m", "merge PR", "feature"],
    );
    land(&work);

    assert_eq!(merged_into_default(&work), Some(true));
}

#[test]
fn a_fast_forwarded_branch_reports_itself_merged() {
    let tmp = tempfile::tempdir().unwrap();
    let work = pr_repo(tmp.path());
    assert_eq!(
        merged_into_default(&work),
        Some(false),
        "nothing landed yet"
    );

    git_in(&work, &["checkout", "-q", "main"]);
    git_in(&work, &["merge", "-q", "--ff-only", "feature"]);
    land(&work);

    assert_eq!(merged_into_default(&work), Some(true));
}

#[test]
fn a_squash_landed_after_the_default_moved_reports_itself_merged() {
    let tmp = tempfile::tempdir().unwrap();
    let work = pr_repo(tmp.path());
    assert_eq!(
        merged_into_default(&work),
        Some(false),
        "nothing landed yet"
    );

    git_in(&work, &["checkout", "-q", "main"]);
    commit_file(&work, "unrelated.txt", "unrelated", "someone else's PR");
    git_in(&work, &["merge", "-q", "--squash", "feature"]);
    git_in(&work, &["commit", "-qm", "feat: the squashed pull request"]);
    land(&work);

    assert_eq!(merged_into_default(&work), Some(true));
}

fn replay_feature_onto_main(work: &Path) {
    git_in(work, &["checkout", "-q", "main"]);
    // Without a commit to land on, git rebase fast-forwards and keeps the old shas.
    commit_file(work, "unrelated.txt", "unrelated", "someone else's PR");
    git_in(work, &["checkout", "-q", "-b", "landing", "feature"]);
    git_in(work, &["rebase", "-q", "main"]);
}

#[test]
fn a_rebase_merged_branch_reports_itself_merged() {
    let tmp = tempfile::tempdir().unwrap();
    let work = pr_repo(tmp.path());
    assert_eq!(
        merged_into_default(&work),
        Some(false),
        "nothing landed yet"
    );

    replay_feature_onto_main(&work);
    git_in(&work, &["checkout", "-q", "main"]);
    git_in(&work, &["merge", "-q", "--ff-only", "landing"]);
    git_in(&work, &["branch", "-qD", "landing"]);
    land(&work);

    assert_eq!(merged_into_default(&work), Some(true));
}

#[test]
fn a_semi_linear_merge_reports_itself_merged() {
    let tmp = tempfile::tempdir().unwrap();
    let work = pr_repo(tmp.path());
    assert_eq!(
        merged_into_default(&work),
        Some(false),
        "nothing landed yet"
    );

    replay_feature_onto_main(&work);
    git_in(&work, &["checkout", "-q", "main"]);
    git_in(
        &work,
        &["merge", "-q", "--no-ff", "-m", "merge PR", "landing"],
    );
    git_in(&work, &["branch", "-qD", "landing"]);
    land(&work);

    assert_eq!(merged_into_default(&work), Some(true));
}

#[test]
fn a_branch_whose_tree_matches_the_default_reports_itself_merged() {
    let tmp = tempfile::tempdir().unwrap();
    let work = pr_repo(tmp.path());
    commit_file(&work, "one.txt", "final", "polish one.txt");
    assert_eq!(
        merged_into_default(&work),
        Some(false),
        "nothing landed yet"
    );

    git_in(&work, &["checkout", "-q", "main"]);
    commit_file(&work, "one.txt", "final", "reimplemented, part 1");
    commit_file(&work, "two.txt", "two.txt", "reimplemented, part 2");
    commit_file(&work, "three.txt", "three.txt", "reimplemented, part 3");
    land(&work);

    assert_eq!(merged_into_default(&work), Some(true));
}

#[test]
fn an_unmerged_branch_stays_unmerged_when_the_default_moves_on() {
    let tmp = tempfile::tempdir().unwrap();
    let work = pr_repo(tmp.path());

    git_in(&work, &["checkout", "-q", "main"]);
    commit_file(&work, "unrelated.txt", "unrelated", "someone else's PR");
    git_in(&work, &["push", "-q", "origin", "main"]);
    git_in(&work, &["checkout", "-q", "feature"]);
    git_in(&work, &["fetch", "-q", "origin"]);

    assert_eq!(merged_into_default(&work), Some(false));
}

#[test]
fn a_branch_with_nothing_ahead_is_never_asked() {
    let tmp = tempfile::tempdir().unwrap();
    let work = squash_merged_repo(tmp.path());
    git_in(&work, &["checkout", "-q", "-b", "spike", "origin/main"]);

    let stats = worktree_stats(&work, None).expect("stats");
    assert_eq!(stats.ahead, 0);
    assert_eq!(stats.merged, None, "not asked, so not answered");
}

#[test]
fn merged_into_default_is_unknown_without_a_remote_default() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("solo");
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "-q", "--initial-branch=main"]);
    git_in(&repo, &["config", "user.email", "t@example.com"]);
    git_in(&repo, &["config", "user.name", "t"]);
    git_in(&repo, &["config", "commit.gpgsign", "false"]);
    commit_file(&repo, "file.txt", "hi", "init");

    assert_eq!(merged_into_default(&repo), None);
}

#[test]
fn parse_status_v2_reads_the_head_oid() {
    let status = parse_status_v2(
        "# branch.oid cf2c3b773317d32908f7ec947f2adca2afdded09\n\
         # branch.head feature\n\
         # branch.upstream origin/feature\n\
         # branch.ab +2 -0\n",
    );
    assert_eq!(
        status.head.as_deref(),
        Some("cf2c3b773317d32908f7ec947f2adca2afdded09")
    );
}

#[test]
fn parse_status_v2_without_a_head_oid_reports_none() {
    // git prints `# branch.oid (initial)` for an unborn branch.
    let status = parse_status_v2("# branch.oid (initial)\n# branch.head main\n");
    assert_eq!(status.head, None);
}

#[test]
fn a_merged_answer_is_reused_only_for_the_commit_it_was_computed_for() {
    let tmp = tempfile::tempdir().unwrap();
    let work = squash_merged_repo(tmp.path());
    let run = |args: &[&str]| {
        let out = git_program()
            .args(args)
            .current_dir(&work)
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?}");
    };

    let landed = worktree_stats(&work, None).expect("stats");
    assert_eq!(landed.merged, Some(true), "the branch was squash-merged");
    let landed_head = landed.head.clone().expect("a committed branch has a head");

    std::fs::write(work.join("followup.txt"), "follow-up").unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-qm", "follow-up work, merged nowhere"]);

    let after = worktree_stats(
        &work,
        Some(KnownMerge {
            head: &landed_head,
            merged: true,
        }),
    )
    .expect("stats");
    assert_ne!(
        after.head, landed.head,
        "the commit the cached answer belonged to is gone"
    );
    assert_eq!(
        after.merged,
        Some(false),
        "a stale key must force the recheck, not hand back the landed answer"
    );
}

#[test]
fn a_merged_answer_is_reused_when_head_has_not_moved() {
    let tmp = tempfile::tempdir().unwrap();
    let work = squash_merged_repo(tmp.path());
    let run = |args: &[&str]| {
        let out = git_program()
            .args(args)
            .current_dir(&work)
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?}");
    };
    run(&["checkout", "-q", "-b", "wip", "origin/main"]);
    std::fs::write(work.join("wip.txt"), "wip").unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-qm", "work in progress"]);

    let fresh = worktree_stats(&work, None).expect("stats");
    assert_eq!(fresh.merged, Some(false), "genuinely unmerged");

    let head = fresh.head.clone().expect("head");
    let cached = worktree_stats(
        &work,
        Some(KnownMerge {
            head: &head,
            merged: true,
        }),
    )
    .expect("stats");
    assert_eq!(
        cached.merged,
        Some(true),
        "the key matches HEAD, so the stored answer is taken without re-asking"
    );
}

#[test]
fn an_unmerged_answer_is_reused_while_head_stands_still() {
    let tmp = tempfile::tempdir().unwrap();
    let work = squash_merged_repo(tmp.path());
    git_in(&work, &["checkout", "-q", "-b", "wip", "origin/main"]);
    commit_file(&work, "wip.txt", "wip", "work in progress");

    let fresh = worktree_stats(&work, None).expect("stats");
    assert_eq!(fresh.merged, Some(false), "genuinely unmerged");
    let head = fresh.head.clone().expect("head");

    git_in(&work, &["remote", "remove", "origin"]);
    assert_eq!(
        worktree_stats(&work, None).and_then(|s| s.merged),
        None,
        "the control: recomputed, this worktree has no answer left to give"
    );

    let cached = worktree_stats(
        &work,
        Some(KnownMerge {
            head: &head,
            merged: false,
        }),
    )
    .expect("stats");
    assert_eq!(
        cached.merged,
        Some(false),
        "the key matches HEAD, so the stored answer stands without re-asking"
    );

    commit_file(&work, "more.txt", "more", "more work");
    assert_eq!(
        worktree_stats(
            &work,
            Some(KnownMerge {
                head: &head,
                merged: false,
            })
        )
        .and_then(|s| s.merged),
        None,
        "a stale key must force the recheck, not hand back the old answer"
    );
}

#[test]
fn an_untracked_only_status_needs_no_numstat() {
    let untracked_only = "# branch.oid abc123\n# branch.head wip\n? new.txt\n";
    let status = parse_status_v2(untracked_only);
    assert!(status.dirty, "an untracked file still dirties the tree");
    assert_eq!(status.untracked, 1);
    assert!(
        !status.tracked,
        "nothing tracked differs from HEAD, so the diff is known to be empty"
    );

    let changed = "# branch.oid abc123\n1 .M N... 100644 100644 100644 aaa bbb one.txt\n";
    assert!(
        parse_status_v2(changed).tracked,
        "a changed tracked file is exactly what the numstat is for"
    );
    let unmerged = "# branch.oid abc123\nu UU N... 100644 100644 100644 100644 a b c one.txt\n";
    assert!(parse_status_v2(unmerged).tracked, "a conflict counts too");
}

#[test]
fn an_untracked_file_reports_dirty_with_no_diff_of_its_own() {
    let tmp = tempfile::tempdir().unwrap();
    let work = pr_repo(tmp.path());
    std::fs::write(work.join("scratch.txt"), "not added").unwrap();

    let stats = worktree_stats(&work, None).expect("stats");
    assert!(stats.dirty);
    assert_eq!(stats.untracked, 1);
    assert_eq!(
        (stats.files_changed, stats.insertions, stats.deletions),
        (0, 0, 0),
        "an untracked file is in no diff against HEAD"
    );
}

#[cfg(unix)]
fn count_git<T>(at: &Path, f: impl FnOnce() -> T) -> (T, usize) {
    use std::os::unix::fs::PermissionsExt;

    let real = crate::paths::resolve_on_path("git").expect("git is on PATH");
    let bin = at.join("shim");
    std::fs::create_dir_all(&bin).unwrap();
    let log = at.join("git-calls.log");
    let _ = std::fs::remove_file(&log);
    let shim = bin.join("git");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{log}\"\nexec \"{real}\" \"$@\"\n",
            log = log.display(),
            real = real.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

    // git resolves its own helpers through the PATH it starts with.
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = crate::paths::with_path(path, f);
    let count = std::fs::read_to_string(&log)
        .map(|calls| calls.lines().count())
        .unwrap_or(0);
    (out, count)
}

#[test]
#[cfg(unix)]
fn a_polled_stat_costs_nine_subprocesses_cold_and_two_warm() {
    let tmp = tempfile::tempdir().unwrap();
    let work = squash_merged_repo(tmp.path());
    git_in(&work, &["remote", "set-head", "origin", "main"]);
    git_in(&work, &["checkout", "-q", "-b", "wip", "origin/main"]);
    commit_file(&work, "wip.txt", "wip", "work in progress");
    std::fs::write(work.join("wip.txt"), "wip, edited").unwrap();

    let (cold, cold_calls) = count_git(tmp.path(), || worktree_stats(&work, None));
    let cold = cold.expect("stats");
    assert_eq!(cold.merged, Some(false), "the stat reached the merge check");
    assert_eq!(
        cold_calls, 9,
        "status + numstat + the merge check's seven: the cost of every single \
         poll before the answer could be remembered"
    );

    let head = cold.head.clone().expect("head");
    let (warm, warm_calls) = count_git(tmp.path(), || {
        worktree_stats(
            &work,
            Some(KnownMerge {
                head: &head,
                merged: false,
            }),
        )
    });
    assert_eq!(warm.expect("stats").merged, Some(false), "same answer");
    assert_eq!(
        warm_calls, 2,
        "with the answer in hand a poll is the status and the numstat"
    );

    git_in(&work, &["checkout", "-q", "--", "wip.txt"]);
    let (clean, clean_calls) = count_git(tmp.path(), || {
        worktree_stats(
            &work,
            Some(KnownMerge {
                head: &head,
                merged: false,
            }),
        )
    });
    let clean = clean.expect("stats");
    assert!(!clean.dirty);
    assert_eq!(clean_calls, 1, "one status, and it answered everything");
}

#[test]
fn a_new_directory_is_created_empty_or_initialised() {
    let dir = tempfile::tempdir().expect("tempdir");
    let empty = dir.path().join("deep/er/empty");
    create_repo_dir(&empty, &NewRepo::Empty).expect("empty");
    assert!(empty.is_dir());
    assert!(!empty.join(".git").exists());

    let init = dir.path().join("fresh");
    create_repo_dir(&init, &NewRepo::Init).expect("init");
    assert!(is_git_repo(&init));
}

#[test]
fn a_clone_lands_in_the_new_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source");
    seed_repo(&source, "README.md", "hello");
    let target = dir.path().join("code/cloned");
    create_repo_dir(
        &target,
        &NewRepo::Clone {
            url: source.to_string_lossy().to_string(),
        },
    )
    .expect("clone");
    assert!(target.join("README.md").is_file());
}

#[test]
fn a_failed_clone_takes_away_the_directory_it_created() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("cloned");
    let url = dir
        .path()
        .join("no-such-repo")
        .to_string_lossy()
        .to_string();
    let error = create_repo_dir(&target, &NewRepo::Clone { url }).unwrap_err();
    assert!(format!("{error:#}").contains("clone"), "{error:#}");
    assert!(!target.exists(), "no half-made directory is left behind");
}

#[test]
fn a_directory_that_already_holds_something_is_refused_and_kept() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("taken");
    std::fs::create_dir_all(&target).expect("mkdir");
    std::fs::write(target.join("keep.txt"), "mine").expect("write");
    let error = create_repo_dir(&target, &NewRepo::Init).unwrap_err();
    assert!(format!("{error:#}").contains("not empty"), "{error:#}");
    assert!(target.join("keep.txt").is_file());
    assert!(!target.join(".git").exists());
}

#[test]
fn an_existing_empty_directory_is_used_and_kept_when_the_clone_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("mkdir-ed");
    std::fs::create_dir_all(&target).expect("mkdir");
    create_repo_dir(&target, &NewRepo::Init).expect("init into empty");
    assert!(is_git_repo(&target));

    let other = dir.path().join("also-empty");
    std::fs::create_dir_all(&other).expect("mkdir");
    let url = dir
        .path()
        .join("no-such-repo")
        .to_string_lossy()
        .to_string();
    create_repo_dir(&other, &NewRepo::Clone { url }).unwrap_err();
    assert!(
        other.is_dir(),
        "a directory the user made is not ours to remove"
    );
}

#[test]
fn a_repository_git_cannot_read_is_a_failure_not_a_plain_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(".git"), "gitdir: /nowhere/at/all\n").expect(".git");
    let read = branches(&dir.path().display().to_string());
    assert!(matches!(read, Err(BranchesUnread::Failed(_))), "{read:?}");
    let plain = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        branches(&plain.path().display().to_string()),
        Err(BranchesUnread::NotGit)
    );
}

fn behind_clone(dir: &Path, branch: &str) -> (PathBuf, PathBuf, String) {
    let upstream = dir.join("upstream");
    seed_repo(&upstream, "README", "one\n");
    run_git(
        git_command(&upstream, &["branch", "-M", branch]),
        "git branch",
    )
    .expect("rename");
    let clone = dir.join("clone");
    let mut cmd = git_program();
    cmd.arg("clone").arg("-q").arg(&upstream).arg(&clone);
    run_git(cmd, "git clone").expect("clone");
    std::fs::write(upstream.join("NEW"), "two\n").expect("write");
    for args in [vec!["add", "NEW"], vec!["commit", "-q", "-m", "moved"]] {
        run_git(git_command(&upstream, &args), "git").expect("advance upstream");
    }
    let moved = head_commit(&upstream).expect("head");
    (upstream, clone, moved)
}

#[test]
fn the_default_base_is_the_freshly_fetched_origin_default_branch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_, clone, moved) = behind_clone(dir.path(), "main");

    fetch_origin(&clone.display().to_string(), Duration::from_secs(10)).expect("fetch");
    let read = branches(&clone.display().to_string()).expect("branches");

    assert_eq!(read.default.as_deref(), Some("origin/main"), "{read:?}");
    assert!(read.names.iter().any(|n| n == "main"), "{read:?}");
    assert!(read.names.iter().any(|n| n == "origin/main"), "{read:?}");
    assert!(!read.names.iter().any(|n| n.ends_with("HEAD")), "{read:?}");
    let fetched = run_git_capture(&["rev-parse", "origin/main"], &clone).expect("origin/main");
    assert_eq!(fetched.trim(), moved, "origin/main is the fetched tip");
}

#[test]
fn the_default_follows_an_upstream_that_changed_its_default_branch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (upstream, clone, _) = behind_clone(dir.path(), "main");
    for args in [
        vec!["checkout", "-q", "-b", "develop"],
        vec!["commit", "-q", "--allow-empty", "-m", "develop"],
    ] {
        run_git(git_command(&upstream, &args), "git").expect("develop");
    }

    fetch_origin(&clone.display().to_string(), Duration::from_secs(10)).expect("fetch");
    let read = branches(&clone.display().to_string()).expect("branches");

    assert_eq!(read.default.as_deref(), Some("origin/develop"), "{read:?}");
}

#[test]
fn without_origin_head_the_default_is_origin_main_then_master() {
    for branch in ["main", "master"] {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_, clone, _) = behind_clone(dir.path(), branch);
        run_git(
            git_command(&clone, &["remote", "set-head", "origin", "-d"]),
            "git remote set-head",
        )
        .expect("drop origin/HEAD");
        let read = branches(&clone.display().to_string()).expect("branches");
        assert_eq!(read.default, Some(format!("origin/{branch}")), "{read:?}");
    }
}

#[test]
fn a_fetch_from_an_origin_that_never_answers_gives_up() {
    let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = silent.local_addr().expect("addr").port();
    let dir = tempfile::tempdir().expect("tempdir");
    seed_repo(dir.path(), "README", "one\n");
    let url = format!("http://127.0.0.1:{port}/repo.git");
    run_git(
        git_command(dir.path(), &["remote", "add", "origin", &url]),
        "git remote add",
    )
    .expect("origin");

    let started = std::time::Instant::now();
    let fetched = fetch_origin(&dir.path().display().to_string(), Duration::from_secs(1));

    assert!(fetched.is_err(), "{fetched:?}");
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "gave up after {:?}",
        started.elapsed()
    );
    drop(silent);
}

#[cfg(unix)]
#[test]
fn a_fetch_keeps_the_repositorys_own_ssh_command() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    seed_repo(&repo, "README", "one\n");
    let marker = dir.path().join("used");
    let wrapper = dir.path().join("ssh-wrapper");
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    )
    .expect("wrapper");
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    for args in [
        vec!["remote", "add", "origin", "ssh://example.invalid/repo.git"],
        vec![
            "config",
            "core.sshCommand",
            wrapper.to_str().expect("utf-8"),
        ],
    ] {
        run_git(git_command(&repo, &args), "git").expect("configure");
    }

    let _ = fetch_origin(&repo.display().to_string(), Duration::from_secs(1));

    assert!(marker.exists(), "the configured ssh command was not run");
}

#[test]
fn a_repository_with_no_origin_fetches_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_repo(dir.path(), "README", "one\n");
    assert_eq!(
        fetch_origin(&dir.path().display().to_string(), Duration::from_secs(1)),
        Ok(())
    );
}

#[test]
fn a_repository_with_no_origin_defaults_to_its_checked_out_branch() {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_repo(dir.path(), "README", "one\n");
    let read = branches(&dir.path().display().to_string()).expect("branches");
    assert_eq!(read.default.as_deref(), Some("main"));
    assert_eq!(read.names, vec!["main".to_string()]);
}

#[test]
fn a_logged_git_failure_keeps_only_its_first_reportable_line() {
    let stderr = b"** WARNING: connection is not using a post-quantum key exchange algorithm.\n\
warning: Not a git repository. Use --no-index to compare two paths outside a working tree\n\
usage: git diff --no-index [<options>] <path> <path>\n\
\n\
Diff output format options\n";
    assert_eq!(
        reportable_stderr_line(stderr),
        "warning: Not a git repository. Use --no-index to compare two paths outside a working tree"
    );
    assert_eq!(reportable_stderr_line(b""), "");
}

#[test]
fn a_plain_directory_has_an_empty_working_diff() {
    let plain = tempfile::tempdir().expect("tempdir");
    let diff = working_diff(plain.path()).expect("a plain directory still answers");
    assert_eq!(
        (
            diff.body.as_str(),
            diff.numstat_z.as_str(),
            diff.name_status_z.as_str()
        ),
        ("", "", "")
    );
    assert_eq!((diff.untracked_total, diff.untracked_shown), (0, 0));
}

#[test]
fn only_git_saying_not_a_repository_reads_as_outside_a_work_tree() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let plain = scratch.path().join("plain");
    let repo = scratch.path().join("repo");
    std::fs::create_dir_all(&plain).expect("mkdir");
    seed_repo(&repo, "a.txt", "one");
    let probe = |dir: &Path, env: &[(&str, &str)]| {
        let mut cmd = git_command(dir, &["rev-parse", "--is-inside-work-tree"]);
        cmd.envs(env.iter().copied());
        work_tree_answer(&cmd.output().expect("git runs"))
    };

    assert_eq!(probe(&plain, &[]), WorkTree::Outside);
    assert_eq!(probe(&repo.join(".git"), &[]), WorkTree::Outside);
    assert_eq!(probe(&repo, &[]), WorkTree::Inside);
    assert_eq!(
        probe(
            &repo,
            &[
                ("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1"),
                ("GIT_CONFIG_GLOBAL", "/dev/null"),
                ("GIT_CONFIG_NOSYSTEM", "1"),
            ]
        ),
        WorkTree::Unknown,
        "a repository git refuses to read is not a directory without changes"
    );
}

#[test]
fn a_translated_git_still_reads_a_plain_directory_as_outside_a_work_tree() {
    let plain = tempfile::tempdir().expect("tempdir");
    let mut probe = work_tree_probe(plain.path());
    probe.env("LANGUAGE", "de");
    assert_eq!(
        work_tree_answer(&probe.output().expect("git runs")),
        WorkTree::Outside
    );
}
