//! Subscriptions: a Task asks to hear of the events on a Board that a
//! filter keeps (`Task.spec.subscriptions`). TaskController keeps one cursor
//! per subscription in `Task.status.subscriptions`. When a subscription has
//! events past its cursor that its filter keeps, or its cursor fell behind
//! the Board's oldest event, TaskController queues one `event-notice` digest
//! in the Task's inbox: the Board, the count and the seq range. The session
//! reads the events itself, with `fleet event list --after`.
//!
//! While a subscription's notice is still to deliver, the subscription waits:
//! its cursor stays, and the next notice covers every event since. So a
//! subscription has at most one notice still to deliver.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::event::{Event, Filter};
use crate::inbox::{DeliveryAttempt, InboxEntry};
use crate::meta::Millis;
use crate::plan::effect_key;

/// The most subscriptions one Task holds.
pub const MAX_SUBSCRIPTIONS: usize = 16;

/// The source prefix of a notice: `board/<board>`.
pub const BOARD_SOURCE_PREFIX: &str = "board/";

/// The most events one pass reads for one subscription. A notice covers at
/// most these; the next pass goes on from where this one stopped.
pub const MAX_SCAN: usize = 10_000;

/// The most events `fleet event list` prints when no `--limit` is given.
const LIST_DEFAULT_LIMIT: u64 = 100;

/// One subscription of a Task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Subscription {
    pub board: String,
    /// What `fleet event list` would keep; empty keeps every event.
    #[serde(default, skip_serializing_if = "Filter::is_empty")]
    pub filter: Filter,
    /// The seq of the last event the Task knows of already. Absent: the
    /// Board's head when TaskController first reads the subscription.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<u64>,
}

impl Subscription {
    /// Whether `cursor` belongs to this subscription: the same Board and
    /// filter. `after` only places a new cursor.
    pub fn owns(&self, cursor: &SubscriptionCursor) -> bool {
        self.board == cursor.board && self.filter == cursor.filter
    }

    fn same(&self, other: &Subscription) -> bool {
        self.board == other.board && self.filter == other.filter
    }
}

/// Check a Task's subscriptions: at most [`MAX_SUBSCRIPTIONS`], each naming
/// a valid Board, no two with the same Board and filter.
pub fn validate(subscriptions: &[Subscription]) -> Result<(), String> {
    if subscriptions.len() > MAX_SUBSCRIPTIONS {
        return Err(format!(
            "{} subscriptions; a Task holds at most {MAX_SUBSCRIPTIONS}",
            subscriptions.len()
        ));
    }
    for (i, s) in subscriptions.iter().enumerate() {
        crate::validate::validate_name(&s.board)
            .map_err(|e| format!("subscriptions[{i}].board: {e}"))?;
        if subscriptions[..i].iter().any(|earlier| earlier.same(s)) {
            return Err(format!(
                "subscriptions[{i}]: the Task already subscribes to Board {} with this filter",
                s.board
            ));
        }
    }
    Ok(())
}

/// `Task.status.subscriptions`: TaskController's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscriptionsStatus {
    /// Notices queued over the Task's life; the next one's key.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub notices: u64,
    /// One per subscription in the spec, in its order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cursors: Vec<SubscriptionCursor>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl SubscriptionsStatus {
    pub fn is_empty(&self) -> bool {
        *self == SubscriptionsStatus::default()
    }
}

/// Where one subscription has read to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscriptionCursor {
    pub board: String,
    #[serde(default, skip_serializing_if = "Filter::is_empty")]
    pub filter: Filter,
    /// The Board's uid when the cursor was placed; absent while the Board
    /// was not there. A Board made again is read from its first event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_uid: Option<String>,
    /// The seq of the last event read, kept or not.
    pub after: u64,
    /// The key of the last notice queued for this subscription.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

impl SubscriptionCursor {
    /// The cursor `sub` reads from, given the cursor `prev` it had and the
    /// Board's uid now (`None`: no Board). A cursor of the same Board goes
    /// on. A new cursor starts at `sub.after`, else at `head()`. A Board
    /// made, deleted or made again since is read from its first event. Asks
    /// `head` only for a new cursor on a Board that is there.
    pub fn place<E>(
        sub: &Subscription,
        prev: Option<&SubscriptionCursor>,
        board_uid: Option<&str>,
        head: impl FnOnce() -> Result<u64, E>,
    ) -> Result<SubscriptionCursor, E> {
        let fresh = |after: u64, notice: Option<String>| SubscriptionCursor {
            board: sub.board.clone(),
            filter: sub.filter.clone(),
            board_uid: board_uid.map(str::to_string),
            after,
            notice,
        };
        Ok(match (prev, board_uid) {
            (Some(prev), uid) if prev.board_uid.as_deref() == uid => prev.clone(),
            (Some(prev), _) => fresh(0, prev.notice.clone()),
            (None, None) => fresh(0, None),
            (None, Some(_)) => match sub.after {
                Some(after) => fresh(after, None),
                None => fresh(head()?, None),
            },
        })
    }

