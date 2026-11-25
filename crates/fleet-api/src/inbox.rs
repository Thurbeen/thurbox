use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::meta::Millis;
use crate::plan::effect_key;

pub const MAX_INBOX: usize = 20;

pub const MAX_DELIVERIES: usize = 100;

pub const CLI_SOURCE_PREFIX: &str = "cli/";

pub const FORGE_SOURCE_PREFIX: &str = "forge/";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InboxEntry {
    pub key: String,
    pub from: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<InboxAbout>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amends: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<crate::subscription::EventNotice>,
    pub queued_at: Millis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InboxAbout {
    pub uid: String,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliveryReceipt {
    pub key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from: String,
    pub at: Millis,
    #[serde(default)]
    pub recovered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_via: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub may_hold: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Deliveries {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub receipts: Vec<DeliveryReceipt>,
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeliveryAttempt {
    #[serde(default)]
    pub sends: u32,
    #[serde(default)]
    pub probe_failures: u32,
    #[serde(default)]
    pub failed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_prompt_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_since: Option<Millis>,
}

pub fn due(inbox: &[InboxEntry], attempts: &BTreeMap<String, DeliveryAttempt>) -> usize {
    inbox
        .iter()
        .filter(|e| !attempts.get(&e.key).is_some_and(|a| a.failed))
        .count()
}

pub fn given_up<'a>(
    inbox: &'a [InboxEntry],
    attempts: &BTreeMap<String, DeliveryAttempt>,
    deliveries: &Deliveries,
    key: &str,
) -> Result<&'a InboxEntry, String> {
    let Some(entry) = inbox.iter().find(|e| e.key == key) else {
        if deliveries.delivered(key) {
            return Err(format!("inbox entry {key} was delivered"));
        }
        return Err(format!("no inbox entry {key}"));
    };
    match attempts.get(key) {
        Some(a) if a.failed => Ok(entry),
        Some(a) if a.sends > 0 => Err(format!(
            "inbox entry {key} is not given up on: it is being delivered, sends so far: {}",
            a.sends
        )),
        _ => Err(format!(
            "inbox entry {key} is not given up on: it is being delivered, not sent yet"
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retry {
    pub key: String,
    pub queued_at: Millis,
}

impl Retry {
    pub fn copy(self, entry: InboxEntry) -> InboxEntry {
        InboxEntry {
            key: self.key,
            queued_at: self.queued_at,
            ..entry
        }
    }
}

pub fn forgotten(
    inbox: &[InboxEntry],
    attempts: &BTreeMap<String, DeliveryAttempt>,
) -> Vec<String> {
    attempts
        .iter()
        .filter(|(key, a)| a.failed && !inbox.iter().any(|e| &e.key == *key))
        .map(|(key, _)| key.clone())
        .collect()
}

pub fn check_append(
    inbox: &[InboxEntry],
    attempts: &BTreeMap<String, DeliveryAttempt>,
    deliveries: &Deliveries,
    entry: &InboxEntry,
) -> Result<(), String> {
    check_entry(inbox, deliveries, entry)?;
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
    check_room(inbox, attempts)
}

pub fn check_retry(
    inbox: &[InboxEntry],
    attempts: &BTreeMap<String, DeliveryAttempt>,
    deliveries: &Deliveries,
    entry: &InboxEntry,
) -> Result<(), String> {
    check_entry(inbox, deliveries, entry)?;
    check_room(inbox, attempts)
}

fn check_entry(
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
    Ok(())
}

fn check_room(
    inbox: &[InboxEntry],
    attempts: &BTreeMap<String, DeliveryAttempt>,
) -> Result<(), String> {
    let due = due(inbox, attempts);
    if due > MAX_INBOX {
        return Err(format!(
            "inbox holds {due} entries still to deliver (more than {MAX_INBOX}); deliver some first"
        ));
    }
    Ok(())
}

pub fn send_key(name: &str, uid: &str, n: u64) -> String {
    let base = effect_key(crate::task::KIND, name, uid, "send", None);
    format!("{base}/{n}")
}

pub fn amend_key(name: &str, uid: &str, generation: u64) -> String {
    let base = effect_key(crate::task::KIND, name, uid, "amend", None);
    format!("{base}/{generation}")
}

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
            notice: None,
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
        assert!(check_append(&queued, &BTreeMap::new(), &d, &entry("k1", "cli/a", None)).is_err());
        assert!(check_append(&queued, &BTreeMap::new(), &d, &entry("k2", "cli/a", None)).is_ok());
        d.record(&queued[0], receipt("k1", 5));
        assert!(check_append(&[], &BTreeMap::new(), &d, &entry("k1", "cli/a", None)).is_err());
        assert_eq!(d.receipts[0].from, "cli/a", "the store copies the source");
    }

    #[test]
    fn the_inbox_is_bounded() {
        let full: Vec<InboxEntry> = (0..=MAX_INBOX)
            .map(|i| entry(&format!("k{i}"), "cli/a", None))
            .collect();
        let d = Deliveries::default();
        assert!(
            check_append(
                &full[..MAX_INBOX],
                &BTreeMap::new(),
                &d,
                &entry("x", "cli/a", None)
            )
            .is_ok()
        );
        assert!(check_append(&full, &BTreeMap::new(), &d, &entry("x", "cli/a", None)).is_err());
    }

    #[test]
    fn an_entry_given_up_on_counts_against_no_bound() {
        let full: Vec<InboxEntry> = (0..=MAX_INBOX)
            .map(|i| entry(&format!("k{i}"), "cli/a", None))
            .collect();
        let given_up: BTreeMap<String, DeliveryAttempt> = full
            .iter()
            .map(|e| {
                let attempt = DeliveryAttempt {
                    failed: true,
                    ..DeliveryAttempt::default()
                };
                (e.key.clone(), attempt)
            })
            .collect();
        let d = Deliveries::default();
        assert_eq!(due(&full, &given_up), 0);
        assert!(check_append(&full, &given_up, &d, &entry("x", "cli/a", None)).is_ok());
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
        assert!(
            check_append(
                &[],
                &BTreeMap::new(),
                &d,
                &entry("r7b", "forge/cr-1", Some(7))
            )
            .is_err()
        );
        assert!(
            check_append(
                &[],
                &BTreeMap::new(),
                &d,
                &entry("r8", "forge/cr-1", Some(8))
            )
            .is_ok()
        );
        assert!(
            check_append(
                &[],
                &BTreeMap::new(),
                &d,
                &entry("o7", "forge/cr-2", Some(7))
            )
            .is_ok()
        );
        let mut rebound = entry("r1b", "forge/cr-1", Some(1));
        rebound.about.as_mut().unwrap().uid = "c1-again".into();
        assert!(check_append(&[], &BTreeMap::new(), &d, &rebound).is_ok());
    }

    #[test]
    fn only_an_entry_given_up_on_is_resolved_and_its_attempt_is_then_forgotten() {
        let queued = vec![entry("k1", "cli/a", None), entry("k2", "cli/a", None)];
        let mut attempts = BTreeMap::new();
        let mut d = Deliveries::default();
        assert!(
            given_up(&queued, &attempts, &d, "k1")
                .unwrap_err()
                .contains("not sent yet")
        );
        attempts.insert(
            "k1".to_string(),
            DeliveryAttempt {
                sends: 1,
                ..DeliveryAttempt::default()
            },
        );
        assert!(
            given_up(&queued, &attempts, &d, "k1")
                .unwrap_err()
                .contains("sends so far: 1")
        );
        attempts.get_mut("k1").unwrap().failed = true;
        assert_eq!(given_up(&queued, &attempts, &d, "k1").unwrap().key, "k1");
        assert!(
            given_up(&queued, &attempts, &d, "k9")
                .unwrap_err()
                .contains("no inbox entry")
        );
        d.record(&entry("k0", "cli/a", None), receipt("k0", 1));
        assert!(
            given_up(&queued, &attempts, &d, "k0")
                .unwrap_err()
                .contains("was delivered")
        );

        assert!(forgotten(&queued, &attempts).is_empty());
        assert_eq!(forgotten(&queued[1..], &attempts), vec!["k1".to_string()]);
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
