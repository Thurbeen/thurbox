//! The Task inbox: messages for a running session, queued by `fleet send`
//! and by the Forge controller, and delivered by TaskController
//! the way it delivers the brief, oldest first, one per pass, under the
//! entry's key.
//!
//! `status.inbox` has no owner. It is reached only through two typed store
//! mutations, each named by its caller: append ([`Store::append_inbox`])
//! and record-and-remove ([`Store::record_delivery`]), which moves an
//! entry's receipt into `status.deliveries` and the entry out of `inbox` in
//! one write. The store refuses a key already present in either, and an
//! append while more than [`MAX_INBOX`] entries are undelivered.
//!
//! [`Store::append_inbox`]: crate::store::Store::append_inbox
//! [`Store::record_delivery`]: crate::store::Store::record_delivery

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::meta::Millis;
use crate::plan::effect_key;

/// An append is refused while more than this many entries are undelivered.
pub const MAX_INBOX: usize = 20;

/// How many delivery receipts `status.deliveries` retains, newest first to
/// go. A source's highest delivered revision outlives its receipts.
pub const MAX_DELIVERIES: usize = 100;

/// The source prefix of an entry `fleet send` appends: `cli/<user>`.
pub const CLI_SOURCE_PREFIX: &str = "cli/";

/// The source prefix of an entry the Forge controller appends:
/// `forge/<change>`, the ChangeRequest's name.
pub const FORGE_SOURCE_PREFIX: &str = "forge/";

/// One message for the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InboxEntry {
    /// Deterministic, unique across `inbox` and `deliveries` for the life
    /// of the Task; also the runtime intent key the delivery is journaled
    /// under. `task/<name>/<uid>/send/<n>` for `fleet send`.
    pub key: String,
    /// Who queued it: `cli/<user>`, or `forge/<change>` for a relay.
    pub from: String,
    /// Delivered as typed.
    pub text: String,
    /// For a relay: what the message was computed from. Absent for `fleet send`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<InboxAbout>,
    /// The declared Questions this entry answers, by id. The store records
    /// each in `status.answered` in the same write as the append, first
    /// answer wins, and refuses an id that names no declared Question.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<String>,
    /// For an amendment: the spec generation it created. Only the store's
    /// amend mutation queues such an entry; TaskController delivers it, or
    /// records it without a send once the worker has it already.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amends: Option<u64>,
    /// Unix milliseconds.
    pub queued_at: Millis,
}

/// What a relayed entry was computed from: the mirror's identity, its
/// revision and its head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InboxAbout {
    /// The ChangeRequest's `uid`.
    pub uid: String,
    /// The ChangeRequest revision the message covers. Per mirror (`uid`),
    /// the highest delivered one is kept in [`Deliveries::revisions`], so a
    /// relay cannot be appended twice even after its receipt rotated out.
    pub revision: u64,
    /// The `head.id` the message was computed from, when the change had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
}

/// The receipt of one delivered entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliveryReceipt {
    /// The entry's key.
    pub key: String,
    /// The entry's source, copied from it by the store.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from: String,
    /// When the receipt was recorded, Unix milliseconds.
    pub at: Millis,
    /// True when the receipt was recovered from the session's journal
    /// rather than returned by the send.
    #[serde(default)]
    pub recovered: bool,
    /// The runtime's route to the agent, when the send named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_via: Option<String>,
    /// True when that route can hold the text behind an approval dialog.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub may_hold: bool,
}

/// `status.deliveries`: TaskController's record of what the session was
/// given.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Deliveries {
    /// The newest [`MAX_DELIVERIES`] receipts, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receipts: Vec<DeliveryReceipt>,
    /// Per mirror (`about.uid`), the highest `about.revision` delivered. A
    /// mirror recreated after retention has a new `uid` and starts afresh.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub revisions: BTreeMap<String, u64>,
}

impl Deliveries {
    pub fn is_empty(&self) -> bool {
        self.receipts.is_empty() && self.revisions.is_empty()
    }

    pub fn delivered(&self, key: &str) -> bool {
        self.receipts.iter().any(|r| r.key == key)
    }

    /// Record one delivery: the receipt in, the oldest out past the cap,
    /// and the mirror's revision raised when the entry carried one.
    pub fn record(&mut self, entry: &InboxEntry, receipt: DeliveryReceipt) {
        if let Some(about) = &entry.about {
            let seen = self.revisions.entry(about.uid.clone()).or_default();
            *seen = (*seen).max(about.revision);
        }
        self.receipts.push(DeliveryReceipt {
            from: entry.from.clone(),
            ..receipt
        });
        if self.receipts.len() > MAX_DELIVERIES {
            let excess = self.receipts.len() - MAX_DELIVERIES;
            self.receipts.drain(..excess);
        }
    }
}

/// TaskController's bookkeeping for an entry it is delivering, keyed by
/// the entry's key; removed with the entry once its receipt is recorded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliveryAttempt {
    /// Sends of this entry, bounded like the brief's.
    #[serde(default)]
    pub sends: u32,
    /// Consecutive unreadable delivery probes.
    #[serde(default)]
    pub probe_failures: u32,
    /// Set once another send would exceed the bound: the entry stays, and
    /// later entries are delivered past it.
    #[serde(default)]
    pub failed: bool,
}

