use serde::{Deserialize, Serialize};

/// What fleet saw when it looked at something it does not own (P1).
///
/// `Unknown` is a value, not an error: a probe that timed out, a file that
/// could not be read, an adapter that could not tell. It never counts as done,
/// merged, empty or gone, so no caller may collapse it into `Known(None)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Observation<T> {
    Known(T),
    Unknown { why: String },
}

impl<T> Observation<T> {
    pub fn unknown(why: impl Into<String>) -> Self {
        Observation::Unknown { why: why.into() }
    }

    pub fn known(&self) -> Option<&T> {
        match self {
            Observation::Known(v) => Some(v),
            Observation::Unknown { .. } => None,
        }
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, Observation::Unknown { .. })
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Observation<U> {
        match self {
            Observation::Known(v) => Observation::Known(f(v)),
            Observation::Unknown { why } => Observation::Unknown { why },
        }
    }

    pub fn why(&self) -> Option<&str> {
        match self {
            Observation::Known(_) => None,
            Observation::Unknown { why } => Some(why),
        }
    }
}
