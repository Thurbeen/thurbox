use std::collections::HashMap;

use rusqlite::params;

use crate::session::{RoleConfig, RolePermissions};
use crate::sync::current_time_millis;

use super::Database;

/// Parse a row with columns: role_name, description, permission_mode, allowed_tools,
/// disallowed_tools, tools, append_system_prompt, env → RoleConfig.
fn row_to_role_config(row: &rusqlite::Row<'_>) -> rusqlite::Result<RoleConfig> {
    let name: String = row.get(0)?;
    let description: String = row.get(1)?;
    let permission_mode: Option<String> = row.get(2)?;
    let allowed_csv: String = row.get(3)?;
    let disallowed_csv: String = row.get(4)?;
    let tools: Option<String> = row.get(5)?;
    let append_system_prompt: Option<String> = row.get(6)?;
    let env_json: String = row.get(7)?;

    Ok(RoleConfig {
        name,
        description,
        permissions: RolePermissions {
            permission_mode,
            allowed_tools: csv_to_vec(&allowed_csv),
            disallowed_tools: csv_to_vec(&disallowed_csv),
            tools,
            append_system_prompt,
            env: json_to_env(&env_json),
        },
    })
}

impl Database {
    /// List all global roles.
    pub fn list_global_roles(&self) -> rusqlite::Result<Vec<RoleConfig>> {
        let mut stmt = self.conn.prepare(
            "SELECT role_name, description, permission_mode, allowed_tools, \
             disallowed_tools, tools, append_system_prompt, env \
             FROM roles ORDER BY role_name",
        )?;

        let roles = stmt
            .query_map([], row_to_role_config)?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(roles)
    }

    /// Atomically replace all global roles (delete existing + insert new).
    pub fn replace_global_roles(&self, roles: &[RoleConfig]) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;

        self.conn.execute("DELETE FROM roles", [])?;

