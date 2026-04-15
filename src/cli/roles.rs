//! Role CRUD subcommands.

use std::path::PathBuf;

use clap::Subcommand;
use serde_json::Value;

use crate::session::{RoleConfig, RolePermissions};
use crate::storage::Database;

#[derive(Subcommand, Debug)]
pub enum Action {
    /// List all global roles.
    List,
    /// Atomically replace all global roles from a JSON file.
    ///
    /// The file must contain an array of role objects matching the MCP
    /// `set_roles` schema (see docs/MCP_ROLES.md).
    Set {
        /// Path to a JSON file containing a role array.
        #[arg(long)]
        file: PathBuf,
    },
}

pub fn run(action: Action, db: &Database) -> Result<Value, String> {
    match action {
        Action::List => {
            let roles = db
                .list_global_roles()
                .map_err(|e| format!("list_global_roles: {e}"))?;
            Ok(serde_json::to_value(roles).map_err(|e| e.to_string())?)
        }
        Action::Set { file } => {
            let content = super::read_file(&file)?;
            // Accept either `[RoleInput...]` or `{"roles":[RoleInput...]}`.
            let value: Value =
                serde_json::from_str(&content).map_err(|e| format!("Failed to parse JSON: {e}"))?;
            let arr = match &value {
                Value::Array(a) => a.clone(),
                Value::Object(o) => match o.get("roles") {
                    Some(Value::Array(a)) => a.clone(),
                    _ => return Err("JSON must be an array or {\"roles\":[...]}".into()),
                },
                _ => return Err("JSON must be an array or {\"roles\":[...]}".into()),
            };

            let roles: Vec<RoleConfig> = arr
                .into_iter()
                .map(|v| -> Result<RoleConfig, String> {
                    let name = v
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or("role.name required")?
                        .to_string();
                    let description = v
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let permission_mode = v
                        .get("permission_mode")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let allowed_tools = str_vec(v.get("allowed_tools"));
                    let disallowed_tools = str_vec(v.get("disallowed_tools"));
                    let tools = v.get("tools").and_then(Value::as_str).map(str::to_string);
                    let append_system_prompt = v
                        .get("append_system_prompt")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let env = str_map(v.get("env"));
                    Ok(RoleConfig {
                        name,
                        description,
                        permissions: RolePermissions {
                            permission_mode,
                            allowed_tools,
                            disallowed_tools,
                            tools,
                            append_system_prompt,
                            env,
                        },
                    })
                })
                .collect::<Result<_, _>>()?;
            db.replace_global_roles(&roles)
                .map_err(|e| format!("replace_global_roles: {e}"))?;
            Ok(serde_json::to_value(&roles).map_err(|e| e.to_string())?)
        }
    }
}

fn str_vec(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn str_map(v: Option<&Value>) -> std::collections::HashMap<String, String> {
    v.and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn list_returns_empty_array_when_no_roles() {
        let db = db();
        let v = run(Action::List, &db).unwrap();
        assert!(v.is_array(), "got {v}");
        assert_eq!(v.as_array().unwrap().len(), 0);
    }

    #[test]
    fn list_returns_seeded_role() {
        let db = db();
        let role = RoleConfig {
            name: "dev".into(),
            description: "developer".into(),
            permissions: RolePermissions {
                permission_mode: Some("plan".into()),
                ..RolePermissions::default()
            },
        };
        db.replace_global_roles(&[role]).unwrap();
        let v = run(Action::List, &db).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"].as_str(), Some("dev"));
        assert_eq!(arr[0]["description"].as_str(), Some("developer"));
    }
}
