//! Durable pre-commit resource plans. The row is not an active session yet.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::session::{LaunchRecipe, SessionId};
use crate::sync::SharedSession;

use super::Database;

#[derive(Clone, Serialize, Deserialize)]
pub struct CreateIntent {
    pub expected_deleted_at: Option<u64>,
    pub session: SharedSession,
    /// Panes present before creation, so an unstamped namesake is never ours.
    pub before: Vec<String>,
    pub expires_at: u64,
}

impl Database {
    pub fn begin_create_intent(&self, intent: &CreateIntent) -> rusqlite::Result<()> {
        let tx = self.write_transaction()?;
        let id = intent.session.id.to_string();
        let held: Option<Option<i64>> = self
            .conn
            .query_row(
                "SELECT deleted_at FROM sessions WHERE id = ?1",
                [&id],
                |r| r.get(0),
            )
            .optional()?;
        let admitted = match intent.expected_deleted_at {
            Some(at) => held == Some(Some(at as i64)),
            None => held.is_none(),
        };
        if !admitted {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "session changed before intent: {id}"
            )));
        }
        self.conn.execute(
            "INSERT INTO session_intents(id, payload, expires_at) VALUES (?1, ?2, ?3)",
            params![
                id,
                serde_json::to_string(intent)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                intent.expires_at as i64
            ],
        )?;
        tx.commit()
    }

    pub fn update_create_intent(&self, intent: &CreateIntent) -> rusqlite::Result<()> {
        let changed = self.conn.execute(
            "UPDATE session_intents SET payload = ?2, expires_at = ?3 WHERE id = ?1",
            params![
                intent.session.id.to_string(),
                serde_json::to_string(intent)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
                intent.expires_at as i64
            ],
        )?;
        if changed != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    }

    pub fn check_create_fence(&self, id: SessionId, token: u64) -> rusqlite::Result<()> {
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM session_intents WHERE id = ?1)",
            [id.to_string()],
            |r| r.get(0),
        )?;
        if !exists
            || !self.owns_lease(
                &super::event_cursors::LockSubject::Session(id),
                token,
                crate::sync::current_time_millis(),
            )?
        {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    }

    pub fn update_owned_create_intent(
        &self,
        intent: &CreateIntent,
        token: u64,
    ) -> rusqlite::Result<()> {
        let tx = self.write_transaction()?;
        self.check_create_fence(intent.session.id, token)?;
        self.update_create_intent(intent)?;
        tx.commit()
    }

    pub fn create_intent(&self, id: SessionId) -> rusqlite::Result<Option<CreateIntent>> {
        let stored: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT payload, expires_at FROM session_intents WHERE id = ?1",
                [id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        stored
            .map(|(payload, expires_at)| {
                let mut intent: CreateIntent = serde_json::from_str(&payload).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                intent.expires_at = expires_at as u64;
                Ok(intent)
            })
            .transpose()
    }

    pub fn expired_create_intents(&self, now: u64) -> rusqlite::Result<Vec<CreateIntent>> {
        let mut stmt = self.conn.prepare("SELECT payload FROM session_intents WHERE expires_at <= ?1 ORDER BY expires_at LIMIT 128")?;
        let rows = stmt.query_map([now as i64], |r| {
            let payload: String = r.get(0)?;
            serde_json::from_str(&payload).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        })?;
        rows.collect()
    }

    pub fn clear_create_intent(&self, id: SessionId) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM session_intents WHERE id = ?1",
            [id.to_string()],
        )?;
        Ok(())
    }

    pub fn commit_prepared_intent(&self, id: SessionId, token: u64) -> rusqlite::Result<()> {
        let tx = self.write_transaction()?;
        self.check_create_fence(id, token)?;
        self.clear_create_intent(id)?;
        tx.commit()
    }

    pub fn commit_created_session(
        &self,
        session: &SharedSession,
        lease_token: u64,
        recipe: Option<&LaunchRecipe>,
        env: &std::collections::BTreeMap<String, String>,
        agent_args: &[String],
    ) -> rusqlite::Result<()> {
        let tx = self.write_transaction()?;
        self.check_create_fence(session.id, lease_token)?;
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
            [session.id.to_string()],
            |r| r.get(0),
        )?;
        if exists {
            return Err(rusqlite::Error::InvalidParameterName(
                "session changed during creation".into(),
            ));
        }
        self.upsert_session_in_transaction(session, super::EventReason::Spawned)?;
        self.set_agent_args(session.id, agent_args)?;
        if let Some(recipe) = recipe {
            self.set_launch_recipe(session.id, recipe)?;
        } else if !env.is_empty() {
            self.set_launch_env(session.id, env)?;
        }
        self.clear_create_intent(session.id)?;
        tx.commit()
    }
}

