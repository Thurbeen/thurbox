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
        let base = plain(tmp.path().canonicalize().expect("canonical tempdir"));
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

    /// A bare repository cloned from a new origin: a URL to it gets a
    /// source of its own, as a remote does.
    fn bare(&self, name: &str) -> PathBuf {
        let origin = self.origin(&format!("{name}-work"));
        let bare = self.base.join(format!("{name}.git"));
        git(
            &self.base,
            &[
                "clone",
                "-q",
                "--bare",
                &origin.display().to_string(),
                &bare.display().to_string(),
            ],
        );
        bare
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
            .filter(|p| p.is_dir())
            .collect();
        assert_eq!(sources.len(), 1, "one source: {sources:?}");
        sources.remove(0)
    }
}

/// `<scheme>://<path>`, spelled as a URL spells a Windows path: `/C:/…`.
fn url(scheme: &str, path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("{scheme}://{text}")
    } else {
        format!("{scheme}:///{text}")
    }
}

/// `path` without Windows' `\\?\` prefix, which `canonicalize` adds and
/// a URL cannot spell.
fn plain(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        Some(rest) => PathBuf::from(rest),
        None => path,
    }
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
    assert_eq!(
        dirs,
        [s.cfg.root.join("t.one").join("app").display().to_string()]
    );
    assert_eq!(
        fs::read_to_string(Path::new(&dirs[0]).join("README"))
            .unwrap()
            .replace("\r\n", "\n"),
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
            s.cfg.root.join("t.many").join("app").display().to_string(),
            s.cfg.root.join("t.many").join("lib").display().to_string(),
            notes.display().to_string(),
        ]
    );
    assert_eq!(
        fs::read_to_string(Path::new(&dirs[1]).join("README"))
            .unwrap()
            .replace("\r\n", "\n"),
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
    let origin = s.bare("app");
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
    // A checkout follows the user's git config: `core.autocrlf` on a
    // Windows runner writes CRLF.
    let text = |name: &str| {
        fs::read_to_string(tree.join(name))
            .unwrap()
            .replace("\r\n", "\n")
    };
    assert_eq!(text("README"), "edited\n");
    assert_eq!(text("new.txt"), "untracked\n");
}

#[test]
fn a_clean_release_preserves_nothing_and_keeps_the_branch() {
    let s = Sandbox::new();
    let origin = s.bare("app");
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
    let (dirs, _) = s.made("t.file", &[url("file", &s.base.join("my%20notes"))]);
    assert_eq!(dirs, [notes.display().to_string()]);
    let (_, adopted) = s.made("t.file", &[url("file", &s.base.join("my%20notes"))]);
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
    let worktrees = git(&origin, &["worktree", "list", "--porcelain"]);
    assert!(!worktrees.contains("t.fails"), "{worktrees}");
}

#[test]
fn preserved_work_past_its_retention_is_pruned_on_the_next_call() {
    let mut s = Sandbox::new();
    s.cfg.preserve_days = 0;
    let origin = s.bare("app");
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
    let origin = s.bare("app");
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
        s.cfg.root.join("t.cli").join("app").display().to_string()
    );
    let released = run("release", serde_json::json!({"root": root, "key": "t.cli"}));
    assert_eq!(released, serde_json::json!("released"));
    assert!(!s.cfg.root.join("t.cli").exists());
}

#[test]
fn a_discard_removes_the_workspace_and_preserves_nothing() {
    let s = Sandbox::new();
    let origin = s.bare("app");
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

/// Two runtimes on one root, such as two Fleets on different sockets, may
/// make workspaces of one new repository at once: neither may remove the
/// source the other is filling.
#[test]
fn concurrent_makes_of_one_new_repository_all_succeed() {
    for round in 0..4 {
        let sandbox = Sandbox::new();
        let origin = sandbox.origin("app");
        let url = url("git+file", &origin);
        let answers: Vec<Answer> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..6)
                .map(|i| {
                    let cfg = sandbox.cfg.clone();
                    let url = url.clone();
                    scope.spawn(move || make(&cfg, &format!("k.{i}"), &[url]).expect("answer"))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("thread"))
                .collect()
        });
        for answer in answers {
            assert!(
                matches!(answer, Answer::Made { .. }),
                "round {round}: {answer:?}"
            );
        }
    }
}

