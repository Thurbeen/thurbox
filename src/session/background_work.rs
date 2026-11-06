//! The work a session has in flight, and what it does to a reported state
//! (issue #1510) — and whether a session that ended its turn is provably at
//! rest.
//!
//! An agent's turn can end while work it started goes on running — claude's
//! background subagents, shells, Monitors, workflows, and the session crons
//! (CronCreate, ScheduleWakeup, `/loop`) that will wake it later. Read as
//! `done`, such a session is torn down, paused or handed new work too early,
//! so a turn that ends with work in flight stores `background` instead.
//!
//! Two sources say what is in flight, and the column ([`WorkRecord`]) keeps
//! both:
//!
//! - **The agent's own account**, [`StopReport`]: claude's `Stop` payload
//!   lists `background_tasks[]` and `session_crons[]`. Either list non-empty
//!   stores `background` and keeps the list as [`WorkRecord::pending`]; both
//!   present and empty stores `done` and *proves* the session idle
//!   ([`IdleProof::Stop`]). A list that is missing (a Claude Code older than
//!   the field) is unknown, never empty: the edges below decide, and nothing
//!   is proven.
//! - **Edges**, counted by id: `--work-start`/`--work-end`, which claude's
//!   `SubagentStart`/`SubagentStop` hooks emit. They are what an agent with no
//!   such payload has, and what keeps a remote pane's single status word
//!   honest about subagents.
//!
//! A finished background task or a fired cron starts a new turn, and that turn
//! ends with a fresh `Stop`, so a payload-held `background` is cleared by the
//! next payload rather than by an end edge. An end edge clears it only when it
//! empties both the counted ids and the listed work.
//!
//! **Idle is proven, never assumed.** [`IdleProof`] is set by exactly two
//! things: a `Stop` whose lists are both present and empty, or an idle claim
//! the agent makes about the turn that just ended ([`StopReport::idle_claimed`],
//! or `session signal --idle` after it). Every later report takes it back, so
//! it only ever describes the turn that ended last.
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
//! - a `Stop` payload whose lists are empty forgets every counted id: the
//!   agent's own account outranks an edge that never came;
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

/// How many ids each list remembers at most, and how many pending items a
/// `Stop` payload is kept to. A start whose end never comes should not grow
/// the row without bound; the oldest is forgotten first.
pub const MAX_IN_FLIGHT: usize = 64;

/// The longest work id kept, in bytes. Ids are opaque words a hook extracts
/// from its agent's payload (claude's `agent_id` is 17 hex digits).
pub const MAX_WORK_ID: usize = 128;

/// The longest description kept for one pending item, in characters. Claude
/// sends up to 1000; a reader deciding whether to pause wants the gist.
pub const MAX_DESCRIPTION: usize = 200;

/// The line an agent ends its final message with to claim that nothing it
/// started is still running or scheduled — what thurbox's injected instruction
/// asks for, and what [`StopReport::parse`] looks for in
/// `last_assistant_message`.
///
/// A line in the reply rather than a command the agent runs: a command is a
/// tool call *inside* the turn, followed by that call's own hooks, and an
/// agent's sandbox may not let it reach thurbox's database at all (codex's
/// `workspace-write` refuses the write). The `Stop` hook runs outside both.
pub const IDLE_MARKER: &str = "[thurbox-idle]";

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

/// What proves a session idle — see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdleProof {
    /// The agent's own `Stop` payload listed nothing in flight and nothing
    /// scheduled.
    Stop,
    /// The agent claimed the turn that just ended left nothing running.
    Claim,
}

impl IdleProof {
    /// The stable word `session get` reports it as.
    pub fn as_str(self) -> &'static str {
        match self {
            IdleProof::Stop => "stop",
            IdleProof::Claim => "claim",
        }
    }
}

/// One piece of work the agent reported still in flight or scheduled when its
/// turn ended.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Pending {
    /// The agent's own word for it (`shell`, `subagent`, `monitor`,
    /// `workflow`, …), or `cron` for a scheduled wake-up.
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A cron's schedule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// Whether a cron fires again after it fires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurring: Option<bool>,
}

