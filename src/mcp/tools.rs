//! Tool implementations for the Thurbox MCP server.

use std::path::PathBuf;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::project::{ProjectConfig, ProjectId};
use crate::session::{ContainerConfig, McpServerConfig, RoleConfig, RolePermissions, SessionId};
use crate::storage::Database;
use crate::sync::{SharedProject, SharedSession};

use crate::session::VmConfig;
use crate::storage::vms::VmRecord;

use super::types::{
    ConfigureProjectContainerParams, ConfigureProjectVmParams, ContainerfileTemplateResponse,
    ContainerfileTemplateSummary, CreateProjectParams, DeleteContainerfileTemplateParams,
    DeleteProjectParams, DeleteSessionParams, DeleteVmImageParams, DownloadVmImageParams,
    GetContainerfileTemplateParams, GetProjectContainerConfigParams, GetProjectParams,
    GetSessionParams, GetVmParams, ListMcpServersParams, ListRolesParams, ListSessionsParams,
    ListVmsParams, McpServerResponse, ProjectContainerConfigResponse, ProjectResponse,
    ProjectVmConfigResponse, RestartSessionParams, RestoreSessionParams, RoleResponse,
    SessionResponse, SetContainerfileTemplateParams, SetMcpServersParams, SetRolesParams,
    UpdateProjectParams, VmImageResponse, VmResponse, WorktreeResponse,
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

/// Resolve a session UUID against the database, returning the session or a JSON error.
fn resolve_session(db: &Database, identifier: &str) -> Result<SharedSession, String> {
    let session_id: SessionId = identifier
        .parse()
        .map_err(|_| error_json(&format!("Invalid session UUID: {identifier}")))?;
    db.get_session_by_id(session_id)
        .map_err(|e| error_json(&e.to_string()))?
        .ok_or_else(|| error_json(&format!("Session not found: {identifier}")))
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
        env: r.permissions.env.clone(),
    }
}

fn session_to_response(s: &SharedSession) -> SessionResponse {
    SessionResponse {
        id: s.id.to_string(),
        name: s.name.clone(),
        project_id: s.project_id.to_string(),
        role: s.role.clone(),
        backend_type: s.backend_type.clone(),
        agent_session_id: s.agent_session_id.clone(),
        cwd: s.cwd.clone(),
        worktrees: s
            .worktrees
            .iter()
            .map(|w| WorktreeResponse {
                repo_path: w.repo_path.clone(),
                worktree_path: w.worktree_path.clone(),
                branch: w.branch.clone(),
            })
            .collect(),
    }
}

fn vm_to_response(r: &VmRecord) -> VmResponse {
    VmResponse {
        id: r.id.clone(),
        session_id: r.session_id.clone(),
        project_id: r.project_id.clone(),
        state: r.state.to_string(),
        ssh_port: r.ssh_port,
        base_image: r.base_image.clone(),
        cpus: r.cpus,
        memory_mb: r.memory_mb,
        disk_gb: r.disk_gb,
        error_msg: r.error_msg.clone(),
    }
}

fn json_text<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|e| error_json(&e.to_string()))
}

fn error_json(msg: &str) -> String {
    serde_json::json!({ "error": msg }).to_string()
}

