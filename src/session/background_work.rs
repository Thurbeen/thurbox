//! The sub-work a session has in flight, and what it does to a reported state
//! (issue #1510).
//!
//! An agent's turn can end while work it started goes on running — claude's
//! background subagents. Read as `done`, such a session is torn down or handed
//! new work too early, so a turn that ends with sub-work in flight stores
//! `background` instead, and the edge that ends the last of it stores `done`.
//!
//! Counted by id rather than by a number: a hook can be delivered twice, and a
//! repeated start or end must not move the count. Ended ids are remembered too,
//! so an end that lands before its own start (two hook processes racing) does
//! not leave the start counted for good. The rules are pure and run inside the
//! storage write that applies them, so two hooks racing each other are decided
//! one after the other against what the first one wrote.
//!
//! **A missed end edge must not leave a session in `background` for good.**
//! Nothing here keys on time — sub-work may legitimately run for an hour — so
//! what forgets an orphan is the session moving on:
//!
//! - the agent starting again (`idle`, claude's `SessionStart`) forgets
//!   everything: whatever was in flight died with the previous process;
//! - a new turn — a `working` report marked as a turn start (claude's
//!   `UserPromptSubmit`) — forgets the earlier turns' work whatever the state
//!   was, so an interrupted subagent cannot outlive its turn either, and the
//!   end of that turn reads `done` (a subagent that outlives a new prompt is
//!   no longer counted, which errs toward today's `done` rather than toward a
//!   state nothing can leave). A `working` the agent reports from inside a
//!   turn — its own tool calls, including those of the turn claude starts to
//!   report a finished subagent back — forgets nothing;
//! - `session stop` and a restart clear the column with the hook state;
//! - `session get --verify` reports a dead pane as contradicting it. The
//!   interface does not fold it by the terminal: sub-work runs out of the
//!   pane's sight, and an interface not yet attached has no answer either.
//!
//! The ids are also what makes a backend's status channel safe to read back.
//! `session signal` publishes the word it stored after its transaction, so a
//! `background` published late can arrive after the end of the last work was
//! stored as `done`; read back as a [`ReportFrom::Echo`] (by the interface's
//! drain or the heartbeat's status poll), it is refused for a session whose
//! work was counted and has all ended.

/// How many ids each list remembers at most. A start whose end never comes
/// should not grow the row without bound; the oldest is forgotten first.
pub const MAX_IN_FLIGHT: usize = 64;

/// The longest work id kept, in bytes. Ids are opaque words a hook extracts
/// from its agent's payload (claude's `agent_id` is 17 hex digits).
pub const MAX_WORK_ID: usize = 128;

/// Where a state report came from, which decides what it does to the work in
/// flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFrom<'a> {
    /// The agent, from inside a turn or about one (a tool call, a stop).
    Agent,
    /// The agent starting a turn: a prompt was submitted.
    TurnStart,
    /// Sub-work `id`, reporting on itself.
    Work(&'a str),
    /// A backend's status channel handing back a word written to it — in the
    /// order the writers published, which need not be the order they stored.
    Echo,
}

/// One edge of a piece of sub-work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkEdge {
    Start,
    End,
}

/// The sub-work ids in flight, and the ones already ended, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InFlight {
    #[serde(default)]
    running: Vec<String>,
    #[serde(default)]
    ended: Vec<String>,
}

impl InFlight {
    /// Read the stored column — `{"running": [...], "ended": [...]}`, NULL
    /// when both are empty. Anything unreadable is read as nothing in flight,
    /// the answer that cannot strand a session.
    pub fn parse(stored: Option<&str>) -> Self {
        stored
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or_default()
    }

    /// The column to store: `None` when there is nothing to remember.
    pub fn to_column(&self) -> Option<String> {
        (!self.running.is_empty() || !self.ended.is_empty())
            .then(|| serde_json::to_string(self).unwrap_or_default())
    }

    /// Whether no work is running.
    pub fn is_empty(&self) -> bool {
        self.running.is_empty()
    }