impl Database {
    pub fn loss_suspects(&self) -> rusqlite::Result<std::collections::HashMap<SessionId, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, pane FROM session_loss_suspects")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut result = std::collections::HashMap::new();
        for row in rows {
            let (id, pane) = row?;
            if let Ok(id) = id.parse() {
                result.insert(id, pane);
            }
        }
        Ok(result)
    }

    pub fn store_loss_suspects(
        &self,
        suspects: &std::collections::HashMap<SessionId, String>,
    ) -> rusqlite::Result<()> {
        let tx = self.write_transaction()?;
        if self.loss_suspects()? == *suspects {
            return tx.commit();
        }
        self.conn.execute("DELETE FROM session_loss_suspects", [])?;
        for (id, pane) in suspects {
            self.conn.execute(
                "INSERT INTO session_loss_suspects (id, pane) VALUES (?1, ?2)",
                params![id.to_string(), pane],
            )?;
        }
        tx.commit()
    }
}

impl Database {
    /// A peer may first learn an id after it was purged. Reserve it without
    /// inventing a live row or overwriting an id another route already owns.
    pub fn insert_purge_tombstone(&self, id: SessionId, backend: &str) -> rusqlite::Result<()> {
        let now = crate::sync::current_time_millis() as i64;
        self.conn.execute("INSERT OR IGNORE INTO sessions (id, name, backend_type, created_at, updated_at, deleted_at, purged_at, force_deleted)
            VALUES (?1, ?1, ?2, ?3, ?3, ?3, ?3, 1)", params![id.to_string(), backend, now])?;
        Ok(())
    }
}

impl Database {
    pub fn session_backend(&self, id: SessionId) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT backend_type FROM sessions WHERE id = ?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()
    }
    pub fn lifecycle_generation(&self, id: SessionId) -> rusqlite::Result<Option<u64>> {
        self.conn
            .query_row(
                "SELECT lifecycle_generation FROM sessions WHERE id = ?1",
                [id.to_string()],
                |r| r.get::<_, i64>(0).map(|v| v as u64),
            )
            .optional()
    }
    pub fn require_lifecycle_generation(
        &self,
        id: SessionId,
        expected: Option<u64>,
    ) -> rusqlite::Result<()> {
        if let Some(expected) = expected {
            if self.lifecycle_generation(id)? != Some(expected) {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
        }
        Ok(())
    }
    pub fn host_lifecycle_generation(&self, id: SessionId) -> rusqlite::Result<Option<u64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT host_lifecycle_generation FROM sessions WHERE id = ?1",
                [id.to_string()],
                |r| r.get::<_, Option<i64>>(0).map(|v| v.map(|v| v as u64)),
            )
            .optional()?
            .flatten())
    }
    pub fn set_host_lifecycle_generation(
        &self,
        id: SessionId,
        generation: u64,
    ) -> rusqlite::Result<()> {
        self.conn.execute("UPDATE sessions SET host_lifecycle_generation = ?2 WHERE id = ?1 AND (host_lifecycle_generation IS NULL OR host_lifecycle_generation < ?2)", params![id.to_string(), generation as i64])?;
        Ok(())
    }
    pub fn load_lifecycle_generations(
        &self,
    ) -> rusqlite::Result<std::collections::HashMap<SessionId, u64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, lifecycle_generation FROM sessions WHERE deleted_at IS NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
        })?;
        let mut values = std::collections::HashMap::new();
        for row in rows {
            let (id, generation) = row?;
            if let Ok(id) = id.parse() {
                values.insert(id, generation);
            }
        }
        Ok(values)
    }
}
