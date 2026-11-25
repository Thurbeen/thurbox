use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

pub const MAX_DIFF_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSummary {
    pub path: String,
    pub added: usize,
    pub removed: usize,
    pub status: &'static str,
    pub old_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Diff {
    Pending,
    Ready {
        files: Vec<FileSummary>,
        body: Vec<String>,
        truncated: bool,
        raw_bytes: usize,
        untracked_omitted: usize,
    },
    Failed(String),
}

struct Computed {
    session: String,
    request: u64,
    diff: Diff,
}

pub fn over_ssh(backend: &str) -> bool {
    crate::session::Route::parse(backend)
        .is_ok_and(|route| route.via() == Some(crate::session::Via::Ssh))
}

const DIFF_TTL: std::time::Duration = std::time::Duration::from_secs(5);
const DIFF_RETRY: std::time::Duration = std::time::Duration::from_secs(3);

struct Held {
    request: u64,
    at: std::time::Instant,
    diff: Diff,
    refreshing: bool,
}

impl Held {
    fn stale(&self) -> bool {
        if self.refreshing {
            return false;
        }
        match self.diff {
            Diff::Pending => false,
            Diff::Failed(_) => self.at.elapsed() >= DIFF_RETRY,
            Diff::Ready { .. } => self.at.elapsed() >= DIFF_TTL,
        }
    }
}

pub struct DiffStore {
    next_request: u64,
    version: u64,
    diffs: HashMap<String, Held>,
    tx: Sender<Computed>,
    rx: Receiver<Computed>,
}

impl DiffStore {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            next_request: 0,
            version: 0,
            diffs: HashMap::new(),
            tx,
            rx,
        }
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    fn mark_changed(&mut self) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        self.version = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn retain_selected(&mut self, selected: Option<&str>) -> bool {
        let before = self.diffs.len();
        self.diffs.retain(|id, _| Some(id.as_str()) == selected);
        let changed = before != self.diffs.len();
        if changed {
            self.mark_changed();
        }
        changed
    }

    pub fn get(&self, session: &str) -> Option<&Diff> {
        self.diffs.get(session).map(|held| &held.diff)
    }

    pub fn request_row(&mut self, row: &crate::kernel::snapshot::SessionRow) {
        if !row.workspace_reachable() {
            let why = format!(
                "this Task's workspace is on {}, which this machine does not reach",
                row.remote_host.as_deref().unwrap_or("another host")
            );
            self.hold(&row.id, Diff::Failed(why));
            return;
        }
        if let Some(cwd) = row.cwd.clone() {
            self.request(&row.id, cwd, row.base_branch.clone(), &row.backend);
        }
    }

    pub fn request(
        &mut self,
        session: &str,
        worktree: PathBuf,
        base: Option<String>,
        backend: &str,
    ) {
        self.retain_selected(Some(session));
        if self.diffs.get(session).is_some_and(|held| !held.stale()) {
            return;
        }
        self.next_request = self.next_request.wrapping_add(1);
        let request = self.next_request;
        match self.diffs.get_mut(session) {
            Some(held) => {
                held.refreshing = true;
                held.request = request;
            }
            None => {
                self.mark_changed();
                self.diffs.insert(
                    session.to_string(),
                    Held {
                        request,
                        at: std::time::Instant::now(),
                        diff: Diff::Pending,
                        refreshing: false,
                    },
                );
            }
        }

        let unreachable = crate::session::Route::is_remote_key(backend);

        let tx = self.tx.clone();
        let session = session.to_string();
        std::thread::spawn(move || {
            let diff = if unreachable {
                Diff::Failed(
                    "this session runs on another machine; thurbox reads diffs on this one only"
                        .to_string(),
                )
            } else {
                compute(&worktree, base.as_deref())
            };
            let _ = tx.send(Computed {
                session,
                request,
                diff,
            });
        });
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(done) = self.rx.try_recv() {
            let Some(held) = self.diffs.get_mut(&done.session) else {
                continue;
            };
            if held.request != done.request {
                continue;
            }
            held.at = std::time::Instant::now();
            held.refreshing = false;
            let diff = done.diff;
            let moved = held.diff != diff;
            held.diff = diff;
            changed |= moved;
        }
        if changed {
            self.mark_changed();
        }
        changed
    }

    #[doc(hidden)]
    pub fn set_for_test(&mut self, session: &str, diff: Diff) {
        self.hold(session, diff);
    }

    fn hold(&mut self, session: &str, diff: Diff) {
        if self.get(session) == Some(&diff) {
            return;
        }
        self.mark_changed();
        self.diffs.insert(
            session.to_string(),
            Held {
                request: 0,
                at: std::time::Instant::now(),
                diff,
                refreshing: false,
            },
        );
    }

    pub fn invalidate(&mut self, session: &str) {
        if self.diffs.remove(session).is_some() {
            self.mark_changed();
        }
    }
}

