//! MCP server CRUD subcommands.

use std::path::PathBuf;

use clap::Subcommand;
use serde_json::Value;

use crate::session::McpServerConfig;
use crate::storage::Database;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// List all global MCP servers.
    List,
    /// Atomically replace all global MCP servers from a JSON file.
    Set {
        /// Path to a JSON file. Accepts `[server...]` or `{"servers":[...]}`.
        #[arg(long)]
        file: PathBuf,
    },
}

pub fn run(action: Action, db: &Database) -> Result<Value, String> {
    match action {
        Action::List => {
            let servers = db
                .list_global_mcp_servers()
                .map_err(|e| format!("list_global_mcp_servers: {e}"))?;
            Ok(serde_json::to_value(servers).map_err(|e| e.to_string())?)
        }
        Action::Set { file } => {
            let content = super::read_file(&file)?;
            let value: Value =
                serde_json::from_str(&content).map_err(|e| format!("Failed to parse JSON: {e}"))?;
            let arr = match &value {
                Value::Array(a) => a.clone(),
                Value::Object(o) => match o.get("servers") {
                    Some(Value::Array(a)) => a.clone(),
                    _ => return Err("JSON must be an array or {\"servers\":[...]}".into()),
                },
                _ => return Err("JSON must be an array or {\"servers\":[...]}".into()),
            };

            let servers: Vec<McpServerConfig> = arr
                .into_iter()
                .map(|v| -> Result<McpServerConfig, String> {
                    let name = v
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or("server.name required")?
                        .to_string();
                    let command = v
                        .get("command")
                        .and_then(Value::as_str)
                        .ok_or("server.command required")?
                        .to_string();
                    let args = v
                        .get("args")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    let env = v
                        .get("env")
                        .and_then(Value::as_object)
                        .map(|o| {
                            o.iter()
                                .filter_map(|(k, val)| {
                                    val.as_str().map(|s| (k.clone(), s.to_string()))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Ok(McpServerConfig {
                        name,
                        command,
                        args,
                        env,
                    })
                })
                .collect::<Result<_, _>>()?;
            db.replace_global_mcp_servers(&servers)
                .map_err(|e| format!("replace_global_mcp_servers: {e}"))?;
            Ok(serde_json::to_value(&servers).map_err(|e| e.to_string())?)
        }
    }
}
