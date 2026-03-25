//! Container CRUD operations for the SQLite database.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::params;

use crate::session::{ContainerConfig, ContainerState};

use super::Database;

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// A container record from the database.
#[derive(Debug, Clone)]
pub struct ContainerRecord {
    pub id: String,
    pub session_id: Option<String>,
    pub state: ContainerState,
    pub docker_container_id: Option<String>,
    pub image: Option<String>,
    pub cpus: u32,
    pub memory_mb: u32,
    pub firewall_enabled: bool,
    pub containerfile: Option<String>,
    pub error_msg: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Database {
    /// Insert a new container record.
    pub fn insert_container(
        &self,
        container_id: &str,
        session_id: Option<&str>,
        state: &ContainerState,
        config: &ContainerConfig,
    ) -> rusqlite::Result<()> {
        let now = now_epoch();
        self.conn.execute(
            "INSERT INTO containers (id, session_id, state, image, cpus, memory_mb, firewall_enabled, containerfile, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                container_id,
                session_id,
                state.to_db_str(),
                config.image,
                config.cpus,
                config.memory_mb,
                config.firewall_enabled as i64,
                config.containerfile,
                now,
                now,
            ],
        )?;
        Ok(())
    }

    /// Update container state.
    pub fn update_container_state(
        &self,
        container_id: &str,
        state: &ContainerState,
        docker_container_id: Option<&str>,
        error_msg: Option<&str>,
    ) -> rusqlite::Result<()> {
        let now = now_epoch();
        self.conn.execute(
            "UPDATE containers SET state = ?1, docker_container_id = ?2, error_msg = ?3, updated_at = ?4 WHERE id = ?5",
            params![
                state.to_db_str(),
                docker_container_id,
                error_msg,
                now,
                container_id,
            ],
        )?;
        Ok(())
    }

    /// Get a container record by ID.
    pub fn get_container(&self, container_id: &str) -> rusqlite::Result<Option<ContainerRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, state, docker_container_id, image, cpus, memory_mb, firewall_enabled, containerfile, error_msg, created_at, updated_at
             FROM containers WHERE id = ?1 AND deleted_at IS NULL",
        )?;

        let mut rows = stmt.query(params![container_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_container_record(row)?)),
            None => Ok(None),
        }
    }

    /// Get container record by session ID.
    pub fn get_container_by_session(
        &self,
        session_id: &str,
    ) -> rusqlite::Result<Option<ContainerRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, state, docker_container_id, image, cpus, memory_mb, firewall_enabled, containerfile, error_msg, created_at, updated_at
             FROM containers WHERE session_id = ?1 AND deleted_at IS NULL",
        )?;

        let mut rows = stmt.query(params![session_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_container_record(row)?)),
            None => Ok(None),
        }
    }

    /// List all active containers.
    pub fn list_containers(&self) -> rusqlite::Result<Vec<ContainerRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, state, docker_container_id, image, cpus, memory_mb, firewall_enabled, containerfile, error_msg, created_at, updated_at
             FROM containers WHERE deleted_at IS NULL
             ORDER BY created_at",
        )?;
        let mut records = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            records.push(row_to_container_record(row)?);
        }
        Ok(records)
    }

    /// Update the session ID on a container record.
    pub fn update_container_session(
        &self,
        container_id: &str,
        session_id: &str,
    ) -> rusqlite::Result<()> {
        let now = now_epoch();
        self.conn.execute(
            "UPDATE containers SET session_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![session_id, now, container_id],
        )?;
        Ok(())
    }

    /// Soft-delete a container record.
    pub fn soft_delete_container(&self, container_id: &str) -> rusqlite::Result<()> {
        let now = now_epoch();
        self.conn.execute(
            "UPDATE containers SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
            params![now, container_id],
        )?;
        Ok(())
    }
}

