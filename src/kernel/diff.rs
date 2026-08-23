//! A session's changes, computed off the render path.
//!
//! A diff shells out to git, so it cannot be built during a snapshot refresh —
//! that runs on the loop. This computes per session on a background thread and
//! publishes when ready, which is the same shape as attaching a terminal and
//! as the command bus: **anything that touches the world runs on a worker, and
//! the UI reads the result.** Third instance of the pattern.
//!
//! The consequence a plugin must render is that "not computed yet" is a real
//! state, distinct from "no changes" — a diff that is merely slow must not look
//! like a clean worktree.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

/// Largest diff read, in bytes. Beyond this the cap is *reported*, because a
/// silently truncated diff is a review that quietly omits things.
pub const MAX_DIFF_BYTES: usize = 4 * 1024 * 1024;

/// One changed file, flattened for rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSummary {
    pub path: String,
    pub added: usize,
    pub removed: usize,
    /// `M` / `A` / `D` / `R`, as the parser determined it.
    ///
    /// Carried because the parser already knew: dropping it made a pane re-read
    /// the whole body to recover a glyph, which for a capped diff is ~99,000 lines
    /// of Lua to learn something the kernel had in hand.
    pub status: &'static str,
    /// The path a rename came from, when it differs.
    pub old_path: Option<String>,
}

/// What is known about a session's changes.
#[derive(Debug, Clone, PartialEq)]
pub enum Diff {
    /// Requested, not finished. Distinct from `Ready` with nothing in it.
    Pending,
    Ready {
        files: Vec<FileSummary>,
        /// The rendered body, one entry per line.
        body: Vec<String>,
        /// Set when the diff was larger than the cap.
        truncated: bool,
        /// The diff's size before any cut, in bytes, so a truncation banner can be
        /// specific about what is missing.
        raw_bytes: usize,
        /// Untracked files found but not represented, because there were more
        /// than `git`'s per-file cap allows.
        ///
        /// Distinct from [`truncated`](Self::Ready::truncated), which is about
        /// bytes: a short *list* and a cut *body* are different failures and read
        /// differently. `0` when everything is accounted for, which is the
        /// ordinary case.
        untracked_omitted: usize,
    },
    Failed(String),
}

struct Computed {
    session: String,
    diff: Diff,
}

/// How long a settled diff is trusted before a request recomputes it, and how
/// soon a failure is retried.
///
/// The TTL matches the git-stat one (`snapshot`'s `GIT_STAT_TTL`, the same 5 s):
/// both shell out to git about the same worktree, and this was the last cache
/// without an age — once `Ready`, an entry stood for the life of the process
/// unless something explicitly invalidated it, so a pane watching an agent that
/// was still writing code showed the diff it first saw. A *failure* retries
/// sooner (mirroring `repos`' `BRANCHES_RETRY`): held for the full TTL, one
/// unreachable moment kept an error on screen well after the host came back.
const DIFF_TTL: std::time::Duration = std::time::Duration::from_secs(5);
const DIFF_RETRY: std::time::Duration = std::time::Duration::from_secs(3);

/// A diff and when it arrived — the age every cache here carries.
struct Held {
    at: std::time::Instant,
    diff: Diff,
    /// A recompute is in flight while `diff` stays published. Replacing a
    /// stale `Ready` with `Pending` would blank the pane every TTL, so the old
    /// answer stands until the fresh one lands in [`DiffStore::poll`] — the
    /// same old-value-while-refreshing shape the git stats use.
    refreshing: bool,
}