/// Why an append is refused, as the store reports it.
pub fn check_append(
    inbox: &[InboxEntry],
    deliveries: &Deliveries,
    entry: &InboxEntry,
) -> Result<(), String> {
    if entry.key.is_empty() || entry.from.is_empty() {
        return Err("an inbox entry needs a key and a source".into());
    }
    if entry.text.is_empty() {
        return Err(format!("inbox entry {}: nothing to deliver", entry.key));
    }
    if inbox.iter().any(|e| e.key == entry.key) || deliveries.delivered(&entry.key) {
        return Err(format!("inbox entry {} already exists", entry.key));
    }
    if let Some(about) = &entry.about
        && deliveries
            .revisions
            .get(&about.uid)
            .is_some_and(|seen| *seen >= about.revision)
    {
        return Err(format!(
            "inbox entry {}: revision {} of {} (mirror {}) was already delivered",
            entry.key, about.revision, entry.from, about.uid
        ));
    }
    if inbox.len() > MAX_INBOX {
        return Err(format!(
            "inbox holds {} undelivered entries (more than {MAX_INBOX}); deliver some first",
            inbox.len()
        ));
    }
    Ok(())
}

/// The key of the `n`th `fleet send` to a Task:
/// `task/<name>/<uid>/send/<n>`.
pub fn send_key(name: &str, uid: &str, n: u64) -> String {
    let base = effect_key(crate::task::KIND, name, uid, "send", None);
    format!("{base}/{n}")
}

/// The key of the amendment that created generation `generation`:
/// `task/<name>/<uid>/amend/<generation>`.
pub fn amend_key(name: &str, uid: &str, generation: u64) -> String {
    let base = effect_key(crate::task::KIND, name, uid, "amend", None);
    format!("{base}/{generation}")
}

/// The key of a join or relay: `task/<name>/<uid>/relay/<change uid>/<revision>`.
/// The mirror's `uid` keeps a recreated mirror's keys apart from its
/// predecessor's; the revision is the one the message covers.
pub fn relay_key(name: &str, uid: &str, change_uid: &str, revision: u64) -> String {
    let base = effect_key(crate::task::KIND, name, uid, "relay", None);
    format!("{base}/{change_uid}/{revision}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, from: &str, revision: Option<u64>) -> InboxEntry {
        InboxEntry {
            key: key.into(),
            from: from.into(),
            text: "hi".into(),
            about: revision.map(|revision| InboxAbout {
                uid: format!("uid-of-{from}"),
                revision,
                head: None,
            }),
            answers: Vec::new(),
            amends: None,
            queued_at: 1,
        }
    }

    fn receipt(key: &str, at: Millis) -> DeliveryReceipt {
        DeliveryReceipt {
            key: key.into(),
            from: String::new(),
            at,
            recovered: false,
            delivered_via: None,
            may_hold: false,
        }
    }

    #[test]
    fn a_key_is_refused_while_it_is_queued_or_receipted() {
        let queued = vec![entry("k1", "cli/a", None)];
        let mut d = Deliveries::default();
        assert!(check_append(&queued, &d, &entry("k1", "cli/a", None)).is_err());
        assert!(check_append(&queued, &d, &entry("k2", "cli/a", None)).is_ok());
        d.record(&queued[0], receipt("k1", 5));
        assert!(check_append(&[], &d, &entry("k1", "cli/a", None)).is_err());
        assert_eq!(d.receipts[0].from, "cli/a", "the store copies the source");
    }

    #[test]
    fn the_inbox_is_bounded() {
        let full: Vec<InboxEntry> = (0..=MAX_INBOX)
            .map(|i| entry(&format!("k{i}"), "cli/a", None))
            .collect();
        let d = Deliveries::default();
        assert!(check_append(&full[..MAX_INBOX], &d, &entry("x", "cli/a", None)).is_ok());
        assert!(check_append(&full, &d, &entry("x", "cli/a", None)).is_err());
    }

    #[test]
    fn a_mirrors_revision_is_kept_past_its_receipt() {
        let mut d = Deliveries::default();
        let relay = entry("r7", "forge/cr-1", Some(7));
        d.record(&relay, receipt("r7", 1));
        for i in 0..MAX_DELIVERIES {
            d.record(
                &entry(&format!("s{i}"), "cli/a", None),
                receipt(&format!("s{i}"), 2),
            );
        }
        assert_eq!(d.receipts.len(), MAX_DELIVERIES);
        assert!(!d.delivered("r7"), "the oldest receipt rotated out");
        assert!(check_append(&[], &d, &entry("r7b", "forge/cr-1", Some(7))).is_err());
        assert!(check_append(&[], &d, &entry("r8", "forge/cr-1", Some(8))).is_ok());
        assert!(check_append(&[], &d, &entry("o7", "forge/cr-2", Some(7))).is_ok());
        // The same name with a new mirror uid, after retention, starts afresh.
        let mut rebound = entry("r1b", "forge/cr-1", Some(1));
        rebound.about.as_mut().unwrap().uid = "c1-again".into();
        assert!(check_append(&[], &d, &rebound).is_ok());
    }

    #[test]
    fn a_relay_key_names_the_mirror_and_the_revision() {
        assert_eq!(relay_key("t", "u7", "c3", 9), "task/t/u7/relay/c3/9");
    }

    #[test]
    fn an_amend_key_names_the_generation() {
        assert_eq!(amend_key("t", "u7", 4), "task/t/u7/amend/4");
    }

    #[test]
    fn a_send_key_names_the_task_and_its_turn() {
        assert_eq!(send_key("t", "u7", 42), "task/t/u7/send/42");
    }
}