/// What an agent's `Stop` payload says about the turn that just ended.
///
/// Each list is `None` when the payload does not carry it — unknown, which is
/// a different answer from an empty list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StopReport {
    pub tasks: Option<Vec<Pending>>,
    pub crons: Option<Vec<Pending>>,
    /// The final message ends with [`IDLE_MARKER`].
    pub idle_claimed: bool,
}

impl StopReport {
    /// Read a `Stop` hook payload. Anything that is not a JSON object reads as
    /// nothing known, the answer that proves nothing.
    pub fn parse(payload: &str) -> Self {
        let Ok(serde_json::Value::Object(map)) = serde_json::from_str(payload) else {
            return Self::default();
        };
        let list = |key: &str, read: fn(&serde_json::Value) -> Option<Pending>| {
            map.get(key)
                .and_then(serde_json::Value::as_array)
                .map(|items| items.iter().filter_map(read).collect::<Vec<_>>())
        };
        Self {
            tasks: list("background_tasks", read_task),
            crons: list("session_crons", read_cron),
            idle_claimed: map
                .get("last_assistant_message")
                .and_then(serde_json::Value::as_str)
                .is_some_and(ends_with_idle_marker),
        }
    }

    /// Whether both lists are present and empty — the payload proves idle.
    fn proves_idle(&self) -> bool {
        matches!((&self.tasks, &self.crons), (Some(t), Some(c)) if t.is_empty() && c.is_empty())
    }

    /// Every listed item, tasks first, kept to [`MAX_IN_FLIGHT`] — and how
    /// many past the cap were left out.
    fn pending(&self) -> (Vec<Pending>, usize) {
        let all = self.tasks.iter().chain(&self.crons).flatten();
        let total = all.clone().count();
        let kept = all.take(MAX_IN_FLIGHT).cloned().collect();
        (kept, total.saturating_sub(MAX_IN_FLIGHT))
    }
}

fn text(value: &serde_json::Value, key: &str) -> Option<String> {
    let text = value.get(key)?.as_str()?.trim();
    (!text.is_empty()).then(|| text.chars().take(MAX_DESCRIPTION).collect())
}

/// An entry with no id is still work in flight — it is kept, under an empty
/// id, because dropping it would read a non-empty list as an empty one.
fn read_task(value: &serde_json::Value) -> Option<Pending> {
    value.is_object().then(|| Pending {
        kind: text(value, "type").unwrap_or_else(|| "task".into()),
        id: text(value, "id").unwrap_or_default(),
        description: text(value, "description"),
        schedule: None,
        recurring: None,
    })
}

fn read_cron(value: &serde_json::Value) -> Option<Pending> {
    value.is_object().then(|| Pending {
        kind: "cron".into(),
        id: text(value, "id").unwrap_or_default(),
        description: text(value, "prompt"),
        schedule: text(value, "schedule"),
        recurring: Some(
            value
                .get("recurring")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        ),
    })
}

/// Whether `message`'s last non-blank line is [`IDLE_MARKER`], allowing the
/// code or emphasis marks a model may wrap it in.
pub fn ends_with_idle_marker(message: &str) -> bool {
    message
        .lines()
        .map(|line| {
            line.trim()
                .trim_matches(|c: char| c == '`' || c == '*' || c == '_')
        })
        .rfind(|line| !line.is_empty())
        == Some(IDLE_MARKER)
}

/// The session's work column: the sub-work ids in flight and the ones already
/// ended (oldest first), what the last `Stop` listed, and what proves the
/// session idle.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkRecord {
    #[serde(default)]
    running: Vec<String>,
    #[serde(default)]
    ended: Vec<String>,
    /// What the agent's last `Stop` listed still in flight or scheduled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending: Vec<Pending>,
    /// How many more items that `Stop` listed past [`MAX_IN_FLIGHT`]. The cap
    /// bounds what is shown, never whether work remains: an omitted item can
    /// only be cleared by the next `Stop`, so it holds `background` until then.
    #[serde(default, skip_serializing_if = "is_zero")]
    omitted: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    idle: Option<IdleProof>,
}

impl WorkRecord {
    /// Read the stored column (NULL when there is nothing to remember).
    /// Anything unreadable is read as nothing in flight and nothing proven,
    /// the answer that can neither strand a session nor call it idle.
    pub fn parse(stored: Option<&str>) -> Self {
        stored
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or_default()
    }

