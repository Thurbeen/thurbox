//! Tool implementations for the Thurbox MCP server.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::session::{McpServerConfig, RoleConfig, RolePermissions, SessionId};
use crate::storage::Database;
use crate::sync::SharedSession;

use crate::storage::vms::VmRecord;

use crate::session::ScheduledCommand;

use super::types::{
    CancelScheduledCommandParams, CaptureSessionOutputParams, ContainerfileTemplateResponse,
    ContainerfileTemplateSummary, CreateSessionParams, DeleteContainerfileTemplateParams,
    DeleteSessionParams, DeleteVmImageParams, DownloadVmImageParams,
    GetContainerfileTemplateParams, GetScheduledCommandParams, GetSessionParams, GetVmParams,
    ListScheduledCommandsParams, ListSessionsParams, ListVmsParams, McpServerResponse,
    RestartSessionParams, RestoreSessionParams, RoleResponse, ScheduleCommandParams,
    ScheduledCommandResponse, SendPromptParams, SessionResponse, SetContainerfileTemplateParams,
    SetEditorCommandParams, SetMcpServersParams, SetRolesParams, VmImageResponse, VmResponse,
    WorktreeResponse,
};
use super::ThurboxMcp;

// ── Helpers ─────────────────────────────────────────────────────

/// Resolve a session UUID against the database, returning the session or a JSON error.
fn resolve_session(db: &Database, identifier: &str) -> Result<SharedSession, String> {
    let session_id: SessionId = identifier
        .parse()
        .map_err(|_| error_json(&format!("Invalid session UUID: {identifier}")))?;
    db.get_session_by_id(session_id)
        .map_err(|e| error_json(&e.to_string()))?
        .ok_or_else(|| error_json(&format!("Session not found: {identifier}")))
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
    #[tool(
        description = "List all global roles. Returns a JSON array of role objects with name, description, permission_mode, allowed_tools, disallowed_tools, tools, append_system_prompt, and env fields. See docs/MCP_ROLES.md for field details."
    )]
    fn list_roles(&self) -> String {
        let db = self.db.lock().unwrap();
        match db.list_global_roles() {
            Ok(roles) => {
                let resp: Vec<RoleResponse> = roles.iter().map(role_to_response).collect();
                json_text(&resp)
            }
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Atomically replace all global roles. Deletes existing roles and inserts the provided list in a single transaction. To add a role, include all existing roles plus the new one. To clear all roles, pass an empty array. Each role has: name (1-64 chars, unique), description, permission_mode (default/plan/acceptEdits/dontAsk/bypassPermissions), allowed_tools, disallowed_tools, tools, append_system_prompt, env (object of key-value environment variables injected into sessions). See docs/MCP_ROLES.md for the complete guide."
    )]
    fn set_roles(&self, Parameters(params): Parameters<SetRolesParams>) -> String {
        let db = self.db.lock().unwrap();

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

        match db.replace_global_roles(&roles) {
            Ok(()) => {
                let resp: Vec<RoleResponse> = roles.iter().map(role_to_response).collect();
                json_text(&resp)
            }
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Get the configured editor command used by Ctrl+O to open a session's worktree. Returns {\"command\": \"...\"} or {\"command\": null} if unset (in which case $VISUAL/$EDITOR is used as fallback)."
    )]
    fn get_editor_command(&self) -> String {
        let db = self.db.lock().unwrap();
        match db.get_editor_command() {
            Ok(cmd) => serde_json::json!({ "command": cmd }).to_string(),
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Set the editor command used by Ctrl+O to open a session's worktree. The target worktree path is appended as the final argument (e.g. command \"code --wait\" runs `code --wait <worktree>`). Pass an empty string to clear and fall back to $VISUAL/$EDITOR."
    )]
    fn set_editor_command(&self, Parameters(params): Parameters<SetEditorCommandParams>) -> String {
        let db = self.db.lock().unwrap();
        match db.set_editor_command(&params.command) {
            Ok(()) => serde_json::json!({
                "command": if params.command.is_empty() { None } else { Some(params.command) }
            })
            .to_string(),
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "List all global MCP servers. Returns a JSON array of server objects with name, command, args, and env fields."
    )]
    fn list_mcp_servers(&self) -> String {
        let db = self.db.lock().unwrap();
        match db.list_global_mcp_servers() {
            Ok(servers) => {
                let resp: Vec<McpServerResponse> =
                    servers.iter().map(mcp_server_to_response).collect();
                json_text(&resp)
            }
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Atomically replace all global MCP servers. Deletes existing servers and inserts the provided list in a single transaction. To add a server, include all existing servers plus the new one. To clear all servers, pass an empty array."
    )]
    fn set_mcp_servers(&self, Parameters(params): Parameters<SetMcpServersParams>) -> String {
        let db = self.db.lock().unwrap();

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

        match db.replace_global_mcp_servers(&servers) {
            Ok(()) => {
                let resp: Vec<McpServerResponse> =
                    servers.iter().map(mcp_server_to_response).collect();
                json_text(&resp)
            }
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(description = "List all active sessions")]
    fn list_sessions(&self, Parameters(_params): Parameters<ListSessionsParams>) -> String {
        let db = self.db.lock().unwrap();

        match db.list_active_sessions() {
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
        description = "Restart a session in-place — kills its tmux window and re-spawns the claude CLI with --resume. Runs synchronously; the session is live once this returns."
    )]
    fn restart_session(&self, Parameters(params): Parameters<RestartSessionParams>) -> String {
        let db = self.db.lock().unwrap();
        let session = match resolve_session(&db, &params.session) {
            Ok(s) => s,
            Err(e) => return e,
        };

        match crate::session_ops::restart_session_headless(&db, session.id) {
            Ok(()) => serde_json::json!({
                "restarted": true,
                "session_id": session.id.to_string(),
                "session_name": session.name,
            })
            .to_string(),
            Err(e) => error_json(&e),
        }
    }

    // ── Orchestrator Tools ────────────────────────────────────

    #[tool(
        description = "Create a new local-tmux session programmatically. Runs synchronously — by the time this returns, the tmux window is live and the session row is in the database. Optionally creates a git worktree off a base branch. Returns `{id, name, agent_session_id, cwd}`."
    )]
    fn create_session(&self, Parameters(params): Parameters<CreateSessionParams>) -> String {
        if let Err(e) = validate_safe_name(&params.name) {
            return e;
        }
        if params.repo_path.is_empty() {
            return error_json("repo_path must not be empty");
        }

        let req = crate::session_ops::SpawnRequest {
            name: params.name.clone(),
            repo_path: std::path::PathBuf::from(&params.repo_path),
            worktree_branch: params.worktree_branch,
            base_branch: params.base_branch,
            role: params.role,
            mcp_servers: params.mcp_servers,
            skills: params.skills,
            agent_session_id: None,
        };

        let db = self.db.lock().unwrap();
        match crate::session_ops::spawn_session_headless(&db, req) {
            Ok(res) => serde_json::json!({
                "id": res.session_id.to_string(),
                "name": res.name,
                "role": res.role,
                "agent_session_id": res.agent_session_id,
                "cwd": res.cwd.display().to_string(),
            })
            .to_string(),
            Err(e) => error_json(&e),
        }
    }

    #[tool(
        description = "Send text to a session's terminal immediately, followed by Enter. Use this to dispatch a prompt to a Claude session. To read the response, wait briefly then call `capture_session_output` (and inspect session status via `get_session` to detect when the session is idle)."
    )]
    fn send_prompt(&self, Parameters(params): Parameters<SendPromptParams>) -> String {
        let db = self.db.lock().unwrap();
        let session = match resolve_session(&db, &params.session) {
            Ok(s) => s,
            Err(e) => return e,
        };
        if params.text.is_empty() {
            return error_json("text must not be empty");
        }
        let name = session.name.clone();
        drop(db);
        match crate::agent::tmux::send_prompt_now(&name, &params.text) {
            Ok(()) => serde_json::json!({
                "sent": true,
                "session_id": session.id.to_string(),
                "session_name": name,
            })
            .to_string(),
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Capture the rendered terminal contents of a session's tmux pane and return them as a single text string. Defaults to 200 lines of scrollback before the visible region."
    )]
    fn capture_session_output(
        &self,
        Parameters(params): Parameters<CaptureSessionOutputParams>,
    ) -> String {
        let db = self.db.lock().unwrap();
        let session = match resolve_session(&db, &params.session) {
            Ok(s) => s,
            Err(e) => return e,
        };
        let name = session.name.clone();
        drop(db);
        let lines = params.lines.unwrap_or(200);
        match crate::agent::tmux::capture_pane_text(&name, lines) {
            Ok(output) => serde_json::json!({
                "session_id": session.id.to_string(),
                "session_name": name,
                "lines": lines,
                "output": output,
            })
            .to_string(),
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(description = "List all active VMs")]
    fn list_vms(&self, Parameters(_params): Parameters<ListVmsParams>) -> String {
        let db = self.db.lock().unwrap();

        match db.list_vms() {
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

    // ── Scheduled Command Tools ────────────────────────────────

    #[tool(
        description = "Schedule a command to be sent to a session at a future time. The command text is typed into the session's terminal as if the user typed it, then Enter is pressed automatically. One-shot: fires once at the scheduled time."
    )]
    fn schedule_command(&self, Parameters(params): Parameters<ScheduleCommandParams>) -> String {
        let db = self.db.lock().unwrap();

        let session = match resolve_session(&db, &params.session) {
            Ok(s) => s,
            Err(e) => return e,
        };

        if params.command_text.is_empty() {
            return error_json("command_text must not be empty");
        }

        let now = crate::sync::current_time_millis();
        if params.scheduled_at <= now {
            return error_json("scheduled_at must be in the future");
        }

        let session_name = match db.get_session_name(session.id) {
            Ok(Some(name)) => name,
            Ok(None) => return error_json("Session name not found"),
            Err(e) => return error_json(&e.to_string()),
        };

        match db.create_scheduled_command(session.id, &params.command_text, params.scheduled_at) {
            Ok(id) => {
                let delay_seconds = params.scheduled_at.saturating_sub(now) / 1000;
                if let Err(e) = crate::agent::tmux::schedule_tmux_command(
                    &session_name,
                    &params.command_text,
                    delay_seconds,
                    id,
                    &self.db_path,
                ) {
                    tracing::warn!("Failed to set tmux timer for command {id}: {e}");
                }

                let cmd = ScheduledCommand {
                    id,
                    session_id: session.id,
                    command_text: params.command_text,
                    scheduled_at: params.scheduled_at,
                    created_at: now,
                    executed_at: None,
                    cancelled_at: None,
                };
                json_text(&scheduled_command_to_response(&cmd))
            }
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(description = "List pending scheduled commands, optionally filtered by session UUID")]
    fn list_scheduled_commands(
        &self,
        Parameters(params): Parameters<ListScheduledCommandsParams>,
    ) -> String {
        let db = self.db.lock().unwrap();

        let commands = match db.list_pending_scheduled_commands() {
            Ok(cmds) => cmds,
            Err(e) => return error_json(&e.to_string()),
        };

        let filtered: Vec<_> = if let Some(ref session_id) = params.session {
            let session = match resolve_session(&db, session_id) {
                Ok(s) => s,
                Err(e) => return e,
            };
            commands
                .into_iter()
                .filter(|c| c.session_id == session.id)
                .collect()
        } else {
            commands
        };

        let responses: Vec<_> = filtered.iter().map(scheduled_command_to_response).collect();
        json_text(&responses)
    }

    #[tool(description = "Get a scheduled command by ID")]
    fn get_scheduled_command(
        &self,
        Parameters(params): Parameters<GetScheduledCommandParams>,
    ) -> String {
        let db = self.db.lock().unwrap();

        match db.get_scheduled_command(params.id) {
            Ok(Some(cmd)) => json_text(&scheduled_command_to_response(&cmd)),
            Ok(None) => error_json(&format!("Scheduled command not found: {}", params.id)),
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(description = "Cancel a pending scheduled command")]
    fn cancel_scheduled_command(
        &self,
        Parameters(params): Parameters<CancelScheduledCommandParams>,
    ) -> String {
        let db = self.db.lock().unwrap();

        match db.cancel_scheduled_command(params.id) {
            Ok(true) => serde_json::json!({
                "cancelled": true,
                "id": params.id,
            })
            .to_string(),
            Ok(false) => error_json("Command not found or already executed/cancelled"),
            Err(e) => error_json(&e.to_string()),
        }
    }
}

fn scheduled_command_to_response(cmd: &ScheduledCommand) -> ScheduledCommandResponse {
    let status = if cmd.cancelled_at.is_some() {
        "cancelled"
    } else if cmd.executed_at.is_some() {
        "executed"
    } else {
        "pending"
    };
    ScheduledCommandResponse {
        id: cmd.id,
        session_id: cmd.session_id.to_string(),
        command_text: cmd.command_text.clone(),
        scheduled_at: cmd.scheduled_at,
        created_at: cmd.created_at,
        status: status.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::mcp::types::{McpServerInput, RoleInput};
    use crate::storage::Database;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn test_server() -> ThurboxMcp {
        let db = Database::open_in_memory().unwrap();
        ThurboxMcp {
            db: Mutex::new(db),
            db_path: PathBuf::from(":memory:"),
        }
    }

    fn parse_json(s: &str) -> serde_json::Value {
        serde_json::from_str(s).unwrap()
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

    // ── Role tool tests (global) ─────────────────────────────────

    #[test]
    fn list_roles_empty() {
        let server = test_server();
        let result = server.list_roles();
        let v = parse_json(&result);
        assert_eq!(v, serde_json::json!([]));
    }

    #[test]
    fn set_and_list_roles() {
        let server = test_server();

        let result = server.set_roles(Parameters(SetRolesParams {
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

        let result = server.list_roles();
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
    fn set_roles_empty_clears_all() {
        let server = test_server();

        // Set one role.
        server.set_roles(Parameters(SetRolesParams {
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
        let result = server.set_roles(Parameters(SetRolesParams { roles: vec![] }));
        let v = parse_json(&result);
        assert_eq!(v, serde_json::json!([]));

        // Verify list_roles also returns empty.
        let list_result = server.list_roles();
        let roles = parse_json(&list_result);
        assert_eq!(roles, serde_json::json!([]));
    }

    #[test]
    fn set_roles_replaces_existing() {
        let server = test_server();

        // Set initial roles.
        server.set_roles(Parameters(SetRolesParams {
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

        let result = server.set_roles(Parameters(SetRolesParams {
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

        let mut env = HashMap::new();
        env.insert("API_KEY".to_string(), "sk-secret".to_string());
        env.insert("DEBUG".to_string(), "1".to_string());

        let result = server.set_roles(Parameters(SetRolesParams {
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
        let list_result = server.list_roles();
        let listed = parse_json(&list_result);
        assert_eq!(listed[0]["env"]["API_KEY"], "sk-secret");
    }

    // ── MCP server tool tests ─────────────────────────────────────

    #[test]
    fn list_mcp_servers_empty() {
        let server = test_server();
        let result = server.list_mcp_servers();
        let v = parse_json(&result);
        assert_eq!(v, serde_json::json!([]));
    }

    #[test]
    fn set_and_list_mcp_servers() {
        let server = test_server();

        let result = server.set_mcp_servers(Parameters(SetMcpServersParams {
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

        let result = server.list_mcp_servers();
        let servers = parse_json(&result);
        assert_eq!(servers.as_array().unwrap().len(), 2);
    }

    #[test]
    fn set_mcp_servers_replaces_existing() {
        let server = test_server();
        server.set_mcp_servers(Parameters(SetMcpServersParams {
            servers: vec![McpServerInput {
                name: "old".to_string(),
                command: "old-cmd".to_string(),
                args: vec![],
                env: HashMap::new(),
            }],
        }));
        server.set_mcp_servers(Parameters(SetMcpServersParams {
            servers: vec![McpServerInput {
                name: "new".to_string(),
                command: "new-cmd".to_string(),
                args: vec![],
                env: HashMap::new(),
            }],
        }));

        let listed = parse_json(&server.list_mcp_servers());
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["name"], "new");
    }

    #[test]
    fn set_mcp_servers_empty_clears() {
        let server = test_server();
        server.set_mcp_servers(Parameters(SetMcpServersParams {
            servers: vec![McpServerInput {
                name: "temp".to_string(),
                command: "cmd".to_string(),
                args: vec![],
                env: HashMap::new(),
            }],
        }));
        server.set_mcp_servers(Parameters(SetMcpServersParams { servers: vec![] }));

        let listed = parse_json(&server.list_mcp_servers());
        assert_eq!(listed, serde_json::json!([]));
    }

    // ── Session tool tests ─────────────────────────────────────

    fn insert_test_session(server: &ThurboxMcp) -> SessionId {
        let db = server.db.lock().unwrap();
        let session = SharedSession {
            id: SessionId::default(),
            name: "1".to_string(),
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
    fn list_sessions_empty() {
        let server = test_server();
        let result = server.list_sessions(Parameters(ListSessionsParams {}));
        let v = parse_json(&result);
        assert_eq!(v, serde_json::json!([]));
    }

    #[test]
    fn get_session_by_uuid() {
        let server = test_server();
        let sid = insert_test_session(&server);

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
        let sid = insert_test_session(&server);

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
    fn restart_session_resolves_session_synchronously() {
        // Headless restart runs inline rather than enqueuing a command.
        // In environments without a live tmux server the tmux call fails,
        // which surfaces as a JSON `{"error": ...}` — but crucially the
        // handler no longer returns `queued: true`. We also verify that
        // unknown session IDs are reported as such (see
        // `restart_session_not_found`).
        let server = test_server();
        let sid = insert_test_session(&server);

        let result = server.restart_session(Parameters(RestartSessionParams {
            session: sid.to_string(),
        }));
        let v = parse_json(&result);
        assert!(v.get("queued").is_none(), "should not enqueue any more");
        // Either the restart succeeded (tmux available) or it surfaced a
        // synchronous error — never a `queued` response.
        let has_result = v.get("restarted").is_some() || v.get("error").is_some();
        assert!(has_result, "expected restarted or error, got {v}");
    }

    // ── Restore session tests ────────────────────────────────────

    #[test]
    fn restore_session_success() {
        let server = test_server();
        let sid = insert_test_session(&server);

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
        let sid = insert_test_session(&server);

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
        let result = server.list_vms(Parameters(ListVmsParams {}));
        let v = parse_json(&result);
        assert_eq!(v, serde_json::json!([]));
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
        let config = crate::session::VmConfig::default();

        {
            let db = server.db.lock().unwrap();
            db.insert_vm(
                "vm-1",
                None,
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

        let cf_dir = temp.path().join("admin").join("containerfiles");
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

        let cf_dir = temp
            .path()
            .join("admin")
            .join("containerfiles")
            .join("python");
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
    }

    #[test]
    fn delete_containerfile_template_success() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let server = test_server();

        let cf_dir = temp
            .path()
            .join("admin")
            .join("containerfiles")
            .join("custom");
        std::fs::create_dir_all(&cf_dir).unwrap();
        std::fs::write(cf_dir.join("Containerfile"), "FROM alpine").unwrap();

        let result = server.delete_containerfile_template(Parameters(
            super::super::types::DeleteContainerfileTemplateParams {
                name: "custom".to_string(),
            },
        ));
        let v = parse_json(&result);
        assert_eq!(v["deleted"], true);
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
}