impl Default for DiffStore {
    fn default() -> Self {
        Self::new()
    }
}

fn compute(worktree: &Path, base: Option<&str>) -> Diff {
    let sources = match read_sources(worktree, base) {
        Ok(sources) => sources,
        Err(failure) => return Diff::Failed(failure.to_string()),
    };
    let Sources {
        raw,
        numstat,
        name_status,
        untracked_omitted,
    } = sources;

    let truncated = raw.len() > MAX_DIFF_BYTES;
    let raw_bytes = raw.len();
    let raw = if truncated {
        // Cut at a newline byte: slicing a `String` mid-codepoint panics.
        match raw.as_bytes()[..MAX_DIFF_BYTES]
            .iter()
            .rposition(|byte| *byte == b'\n')
        {
            Some(at) => raw[..at].to_string(),
            None => String::new(),
        }
    } else {
        raw
    };

    let files = crate::session::review::parse_changed_files(&numstat, &name_status)
        .into_iter()
        .map(|file| FileSummary {
            path: file.path,
            added: file.added,
            removed: file.removed,
            status: file.status.glyph(),
            old_path: file.old_path,
        })
        .collect();

    Diff::Ready {
        files,
        body: raw.lines().map(str::to_string).collect(),
        truncated,
        raw_bytes,
        untracked_omitted,
    }
}

struct Sources {
    raw: String,
    numstat: String,
    name_status: String,
    untracked_omitted: usize,
}

