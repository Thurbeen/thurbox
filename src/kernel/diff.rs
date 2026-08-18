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

use crate::session::review::{parse_unified_diff, DiffFile};

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
    },
    Failed(String),
}

struct Computed {
    session: String,
    diff: Diff,
}

/// Computes and caches diffs, one per session.
pub struct DiffStore {
    diffs: HashMap<String, Diff>,
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
        self.diffs.get(session)
    }

    /// Ask for a session's diff, unless it is already known or in flight.
    ///
    /// Idempotent by design: this is called from the loop with whatever session
    /// is selected, so it happens every frame and must cost nothing after the
    /// first.
    pub fn request(
        &mut self,
        session: &str,
        worktree: PathBuf,
        base: Option<String>,
        backend: &str,
    ) {
        if self.diffs.contains_key(session) {
            return;
        }
        self.diffs.insert(session.to_string(), Diff::Pending);

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
            self.diffs.insert(done.session, done.diff);
            changed = true;
        }
        changed
    }

    /// Seed a diff directly, so a test can render a known one without git.
    #[doc(hidden)]
    pub fn set_for_test(&mut self, session: &str, diff: Diff) {
        self.diffs.insert(session.to_string(), diff);
    }

    /// Forget a session's diff, so the next request recomputes it.
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
    // Against the base branch when there is one, else the uncommitted changes —
    // which is what a session with no base branch has to show.
    let raw = match base {
        Some(base) => crate::git::diff_against_on(host, worktree, base),
        None => crate::git::diff_working_on(host, worktree),
    };
    let Some(raw) = raw else {
        return Diff::Failed("could not read the diff".to_string());
    };

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

    let parsed = parse_unified_diff(&raw);
    Diff::Ready {
        files: parsed.iter().map(summarise).collect(),
        body: raw.lines().map(str::to_string).collect(),
        truncated,
        raw_bytes,
    }
}

fn summarise(file: &DiffFile) -> FileSummary {
    // `DiffFile` already counts both sides; reusing it keeps this and the
    // review types from drifting apart.
    FileSummary {
        path: file.path.clone(),
        added: file.added_count(),
        removed: file.deleted_count(),
        status: file.status.glyph(),
        old_path: file.old_path.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_starts_unknown() {
        let store = DiffStore::new();
        assert!(store.get("nobody").is_none());
    }

    /// The property `Command::Diff` exists to provide, and which nothing provided
    /// before it: a diff can be asked for a second time.
    ///
    /// `request` returns early while it holds an answer, so without invalidation a
    /// diff was computed once per session per process. A pane watching an agent
    /// that is still writing code would show the diff it first saw, forever —
    /// stale and confident, which is worse than absent.
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

    /// A rename and a status reach the pane without it re-reading the body — the
    /// parser knew both, and dropping them cost ~99,000 lines of Lua per capped
    /// diff to recover a glyph and an arrow.
    #[test]
    fn a_summary_carries_the_status_and_the_old_path() {
        let raw = "diff --git a/old.rs b/new.rs\n\
                   similarity index 90%\n\
                   rename from old.rs\n\
                   rename to new.rs\n\
                   --- a/old.rs\n\
                   +++ b/new.rs\n\
                   @@ -1,1 +1,1 @@\n\
                   -was\n\
                   +is\n";
        let parsed = parse_unified_diff(raw);
        let summary: Vec<FileSummary> = parsed.iter().map(summarise).collect();
        let file = summary.first().expect("one file");
        assert_eq!(file.status, "R", "a rename reports itself as one");
        assert_eq!(
            file.old_path.as_deref(),
            Some("old.rs"),
            "and says where it came from: {file:?}"
        );
        assert_eq!((file.added, file.removed), (1, 1));
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

    #[test]
    fn a_summary_counts_added_and_removed_lines() {
        let raw = "diff --git a/x.rs b/x.rs\n\
                   --- a/x.rs\n\
                   +++ b/x.rs\n\
                   @@ -1,2 +1,3 @@\n\
                   \x20context\n\
                   -gone\n\
                   +new\n\
                   +also new\n";
        let parsed = parse_unified_diff(raw);
        assert_eq!(parsed.len(), 1);
        let summary = summarise(&parsed[0]);
        assert_eq!(summary.added, 2);
        assert_eq!(summary.removed, 1);
        assert!(summary.path.contains("x.rs"));
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