    /// How much work is running.
    pub fn len(&self) -> usize {
        self.running.len()
    }

    /// Whether work was counted here and all of it has ended.
    fn all_ended(&self) -> bool {
        self.running.is_empty() && !self.ended.is_empty()
    }

    /// Whether `id` is running.
    pub fn contains(&self, id: &str) -> bool {
        self.running.iter().any(|known| known == id)
    }

    /// Forget every running id; the ended ones are kept, so a late start of
    /// one of them is still recognised.
    fn forget_running(&mut self) {
        self.running.clear();
    }

    fn forget_all(&mut self) {
        self.running.clear();
        self.ended.clear();
    }

    fn start(&mut self, id: &str) {
        if self.contains(id) || self.ended.iter().any(|known| known == id) {
            return;
        }
        push_capped(&mut self.running, id);
    }

    /// Whether `id` was running.
    fn end(&mut self, id: &str) -> bool {
        let before = self.running.len();
        self.running.retain(|known| known != id);
        if !self.ended.iter().any(|known| known == id) {
            push_capped(&mut self.ended, id);
        }
        self.running.len() != before
    }
}

fn push_capped(ids: &mut Vec<String>, id: &str) {
    if ids.len() >= MAX_IN_FLIGHT {
        ids.remove(0);
    }
    ids.push(id.to_string());
}

/// Whether `id` is a work id worth keeping: not blank, and not so long that
/// it is plainly not an id.
pub fn valid_work_id(id: &str) -> bool {
    !id.trim().is_empty() && id.len() <= MAX_WORK_ID
}

/// What a reported state becomes, given what the row held and the work in
/// flight — and the work in flight after it.
///
/// A subagent's own tool call says `working` about the subagent, not about
/// the turn, so while the turn rests in `background` it is that, not a new
/// turn. Only a [`ReportFrom::TurnStart`] forgets running work: the agent's
/// other reports cannot tell a turn starting from one already under way.
pub fn settle_report(
    previous: Option<&str>,
    in_flight: &mut InFlight,
    reported: &str,
    from: ReportFrom<'_>,
) -> String {
    match (reported, from) {
        ("idle", _) => in_flight.forget_all(),
        ("working", ReportFrom::Work(id))
            if previous == Some("background") && in_flight.contains(id) =>
        {
            return "background".into();
        }
        ("working", ReportFrom::TurnStart) => in_flight.forget_running(),
        ("background", ReportFrom::Echo) if in_flight.all_ended() => {
            return previous.unwrap_or(reported).to_string();
        }
        ("done", _) if !in_flight.is_empty() => return "background".into(),
        _ => {}
    }
    reported.to_string()
}

