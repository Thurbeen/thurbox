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
    pub project_id: Option<String>,
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

/// Per-project container configuration from the database.
#[derive(Debug, Clone)]
pub struct ProjectContainerConfigRecord {
    pub project_id: String,
    pub image: Option<String>,
    pub cpus: Option<u32>,
    pub memory_mb: Option<u32>,
    pub firewall_enabled: Option<bool>,
    pub containerfile: Option<String>,
}

impl Database {
    /// Insert a new container record.
    pub fn insert_container(
        &self,
        container_id: &str,
        session_id: Option<&str>,
        project_id: Option<&str>,
        state: &ContainerState,
        config: &ContainerConfig,
    ) -> rusqlite::Result<()> {
        let now = now_epoch();
        self.conn.execute(
            "INSERT INTO containers (id, session_id, project_id, state, image, cpus, memory_mb, firewall_enabled, containerfile, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                container_id,
                session_id,
                project_id,
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
            "SELECT id, session_id, project_id, state, docker_container_id, image, cpus, memory_mb, firewall_enabled, containerfile, error_msg, created_at, updated_at
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
            "SELECT id, session_id, project_id, state, docker_container_id, image, cpus, memory_mb, firewall_enabled, containerfile, error_msg, created_at, updated_at
             FROM containers WHERE session_id = ?1 AND deleted_at IS NULL",
        )?;

        let mut rows = stmt.query(params![session_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_container_record(row)?)),
            None => Ok(None),
        }
    }

    /// List all active containers, optionally filtered by project.
    pub fn list_containers(
        &self,
        project_id: Option<&str>,
    ) -> rusqlite::Result<Vec<ContainerRecord>> {
        let mut records = Vec::new();

        if let Some(pid) = project_id {
            let mut stmt = self.conn.prepare(
                "SELECT id, session_id, project_id, state, docker_container_id, image, cpus, memory_mb, firewall_enabled, containerfile, error_msg, created_at, updated_at
                 FROM containers WHERE project_id = ?1 AND deleted_at IS NULL
                 ORDER BY created_at",
            )?;
            let mut rows = stmt.query(params![pid])?;
            while let Some(row) = rows.next()? {
                records.push(row_to_container_record(row)?);
            }
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id, session_id, project_id, state, docker_container_id, image, cpus, memory_mb, firewall_enabled, containerfile, error_msg, created_at, updated_at
                 FROM containers WHERE deleted_at IS NULL
                 ORDER BY created_at",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                records.push(row_to_container_record(row)?);
            }
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

    /// Get project container configuration.
    pub fn get_project_container_config(
        &self,
        project_id: &str,
    ) -> rusqlite::Result<Option<ProjectContainerConfigRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT project_id, image, cpus, memory_mb, firewall_enabled, containerfile
             FROM project_container_config WHERE project_id = ?1",
        )?;

        let mut rows = stmt.query(params![project_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(ProjectContainerConfigRecord {
                project_id: row.get(0)?,
                image: row.get(1)?,
                cpus: row.get::<_, Option<i64>>(2)?.map(|v| v as u32),
                memory_mb: row.get::<_, Option<i64>>(3)?.map(|v| v as u32),
                firewall_enabled: row.get::<_, Option<i64>>(4)?.map(|v| v != 0),
                containerfile: row.get(5)?,
            })),
            None => Ok(None),
        }
    }

    /// Set project container configuration (upsert).
    pub fn set_project_container_config(
        &self,
        project_id: &str,
        config: &ContainerConfig,
    ) -> rusqlite::Result<()> {
        let now = now_epoch();
        self.conn.execute(
            "INSERT INTO project_container_config (project_id, image, cpus, memory_mb, firewall_enabled, containerfile, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(project_id) DO UPDATE SET
                image = ?2, cpus = ?3, memory_mb = ?4, firewall_enabled = ?5, containerfile = ?6, updated_at = ?7",
            params![
                project_id,
                config.image,
                config.cpus,
                config.memory_mb,
                config.firewall_enabled as i64,
                config.containerfile,
                now,
            ],
        )?;
        Ok(())
    }
}

