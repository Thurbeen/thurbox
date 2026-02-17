use std::path::PathBuf;

use rusqlite::params;

use crate::project::ProjectId;
use crate::sync::current_time_millis;
use crate::sync::SharedProject;

use super::audit::{AuditAction, EntityType};
use super::Database;

impl Database {
    /// Insert a new project with its repos.
    pub fn insert_project(
        &self,
        id: ProjectId,
        name: &str,
        repos: &[PathBuf],
    ) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        let id_str = id.to_string();

        self.conn.execute(
            "INSERT INTO projects (id, name, is_default, created_at, updated_at) \
             VALUES (?1, ?2, 0, ?3, ?4)",
            params![id_str, name, now, now],
        )?;

        for repo in repos {
            self.conn.execute(
                "INSERT INTO project_repos (project_id, repo_path) VALUES (?1, ?2)",
                params![id_str, repo.display().to_string()],
            )?;
        }

        self.log_audit(
            EntityType::Project,
            &id_str,
            AuditAction::Created,
            None,
            None,
            Some(name),
        )?;

        Ok(())
    }

    /// Update a project's name and repos.
    pub fn update_project(
        &self,
        id: ProjectId,
        name: &str,
        repos: &[PathBuf],
    ) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        let id_str = id.to_string();

        // Get old name for audit
        let old_name: Option<String> = self
            .conn
            .query_row(
                "SELECT name FROM projects WHERE id = ?1",
                params![id_str],
                |row| row.get(0),
            )
            .ok();

        self.conn.execute(
            "UPDATE projects SET name = ?1, updated_at = ?2 WHERE id = ?3",
            params![name, now, id_str],
        )?;

        // Replace repos
        self.conn.execute(
            "DELETE FROM project_repos WHERE project_id = ?1",
            params![id_str],
        )?;
        for repo in repos {
            self.conn.execute(
                "INSERT INTO project_repos (project_id, repo_path) VALUES (?1, ?2)",
                params![id_str, repo.display().to_string()],
            )?;
        }

        if old_name.as_deref() != Some(name) {
            self.log_audit(
                EntityType::Project,
                &id_str,
                AuditAction::Updated,
                Some("name"),
                old_name.as_deref(),
                Some(name),
            )?;
        }

        Ok(())
    }

    /// Soft-delete a project by setting deleted_at.
    pub fn soft_delete_project(&self, id: ProjectId) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        let id_str = id.to_string();

        self.conn.execute(
            "UPDATE projects SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
            params![now, id_str],
        )?;

        self.log_audit(
            EntityType::Project,
            &id_str,
            AuditAction::Deleted,
            None,
            None,
            None,
        )?;

        Ok(())
    }

    /// Restore a soft-deleted project.
    pub fn restore_project(&self, id: ProjectId) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        let id_str = id.to_string();

        self.conn.execute(
            "UPDATE projects SET deleted_at = NULL, updated_at = ?1 WHERE id = ?2 AND deleted_at IS NOT NULL",
            params![now, id_str],
        )?;

        self.log_audit(
            EntityType::Project,
            &id_str,
            AuditAction::Restored,
            None,
            None,
            None,
        )?;

        Ok(())
    }

    /// List only active (non-deleted) projects.
    pub fn list_active_projects(&self) -> rusqlite::Result<Vec<SharedProject>> {
        self.list_projects_where("deleted_at IS NULL")
    }

    /// List all projects including soft-deleted ones.
    pub fn list_all_projects(&self) -> rusqlite::Result<Vec<SharedProject>> {
        self.list_projects_where("1=1")
    }

    fn list_projects_where(&self, condition: &str) -> rusqlite::Result<Vec<SharedProject>> {
        let sql = format!("SELECT id, name FROM projects WHERE {condition} ORDER BY created_at");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;

        let mut projects = Vec::new();
        for (id_str, name) in rows {
            let id: ProjectId = id_str
                .parse::<uuid::Uuid>()
                .map(ProjectId::from_uuid)
                .unwrap_or_default();

            let mut repo_stmt = self.conn.prepare(
                "SELECT repo_path FROM project_repos WHERE project_id = ?1 ORDER BY repo_path",
            )?;
            let repos: Vec<PathBuf> = repo_stmt
                .query_map(params![id_str], |row| {
                    let path: String = row.get(0)?;
                    Ok(PathBuf::from(path))
                })?
                .collect::<Result<_, _>>()?;

            let roles = self.list_roles(id)?;
            let mcp_servers = self.list_mcp_servers(id)?;

            projects.push(SharedProject {
                id,
                name,
                repos,
                roles,
                mcp_servers,
            });
        }

        Ok(projects)
    }

    /// Check if a project exists (active only).
    pub fn project_exists(&self, id: ProjectId) -> rusqlite::Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM projects WHERE id = ?1 AND deleted_at IS NULL",
            params![id.to_string()],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }
}

