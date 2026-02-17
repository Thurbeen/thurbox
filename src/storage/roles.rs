use std::collections::HashMap;

use rusqlite::params;

use crate::project::ProjectId;
use crate::session::{RoleConfig, RolePermissions};
use crate::sync::current_time_millis;

use super::Database;

impl Database {
    /// List all roles for a specific project.
    pub fn list_roles(&self, project_id: ProjectId) -> rusqlite::Result<Vec<RoleConfig>> {
        let id_str = project_id.to_string();
        let mut stmt = self.conn.prepare(
            "SELECT role_name, description, permission_mode, allowed_tools, \
             disallowed_tools, tools, append_system_prompt \
             FROM project_roles WHERE project_id = ?1 ORDER BY role_name",
        )?;

        let roles = stmt
            .query_map(params![id_str], |row| {
                let name: String = row.get(0)?;
                let description: String = row.get(1)?;
                let permission_mode: Option<String> = row.get(2)?;
                let allowed_csv: String = row.get(3)?;
                let disallowed_csv: String = row.get(4)?;
                let tools: Option<String> = row.get(5)?;
                let append_system_prompt: Option<String> = row.get(6)?;

                Ok(RoleConfig {
                    name,
                    description,
                    permissions: RolePermissions {
                        permission_mode,
                        allowed_tools: csv_to_vec(&allowed_csv),
                        disallowed_tools: csv_to_vec(&disallowed_csv),
                        tools,
                        append_system_prompt,
                    },
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(roles)
    }

    /// List roles for all active projects, keyed by project ID.
    pub fn list_all_roles(&self) -> rusqlite::Result<HashMap<ProjectId, Vec<RoleConfig>>> {
        let mut stmt = self.conn.prepare(
            "SELECT pr.project_id, pr.role_name, pr.description, pr.permission_mode, \
             pr.allowed_tools, pr.disallowed_tools, pr.tools, pr.append_system_prompt \
             FROM project_roles pr \
             INNER JOIN projects p ON p.id = pr.project_id AND p.deleted_at IS NULL \
             ORDER BY pr.project_id, pr.role_name",
        )?;

        let mut map: HashMap<ProjectId, Vec<RoleConfig>> = HashMap::new();

        let rows = stmt.query_map([], |row| {
            let pid_str: String = row.get(0)?;
            let name: String = row.get(1)?;
            let description: String = row.get(2)?;
            let permission_mode: Option<String> = row.get(3)?;
            let allowed_csv: String = row.get(4)?;
            let disallowed_csv: String = row.get(5)?;
            let tools: Option<String> = row.get(6)?;
            let append_system_prompt: Option<String> = row.get(7)?;

            Ok((
                pid_str,
                RoleConfig {
                    name,
                    description,
                    permissions: RolePermissions {
                        permission_mode,
                        allowed_tools: csv_to_vec(&allowed_csv),
                        disallowed_tools: csv_to_vec(&disallowed_csv),
                        tools,
                        append_system_prompt,
                    },
                },
            ))
        })?;

        for row in rows {
            let (pid_str, role) = row?;
            let pid = pid_str
                .parse::<uuid::Uuid>()
                .map(ProjectId::from_uuid)
                .unwrap_or_default();
            map.entry(pid).or_default().push(role);
        }

        Ok(map)
    }

    /// Replace all roles for a project (delete existing + insert new).
    pub fn replace_roles(
        &self,
        project_id: ProjectId,
        roles: &[RoleConfig],
    ) -> rusqlite::Result<()> {
        let id_str = project_id.to_string();
        let now = current_time_millis() as i64;

        self.conn.execute(
            "DELETE FROM project_roles WHERE project_id = ?1",
            params![id_str],
        )?;

        for role in roles {
            self.conn.execute(
                "INSERT INTO project_roles \
                 (project_id, role_name, description, permission_mode, \
                  allowed_tools, disallowed_tools, tools, append_system_prompt, \
                  created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    id_str,
                    role.name,
                    role.description,
                    role.permissions.permission_mode,
                    vec_to_csv(&role.permissions.allowed_tools),
                    vec_to_csv(&role.permissions.disallowed_tools),
                    role.permissions.tools,
                    role.permissions.append_system_prompt,
                    now,
                    now,
                ],
            )?;
        }

        Ok(())
    }
}

/// Convert a comma-separated string to a Vec<String>, filtering empty entries.
fn csv_to_vec(csv: &str) -> Vec<String> {
    if csv.is_empty() {
        Vec::new()
    } else {
        csv.split(',').map(|s| s.to_string()).collect()
    }
}

/// Convert a Vec<String> to a comma-separated string.
fn vec_to_csv(v: &[String]) -> String {
    v.join(",")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

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

    fn setup_db_with_project(name: &str) -> (Database, ProjectId) {
        let db = Database::open_in_memory().unwrap();
        let pid = test_project_id(name);
        db.insert_project(pid, name, &[PathBuf::from("/repo")], false)
            .unwrap();
        (db, pid)
    }

    #[test]
    fn list_roles_empty_project() {
        let (db, pid) = setup_db_with_project("test");
        let roles = db.list_roles(pid).unwrap();
        assert!(roles.is_empty());
    }

    #[test]
    fn replace_and_list_roles() {
        let (db, pid) = setup_db_with_project("test");

        let roles = vec![
            RoleConfig {
                name: "developer".to_string(),
                description: "Full access".to_string(),
                permissions: RolePermissions::default(),
            },
            RoleConfig {
                name: "reviewer".to_string(),
                description: "Read-only review".to_string(),
                permissions: RolePermissions {
                    permission_mode: Some("plan".to_string()),
                    allowed_tools: vec!["Read".to_string(), "Bash(git:*)".to_string()],
                    disallowed_tools: vec!["Edit".to_string()],
                    tools: Some("default".to_string()),
                    append_system_prompt: Some("Be careful".to_string()),
                },
            },
        ];

        db.replace_roles(pid, &roles).unwrap();
        let loaded = db.list_roles(pid).unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "developer");
        assert_eq!(loaded[0].description, "Full access");

        assert_eq!(loaded[1].name, "reviewer");
        assert_eq!(
            loaded[1].permissions.permission_mode,
            Some("plan".to_string())
        );
        assert_eq!(
            loaded[1].permissions.allowed_tools,
            vec!["Read".to_string(), "Bash(git:*)".to_string()]
        );
        assert_eq!(
            loaded[1].permissions.disallowed_tools,
            vec!["Edit".to_string()]
        );
        assert_eq!(loaded[1].permissions.tools, Some("default".to_string()));
        assert_eq!(
            loaded[1].permissions.append_system_prompt,
            Some("Be careful".to_string())
        );
    }

    #[test]
    fn replace_roles_overwrites_existing() {
        let (db, pid) = setup_db_with_project("test");

        let initial = vec![RoleConfig {
            name: "old-role".to_string(),
            description: "old".to_string(),
            permissions: RolePermissions::default(),
        }];
        db.replace_roles(pid, &initial).unwrap();

        let updated = vec![RoleConfig {
            name: "new-role".to_string(),
            description: "new".to_string(),
            permissions: RolePermissions::default(),
        }];
        db.replace_roles(pid, &updated).unwrap();

        let loaded = db.list_roles(pid).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "new-role");
    }

    #[test]
    fn replace_roles_empty_clears_all() {
        let (db, pid) = setup_db_with_project("test");

        let roles = vec![RoleConfig {
            name: "dev".to_string(),
            description: String::new(),
            permissions: RolePermissions::default(),
        }];
        db.replace_roles(pid, &roles).unwrap();
        db.replace_roles(pid, &[]).unwrap();

        let loaded = db.list_roles(pid).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn list_all_roles_multiple_projects() {
        let db = Database::open_in_memory().unwrap();

        let pid1 = test_project_id("proj1");
        let pid2 = test_project_id("proj2");
        db.insert_project(pid1, "proj1", &[], false).unwrap();
        db.insert_project(pid2, "proj2", &[], false).unwrap();

        db.replace_roles(
            pid1,
            &[RoleConfig {
                name: "dev".to_string(),
                description: String::new(),
                permissions: RolePermissions::default(),
            }],
        )
        .unwrap();
        db.replace_roles(
            pid2,
            &[
                RoleConfig {
                    name: "reviewer".to_string(),
                    description: String::new(),
                    permissions: RolePermissions::default(),
                },
                RoleConfig {
                    name: "admin".to_string(),
                    description: String::new(),
                    permissions: RolePermissions::default(),
                },
            ],
        )
        .unwrap();

        let all = db.list_all_roles().unwrap();
        assert_eq!(all.get(&pid1).unwrap().len(), 1);
        assert_eq!(all.get(&pid2).unwrap().len(), 2);
    }

    #[test]
    fn list_all_roles_excludes_deleted_projects() {
        let db = Database::open_in_memory().unwrap();
        let pid = test_project_id("deleted");
        db.insert_project(pid, "deleted", &[], false).unwrap();
        db.replace_roles(
            pid,
            &[RoleConfig {
                name: "dev".to_string(),
                description: String::new(),
                permissions: RolePermissions::default(),
            }],
        )
        .unwrap();

        db.soft_delete_project(pid).unwrap();

        let all = db.list_all_roles().unwrap();
        assert!(all.is_empty());
    }

    #[test]
    fn csv_roundtrip() {
        assert_eq!(csv_to_vec(""), Vec::<String>::new());
        assert_eq!(csv_to_vec("Read"), vec!["Read".to_string()]);
        assert_eq!(
            csv_to_vec("Read,Bash(git:*)"),
            vec!["Read".to_string(), "Bash(git:*)".to_string()]
        );
        assert_eq!(vec_to_csv(&[]), "");
        assert_eq!(vec_to_csv(&["Read".to_string()]), "Read");
        assert_eq!(
            vec_to_csv(&["Read".to_string(), "Edit".to_string()]),
            "Read,Edit"
        );
    }
}