fn read_sources(worktree: &Path, base: Option<&str>) -> Result<Sources, &'static str> {
    let Some(base) = base else {
        let working = crate::git::working_diff(worktree).ok_or("could not read the diff")?;
        return Ok(Sources {
            raw: working.body,
            numstat: working.numstat_z,
            name_status: working.name_status_z,
            untracked_omitted: working
                .untracked_total
                .saturating_sub(working.untracked_shown),
        });
    };
    let raw = crate::git::diff_against(worktree, base).ok_or("could not read the diff")?;
    let (numstat, name_status) = crate::git::diff_numstat(worktree, Some(base))
        .zip(crate::git::diff_name_status(worktree, Some(base)))
        .ok_or("could not list the changed files")?;
    Ok(Sources {
        raw,
        numstat,
        name_status,
        untracked_omitted: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_starts_unknown() {
        let store = DiffStore::new();
        assert!(store.get("nobody").is_none());
    }

    #[test]
    fn invalidating_lets_the_next_request_recompute() {
        let mut store = DiffStore::new();
        let ask = |store: &mut DiffStore| {
            store.request(
                "s1",
                PathBuf::from("/definitely/not/a/repo"),
                None,
                "local:tmux",
            )
        };
        ask(&mut store);
        assert_eq!(store.get("s1"), Some(&Diff::Pending));

        ask(&mut store);
        assert_eq!(store.get("s1"), Some(&Diff::Pending));

        store.invalidate("s1");
        assert!(
            store.get("s1").is_none(),
            "invalidation has to clear the entry, or the next request returns early"
        );
        ask(&mut store);
        assert_eq!(store.get("s1"), Some(&Diff::Pending), "and recomputes");
    }

    #[test]
    fn requesting_marks_it_pending_immediately() {
        let mut store = DiffStore::new();
        let started = std::time::Instant::now();
        store.request(
            "s1",
            PathBuf::from("/definitely/not/a/repo"),
            None,
            "local:tmux",
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
        assert_eq!(store.get("s1"), Some(&Diff::Pending));
    }

    #[test]
    fn pending_is_distinct_from_empty() {
        let pending = Diff::Pending;
        let empty = Diff::Ready {
            files: Vec::new(),
            body: Vec::new(),
            truncated: false,
            raw_bytes: 0,
            untracked_omitted: 0,
        };
        assert_ne!(pending, empty);
    }

    #[test]
    fn a_second_request_does_not_recompute() {
        let mut store = DiffStore::new();
        store.request("s1", PathBuf::from("/tmp"), None, "local:tmux");
        let first = store.get("s1").cloned();
        store.request("s1", PathBuf::from("/tmp"), None, "local:tmux");
        assert_eq!(store.get("s1").cloned(), first);
    }

    fn empty_ready() -> Diff {
        Diff::Ready {
            files: Vec::new(),
            body: Vec::new(),
            truncated: false,
            raw_bytes: 0,
            untracked_omitted: 0,
        }
    }

    fn held(diff: Diff, age: std::time::Duration) -> Held {
        Held {
            request: 0,
            at: std::time::Instant::now() - age,
            diff,
            refreshing: false,
        }
    }

    #[test]
    fn a_settled_diff_is_computed_again_once_it_is_old() {
        assert!(!held(empty_ready(), std::time::Duration::ZERO).stale());
        assert!(held(empty_ready(), DIFF_TTL).stale());
        assert!(!held(Diff::Failed("gone".into()), std::time::Duration::ZERO).stale());
        assert!(held(Diff::Failed("gone".into()), DIFF_RETRY).stale());
    }

    #[test]
    fn an_in_flight_diff_is_never_asked_for_twice() {
        assert!(!held(Diff::Pending, DIFF_TTL * 10).stale());
        let mut refreshing = held(empty_ready(), DIFF_TTL * 10);
        refreshing.refreshing = true;
        assert!(!refreshing.stale());
    }

    #[test]
    fn an_old_diff_is_recomputed_and_stays_published_meanwhile() {
        let mut store = DiffStore::new();
        store.set_for_test("s1", empty_ready());
        store.request(
            "s1",
            PathBuf::from("/definitely/not/a/repo"),
            None,
            "local:tmux",
        );
        assert_eq!(
            store.get("s1"),
            Some(&empty_ready()),
            "a fresh answer is reused, not recomputed"
        );

        store.diffs.get_mut("s1").unwrap().at = std::time::Instant::now() - DIFF_TTL;
        store.request(
            "s1",
            PathBuf::from("/definitely/not/a/repo"),
            None,
            "local:tmux",
        );
        assert_eq!(store.get("s1"), Some(&empty_ready()));
        assert!(store.diffs.get("s1").unwrap().refreshing);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            store.poll();
            if let Some(Diff::Failed(_)) = store.get("s1") {
                assert!(
                    !store.diffs.get("s1").unwrap().refreshing,
                    "the landed answer clears the in-flight mark"
                );
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("the recompute never ran: {:?}", store.get("s1"));
    }

    #[test]
    fn invalidating_allows_a_recompute() {
        let mut store = DiffStore::new();
        store.request("s1", PathBuf::from("/tmp"), None, "local:tmux");
        store.invalidate("s1");
        assert!(store.get("s1").is_none());
    }

    #[test]
    fn a_failing_diff_reports_rather_than_hanging() {
        let mut store = DiffStore::new();
        store.request(
            "s1",
            PathBuf::from("/definitely/not/a/repo"),
            None,
            "local:tmux",
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            store.poll();
            if let Some(Diff::Failed(_)) = store.get("s1") {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("the failure never arrived");
    }

    #[test]
    fn late_diff_workers_cannot_restore_an_evicted_answer() {
        let mut store = DiffStore::new();
        store.set_for_test("current", empty_ready());
        let version = store.version();
        store
            .tx
            .send(Computed {
                session: "evicted".into(),
                request: 1,
                diff: empty_ready(),
            })
            .unwrap();
        store
            .tx
            .send(Computed {
                session: "current".into(),
                request: 99,
                diff: Diff::Failed("obsolete".into()),
            })
            .unwrap();
        assert!(!store.poll());
        assert!(store.get("evicted").is_none());
        assert_eq!(store.get("current"), Some(&empty_ready()));
        assert_eq!(store.version(), version);
    }

    #[test]
    fn identical_diff_answers_do_not_republish_the_body() {
        let mut store = DiffStore::new();
        store.set_for_test("current", empty_ready());
        let version = store.version();
        store
            .tx
            .send(Computed {
                session: "current".into(),
                request: 0,
                diff: empty_ready(),
            })
            .unwrap();
        assert!(!store.poll());
        assert_eq!(store.version(), version);
        assert!(!store.diffs["current"].refreshing);
    }

    #[test]
    fn only_ssh_backends_are_over_ssh() {
        assert!(over_ssh("ssh:devbox:tmux"));
        assert!(over_ssh("ssh:devbox:rmux"));
        assert!(!over_ssh("local:tmux"));
        assert!(!over_ssh("wsl:Ubuntu:tmux"));
    }

    fn git_in(repo: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env_remove("GIT_CONFIG_COUNT")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}");
    }

    fn repo_with_a_base_commit(home: &tempfile::TempDir) -> PathBuf {
        let repo = home.path().join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        git_in(&repo, &["init", "-q", "-b", "main"]);
        git_in(&repo, &["config", "user.email", "t@example.com"]);
        git_in(&repo, &["config", "user.name", "T"]);
        std::fs::write(repo.join("tracked.txt"), "one\n").expect("write");
        git_in(&repo, &["add", "-A"]);
        git_in(&repo, &["commit", "-q", "-m", "base"]);
        repo
    }

    #[test]
    fn a_capped_body_still_lists_every_changed_file() {
        let home = tempfile::tempdir().expect("tempdir");
        let repo = home.path().join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        let git = |args: &[&str]| git_in(&repo, args);
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "T"]);
        git(&["commit", "-q", "--allow-empty", "-m", "base"]);

        git(&["checkout", "-q", "-b", "work"]);

        let filler = "x".repeat(200);
        let files = 400;
        for index in 0..files {
            let body: String = (0..80)
                .map(|line| format!("{index} {line} {filler}\n"))
                .collect();
            std::fs::write(repo.join(format!("file{index:03}.txt")), body).expect("write");
        }
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "everything"]);

        let raw = crate::git::diff_against(&repo, "main").expect("a diff");
        assert!(
            raw.len() > MAX_DIFF_BYTES,
            "the fixture must exceed the cap to be testing anything: {} bytes",
            raw.len()
        );

        let diff = compute(&repo, Some("main"));
        match diff {
            Diff::Ready {
                files: listed,
                truncated,
                ..
            } => {
                assert!(truncated, "the body was over the cap");
                assert_eq!(
                    listed.len(),
                    files,
                    "every changed file is listed even though the body was cut"
                );
                assert!(
                    listed.iter().all(|file| file.added == 80),
                    "and with its true counts, not the ones that survived the cap"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_untracked_file_is_in_the_body_and_the_file_list() {
        let home = tempfile::tempdir().expect("tempdir");
        let repo = repo_with_a_base_commit(&home);
        std::fs::write(repo.join("tracked.txt"), "one\ntwo\n").expect("write");
        std::fs::write(repo.join("new.txt"), "alpha\nbeta\ngamma\n").expect("write");

        let Diff::Ready {
            files,
            body,
            untracked_omitted,
            ..
        } = compute(&repo, None)
        else {
            panic!("expected a ready diff");
        };

        assert_eq!(untracked_omitted, 0, "nothing was left out");
        let new = files
            .iter()
            .find(|file| file.path == "new.txt")
            .expect("the untracked file is listed");
        assert_eq!(new.added, 3, "with its real line count");
        assert_eq!(new.removed, 0);
        assert_eq!(new.status, "A", "an untracked file is an addition");
        assert_eq!(
            new.old_path, None,
            "not a rename, despite the /dev/null pair"
        );
        assert!(
            files.iter().any(|file| file.path == "tracked.txt"),
            "the tracked edit is still there: {files:?}"
        );

        let text = body.join("\n");
        assert!(text.contains("new file mode"), "{text}");
        assert!(text.contains("+++ b/new.txt"), "{text}");
        assert!(text.contains("+alpha"), "the contents are shown: {text}");
        assert!(text.contains("+two"), "and the tracked edit too: {text}");
    }

    #[test]
    fn an_ignored_file_is_not_smuggled_in_as_untracked() {
        // git's `--exclude-standard` keeps ignored build output out.
        let home = tempfile::tempdir().expect("tempdir");
        let repo = repo_with_a_base_commit(&home);
        std::fs::write(repo.join(".gitignore"), "ignored.txt\n").expect("write");
        std::fs::write(repo.join("ignored.txt"), "noise\n").expect("write");
        std::fs::write(repo.join("wanted.txt"), "signal\n").expect("write");

        let Diff::Ready { files, .. } = compute(&repo, None) else {
            panic!("expected a ready diff");
        };
        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert!(paths.contains(&"wanted.txt"), "{paths:?}");
        assert!(paths.contains(&".gitignore"), "itself untracked: {paths:?}");
        assert!(!paths.contains(&"ignored.txt"), "{paths:?}");
    }

    #[test]
    fn an_untracked_path_with_a_space_and_a_binary_file_both_survive() {
        // git appends a TAB to a `+++` path containing a space, and counts a binary file as `-`.
        let home = tempfile::tempdir().expect("tempdir");
        let repo = repo_with_a_base_commit(&home);
        std::fs::create_dir_all(repo.join("sub dir")).expect("mkdir");
        std::fs::write(repo.join("sub dir/has space.txt"), "spaced\n").expect("write");
        std::fs::write(repo.join("bin.dat"), [0u8, 1, 2, 0, 255]).expect("write");

        let Diff::Ready { files, .. } = compute(&repo, None) else {
            panic!("expected a ready diff");
        };
        let spaced = files
            .iter()
            .find(|file| file.path == "sub dir/has space.txt")
            .expect("the spaced path, without a trailing tab");
        assert_eq!(spaced.added, 1);
        assert_eq!(spaced.status, "A");
        let binary = files
            .iter()
            .find(|file| file.path == "bin.dat")
            .expect("the binary file is listed");
        assert_eq!(
            (binary.added, binary.removed),
            (0, 0),
            "`-` counts read as zero rather than failing the parse"
        );
    }

    #[test]
    fn a_base_branch_diff_is_committed_history_and_gains_no_untracked_files() {
        let home = tempfile::tempdir().expect("tempdir");
        let repo = repo_with_a_base_commit(&home);
        git_in(&repo, &["checkout", "-q", "-b", "work"]);
        std::fs::write(repo.join("committed.txt"), "in history\n").expect("write");
        git_in(&repo, &["add", "-A"]);
        git_in(&repo, &["commit", "-q", "-m", "work"]);
        std::fs::write(repo.join("untracked.txt"), "not in history\n").expect("write");

        let Diff::Ready {
            files,
            untracked_omitted,
            ..
        } = compute(&repo, Some("main"))
        else {
            panic!("expected a ready diff");
        };
        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, vec!["committed.txt"], "{paths:?}");
        assert_eq!(
            untracked_omitted, 0,
            "no untracked file was in scope, so none was omitted"
        );
    }

    #[test]
    fn more_untracked_files_than_the_cap_reports_what_it_left_out() {
        let home = tempfile::tempdir().expect("tempdir");
        let repo = repo_with_a_base_commit(&home);
        let extra = 3;
        let total = crate::git::UNTRACKED_FILE_CAP + extra;
        for index in 0..total {
            std::fs::write(repo.join(format!("new{index:04}.txt")), "x\n").expect("write");
        }

        let Diff::Ready {
            files,
            untracked_omitted,
            truncated,
            ..
        } = compute(&repo, None)
        else {
            panic!("expected a ready diff");
        };
        assert_eq!(untracked_omitted, extra);
        assert_eq!(files.len(), crate::git::UNTRACKED_FILE_CAP);
        assert!(
            !truncated,
            "the BODY was not over the byte cap; the two bounds are separate"
        );
    }

    #[test]
    fn a_remote_session_is_reported_rather_than_diffed_locally() {
        let mut store = DiffStore::new();
        store.request(
            "s1",
            PathBuf::from("/srv/worktree"),
            Some("main".into()),
            "ssh:devbox:tmux",
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            store.poll();
            if let Some(Diff::Failed(message)) = store.get("s1") {
                assert!(message.contains("another machine"), "{message}");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!(
            "the diff never reported the other machine: {:?}",
            store.get("s1")
        );
    }

    #[derive(Clone, Default)]
    struct CapturedLog(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLog {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log buffer").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl CapturedLog {
        fn lines_at(&self, level: &str) -> Vec<String> {
            String::from_utf8_lossy(&self.0.lock().expect("log buffer"))
                .lines()
                .filter(|line| line.contains(level))
                .map(str::to_string)
                .collect()
        }
    }

    fn compute_capturing_logs(worktree: &Path, refreshes: usize) -> (Diff, CapturedLog) {
        let log = CapturedLog::default();
        let writer = log.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let diff = tracing::subscriber::with_default(subscriber, || {
            let mut diff = compute(worktree, None);
            for _ in 1..refreshes {
                diff = compute(worktree, None);
            }
            diff
        });
        (diff, log)
    }

    #[test]
    fn a_workspace_outside_git_shows_no_diff_and_logs_no_warning() {
        let plain = tempfile::tempdir().expect("tempdir");
        let (diff, log) = compute_capturing_logs(plain.path(), 3);

        assert_eq!(log.lines_at("WARN"), Vec::<String>::new());
        assert_eq!(diff, empty_ready());
        assert!(
            log.lines_at("DEBUG").len() <= 1,
            "refreshing a plain directory logged more than once: {:?}",
            log.lines_at("DEBUG")
        );
    }
}