fn row_to_container_record(row: &rusqlite::Row) -> rusqlite::Result<ContainerRecord> {
    let state_str: String = row.get(3)?;
    Ok(ContainerRecord {
        id: row.get(0)?,
        session_id: row.get(1)?,
        project_id: row.get(2)?,
        state: ContainerState::from_db_str(&state_str),
        docker_container_id: row.get(4)?,
        image: row.get(5)?,
        cpus: row.get::<_, i64>(6)? as u32,
        memory_mb: row.get::<_, i64>(7)? as u32,
        firewall_enabled: row.get::<_, i64>(8)? != 0,
        containerfile: row.get(9)?,
        error_msg: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn test_project(db: &Database) -> String {
        let id = crate::project::ProjectId::default();
        db.insert_project(id, "test-project", &[]).unwrap();
        id.to_string()
    }

    #[test]
    fn insert_and_get_container() {
        let db = test_db();
        let project_id = test_project(&db);
        let config = ContainerConfig::default();

        db.insert_container(
            "c-1",
            None,
            Some(&project_id),
            &ContainerState::Building,
            &config,
        )
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
        let project_id = test_project(&db);
        let config = ContainerConfig::default();

        db.insert_container(
            "c-1",
            None,
            Some(&project_id),
            &ContainerState::Building,
            &config,
        )
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
        let project_id = test_project(&db);
        let config = ContainerConfig::default();

        db.insert_container(
            "c-1",
            None,
            Some(&project_id),
            &ContainerState::Ready,
            &config,
        )
        .unwrap();
        db.insert_container(
            "c-2",
            None,
            Some(&project_id),
            &ContainerState::Stopped,
            &config,
        )
        .unwrap();

        let containers = db.list_containers(None).unwrap();
        assert_eq!(containers.len(), 2);
    }

    #[test]
    fn list_containers_by_project() {
        let db = test_db();
        let project_a = test_project(&db);
        let config = ContainerConfig::default();

        // Create a second project
        let project_b = crate::project::ProjectId::default();
        db.insert_project(project_b, "other-project", &[]).unwrap();
        let project_b = project_b.to_string();

        db.insert_container(
            "c-1",
            None,
            Some(&project_a),
            &ContainerState::Ready,
            &config,
        )
        .unwrap();
        db.insert_container(
            "c-2",
            None,
            Some(&project_b),
            &ContainerState::Ready,
            &config,
        )
        .unwrap();

        let a_containers = db.list_containers(Some(&project_a)).unwrap();
        assert_eq!(a_containers.len(), 1);
        assert_eq!(a_containers[0].id, "c-1");

        let b_containers = db.list_containers(Some(&project_b)).unwrap();
        assert_eq!(b_containers.len(), 1);
        assert_eq!(b_containers[0].id, "c-2");
    }

    #[test]
    fn soft_delete_container() {
        let db = test_db();
        let project_id = test_project(&db);
        let config = ContainerConfig::default();

        db.insert_container(
            "c-1",
            None,
            Some(&project_id),
            &ContainerState::Stopped,
            &config,
        )
        .unwrap();
        db.soft_delete_container("c-1").unwrap();

        let c = db.get_container("c-1").unwrap();
        assert!(c.is_none());
    }

    #[test]
    fn get_container_by_session() {
        let db = test_db();
        let project_id = test_project(&db);
        let config = ContainerConfig::default();

        let session_id = uuid::Uuid::new_v4().to_string();
        db.conn
            .execute(
                "INSERT INTO sessions (id, name, project_id, created_at, updated_at) VALUES (?1, 'test', ?2, 0, 0)",
                rusqlite::params![session_id, project_id],
            )
            .unwrap();

        db.insert_container(
            "c-1",
            Some(&session_id),
            Some(&project_id),
            &ContainerState::Ready,
            &config,
        )
        .unwrap();

        let c = db.get_container_by_session(&session_id).unwrap().unwrap();
        assert_eq!(c.id, "c-1");
        assert_eq!(c.session_id, Some(session_id));
    }

    #[test]
    fn update_container_state_with_error() {
        let db = test_db();
        let project_id = test_project(&db);
        let config = ContainerConfig::default();

        db.insert_container(
            "c-1",
            None,
            Some(&project_id),
            &ContainerState::Starting,
            &config,
        )
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
    fn project_container_config_crud() {
        let db = test_db();
        let project_id = test_project(&db);

        let cfg = db.get_project_container_config(&project_id).unwrap();
        assert!(cfg.is_none());

        let config = ContainerConfig {
            image: Some("node:20".to_string()),
            cpus: 4,
            memory_mb: 8192,
            firewall_enabled: false,
            containerfile: Some("default".to_string()),
        };
        db.set_project_container_config(&project_id, &config)
            .unwrap();

        let cfg = db
            .get_project_container_config(&project_id)
            .unwrap()
            .unwrap();
        assert_eq!(cfg.image, Some("node:20".to_string()));
        assert_eq!(cfg.cpus, Some(4));
        assert_eq!(cfg.memory_mb, Some(8192));
        assert_eq!(cfg.firewall_enabled, Some(false));

        // Update (upsert)
        let config2 = ContainerConfig { cpus: 8, ..config };
        db.set_project_container_config(&project_id, &config2)
            .unwrap();
        let cfg = db
            .get_project_container_config(&project_id)
            .unwrap()
            .unwrap();
        assert_eq!(cfg.cpus, Some(8));
    }

    #[test]
    fn update_container_session_links() {
        let db = test_db();
        let project_id = test_project(&db);
        let config = ContainerConfig::default();

        let session_id = uuid::Uuid::new_v4().to_string();
        db.conn
            .execute(
                "INSERT INTO sessions (id, name, project_id, created_at, updated_at) VALUES (?1, 'test', ?2, 0, 0)",
                rusqlite::params![session_id, project_id],
            )
            .unwrap();

        db.insert_container(
            "c-1",
            None,
            Some(&project_id),
            &ContainerState::Ready,
            &config,
        )
        .unwrap();
        db.update_container_session("c-1", &session_id).unwrap();

        let c = db.get_container("c-1").unwrap().unwrap();
        assert_eq!(c.session_id, Some(session_id.clone()));

        let c = db.get_container_by_session(&session_id).unwrap().unwrap();
        assert_eq!(c.id, "c-1");
    }
}
