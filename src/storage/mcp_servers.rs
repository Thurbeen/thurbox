use std::collections::HashMap;

use rusqlite::params;

use crate::session::McpServerConfig;
use crate::sync::current_time_millis;

use super::Database;

/// Where a resolved MCP server came from in the merged view.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpServerSource {
    Registered,
}

/// Parse a row with columns (server_name, command, args, env) into McpServerConfig.
fn row_to_mcp_server_config(row: &rusqlite::Row<'_>) -> rusqlite::Result<McpServerConfig> {
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
}

impl Database {
    /// List all global MCP servers.
    pub fn list_global_mcp_servers(&self) -> rusqlite::Result<Vec<McpServerConfig>> {
        let mut stmt = self.conn.prepare(
            "SELECT server_name, command, args, env \
             FROM mcp_servers ORDER BY server_name",
        )?;

        let servers = stmt
            .query_map([], row_to_mcp_server_config)?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(servers)
    }

    /// Atomically replace all global MCP servers (delete existing + insert new).
    pub fn replace_global_mcp_servers(&self, servers: &[McpServerConfig]) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;

        self.conn.execute("DELETE FROM mcp_servers", [])?;

        for server in servers {
            self.conn.execute(
                "INSERT INTO mcp_servers \
                 (server_name, command, args, env, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
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

    /// Insert or update a single global MCP server.
    pub fn upsert_global_mcp_server(&self, server: &McpServerConfig) -> rusqlite::Result<()> {
        let now = current_time_millis() as i64;
        self.conn.execute(
            "INSERT INTO mcp_servers \
             (server_name, command, args, env, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(server_name) DO UPDATE SET \
                 command = excluded.command, \
                 args = excluded.args, \
                 env = excluded.env, \
                 updated_at = excluded.updated_at",
            params![
                server.name,
                server.command,
                args_to_str(&server.args),
                env_to_str(&server.env),
                now,
                now,
            ],
        )?;
        Ok(())
    }

    /// Delete a single global MCP server by name. Returns true if it existed.
    pub fn delete_global_mcp_server(&self, name: &str) -> rusqlite::Result<bool> {
        let count = self.conn.execute(
            "DELETE FROM mcp_servers WHERE server_name = ?1",
            params![name],
        )?;
        Ok(count > 0)
    }

    /// All registered MCP servers as `(server, source)` pairs.
    pub fn list_effective_mcp_servers(
        &self,
    ) -> rusqlite::Result<Vec<(McpServerConfig, McpServerSource)>> {
        Ok(self
            .list_global_mcp_servers()?
            .into_iter()
            .map(|s| (s, McpServerSource::Registered))
            .collect())
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

    use super::*;

    #[test]
    fn list_global_mcp_servers_empty() {
        let db = Database::open_in_memory().unwrap();
        let servers = db.list_global_mcp_servers().unwrap();
        assert!(servers.is_empty());
    }

    #[test]
    fn replace_and_list_global_mcp_servers() {
        let db = Database::open_in_memory().unwrap();
        let servers = vec![
            McpServerConfig {
                name: "filesystem".to_string(),
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "server-filesystem".to_string()],
                env: HashMap::new(),
            },
            McpServerConfig {
                name: "github".to_string(),
                command: "npx".to_string(),
                args: vec!["-y".to_string(), "server-github".to_string()],
                env: HashMap::from([("TOKEN".to_string(), "ghp_xxx".to_string())]),
            },
        ];

        db.replace_global_mcp_servers(&servers).unwrap();
        let loaded = db.list_global_mcp_servers().unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "filesystem");
        assert_eq!(loaded[1].name, "github");
        assert_eq!(loaded[1].env.get("TOKEN"), Some(&"ghp_xxx".to_string()));
    }

    #[test]
    fn replace_global_mcp_servers_overwrites() {
        let db = Database::open_in_memory().unwrap();
        db.replace_global_mcp_servers(&[McpServerConfig {
            name: "old".to_string(),
            command: "old-cmd".to_string(),
            args: vec![],
            env: HashMap::new(),
        }])
        .unwrap();

        db.replace_global_mcp_servers(&[McpServerConfig {
            name: "new".to_string(),
            command: "new-cmd".to_string(),
            args: vec![],
            env: HashMap::new(),
        }])
        .unwrap();

        let loaded = db.list_global_mcp_servers().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "new");
    }

    #[test]
    fn upsert_global_mcp_server_insert_and_update() {
        let db = Database::open_in_memory().unwrap();
        let server = McpServerConfig {
            name: "test-server".to_string(),
            command: "cmd1".to_string(),
            args: vec![],
            env: HashMap::new(),
        };
        db.upsert_global_mcp_server(&server).unwrap();

        let loaded = db.list_global_mcp_servers().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].command, "cmd1");

        let updated = McpServerConfig {
            name: "test-server".to_string(),
            command: "cmd2".to_string(),
            args: vec!["--flag".to_string()],
            env: HashMap::new(),
        };
        db.upsert_global_mcp_server(&updated).unwrap();

        let loaded = db.list_global_mcp_servers().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].command, "cmd2");
        assert_eq!(loaded[0].args, vec!["--flag".to_string()]);
    }

    #[test]
    fn delete_global_mcp_server() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_global_mcp_server(&McpServerConfig {
            name: "to-delete".to_string(),
            command: "cmd".to_string(),
            args: vec![],
            env: HashMap::new(),
        })
        .unwrap();

        assert!(db.delete_global_mcp_server("to-delete").unwrap());
        assert!(!db.delete_global_mcp_server("to-delete").unwrap());
        assert!(db.list_global_mcp_servers().unwrap().is_empty());
    }

    #[test]
    fn args_env_roundtrip() {
        assert_eq!(str_to_args(""), Vec::<String>::new());
        assert_eq!(str_to_args("one"), vec!["one".to_string()]);
        assert_eq!(args_to_str(&[]), "");
        assert_eq!(args_to_str(&["a".to_string()]), "a");

        assert_eq!(str_to_env(""), HashMap::new());
        assert_eq!(
            str_to_env("KEY=VALUE"),
            HashMap::from([("KEY".to_string(), "VALUE".to_string())])
        );
        assert_eq!(env_to_str(&HashMap::new()), "");
    }
}
