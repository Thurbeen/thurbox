use rusqlite::params;

use crate::sync::current_time_millis;

use super::Database;

/// Entity type for audit log entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityType {
    Session,
    Worktree,
    Task,
}

impl EntityType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Worktree => "worktree",
            Self::Task => "task",
        }
    }
}

/// Action recorded in the audit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditAction {
    Created,
    Updated,
    Deleted,
    Restored,
}

impl AuditAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Updated => "updated",
            Self::Deleted => "deleted",
            Self::Restored => "restored",
        }
    }
}

/// Audit entries older than this are pruned on `Database::open`. They are
/// debugging breadcrumbs, not compliance data, so 90 days is plenty.
pub const AUDIT_RETENTION_DAYS: u64 = 90;

/// A single audit log entry.
#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub id: i64,
    pub timestamp: u64,
    pub entity_type: String,
    pub entity_id: String,
    pub action: String,
    pub field: Option<String>,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub instance_id: Option<String>,
}

impl Database {
    /// Record an audit log entry.
    pub fn log_audit(
        &self,
        entity_type: EntityType,
        entity_id: &str,
        action: AuditAction,
        field: Option<&str>,
        old_value: Option<&str>,
        new_value: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO audit_log (timestamp, entity_type, entity_id, action, field, old_value, new_value, instance_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                current_time_millis() as i64,
                entity_type.as_str(),
                entity_id,
                action.as_str(),
                field,
                old_value,
                new_value,
                self.instance_id,
            ],
        )?;
        Ok(())
    }

    /// Delete audit entries older than [`AUDIT_RETENTION_DAYS`]. Returns the
    /// number of rows removed. Cheap thanks to `idx_audit_log_timestamp`.
    pub fn prune_audit_log(&self) -> rusqlite::Result<usize> {
        let cutoff =
            current_time_millis().saturating_sub(AUDIT_RETENTION_DAYS * 24 * 60 * 60 * 1000);
        self.conn.execute(
            "DELETE FROM audit_log WHERE timestamp < ?1",
            params![cutoff as i64],
        )
    }

    /// Query audit log entries with optional filters.
    pub fn get_audit_log(
        &self,
        entity_type: Option<EntityType>,
        entity_id: Option<&str>,
        limit: usize,
    ) -> rusqlite::Result<Vec<AuditEntry>> {
        let mut sql = String::from(
            "SELECT id, timestamp, entity_type, entity_id, action, field, old_value, new_value, instance_id \
             FROM audit_log WHERE 1=1",
        );

        if entity_type.is_some() {
            sql.push_str(" AND entity_type = ?1");
        }
        if entity_id.is_some() {
            sql.push_str(" AND entity_id = ?2");
        }
        sql.push_str(" ORDER BY id DESC LIMIT ?3");

        let mut stmt = self.conn.prepare(&sql)?;

        let rows = stmt.query_map(
            params![entity_type.map(|e| e.as_str()), entity_id, limit as i64,],
            |row| {
                Ok(AuditEntry {
                    id: row.get(0)?,
                    timestamp: row.get::<_, i64>(1)? as u64,
                    entity_type: row.get(2)?,
                    entity_id: row.get(3)?,
                    action: row.get(4)?,
                    field: row.get(5)?,
                    old_value: row.get(6)?,
                    new_value: row.get(7)?,
                    instance_id: row.get(8)?,
                })
            },
        )?;

        rows.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_type_strings() {
        assert_eq!(EntityType::Session.as_str(), "session");
        assert_eq!(EntityType::Worktree.as_str(), "worktree");
        assert_eq!(EntityType::Task.as_str(), "task");
    }

    #[test]
    fn log_and_query_audit() {
        let db = Database::open_in_memory().unwrap();

        db.log_audit(
            EntityType::Session,
            "proj-1",
            AuditAction::Created,
            None,
            None,
            Some("My Project"),
        )
        .unwrap();

        let entries = db.get_audit_log(None, None, 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entity_type, "session");
        assert_eq!(entries[0].entity_id, "proj-1");
        assert_eq!(entries[0].action, "created");
    }

    #[test]
    fn filter_by_entity_type() {
        let db = Database::open_in_memory().unwrap();

        db.log_audit(
            EntityType::Worktree,
            "w1",
            AuditAction::Created,
            None,
            None,
            None,
        )
        .unwrap();
        db.log_audit(
            EntityType::Session,
            "s1",
            AuditAction::Created,
            None,
            None,
            None,
        )
        .unwrap();

        let worktrees = db
            .get_audit_log(Some(EntityType::Worktree), None, 10)
            .unwrap();
        assert_eq!(worktrees.len(), 1);
        assert_eq!(worktrees[0].entity_id, "w1");
    }

    #[test]
    fn filter_by_entity_id() {
        let db = Database::open_in_memory().unwrap();

        db.log_audit(
            EntityType::Session,
            "s1",
            AuditAction::Created,
            None,
            None,
            None,
        )
        .unwrap();
        db.log_audit(
            EntityType::Session,
            "s2",
            AuditAction::Created,
            None,
            None,
            None,
        )
        .unwrap();

        let entries = db
            .get_audit_log(Some(EntityType::Session), Some("s1"), 10)
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entity_id, "s1");
    }

    #[test]
    fn audit_records_field_changes() {
        let db = Database::open_in_memory().unwrap();

        db.log_audit(
            EntityType::Session,
            "p1",
            AuditAction::Updated,
            Some("name"),
            Some("Old Name"),
            Some("New Name"),
        )
        .unwrap();

        let entries = db.get_audit_log(None, None, 10).unwrap();
        assert_eq!(entries[0].field.as_deref(), Some("name"));
        assert_eq!(entries[0].old_value.as_deref(), Some("Old Name"));
        assert_eq!(entries[0].new_value.as_deref(), Some("New Name"));
    }

    #[test]
    fn audit_limit_works() {
        let db = Database::open_in_memory().unwrap();

        for i in 0..5 {
            db.log_audit(
                EntityType::Session,
                &format!("s{i}"),
                AuditAction::Created,
                None,
                None,
                None,
            )
            .unwrap();
        }

        let entries = db.get_audit_log(None, None, 3).unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn prune_removes_only_entries_past_retention() {
        let db = Database::open_in_memory().unwrap();

        let now = current_time_millis();
        let day_ms = 24 * 60 * 60 * 1000;
        let stale = now - (AUDIT_RETENTION_DAYS + 1) * day_ms;
        let insert = |timestamp: u64, entity_id: &str| {
            db.conn_ref()
                .execute(
                    "INSERT INTO audit_log (timestamp, entity_type, entity_id, action) \
                     VALUES (?1, 'session', ?2, 'created')",
                    params![timestamp as i64, entity_id],
                )
                .unwrap();
        };
        insert(stale, "old");
        insert(now, "fresh");

        assert_eq!(db.prune_audit_log().unwrap(), 1);

        let entries = db.get_audit_log(None, None, 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].entity_id, "fresh");
    }

    #[test]
    fn audit_includes_instance_id() {
        let db = Database::open_in_memory().unwrap();

        db.log_audit(
            EntityType::Session,
            "p1",
            AuditAction::Created,
            None,
            None,
            None,
        )
        .unwrap();

        let entries = db.get_audit_log(None, None, 1).unwrap();
        assert!(entries[0].instance_id.is_some());
    }
}