/// A `git+file` URL naming a local checkout gets a worktree of the checkout
/// itself: the checkout lists it, and a commit made in it is in the
/// checkout at once. No source of its own is made.
#[test]
fn a_local_checkout_gets_a_worktree_of_itself() {
    let s = Sandbox::new();
    let origin = s.origin("app");
    let (dirs, _) = s.made(
        "t.co",
        &[format!("{}?branch=feature#main", url("git+file", &origin))],
    );
    let tree = PathBuf::from(&dirs[0]);
    let listed = git(&origin, &["worktree", "list", "--porcelain"]);
    assert!(
        listed.contains(&format!("worktree {}", tree.display())),
        "the checkout lists the worktree: {listed}"
    );
    fs::write(tree.join("work.txt"), "work\n").unwrap();
    git(&tree, &["add", "work.txt"]);
    git(&tree, &["commit", "-q", "-m", "work"]);
    assert_eq!(
        git(&origin, &["log", "-1", "--format=%s", "feature"]),
        "work",
        "the commit is on the checkout's own branch"
    );
    assert!(
        !s.cfg
            .root
            .join(".sources")
            .read_dir()
            .map(|mut d| d.any(|e| e.unwrap().path().is_dir()))
            .unwrap_or(false),
        "no source of its own"
    );
}

/// An existing branch of a checkout is the real one: a commit in the
/// worktree moves the checkout's branch.
#[test]
fn an_existing_branch_of_a_checkout_is_its_real_branch() {
    let s = Sandbox::new();
    let origin = s.origin("app");
    s.branch(&origin, "feat-x", "x.txt");
    let (dirs, _) = s.made(
        "t.real",
        &[format!("{}?branch=feat-x#main", url("git+file", &origin))],
    );
    let tree = PathBuf::from(&dirs[0]);
    assert_eq!(branch_of(&dirs[0]), "feat-x");
    fs::write(tree.join("more.txt"), "more\n").unwrap();
    git(&tree, &["add", "more.txt"]);
    git(&tree, &["commit", "-q", "-m", "more"]);
    assert_eq!(
        git(&origin, &["log", "-1", "--format=%s", "feat-x"]),
        "more"
    );
}

/// A release of a checkout's worktree keeps its uncommitted work in the
/// checkout, removes the worktree and keeps the branch.
#[test]
fn a_release_preserves_a_checkouts_dirty_work_in_the_checkout() {
    let s = Sandbox::new();
    let origin = s.origin("app");
    let (dirs, _) = s.made(
        "t.keep",
        &[format!("{}?branch=keep#main", url("git+file", &origin))],
    );
    let tree = PathBuf::from(&dirs[0]);
    fs::write(tree.join("draft.txt"), "draft\n").unwrap();
    release(&s.cfg, "t.keep").expect("release");
    assert!(!tree.exists(), "the worktree goes");
    let listed = git(&origin, &["worktree", "list", "--porcelain"]);
    assert!(!listed.contains("t.keep"), "{listed}");
    let preserved = "refs/fleet/preserved/t.keep/app";
    assert_eq!(
        git(&origin, &["show", &format!("{preserved}:draft.txt")]),
        "draft"
    );
    git(&origin, &["rev-parse", "--verify", "refs/heads/keep"]);
    assert!(
        git(&origin, &["status", "--porcelain"]).is_empty(),
        "nothing is left in the user's working tree"
    );
}

/// The lock file workspace-git takes for `repository` under `root`.
fn repository_lock(root: &Path, repository: &Path) -> fs::File {
    use sha2::{Digest, Sha256};
    let digest: String = Sha256::digest(repository.display().to_string().as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    fs::create_dir_all(root.join(".sources")).unwrap();
    fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(".sources").join(format!("{digest}.lock")))
        .unwrap()
}

/// The retention never touches a repository another call holds: a make that
/// finds one locked leaves its preserved work for a later make.
#[test]
fn the_retention_leaves_a_repository_another_call_holds() {
    let mut s = Sandbox::new();
    s.cfg.preserve_days = 0;
    let remote = s.bare("app");
    let (dirs, _) = s.made("t.held", &[url("git+file", &remote)]);
    fs::write(Path::new(&dirs[0]).join("w.txt"), "w").unwrap();
    release(&s.cfg, "t.held").expect("release");
    let source = s.source();
    let preserved = "refs/fleet/preserved/t.held/app";
    let other = s.origin("other");

    let held = repository_lock(&s.cfg.root, &source);
    held.lock().unwrap();
    s.made("t.during", &[url("git+file", &other)]);
    git(&source, &["rev-parse", "--verify", preserved]);
    drop(held);

    s.made("t.after", &[url("git+file", &other)]);
    assert_eq!(git(&source, &["for-each-ref", PRESERVED_PREFIX]), "");
}

const PRESERVED_PREFIX: &str = "refs/fleet/preserved";
