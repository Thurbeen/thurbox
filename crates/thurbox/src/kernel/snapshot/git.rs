//! The git-stat worker: each session's diffstat and merge answer, computed on
//! a thread because `git` shells out and a refresh runs on the loop.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::GitState;

/// What a git-stat worker reports back: the session, its state when the path
/// turned out to be a repository, and the merge answer that state carries with
/// the commit it was computed for (`None` when the worktree reached no answer).
type StatResult = (String, Option<GitState>, Option<(String, bool)>);

/// The base interval a session's git stat is trusted for, from `settings.toml`;
/// `None` when `git_poll_secs = 0` and the polling is off altogether.
///
/// v1 refreshes on the same ~5 s cadence (`GIT_REFRESH_TICKS`) for the same
/// reason the default is that: `worktree_stats` shells out to `git`, which is
/// far too expensive per frame — and answered only once, a session's diffstat
/// freezes at whatever it was the first time it was looked at and never moves
/// again.
///
/// A setting rather than the constant it was, because the number governs a cost
/// that is **per session**: sixteen of them at five seconds is a `git` burst
/// every 300ms for as long as the interface runs, most of it about rows nobody
/// is reading. It is read once, when the store is built, so it takes effect on
/// the next launch (issue #1167).
pub(super) fn git_poll_interval(secs: u64) -> Option<Duration> {
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// How far a session's own interval may stretch past the base, once its answer
/// stops moving.
///
/// Twelve — a minute at the default — because that is about how long a diffstat
/// may lag before it reads as broken rather than as stale, and because the
/// backoff is what stops the cost tracking the session *list*: a dozen dormant
/// sessions are statted as often as one active one. Any change at all resets
/// it, so the session an agent is working in never leaves the base cadence.
const GIT_STAT_BACKOFF: u32 = 12;

/// How long an *unmerged* answer stands before it is computed again.
///
/// The one answer whose staleness is not bounded by HEAD: a branch lands
/// upstream without the worktree moving at all, so `merged: false` has to be
/// re-asked on a clock. A minute rather than the poll interval because the
/// check is seven subprocesses, it is the dominant cost of a session with an
/// open pull request, and nothing acts on the answer faster than a person can
/// read it — `at_risk` warns before a delete.
///
/// A floor on the cadence, not a deadline. The recheck rides on a poll, so what
/// actually bounds the answer's age is this **or the session's own interval,
/// whichever is longer** — the same thing at the default, where the backoff
/// caps at a minute, and `12 × git_poll_secs` for an operator who raised it.
/// Deliberately so: raising that knob is asking for fewer subprocesses across
/// the board, and a recheck forced ahead of the interval would spend the ones
/// it was raised to save.
const MERGE_RECHECK: Duration = Duration::from_secs(60);

/// A [`crate::git::merged_into_default`] answer, the commit it was computed
/// for, and when it was computed.
struct MergeAnswer {
    head: String,
    merged: bool,
    /// When the answer was last *computed* — not when it was last handed back,
    /// which is every poll. Only [`MERGE_RECHECK`] reads it, and only for a
    /// `false`.
    at: Instant,
}

/// One session's last git answer, and when it landed.
struct Stat {
    /// `None` when the path turned out not to be a repository.
    ///
    /// Recorded rather than dropped: a miss nobody remembers is asked again on
    /// every single refresh, which is a thread and a `git` process per
    /// non-repository session forever — and *every remote session* is a miss, its
    /// worktree being on another machine entirely. A miss that keeps coming back
    /// is also a stable answer, so it backs off like any other.
    state: Option<GitState>,
    at: Instant,
    /// The merge answer this session last reached, kept so the next run can
    /// skip the check that costs seven subprocesses.
    merge: Option<MergeAnswer>,
    /// How long [`Self::state`] is trusted: the base interval, doubled for
    /// every poll that changed nothing, capped at [`GIT_STAT_BACKOFF`] times it.
    interval: Duration,
}

/// Git stats computed off the render path.
///
/// `git::worktree_stats` shells out, so it cannot run during a refresh — that
/// happens on the loop. Fourth instance of the worker pattern: touch the world
/// on a thread, publish the result.
pub(super) struct GitStats {
    /// The base interval, held rather than read per request: it is a
    /// restart-only setting, and a cache that reads a global is a cache a test
    /// cannot put on a clock of its own.
    poll: Option<Duration>,
    known: std::collections::HashMap<String, Stat>,
    inflight: std::collections::HashSet<String>,
    channel: Option<(
        std::sync::mpsc::Sender<StatResult>,
        std::sync::mpsc::Receiver<StatResult>,
    )>,
}

impl GitStats {
    /// `poll` is the base interval; `None` asks for nothing, ever.
    pub(super) fn new(poll: Option<Duration>) -> Self {
        Self {
            poll,
            known: std::collections::HashMap::new(),
            inflight: std::collections::HashSet::new(),
            channel: None,
        }
    }

    fn ensure_channel(&mut self) -> std::sync::mpsc::Sender<StatResult> {
        if self.channel.is_none() {
            self.channel = Some(std::sync::mpsc::channel());
        }
        self.channel.as_ref().expect("just created").0.clone()
    }

    /// Ask for a session's stats, unless polling is off, a run is in flight, or
    /// the last answer is still inside this session's own interval.
    pub(super) fn request(&mut self, session: &str, worktree: PathBuf) {
        if self.poll.is_none() || self.inflight.contains(session) {
            return;
        }
        // The merge answer to offer the worker, if it is still worth offering.
        // Withholding one *is* the decision to recheck, which is why the stamp
        // moves here: the answer that comes back will be a fresh one.
        let known = match self.known.get_mut(session) {
            Some(stat) => {
                if stat.at.elapsed() < stat.interval {
                    return;
                }
                match &mut stat.merge {
                    // A landed commit never un-lands, so a `true` stands for
                    // exactly as long as HEAD does.
                    Some(answer) if answer.merged => Some((answer.head.clone(), true)),
                    // A `false` is true of a moment as well as of a commit —
                    // the branch lands without the worktree moving — so it is
                    // offered only until it ages out.
                    Some(answer) if answer.at.elapsed() < MERGE_RECHECK => {
                        Some((answer.head.clone(), false))
                    }
                    Some(answer) => {
                        answer.at = Instant::now();
                        None
                    }
                    None => None,
                }
            }
            None => None,
        };
        self.inflight.insert(session.to_string());
        let tx = self.ensure_channel();
        let session = session.to_string();
        std::thread::spawn(move || {
            let known = known.as_ref().map(|(head, merged)| crate::git::KnownMerge {
                head,
                merged: *merged,
            });
            let stats = crate::git::worktree_stats(&worktree, known);
            // The answer and the commit it belongs to, so the next run can skip
            // the check. Neither half means anything without the other.
            let merge = stats.as_ref().and_then(|s| s.head.clone().zip(s.merged));
            let state = stats.map(|s| GitState {
                files_changed: s.files_changed,
                insertions: s.insertions,
                deletions: s.deletions,
                untracked: s.untracked,
                dirty: s.dirty,
                ahead: s.ahead,
                behind: s.behind,
                merged: s.merged,
            });
            let _ = tx.send((session, state, merge));
        });
    }

    pub(super) fn drain(&mut self) {
        let Some(base) = self.poll else { return };
        let Some((_, rx)) = &self.channel else { return };
        while let Ok((session, stats, merge)) = rx.try_recv() {
            self.inflight.remove(&session);
            let previous = self.known.remove(&session);
            // An answer that did not move is one this session did not need, so
            // it is asked for less often until something changes. A miss is an
            // answer too — see `Stat::state` — and backs off the same way.
            // Saturating because both operands are reachable from
            // `settings.toml`: a `git_poll_secs` big enough to overflow a
            // `Duration` would otherwise panic the loop on the first answer
            // that came back, which is a config file crashing the interface.
            let interval = match &previous {
                Some(prev) if prev.state == stats => prev
                    .interval
                    .saturating_mul(2)
                    .min(base.saturating_mul(GIT_STAT_BACKOFF)),
                _ => base,
            };
            let merge = match (previous.and_then(|prev| prev.merge), merge) {
                // The same answer about the same commit: keep the stamp it was
                // computed with, or a `false` re-offered every poll would reset
                // its own recheck and stand forever.
                (Some(prev), Some((head, merged)))
                    if prev.head == head && prev.merged == merged =>
                {
                    Some(prev)
                }
                (_, Some((head, merged))) => Some(MergeAnswer {
                    head,
                    merged,
                    at: Instant::now(),
                }),
                (_, None) => None,
            };
            self.known.insert(
                session,
                Stat {
                    state: stats,
                    at: Instant::now(),
                    merge,
                    interval,
                },
            );
        }
    }

    /// The last answer for `session`; `None` until one landed, or when the
    /// path is not a repository.
    pub(super) fn state(&self, session: &str) -> Option<GitState> {
        self.known.get(session).and_then(|stat| stat.state)
    }

    /// Forget sessions that are no longer in the snapshot, so the cache tracks
    /// what exists rather than everything that ever did.
    pub(super) fn retain(&mut self, present: &std::collections::HashSet<&str>) {
        self.known.retain(|id, _| present.contains(id.as_str()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A git answer to hand a cache, distinguishable from the next one by its
    /// insertion count.
    fn git_state(insertions: usize) -> GitState {
        GitState {
            files_changed: 1,
            insertions,
            deletions: 0,
            untracked: 0,
            dirty: true,
            ahead: 1,
            behind: 0,
            merged: Some(false),
        }
    }

    #[test]
    fn a_session_whose_answer_stops_moving_is_asked_less_often() {
        // The scaling issue #1167 reports: the cost of this cache is one
        // `git` burst per session per interval, so a fixed interval makes an
        // instance's git load linear in how many sessions it holds — including
        // the ones nobody has touched in hours. An answer that keeps coming
        // back identical is one nobody needs at five-second resolution, so the
        // interval doubles until it caps, and the first change resets it.
        let base = Duration::from_secs(5);
        let mut git = GitStats::new(Some(base));
        let tx = git.ensure_channel();
        let answer = |git: &mut GitStats, state: GitState| {
            tx.send(("s1".to_string(), Some(state), None))
                .expect("send");
            git.drain();
            git.known["s1"].interval
        };

        assert_eq!(answer(&mut git, git_state(1)), base, "a first answer");
        assert_eq!(answer(&mut git, git_state(1)), base * 2);
        assert_eq!(answer(&mut git, git_state(1)), base * 4);
        for _ in 0..6 {
            answer(&mut git, git_state(1));
        }
        assert_eq!(
            git.known["s1"].interval,
            base * GIT_STAT_BACKOFF,
            "a settled session backs off to the cap and stays there"
        );

        assert_eq!(
            answer(&mut git, git_state(2)),
            base,
            "work landing in the worktree puts it back on the fast cadence"
        );
    }

    #[test]
    fn a_backed_off_session_is_not_asked_again_inside_its_interval() {
        // The interval above is only a number until `request` honours it: this
        // is the gate that decides whether a `git` process is spawned at all.
        let base = Duration::from_secs(5);
        let dir = tempfile::tempdir().expect("tempdir");
        let mut git = GitStats::new(Some(base));
        let stale_by = |age: Duration| Stat {
            state: Some(git_state(1)),
            at: Instant::now().checked_sub(age).expect("recent enough"),
            merge: None,
            interval: base * 4,
        };

        git.known.insert("s1".to_string(), stale_by(base * 3));
        git.request("s1", dir.path().to_path_buf());
        assert!(
            git.inflight.is_empty(),
            "the answer is older than the base interval but younger than this session's"
        );

        git.known.insert("s1".to_string(), stale_by(base * 5));
        git.request("s1", dir.path().to_path_buf());
        assert!(
            git.inflight.contains("s1"),
            "past its own interval it is asked again"
        );
    }

    #[test]
    fn the_merge_recheck_rides_on_a_poll_rather_than_outpacing_it() {
        // Deliberate, and asserted so that a later reading of `MERGE_RECHECK`
        // as a deadline does not "fix" it: the recheck happens on the first
        // poll that finds the answer stale, so a session polled every six
        // minutes rechecks every six minutes. An operator who raised
        // `git_poll_secs` asked for fewer subprocesses, and a recheck forced
        // ahead of their interval would spend exactly the ones they saved.
        let base = Duration::from_secs(30);
        let dir = tempfile::tempdir().expect("tempdir");
        let mut git = GitStats::new(Some(base));
        let aged = |age: Duration| Stat {
            state: Some(git_state(1)),
            at: Instant::now().checked_sub(age).expect("recent enough"),
            merge: Some(MergeAnswer {
                head: "0f00".to_string(),
                merged: false,
                at: Instant::now().checked_sub(age).expect("recent enough"),
            }),
            interval: base * GIT_STAT_BACKOFF,
        };

        // Well past MERGE_RECHECK, nowhere near this session's own interval.
        git.known.insert("s1".to_string(), aged(MERGE_RECHECK * 2));
        git.request("s1", dir.path().to_path_buf());
        assert!(
            git.inflight.is_empty(),
            "an overdue merge answer must not pull the whole stat forward"
        );

        // And on the poll that is due, the answer is withheld from the worker —
        // which is the recheck being issued, and is stamped as such.
        git.known
            .insert("s1".to_string(), aged(base * GIT_STAT_BACKOFF * 2));
        git.request("s1", dir.path().to_path_buf());
        assert!(git.inflight.contains("s1"), "the poll itself is due");
        let stamp = git.known["s1"]
            .merge
            .as_ref()
            .expect("the answer is still held")
            .at;
        assert!(
            stamp.elapsed() < Duration::from_secs(5),
            "withholding the answer is the recheck, so it is stamped now"
        );
    }

    #[test]
    fn git_polling_turned_off_asks_for_nothing() {
        // `git_poll_secs = 0`: the escape hatch for a machine where every
        // subprocess is scanned by an endpoint-protection agent before it may
        // run. Nothing is asked, and nothing is remembered to publish.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut git = GitStats::new(None);
        git.request("s1", dir.path().to_path_buf());
        assert!(git.inflight.is_empty(), "no worker was started");
        assert!(git.known.is_empty(), "and nothing was recorded");
    }
}