    /// The column to store: `None` when there is nothing to remember.
    pub fn to_column(&self) -> Option<String> {
        (*self != Self::default()).then(|| serde_json::to_string(self).unwrap_or_default())
    }

    /// Whether no counted sub-work is running.
    pub fn is_empty(&self) -> bool {
        self.running.is_empty()
    }

    /// How much counted sub-work is running.
    pub fn len(&self) -> usize {
        self.running.len()
    }

    /// What proves the session idle, if anything does.
    pub fn idle_proof(&self) -> Option<IdleProof> {
        self.idle
    }

    /// Everything known to be in flight or scheduled: what the last `Stop`
    /// listed, else the counted sub-work ids.
    pub fn pending(&self) -> Vec<Pending> {
        if !self.pending.is_empty() {
            return self.pending.clone();
        }
        self.running
            .iter()
            .map(|id| Pending {
                kind: "subagent".into(),
                id: id.clone(),
                description: None,
                schedule: None,
                recurring: None,
            })
            .collect()
    }

    /// How many items the last `Stop` listed past the cap of
    /// [`WorkRecord::pending`].
    pub fn pending_omitted(&self) -> usize {
        self.omitted
    }

    /// Whether the last `Stop` listed work that is still held, shown or not.
    fn listed(&self) -> bool {
        !self.pending.is_empty() || self.omitted > 0
    }

    fn forget_listed(&mut self) {
        self.pending.clear();
        self.omitted = 0;
    }

    /// Whether work was counted here and all of it has ended, with nothing
    /// listed either.
    fn all_ended(&self) -> bool {
        self.running.is_empty() && !self.ended.is_empty() && !self.listed()
    }

    /// Whether `id` is running.
    pub fn contains(&self, id: &str) -> bool {
        self.running.iter().any(|known| known == id)
    }

    /// Forget every running id; the ended ones are kept, so a late start of
    /// one of them is still recognised.
    fn forget_running(&mut self) {
        self.running.clear();
        self.forget_listed();
    }

    fn forget_all(&mut self) {
        self.running.clear();
        self.ended.clear();
        self.forget_listed();
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
        // The subagent's entry in a `Stop` list carries the same id.
        self.pending.retain(|item| item.id != id);
        if !self.ended.iter().any(|known| known == id) {
            push_capped(&mut self.ended, id);
        }
        self.running.len() != before
    }

    /// What a turn ending makes of the record, and the state it ends as.
    fn settle_turn_end(&mut self, stop: Option<&StopReport>) -> &'static str {
        let (listed, omitted) = stop.map(StopReport::pending).unwrap_or_default();
        if !listed.is_empty() {
            self.pending = listed;
            self.omitted = omitted;
            return "background";
        }
        self.forget_listed();
        if stop.is_some_and(StopReport::proves_idle) {
            // The agent's own account outranks an end edge that never came.
            self.running.clear();
            self.idle = Some(IdleProof::Stop);
            return "done";
        }
        if !self.running.is_empty() {
            return "background";
        }
        if stop.is_some_and(|s| s.idle_claimed) {
            self.idle = Some(IdleProof::Claim);
        }
        "done"
    }
}

