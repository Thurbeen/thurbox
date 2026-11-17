//! The subscriptions step of a sweep: TaskController reads each Board a
//! Task subscribes to, after the subscription's cursor, and queues one
//! `event-notice` per subscription with news. The cursors and the notices
//! land in one write under the lease (`Store::record_relay`), so a stop
//! between them cannot happen. A Board that cannot be read moves nothing:
//! its cursor stays, and no notice is queued for it.

use std::collections::BTreeMap;

use fleet_api::board;
use fleet_api::event::{Filter, Page};
use fleet_api::inbox::{self, DeliveryAttempt, InboxEntry, MAX_INBOX};
use fleet_api::subscription::{
    self, MAX_SCAN, Scan, Subscription, SubscriptionCursor, SubscriptionsStatus,
};
use fleet_api::{LeaseToken, Millis, Object, Observation, StoreError};
use fleet_controllers::TaskController;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{Manager, RunReport};

const READ_PAGE: usize = 1_000;
const MAX_PAGES: usize = MAX_SCAN / READ_PAGE + 2;

fn field<T: DeserializeOwned + Default>(
    obj: &Object,
    value: Option<&Value>,
) -> Result<T, StoreError> {
    match value {
        None | Some(Value::Null) => Ok(T::default()),
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| StoreError::Invalid(format!("{}: {e}", obj.key()))),
    }
}

impl Manager<'_> {
    /// Read the Boards `task` subscribes to and queue the notices they owe.
    /// Nothing is read for a Task being deleted, Concluded or Releasing: its
    /// cursors wait. Answers the Task as written, when anything moved.
    pub(crate) fn notify_subscriptions(
        &self,
        task: &Object,
        lease: &mut LeaseToken,
        ttl: Millis,
        report: &mut RunReport,
    ) -> Result<Option<Object>, StoreError> {
        let subs: Vec<Subscription> = field(task, task.spec.get("subscriptions"))?;
        let before: SubscriptionsStatus = field(task, task.status.get("subscriptions"))?;
        if subs.is_empty() && before.is_empty() {
            return Ok(None);
        }
        let phase = task.status.get("phase").and_then(Value::as_str);
        if task.is_deleting() || matches!(phase, Some("Concluded" | "Releasing")) {
            return Ok(None);
        }
        let queued: Vec<InboxEntry> = field(task, task.status.get("inbox"))?;
        let attempts: BTreeMap<String, DeliveryAttempt> =
            field(task, task.status.get("deliveryAttempts"))?;
        let mut room = (MAX_INBOX + 1).saturating_sub(inbox::due(&queued, &attempts));
        let mut next = SubscriptionsStatus {
            notices: before.notices,
            cursors: Vec::new(),
        };
        let mut appends = Vec::new();
        let now = self.clock.now();
        for sub in &subs {
            let prev = before.cursors.iter().find(|c| sub.owns(c));
            let unknown = |report: &mut RunReport, why: &dyn std::fmt::Display| {
                report.unknowns.push(format!(
                    "{} subscription to Board/{}: {why}",
                    task.key(),
                    sub.board
                ));
            };
            let (board_uid, recorded_head) = match self.store.get(board::KIND, &sub.board) {
                Observation::Known(board) => (
                    board.as_ref().map(|b| b.metadata.uid.clone()),
                    board.and_then(|b| b.status.get("head").and_then(Value::as_u64)),
                ),
                Observation::Unknown { why } => {
                    unknown(report, &why);
                    next.cursors.extend(prev.cloned());
                    continue;
                }
            };
            let placed = SubscriptionCursor::place(sub, prev, board_uid.as_deref(), || {
                self.store.event_head(&sub.board)
            });
            let mut cursor = match placed {
                Ok(cursor) => cursor,
                Err(e) => {
                    unknown(report, &e);
                    next.cursors.extend(prev.cloned());
                    continue;
                }
            };
            let nothing_new = recorded_head.unwrap_or(0) <= cursor.after;
            if cursor.board_uid.is_none()
                || nothing_new
                || room == 0
                || cursor.waiting(&queued, &attempts)
            {
                next.cursors.push(cursor);
                continue;
            }
            let mut after = cursor.after;
            let scan = match self.scan(&sub.board, &sub.filter, &mut after, lease, ttl) {
                Ok(scan) => scan,
                Err(e @ StoreError::LeaseLost(_)) => return Err(e),
                Err(e) => {
                    unknown(report, &e);
                    next.cursors.push(cursor);
                    continue;
                }
            };
            cursor.after = after;
            if let Some(notice) = scan.notice(&sub.board) {
                next.notices += 1;
                let key =
                    subscription::notice_key(&task.metadata.name, &task.metadata.uid, next.notices);
                appends.push(subscription::notice_entry(
                    key.clone(),
                    notice,
                    &sub.filter,
                    now,
                ));
                cursor.notice = Some(key);
                room -= 1;
            }
            next.cursors.push(cursor);
        }
        if next == before && appends.is_empty() {
            return Ok(None);
        }
        let status = serde_json::to_value(&next).map_err(|e| StoreError::Invalid(e.to_string()))?;
        *lease = self.store.renew_lease(lease, ttl)?;
        match self.store.record_relay(
            &task.metadata.name,
            status,
            appends,
            task.metadata.resource_version,
            TaskController::MANAGER,
            lease,
        ) {
            Ok(written) => {
                report.status_writes += 1;
                Ok(Some(written))
            }
            Err(StoreError::Conflict { .. }) => {
                report.conflicts += 1;
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Read `board` after `after`, up to its head or [`MAX_SCAN`] events,
    /// moving `after` past each event read and renewing the lease before
    /// each page. An error leaves the caller's cursor to discard.
    fn scan(
        &self,
        board: &str,
        filter: &Filter,
        after: &mut u64,
        lease: &mut LeaseToken,
        ttl: Millis,
    ) -> Result<Scan, StoreError> {
        let mut scan = Scan::default();
        for _ in 0..MAX_PAGES {
            *lease = self.store.renew_lease(lease, ttl)?;
            match self.store.read_events(board, *after, READ_PAGE)? {
                Page::Expired { oldest } => scan.expired(after, oldest),
                Page::Events(events) => {
                    let caught_up = events.len() < READ_PAGE;
                    scan.page(after, filter, &events);
                    if caught_up || scan.read >= MAX_SCAN {
                        break;
                    }
                }
            }
        }
        Ok(scan)
    }
}