        for role in roles {
            self.conn.execute(
                "INSERT INTO roles \
                 (role_name, description, permission_mode, \
                  allowed_tools, disallowed_tools, tools, append_system_prompt, env, \
                  created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    role.name,
                    role.description,
                    role.permissions.permission_mode,
                    vec_to_csv(&role.permissions.allowed_tools),
                    vec_to_csv(&role.permissions.disallowed_tools),
                    role.permissions.tools,
                    role.permissions.append_system_prompt,
                    env_to_json(&role.permissions.env),
                    now,
                    now,
                ],
            )?;
        }

        Ok(())
    }

    /// Insert or update a single global role.
    pub fn upsert_global_role(&self, role: &RoleConfig) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;

        self.conn.execute(
            "INSERT INTO roles \
             (role_name, description, permission_mode, \
              allowed_tools, disallowed_tools, tools, append_system_prompt, env, \
              created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
             ON CONFLICT(role_name) DO UPDATE SET \
              description = excluded.description, \
              permission_mode = excluded.permission_mode, \
              allowed_tools = excluded.allowed_tools, \
              disallowed_tools = excluded.disallowed_tools, \
              tools = excluded.tools, \
              append_system_prompt = excluded.append_system_prompt, \
              env = excluded.env, \
              updated_at = excluded.updated_at",
            params![
                role.name,
                role.description,
                role.permissions.permission_mode,
                vec_to_csv(&role.permissions.allowed_tools),
                vec_to_csv(&role.permissions.disallowed_tools),
                role.permissions.tools,
                role.permissions.append_system_prompt,
                env_to_json(&role.permissions.env),
                now,
                now,
            ],
        )?;

        Ok(())
    }

    /// Delete a single global role by name.
    pub fn delete_global_role(&self, role_name: &str) -> rusqlite::Result<bool> {
        let rows = self
            .conn
            .execute("DELETE FROM roles WHERE role_name = ?1", params![role_name])?;
        Ok(rows > 0)
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

/// Deserialize a JSON string to a HashMap of environment variables.
fn json_to_env(json: &str) -> HashMap<String, String> {
    if json.is_empty() {
        HashMap::new()
    } else {
        serde_json::from_str(json).unwrap_or_default()
    }
}

/// Serialize a HashMap of environment variables to a JSON string.
fn env_to_json(env: &HashMap<String, String>) -> String {
    if env.is_empty() {
        String::new()
    } else {
        serde_json::to_string(env).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_global_roles_empty() {
        let db = Database::open_in_memory().unwrap();
        let roles = db.list_global_roles().unwrap();
        assert!(roles.is_empty());
    }

    #[test]
    fn replace_and_list_global_roles() {
        let db = Database::open_in_memory().unwrap();

        let roles = vec![
            RoleConfig {
                name: "developer".to_string(),
                description: "Full access".to_string(),
                permissions: RolePermissions::default(),
            },
            RoleConfig {
                name: "reviewer".to_string(),
                description: "Read-only".to_string(),
                permissions: RolePermissions {
                    permission_mode: Some("plan".to_string()),
                    allowed_tools: vec!["Read".to_string()],
                    ..RolePermissions::default()
                },
            },
        ];

        db.replace_global_roles(&roles).unwrap();
        let loaded = db.list_global_roles().unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "developer");
        assert_eq!(loaded[1].name, "reviewer");
        assert_eq!(
            loaded[1].permissions.permission_mode,
            Some("plan".to_string())
        );
    }

    #[test]
    fn replace_global_roles_overwrites() {
        let db = Database::open_in_memory().unwrap();

        db.replace_global_roles(&[RoleConfig {
            name: "old".to_string(),
            description: String::new(),
            permissions: RolePermissions::default(),
        }])
        .unwrap();

        db.replace_global_roles(&[RoleConfig {
            name: "new".to_string(),
            description: String::new(),
            permissions: RolePermissions::default(),
        }])
        .unwrap();

        let loaded = db.list_global_roles().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "new");
    }

    #[test]
    fn upsert_global_role_insert_and_update() {
        let db = Database::open_in_memory().unwrap();

        let role = RoleConfig {
            name: "dev".to_string(),
            description: "original".to_string(),
            permissions: RolePermissions::default(),
        };
        db.upsert_global_role(&role).unwrap();

        let loaded = db.list_global_roles().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].description, "original");

        let updated = RoleConfig {
            name: "dev".to_string(),
            description: "updated".to_string(),
            permissions: RolePermissions::default(),
        };
        db.upsert_global_role(&updated).unwrap();

        let loaded = db.list_global_roles().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].description, "updated");
    }

    #[test]
    fn delete_global_role() {
        let db = Database::open_in_memory().unwrap();

        db.upsert_global_role(&RoleConfig {
            name: "dev".to_string(),
            description: String::new(),
            permissions: RolePermissions::default(),
        })
        .unwrap();

        assert!(db.delete_global_role("dev").unwrap());
        assert!(!db.delete_global_role("dev").unwrap());

        let loaded = db.list_global_roles().unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn global_role_env_preserved() {
        let db = Database::open_in_memory().unwrap();

        let env = HashMap::from([
            ("API_KEY".to_string(), "sk-secret".to_string()),
            ("DEBUG".to_string(), "1".to_string()),
        ]);
        db.upsert_global_role(&RoleConfig {
            name: "with-env".to_string(),
            description: String::new(),
            permissions: RolePermissions {
                env: env.clone(),
                ..RolePermissions::default()
            },
        })
        .unwrap();

        let loaded = db.list_global_roles().unwrap();
        assert_eq!(loaded[0].permissions.env, env);
    }

    #[test]
    fn json_env_roundtrip() {
        let env = HashMap::from([
            ("KEY".to_string(), "value".to_string()),
            ("EMPTY".to_string(), String::new()),
        ]);
        let json = env_to_json(&env);
        let parsed = json_to_env(&json);
        assert_eq!(parsed, env);
    }

    #[test]
    fn json_env_empty_roundtrip() {
        assert_eq!(env_to_json(&HashMap::new()), "");
        assert!(json_to_env("").is_empty());
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