/// Validate a name for use as a filesystem directory/file name.
/// Rejects path traversal, hidden files, and overly long names.
fn validate_safe_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(error_json("Name cannot be empty"));
    }
    if name.len() > 64 {
        return Err(error_json("Name too long (max 64 characters)"));
    }
    if name.starts_with('.') {
        return Err(error_json("Name cannot start with '.'"));
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(error_json("Name contains invalid characters"));
    }
    Ok(())
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
        description = "Atomically replace all roles for a project. Deletes existing roles and inserts the provided list in a single transaction. To add a role, include all existing roles plus the new one. To clear all roles, pass an empty array. Each role has: name (1-64 chars, unique), description, permission_mode (default/plan/acceptEdits/dontAsk/bypassPermissions), allowed_tools, disallowed_tools, tools, append_system_prompt, env (object of key-value environment variables injected into sessions). See docs/MCP_ROLES.md for the complete guide."
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
                    env: r.env,
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

    #[tool(description = "Get a session by its UUID")]
    fn get_session(&self, Parameters(params): Parameters<GetSessionParams>) -> String {
        let db = self.db.lock().unwrap();
        match resolve_session(&db, &params.session) {
            Ok(session) => json_text(&session_to_response(&session)),
            Err(e) => e,
        }
    }

    #[tool(
        description = "Delete a session (soft delete). The TUI will detect the deletion and clean up the tmux pane and worktree."
    )]
    fn delete_session(&self, Parameters(params): Parameters<DeleteSessionParams>) -> String {
        let db = self.db.lock().unwrap();
        let session = match resolve_session(&db, &params.session) {
            Ok(s) => s,
            Err(e) => return e,
        };

        match db.soft_delete_session(session.id) {
            Ok(()) => serde_json::json!({
                "deleted": true,
                "id": session.id.to_string(),
                "name": session.name,
            })
            .to_string(),
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Restart a session by queuing a restart command. The TUI will process the command and restart the session with its existing Claude session ID."
    )]
    fn restart_session(&self, Parameters(params): Parameters<RestartSessionParams>) -> String {
        let db = self.db.lock().unwrap();
        let session = match resolve_session(&db, &params.session) {
            Ok(s) => s,
            Err(e) => return e,
        };

        match db.enqueue_session_command(session.id, "restart") {
            Ok(command_id) => serde_json::json!({
                "queued": true,
                "command_id": command_id,
                "session_id": session.id.to_string(),
                "session_name": session.name,
            })
            .to_string(),
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(description = "List all active VMs, optionally filtered by project name or UUID")]
    fn list_vms(&self, Parameters(params): Parameters<ListVmsParams>) -> String {
        let db = self.db.lock().unwrap();

        let project_id = match &params.project {
            Some(filter) => {
                let (projects, idx) = match require_project(&db, filter) {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                Some(projects[idx].id.to_string())
            }
            None => None,
        };

        match db.list_vms(project_id.as_deref()) {
            Ok(vms) => {
                let resp: Vec<VmResponse> = vms.iter().map(vm_to_response).collect();
                json_text(&resp)
            }
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(description = "Get a VM by its UUID")]
    fn get_vm(&self, Parameters(params): Parameters<GetVmParams>) -> String {
        let db = self.db.lock().unwrap();
        match db.get_vm(&params.vm) {
            Ok(Some(vm)) => json_text(&vm_to_response(&vm)),
            Ok(None) => error_json(&format!("VM not found: {}", params.vm)),
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Configure default VM settings for a project. These settings apply to new VM sessions created for the project."
    )]
    fn configure_project_vm(
        &self,
        Parameters(params): Parameters<ConfigureProjectVmParams>,
    ) -> String {
        let db = self.db.lock().unwrap();
        let (projects, idx) = match require_project(&db, &params.project) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let project = &projects[idx];

        let config = VmConfig {
            base_image: params
                .base_image
                .unwrap_or_else(|| VmConfig::default().base_image),
            cpus: params.cpus.unwrap_or(VmConfig::default().cpus),
            memory_mb: params.memory_mb.unwrap_or(VmConfig::default().memory_mb),
            disk_gb: params.disk_gb.unwrap_or(VmConfig::default().disk_gb),
            setup_script: params.setup_script,
        };

        if let Err(e) = db.set_project_vm_config(&project.id.to_string(), &config) {
            return error_json(&e.to_string());
        }

        // Read back the saved config
        match db.get_project_vm_config(&project.id.to_string()) {
            Ok(Some(cfg)) => json_text(&ProjectVmConfigResponse {
                project_id: cfg.project_id,
                base_image: cfg.base_image,
                cpus: cfg.cpus,
                memory_mb: cfg.memory_mb,
                disk_gb: cfg.disk_gb,
                setup_script: cfg.setup_script,
            }),
            Ok(None) => error_json("Config not found after save"),
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Restore a soft-deleted session. The TUI will detect the restored session via sync polling and spawn it with --resume if a Claude session ID exists."
    )]
    fn restore_session(&self, Parameters(params): Parameters<RestoreSessionParams>) -> String {
        let db = self.db.lock().unwrap();
        let session_id: SessionId = match params.session.parse() {
            Ok(id) => id,
            Err(_) => return error_json(&format!("Invalid session UUID: {}", params.session)),
        };

        let deleted = match db.get_deleted_session_by_id(session_id) {
            Ok(Some(s)) => s,
            Ok(None) => {
                return error_json(&format!("Deleted session not found: {}", params.session))
            }
            Err(e) => return error_json(&e.to_string()),
        };

        match db.restore_session(deleted.id) {
            Ok(()) => serde_json::json!({
                "restored": true,
                "id": deleted.id.to_string(),
                "name": deleted.name,
            })
            .to_string(),
            Err(e) => error_json(&e.to_string()),
        }
    }

    // ── Containerfile Template Tools ────────────────────────────

    #[tool(description = "List all Containerfile templates with their files")]
    fn list_containerfile_templates(&self) -> String {
        let dir = match crate::paths::containerfiles_directory() {
            Some(d) => d,
            None => return error_json("Could not resolve containerfiles directory"),
        };

        if !dir.exists() {
            return json_text(&Vec::<ContainerfileTemplateSummary>::new());
        }

        let mut templates = Vec::new();
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => return error_json(&format!("Failed to read directory: {e}")),
        };

        for entry in entries.flatten() {
            if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let mut files = Vec::new();
            if let Ok(children) = std::fs::read_dir(entry.path()) {
                for child in children.flatten() {
                    if child.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                        files.push(child.file_name().to_string_lossy().to_string());
                    }
                }
            }
            files.sort();
            templates.push(ContainerfileTemplateSummary { name, files });
        }
        templates.sort_by(|a, b| a.name.cmp(&b.name));
        json_text(&templates)
    }

    #[tool(description = "Get a Containerfile template's content and list its support files")]
    fn get_containerfile_template(
        &self,
        Parameters(params): Parameters<GetContainerfileTemplateParams>,
    ) -> String {
        if let Err(e) = validate_safe_name(&params.name) {
            return e;
        }

        let dir = match crate::paths::containerfiles_directory() {
            Some(d) => d,
            None => return error_json("Could not resolve containerfiles directory"),
        };

        let template_dir = dir.join(&params.name);
        if !template_dir.is_dir() {
            return error_json(&format!("Template not found: {}", params.name));
        }

        let containerfile_path = template_dir.join("Containerfile");
        let containerfile_content = match std::fs::read_to_string(&containerfile_path) {
            Ok(c) => c,
            Err(_) => {
                return error_json(&format!("Template '{}' has no Containerfile", params.name))
            }
        };

        let mut support_files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&template_dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                    let fname = entry.file_name().to_string_lossy().to_string();
                    if fname != "Containerfile" {
                        support_files.push(fname);
                    }
                }
            }
        }
        support_files.sort();

        json_text(&ContainerfileTemplateResponse {
            name: params.name,
            containerfile_content,
            support_files,
        })
    }

    #[tool(
        description = "Create or update a Containerfile template. Writes the Containerfile and optional support files to the template directory."
    )]
    fn set_containerfile_template(
        &self,
        Parameters(params): Parameters<SetContainerfileTemplateParams>,
    ) -> String {
        if let Err(e) = validate_safe_name(&params.name) {
            return e;
        }

        let dir = match crate::paths::containerfiles_directory() {
            Some(d) => d,
            None => return error_json("Could not resolve containerfiles directory"),
        };

        let template_dir = dir.join(&params.name);
        if let Err(e) = std::fs::create_dir_all(&template_dir) {
            return error_json(&format!("Failed to create template directory: {e}"));
        }

        if let Err(e) = std::fs::write(
            template_dir.join("Containerfile"),
            &params.containerfile_content,
        ) {
            return error_json(&format!("Failed to write Containerfile: {e}"));
        }

        let mut all_files = vec!["Containerfile".to_string()];
        if let Some(ref files) = params.support_files {
            for f in files {
                if let Err(e) = validate_safe_name(&f.filename) {
                    return e;
                }
                if let Err(e) = std::fs::write(template_dir.join(&f.filename), &f.content) {
                    return error_json(&format!("Failed to write {}: {e}", f.filename));
                }
                all_files.push(f.filename.clone());
            }
        }
        all_files.sort();

        json_text(&ContainerfileTemplateSummary {
            name: params.name,
            files: all_files,
        })
    }

    #[tool(description = "Delete a Containerfile template directory. Refuses to delete 'default'.")]
    fn delete_containerfile_template(
        &self,
        Parameters(params): Parameters<DeleteContainerfileTemplateParams>,
    ) -> String {
        if let Err(e) = validate_safe_name(&params.name) {
            return e;
        }

        if params.name == "default" {
            return error_json("Cannot delete the 'default' template");
        }

        let dir = match crate::paths::containerfiles_directory() {
            Some(d) => d,
            None => return error_json("Could not resolve containerfiles directory"),
        };

        let template_dir = dir.join(&params.name);
        if !template_dir.is_dir() {
            return error_json(&format!("Template not found: {}", params.name));
        }

        if let Err(e) = std::fs::remove_dir_all(&template_dir) {
            return error_json(&format!("Failed to delete template: {e}"));
        }

        serde_json::json!({
            "deleted": true,
            "name": params.name,
        })
        .to_string()
    }

    // ── Project Container Config Tools ──────────────────────────

    #[tool(
        description = "Configure default container settings for a project. These settings apply to new container sessions created for the project."
    )]
    fn configure_project_container(
        &self,
        Parameters(params): Parameters<ConfigureProjectContainerParams>,
    ) -> String {
        let db = self.db.lock().unwrap();
        let (projects, idx) = match require_project(&db, &params.project) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let project = &projects[idx];

        let defaults = ContainerConfig::default();
        let config = ContainerConfig {
            image: params.image.or(defaults.image),
            cpus: params.cpus.unwrap_or(defaults.cpus),
            memory_mb: params.memory_mb.unwrap_or(defaults.memory_mb),
            firewall_enabled: params.firewall_enabled.unwrap_or(defaults.firewall_enabled),
            containerfile: params.containerfile.or(defaults.containerfile),
        };

        if let Err(e) = db.set_project_container_config(&project.id.to_string(), &config) {
            return error_json(&e.to_string());
        }

        match db.get_project_container_config(&project.id.to_string()) {
            Ok(Some(cfg)) => json_text(&ProjectContainerConfigResponse {
                project_id: cfg.project_id,
                image: cfg.image,
                cpus: cfg.cpus,
                memory_mb: cfg.memory_mb,
                firewall_enabled: cfg.firewall_enabled,
                containerfile: cfg.containerfile,
            }),
            Ok(None) => error_json("Config not found after save"),
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(description = "Get the current container configuration for a project")]
    fn get_project_container_config(
        &self,
        Parameters(params): Parameters<GetProjectContainerConfigParams>,
    ) -> String {
        let db = self.db.lock().unwrap();
        let (projects, idx) = match require_project(&db, &params.project) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let project = &projects[idx];

        match db.get_project_container_config(&project.id.to_string()) {
            Ok(Some(cfg)) => json_text(&ProjectContainerConfigResponse {
                project_id: cfg.project_id,
                image: cfg.image,
                cpus: cfg.cpus,
                memory_mb: cfg.memory_mb,
                firewall_enabled: cfg.firewall_enabled,
                containerfile: cfg.containerfile,
            }),
            Ok(None) => json_text(&serde_json::json!({
                "project_id": project.id.to_string(),
                "message": "No container config set (using defaults)"
            })),
            Err(e) => error_json(&e.to_string()),
        }
    }

    // ── VM Image Tools ──────────────────────────────────────────

    #[tool(description = "List downloaded VM images with file sizes")]
    fn list_vm_images(&self) -> String {
        let dir = match crate::paths::images_directory() {
            Some(d) => d,
            None => return error_json("Could not resolve images directory"),
        };

        if !dir.exists() {
            return json_text(&Vec::<VmImageResponse>::new());
        }

        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => return error_json(&format!("Failed to read images directory: {e}")),
        };

        let mut images = Vec::new();
        for entry in entries.flatten() {
            if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                let filename = entry.file_name().to_string_lossy().to_string();
                if filename.ends_with(".partial") {
                    continue;
                }
                let size_bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
                images.push(VmImageResponse {
                    filename,
                    size_bytes,
                });
            }
        }
        images.sort_by(|a, b| a.filename.cmp(&b.filename));
        json_text(&images)
    }

    #[tool(
        description = "Download a VM image from an HTTPS URL to the images directory. Uses atomic download (writes to .partial file, then renames)."
    )]
    fn download_vm_image(&self, Parameters(params): Parameters<DownloadVmImageParams>) -> String {
        if !params.url.starts_with("https://") {
            return error_json("Only HTTPS URLs are allowed");
        }

        let filename = match &params.filename {
            Some(f) => {
                if let Err(e) = validate_safe_name(f) {
                    return e;
                }
                f.clone()
            }
            None => {
                // Derive filename from URL path
                match params.url.rsplit('/').next() {
                    Some(f) if !f.is_empty() => {
                        let name = f.split('?').next().unwrap_or(f).to_string();
                        if let Err(e) = validate_safe_name(&name) {
                            return e;
                        }
                        name
                    }
                    _ => {
                        return error_json(
                            "Cannot derive filename from URL; provide one explicitly",
                        )
                    }
                }
            }
        };

        let dir = match crate::paths::images_directory() {
            Some(d) => d,
            None => return error_json("Could not resolve images directory"),
        };

        if let Err(e) = std::fs::create_dir_all(&dir) {
            return error_json(&format!("Failed to create images directory: {e}"));
        }

        let partial_path = dir.join(format!("{filename}.partial"));
        let final_path = dir.join(&filename);

        let result = std::process::Command::new("curl")
            .args([
                "-fSL",
                "--output",
                &partial_path.to_string_lossy(),
                &params.url,
            ])
            .output();

        match result {
            Ok(output) if output.status.success() => {
                if let Err(e) = std::fs::rename(&partial_path, &final_path) {
                    let _ = std::fs::remove_file(&partial_path);
                    return error_json(&format!("Failed to finalize download: {e}"));
                }
                let size = std::fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);
                json_text(&VmImageResponse {
                    filename,
                    size_bytes: size,
                })
            }
            Ok(output) => {
                let _ = std::fs::remove_file(&partial_path);
                let stderr = String::from_utf8_lossy(&output.stderr);
                error_json(&format!("Download failed: {stderr}"))
            }
            Err(e) => {
                let _ = std::fs::remove_file(&partial_path);
                error_json(&format!("Failed to run curl: {e}"))
            }
        }
    }

    #[tool(description = "Delete a cached VM image")]
    fn delete_vm_image(&self, Parameters(params): Parameters<DeleteVmImageParams>) -> String {
        if let Err(e) = validate_safe_name(&params.filename) {
            return e;
        }

        let dir = match crate::paths::images_directory() {
            Some(d) => d,
            None => return error_json("Could not resolve images directory"),
        };

        let path = dir.join(&params.filename);
        if !path.is_file() {
            return error_json(&format!("Image not found: {}", params.filename));
        }

        if let Err(e) = std::fs::remove_file(&path) {
            return error_json(&format!("Failed to delete image: {e}"));
        }

        serde_json::json!({
            "deleted": true,
            "filename": params.filename,
        })
        .to_string()
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
                    env: HashMap::new(),
                },
                RoleInput {
                    name: "reviewer".to_string(),
                    description: "Read only".to_string(),
                    permission_mode: Some("plan".to_string()),
                    allowed_tools: vec!["Read".to_string()],
                    disallowed_tools: vec!["Edit".to_string()],
                    tools: None,
                    append_system_prompt: Some("Be careful".to_string()),
                    env: HashMap::new(),
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
                env: HashMap::new(),
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
                env: HashMap::new(),
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
                    env: HashMap::new(),
                },
                RoleInput {
                    name: "beta".to_string(),
                    description: "Second".to_string(),
                    permission_mode: None,
                    allowed_tools: vec![],
                    disallowed_tools: vec![],
                    tools: None,
                    append_system_prompt: None,
                    env: HashMap::new(),
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
                env: HashMap::new(),
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
                env: HashMap::new(),
            }],
        }));
        let roles = parse_json(&result);
        assert_eq!(roles[0]["tools"], "default");
    }

    #[test]
    fn set_roles_with_env() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "envtest".to_string(),
            repos: vec![],
        }));

        let mut env = HashMap::new();
        env.insert("API_KEY".to_string(), "sk-secret".to_string());
        env.insert("DEBUG".to_string(), "1".to_string());

        let result = server.set_roles(Parameters(SetRolesParams {
            project: "envtest".to_string(),
            roles: vec![RoleInput {
                name: "with-env".to_string(),
                description: "Has env vars".to_string(),
                permission_mode: None,
                allowed_tools: vec![],
                disallowed_tools: vec![],
                tools: None,
                append_system_prompt: None,
                env: env.clone(),
            }],
        }));
        let roles = parse_json(&result);
        assert_eq!(roles[0]["name"], "with-env");
        assert_eq!(roles[0]["env"]["API_KEY"], "sk-secret");
        assert_eq!(roles[0]["env"]["DEBUG"], "1");

        // Verify persistence via list_roles
        let list_result = server.list_roles(Parameters(ListRolesParams {
            project: "envtest".to_string(),
        }));
        let listed = parse_json(&list_result);
        assert_eq!(listed[0]["env"]["API_KEY"], "sk-secret");
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

    // ── Session tool tests ─────────────────────────────────────

    fn insert_test_session(server: &ThurboxMcp, project_name: &str) -> SessionId {
        let db = server.db.lock().unwrap();
        let pid = test_project_id(project_name);
        let session = SharedSession {
            id: SessionId::default(),
            name: "1".to_string(),
            project_id: pid,
            role: "developer".to_string(),
            backend_id: "thurbox:@0".to_string(),
            backend_type: "tmux".to_string(),
            agent_session_id: Some("claude-abc".to_string()),
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            tombstone: false,
            tombstone_at: None,
        };
        let sid = session.id;
        db.upsert_session(&session).unwrap();
        sid
    }

    #[test]
    fn get_session_by_uuid() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "sesstest".to_string(),
            repos: vec![],
        }));
        let sid = insert_test_session(&server, "sesstest");

        let result = server.get_session(Parameters(GetSessionParams {
            session: sid.to_string(),
        }));
        let v = parse_json(&result);
        assert_eq!(v["id"], sid.to_string());
        assert_eq!(v["name"], "1");
        assert_eq!(v["role"], "developer");
    }

    #[test]
    fn get_session_not_found() {
        let server = test_server();
        let result = server.get_session(Parameters(GetSessionParams {
            session: SessionId::default().to_string(),
        }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("Session not found"));
    }

    #[test]
    fn get_session_invalid_uuid() {
        let server = test_server();
        let result = server.get_session(Parameters(GetSessionParams {
            session: "not-a-uuid".to_string(),
        }));
        let v = parse_json(&result);
        assert!(v["error"]
            .as_str()
            .unwrap()
            .contains("Invalid session UUID"));
    }

    #[test]
    fn delete_session_soft() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "deltest".to_string(),
            repos: vec![],
        }));
        let sid = insert_test_session(&server, "deltest");

        let result = server.delete_session(Parameters(DeleteSessionParams {
            session: sid.to_string(),
        }));
        let v = parse_json(&result);
        assert_eq!(v["deleted"], true);
        assert_eq!(v["id"], sid.to_string());

        // Should no longer be findable
        let get_result = server.get_session(Parameters(GetSessionParams {
            session: sid.to_string(),
        }));
        let v = parse_json(&get_result);
        assert!(v["error"].as_str().unwrap().contains("Session not found"));
    }

    #[test]
    fn delete_session_not_found() {
        let server = test_server();
        let result = server.delete_session(Parameters(DeleteSessionParams {
            session: SessionId::default().to_string(),
        }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("Session not found"));
    }

    #[test]
    fn restart_session_not_found() {
        let server = test_server();
        let result = server.restart_session(Parameters(RestartSessionParams {
            session: SessionId::default().to_string(),
        }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("Session not found"));
    }

    #[test]
    fn restart_session_queues_command() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "resttest".to_string(),
            repos: vec![],
        }));
        let sid = insert_test_session(&server, "resttest");

        let result = server.restart_session(Parameters(RestartSessionParams {
            session: sid.to_string(),
        }));
        let v = parse_json(&result);
        assert_eq!(v["queued"], true);
        assert_eq!(v["session_id"], sid.to_string());
        assert!(v["command_id"].as_i64().unwrap() > 0);

        // Verify command exists in DB
        let db = server.db.lock().unwrap();
        let cmds = db.pending_session_commands().unwrap();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].command, "restart");
        assert_eq!(cmds[0].session_id, sid);
    }

    // ── Restore session tests ────────────────────────────────────

    #[test]
    fn restore_session_success() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "restoretest".to_string(),
            repos: vec![],
        }));
        let sid = insert_test_session(&server, "restoretest");

        // Delete first
        server.delete_session(Parameters(DeleteSessionParams {
            session: sid.to_string(),
        }));

        // Restore
        let result = server.restore_session(Parameters(RestoreSessionParams {
            session: sid.to_string(),
        }));
        let v = parse_json(&result);
        assert_eq!(v["restored"], true);
        assert_eq!(v["id"], sid.to_string());
        assert_eq!(v["name"], "1");

        // Session should be active again
        let get_result = server.get_session(Parameters(GetSessionParams {
            session: sid.to_string(),
        }));
        let v = parse_json(&get_result);
        assert_eq!(v["name"], "1");
    }

    #[test]
    fn restore_session_not_found() {
        let server = test_server();
        let result = server.restore_session(Parameters(RestoreSessionParams {
            session: SessionId::default().to_string(),
        }));
        let v = parse_json(&result);
        assert!(v["error"]
            .as_str()
            .unwrap()
            .contains("Deleted session not found"));
    }

    #[test]
    fn restore_session_invalid_uuid() {
        let server = test_server();
        let result = server.restore_session(Parameters(RestoreSessionParams {
            session: "not-a-uuid".to_string(),
        }));
        let v = parse_json(&result);
        assert!(v["error"]
            .as_str()
            .unwrap()
            .contains("Invalid session UUID"));
    }

    #[test]
    fn restore_session_rejects_active_session() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "activetest".to_string(),
            repos: vec![],
        }));
        let sid = insert_test_session(&server, "activetest");

        // Try to restore an active (non-deleted) session
        let result = server.restore_session(Parameters(RestoreSessionParams {
            session: sid.to_string(),
        }));
        let v = parse_json(&result);
        assert!(v["error"]
            .as_str()
            .unwrap()
            .contains("Deleted session not found"));
    }

    // ── VM tool tests ───────────────────────────────────────────

    #[test]
    fn list_vms_empty() {
        let server = test_server();
        let result = server.list_vms(Parameters(ListVmsParams { project: None }));
        let v = parse_json(&result);
        assert_eq!(v, serde_json::json!([]));
    }

    #[test]
    fn list_vms_with_project_filter() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "vmproj".to_string(),
            repos: vec![],
        }));
        let result = server.list_vms(Parameters(ListVmsParams {
            project: Some("vmproj".to_string()),
        }));
        let v = parse_json(&result);
        assert_eq!(v, serde_json::json!([]));
    }

    #[test]
    fn list_vms_nonexistent_project() {
        let server = test_server();
        let result = server.list_vms(Parameters(ListVmsParams {
            project: Some("ghost".to_string()),
        }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("Project not found"));
    }

    #[test]
    fn get_vm_not_found() {
        let server = test_server();
        let result = server.get_vm(Parameters(GetVmParams {
            vm: "nonexistent".to_string(),
        }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("VM not found"));
    }

    #[test]
    fn get_vm_exists() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "vmtest".to_string(),
            repos: vec![],
        }));
        let pid = test_project_id("vmtest").to_string();
        let config = crate::session::VmConfig::default();

        {
            let db = server.db.lock().unwrap();
            db.insert_vm(
                "vm-1",
                None,
                Some(&pid),
                &crate::session::VmState::Ready,
                22200,
                &config,
            )
            .unwrap();
        }

        let result = server.get_vm(Parameters(GetVmParams {
            vm: "vm-1".to_string(),
        }));
        let v = parse_json(&result);
        assert_eq!(v["id"], "vm-1");
        assert_eq!(v["state"], "Ready");
        assert_eq!(v["ssh_port"], 22200);
        assert_eq!(v["cpus"], 2);
    }

    #[test]
    fn configure_project_vm_creates_config() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "vmcfg".to_string(),
            repos: vec![],
        }));

        let result = server.configure_project_vm(Parameters(ConfigureProjectVmParams {
            project: "vmcfg".to_string(),
            base_image: Some("custom.img".to_string()),
            cpus: Some(4),
            memory_mb: Some(8192),
            disk_gb: None,
            setup_script: Some("apt install nodejs".to_string()),
        }));
        let v = parse_json(&result);
        assert_eq!(v["base_image"], "custom.img");
        assert_eq!(v["cpus"], 4);
        assert_eq!(v["memory_mb"], 8192);
        assert_eq!(v["disk_gb"], 10); // default
        assert_eq!(v["setup_script"], "apt install nodejs");
    }

    #[test]
    fn configure_project_vm_nonexistent_project() {
        let server = test_server();
        let result = server.configure_project_vm(Parameters(ConfigureProjectVmParams {
            project: "ghost".to_string(),
            base_image: None,
            cpus: None,
            memory_mb: None,
            disk_gb: None,
            setup_script: None,
        }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("Project not found"));
    }

    // ── validate_safe_name tests ───────────────────────────────

    #[test]
    fn validate_safe_name_rejects_empty() {
        assert!(validate_safe_name("").is_err());
    }

    #[test]
    fn validate_safe_name_rejects_long_name() {
        let long = "a".repeat(65);
        assert!(validate_safe_name(&long).is_err());
    }

    #[test]
    fn validate_safe_name_accepts_max_length() {
        let max = "a".repeat(64);
        assert!(validate_safe_name(&max).is_ok());
    }

    #[test]
    fn validate_safe_name_rejects_dot_prefix() {
        assert!(validate_safe_name(".hidden").is_err());
    }

    #[test]
    fn validate_safe_name_rejects_path_traversal() {
        assert!(validate_safe_name("../etc").is_err());
        assert!(validate_safe_name("foo/bar").is_err());
        assert!(validate_safe_name("foo\\bar").is_err());
        assert!(validate_safe_name("foo..bar").is_err());
    }

    #[test]
    fn validate_safe_name_accepts_valid_names() {
        assert!(validate_safe_name("default").is_ok());
        assert!(validate_safe_name("my-template").is_ok());
        assert!(validate_safe_name("python_3.12").is_ok());
    }

    // ── Containerfile template tool tests ──────────────────────

    #[test]
    fn list_containerfile_templates_empty() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let server = test_server();

        let result = server.list_containerfile_templates();
        let v = parse_json(&result);
        assert_eq!(v, serde_json::json!([]));
    }

    #[test]
    fn list_containerfile_templates_with_templates() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let server = test_server();

        let cf_dir = temp.path().join("containerfiles");
        std::fs::create_dir_all(cf_dir.join("default")).unwrap();
        std::fs::write(cf_dir.join("default/Containerfile"), "FROM ubuntu").unwrap();
        std::fs::write(cf_dir.join("default/init.sh"), "#!/bin/sh").unwrap();

        let result = server.list_containerfile_templates();
        let v = parse_json(&result);
        assert_eq!(v[0]["name"], "default");
        assert!(v[0]["files"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("Containerfile")));
        assert!(v[0]["files"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("init.sh")));
    }

    #[test]
    fn get_containerfile_template_existing() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let server = test_server();

        let cf_dir = temp.path().join("containerfiles").join("python");
        std::fs::create_dir_all(&cf_dir).unwrap();
        std::fs::write(cf_dir.join("Containerfile"), "FROM python:3.12").unwrap();
        std::fs::write(cf_dir.join("setup.sh"), "pip install stuff").unwrap();

        let result = server.get_containerfile_template(Parameters(
            super::super::types::GetContainerfileTemplateParams {
                name: "python".to_string(),
            },
        ));
        let v = parse_json(&result);
        assert_eq!(v["name"], "python");
        assert_eq!(v["containerfile_content"], "FROM python:3.12");
        assert!(v["support_files"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("setup.sh")));
    }

    #[test]
    fn get_containerfile_template_not_found() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let server = test_server();

        let result = server.get_containerfile_template(Parameters(
            super::super::types::GetContainerfileTemplateParams {
                name: "nonexistent".to_string(),
            },
        ));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("Template not found"));
    }

    #[test]
    fn get_containerfile_template_invalid_name() {
        let server = test_server();
        let result = server.get_containerfile_template(Parameters(
            super::super::types::GetContainerfileTemplateParams {
                name: "../escape".to_string(),
            },
        ));
        let v = parse_json(&result);
        assert!(v["error"].is_string());
    }

    #[test]
    fn set_containerfile_template_creates_new() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let server = test_server();

        let result = server.set_containerfile_template(Parameters(
            super::super::types::SetContainerfileTemplateParams {
                name: "rust".to_string(),
                containerfile_content: "FROM rust:latest".to_string(),
                support_files: Some(vec![super::super::types::SupportFileInput {
                    filename: "build.sh".to_string(),
                    content: "cargo build".to_string(),
                }]),
            },
        ));
        let v = parse_json(&result);
        assert_eq!(v["name"], "rust");
        assert!(v["files"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("Containerfile")));
        assert!(v["files"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("build.sh")));

        // Verify files on disk
        let cf_dir = temp.path().join("containerfiles").join("rust");
        assert!(cf_dir.join("Containerfile").exists());
        assert_eq!(
            std::fs::read_to_string(cf_dir.join("Containerfile")).unwrap(),
            "FROM rust:latest"
        );
        assert_eq!(
            std::fs::read_to_string(cf_dir.join("build.sh")).unwrap(),
            "cargo build"
        );
    }

    #[test]
    fn delete_containerfile_template_refuses_default() {
        let server = test_server();
        let result = server.delete_containerfile_template(Parameters(
            super::super::types::DeleteContainerfileTemplateParams {
                name: "default".to_string(),
            },
        ));
        let v = parse_json(&result);
        assert!(v["error"]
            .as_str()
            .unwrap()
            .contains("Cannot delete the 'default' template"));
    }

    #[test]
    fn delete_containerfile_template_not_found() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let server = test_server();

        let result = server.delete_containerfile_template(Parameters(
            super::super::types::DeleteContainerfileTemplateParams {
                name: "ghost".to_string(),
            },
        ));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("Template not found"));
    }

    #[test]
    fn delete_containerfile_template_success() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let server = test_server();

        let cf_dir = temp.path().join("containerfiles").join("custom");
        std::fs::create_dir_all(&cf_dir).unwrap();
        std::fs::write(cf_dir.join("Containerfile"), "FROM ubuntu").unwrap();

        let result = server.delete_containerfile_template(Parameters(
            super::super::types::DeleteContainerfileTemplateParams {
                name: "custom".to_string(),
            },
        ));
        let v = parse_json(&result);
        assert_eq!(v["deleted"], true);
        assert_eq!(v["name"], "custom");
        assert!(!cf_dir.exists());
    }

    // ── Project container config tool tests ────────────────────

    #[test]
    fn configure_project_container_happy_path() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "myapp".to_string(),
            repos: vec![],
        }));

        let result = server.configure_project_container(Parameters(
            super::super::types::ConfigureProjectContainerParams {
                project: "myapp".to_string(),
                image: Some("ubuntu:24.04".to_string()),
                cpus: Some(4),
                memory_mb: None,
                firewall_enabled: Some(false),
                containerfile: None,
            },
        ));
        let v = parse_json(&result);
        assert!(v["project_id"].is_string());
        assert_eq!(v["image"], "ubuntu:24.04");
        assert_eq!(v["cpus"], 4);
        assert_eq!(v["firewall_enabled"], false);
    }

    #[test]
    fn configure_project_container_nonexistent_project() {
        let server = test_server();
        let result = server.configure_project_container(Parameters(
            super::super::types::ConfigureProjectContainerParams {
                project: "ghost".to_string(),
                image: None,
                cpus: None,
                memory_mb: None,
                firewall_enabled: None,
                containerfile: None,
            },
        ));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("Project not found"));
    }

    #[test]
    fn get_project_container_config_no_config() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "myapp".to_string(),
            repos: vec![],
        }));

        let result = server.get_project_container_config(Parameters(
            super::super::types::GetProjectContainerConfigParams {
                project: "myapp".to_string(),
            },
        ));
        let v = parse_json(&result);
        assert!(v["message"].as_str().unwrap().contains("using defaults"));
    }

    #[test]
    fn get_project_container_config_with_config() {
        let server = test_server();
        server.create_project(Parameters(CreateProjectParams {
            name: "myapp".to_string(),
            repos: vec![],
        }));
        server.configure_project_container(Parameters(
            super::super::types::ConfigureProjectContainerParams {
                project: "myapp".to_string(),
                image: Some("node:20".to_string()),
                cpus: Some(2),
                memory_mb: Some(4096),
                firewall_enabled: Some(true),
                containerfile: Some("default".to_string()),
            },
        ));

        let result = server.get_project_container_config(Parameters(
            super::super::types::GetProjectContainerConfigParams {
                project: "myapp".to_string(),
            },
        ));
        let v = parse_json(&result);
        assert_eq!(v["image"], "node:20");
        assert_eq!(v["cpus"], 2);
        assert_eq!(v["memory_mb"], 4096);
        assert_eq!(v["firewall_enabled"], true);
        assert_eq!(v["containerfile"], "default");
    }

    // ── VM image tool tests ────────────────────────────────────

    #[test]
    fn list_vm_images_empty() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let server = test_server();

        let result = server.list_vm_images();
        let v = parse_json(&result);
        assert_eq!(v, serde_json::json!([]));
    }

    #[test]
    fn list_vm_images_skips_partial() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let server = test_server();

        let img_dir = temp.path().join("images");
        std::fs::create_dir_all(&img_dir).unwrap();
        std::fs::write(img_dir.join("debian.qcow2"), "image data").unwrap();
        std::fs::write(img_dir.join("ubuntu.qcow2.partial"), "downloading").unwrap();

        let result = server.list_vm_images();
        let v = parse_json(&result);
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["filename"], "debian.qcow2");
        assert!(arr[0]["size_bytes"].as_u64().unwrap() > 0);
    }

    #[test]
    fn delete_vm_image_not_found() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let server = test_server();

        let result = server.delete_vm_image(Parameters(super::super::types::DeleteVmImageParams {
            filename: "ghost.qcow2".to_string(),
        }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("Image not found"));
    }

    #[test]
    fn delete_vm_image_invalid_name() {
        let server = test_server();
        let result = server.delete_vm_image(Parameters(super::super::types::DeleteVmImageParams {
            filename: "../etc/passwd".to_string(),
        }));
        let v = parse_json(&result);
        assert!(v["error"].is_string());
    }

    #[test]
    fn delete_vm_image_success() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let server = test_server();

        let img_dir = temp.path().join("images");
        std::fs::create_dir_all(&img_dir).unwrap();
        std::fs::write(img_dir.join("old.qcow2"), "data").unwrap();

        let result = server.delete_vm_image(Parameters(super::super::types::DeleteVmImageParams {
            filename: "old.qcow2".to_string(),
        }));
        let v = parse_json(&result);
        assert_eq!(v["deleted"], true);
        assert!(!img_dir.join("old.qcow2").exists());
    }

    #[test]
    fn download_vm_image_rejects_non_https() {
        let server = test_server();
        let result =
            server.download_vm_image(Parameters(super::super::types::DownloadVmImageParams {
                url: "http://example.com/image.qcow2".to_string(),
                filename: None,
            }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("HTTPS"));
    }

    #[test]
    fn download_vm_image_rejects_file_url() {
        let server = test_server();
        let result =
            server.download_vm_image(Parameters(super::super::types::DownloadVmImageParams {
                url: "file:///etc/passwd".to_string(),
                filename: None,
            }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("HTTPS"));
    }

    #[test]
    fn download_vm_image_rejects_unsafe_filename() {
        let server = test_server();
        let result =
            server.download_vm_image(Parameters(super::super::types::DownloadVmImageParams {
                url: "https://example.com/image.qcow2".to_string(),
                filename: Some("../escape.qcow2".to_string()),
            }));
        let v = parse_json(&result);
        assert!(v["error"].is_string());
    }
}