fn is_zero(n: &usize) -> bool {
    *n == 0
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
/// flight — and the work record after it.
///
/// A subagent's own tool call says `working` about the subagent, not about
/// the turn, so while the turn rests in `background` it is that, not a new
/// turn. Only a [`ReportFrom::TurnStart`] forgets running work: the agent's
/// other reports cannot tell a turn starting from one already under way.
///
/// `stop` is the turn's `Stop` payload, read only for a `done`. Every report
/// but an echo that is not news takes back an [`IdleProof`]; a `done` may set
/// a new one.
///
/// An echo never replaces what a `Stop` listed: it carries no payload, and the
/// interface may hand back a word published before that `Stop` was stored.
pub fn settle_report(
    previous: Option<&str>,
    work: &mut WorkRecord,
    reported: &str,
    from: ReportFrom<'_>,
    stop: Option<&StopReport>,
) -> String {
    // An echo that changes nothing — the word already stored, a late
    // `background` for work that has all ended, or any word while a `Stop`
    // proof holds — is not news, and takes back no proof. A `Stop` proof is
    // only ever set by a local write, so every word it could be echoing was
    // stored before it; a remote session, whose only reports are echoes,
    // never holds one.
    if from == ReportFrom::Echo
        && (previous == Some(reported)
            || work.idle == Some(IdleProof::Stop)
            || (reported == "background" && work.all_ended()))
    {
        return previous.unwrap_or(reported).to_string();
    }
    work.idle = None;
    match (reported, from) {
        ("idle", _) => work.forget_all(),
        ("working", ReportFrom::Work(id))
            if previous == Some("background") && work.contains(id) =>
        {
            return "background".into();
        }
        ("working", ReportFrom::TurnStart) => work.forget_running(),
        ("done", ReportFrom::Echo) if !work.is_empty() || work.listed() => {
            return "background".into();
        }
        ("done", _) => return work.settle_turn_end(stop).to_string(),
        _ => {}
    }
    reported.to_string()
}

/// Why an idle claim was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimRefused {
    /// The turn has not ended: the state is not `done` or `idle`.
    TurnNotEnded,
    /// Work is still counted or listed as in flight.
    WorkInFlight,
}

/// Take an idle claim about the turn that ended last (`session signal
/// --idle`). Accepted only when the turn has ended and nothing is known to be
/// in flight; the next report of any kind takes it back.
pub fn settle_claim(previous: Option<&str>, work: &mut WorkRecord) -> Result<(), ClaimRefused> {
    if !matches!(previous, Some("done" | "idle")) {
        return Err(ClaimRefused::TurnNotEnded);
    }
    if !work.running.is_empty() || work.listed() {
        return Err(ClaimRefused::WorkInFlight);
    }
    if work.idle.is_none() {
        work.idle = Some(IdleProof::Claim);
    }
    Ok(())
}

