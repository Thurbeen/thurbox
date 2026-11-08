//! Naming an automation the way a script does: by id, by key, or by a name
//! nothing else carries (issue #1515).

use crate::session::Automation;
use crate::storage::Database;

/// Why a reference named no single automation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefError {
    /// Nothing answers to it.
    NotFound(String),
    /// Several automations share the name; the message lists them.
    Ambiguous(String),
    /// The database could not be asked.
    Storage(String),
}

impl std::fmt::Display for RefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefError::NotFound(m) | RefError::Ambiguous(m) | RefError::Storage(m) => f.write_str(m),
        }
    }
}

/// The automation `reference` names: an id, then a key, then a name only one
/// automation has.
///
/// In that order because each is more deliberate than the next: an id is
/// minted by the database, a key is chosen to be unique, and a name is only a
/// label. A name two automations share is refused with both ids rather than
/// guessed at — acting on the wrong one of two schedules is the mistake a
/// script cannot see it made.
pub fn resolve(db: &Database, reference: &str) -> Result<Automation, RefError> {
    let storage = |e: rusqlite::Error| RefError::Storage(format!("look up automation: {e}"));
    if let Ok(id) = reference.parse::<i64>() {
        if let Some(auto) = db.get_automation(id).map_err(storage)? {
            return Ok(auto);
        }
    }
    if let Some(auto) = db.get_automation_by_key(reference).map_err(storage)? {
        return Ok(auto);
    }
    let mut named = db.find_automations_by_name(reference).map_err(storage)?;
    match named.len() {
        0 => Err(RefError::NotFound(format!(
            "no automation has id, key or name '{reference}'"
        ))),
        1 => Ok(named.remove(0)),
        _ => {
            let ids: Vec<String> = named.iter().map(|a| format!("#{}", a.id)).collect();
            Err(RefError::Ambiguous(format!(
                "{} automations are named '{reference}' ({}); name one by its id or key",
                named.len(),
                ids.join(", ")
            )))
        }
    }
}
