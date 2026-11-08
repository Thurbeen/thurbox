//! ListenerController: judges one account's listener program from the
//! heartbeats and refusals the receive verb recorded on its Listener, and
//! the account's identity from the platform. Fleet's calls are outbound, so
//! reaching the platform proves nothing about inbound delivery: the beats
//! do, against the configuration Fleet runs.
//!
//! Pure: it reads the Listener and what the probes saw, and returns a Plan
//! whose `status` is the three conditions and the bookkeeping, and whose
//! `delete` retires a Listener whose account left the config. It performs
//! no effect.

use fleet_api::condition::{ConditionStatus, set_condition, settle_transitions};
use fleet_api::gateway::{Capabilities, GatewayDefaults, Identity};
use fleet_api::listener::{self, Accepted, BeatState, ListenerSpec, ListenerStatus, STALE_BEATS};
use fleet_api::meta::{Millis, Object};
use fleet_api::plan::{Plan, Probe};
use fleet_api::store::FieldManager;
use fleet_api::validate::typed;
use fleet_api::{Observation, StoreError};

/// What one reconcile of one Listener reads.
#[derive(Debug, Clone)]
pub struct ListenerInput<'a> {
    pub object: &'a Object,
    pub observations: ListenerObservations,
    pub now: Millis,
    /// The `[gateway]` timings, and the account the spec names while the
    /// config has it, with its digest.
    pub defaults: GatewayDefaults,
}

/// The answers to the probes; `None` means "not asked".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListenerObservations {
    pub capabilities: Option<Observation<Capabilities>>,
    pub whoami: Option<Observation<Identity>>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ListenerController;

/// Condition types ListenerController writes.
pub mod conditions {
    /// The account is valid and some route uses it.
    pub const ACCEPTED: &str = "Accepted";
    /// The platform's `whoami` is the configured bot, and `capabilities`
    /// answered.
    pub const RESOLVED: &str = "Resolved";
    /// Some instance beat `connected`, on the current digest, within three
    /// `gateway.beatMs`, and no envelope was refused for the listener's
    /// own fault in that time.
    pub const INBOUND_READY: &str = "InboundReady";
}
use conditions::*;

impl ListenerController {
    /// The kind this controller reconciles and the field manager it writes as.
    pub const KIND: &'static str = listener::KIND;
    pub const MANAGER: FieldManager = FieldManager::ListenerController;

    /// Whether the manager should run this controller on the object now:
    /// a beat or refusal was recorded since the last pass, or `notBefore`
    /// passed. A deleting Listener is never due.
    pub fn due(object: &Object, now: Millis) -> bool {
        if object.is_deleting() {
            return false;
        }
        let Ok((_, st)) = decode(object) else {
            return true;
        };
        st.beat_revision > st.seen || st.not_before.is_none_or(|t| t <= now)
    }

    /// What the next `reconcile` needs observed: the account's capabilities
    /// and Fleet's identity there, every `gateway.pollMs`.
    pub fn probes(&self, input: &ListenerInput<'_>) -> Result<Vec<Probe>, StoreError> {
        if input.object.is_deleting() || input.defaults.account.is_none() {
            return Ok(Vec::new());
        }
        let (spec, st) = decode(input.object)?;
        let poll_due = st
            .observed_at
            .is_none_or(|at| input.now.saturating_sub(at) >= input.defaults.poll_ms);
        if !poll_due {
            return Ok(Vec::new());
        }
        let mut probes = Vec::new();
        if input.observations.capabilities.is_none() {
            probes.push(Probe::Capabilities {
                account: spec.account.clone(),
            });
        }
        if input.observations.whoami.is_none() {
            probes.push(Probe::GatewayWhoami {
                account: spec.account,
            });
        }
        Ok(probes)
    }