impl Held {
    /// Whether this answer should be computed again.
    ///
    /// Never while one is on its way — a `Pending` first answer or a settled
    /// one already `refreshing` — however old it has grown: asking again would
    /// spawn a second worker for the same session.
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

/// Computes and caches diffs, one per session.
pub struct DiffStore {
    diffs: HashMap<String, Held>,
    tx: Sender<Computed>,
    rx: Receiver<Computed>,
}

impl DiffStore {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            diffs: HashMap::new(),
            tx,
            rx,
        }
    }

    /// What is known about `session`, if anything has been asked for.
    pub fn get(&self, session: &str) -> Option<&Diff> {
        self.diffs.get(session).map(|held| &held.diff)
    }

    /// Ask for a session's diff, unless a fresh answer is held or one is in
    /// flight.
    ///
    /// Idempotent by design: this is called from the loop with whatever session
    /// is selected, so it happens every frame and must cost nothing while the
    /// held answer is fresh. Once it is older than `DIFF_TTL` (or
    /// `DIFF_RETRY` for a failure) the recompute is dispatched — with the old
    /// answer still published, per `Held::refreshing`.
    pub fn request(
        &mut self,
        session: &str,
        worktree: PathBuf,
        base: Option<String>,
        backend: &str,
    ) {
        match self.diffs.get_mut(session) {
            Some(held) if !held.stale() => return,
            // A settled answer past its age: keep publishing it, mark the
            // recompute in flight, and dispatch below.
            Some(held) => held.refreshing = true,
            None => {
                self.diffs.insert(
                    session.to_string(),
                    Held {
                        at: std::time::Instant::now(),
                        diff: Diff::Pending,
                        refreshing: false,
                    },
                );
            }
        }

        // The worktree is a path on the session's OWN machine. Running the local
        // `git` against a remote path either fails or, on an unlucky collision,
        // diffs something else entirely — the same reasoning that made `sync`
        // host-aware. Resolved here rather than on the worker so an unreachable
        // backend is reported as a failed diff instead of a silent local read.
        let host = crate::session_ops::resolve_host(backend).flatten();
        let unreachable = host.is_none() && crate::session::is_remote_backend(backend);

        let tx = self.tx.clone();
        let session = session.to_string();
        std::thread::spawn(move || {
            let diff = if unreachable {
                Diff::Failed("this session's host is not in hosts.toml".to_string())
            } else {
                compute(&worktree, base.as_deref(), host.as_ref())
            };
            let _ = tx.send(Computed { session, diff });
        });
    }

    /// Fold finished computations in. Returns true when anything arrived, so
    /// the caller can repaint.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(done) = self.rx.try_recv() {
            self.diffs.insert(
                done.session,
                Held {
                    at: std::time::Instant::now(),
                    diff: done.diff,
                    refreshing: false,
                },
            );
            changed = true;
        }
        changed
    }

    /// Seed a diff directly, so a test can render a known one without git.
    #[doc(hidden)]
    pub fn set_for_test(&mut self, session: &str, diff: Diff) {
        self.diffs.insert(
            session.to_string(),
            Held {
                at: std::time::Instant::now(),
                diff,
                refreshing: false,
            },
        );
    }

    /// Forget a session's diff, so the next request recomputes it — the
    /// immediate route, for a caller that *knows* the worktree changed rather
    /// than waiting out `DIFF_TTL`.
    pub fn invalidate(&mut self, session: &str) {
        self.diffs.remove(session);
    }
}