#[cfg(test)]
mod tests {
    use crate::project::ProjectConfig;

    use super::*;

    fn test_project_id(name: &str) -> ProjectId {
        let config = ProjectConfig {
            name: name.to_string(),
            repos: vec![],
            roles: vec![],
            mcp_servers: vec![],
            id: None,
        };
        config.deterministic_id()
    }

    #[test]
    fn insert_and_list_project() {
        let db = Database::open_in_memory().unwrap();
        let id = test_project_id("test");

        db.insert_project(id, "test", &[PathBuf::from("/repo")])
            .unwrap();

        let projects = db.list_active_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "test");
        assert_eq!(projects[0].repos, vec![PathBuf::from("/repo")]);
    }

    #[test]
    fn update_project() {
        let db = Database::open_in_memory().unwrap();
        let id = test_project_id("test");

        db.insert_project(id, "test", &[PathBuf::from("/repo1")])
            .unwrap();
        db.update_project(id, "renamed", &[PathBuf::from("/repo2")])
            .unwrap();

        let projects = db.list_active_projects().unwrap();
        assert_eq!(projects[0].name, "renamed");
        assert_eq!(projects[0].repos, vec![PathBuf::from("/repo2")]);
    }

    #[test]
    fn soft_delete_hides_from_active() {
        let db = Database::open_in_memory().unwrap();
        let id = test_project_id("test");

        db.insert_project(id, "test", &[]).unwrap();
        db.soft_delete_project(id).unwrap();

        let active = db.list_active_projects().unwrap();
        assert!(active.is_empty());

        let all = db.list_all_projects().unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn restore_project_makes_active() {
        let db = Database::open_in_memory().unwrap();
        let id = test_project_id("test");

        db.insert_project(id, "test", &[]).unwrap();
        db.soft_delete_project(id).unwrap();
        db.restore_project(id).unwrap();

        let active = db.list_active_projects().unwrap();
        assert_eq!(active.len(), 1);
    }

    #[test]
    fn project_exists_check() {
        let db = Database::open_in_memory().unwrap();
        let id = test_project_id("test");

        assert!(!db.project_exists(id).unwrap());

        db.insert_project(id, "test", &[]).unwrap();
        assert!(db.project_exists(id).unwrap());

        db.soft_delete_project(id).unwrap();
        assert!(!db.project_exists(id).unwrap());
    }

    #[test]
    fn insert_creates_audit_entry() {
        let db = Database::open_in_memory().unwrap();
        let id = test_project_id("test");

        db.insert_project(id, "test", &[]).unwrap();

        let entries = db.get_audit_log(None, None, 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "created");
        assert_eq!(entries[0].entity_type, "project");
    }

    #[test]
    fn multiple_repos() {
        let db = Database::open_in_memory().unwrap();
        let id = test_project_id("multi");

        let repos = vec![
            PathBuf::from("/a"),
            PathBuf::from("/b"),
            PathBuf::from("/c"),
        ];
        db.insert_project(id, "multi", &repos).unwrap();

        let projects = db.list_active_projects().unwrap();
        assert_eq!(projects[0].repos.len(), 3);
    }
}
