//! Listener: the inbound health of one chat account, from the heartbeats
//! its listener program sends through `fleet gateway receive --beat` and
//! the envelopes that verb refused.
//!
//! Three status fields have no owner: `beats`, `refused` and
//! `beatRevision`. They change only through the typed mutations
//! [`Store::record_beat`] and [`Store::record_refusal`], which the verb
//! calls as `listener/<account>`; ListenerController owns the rest.
//!
//! [`Store::record_beat`]: crate::store::Store::record_beat
//! [`Store::record_refusal`]: crate::store::Store::record_refusal

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::condition::Condition;
use crate::meta::Millis;

pub const KIND: &str = "Listener";

/// The listener instances a Listener keeps a beat for; a new one past that
/// replaces the entry with the oldest `at`.
pub const INSTANCE_CAP: usize = 8;

/// A beat is stale after this many `gateway.beatMs`.
pub const STALE_BEATS: Millis = 3;

/// `listener-<account>`.
pub fn listener_name(account: &str) -> String {
    format!("listener-{account}")
}

/// What a Listener reports on. Fixed once created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListenerSpec {
    /// The `[gateway.accounts]` entry.
    pub account: String,
}

/// Where a listener process stands with the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BeatState {
    Connecting,
    Connected,
    Closing,
}

/// One heartbeat, as the listener writes it to the verb's stdin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Heartbeat {
    /// Names one listener process, so two on one account never overwrite
    /// each other.
    pub instance: String,
    pub state: BeatState,
    /// The account configuration the listener started from.
    pub digest: String,
    /// The listener's own clock.
    pub sent_at: Millis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_delivery_at: Option<Millis>,
}

/// One instance's last beat, stamped by the verb's clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Beat {
    pub state: BeatState,
    pub digest: String,
    /// When the verb recorded it, by its own clock.
    pub at: Millis,
    pub sent_at: Millis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_delivery_at: Option<Millis>,
}

/// Envelopes the verb refused or could not read, counted by reason.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Refused {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub counts: BTreeMap<String, u64>,
    /// The last refusal that was the listener's own fault: not an envelope,
    /// or the wrong `verified`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fault_at: Option<Millis>,
}

/// `{digest, at}`: the account's digest in the loaded config, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Accepted {
    pub digest: String,
    pub at: Millis,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListenerStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted: Option<Accepted>,
    /// Per instance, its last beat; `listener/<account>`'s.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub beats: BTreeMap<String, Beat>,
    /// `listener/<account>`'s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<Refused>,
    /// Incremented by every `record_beat` and `record_refusal`. Absent
    /// while zero, so the controller's first write, which may not touch
    /// it, matches a status that was never written.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub beat_revision: u64,
    /// The `beatRevision` last judged.
    #[serde(default)]
    pub seen: u64,
    /// When `whoami` and `capabilities` last answered Known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<Millis>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
}

impl ListenerStatus {
    /// Record one beat: the instance's entry replaced, the oldest evicted
    /// past the cap, the revision moved.
    pub fn record_beat(&mut self, instance: &str, beat: Beat) {
        self.beats.insert(instance.to_string(), beat);
        while self.beats.len() > INSTANCE_CAP {
            let oldest = self
                .beats
                .iter()
                .min_by_key(|(name, b)| (b.at, (*name).clone()))
                .map(|(name, _)| name.clone());
            match oldest {
                Some(name) => {
                    self.beats.remove(&name);
                }
                None => break,
            }
        }
        self.beat_revision += 1;
    }

    /// Count one refusal, and when it was the listener's own fault, when.
    pub fn record_refusal(&mut self, reason: &str, fault: bool, at: Millis) {
        let refused = self.refused.get_or_insert_with(Refused::default);
        *refused.counts.entry(reason.to_string()).or_default() += 1;
        if fault {
            refused.last_fault_at = Some(at);
        }
        self.beat_revision += 1;
    }

    /// The newest `lastDeliveryAt` any instance reported.
    pub fn last_delivery_at(&self) -> Option<Millis> {
        self.beats.values().filter_map(|b| b.last_delivery_at).max()
    }
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// The status fields nobody owns: reached only through typed mutations.
pub const UNOWNED_FIELDS: &[&str] = &["beats", "refused", "beatRevision"];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn beat(at: Millis) -> Beat {
        Beat {
            state: BeatState::Connected,
            digest: "sha256:x".into(),
            at,
            sent_at: at,
            last_delivery_at: None,
        }
    }

    #[test]
    fn a_heartbeat_is_the_interface_pages_shape() {
        let b: Heartbeat = serde_json::from_value(json!({
            "instance": "a1", "state": "connected", "digest": "sha256:abc",
            "sentAt": 1_730_000_419_990u64, "lastDeliveryAt": 1_730_000_300_100u64
        }))
        .unwrap();
        assert_eq!(b.state, BeatState::Connected);
        assert!(
            serde_json::from_value::<Heartbeat>(
                json!({"instance": "a1", "state": "asleep", "digest": "d", "sentAt": 1})
            )
            .is_err()
        );
        assert_eq!(listener_name("slack-main"), "listener-slack-main");
        assert_eq!(
            serde_json::to_value(ListenerStatus::default()).unwrap(),
            json!({"seen": 0})
        );
    }

    #[test]
    fn beats_keep_eight_instances_and_refusals_count_by_reason() {
        let mut st = ListenerStatus::default();
        for i in 0..10u64 {
            st.record_beat(&format!("i{i}"), beat(100 + i));
        }
        assert_eq!(st.beats.len(), INSTANCE_CAP);
        assert!(!st.beats.contains_key("i0") && !st.beats.contains_key("i1"));
        assert!(st.beats.contains_key("i9"));
        st.record_beat("i9", beat(50));
        assert_eq!(
            st.beats.len(),
            INSTANCE_CAP,
            "a replaced entry evicts nothing"
        );
        assert_eq!(st.beat_revision, 11);
        st.record_refusal("SpoolFull", false, 7);
        st.record_refusal("WrongVerified", true, 8);
        st.record_refusal("WrongVerified", true, 9);
        let r = st.refused.as_ref().unwrap();
        assert_eq!(r.counts["SpoolFull"], 1);
        assert_eq!(r.counts["WrongVerified"], 2);
        assert_eq!(r.last_fault_at, Some(9));
        assert_eq!(st.beat_revision, 14);
        assert_eq!(st.last_delivery_at(), None);
    }
}