fn row_to_container_record(row: &rusqlite::Row) -> rusqlite::Result<ContainerRecord> {
    let state_str: String = row.get(2)?;
    Ok(ContainerRecord {
        id: row.get(0)?,
        session_id: row.get(1)?,
        state: ContainerState::from_db_str(&state_str),
        docker_container_id: row.get(3)?,
        image: row.get(4)?,
        cpus: row.get::<_, i64>(5)? as u32,
        memory_mb: row.get::<_, i64>(6)? as u32,
        firewall_enabled: row.get::<_, i64>(7)? != 0,
        containerfile: row.get(8)?,
        error_msg: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn insert_and_get_container() {
        let db = test_db();
        let config = ContainerConfig::default();

        db.insert_container("c-1", None, &ContainerState::Building, &config)
            .unwrap();

        let c = db.get_container("c-1").unwrap().unwrap();
        assert_eq!(c.id, "c-1");
        assert_eq!(c.state, ContainerState::Building);
        assert_eq!(c.cpus, 2);
        assert_eq!(c.memory_mb, 2048);
        assert!(c.firewall_enabled);
    }

    #[test]
    fn get_nonexistent_container() {
        let db = test_db();
        let c = db.get_container("nonexistent").unwrap();
        assert!(c.is_none());
    }

    #[test]
    fn update_container_state() {
        let db = test_db();
        let config = ContainerConfig::default();

        db.insert_container("c-1", None, &ContainerState::Building, &config)
            .unwrap();
        db.update_container_state("c-1", &ContainerState::Ready, Some("abc123"), None)
            .unwrap();

        let c = db.get_container("c-1").unwrap().unwrap();
        assert_eq!(c.state, ContainerState::Ready);
        assert_eq!(c.docker_container_id, Some("abc123".to_string()));
    }

    #[test]
    fn list_containers_all() {
        let db = test_db();
        let config = ContainerConfig::default();

        db.insert_container("c-1", None, &ContainerState::Ready, &config)
            .unwrap();
        db.insert_container("c-2", None, &ContainerState::Stopped, &config)
            .unwrap();

        let containers = db.list_containers().unwrap();
        assert_eq!(containers.len(), 2);
    }

    #[test]
    fn soft_delete_container() {
        let db = test_db();
        let config = ContainerConfig::default();

        db.insert_container("c-1", None, &ContainerState::Stopped, &config)
            .unwrap();
        db.soft_delete_container("c-1").unwrap();

        let c = db.get_container("c-1").unwrap();
        assert!(c.is_none());
    }

    #[test]
    fn get_container_by_session() {
        let db = test_db();
        let config = ContainerConfig::default();

        let session_id = uuid::Uuid::new_v4().to_string();
        db.conn
            .execute(
                "INSERT INTO sessions (id, name, created_at, updated_at) VALUES (?1, 'test', 0, 0)",
                rusqlite::params![session_id],
            )
            .unwrap();

        db.insert_container("c-1", Some(&session_id), &ContainerState::Ready, &config)
            .unwrap();

        let c = db.get_container_by_session(&session_id).unwrap().unwrap();
        assert_eq!(c.id, "c-1");
        assert_eq!(c.session_id, Some(session_id));
    }

    #[test]
    fn update_container_state_with_error() {
        let db = test_db();
        let config = ContainerConfig::default();

        db.insert_container("c-1", None, &ContainerState::Starting, &config)
            .unwrap();
        db.update_container_state(
            "c-1",
            &ContainerState::Failed("build error".to_string()),
            None,
            Some("build error"),
        )
        .unwrap();

        let c = db.get_container("c-1").unwrap().unwrap();
        assert_eq!(c.state, ContainerState::Failed("build error".to_string()));
        assert_eq!(c.error_msg, Some("build error".to_string()));
    }

    #[test]
    fn update_container_session_links() {
        let db = test_db();
        let config = ContainerConfig::default();

        let session_id = uuid::Uuid::new_v4().to_string();
        db.conn
            .execute(
                "INSERT INTO sessions (id, name, created_at, updated_at) VALUES (?1, 'test', 0, 0)",
                rusqlite::params![session_id],
            )
            .unwrap();

        db.insert_container("c-1", None, &ContainerState::Ready, &config)
            .unwrap();
        db.update_container_session("c-1", &session_id).unwrap();

        let c = db.get_container("c-1").unwrap().unwrap();
        assert_eq!(c.session_id, Some(session_id.clone()));

        let c = db.get_container_by_session(&session_id).unwrap().unwrap();
        assert_eq!(c.id, "c-1");
    }
}
