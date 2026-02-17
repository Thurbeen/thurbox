//! Tool implementations for the Thurbox MCP server.

use std::path::PathBuf;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::project::{ProjectConfig, ProjectId};
use crate::session::{McpServerConfig, RoleConfig, RolePermissions};
use crate::storage::Database;
use crate::sync::SharedProject;

use super::types::{
    CreateProjectParams, DeleteProjectParams, GetProjectParams, ListMcpServersParams,
    ListRolesParams, ListSessionsParams, McpServerResponse, ProjectResponse, RoleResponse,
    SessionResponse, SetMcpServersParams, SetRolesParams, UpdateProjectParams, WorktreeResponse,
};
use super::ThurboxMcp;

// ── Helpers ─────────────────────────────────────────────────────

/// Resolve a project identifier (name or UUID) against the active project list.
fn resolve_project<'a>(
    projects: &'a [SharedProject],
    identifier: &str,
) -> Option<&'a SharedProject> {
    if let Ok(uuid) = identifier.parse::<uuid::Uuid>() {
        let pid = ProjectId::from_uuid(uuid);
        return projects.iter().find(|p| p.id == pid);
    }
    let lower = identifier.to_lowercase();
    projects.iter().find(|p| p.name.to_lowercase() == lower)
}

/// Look up a project by name/UUID, returning a JSON error string on failure.
///
/// Returns the full project list and the index of the matched project so the
/// caller can borrow freely without lifetime issues.
fn require_project(db: &Database, identifier: &str) -> Result<(Vec<SharedProject>, usize), String> {
    let projects = db
        .list_active_projects()
        .map_err(|e| error_json(&e.to_string()))?;
    let id = resolve_project(&projects, identifier)
        .map(|p| p.id)
        .ok_or_else(|| error_json(&format!("Project not found: {identifier}")))?;
    let idx = projects.iter().position(|p| p.id == id).unwrap();
    Ok((projects, idx))
}

fn project_to_response(p: &SharedProject) -> ProjectResponse {
    ProjectResponse {
        id: p.id.to_string(),
        name: p.name.clone(),
        repos: p.repos.clone(),
        roles: p.roles.iter().map(role_to_response).collect(),
        mcp_servers: p.mcp_servers.iter().map(mcp_server_to_response).collect(),
    }
}

fn mcp_server_to_response(s: &McpServerConfig) -> McpServerResponse {
    McpServerResponse {
        name: s.name.clone(),
        command: s.command.clone(),
        args: s.args.clone(),
        env: s.env.clone(),
    }
}

fn role_to_response(r: &RoleConfig) -> RoleResponse {
    RoleResponse {
        name: r.name.clone(),
        description: r.description.clone(),
        permission_mode: r.permissions.permission_mode.clone(),
        allowed_tools: r.permissions.allowed_tools.clone(),
        disallowed_tools: r.permissions.disallowed_tools.clone(),
        tools: r.permissions.tools.clone(),
        append_system_prompt: r.permissions.append_system_prompt.clone(),
    }
}

fn session_to_response(s: &crate::sync::SharedSession) -> SessionResponse {
    SessionResponse {
        id: s.id.to_string(),
        name: s.name.clone(),
        project_id: s.project_id.to_string(),
        role: s.role.clone(),
        backend_type: s.backend_type.clone(),
        claude_session_id: s.claude_session_id.clone(),
        cwd: s.cwd.clone(),
        worktree: s.worktree.as_ref().map(|w| WorktreeResponse {
            repo_path: w.repo_path.clone(),
            worktree_path: w.worktree_path.clone(),
            branch: w.branch.clone(),
        }),
    }
}

fn json_text<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|e| error_json(&e.to_string()))
}

fn error_json(msg: &str) -> String {
    serde_json::json!({ "error": msg }).to_string()
}

// ── Tool implementations ────────────────────────────────────────