/// What one start or end of sub-work does: the state the row should hold
/// afterwards, or `None` when the state does not move.
///
/// A start never moves the state — the turn that started the work is still
/// running, and what it ends as is decided by [`settle_report`] when it ends.
/// The end of the last piece of work moves a `background` session to `done`,
/// which is the edge a driver waiting for the session to finish is waiting for.
/// An end for an id nothing knows changes no state, but is remembered: if its
/// start arrives after it, that start is not counted.
pub fn settle_edge(
    previous: Option<&str>,
    in_flight: &mut InFlight,
    edge: WorkEdge,
    id: &str,
) -> Option<&'static str> {
    match edge {
        WorkEdge::Start => {
            in_flight.start(id);
            None
        }
        WorkEdge::End => {
            let ended = in_flight.end(id);
            (ended && in_flight.is_empty() && previous == Some("background")).then_some("done")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started(ids: &[&str]) -> InFlight {
        let mut work = InFlight::default();
        for id in ids {
            settle_edge(Some("working"), &mut work, WorkEdge::Start, id);
        }
        work
    }

    #[test]
    fn a_turn_that_ends_with_work_in_flight_is_background() {
        let mut work = started(&["a"]);
        assert_eq!(
            settle_report(Some("working"), &mut work, "done", ReportFrom::Agent),
            "background"
        );
        assert_eq!(work.len(), 1);
    }

    #[test]
    fn a_turn_that_ends_with_nothing_in_flight_is_done() {
        let mut work = InFlight::default();
        assert_eq!(
            settle_report(Some("working"), &mut work, "done", ReportFrom::Agent),
            "done"
        );
    }

    #[test]
    fn the_last_end_finishes_a_background_session_and_not_before() {
        let mut work = started(&["a", "b"]);
        assert_eq!(
            settle_edge(Some("background"), &mut work, WorkEdge::End, "a"),
            None
        );
        assert_eq!(
            settle_edge(Some("background"), &mut work, WorkEdge::End, "b"),
            Some("done")
        );
        assert!(work.is_empty());
    }

    #[test]
    fn work_that_ends_inside_its_turn_moves_nothing() {
        let mut work = started(&["a"]);
        assert_eq!(
            settle_edge(Some("working"), &mut work, WorkEdge::End, "a"),
            None
        );
        assert_eq!(
            settle_report(Some("working"), &mut work, "done", ReportFrom::Agent),
            "done"
        );
    }

    #[test]
    fn a_repeated_edge_does_not_move_the_count() {
        let mut work = started(&["a", "a"]);
        assert_eq!(work.len(), 1);
        assert_eq!(
            settle_edge(Some("background"), &mut work, WorkEdge::End, "a"),
            Some("done")
        );
        assert_eq!(
            settle_edge(Some("done"), &mut work, WorkEdge::End, "a"),
            None
        );
    }

    /// Two hook processes racing can land an end before its own start. The
    /// start that arrives late must not be counted, or the session would rest
    /// in `background` with nothing running.
    #[test]
    fn an_end_that_lands_before_its_start_cancels_it() {
        let mut work = InFlight::default();
        assert_eq!(
            settle_edge(Some("working"), &mut work, WorkEdge::End, "a"),
            None
        );
        settle_edge(Some("working"), &mut work, WorkEdge::Start, "a");
        assert!(work.is_empty());
        assert_eq!(
            settle_report(Some("working"), &mut work, "done", ReportFrom::Agent),
            "done"
        );
    }

    /// A start delivered again after its end is the same late start.
    #[test]
    fn a_start_repeated_after_its_end_is_not_counted_again() {
        let mut work = started(&["a"]);
        settle_edge(Some("working"), &mut work, WorkEdge::End, "a");
        settle_edge(Some("working"), &mut work, WorkEdge::Start, "a");
        assert!(work.is_empty());
    }

    #[test]
    fn an_end_nothing_started_changes_nothing() {
        let mut work = started(&["a"]);
        assert_eq!(
            settle_edge(Some("background"), &mut work, WorkEdge::End, "zzz"),
            None
        );
        assert_eq!(work.len(), 1);
    }

    #[test]
    fn the_work_s_own_activity_keeps_a_resting_turn_in_background() {
        let mut work = started(&["a"]);
        assert_eq!(
            settle_report(
                Some("background"),
                &mut work,
                "working",
                ReportFrom::Work("a")
            ),
            "background"
        );
        // Work nothing counted is reported as it always was.
        assert_eq!(
            settle_report(
                Some("background"),
                &mut work,
                "working",
                ReportFrom::Work("b")
            ),
            "working"
        );
        assert_eq!(work.len(), 1);
    }

    #[test]
    fn a_new_turn_forgets_the_work_of_earlier_ones() {
        let mut work = started(&["orphan"]);
        assert_eq!(
            settle_report(
                Some("background"),
                &mut work,
                "working",
                ReportFrom::TurnStart
            ),
            "working"
        );
        assert!(work.is_empty());
        assert_eq!(
            settle_report(Some("working"), &mut work, "done", ReportFrom::Agent),
            "done"
        );
    }

    #[test]
    fn the_turn_s_own_tool_calls_forget_nothing() {
        let mut work = started(&["a"]);
        assert_eq!(
            settle_report(Some("working"), &mut work, "working", ReportFrom::Agent),
            "working"
        );
        assert_eq!(
            settle_report(Some("blocked"), &mut work, "working", ReportFrom::Agent),
            "working"
        );
        assert_eq!(work.len(), 1);
    }

    /// The turn claude starts to report one finished subagent back may make
    /// tool calls of its own; they are not a new prompt, and the subagent
    /// still running must still be counted when that turn stops.
    #[test]
    fn a_wake_up_turn_s_tool_calls_keep_the_other_work_counted() {
        let mut work = started(&["a", "b"]);
        settle_edge(Some("background"), &mut work, WorkEdge::End, "a");
        assert_eq!(
            settle_report(Some("background"), &mut work, "working", ReportFrom::Agent),
            "working"
        );
        assert_eq!(
            settle_report(Some("working"), &mut work, "done", ReportFrom::Agent),
            "background"
        );
    }

    /// A late echo of `background` is refused once the counted work has all
    /// ended; with nothing ever counted (a remote pane) it is the report.
    #[test]
    fn a_background_echo_needs_work_still_running() {
        let mut work = started(&["a"]);
        settle_edge(Some("background"), &mut work, WorkEdge::End, "a");
        assert_eq!(
            settle_report(Some("done"), &mut work, "background", ReportFrom::Echo),
            "done"
        );
        let mut none = InFlight::default();
        assert_eq!(
            settle_report(Some("done"), &mut none, "background", ReportFrom::Echo),
            "background"
        );
        let mut running = started(&["b"]);
        assert_eq!(
            settle_report(
                Some("working"),
                &mut running,
                "background",
                ReportFrom::Echo
            ),
            "background"
        );
    }

    /// An interrupted turn fires no stop, so its state is still `working` when
    /// the next prompt arrives; that prompt still forgets the work it started.
    #[test]
    fn a_turn_start_forgets_work_whatever_the_state() {
        let mut work = started(&["interrupted"]);
        assert_eq!(
            settle_report(Some("working"), &mut work, "working", ReportFrom::TurnStart),
            "working"
        );
        assert!(work.is_empty());
    }

    #[test]
    fn the_agent_starting_again_forgets_everything() {
        let mut work = started(&["a", "b"]);
        assert_eq!(
            settle_report(Some("background"), &mut work, "idle", ReportFrom::Agent),
            "idle"
        );
        assert!(work.is_empty());
    }

    #[test]
    fn the_oldest_id_is_forgotten_past_the_cap() {
        let ids: Vec<String> = (0..=MAX_IN_FLIGHT).map(|i| format!("w{i}")).collect();
        let mut work = InFlight::default();
        for id in &ids {
            settle_edge(Some("working"), &mut work, WorkEdge::Start, id);
        }
        assert_eq!(work.len(), MAX_IN_FLIGHT);
        assert!(!work.contains("w0"));
        assert!(work.contains(&format!("w{MAX_IN_FLIGHT}")));
        for id in &ids {
            settle_edge(Some("working"), &mut work, WorkEdge::End, id);
        }
        assert!(work.is_empty());
        assert_eq!(work.ended.len(), MAX_IN_FLIGHT);
    }

    #[test]
    fn the_column_round_trips_and_garbage_reads_as_nothing() {
        let mut work = started(&["a", "b"]);
        settle_edge(Some("working"), &mut work, WorkEdge::End, "a");
        assert_eq!(InFlight::parse(work.to_column().as_deref()), work);
        assert_eq!(InFlight::default().to_column(), None);
        assert!(InFlight::parse(Some("not json")).is_empty());
        assert!(InFlight::parse(None).is_empty());
    }

    #[test]
    fn a_work_id_is_a_short_non_blank_word() {
        assert!(valid_work_id("a2a3df31469ca24ca"));
        assert!(!valid_work_id(" "));
        assert!(!valid_work_id(&"x".repeat(MAX_WORK_ID + 1)));
    }
}
