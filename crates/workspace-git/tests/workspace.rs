//! workspace-git against real git repositories in temporary directories: the
//! rules docs/runtime/README.md gives a workspace, one by one.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use workspace_git::{Answer, Config, make, release};

/// The ambient variables that pin git to another repository. A test run
/// under a pre-commit hook inherits them.
const GIT_LOCATION_ENV: [&str; 10] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
    "GIT_NAMESPACE",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
];

fn git(dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    for var in GIT_LOCATION_ENV {
        cmd.env_remove(var);
    }
    let out = cmd
        .current_dir(dir)
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Sandbox {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    cfg: Config,
}

impl Sandbox {
    fn new() -> Sandbox {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path().canonicalize().expect("canonical tempdir");
        let cfg = Config::new(base.join("root"));
        Sandbox {
            _tmp: tmp,
            base,
            cfg,
        }
    }

    /// A repository with one commit on `main` holding `README`.
    fn origin(&self, name: &str) -> PathBuf {
        let dir = self.base.join(name);
        fs::create_dir_all(&dir).expect("origin dir");
        git(&dir, &["init", "-q"]);
        fs::write(dir.join("README"), format!("{name}\n")).expect("README");
        git(&dir, &["add", "README"]);
        git(&dir, &["commit", "-q", "-m", "first"]);
        dir
    }

    /// A branch of `origin` holding `file`, `main` left checked out.
    fn branch(&self, origin: &Path, branch: &str, file: &str) {
        git(origin, &["checkout", "-q", "-b", branch]);
        fs::write(origin.join(file), branch).expect("branch file");
        git(origin, &["add", file]);
        git(origin, &["commit", "-q", "-m", branch]);
        git(origin, &["checkout", "-q", "main"]);
    }

    fn make(&self, key: &str, urls: &[String]) -> Answer {
        make(&self.cfg, key, urls).expect("an answer")
    }

    fn made(&self, key: &str, urls: &[String]) -> (Vec<String>, bool) {
        match self.make(key, urls) {
            Answer::Made { dirs, adopted } => (dirs, adopted),
            other => panic!("expected Made, got {other:?}"),
        }
    }

    /// The one source repository workspace-git keeps.
    fn source(&self) -> PathBuf {
        let mut sources: Vec<PathBuf> = fs::read_dir(self.cfg.root.join(".sources"))
            .expect("sources")
            .map(|e| e.expect("entry").path())
            .collect();
        assert_eq!(sources.len(), 1, "one source: {sources:?}");
        sources.remove(0)
    }
}

fn url(scheme: &str, path: &Path) -> String {
    format!("{scheme}://{}", path.display())
}

fn branch_of(dir: &str) -> String {
    git(Path::new(dir), &["branch", "--show-current"])
}

#[test]
fn one_git_url_makes_a_worktree_on_a_branch_named_after_the_key() {
    let s = Sandbox::new();
    let origin = s.origin("app");
    let (dirs, adopted) = s.made("t.one", &[url("git+file", &origin)]);
    assert!(!adopted);
    assert_eq!(dirs, [s.cfg.root.join("t.one/app").display().to_string()]);
    assert_eq!(
        fs::read_to_string(Path::new(&dirs[0]).join("README")).unwrap(),
        "app\n"
    );
    assert_eq!(branch_of(&dirs[0]), "t.one");
    assert!(s.cfg.root.join("t.one/.fleet-workspace.json").is_file());
}

#[test]
fn a_second_call_with_the_key_adopts_the_same_directories() {
    let s = Sandbox::new();
    let origin = s.origin("app");
    let urls = [url("git+file", &origin)];
    let (first, _) = s.made("t.again", &urls);
    let (second, adopted) = s.made("t.again", &urls);
    assert!(adopted);
    assert_eq!(first, second);
}

#[test]
fn the_same_key_with_other_urls_fails() {
    let s = Sandbox::new();
    let app = s.origin("app");
    let lib = s.origin("lib");
    s.made("t.other", &[url("git+file", &app)]);
    match s.make("t.other", &[url("git+file", &lib)]) {
        Answer::Failed { why } => assert!(why.contains("other URLs"), "{why}"),
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn several_urls_answer_one_directory_each_in_order() {
    let s = Sandbox::new();
    let app = s.origin("app");
    let lib = s.origin("lib");
    let notes = s.base.join("notes");
    fs::create_dir_all(&notes).unwrap();
    let (dirs, _) = s.made(
        "t.many",
        &[
            url("git+file", &app),
            url("git+file", &lib),
            url("file", &notes),
        ],
    );
    assert_eq!(
        dirs,
        [
            s.cfg.root.join("t.many/app").display().to_string(),
            s.cfg.root.join("t.many/lib").display().to_string(),
            notes.display().to_string(),
        ]
    );
    assert_eq!(
        fs::read_to_string(Path::new(&dirs[1]).join("README")).unwrap(),
        "lib\n"
    );
}

#[test]
fn a_branch_query_checks_out_an_existing_branch() {
    let s = Sandbox::new();
    let origin = s.origin("app");
    s.branch(&origin, "feat-x", "x.txt");
    let (dirs, _) = s.made(
        "t.existing",
        &[format!("{}?branch=feat-x#main", url("git+file", &origin))],
    );
    assert_eq!(branch_of(&dirs[0]), "feat-x");
    assert!(
        Path::new(&dirs[0]).join("x.txt").is_file(),
        "the branch's own file"
    );
}

#[test]
fn a_branch_query_makes_a_new_branch_from_the_fragment() {
    let s = Sandbox::new();
    let origin = s.origin("app");
    s.branch(&origin, "v2", "v2.txt");
    let (dirs, _) = s.made(
        "t.new",
        &[format!("{}?branch=fresh#v2", url("git+file", &origin))],
    );
    assert_eq!(branch_of(&dirs[0]), "fresh");
    assert!(Path::new(&dirs[0]).join("v2.txt").is_file(), "made from v2");
}

#[test]
fn a_fragment_alone_bases_the_key_branch_on_it() {
    let s = Sandbox::new();
    let origin = s.origin("app");
    s.branch(&origin, "v2", "v2.txt");
    let (dirs, _) = s.made("t.frag", &[format!("{}#v2", url("git+file", &origin))]);
    assert_eq!(branch_of(&dirs[0]), "t.frag");
    assert!(Path::new(&dirs[0]).join("v2.txt").is_file());
}

#[test]
fn a_branch_checked_out_elsewhere_gets_a_numbered_one() {
    let s = Sandbox::new();
    let origin = s.origin("app");
    let u = format!("{}?branch=shared", url("git+file", &origin));
    let (one, _) = s.made("t.first", std::slice::from_ref(&u));
    let (two, _) = s.made("t.second", &[u]);
    assert_eq!(branch_of(&one[0]), "shared");
    assert_eq!(branch_of(&two[0]), "shared-2");
}

#[test]
fn a_dirty_release_is_preserved_and_a_git_file_url_restores_it() {
    let s = Sandbox::new();
    let origin = s.origin("app");
    let (dirs, _) = s.made("t.dirty", &[url("git+file", &origin)]);
    let tree = PathBuf::from(&dirs[0]);
    fs::write(tree.join("README"), "edited\n").unwrap();
    fs::write(tree.join("new.txt"), "untracked\n").unwrap();
    release(&s.cfg, "t.dirty").expect("release");
    assert!(!tree.exists(), "the worktree goes");
    assert!(
        !s.cfg.root.join("t.dirty").exists(),
        "the key's directory goes"
    );
    let source = s.source();
    let preserved = "refs/fleet/preserved/t.dirty/app";
    git(&source, &["rev-parse", "--verify", preserved]);
    git(&source, &["rev-parse", "--verify", "refs/heads/t.dirty"]);

    let restore = format!("{}#{preserved}", url("git+file", &source));
    let (back, _) = s.made("t.restored", &[restore]);
    let tree = Path::new(&back[0]);
    assert_eq!(fs::read_to_string(tree.join("README")).unwrap(), "edited\n");
    assert_eq!(
        fs::read_to_string(tree.join("new.txt")).unwrap(),
        "untracked\n"
    );
}

#[test]
fn a_clean_release_preserves_nothing_and_keeps_the_branch() {
    let s = Sandbox::new();
    let origin = s.origin("app");
    s.made("t.clean", &[url("git+file", &origin)]);
    release(&s.cfg, "t.clean").expect("release");
    let source = s.source();
    assert_eq!(git(&source, &["for-each-ref", "refs/fleet/preserved"]), "");
    git(&source, &["rev-parse", "--verify", "refs/heads/t.clean"]);
}

#[test]
fn a_file_url_is_used_as_it_is_and_survives_release() {
    let s = Sandbox::new();
    let notes = s.base.join("my notes");
    fs::create_dir_all(&notes).unwrap();
    fs::write(notes.join("n.txt"), "keep").unwrap();
    let (dirs, _) = s.made(
        "t.file",
        &[format!("file://{}", s.base.join("my%20notes").display())],
    );
    assert_eq!(dirs, [notes.display().to_string()]);
    let (_, adopted) = s.made(
        "t.file",
        &[format!("file://{}", s.base.join("my%20notes").display())],
    );
    assert!(adopted);
    release(&s.cfg, "t.file").expect("release");
    assert_eq!(fs::read_to_string(notes.join("n.txt")).unwrap(), "keep");
}

#[test]
fn a_scheme_it_does_not_handle_is_unsupported() {
    let s = Sandbox::new();
    let answer = s.make("t.hg", &["hg+https://example.invalid/r".to_string()]);
    assert!(
        matches!(&answer, Answer::Unsupported { url } if url == "hg+https://example.invalid/r"),
        "{answer:?}"
    );
}

#[test]
fn a_partial_workspace_is_rolled_back_and_made_again() {
    let s = Sandbox::new();
    let origin = s.origin("app");
    fs::create_dir_all(s.cfg.root.join("t.partial/app")).unwrap();
    fs::write(s.cfg.root.join("t.partial/app/stale"), "x").unwrap();
    let (dirs, adopted) = s.made("t.partial", &[url("git+file", &origin)]);
    assert!(!adopted);
    assert!(!Path::new(&dirs[0]).join("stale").exists());
    assert_eq!(branch_of(&dirs[0]), "t.partial");
}

#[test]
fn a_failing_url_rolls_back_what_was_made() {
    let s = Sandbox::new();
    let origin = s.origin("app");
    let answer = s.make(
        "t.fails",
        &[
            url("git+file", &origin),
            url("git+file", &s.base.join("missing")),
        ],
    );
    assert!(matches!(answer, Answer::Failed { .. }), "{answer:?}");
    assert!(
        !s.cfg.root.join("t.fails").exists(),
        "nothing of the key is left"
    );
    let worktrees = git(&s.source(), &["worktree", "list", "--porcelain"]);
    assert!(!worktrees.contains("t.fails"), "{worktrees}");
}

#[test]
fn preserved_work_past_its_retention_is_pruned_on_the_next_call() {
    let mut s = Sandbox::new();
    s.cfg.preserve_days = 0;
    let origin = s.origin("app");
    let (dirs, _) = s.made("t.old", &[url("git+file", &origin)]);
    fs::write(Path::new(&dirs[0]).join("w.txt"), "w").unwrap();
    release(&s.cfg, "t.old").expect("release");
    let source = s.source();
    git(
        &source,
        &["rev-parse", "--verify", "refs/fleet/preserved/t.old/app"],
    );
    s.made("t.next", &[url("git+file", &origin)]);
    assert_eq!(git(&source, &["for-each-ref", "refs/fleet/preserved"]), "");
}

#[test]
fn a_released_branch_origin_already_has_is_pruned() {
    let s = Sandbox::new();
    let origin = s.origin("app");
    s.made("t.merged", &[url("git+file", &origin)]);
    release(&s.cfg, "t.merged").expect("release");
    // The branch's head is origin's main: origin already has it.
    s.made("t.after", &[url("git+file", &origin)]);
    let heads = git(
        &s.source(),
        &["for-each-ref", "--format=%(refname)", "refs/heads"],
    );
    assert!(!heads.contains("t.merged"), "{heads}");
    assert!(heads.contains("t.after"), "a branch in use stays: {heads}");
}

#[test]
fn the_program_answers_a_make_and_a_release_as_json() {
    let s = Sandbox::new();
    let origin = s.origin("app");
    let run = |verb: &str, request: serde_json::Value| -> serde_json::Value {
        let mut child = Command::new(env!("CARGO_BIN_EXE_workspace-git"))
            .arg(verb)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("run workspace-git");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(request.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        serde_json::from_slice(&out.stdout).expect("JSON")
    };
    let root = s.cfg.root.display().to_string();
    let made = run(
        "make",
        serde_json::json!({"root": root, "key": "t.cli", "urls": [url("git+file", &origin)]}),
    );
    assert_eq!(made["made"]["adopted"], false, "{made}");
    assert_eq!(
        made["made"]["dirs"][0],
        s.cfg.root.join("t.cli/app").display().to_string()
    );
    let released = run("release", serde_json::json!({"root": root, "key": "t.cli"}));
    assert_eq!(released, serde_json::json!("released"));
    assert!(!s.cfg.root.join("t.cli").exists());
}

#[test]
fn a_discard_removes_the_workspace_and_preserves_nothing() {
    let s = Sandbox::new();
    let origin = s.origin("app");
    let (dirs, _) = s.made("t.discard", &[url("git+file", &origin)]);
    fs::write(Path::new(&dirs[0]).join("setup.txt"), "made by setup").unwrap();
    workspace_git::discard(&s.cfg, "t.discard").expect("discard");
    assert!(!s.cfg.root.join("t.discard").exists());
    assert_eq!(
        git(&s.source(), &["for-each-ref", "refs/fleet/preserved"]),
        ""
    );
}

#[test]
fn inspect_answers_the_urls_and_directories_of_a_whole_workspace() {
    let s = Sandbox::new();
    let origin = s.origin("app");
    let urls = [url("git+file", &origin)];
    assert_eq!(
        workspace_git::inspect(&s.cfg, "t.seen").expect("inspect"),
        None
    );
    let (dirs, _) = s.made("t.seen", &urls);
    assert_eq!(
        workspace_git::inspect(&s.cfg, "t.seen").expect("inspect"),
        Some(workspace_git::Workspace {
            urls: urls.to_vec(),
            dirs
        })
    );
}