impl Default for DiffStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Run the diff and render it. Called on a worker thread.
///
/// `host` is the machine the worktree is on — `None` for local. v1 threads the
/// same argument through `code_review`; passing `None` unconditionally is how a
/// remote session came to be diffed against a path that does not exist here.
fn compute(worktree: &Path, base: Option<&str>, host: Option<&crate::session::HostDef>) -> Diff {
    let sources = match read_sources(worktree, base, host) {
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
    // The size before the cut, so a pane can say "4.0 of 20.3 MB" rather than
    // "some changes are not shown". The file *count* is deliberately not offered:
    // knowing it would mean parsing the whole diff, which is what the cap exists
    // to avoid.
    let raw_bytes = raw.len();
    let raw = if truncated {
        // Cut on a line boundary so the parser is not handed half a hunk. Searched
        // over the *bytes* rather than by slicing the string first: a `String` is
        // indexed by byte offset and slicing one mid-codepoint panics, which a
        // 4 MiB cap landing inside any multi-byte character is enough to do. A
        // newline byte is always a character boundary, so the offset it finds is
        // safe to slice at.
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

    // The file list comes from git, not from the body above, and this is the whole
    // point: the body is capped and the list is not. Derived from the capped text it
    // was silently short — 310 files of 433 on this repository's own diff — with
    // totals to match, and nothing said so. `truncated` is about bytes; a reviewer
    // scrolling a list that ends early has no way to know.
    //
    // A failure reading them fails the diff rather than falling back to the body's
    // own files (see `read_sources`). These are the cheap commands: if they cannot
    // run, the expensive one that just did was luck, and reporting a partial list as
    // complete is the fault this exists to remove.
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

/// The three raw git outputs a diff is rendered from, plus what was left out.
struct Sources {
    raw: String,
    numstat: String,
    name_status: String,
    untracked_omitted: usize,
}

/// Read the diff, the counts and the statuses for whichever target is asked for.
///
/// Against the base branch when there is one, else the uncommitted changes —
/// which is what a session with no base branch has to show. The two are gathered
/// differently: `base..HEAD` is committed history, so its body and file list are
/// independent commands, while the working tree has to fold in **untracked
/// files** and therefore comes back from [`crate::git::working_diff_on`] as one
/// consistent set. Asking for them separately there would let the body describe a
/// file the list omitted.
///
/// `Err` carries the message to report, keeping "could not read the diff" and
/// "could not list the changed files" distinct — the second means the cheap
/// commands failed after the expensive one worked, which is worth telling apart.
fn read_sources(
    worktree: &Path,
    base: Option<&str>,
    host: Option<&crate::session::HostDef>,
) -> Result<Sources, &'static str> {
    let Some(base) = base else {
        let working =
            crate::git::working_diff_on(host, worktree).ok_or("could not read the diff")?;
        return Ok(Sources {
            raw: working.body,
            numstat: working.numstat_z,
            name_status: working.name_status_z,
            untracked_omitted: working
                .untracked_total
                .saturating_sub(working.untracked_shown),
        });
    };
    let raw = crate::git::diff_against_on(host, worktree, base).ok_or("could not read the diff")?;
    let (numstat, name_status) = crate::git::diff_numstat_on(host, worktree, Some(base))
        .zip(crate::git::diff_name_status_on(host, worktree, Some(base)))
        .ok_or("could not list the changed files")?;
    Ok(Sources {
        raw,
        numstat,
        name_status,
        // `base..HEAD` is committed history; nothing untracked can be in it.
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

    /// The *immediate* refresh route: `Command::Diff` need not wait out the
    /// TTL when the caller knows the worktree changed.
    #[test]
    fn invalidating_lets_the_next_request_recompute() {
        let mut store = DiffStore::new();
        let ask = |store: &mut DiffStore| {
            store.request(
                "s1",
                PathBuf::from("/definitely/not/a/repo"),
                None,
                "local-tmux",
            )
        };
        ask(&mut store);
        assert_eq!(store.get("s1"), Some(&Diff::Pending));

        // Asking again is a no-op while an answer is held — that is the cache.
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
        // The property that matters: asking does not wait for git.
        let mut store = DiffStore::new();
        let started = std::time::Instant::now();
        store.request(
            "s1",
            PathBuf::from("/definitely/not/a/repo"),
            None,
            "local-tmux",
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
        assert_eq!(store.get("s1"), Some(&Diff::Pending));
    }

    #[test]
    fn pending_is_distinct_from_empty() {
        // A slow diff must not look like a clean worktree.
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
        store.request("s1", PathBuf::from("/tmp"), None, "local-tmux");
        let first = store.get("s1").cloned();
        store.request("s1", PathBuf::from("/tmp"), None, "local-tmux");
        assert_eq!(store.get("s1").cloned(), first);
    }

    /// An empty `Ready`, for tests that need a settled answer without git.
    fn empty_ready() -> Diff {
        Diff::Ready {
            files: Vec::new(),
            body: Vec::new(),
            truncated: false,
            raw_bytes: 0,
            untracked_omitted: 0,
        }
    }

    /// A held answer, back-dated so the age rule can be exercised without
    /// waiting out the interval.
    fn held(diff: Diff, age: std::time::Duration) -> Held {
        Held {
            at: std::time::Instant::now() - age,
            diff,
            refreshing: false,
        }
    }

    #[test]
    fn a_settled_diff_is_computed_again_once_it_is_old() {
        // Without this an agent still writing code showed the diff first seen,
        // forever, unless something explicitly invalidated it.
        assert!(!held(empty_ready(), std::time::Duration::ZERO).stale());
        assert!(held(empty_ready(), DIFF_TTL).stale());
        // A failure retries sooner — and, above all, retries at all.
        assert!(!held(Diff::Failed("gone".into()), std::time::Duration::ZERO).stale());
        assert!(held(Diff::Failed("gone".into()), DIFF_RETRY).stale());
    }

    #[test]
    fn an_in_flight_diff_is_never_asked_for_twice() {
        // However slow git is: a second request would spawn a second worker
        // for an answer already on its way — first computation and refresh
        // alike.
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
            "local-tmux",
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
            "local-tmux",
        );
        // The old answer stays published while the recompute is in flight —
        // replacing it with `Pending` would blank the pane every TTL.
        assert_eq!(store.get("s1"), Some(&empty_ready()));
        assert!(store.diffs.get("s1").unwrap().refreshing);

        // And the recompute really was dispatched: its (failing) answer lands.
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
        store.request("s1", PathBuf::from("/tmp"), None, "local-tmux");
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
            "local-tmux",
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

    /// The property the file list was changed for: **the body is capped and the list
    /// is not.**
    ///
    /// Built against a real repository, because it is git's own output that has to be
    /// complete. The body here is deliberately pushed past `MAX_DIFF_BYTES`, and every
    /// file must still be listed with its true counts — derived from the capped text,
    /// this listed only the files that fit.
    /// Run `git` in `repo` with the location variables scrubbed.
    ///
    /// The pre-commit hook exports `GIT_DIR` and friends, so an unscrubbed call
    /// from the suite lands in the real repository (see CLAUDE.md → Testing).
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

    /// A repository with one commit on `main`, ready to be dirtied.
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

        // On a BRANCH, or `main..HEAD` is empty — which is precisely the bug this
        // whole change exists to fix, and it caught this fixture first.
        git(&["checkout", "-q", "-b", "work"]);

        // Enough text that the diff is past the cap several times over, spread over
        // more files than the cap can hold.
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

        let raw = crate::git::diff_against_on(None, &repo, "main").expect("a diff");
        assert!(
            raw.len() > MAX_DIFF_BYTES,
            "the fixture must exceed the cap to be testing anything: {} bytes",
            raw.len()
        );

        let diff = compute(&repo, Some("main"), None);
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

    /// The bug this exists for: **a new file is the most common thing an agent
    /// produces, and `git diff HEAD` cannot show one.**
    ///
    /// A session with no base branch — the scratch worktree someone watches an
    /// agent work in — reported "no changes" after three files had been written.
    #[test]
    fn an_untracked_file_is_in_the_body_and_the_file_list() {
        let home = tempfile::tempdir().expect("tempdir");
        let repo = repo_with_a_base_commit(&home);
        // A tracked edit alongside, so the fix cannot work by replacing the
        // tracked half rather than adding to it.
        std::fs::write(repo.join("tracked.txt"), "one\ntwo\n").expect("write");
        std::fs::write(repo.join("new.txt"), "alpha\nbeta\ngamma\n").expect("write");

        let Diff::Ready {
            files,
            body,
            untracked_omitted,
            ..
        } = compute(&repo, None, None)
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
        // `--exclude-standard` is what keeps build output out; without it a
        // scratch worktree's `target/` would drown the review.
        let home = tempfile::tempdir().expect("tempdir");
        let repo = repo_with_a_base_commit(&home);
        std::fs::write(repo.join(".gitignore"), "ignored.txt\n").expect("write");
        std::fs::write(repo.join("ignored.txt"), "noise\n").expect("write");
        std::fs::write(repo.join("wanted.txt"), "signal\n").expect("write");

        let Diff::Ready { files, .. } = compute(&repo, None, None) else {
            panic!("expected a ready diff");
        };
        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert!(paths.contains(&"wanted.txt"), "{paths:?}");
        assert!(paths.contains(&".gitignore"), "itself untracked: {paths:?}");
        assert!(!paths.contains(&"ignored.txt"), "{paths:?}");
    }

    #[test]
    fn an_untracked_path_with_a_space_and_a_binary_file_both_survive() {
        // git appends a TAB to a `+++` path containing a space, which looks like
        // corruption the first time you meet it, and a binary file has counts of
        // `-` rather than numbers.
        let home = tempfile::tempdir().expect("tempdir");
        let repo = repo_with_a_base_commit(&home);
        std::fs::create_dir_all(repo.join("sub dir")).expect("mkdir");
        std::fs::write(repo.join("sub dir/has space.txt"), "spaced\n").expect("write");
        std::fs::write(repo.join("bin.dat"), [0u8, 1, 2, 0, 255]).expect("write");

        let Diff::Ready { files, .. } = compute(&repo, None, None) else {
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
        // `base..HEAD` cannot contain an uncommitted file, and folding one in
        // would make the review claim work was committed that was not.
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
        } = compute(&repo, Some("main"), None)
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
        // A short LIST is a different failure from a cut BODY, so it gets its own
        // signal: silently stopping at the cap would read as a complete review.
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
        } = compute(&repo, None, None)
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
    fn a_remote_session_whose_host_is_gone_is_reported_rather_than_diffed_locally() {
        // The path is on another machine. Running the local `git` against it
        // either fails or, on a collision, diffs something else entirely — so an
        // unresolvable host is a reported failure, not a fallback.
        let mut store = DiffStore::new();
        store.request(
            "s1",
            PathBuf::from("/srv/worktree"),
            Some("main".into()),
            "ssh:host-that-is-not-configured",
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            store.poll();
            if let Some(Diff::Failed(message)) = store.get("s1") {
                assert!(message.contains("hosts.toml"), "{message}");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!(
            "the diff never reported the missing host: {:?}",
            store.get("s1")
        );
    }
}