    pub fn reconcile(&self, input: &ListenerInput<'_>) -> Result<Plan, StoreError> {
        if input.object.is_deleting() {
            return Ok(Plan::default());
        }
        let (_, before) = decode(input.object)?;
        let Some(account) = &input.defaults.account else {
            // The account left the config: the manager creates the Listener
            // again if it returns.
            return Ok(Plan {
                delete: true,
                ..Plan::default()
            });
        };
        let mut st = before.clone();
        let now = input.now;
        let generation = input.object.metadata.generation;
        let cond = |st: &mut ListenerStatus, t: &str, s, reason: &str, message: String| {
            set_condition(&mut st.conditions, t, s, reason, message, generation, now);
        };

        // Accepted: the digest the config loaded, and whether a route uses it.
        let accepted_at = match &st.accepted {
            Some(a) if a.digest == account.digest => a.at,
            _ => now,
        };
        st.accepted = Some(Accepted {
            digest: account.digest.clone(),
            at: accepted_at,
        });
        if account.routed {
            cond(
                &mut st,
                ACCEPTED,
                ConditionStatus::True,
                "Loaded",
                String::new(),
            );
        } else {
            cond(
                &mut st,
                ACCEPTED,
                ConditionStatus::False,
                "NoRoute",
                "no route under [gateway.routes] uses this account".into(),
            );
        }

        // Resolved: the platform's identity against the configured bot.
        match (&input.observations.whoami, &input.observations.capabilities) {
            (Some(Observation::Known(identity)), Some(Observation::Known(_))) => {
                st.observed_at = Some(now);
                if identity.id == account.config.bot {
                    cond(
                        &mut st,
                        RESOLVED,
                        ConditionStatus::True,
                        "IdentityMatches",
                        String::new(),
                    );
                } else {
                    cond(
                        &mut st,
                        RESOLVED,
                        ConditionStatus::False,
                        "IdentityMismatch",
                        format!(
                            "the platform reports bot {}, the config names {}",
                            identity.id, account.config.bot
                        ),
                    );
                }
            }
            (Some(whoami), Some(caps)) => {
                let why = whoami
                    .why()
                    .or_else(|| caps.why())
                    .unwrap_or_default()
                    .to_string();
                cond(
                    &mut st,
                    RESOLVED,
                    ConditionStatus::Unknown,
                    "Unreachable",
                    why,
                );
            }
            // Not asked this pass: what was recorded stands.
            _ => {}
        }

        // InboundReady: the beats against the window and the digest.
        let window = input.defaults.beat_ms.saturating_mul(STALE_BEATS);
        let fresh = |at: Millis| now.saturating_sub(at) <= window;
        let current = |digest: &str| digest == account.digest;
        let beats = &st.beats;
        let fresh_current = beats
            .values()
            .filter(|b| fresh(b.at) && current(&b.digest))
            .collect::<Vec<_>>();
        let connected = fresh_current
            .iter()
            .filter(|b| b.state == BeatState::Connected)
            .map(|b| b.at)
            .max();
        let faulted = st
            .refused
            .as_ref()
            .and_then(|r| r.last_fault_at)
            .is_some_and(fresh);
        let (status, reason, message) = match connected {
            Some(_) if faulted => (
                ConditionStatus::False,
                "DeliveriesRefused",
                "an envelope was refused for the listener's own fault within the last three beats",
            ),
            Some(_) => (ConditionStatus::True, "Connected", ""),
            None if fresh_current
                .iter()
                .any(|b| b.state == BeatState::Connecting) =>
            {
                (
                    ConditionStatus::False,
                    "Connecting",
                    "the listener is connecting",
                )
            }
            None if !fresh_current.is_empty() => {
                (ConditionStatus::False, "Closing", "the listener is closing")
            }
            None if beats.values().any(|b| fresh(b.at)) => (
                ConditionStatus::False,
                "ListenerConfigOld",
                "the listener runs on an older account configuration; restart it from fleet gateway account",
            ),
            None if !beats.is_empty() => (
                ConditionStatus::False,
                "ListenerStale",
                "no instance beat within the last three beat intervals",
            ),
            None => (
                ConditionStatus::False,
                "NoListener",
                "no listener has beaten for this account",
            ),
        };
        cond(&mut st, INBOUND_READY, status, reason, message.into());

        st.seen = st.beat_revision;
        // The next poll: one interval after the last Known answer, or after
        // an Unknown one asked this pass; at once when nothing was asked yet.
        let asked =
            input.observations.whoami.is_some() || input.observations.capabilities.is_some();
        let next_poll = match st.observed_at {
            Some(at) => at.saturating_add(input.defaults.poll_ms),
            None if asked => now.saturating_add(input.defaults.poll_ms),
            None => now,
        };
        let goes_stale = st
            .beats
            .values()
            .filter(|b| fresh(b.at))
            .map(|b| b.at.saturating_add(window))
            .max();
        st.not_before = Some(goes_stale.map_or(next_poll, |t| t.min(next_poll)).max(now));
        settle_transitions(&mut st.conditions, &before.conditions);
        let status = if st == before {
            None
        } else {
            Some(
                serde_json::to_value(&st)
                    .map_err(|e| StoreError::Invalid(format!("Listener status: {e}")))?,
            )
        };
        Ok(Plan {
            status,
            ..Plan::default()
        })
    }
}

fn decode(obj: &Object) -> Result<(ListenerSpec, ListenerStatus), StoreError> {
    let spec: ListenerSpec = typed(listener::KIND, &obj.spec)?;
    let st: ListenerStatus = if obj.status.is_null() {
        ListenerStatus::default()
    } else {
        serde_json::from_value(obj.status.clone())
            .map_err(|e| StoreError::Invalid(format!("Listener status: {e}")))?
    };
    Ok((spec, st))
}