    /// Whether this subscription's last notice is still to deliver: in the
    /// inbox and not given up on.
    pub fn waiting(
        &self,
        inbox: &[InboxEntry],
        attempts: &BTreeMap<String, DeliveryAttempt>,
    ) -> bool {
        self.notice.as_ref().is_some_and(|key| {
            inbox.iter().any(|e| e.key == *key) && !attempts.get(key).is_some_and(|a| a.failed)
        })
    }
}

/// What a notice tells: the Board, how many events past the cursor the
/// filter keeps, the first and last of their seqs, and the oldest seq the
/// Board held when the cursor was behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventNotice {
    pub board: String,
    pub count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<u64>,
    /// The events before this seq expired before the Task read them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expired_before: Option<u64>,
}

/// What one pass read for a subscription, from its cursor on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    pub read: usize,
    count: u64,
    first: Option<u64>,
    last: Option<u64>,
    expired_before: Option<u64>,
}

impl Scan {
    /// Count the events of one page that `filter` keeps, and move `after`
    /// past each event of it.
    pub fn page(&mut self, after: &mut u64, filter: &Filter, events: &[Event]) {
        for e in events {
            *after = e.seq;
            self.read += 1;
            if filter.matches(e) {
                self.count += 1;
                self.first.get_or_insert(e.seq);
                self.last = Some(e.seq);
            }
        }
    }

    /// The Board no longer holds the events before `oldest`: `after` goes
    /// on from just before it, and the notice says so.
    pub fn expired(&mut self, after: &mut u64, oldest: u64) {
        *after = oldest.saturating_sub(1);
        self.expired_before = Some(oldest);
    }

    /// The notice this read owes, if any.
    pub fn notice(&self, board: &str) -> Option<EventNotice> {
        if self.count == 0 && self.expired_before.is_none() {
            return None;
        }
        Some(EventNotice {
            board: board.to_string(),
            count: self.count,
            first: self.first,
            last: self.last,
            expired_before: self.expired_before,
        })
    }
}

/// The key of the `n`th notice queued for a Task:
/// `task/<name>/<uid>/notice/<n>`.
pub fn notice_key(name: &str, uid: &str, n: u64) -> String {
    let base = effect_key(crate::task::KIND, name, uid, "notice", None);
    format!("{base}/{n}")
}

/// The inbox entry that delivers `notice` for a subscription with `filter`.
pub fn notice_entry(key: String, notice: EventNotice, filter: &Filter, now: Millis) -> InboxEntry {
    InboxEntry {
        key,
        from: format!("{BOARD_SOURCE_PREFIX}{}", notice.board),
        text: notice_text(&notice, filter),
        about: None,
        answers: Vec::new(),
        amends: None,
        notice: Some(notice),
        queued_at: now,
    }
}

