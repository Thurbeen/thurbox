use std::collections::HashMap;

use rusqlite::params;

use crate::project::ProjectId;
use crate::session::McpServerConfig;
use crate::sync::current_time_millis;

use super::Database;

impl Database {
    /// List all MCP servers for a specific project.
    pub fn list_mcp_servers(
        &self,
        project_id: ProjectId,
    ) -> rusqlite::Result<Vec<McpServerConfig>> {
        let id_str = project_id.to_string();
        let mut stmt = self.conn.prepare(
            "SELECT server_name, command, args, env \
             FROM project_mcp_servers WHERE project_id = ?1 ORDER BY server_name",
        )?;

        let servers = stmt
            .query_map(params![id_str], |row| {
                let name: String = row.get(0)?;
                let command: String = row.get(1)?;
                let args_str: String = row.get(2)?;
                let env_str: String = row.get(3)?;

                Ok(McpServerConfig {
                    name,
                    command,
                    args: str_to_args(&args_str),
                    env: str_to_env(&env_str),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(servers)
    }

    /// List MCP servers for all active projects, keyed by project ID.
    pub fn list_all_mcp_servers(
        &self,
    ) -> rusqlite::Result<HashMap<ProjectId, Vec<McpServerConfig>>> {
        let mut stmt = self.conn.prepare(
            "SELECT ms.project_id, ms.server_name, ms.command, ms.args, ms.env \
             FROM project_mcp_servers ms \
             INNER JOIN projects p ON p.id = ms.project_id AND p.deleted_at IS NULL \
             ORDER BY ms.project_id, ms.server_name",
        )?;

        let mut map: HashMap<ProjectId, Vec<McpServerConfig>> = HashMap::new();

        let rows = stmt.query_map([], |row| {
            let pid_str: String = row.get(0)?;
            let name: String = row.get(1)?;
            let command: String = row.get(2)?;
            let args_str: String = row.get(3)?;
            let env_str: String = row.get(4)?;

            Ok((
                pid_str,
                McpServerConfig {
                    name,
                    command,
                    args: str_to_args(&args_str),
                    env: str_to_env(&env_str),
                },
            ))
        })?;

        for row in rows {
            let (pid_str, server) = row?;
            let pid = pid_str
                .parse::<uuid::Uuid>()
                .map(ProjectId::from_uuid)
                .unwrap_or_default();
            map.entry(pid).or_default().push(server);
        }

        Ok(map)
    }

    /// Replace all MCP servers for a project (delete existing + insert new).
    pub fn replace_mcp_servers(
        &self,
        project_id: ProjectId,
        servers: &[McpServerConfig],
    ) -> rusqlite::Result<()> {
        let id_str = project_id.to_string();
        let now = current_time_millis() as i64;

        self.conn.execute(
            "DELETE FROM project_mcp_servers WHERE project_id = ?1",
            params![id_str],
        )?;

        for server in servers {
            self.conn.execute(
                "INSERT INTO project_mcp_servers \
                 (project_id, server_name, command, args, env, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id_str,
                    server.name,
                    server.command,
                    args_to_str(&server.args),
                    env_to_str(&server.env),
                    now,
                    now,
                ],
            )?;
        }

        Ok(())
    }
}

/// Serialize args as newline-separated (args may contain commas).
fn args_to_str(args: &[String]) -> String {
    args.join("\n")
}

/// Deserialize newline-separated args.
fn str_to_args(s: &str) -> Vec<String> {
    if s.is_empty() {
        Vec::new()
    } else {
        s.split('\n').map(|s| s.to_string()).collect()
    }
}

/// Serialize env as newline-separated `KEY=VALUE` pairs.
fn env_to_str(env: &HashMap<String, String>) -> String {
    let mut pairs: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    pairs.sort(); // deterministic ordering
    pairs.join("\n")
}

/// Deserialize newline-separated `KEY=VALUE` pairs.
fn str_to_env(s: &str) -> HashMap<String, String> {
    if s.is_empty() {
        HashMap::new()
    } else {
        s.split('\n')
            .filter_map(|line| {
                let (k, v) = line.split_once('=')?;
                Some((k.to_string(), v.to_string()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
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
    fn list_mcp_servers_empty_project() {
        let (db, pid) = setup_db_with_project("test");
        let servers = db.list_mcp_servers(pid).unwrap();
        assert!(servers.is_empty());
    }

    #[test]
    fn replace_and_list_mcp_servers() {
        let (db, pid) = setup_db_with_project("test");

        let servers = vec![
            McpServerConfig {
                name: "filesystem".to_string(),
                command: "npx".to_string(),
                args: vec![
                    "-y".to_string(),
                    "@modelcontextprotocol/server-filesystem".to_string(),
                ],
                env: HashMap::new(),
            },
            McpServerConfig {
                name: "github".to_string(),
                command: "npx".to_string(),
                args: vec![
                    "-y".to_string(),
                    "@modelcontextprotocol/server-github".to_string(),
                ],
                env: HashMap::from([("GITHUB_TOKEN".to_string(), "ghp_xxx".to_string())]),
            },
        ];

        db.replace_mcp_servers(pid, &servers).unwrap();
        let loaded = db.list_mcp_servers(pid).unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "filesystem");
        assert_eq!(loaded[0].command, "npx");
        assert_eq!(loaded[0].args.len(), 2);
        assert!(loaded[0].env.is_empty());

        assert_eq!(loaded[1].name, "github");
        assert_eq!(
            loaded[1].env.get("GITHUB_TOKEN"),
            Some(&"ghp_xxx".to_string())
        );
    }

    #[test]
    fn replace_mcp_servers_overwrites_existing() {
        let (db, pid) = setup_db_with_project("test");

        let initial = vec![McpServerConfig {
            name: "old-server".to_string(),
            command: "old-cmd".to_string(),
            args: vec![],
            env: HashMap::new(),
        }];
        db.replace_mcp_servers(pid, &initial).unwrap();

        let updated = vec![McpServerConfig {
            name: "new-server".to_string(),
            command: "new-cmd".to_string(),
            args: vec!["--flag".to_string()],
            env: HashMap::new(),
        }];
        db.replace_mcp_servers(pid, &updated).unwrap();

        let loaded = db.list_mcp_servers(pid).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "new-server");
    }

    #[test]
    fn replace_mcp_servers_empty_clears_all() {
        let (db, pid) = setup_db_with_project("test");

        let servers = vec![McpServerConfig {
            name: "server".to_string(),
            command: "cmd".to_string(),
            args: vec![],
            env: HashMap::new(),
        }];
        db.replace_mcp_servers(pid, &servers).unwrap();
        db.replace_mcp_servers(pid, &[]).unwrap();

        let loaded = db.list_mcp_servers(pid).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn list_all_mcp_servers_multiple_projects() {
        let db = Database::open_in_memory().unwrap();

        let pid1 = test_project_id("proj1");
        let pid2 = test_project_id("proj2");
        db.insert_project(pid1, "proj1", &[], false).unwrap();
        db.insert_project(pid2, "proj2", &[], false).unwrap();

        db.replace_mcp_servers(
            pid1,
            &[McpServerConfig {
                name: "s1".to_string(),
                command: "cmd1".to_string(),
                args: vec![],
                env: HashMap::new(),
            }],
        )
        .unwrap();
        db.replace_mcp_servers(
            pid2,
            &[
                McpServerConfig {
                    name: "s2a".to_string(),
                    command: "cmd2a".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                },
                McpServerConfig {
                    name: "s2b".to_string(),
                    command: "cmd2b".to_string(),
                    args: vec![],
                    env: HashMap::new(),
                },
            ],
        )
        .unwrap();

        let all = db.list_all_mcp_servers().unwrap();
        assert_eq!(all.get(&pid1).unwrap().len(), 1);
        assert_eq!(all.get(&pid2).unwrap().len(), 2);
    }

    #[test]
    fn list_all_mcp_servers_excludes_deleted_projects() {
        let db = Database::open_in_memory().unwrap();
        let pid = test_project_id("deleted");
        db.insert_project(pid, "deleted", &[], false).unwrap();
        db.replace_mcp_servers(
            pid,
            &[McpServerConfig {
                name: "s".to_string(),
                command: "c".to_string(),
                args: vec![],
                env: HashMap::new(),
            }],
        )
        .unwrap();

        db.soft_delete_project(pid).unwrap();

        let all = db.list_all_mcp_servers().unwrap();
        assert!(all.is_empty());
    }

    #[test]
    fn args_env_roundtrip() {
        assert_eq!(str_to_args(""), Vec::<String>::new());
        assert_eq!(str_to_args("one"), vec!["one".to_string()]);
        assert_eq!(
            str_to_args("one\ntwo\nthree"),
            vec!["one".to_string(), "two".to_string(), "three".to_string()]
        );
        assert_eq!(args_to_str(&[]), "");
        assert_eq!(args_to_str(&["a".to_string()]), "a");
        assert_eq!(args_to_str(&["a".to_string(), "b,c".to_string()]), "a\nb,c");

        assert_eq!(str_to_env(""), HashMap::new());
        assert_eq!(
            str_to_env("KEY=VALUE"),
            HashMap::from([("KEY".to_string(), "VALUE".to_string())])
        );
        assert_eq!(
            str_to_env("A=1\nB=2"),
            HashMap::from([
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "2".to_string()),
            ])
        );
        assert_eq!(env_to_str(&HashMap::new()), "");
    }

    #[test]
    fn env_value_with_equals_sign() {
        let env = HashMap::from([("PATH".to_string(), "/usr/bin:/usr/local/bin".to_string())]);
        let serialized = env_to_str(&env);
        let deserialized = str_to_env(&serialized);
        assert_eq!(deserialized, env);
    }

    #[test]
    fn env_roundtrip_multiple_entries() {
        let env = HashMap::from([
            ("API_KEY".to_string(), "sk-abc123".to_string()),
            ("DEBUG".to_string(), "1".to_string()),
            ("URL".to_string(), "https://example.com?a=1&b=2".to_string()),
        ]);
        let serialized = env_to_str(&env);
        let deserialized = str_to_env(&serialized);
        assert_eq!(deserialized, env);
    }

    #[test]
    fn args_roundtrip_preserves_commas() {
        let args = vec![
            "-y".to_string(),
            "--config=a,b,c".to_string(),
            "path with spaces".to_string(),
        ];
        let serialized = args_to_str(&args);
        let deserialized = str_to_args(&serialized);
        assert_eq!(deserialized, args);
    }
}