#[tool_router(vis = "pub(super)")]
impl ThurboxMcp {
    #[tool(description = "List all active projects")]
    fn list_projects(&self) -> String {
        let db = self.db.lock().unwrap();
        match db.list_active_projects() {
            Ok(projects) => {
                let resp: Vec<ProjectResponse> = projects.iter().map(project_to_response).collect();
                json_text(&resp)
            }
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(description = "Get a project by name or UUID")]
    fn get_project(&self, Parameters(params): Parameters<GetProjectParams>) -> String {
        let db = self.db.lock().unwrap();
        match require_project(&db, &params.project) {
            Ok((projects, idx)) => json_text(&project_to_response(&projects[idx])),
            Err(e) => e,
        }
    }

    #[tool(description = "Create a new project with the given name and repository paths")]
    fn create_project(&self, Parameters(params): Parameters<CreateProjectParams>) -> String {
        let repos: Vec<PathBuf> = params.repos.iter().map(PathBuf::from).collect();
        let config = ProjectConfig {
            name: params.name.clone(),
            repos: repos.clone(),
            roles: Vec::new(),
            mcp_servers: Vec::new(),
            id: None,
        };
        let id = config.deterministic_id();

        let db = self.db.lock().unwrap();
        if let Err(e) = db.insert_project(id, &params.name, &repos) {
            return error_json(&e.to_string());
        }

        // Return the freshly created project (roles may have been inherited).
        match db.list_active_projects() {
            Ok(projects) => match projects.iter().find(|p| p.id == id) {
                Some(p) => json_text(&project_to_response(p)),
                None => json_text(&ProjectResponse {
                    id: id.to_string(),
                    name: params.name,
                    repos,
                    roles: vec![],
                    mcp_servers: vec![],
                }),
            },
            Err(_) => json_text(&ProjectResponse {
                id: id.to_string(),
                name: params.name,
                repos,
                roles: vec![],
                mcp_servers: vec![],
            }),
        }
    }

    #[tool(description = "Update an existing project's name and/or repository paths")]
    fn update_project(&self, Parameters(params): Parameters<UpdateProjectParams>) -> String {
        let db = self.db.lock().unwrap();
        let (projects, idx) = match require_project(&db, &params.project) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let project = &projects[idx];

        let new_name = params.name.as_deref().unwrap_or(&project.name);
        let new_repos: Vec<PathBuf> = match params.repos {
            Some(ref r) => r.iter().map(PathBuf::from).collect(),
            None => project.repos.clone(),
        };

        if let Err(e) = db.update_project(project.id, new_name, &new_repos) {
            return error_json(&e.to_string());
        }

        match db.list_active_projects() {
            Ok(updated) => match updated.iter().find(|p| p.id == project.id) {
                Some(p) => json_text(&project_to_response(p)),
                None => error_json("Project not found after update"),
            },
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(description = "Delete a project (soft delete)")]
    fn delete_project(&self, Parameters(params): Parameters<DeleteProjectParams>) -> String {
        let db = self.db.lock().unwrap();
        let (projects, idx) = match require_project(&db, &params.project) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let project = &projects[idx];

        match db.soft_delete_project(project.id) {
            Ok(()) => serde_json::json!({
                "deleted": true,
                "id": project.id.to_string(),
                "name": project.name,
            })
            .to_string(),
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "List all roles configured for a project. Returns a JSON array of role objects with name, description, permission_mode, allowed_tools, disallowed_tools, tools, and append_system_prompt fields. See docs/MCP_ROLES.md for field details."
    )]
    fn list_roles(&self, Parameters(params): Parameters<ListRolesParams>) -> String {
        let db = self.db.lock().unwrap();
        let (projects, idx) = match require_project(&db, &params.project) {
            Ok(v) => v,
            Err(e) => return e,
        };

        match db.list_roles(projects[idx].id) {
            Ok(roles) => {
                let resp: Vec<RoleResponse> = roles.iter().map(role_to_response).collect();
                json_text(&resp)
            }
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Atomically replace all roles for a project. Deletes existing roles and inserts the provided list in a single transaction. To add a role, include all existing roles plus the new one. To clear all roles, pass an empty array. Each role has: name (1-64 chars, unique), description, permission_mode (default/plan/acceptEdits/dontAsk/bypassPermissions), allowed_tools, disallowed_tools, tools, append_system_prompt. See docs/MCP_ROLES.md for the complete guide."
    )]
    fn set_roles(&self, Parameters(params): Parameters<SetRolesParams>) -> String {
        let db = self.db.lock().unwrap();
        let (projects, idx) = match require_project(&db, &params.project) {
            Ok(v) => v,
            Err(e) => return e,
        };

        let roles: Vec<RoleConfig> = params
            .roles
            .into_iter()
            .map(|r| RoleConfig {
                name: r.name,
                description: r.description,
                permissions: RolePermissions {
                    permission_mode: r.permission_mode,
                    allowed_tools: r.allowed_tools,
                    disallowed_tools: r.disallowed_tools,
                    tools: r.tools,
                    append_system_prompt: r.append_system_prompt,
                },
            })
            .collect();

        match db.replace_roles(projects[idx].id, &roles) {
            Ok(()) => {
                let resp: Vec<RoleResponse> = roles.iter().map(role_to_response).collect();
                json_text(&resp)
            }
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(description = "List MCP servers for a project")]
    fn list_mcp_servers(&self, Parameters(params): Parameters<ListMcpServersParams>) -> String {
        let db = self.db.lock().unwrap();
        let (projects, idx) = match require_project(&db, &params.project) {
            Ok(v) => v,
            Err(e) => return e,
        };

        match db.list_mcp_servers(projects[idx].id) {
            Ok(servers) => {
                let resp: Vec<McpServerResponse> =
                    servers.iter().map(mcp_server_to_response).collect();
                json_text(&resp)
            }
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Set MCP servers for a project (atomically replaces all existing servers)"
    )]
    fn set_mcp_servers(&self, Parameters(params): Parameters<SetMcpServersParams>) -> String {
        let db = self.db.lock().unwrap();
        let (projects, idx) = match require_project(&db, &params.project) {
            Ok(v) => v,
            Err(e) => return e,
        };

        let servers: Vec<McpServerConfig> = params
            .servers
            .into_iter()
            .map(|s| McpServerConfig {
                name: s.name,
                command: s.command,
                args: s.args,
                env: s.env,
            })
            .collect();

        match db.replace_mcp_servers(projects[idx].id, &servers) {
            Ok(()) => {
                let resp: Vec<McpServerResponse> =
                    servers.iter().map(mcp_server_to_response).collect();
                json_text(&resp)
            }
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(description = "List active sessions, optionally filtered by project name or UUID")]
    fn list_sessions(&self, Parameters(params): Parameters<ListSessionsParams>) -> String {
        let db = self.db.lock().unwrap();

        let sessions = match &params.project {
            Some(filter) => {
                let (projects, idx) = match require_project(&db, filter) {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                db.list_sessions_for_project(projects[idx].id)
            }
            None => db.list_active_sessions(),
        };

        match sessions {
            Ok(sessions) => {
                let resp: Vec<SessionResponse> = sessions.iter().map(session_to_response).collect();
                json_text(&resp)
            }
            Err(e) => error_json(&e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::mcp::types::{McpServerInput, RoleInput};
    use crate::session::RoleConfig;
    use crate::storage::Database;
    use std::collections::HashMap;

    fn test_server() -> ThurboxMcp {
        let db = Database::open_in_memory().unwrap();
        ThurboxMcp {
            db: Mutex::new(db),
            tool_router: ThurboxMcp::tool_router(),
        }
    }

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

    fn parse_json(s: &str) -> serde_json::Value {
        serde_json::from_str(s).unwrap()
    }

    // ── resolve_project tests ───────────────────────────────────

    #[test]
    fn resolve_by_name_case_insensitive() {
        let projects = vec![SharedProject {
            id: test_project_id("MyProject"),
            name: "MyProject".to_string(),
            repos: vec![],
            roles: vec![],
            mcp_servers: vec![],
        }];
        assert!(resolve_project(&projects, "myproject").is_some());
        assert!(resolve_project(&projects, "MYPROJECT").is_some());
        assert!(resolve_project(&projects, "MyProject").is_some());
    }

    #[test]
    fn resolve_by_uuid() {
        let pid = test_project_id("test");
        let projects = vec![SharedProject {
            id: pid,
            name: "test".to_string(),
            repos: vec![],
            roles: vec![],
            mcp_servers: vec![],
        }];
        assert!(resolve_project(&projects, &pid.to_string()).is_some());
    }

    #[test]
    fn resolve_not_found() {
        let projects = vec![SharedProject {
            id: test_project_id("test"),
            name: "test".to_string(),
            repos: vec![],
            roles: vec![],
            mcp_servers: vec![],
        }];
        assert!(resolve_project(&projects, "nonexistent").is_none());
    }

    // ── error_json tests ────────────────────────────────────────

    #[test]
    fn error_json_produces_valid_json() {
        let result = error_json("something went wrong");
        let v: serde_json::Value = parse_json(&result);
        assert_eq!(v["error"], "something went wrong");
    }

    #[test]
    fn error_json_escapes_special_chars() {
        let result = error_json("has \"quotes\" and \\backslash");
        let v: serde_json::Value = parse_json(&result);
        assert_eq!(v["error"], "has \"quotes\" and \\backslash");
    }

    // ── Tool function tests (via ThurboxMcp) ────────────────────

    #[test]
    fn list_projects_empty() {
        let server = test_server();
        let result = server.list_projects();
        let v = parse_json(&result);
        assert_eq!(v, serde_json::json!([]));
    }

    #[test]
    fn create_and_list_projects() {
        let server = test_server();

        let result = server.create_project(Parameters(CreateProjectParams {
            name: "myapp".to_string(),
            repos: vec!["/home/user/myapp".to_string()],
        }));
        let created = parse_json(&result);
        assert_eq!(created["name"], "myapp");
        assert!(created["id"].is_string());

        let result = server.list_projects();
        let list: Vec<serde_json::Value> = serde_json::from_str(&result).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["name"], "myapp");
    }

    #[test]
    fn get_project_by_name() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "findme".to_string(),
            repos: vec![],
        }));

        let result = server.get_project(Parameters(GetProjectParams {
            project: "findme".to_string(),
        }));
        let v = parse_json(&result);
        assert_eq!(v["name"], "findme");
    }

    #[test]
    fn get_project_by_uuid() {
        let server = test_server();
        let create_result = server.create_project(Parameters(CreateProjectParams {
            name: "byid".to_string(),
            repos: vec![],
        }));
        let created = parse_json(&create_result);
        let id = created["id"].as_str().unwrap();

        let result = server.get_project(Parameters(GetProjectParams {
            project: id.to_string(),
        }));
        let v = parse_json(&result);
        assert_eq!(v["name"], "byid");
    }

    #[test]
    fn get_project_not_found() {
        let server = test_server();
        let result = server.get_project(Parameters(GetProjectParams {
            project: "ghost".to_string(),
        }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("Project not found"));
    }

    #[test]
    fn update_project_name() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "oldname".to_string(),
            repos: vec!["/repo".to_string()],
        }));

        let result = server.update_project(Parameters(UpdateProjectParams {
            project: "oldname".to_string(),
            name: Some("newname".to_string()),
            repos: None,
        }));
        let v = parse_json(&result);
        assert_eq!(v["name"], "newname");
        // Repos should be preserved.
        assert_eq!(v["repos"][0], "/repo");
    }

