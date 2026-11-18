//! Board: a named, append-only log of events that every Task may publish
//! to and read. The spec bounds the log; the status says which `seq`s it
//! still holds. The events themselves are not resources: they live in the
//! store's per-board log, reached only through [`Store::append_events`]
//! and [`Store::read_events`].
//!
//! A Board has no controller. Nobody writes its status through
//! `update_status`: the store's typed append moves `head` and `oldest` in
//! the same write that appends the events.
//!
//! [`Store::append_events`]: crate::store::Store::append_events
//! [`Store::read_events`]: crate::store::Store::read_events

use serde::{Deserialize, Serialize};

use crate::meta::Millis;

pub const KIND: &str = "Board";

/// The board `fleet init` ensures, and the one `fleet event` uses by default.
pub const SHARED: &str = "shared";

/// The board `fleet init` ensures for Fleet's own events: TaskController
/// publishes `fleet.task.*` to it.
pub const FLEET: &str = "fleet";

pub const DEFAULT_MAX_AGE_SECONDS: u64 = 7 * 24 * 60 * 60;
pub const DEFAULT_MAX_COUNT: u64 = 10_000;
pub const DEFAULT_MAX_DATA_BYTES: u64 = 16 * 1024;

pub const MAX_COUNT_LIMIT: u64 = 1_000_000;
pub const MAX_DATA_BYTES_LIMIT: u64 = 1024 * 1024;

/// The most events one log segment holds. Retention drops whole segments,
/// so a segment is a tenth of `maxCount`, at least one event and at most
/// this many.
pub const MAX_SEGMENT_EVENTS: u64 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardSpec {
    #[serde(default)]
    pub retention: Retention,
    /// The most bytes an event's `data` may take, serialized as JSON.
    #[serde(default = "default_max_data_bytes")]
    pub max_data_bytes: u64,
}

impl Default for BoardSpec {
    fn default() -> BoardSpec {
        BoardSpec {
            retention: Retention::default(),
            max_data_bytes: DEFAULT_MAX_DATA_BYTES,
        }
    }
}

impl BoardSpec {
    /// How many events one segment of this board's log holds.
    pub fn segment_events(&self) -> u64 {
        (self.retention.max_count / 10).clamp(1, MAX_SEGMENT_EVENTS)
    }

    pub fn validate(&self) -> Result<(), String> {
        let r = &self.retention;
        if r.max_age_seconds == 0 {
            return Err("retention.maxAgeSeconds must be at least 1".into());
        }
        if !(1..=MAX_COUNT_LIMIT).contains(&r.max_count) {
            return Err(format!("retention.maxCount must be 1 to {MAX_COUNT_LIMIT}"));
        }
        if !(1..=MAX_DATA_BYTES_LIMIT).contains(&self.max_data_bytes) {
            return Err(format!("maxDataBytes must be 1 to {MAX_DATA_BYTES_LIMIT}"));
        }
        Ok(())
    }
}

/// How long the log keeps an event. Retention runs only when someone
/// publishes, and drops whole segments: a segment goes once all its events
/// are older than `maxAgeSeconds`, or while the log holds more than
/// `maxCount` events. Until the next publish, a read can still show older
/// events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Retention {
    #[serde(default = "default_max_age_seconds")]
    pub max_age_seconds: u64,
    #[serde(default = "default_max_count")]
    pub max_count: u64,
}

impl Default for Retention {
    fn default() -> Retention {
        Retention {
            max_age_seconds: DEFAULT_MAX_AGE_SECONDS,
            max_count: DEFAULT_MAX_COUNT,
        }
    }
}

impl Retention {
    pub fn max_age_ms(&self) -> Millis {
        self.max_age_seconds.saturating_mul(1000)
    }
}

fn default_max_age_seconds() -> u64 {
    DEFAULT_MAX_AGE_SECONDS
}

fn default_max_count() -> u64 {
    DEFAULT_MAX_COUNT
}

fn default_max_data_bytes() -> u64 {
    DEFAULT_MAX_DATA_BYTES
}

/// Which `seq`s the log holds: `oldest..=head`. `head` is the last `seq`
/// handed out, 0 before the first event; `oldest` is the first `seq` a
/// reader can still get, `head + 1` when the log holds none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoardStatus {
    #[serde(default)]
    pub head: u64,
    #[serde(default = "first_seq")]
    pub oldest: u64,
}

impl Default for BoardStatus {
    fn default() -> BoardStatus {
        BoardStatus {
            head: 0,
            oldest: first_seq(),
        }
    }
}

fn first_seq() -> u64 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_empty_spec_takes_the_defaults() {
        let spec: BoardSpec = serde_json::from_value(json!({})).unwrap();
        assert_eq!(spec, BoardSpec::default());
        assert_eq!(spec.retention.max_age_seconds, 604_800);
        assert_eq!(spec.retention.max_count, 10_000);
        assert_eq!(spec.max_data_bytes, 16_384);
        assert_eq!(spec.segment_events(), 1_000);
    }

    #[test]
    fn a_segment_is_a_tenth_of_max_count_within_bounds() {
        let with = |max_count| BoardSpec {
            retention: Retention {
                max_count,
                ..Retention::default()
            },
            ..BoardSpec::default()
        };
        assert_eq!(with(2).segment_events(), 1);
        assert_eq!(with(250).segment_events(), 25);
        assert_eq!(with(MAX_COUNT_LIMIT).segment_events(), MAX_SEGMENT_EVENTS);
    }

    #[test]
    fn zero_bounds_are_refused() {
        for spec in [
            json!({"retention": {"maxAgeSeconds": 0}}),
            json!({"retention": {"maxCount": 0}}),
            json!({"maxDataBytes": 0}),
            json!({"maxDataBytes": MAX_DATA_BYTES_LIMIT + 1}),
        ] {
            let spec: BoardSpec = serde_json::from_value(spec).unwrap();
            assert!(spec.validate().is_err(), "{spec:?}");
        }
    }

    #[test]
    fn a_fresh_board_holds_nothing_and_starts_at_one() {
        let st: BoardStatus = serde_json::from_value(json!({})).unwrap();
        assert_eq!(st, BoardStatus { head: 0, oldest: 1 });
    }
}
