use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::GitState;

type StatResult = (String, Option<GitState>, Option<(String, bool)>);

pub(super) fn git_poll_interval(secs: u64) -> Option<Duration> {
    (secs > 0).then(|| Duration::from_secs(secs))
}

const GIT_STAT_BACKOFF: u32 = 12;

const MERGE_RECHECK: Duration = Duration::from_secs(60);

struct MergeAnswer {
    head: String,
    merged: bool,
    at: Instant,
}

struct Stat {
    state: Option<GitState>,
    at: Instant,
    merge: Option<MergeAnswer>,
    interval: Duration,
}

pub(super) struct GitStats {
    poll: Option<Duration>,
    known: std::collections::HashMap<String, Stat>,
    inflight: std::collections::HashSet<String>,
    channel: Option<(
        std::sync::mpsc::Sender<StatResult>,
        std::sync::mpsc::Receiver<StatResult>,
    )>,
}

impl GitStats {
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

    pub(super) fn state(&self, session: &str) -> Option<GitState> {
        self.known.get(session).and_then(|stat| stat.state)
    }

    pub(super) fn retain(&mut self, present: &std::collections::HashSet<&str>) {
        self.known.retain(|id, _| present.contains(id.as_str()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