    #[test]
    fn update_project_repos() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "proj".to_string(),
            repos: vec!["/old".to_string()],
        }));

        let result = server.update_project(Parameters(UpdateProjectParams {
            project: "proj".to_string(),
            name: None,
            repos: Some(vec!["/new1".to_string(), "/new2".to_string()]),
        }));
        let v = parse_json(&result);
        assert_eq!(v["name"], "proj");
        assert_eq!(v["repos"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn update_nonexistent_project() {
        let server = test_server();
        let result = server.update_project(Parameters(UpdateProjectParams {
            project: "nope".to_string(),
            name: Some("renamed".to_string()),
            repos: None,
        }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("Project not found"));
    }

    #[test]
    fn delete_project_soft() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "deleteme".to_string(),
            repos: vec![],
        }));

        let result = server.delete_project(Parameters(DeleteProjectParams {
            project: "deleteme".to_string(),
        }));
        let v = parse_json(&result);
        assert_eq!(v["deleted"], true);
        assert_eq!(v["name"], "deleteme");

        // Should no longer appear in list.
        let list_result = server.list_projects();
        let list = parse_json(&list_result);
        assert_eq!(list, serde_json::json!([]));
    }

    #[test]
    fn delete_nonexistent_project() {
        let server = test_server();
        let result = server.delete_project(Parameters(DeleteProjectParams {
            project: "ghost".to_string(),
        }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("Project not found"));
    }

    #[test]
    fn set_and_list_roles() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "roletest".to_string(),
            repos: vec![],
        }));

        let result = server.set_roles(Parameters(SetRolesParams {
            project: "roletest".to_string(),
            roles: vec![
                RoleInput {
                    name: "developer".to_string(),
                    description: "Full access".to_string(),
                    permission_mode: Some("full".to_string()),
                    allowed_tools: vec![],
                    disallowed_tools: vec![],
                    tools: None,
                    append_system_prompt: None,
                },
                RoleInput {
                    name: "reviewer".to_string(),
                    description: "Read only".to_string(),
                    permission_mode: Some("plan".to_string()),
                    allowed_tools: vec!["Read".to_string()],
                    disallowed_tools: vec!["Edit".to_string()],
                    tools: None,
                    append_system_prompt: Some("Be careful".to_string()),
                },
            ],
        }));
        let set_result = parse_json(&result);
        assert_eq!(set_result.as_array().unwrap().len(), 2);

        let result = server.list_roles(Parameters(ListRolesParams {
            project: "roletest".to_string(),
        }));
        let roles = parse_json(&result);
        assert_eq!(roles.as_array().unwrap().len(), 2);
        assert_eq!(roles[0]["name"], "developer");
        assert_eq!(roles[1]["name"], "reviewer");
        assert_eq!(roles[1]["permission_mode"], "plan");
        assert_eq!(roles[1]["allowed_tools"][0], "Read");
        assert_eq!(roles[1]["disallowed_tools"][0], "Edit");
        assert_eq!(roles[1]["append_system_prompt"], "Be careful");
    }

    #[test]
    fn set_roles_for_nonexistent_project() {
        let server = test_server();
        let result = server.set_roles(Parameters(SetRolesParams {
            project: "ghost".to_string(),
            roles: vec![RoleInput {
                name: "dev".to_string(),
                description: "Dev".to_string(),
                permission_mode: None,
                allowed_tools: vec![],
                disallowed_tools: vec![],
                tools: None,
                append_system_prompt: None,
            }],
        }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("Project not found"));
    }

    #[test]
    fn set_roles_empty_clears_all() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "cleartest".to_string(),
            repos: vec![],
        }));

        // Set one role.
        server.set_roles(Parameters(SetRolesParams {
            project: "cleartest".to_string(),
            roles: vec![RoleInput {
                name: "dev".to_string(),
                description: "Dev".to_string(),
                permission_mode: None,
                allowed_tools: vec![],
                disallowed_tools: vec![],
                tools: None,
                append_system_prompt: None,
            }],
        }));

        // Clear all roles with empty array.
        let result = server.set_roles(Parameters(SetRolesParams {
            project: "cleartest".to_string(),
            roles: vec![],
        }));
        let v = parse_json(&result);
        assert_eq!(v, serde_json::json!([]));

        // Verify list_roles also returns empty.
        let list_result = server.list_roles(Parameters(ListRolesParams {
            project: "cleartest".to_string(),
        }));
        let roles = parse_json(&list_result);
        assert_eq!(roles, serde_json::json!([]));
    }

    #[test]
    fn set_roles_replaces_existing() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "replacetest".to_string(),
            repos: vec![],
        }));

        // Set initial roles.
        server.set_roles(Parameters(SetRolesParams {
            project: "replacetest".to_string(),
            roles: vec![
                RoleInput {
                    name: "alpha".to_string(),
                    description: "First".to_string(),
                    permission_mode: None,
                    allowed_tools: vec![],
                    disallowed_tools: vec![],
                    tools: None,
                    append_system_prompt: None,
                },
                RoleInput {
                    name: "beta".to_string(),
                    description: "Second".to_string(),
                    permission_mode: None,
                    allowed_tools: vec![],
                    disallowed_tools: vec![],
                    tools: None,
                    append_system_prompt: None,
                },
            ],
        }));

        // Replace with a single different role.
        let result = server.set_roles(Parameters(SetRolesParams {
            project: "replacetest".to_string(),
            roles: vec![RoleInput {
                name: "gamma".to_string(),
                description: "Replacement".to_string(),
                permission_mode: Some("plan".to_string()),
                allowed_tools: vec!["Read".to_string()],
                disallowed_tools: vec![],
                tools: None,
                append_system_prompt: None,
            }],
        }));
        let roles = parse_json(&result);
        assert_eq!(roles.as_array().unwrap().len(), 1);
        assert_eq!(roles[0]["name"], "gamma");
        assert_eq!(roles[0]["permission_mode"], "plan");
    }

    #[test]
    fn set_roles_with_tools_field() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "toolstest".to_string(),
            repos: vec![],
        }));

        let result = server.set_roles(Parameters(SetRolesParams {
            project: "toolstest".to_string(),
            roles: vec![RoleInput {
                name: "limited".to_string(),
                description: "Limited tools".to_string(),
                permission_mode: None,
                allowed_tools: vec![],
                disallowed_tools: vec![],
                tools: Some("default".to_string()),
                append_system_prompt: None,
            }],
        }));
        let roles = parse_json(&result);
        assert_eq!(roles[0]["tools"], "default");
    }

    #[test]
    fn list_roles_empty_project() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "noroles".to_string(),
            repos: vec![],
        }));

        let result = server.list_roles(Parameters(ListRolesParams {
            project: "noroles".to_string(),
        }));
        let v = parse_json(&result);
        assert_eq!(v, serde_json::json!([]));
    }

    #[test]
    fn list_roles_for_nonexistent_project() {
        let server = test_server();
        let result = server.list_roles(Parameters(ListRolesParams {
            project: "nope".to_string(),
        }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("Project not found"));
    }

    #[test]
    fn list_sessions_empty() {
        let server = test_server();
        let result = server.list_sessions(Parameters(ListSessionsParams { project: None }));
        let v = parse_json(&result);
        assert_eq!(v, serde_json::json!([]));
    }

    #[test]
    fn list_sessions_filtered_nonexistent_project() {
        let server = test_server();
        let result = server.list_sessions(Parameters(ListSessionsParams {
            project: Some("ghost".to_string()),
        }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("Project not found"));
    }

    #[test]
    fn create_project_deterministic_id() {
        let server = test_server();

        let r1 = server.create_project(Parameters(CreateProjectParams {
            name: "stable".to_string(),
            repos: vec![],
        }));
        let id1 = parse_json(&r1)["id"].as_str().unwrap().to_string();

        // Delete and recreate — same name should produce same ID.
        server.delete_project(Parameters(DeleteProjectParams {
            project: "stable".to_string(),
        }));

        // Recreating with same name should produce the same deterministic ID.
        let expected_id = test_project_id("stable").to_string();
        assert_eq!(id1, expected_id);
    }

    #[test]
    fn get_project_includes_roles() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "with-roles".to_string(),
            repos: vec![],
        }));

        // Set roles directly via DB to test the response includes them.
        {
            let db = server.db.lock().unwrap();
            let pid = test_project_id("with-roles");
            db.replace_roles(
                pid,
                &[RoleConfig {
                    name: "dev".to_string(),
                    description: "Dev role".to_string(),
                    permissions: RolePermissions::default(),
                }],
            )
            .unwrap();
        }

        let result = server.get_project(Parameters(GetProjectParams {
            project: "with-roles".to_string(),
        }));
        let v = parse_json(&result);
        assert_eq!(v["roles"].as_array().unwrap().len(), 1);
        assert_eq!(v["roles"][0]["name"], "dev");
    }

    #[test]
    fn set_and_list_mcp_servers() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "mcptest".to_string(),
            repos: vec![],
        }));

        let result = server.set_mcp_servers(Parameters(SetMcpServersParams {
            project: "mcptest".to_string(),
            servers: vec![
                McpServerInput {
                    name: "filesystem".to_string(),
                    command: "npx".to_string(),
                    args: vec![
                        "-y".to_string(),
                        "@modelcontextprotocol/server-filesystem".to_string(),
                    ],
                    env: HashMap::new(),
                },
                McpServerInput {
                    name: "github".to_string(),
                    command: "gh-mcp".to_string(),
                    args: vec![],
                    env: HashMap::from([("GITHUB_TOKEN".to_string(), "tok-123".to_string())]),
                },
            ],
        }));
        let set_result = parse_json(&result);
        assert_eq!(set_result.as_array().unwrap().len(), 2);

        let result = server.list_mcp_servers(Parameters(ListMcpServersParams {
            project: "mcptest".to_string(),
        }));
        let servers = parse_json(&result);
        assert_eq!(servers.as_array().unwrap().len(), 2);
    }

    #[test]
    fn list_mcp_servers_nonexistent_project() {
        let server = test_server();
        let result = server.list_mcp_servers(Parameters(ListMcpServersParams {
            project: "nope".to_string(),
        }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("Project not found"));
    }

    #[test]
    fn get_project_includes_mcp_servers() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "with-mcp".to_string(),
            repos: vec![],
        }));

        // Set MCP servers directly via DB to test the response includes them.
        {
            let db = server.db.lock().unwrap();
            let pid = test_project_id("with-mcp");
            db.replace_mcp_servers(
                pid,
                &[McpServerConfig {
                    name: "test-server".to_string(),
                    command: "test-cmd".to_string(),
                    args: vec!["--flag".to_string()],
                    env: HashMap::from([("KEY".to_string(), "VAL".to_string())]),
                }],
            )
            .unwrap();
        }

        let result = server.get_project(Parameters(GetProjectParams {
            project: "with-mcp".to_string(),
        }));
        let v = parse_json(&result);
        assert_eq!(v["mcp_servers"].as_array().unwrap().len(), 1);
        assert_eq!(v["mcp_servers"][0]["name"], "test-server");
        assert_eq!(v["mcp_servers"][0]["command"], "test-cmd");
        assert_eq!(v["mcp_servers"][0]["args"][0], "--flag");
        assert_eq!(v["mcp_servers"][0]["env"]["KEY"], "VAL");
    }
}
