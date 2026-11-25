use serde::{Deserialize, Serialize};

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