fn notice_text(n: &EventNotice, filter: &Filter) -> String {
    let board = &n.board;
    let mut parts = Vec::new();
    if let Some(oldest) = n.expired_before {
        parts.push(format!(
            "Events on Board {board} up to seq {} expired before this Task read them; some may have matched your subscription.",
            oldest.saturating_sub(1)
        ));
    }
    match (n.first, n.last) {
        (Some(first), Some(last)) => {
            let events = if n.count == 1 { "event" } else { "events" };
            let limit = if n.count > LIST_DEFAULT_LIMIT {
                format!(" --limit {}", n.count)
            } else {
                String::new()
            };
            parts.push(format!(
                "{} new {events} on Board {board} match your subscription, seq {first} to {last}. Read them with: fleet event list --board {board} --after {}{}{limit}",
                n.count,
                first - 1,
                filter.cli_args()
            ));
        }
        _ => parts.push(format!(
            "No event still on Board {board} matches your subscription."
        )),
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{CliSource, Pattern, Source};
    use serde_json::json;

    fn sub(board: &str, event_type: Option<&str>) -> Subscription {
        Subscription {
            board: board.into(),
            filter: Filter {
                event_type: event_type.map(Pattern::new),
                ..Filter::default()
            },
            after: None,
        }
    }

    fn event(seq: u64, t: &str) -> Event {
        Event {
            board: "shared".into(),
            seq,
            time: 1,
            source: Source::Cli(CliSource::new("a").unwrap()),
            event_type: t.into(),
            subject: None,
            key: None,
            correlation: None,
            data: json!({}),
        }
    }

    #[test]
    fn a_subscription_is_written_as_its_board_and_filter() {
        let s = sub("shared", Some("git.*"));
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            json!({"board": "shared", "filter": {"type": "git.*"}})
        );
        assert!(
            serde_json::from_value::<Subscription>(
                json!({"board": "shared", "filter": {"kind": "x"}})
            )
            .is_err()
        );
    }

    #[test]
    fn subscriptions_are_bounded_named_and_distinct() {
        assert!(validate(&[sub("shared", None), sub("shared", Some("a"))]).is_ok());
        assert!(validate(&[sub("shared", None), sub("shared", None)]).is_err());
        assert!(validate(&[sub("Not a name", None)]).is_err());
        let many: Vec<_> = (0..=MAX_SUBSCRIPTIONS)
            .map(|i| sub("shared", Some(&format!("t{i}"))))
            .collect();
        assert!(validate(&many).is_err());
    }

    #[test]
    fn a_cursor_goes_on_starts_at_the_head_or_restarts_on_a_board_made_again() {
        let s = sub("shared", None);
        let none = || -> Result<u64, ()> { panic!("no head is needed") };
        let new = SubscriptionCursor::place(&s, None, Some("u1"), || Ok::<_, ()>(7)).unwrap();
        assert_eq!((new.after, new.board_uid.as_deref()), (7, Some("u1")));
        let mut moved = new.clone();
        moved.after = 9;
        moved.notice = Some("k".into());
        assert_eq!(
            SubscriptionCursor::place(&s, Some(&moved), Some("u1"), none).unwrap(),
            moved
        );
        let remade = SubscriptionCursor::place(&s, Some(&moved), Some("u2"), none).unwrap();
        assert_eq!(remade.after, 0);
        assert_eq!(remade.notice.as_deref(), Some("k"));
        let absent = SubscriptionCursor::place(&s, None, None, none).unwrap();
        assert_eq!((absent.after, absent.board_uid), (0, None));
        let mut named = s.clone();
        named.after = Some(3);
        assert_eq!(
            SubscriptionCursor::place(&named, None, Some("u1"), none)
                .unwrap()
                .after,
            3
        );
    }

    #[test]
    fn a_scan_counts_the_matches_and_moves_past_every_event() {
        let s = sub("shared", Some("git.*"));
        let mut after = 0;
        let mut scan = Scan::default();
        scan.page(
            &mut after,
            &s.filter,
            &[event(1, "ci"), event(2, "git.push"), event(3, "git.tag")],
        );
        scan.page(&mut after, &s.filter, &[event(4, "ci")]);
        assert_eq!(after, 4);
        assert_eq!(
            scan.notice("shared"),
            Some(EventNotice {
                board: "shared".into(),
                count: 2,
                first: Some(2),
                last: Some(3),
                expired_before: None,
            })
        );
        let mut quiet = Scan::default();
        quiet.page(&mut after, &s.filter, &[event(5, "ci")]);
        assert_eq!(quiet.notice("shared"), None);
    }

    #[test]
    fn an_expired_scan_owes_a_notice_even_with_no_match() {
        let mut after = 1;
        let mut scan = Scan::default();
        scan.expired(&mut after, 5);
        assert_eq!(after, 4);
        let notice = scan.notice("tiny").unwrap();
        assert_eq!(notice.expired_before, Some(5));
        let entry = notice_entry("k".into(), notice, &Filter::default(), 1);
        assert!(entry.text.contains("up to seq 4 expired"), "{}", entry.text);
        assert!(entry.text.contains("No event still"), "{}", entry.text);
        assert_eq!(entry.from, "board/tiny");
    }

    #[test]
    fn the_notice_names_the_list_command_with_its_filter_quoted() {
        let filter = Filter {
            event_type: Some(Pattern::new("git.*")),
            subject: Some(Pattern::new("it's")),
            source: Some("task/a".into()),
            correlation: None,
        };
        let notice = EventNotice {
            board: "shared".into(),
            count: 150,
            first: Some(10),
            last: Some(200),
            expired_before: None,
        };
        let entry = notice_entry("k".into(), notice, &filter, 1);
        assert!(
            entry.text.ends_with(
                r"fleet event list --board shared --after 9 --type='git.*' --subject='it'\''s' --source=task/a --limit 150"
            ),
            "{}",
            entry.text
        );
    }

    #[test]
    fn a_notice_key_names_the_task_and_its_turn() {
        assert_eq!(notice_key("t", "u7", 3), "task/t/u7/notice/3");
    }
}