/// What one start or end of sub-work does: the state the row should hold
/// afterwards, or `None` when the state does not move.
///
/// A start never moves the state — the turn that started the work is still
/// running, and what it ends as is decided by [`settle_report`] when it ends —
/// but it does take back an idle proof. The end of the last piece of work
/// moves a `background` session to `done` (proving nothing), which is the edge
/// a driver waiting for the session to finish is waiting for — unless the
/// last `Stop` listed other work, which only the next `Stop` can clear. An
/// end for an id nothing knows changes no state, but is remembered: if its
/// start arrives after it, that start is not counted.
pub fn settle_edge(
    previous: Option<&str>,
    work: &mut WorkRecord,
    edge: WorkEdge,
    id: &str,
) -> Option<&'static str> {
    match edge {
        WorkEdge::Start => {
            work.start(id);
            work.idle = None;
            None
        }
        WorkEdge::End => {
            let listed = work.listed();
            let ended = work.end(id);
            ((ended || listed)
                && work.is_empty()
                && !work.listed()
                && previous == Some("background"))
            .then_some("done")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(
        previous: Option<&str>,
        work: &mut WorkRecord,
        reported: &str,
        from: ReportFrom<'_>,
    ) -> String {
        settle_report(previous, work, reported, from, None)
    }

    fn started(ids: &[&str]) -> WorkRecord {
        let mut work = WorkRecord::default();
        for id in ids {
            settle_edge(Some("working"), &mut work, WorkEdge::Start, id);
        }
        work
    }

    #[test]
    fn a_turn_that_ends_with_work_in_flight_is_background() {
        let mut work = started(&["a"]);
        assert_eq!(
            report(Some("working"), &mut work, "done", ReportFrom::Agent),
            "background"
        );
        assert_eq!(work.len(), 1);
    }

    #[test]
    fn a_turn_that_ends_with_nothing_in_flight_is_done() {
        let mut work = WorkRecord::default();
        assert_eq!(
            report(Some("working"), &mut work, "done", ReportFrom::Agent),
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
            report(Some("working"), &mut work, "done", ReportFrom::Agent),
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
        let mut work = WorkRecord::default();
        assert_eq!(
            settle_edge(Some("working"), &mut work, WorkEdge::End, "a"),
            None
        );
        settle_edge(Some("working"), &mut work, WorkEdge::Start, "a");
        assert!(work.is_empty());
        assert_eq!(
            report(Some("working"), &mut work, "done", ReportFrom::Agent),
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
            report(
                Some("background"),
                &mut work,
                "working",
                ReportFrom::Work("a")
            ),
            "background"
        );
        // Work nothing counted is reported as it always was.
        assert_eq!(
            report(
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
            report(
                Some("background"),
                &mut work,
                "working",
                ReportFrom::TurnStart
            ),
            "working"
        );
        assert!(work.is_empty());
        assert_eq!(
            report(Some("working"), &mut work, "done", ReportFrom::Agent),
            "done"
        );
    }

    #[test]
    fn the_turn_s_own_tool_calls_forget_nothing() {
        let mut work = started(&["a"]);
        assert_eq!(
            report(Some("working"), &mut work, "working", ReportFrom::Agent),
            "working"
        );
        assert_eq!(
            report(Some("blocked"), &mut work, "working", ReportFrom::Agent),
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
            report(Some("background"), &mut work, "working", ReportFrom::Agent),
            "working"
        );
        assert_eq!(
            report(Some("working"), &mut work, "done", ReportFrom::Agent),
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
            report(Some("done"), &mut work, "background", ReportFrom::Echo),
            "done"
        );
        let mut none = WorkRecord::default();
        assert_eq!(
            report(Some("done"), &mut none, "background", ReportFrom::Echo),
            "background"
        );
        let mut running = started(&["b"]);
        assert_eq!(
            report(
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
            report(Some("working"), &mut work, "working", ReportFrom::TurnStart),
            "working"
        );
        assert!(work.is_empty());
    }

    #[test]
    fn the_agent_starting_again_forgets_everything() {
        let mut work = started(&["a", "b"]);
        assert_eq!(
            report(Some("background"), &mut work, "idle", ReportFrom::Agent),
            "idle"
        );
        assert!(work.is_empty());
    }

    #[test]
    fn the_oldest_id_is_forgotten_past_the_cap() {
        let ids: Vec<String> = (0..=MAX_IN_FLIGHT).map(|i| format!("w{i}")).collect();
        let mut work = WorkRecord::default();
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
        assert_eq!(WorkRecord::parse(work.to_column().as_deref()), work);
        assert_eq!(WorkRecord::default().to_column(), None);
        assert!(WorkRecord::parse(Some("not json")).is_empty());
        assert!(WorkRecord::parse(None).is_empty());
    }

    #[test]
    fn a_work_id_is_a_short_non_blank_word() {
        assert!(valid_work_id("a2a3df31469ca24ca"));
        assert!(!valid_work_id(" "));
        assert!(!valid_work_id(&"x".repeat(MAX_WORK_ID + 1)));
    }

    fn stop(payload: serde_json::Value) -> StopReport {
        StopReport::parse(&payload.to_string())
    }

    #[test]
    fn a_stop_payload_reads_its_lists_and_tells_missing_from_empty() {
        let both = stop(serde_json::json!({
            "background_tasks": [
                {"id": "b1", "type": "shell", "status": "running", "description": "npm run dev"},
                {"id": "m1", "type": "monitor", "status": "running"}
            ],
            "session_crons": [
                {"id": "c1", "schedule": "*/5 * * * *", "recurring": true, "prompt": "check"}
            ]
        }));
        let (items, _) = both.pending();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].kind, "shell");
        assert_eq!(items[0].description.as_deref(), Some("npm run dev"));
        assert_eq!(items[2].kind, "cron");
        assert_eq!(items[2].schedule.as_deref(), Some("*/5 * * * *"));
        assert_eq!(items[2].recurring, Some(true));

        assert!(
            stop(serde_json::json!({"background_tasks": [], "session_crons": []})).proves_idle()
        );
        assert!(!stop(serde_json::json!({"background_tasks": []})).proves_idle());
        assert!(!stop(serde_json::json!({})).proves_idle());
        assert_eq!(StopReport::parse("not json"), StopReport::default());
    }

    #[test]
    fn the_idle_marker_is_the_last_line_of_the_reply() {
        assert!(ends_with_idle_marker("Done.\n\n[thurbox-idle]\n"));
        assert!(ends_with_idle_marker("Done.\n`[thurbox-idle]`"));
        assert!(!ends_with_idle_marker(
            "[thurbox-idle]\nbut the watcher is still up"
        ));
        assert!(!ends_with_idle_marker(
            "I will add [thurbox-idle] when done"
        ));
        assert!(!ends_with_idle_marker(""));
    }

    #[test]
    fn a_long_list_is_kept_to_the_cap() {
        let tasks: Vec<_> = (0..=MAX_IN_FLIGHT)
            .map(|i| serde_json::json!({"id": format!("t{i}"), "type": "shell"}))
            .collect();
        let report = stop(serde_json::json!({"background_tasks": tasks, "session_crons": []}));
        let mut work = WorkRecord::default();
        assert_eq!(
            settle_report(
                Some("working"),
                &mut work,
                "done",
                ReportFrom::Agent,
                Some(&report)
            ),
            "background"
        );
        assert_eq!(work.pending().len(), MAX_IN_FLIGHT);
        assert_eq!(work.pending_omitted(), 1);
    }

    /// The cap bounds what is shown, not whether work remains: a cron listed
    /// past it still holds `background` once every shown item has ended.
    #[test]
    fn work_past_the_cap_holds_background_until_the_next_payload() {
        let tasks: Vec<_> = (0..MAX_IN_FLIGHT)
            .map(|i| serde_json::json!({"id": format!("s{i}"), "type": "subagent"}))
            .collect();
        let report = stop(serde_json::json!({
            "background_tasks": tasks,
            "session_crons": [{"id": "c1", "schedule": "*/5 * * * *", "recurring": true}]
        }));
        let mut work = WorkRecord::default();
        settle_report(
            Some("working"),
            &mut work,
            "done",
            ReportFrom::Agent,
            Some(&report),
        );
        for i in 0..MAX_IN_FLIGHT {
            assert_eq!(
                settle_edge(
                    Some("background"),
                    &mut work,
                    WorkEdge::End,
                    &format!("s{i}")
                ),
                None
            );
        }
        assert!(work.pending().is_empty());
        assert_eq!(work.pending_omitted(), 1);
        assert_eq!(
            settle_claim(Some("done"), &mut work),
            Err(ClaimRefused::WorkInFlight)
        );
        let round_trip = WorkRecord::parse(work.to_column().as_deref());
        assert_eq!(round_trip.pending_omitted(), 1);
    }

    /// A `done` echo published before a `Stop` that listed work does not
    /// replace that list, and a claim made after it is still refused.
    #[test]
    fn a_late_done_echo_keeps_the_listed_work() {
        let mut work = WorkRecord::default();
        let shell = stop(serde_json::json!({
            "background_tasks": [{"id": "b1", "type": "shell"}],
            "session_crons": []
        }));
        settle_report(
            Some("working"),
            &mut work,
            "done",
            ReportFrom::Agent,
            Some(&shell),
        );
        assert_eq!(
            report(Some("background"), &mut work, "done", ReportFrom::Echo),
            "background"
        );
        assert_eq!(work.pending().len(), 1);
        assert_eq!(
            settle_claim(Some("background"), &mut work),
            Err(ClaimRefused::TurnNotEnded)
        );
    }

    /// Once a `Stop` proved the session idle, a word published before it and
    /// echoed late neither moves the state nor takes the proof back.
    #[test]
    fn a_late_echo_does_not_undo_a_stop_proof() {
        let mut work = WorkRecord::default();
        let empty = stop(serde_json::json!({"background_tasks": [], "session_crons": []}));
        settle_report(
            Some("background"),
            &mut work,
            "done",
            ReportFrom::Agent,
            Some(&empty),
        );
        for late in ["background", "working"] {
            assert_eq!(
                report(Some("done"), &mut work, late, ReportFrom::Echo),
                "done"
            );
            assert_eq!(work.idle_proof(), Some(IdleProof::Stop));
        }
    }

    #[test]
    fn a_payload_proving_nothing_runs_proves_idle_and_the_next_report_takes_it_back() {
        let mut work = started(&["missed"]);
        let empty = stop(serde_json::json!({"background_tasks": [], "session_crons": []}));
        assert_eq!(
            settle_report(
                Some("working"),
                &mut work,
                "done",
                ReportFrom::Agent,
                Some(&empty)
            ),
            "done"
        );
        assert_eq!(work.idle_proof(), Some(IdleProof::Stop));
        assert!(work.is_empty());
        // An echo of the word stored is not news.
        assert_eq!(
            report(Some("done"), &mut work, "done", ReportFrom::Echo),
            "done"
        );
        assert_eq!(work.idle_proof(), Some(IdleProof::Stop));
        assert_eq!(
            report(Some("done"), &mut work, "working", ReportFrom::Agent),
            "working"
        );
        assert_eq!(work.idle_proof(), None);
    }

    #[test]
    fn a_payload_with_work_holds_background_until_the_next_payload() {
        let mut work = WorkRecord::default();
        let shell = stop(serde_json::json!({
            "background_tasks": [{"id": "a1", "type": "subagent"}, {"id": "b1", "type": "shell"}],
            "session_crons": []
        }));
        assert_eq!(
            settle_report(
                Some("working"),
                &mut work,
                "done",
                ReportFrom::Agent,
                Some(&shell)
            ),
            "background"
        );
        // A subagent's end removes it from the list, but the shell holds.
        assert_eq!(
            settle_edge(Some("background"), &mut work, WorkEdge::End, "a1"),
            None
        );
        assert_eq!(work.pending().len(), 1);
        // A late echo of `background` is taken while the list holds work.
        assert_eq!(
            report(Some("done"), &mut work, "background", ReportFrom::Echo),
            "background"
        );
    }

    #[test]
    fn an_end_that_empties_the_list_finishes_the_background() {
        let mut work = WorkRecord::default();
        let one = stop(serde_json::json!({
            "background_tasks": [{"id": "a1", "type": "subagent"}],
            "session_crons": []
        }));
        settle_report(
            Some("working"),
            &mut work,
            "done",
            ReportFrom::Agent,
            Some(&one),
        );
        assert_eq!(
            settle_edge(Some("background"), &mut work, WorkEdge::End, "a1"),
            Some("done")
        );
        assert_eq!(work.idle_proof(), None);
    }

    #[test]
    fn a_claim_needs_an_ended_turn_with_nothing_in_flight() {
        let mut work = WorkRecord::default();
        assert_eq!(
            settle_claim(Some("working"), &mut work),
            Err(ClaimRefused::TurnNotEnded)
        );
        assert_eq!(
            settle_claim(None, &mut work),
            Err(ClaimRefused::TurnNotEnded)
        );
        let mut busy = started(&["a"]);
        assert_eq!(
            settle_claim(Some("done"), &mut busy),
            Err(ClaimRefused::WorkInFlight)
        );
        assert_eq!(settle_claim(Some("done"), &mut work), Ok(()));
        assert_eq!(work.idle_proof(), Some(IdleProof::Claim));
        // A work start takes it back.
        settle_edge(Some("done"), &mut work, WorkEdge::Start, "b");
        assert_eq!(work.idle_proof(), None);
    }

    #[test]
    fn a_claim_in_the_stop_payload_proves_idle_only_with_nothing_counted() {
        let claimed = stop(serde_json::json!({"last_assistant_message": "ok\n[thurbox-idle]"}));
        let mut work = WorkRecord::default();
        settle_report(
            Some("working"),
            &mut work,
            "done",
            ReportFrom::Agent,
            Some(&claimed),
        );
        assert_eq!(work.idle_proof(), Some(IdleProof::Claim));
        let mut busy = started(&["a"]);
        assert_eq!(
            settle_report(
                Some("working"),
                &mut busy,
                "done",
                ReportFrom::Agent,
                Some(&claimed)
            ),
            "background"
        );
        assert_eq!(busy.idle_proof(), None);
    }

    #[test]
    fn an_older_column_without_the_new_fields_still_reads() {
        let old = r#"{"running":["a"],"ended":[]}"#;
        let work = WorkRecord::parse(Some(old));
        assert_eq!(work.len(), 1);
        assert_eq!(work.idle_proof(), None);
        assert_eq!(work.pending()[0].kind, "subagent");
    }
}
