//! Tool implementations for the Thurbox MCP server.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::plugin_bridge::{client as control_client, ControlError};
use crate::session::{McpServerConfig, RoleConfig, RolePermissions, SessionId, SkillConfig};
use crate::storage::Database;
use crate::sync::SharedSession;

use crate::storage::vms::VmRecord;

use crate::session::ScheduledCommand;

use super::types::{
    CallPluginToolParams, CancelScheduledCommandParams, CaptureSessionOutputParams,
    ContainerfileTemplateResponse, ContainerfileTemplateSummary, ContributionsSummary,
    CreateSessionParams, DeleteContainerfileTemplateParams, DeleteSessionParams,
    DeleteVmImageParams, DisablePluginParams, DownloadVmImageParams, EnablePluginParams,
    GetContainerfileTemplateParams, GetPluginSettingParams, GetScheduledCommandParams,
    GetSessionParams, GetVmParams, InstallPluginParams, ListPluginSettingsParams,
    ListPluginToolsParams, ListScheduledCommandsParams, ListSessionsParams, ListVmsParams,
    McpServerResponse, PluginResponse, PluginSettingResponse, ProcessSummary, RegisterPluginParams,
    RegisterSkillParams, ResetPluginSettingParams, RestartSessionParams, RestoreSessionParams,
    RoleResponse, ScheduleCommandParams, ScheduledCommandResponse, SendPromptParams,
    SessionResponse, SetContainerfileTemplateParams, SetEditorCommandParams, SetKeybindingsParams,
    SetMcpServersParams, SetPluginSettingParams, SetPluginsParams, SetRolesParams, SetSkillsParams,
    SetThemeParams, SkillResponse, UninstallPluginParams, UnregisterPluginParams,
    UnregisterSkillParams, VmImageResponse, VmResponse, WorktreeResponse,
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

fn skill_to_response(s: &SkillConfig) -> SkillResponse {
    SkillResponse {
        name: s.name.clone(),
        path: s.path.clone(),
        source: None,
    }
}

fn effective_skill_to_response(
    s: &SkillConfig,
    source: crate::storage::SkillSource,
) -> SkillResponse {
    SkillResponse {
        name: s.name.clone(),
        path: s.path.clone(),
        source: Some(source),
    }
}

/// Convert a `toml::Value` to `serde_json::Value` for response payloads.
fn toml_to_json(v: &toml::Value) -> serde_json::Value {
    match v {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::Value::Number((*i).into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
        toml::Value::Array(a) => serde_json::Value::Array(a.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => serde_json::Value::Object(
            t.iter()
                .map(|(k, v)| (k.clone(), toml_to_json(v)))
                .collect(),
        ),
    }
}

/// Convert a `serde_json::Value` (from a tool input parameter) to `toml::Value`
/// so it can be validated against a `ConfigurationSchema` and stored.
fn json_to_toml(v: &serde_json::Value) -> Result<toml::Value, String> {
    match v {
        serde_json::Value::Null => Err("null is not supported in plugin settings".into()),
        serde_json::Value::Bool(b) => Ok(toml::Value::Boolean(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(toml::Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Ok(toml::Value::Float(f))
            } else {
                Err(format!("unsupported number: {n}"))
            }
        }
        serde_json::Value::String(s) => Ok(toml::Value::String(s.clone())),
        serde_json::Value::Array(arr) => {
            let items: Result<Vec<_>, _> = arr.iter().map(json_to_toml).collect();
            Ok(toml::Value::Array(items?))
        }
        serde_json::Value::Object(obj) => {
            let mut table = toml::value::Table::new();
            for (k, val) in obj {
                table.insert(k.clone(), json_to_toml(val)?);
            }
            Ok(toml::Value::Table(table))
        }
    }
}

fn configuration_type_str(ty: crate::session::ConfigurationType) -> &'static str {
    match ty {
        crate::session::ConfigurationType::String => "string",
        crate::session::ConfigurationType::Int => "int",
        crate::session::ConfigurationType::Bool => "bool",
        crate::session::ConfigurationType::Duration => "duration",
        crate::session::ConfigurationType::Path => "path",
        crate::session::ConfigurationType::Enum => "enum",
    }
}

/// Project a registered/disk plugin row + its manifest (loaded on demand) into
/// the response shape exposed via MCP. A failed manifest parse becomes an
/// `error` field — the row still appears so the user can see the plugin needs
/// fixing, with empty contributions.
fn plugin_to_response(
    plugin: &crate::session::PluginConfig,
    source: crate::storage::PluginSource,
) -> PluginResponse {
    let mut response = PluginResponse {
        name: plugin.name.clone(),
        path: plugin.path.clone(),
        version: plugin.version.clone(),
        enabled: plugin.enabled,
        source,
        contributions: ContributionsSummary::default(),
        process: None,
        error: None,
    };
    match crate::session::PluginManifest::load(&plugin.path) {
        Ok(manifest) => {
            response.contributions = ContributionsSummary {
                skills: manifest
                    .contributes
                    .skills
                    .iter()
                    .map(|c| c.name.clone())
                    .collect(),
                roles: manifest
                    .contributes
                    .roles
                    .iter()
                    .map(|c| c.name.clone())
                    .collect(),
                mcp_servers: manifest
                    .contributes
                    .mcp_servers
                    .iter()
                    .map(|c| c.name.clone())
                    .collect(),
                themes: manifest
                    .contributes
                    .themes
                    .iter()
                    .map(|c| c.name.clone())
                    .collect(),
                configuration_keys: manifest
                    .contributes
                    .configuration
                    .iter()
                    .map(|c| c.key.clone())
                    .collect(),
            };
            if let Some(p) = manifest.process {
                response.process = Some(ProcessSummary {
                    exec: p.exec,
                    capabilities: p
                        .capabilities
                        .iter()
                        .map(|c| match c {
                            crate::session::Capability::Backend => "backend".to_string(),
                            crate::session::Capability::McpTools => "mcp-tools".to_string(),
                        })
                        .collect(),
                    activation_events: p
                        .activation_events
                        .iter()
                        .map(|e| e.as_manifest_string())
                        .collect(),
                });
            }
            if response.version.is_empty() {
                response.version = manifest.version;
            }
        }
        Err(e) => response.error = Some(e),
    }
    response
}

/// Recursively copy `src` into `dst` (which must not yet exist). Mirrors
/// `cp -r src dst` semantics. Used by `install_plugin`.
fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else if ft.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
        // Ignore symlinks and special files — plugins are TOML/text/exec.
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
        description = "List the built-in TUI theme presets. Returns a JSON array of {id, display_name} objects."
    )]
    fn list_themes(&self) -> String {
        let presets: Vec<serde_json::Value> = crate::session::ThemePreset::all()
            .iter()
            .map(|p| serde_json::json!({ "id": p.as_str(), "display_name": p.display_name() }))
            .collect();
        serde_json::json!(presets).to_string()
    }

    #[tool(
        description = "Get the currently active TUI theme. Returns {\"name\": \"...\"} or {\"name\": null} if unset (in which case the Default preset is used)."
    )]
    fn get_theme(&self) -> String {
        let db = self.db.lock().unwrap();
        match db.get_active_theme() {
            Ok(name) => serde_json::json!({ "name": name }).to_string(),
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Set the active TUI theme by preset id. Other Thurbox processes (e.g. the running TUI) pick up the change automatically via SQLite data-version polling."
    )]
    fn set_theme(&self, Parameters(params): Parameters<SetThemeParams>) -> String {
        if crate::session::ThemePreset::from_str(&params.name).is_none() {
            return error_json(&format!(
                "Unknown theme preset '{}'. Run list_themes for valid ids.",
                params.name
            ));
        }
        let db = self.db.lock().unwrap();
        match db.set_active_theme(&params.name) {
            Ok(()) => serde_json::json!({ "name": params.name }).to_string(),
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Get the user's keybindings JSON document, or the default document when no override file exists. Returns {\"json\": \"...\"}."
    )]
    fn get_keybindings(&self) -> String {
        let json = match crate::storage::keybindings::load_keybindings_json() {
            Ok(Some(s)) => s,
            Ok(None) => match crate::session::KeyBindings::default().to_json() {
                Ok(s) => s,
                Err(e) => return error_json(&e),
            },
            Err(e) => return error_json(&e),
        };
        serde_json::json!({ "json": json }).to_string()
    }

    #[tool(
        description = "Replace the user's keybindings JSON document at ~/.config/thurbox/keybindings.json. Shape: { \"<Action>\": [\"<chord>\", ...] }. Unknown actions and unparseable chords are dropped on load. The change takes effect at the next TUI restart."
    )]
    fn set_keybindings(&self, Parameters(params): Parameters<SetKeybindingsParams>) -> String {
        // Validate by attempting to parse before persisting.
        if let Err(e) = crate::session::KeyBindings::from_json(&params.json) {
            return error_json(&format!("Invalid keybindings JSON: {e}"));
        }
        match crate::storage::keybindings::save_keybindings_json(&params.json) {
            Ok(()) => serde_json::json!({ "saved": true }).to_string(),
            Err(e) => error_json(&e),
        }
    }

    #[tool(
        description = "Delete the user's keybindings override file, restoring built-in defaults at the next TUI restart."
    )]
    fn reset_keybindings(&self) -> String {
        match crate::storage::keybindings::delete_keybindings_json() {
            Ok(()) => serde_json::json!({ "reset": true }).to_string(),
            Err(e) => error_json(&e),
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
        description = "Delete a session. By default only the DB row is soft-deleted and the TUI (when running) will clean up the tmux pane and worktree on next sync. Pass `force: true` to also kill the tmux window, remove worktrees, and cancel pending scheduled commands immediately — use this for headless cleanup when no TUI is attached."
    )]
    fn delete_session(&self, Parameters(params): Parameters<DeleteSessionParams>) -> String {
        let db = self.db.lock().unwrap();
        let session = match resolve_session(&db, &params.session) {
            Ok(s) => s,
            Err(e) => return e,
        };

        match crate::session_ops::delete_session_headless(&db, session.id, params.force) {
            Ok(report) => serde_json::json!({
                "deleted": true,
                "id": session.id.to_string(),
                "name": session.name,
                "forced": params.force,
                "killed_window": report.killed_window,
                "removed_worktrees": report.removed_worktrees,
                "worktree_errors": report.worktree_errors,
                "cancelled_scheduled": report.cancelled_scheduled,
            })
            .to_string(),
            Err(e) => error_json(&e),
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
            model: params.model,
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
        match crate::paths::list_containerfile_templates() {
            Ok(rows) => {
                let templates: Vec<ContainerfileTemplateSummary> = rows
                    .into_iter()
                    .map(|(name, files)| ContainerfileTemplateSummary { name, files })
                    .collect();
                json_text(&templates)
            }
            Err(e) => error_json(&e),
        }
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

    // ── Skill Registry Tools ────────────────────────────────────

    #[tool(
        description = "List all effective skills. Returns a JSON array of {name, path, source} objects, where `source` is \"disk\" for skills auto-discovered under ~/.local/share/thurbox/admin/skills/ and \"registered\" for SQLite registry entries. Registered entries shadow disk-source entries of the same name. Thurbox never creates, edits, or deletes skill files through these tools."
    )]
    fn list_skills(&self) -> String {
        let db = self.db.lock().unwrap();
        match db.list_effective_skills() {
            Ok(skills) => {
                let resp: Vec<SkillResponse> = skills
                    .iter()
                    .map(|(s, source)| effective_skill_to_response(s, source.clone()))
                    .collect();
                json_text(&resp)
            }
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Atomically replace all registered skills. Deletes existing registry entries and inserts the provided list in a single transaction. To add a skill, include all existing skills plus the new one. To clear the registry, pass an empty array. Each entry's path must be absolute, exist, and contain a SKILL.md file. Disk files are never modified."
    )]
    fn set_skills(&self, Parameters(params): Parameters<SetSkillsParams>) -> String {
        let db = self.db.lock().unwrap();

        let mut skills: Vec<SkillConfig> = Vec::with_capacity(params.skills.len());
        for s in params.skills {
            if let Err(e) = validate_safe_name(&s.name) {
                return e;
            }
            let path = std::path::PathBuf::from(&s.path);
            if let Err(e) = crate::paths::validate_skill_path(&path) {
                return error_json(&format!("skill '{}': {e}", s.name));
            }
            skills.push(SkillConfig { name: s.name, path });
        }

        match db.replace_global_skills(&skills) {
            Ok(()) => {
                let resp: Vec<SkillResponse> = skills.iter().map(skill_to_response).collect();
                json_text(&resp)
            }
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Register (or update) a single skill by name, pointing at an existing on-disk skill directory. The path must be absolute, exist, and contain a SKILL.md file. Disk files are never modified."
    )]
    fn register_skill(&self, Parameters(params): Parameters<RegisterSkillParams>) -> String {
        if let Err(e) = validate_safe_name(&params.name) {
            return e;
        }
        let path = std::path::PathBuf::from(&params.path);
        if let Err(e) = crate::paths::validate_skill_path(&path) {
            return error_json(&e);
        }
        let db = self.db.lock().unwrap();
        let skill = SkillConfig {
            name: params.name,
            path,
        };
        match db.upsert_global_skill(&skill) {
            Ok(()) => json_text(&skill_to_response(&skill)),
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Unregister a skill by name. Removes only the registry entry; on-disk skill files are never touched. Returns {\"deleted\": true, \"name\": ...} if a row was removed, or an error if no such skill was registered."
    )]
    fn unregister_skill(&self, Parameters(params): Parameters<UnregisterSkillParams>) -> String {
        let db = self.db.lock().unwrap();
        match db.delete_global_skill(&params.name) {
            Ok(true) => serde_json::json!({ "deleted": true, "name": params.name }).to_string(),
            Ok(false) => error_json(&format!("Skill not registered: {}", params.name)),
            Err(e) => error_json(&e.to_string()),
        }
    }

    // ── Plugin Lifecycle Tools ──────────────────────────────────

    #[tool(
        description = "List all effective plugins (auto-discovered under ~/.local/share/thurbox/admin/plugins/ plus SQLite registry entries). Returns a JSON array of {name, path, version, enabled, source, contributions, process?, error?} objects."
    )]
    fn list_plugins(&self) -> String {
        let db = self.db.lock().unwrap();
        match db.list_effective_plugins() {
            Ok(plugins) => {
                let resp: Vec<PluginResponse> = plugins
                    .into_iter()
                    .map(|(p, src)| plugin_to_response(&p, src))
                    .collect();
                json_text(&resp)
            }
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Atomically replace the plugin registry. Deletes existing rows and inserts the provided list in a single transaction. Disk-source plugins are unaffected. To clear the registry, pass an empty array."
    )]
    fn set_plugins(&self, Parameters(params): Parameters<SetPluginsParams>) -> String {
        let mut plugins = Vec::with_capacity(params.plugins.len());
        for p in params.plugins {
            if let Err(e) = validate_safe_name(&p.name) {
                return e;
            }
            let path = std::path::PathBuf::from(&p.path);
            if !path.is_absolute() {
                return error_json(&format!("plugin '{}': path must be absolute", p.name));
            }
            let manifest = match crate::session::PluginManifest::load(&path) {
                Ok(m) => m,
                Err(e) => return error_json(&format!("plugin '{}': {e}", p.name)),
            };
            plugins.push(crate::session::PluginConfig {
                name: p.name,
                path,
                version: p.version.unwrap_or(manifest.version),
                enabled: p.enabled,
            });
        }
        let db = self.db.lock().unwrap();
        match db.replace_global_plugins(&plugins) {
            Ok(()) => json_text(&plugins.iter().map(|p| &p.name).collect::<Vec<_>>()),
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Register (or update) a single plugin by name, pointing at an existing on-disk plugin directory. The path must be absolute and contain a valid thurbox-plugin.toml. Disk files are never modified."
    )]
    fn register_plugin(&self, Parameters(params): Parameters<RegisterPluginParams>) -> String {
        if let Err(e) = validate_safe_name(&params.name) {
            return e;
        }
        let path = std::path::PathBuf::from(&params.path);
        if !path.is_absolute() {
            return error_json("path must be absolute");
        }
        let manifest = match crate::session::PluginManifest::load(&path) {
            Ok(m) => m,
            Err(e) => return error_json(&e),
        };
        if manifest.name != params.name {
            return error_json(&format!(
                "manifest name '{}' does not match registration name '{}'",
                manifest.name, params.name
            ));
        }
        let plugin = crate::session::PluginConfig {
            name: params.name,
            path,
            version: manifest.version,
            enabled: true,
        };
        let db = self.db.lock().unwrap();
        match db.upsert_global_plugin(&plugin) {
            Ok(()) => json_text(&plugin_to_response(
                &plugin,
                crate::storage::PluginSource::Registered,
            )),
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Unregister a plugin by name. Removes only the registry row (cascades plugin_settings); on-disk files are never touched. Use uninstall_plugin to remove disk files."
    )]
    fn unregister_plugin(&self, Parameters(params): Parameters<UnregisterPluginParams>) -> String {
        let db = self.db.lock().unwrap();
        match db.delete_global_plugin(&params.name) {
            Ok(true) => serde_json::json!({ "deleted": true, "name": params.name }).to_string(),
            Ok(false) => error_json(&format!("Plugin not registered: {}", params.name)),
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Enable a plugin by name. For disk-only plugins, creates a shadow registry row so the flag survives restarts. Errors if the plugin doesn't exist on disk and isn't registered."
    )]
    fn enable_plugin(&self, Parameters(params): Parameters<EnablePluginParams>) -> String {
        let db = self.db.lock().unwrap();
        match db.set_plugin_enabled(&params.name, true) {
            Ok(()) => serde_json::json!({ "enabled": true, "name": params.name }).to_string(),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                error_json(&format!("Plugin not found: {}", params.name))
            }
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Disable a plugin by name. The plugin's contributions are dropped from effective lists; a running process plugin would be stopped (process runtime forthcoming). Shadow row is created for disk-only plugins so the flag survives restarts."
    )]
    fn disable_plugin(&self, Parameters(params): Parameters<DisablePluginParams>) -> String {
        let db = self.db.lock().unwrap();
        match db.set_plugin_enabled(&params.name, false) {
            Ok(()) => serde_json::json!({ "enabled": false, "name": params.name }).to_string(),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                error_json(&format!("Plugin not found: {}", params.name))
            }
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Install a plugin by copying a source directory into ~/.local/share/thurbox/admin/plugins/<name>/. The source must contain a valid thurbox-plugin.toml. Returns an error if the destination already exists. After install, the plugin is auto-discovered (no register call needed)."
    )]
    fn install_plugin(&self, Parameters(params): Parameters<InstallPluginParams>) -> String {
        let source = std::path::PathBuf::from(&params.source_path);
        if !source.is_dir() {
            return error_json("source_path must be an existing directory");
        }
        let manifest = match crate::session::PluginManifest::load(&source) {
            Ok(m) => m,
            Err(e) => return error_json(&format!("Invalid plugin: {e}")),
        };
        let install_name = params.name.unwrap_or_else(|| manifest.name.clone());
        if let Err(e) = validate_safe_name(&install_name) {
            return e;
        }
        let plugins_dir = match crate::paths::plugins_directory() {
            Some(d) => d,
            None => return error_json("Could not resolve plugins directory"),
        };
        if let Err(e) = std::fs::create_dir_all(&plugins_dir) {
            return error_json(&format!("Failed to create plugins directory: {e}"));
        }
        let dest = plugins_dir.join(&install_name);
        if dest.exists() {
            return error_json(&format!(
                "Plugin already installed at {} — uninstall first or pick a different name",
                dest.display()
            ));
        }
        if let Err(e) = copy_dir_all(&source, &dest) {
            // Best-effort cleanup on partial copy.
            let _ = std::fs::remove_dir_all(&dest);
            return error_json(&format!("Copy failed: {e}"));
        }
        // Re-validate after copy in case something didn't survive.
        if let Err(e) = crate::session::PluginManifest::load(&dest) {
            let _ = std::fs::remove_dir_all(&dest);
            return error_json(&format!("Post-install validation failed: {e}"));
        }
        serde_json::json!({
            "installed": true,
            "name": install_name,
            "path": dest.to_string_lossy(),
        })
        .to_string()
    }

    #[tool(
        description = "Uninstall a plugin: removes the directory under ~/.local/share/thurbox/admin/plugins/ AND deletes the registry row (cascades plugin_settings). Refuses if confirm != true. For plugins registered outside the admin dir, only the registry row is removed."
    )]
    fn uninstall_plugin(&self, Parameters(params): Parameters<UninstallPluginParams>) -> String {
        if !params.confirm {
            return error_json("uninstall_plugin requires confirm = true");
        }
        if let Err(e) = validate_safe_name(&params.name) {
            return e;
        }
        let db = self.db.lock().unwrap();
        // Resolve the path: registry takes precedence, else assume admin dir.
        let registered = match db.list_global_plugins() {
            Ok(rows) => rows.into_iter().find(|p| p.name == params.name),
            Err(e) => return error_json(&e.to_string()),
        };
        let plugins_dir = match crate::paths::plugins_directory() {
            Some(d) => d,
            None => return error_json("Could not resolve plugins directory"),
        };
        let disk_path = registered
            .as_ref()
            .map(|p| p.path.clone())
            .unwrap_or_else(|| plugins_dir.join(&params.name));
        let mut removed_disk = false;
        if disk_path.starts_with(&plugins_dir) && disk_path.exists() {
            if let Err(e) = std::fs::remove_dir_all(&disk_path) {
                return error_json(&format!("Failed to remove plugin directory: {e}"));
            }
            removed_disk = true;
        }
        let removed_row = match db.delete_global_plugin(&params.name) {
            Ok(b) => b,
            Err(e) => return error_json(&e.to_string()),
        };
        if !removed_disk && !removed_row {
            return error_json(&format!("Plugin not found: {}", params.name));
        }
        serde_json::json!({
            "uninstalled": true,
            "name": params.name,
            "removed_disk": removed_disk,
            "removed_registry": removed_row,
        })
        .to_string()
    }

    // ── Plugin Configuration Tools ──────────────────────────────

    #[tool(
        description = "List a plugin's configuration schema with current effective values. Returns a JSON array of {key, type, default, user_value?, effective_value, description} objects. Type is one of: string, int, bool, duration, path, enum."
    )]
    fn list_plugin_settings(
        &self,
        Parameters(params): Parameters<ListPluginSettingsParams>,
    ) -> String {
        let db = self.db.lock().unwrap();
        let plugin = match db.list_global_plugins() {
            Ok(rows) => rows.into_iter().find(|p| p.name == params.plugin_name),
            Err(e) => return error_json(&e.to_string()),
        };
        let path = match plugin {
            Some(p) => p.path,
            None => match crate::paths::plugins_directory() {
                Some(d) => d.join(&params.plugin_name),
                None => return error_json("Could not resolve plugins directory"),
            },
        };
        let manifest = match crate::session::PluginManifest::load(&path) {
            Ok(m) => m,
            Err(e) => return error_json(&format!("Failed to load manifest: {e}")),
        };
        let schema = manifest.contributes.configuration;
        let effective = match db.list_plugin_settings_with_defaults(&params.plugin_name, &schema) {
            Ok(v) => v,
            Err(e) => return error_json(&e.to_string()),
        };
        let resp: Vec<PluginSettingResponse> = effective
            .into_iter()
            .map(|s| PluginSettingResponse {
                key: s.key,
                ty: configuration_type_str(s.ty).to_string(),
                default: toml_to_json(&s.default),
                user_value: s.user_value.as_ref().map(toml_to_json),
                effective_value: toml_to_json(&s.effective_value),
                description: s.description,
            })
            .collect();
        json_text(&resp)
    }

    #[tool(
        description = "Get the current effective value for one plugin setting. Returns the user override if set, otherwise the manifest default. Errors if the key isn't declared in the manifest."
    )]
    fn get_plugin_setting(&self, Parameters(params): Parameters<GetPluginSettingParams>) -> String {
        let db = self.db.lock().unwrap();
        let plugin = match db.list_global_plugins() {
            Ok(rows) => rows.into_iter().find(|p| p.name == params.plugin_name),
            Err(e) => return error_json(&e.to_string()),
        };
        let path = match plugin {
            Some(p) => p.path,
            None => match crate::paths::plugins_directory() {
                Some(d) => d.join(&params.plugin_name),
                None => return error_json("Could not resolve plugins directory"),
            },
        };
        let manifest = match crate::session::PluginManifest::load(&path) {
            Ok(m) => m,
            Err(e) => return error_json(&format!("Failed to load manifest: {e}")),
        };
        let Some(schema) = manifest
            .contributes
            .configuration
            .into_iter()
            .find(|c| c.key == params.key)
        else {
            return error_json(&format!(
                "Setting '{}' not declared in manifest",
                params.key
            ));
        };
        let user_value = match db.get_plugin_setting(&params.plugin_name, &params.key) {
            Ok(v) => v,
            Err(e) => return error_json(&e.to_string()),
        };
        let effective = user_value.unwrap_or(schema.default);
        json_text(&toml_to_json(&effective))
    }

    #[tool(
        description = "Set a user override for one plugin setting. The value is validated against the manifest's declared type, min/max, and enum values; out-of-range or wrong-type values are rejected. Pass JSON: a string, integer, or boolean. Returns the new effective value."
    )]
    fn set_plugin_setting(&self, Parameters(params): Parameters<SetPluginSettingParams>) -> String {
        let value = match json_to_toml(&params.value) {
            Ok(v) => v,
            Err(e) => return error_json(&format!("Invalid value: {e}")),
        };
        let db = self.db.lock().unwrap();
        let plugin = match db.list_global_plugins() {
            Ok(rows) => rows.into_iter().find(|p| p.name == params.plugin_name),
            Err(e) => return error_json(&e.to_string()),
        };
        let path = match plugin {
            Some(p) => p.path,
            None => match crate::paths::plugins_directory() {
                Some(d) => d.join(&params.plugin_name),
                None => return error_json("Could not resolve plugins directory"),
            },
        };
        let manifest = match crate::session::PluginManifest::load(&path) {
            Ok(m) => m,
            Err(e) => return error_json(&format!("Failed to load manifest: {e}")),
        };
        let Some(schema) = manifest
            .contributes
            .configuration
            .into_iter()
            .find(|c| c.key == params.key)
        else {
            return error_json(&format!(
                "Setting '{}' not declared in manifest",
                params.key
            ));
        };
        if let Err(e) = schema.validate_value(&value) {
            return error_json(&format!("Validation failed: {e}"));
        }
        if let Err(e) = db.upsert_plugin_setting(&params.plugin_name, &params.key, &value) {
            return error_json(&e.to_string());
        }
        json_text(&toml_to_json(&value))
    }

    #[tool(
        description = "Clear a user override, restoring the manifest default. Returns {\"reset\": true, ...}; or {\"reset\": false, ...} if no override existed."
    )]
    fn reset_plugin_setting(
        &self,
        Parameters(params): Parameters<ResetPluginSettingParams>,
    ) -> String {
        let db = self.db.lock().unwrap();
        match db.reset_plugin_setting(&params.plugin_name, &params.key) {
            Ok(removed) => serde_json::json!({
                "reset": removed,
                "plugin_name": params.plugin_name,
                "key": params.key,
            })
            .to_string(),
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

    // ── Plugin Runtime Tools (via TUI control socket) ────────────

    #[tool(
        description = "List the tools a plugin exposes via its `mcp-tools` capability. Returns {\"tools\": [...], \"source\": \"manifest\"|\"runtime\"}. When the plugin's manifest declares [[contributes.mcp_tools]] entries, thurbox answers from the manifest without waking the plugin (source=manifest); otherwise it asks the running plugin's mcp.list_tools op (source=runtime). Errors if the TUI isn't running (no control socket) or the plugin isn't running with the mcp-tools capability."
    )]
    fn list_plugin_tools(&self, Parameters(params): Parameters<ListPluginToolsParams>) -> String {
        let Some(client) = control_client::default_client() else {
            return error_json("control socket path unavailable (no runtime directory)");
        };
        match run_control_call(
            || async move { client.list_plugin_tools(&params.plugin_name).await },
        ) {
            Ok(v) => v.to_string(),
            Err(e) => error_json(&e.to_string()),
        }
    }

    #[tool(
        description = "Invoke a tool on a running plugin. `args` is forwarded verbatim as the tool's MCP input (shape defined by its input_schema). Returns the tool's raw result. Errors if the TUI isn't running, the plugin isn't running, or the tool call times out (60s bound per call)."
    )]
    fn call_plugin_tool(&self, Parameters(params): Parameters<CallPluginToolParams>) -> String {
        let Some(client) = control_client::default_client() else {
            return error_json("control socket path unavailable (no runtime directory)");
        };
        match run_control_call(|| async move {
            client
                .call_plugin_tool(&params.plugin_name, &params.tool, params.args)
                .await
        }) {
            Ok(v) => v.to_string(),
            Err(e) => error_json(&e.to_string()),
        }
    }
}

/// Bridge a synchronous MCP tool handler into the tokio runtime so the
/// async [`ControlClient`] calls can complete. `block_in_place` is required
/// — we're running on a worker thread and a plain `block_on` would deadlock.
fn run_control_call<F, Fut, T>(f: F) -> Result<T, ControlError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, ControlError>>,
{
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(f()))
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
    use crate::mcp::types::{McpServerInput, RoleInput, SkillInput};
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
            model: None,
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
            force: false,
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
            force: false,
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
            force: false,
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

    // ── Skill Registry Tests ────────────────────────────────────

    /// Create `<root>/<name>/SKILL.md` and return the skill directory.
    fn make_skill_dir(root: &std::path::Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "---\nname: x\n---\n").unwrap();
        dir
    }

    #[test]
    fn list_skills_empty() {
        let server = test_server();
        let result = server.list_skills();
        let v = parse_json(&result);
        assert_eq!(v.as_array().unwrap().len(), 0);
    }

    #[test]
    fn register_skill_then_list() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = make_skill_dir(temp.path(), "domain-cli");
        let server = test_server();

        let result = server.register_skill(Parameters(RegisterSkillParams {
            name: "domain-cli".to_string(),
            path: path.to_string_lossy().into_owned(),
        }));
        let v = parse_json(&result);
        assert_eq!(v["name"], "domain-cli");

        let listed = parse_json(&server.list_skills());
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["name"], "domain-cli");
    }

    #[test]
    fn register_skill_rejects_missing_skill_md() {
        let temp = tempfile::TempDir::new().unwrap();
        let empty = temp.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let server = test_server();

        let result = server.register_skill(Parameters(RegisterSkillParams {
            name: "x".to_string(),
            path: empty.to_string_lossy().into_owned(),
        }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("SKILL.md"));
    }

    #[test]
    fn register_skill_rejects_relative_path() {
        let server = test_server();
        let result = server.register_skill(Parameters(RegisterSkillParams {
            name: "x".to_string(),
            path: "relative/path".to_string(),
        }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("absolute"));
    }

    #[test]
    fn register_skill_rejects_invalid_name() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = make_skill_dir(temp.path(), "ok-dir");
        let server = test_server();

        let result = server.register_skill(Parameters(RegisterSkillParams {
            name: "../escape".to_string(),
            path: path.to_string_lossy().into_owned(),
        }));
        let v = parse_json(&result);
        assert!(v["error"].is_string());
    }

    #[test]
    fn set_skills_replaces_all() {
        let temp = tempfile::TempDir::new().unwrap();
        let a = make_skill_dir(temp.path(), "a");
        let b = make_skill_dir(temp.path(), "b");
        let c = make_skill_dir(temp.path(), "c");
        let server = test_server();

        server.set_skills(Parameters(SetSkillsParams {
            skills: vec![
                SkillInput {
                    name: "a".to_string(),
                    path: a.to_string_lossy().into_owned(),
                },
                SkillInput {
                    name: "b".to_string(),
                    path: b.to_string_lossy().into_owned(),
                },
            ],
        }));

        let result = server.set_skills(Parameters(SetSkillsParams {
            skills: vec![SkillInput {
                name: "c".to_string(),
                path: c.to_string_lossy().into_owned(),
            }],
        }));
        let v = parse_json(&result);
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(v[0]["name"], "c");
    }

    #[test]
    fn set_skills_empty_clears() {
        let temp = tempfile::TempDir::new().unwrap();
        let a = make_skill_dir(temp.path(), "a");
        let server = test_server();

        server.set_skills(Parameters(SetSkillsParams {
            skills: vec![SkillInput {
                name: "a".to_string(),
                path: a.to_string_lossy().into_owned(),
            }],
        }));
        server.set_skills(Parameters(SetSkillsParams { skills: vec![] }));

        let listed = parse_json(&server.list_skills());
        assert_eq!(listed.as_array().unwrap().len(), 0);
    }

    #[test]
    fn unregister_skill_removes_entry() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = make_skill_dir(temp.path(), "s");
        let server = test_server();

        server.register_skill(Parameters(RegisterSkillParams {
            name: "s".to_string(),
            path: path.to_string_lossy().into_owned(),
        }));

        let result = server.unregister_skill(Parameters(UnregisterSkillParams {
            name: "s".to_string(),
        }));
        let v = parse_json(&result);
        assert_eq!(v["deleted"], true);
        assert_eq!(v["name"], "s");

        // Skill directory on disk must still exist — registry removal
        // never touches disk files.
        assert!(path.join("SKILL.md").is_file());
    }

    #[test]
    fn unregister_skill_unknown_returns_error() {
        let server = test_server();
        let result = server.unregister_skill(Parameters(UnregisterSkillParams {
            name: "nope".to_string(),
        }));
        let v = parse_json(&result);
        assert!(v["error"].as_str().unwrap().contains("not registered"));
    }

    // ── Plugin tool tests ───────────────────────────────────────

    fn make_plugin_dir(root: &std::path::Path, name: &str, with_config: bool) -> PathBuf {
        let p = root.join(name);
        std::fs::create_dir_all(&p).unwrap();
        let mut manifest =
            format!("name = \"{name}\"\nversion = \"0.1.0\"\nthurbox_plugin_api = 1\n");
        if with_config {
            manifest.push_str(
                "[[contributes.configuration]]\n\
                 key = \"workers\"\n\
                 type = \"int\"\n\
                 default = 3\n\
                 min = 1\n\
                 max = 16\n",
            );
        }
        std::fs::write(p.join("thurbox-plugin.toml"), manifest).unwrap();
        p
    }

    #[test]
    fn list_plugins_includes_disk_source() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let dir = crate::paths::plugins_directory().unwrap();
        make_plugin_dir(&dir, "alpha", false);

        let server = test_server();
        let v = parse_json(&server.list_plugins());
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "alpha");
        assert_eq!(arr[0]["source"], "disk");
        assert_eq!(arr[0]["enabled"], true);
    }

    #[test]
    fn register_plugin_then_list() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let path = make_plugin_dir(temp.path(), "beta", false);

        let server = test_server();
        let v = parse_json(&server.register_plugin(Parameters(RegisterPluginParams {
            name: "beta".to_string(),
            path: path.to_string_lossy().into_owned(),
        })));
        assert_eq!(v["name"], "beta");
        assert_eq!(v["source"], "registered");
        assert_eq!(v["version"], "0.1.0");
    }

    #[test]
    fn register_plugin_rejects_name_mismatch() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let path = make_plugin_dir(temp.path(), "actual", false);

        let server = test_server();
        let v = parse_json(&server.register_plugin(Parameters(RegisterPluginParams {
            name: "claimed".to_string(),
            path: path.to_string_lossy().into_owned(),
        })));
        assert!(v["error"].as_str().unwrap().contains("does not match"));
    }

    #[test]
    fn install_plugin_copies_into_admin_dir() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let source = make_plugin_dir(temp.path(), "source", false);

        let server = test_server();
        let v = parse_json(&server.install_plugin(Parameters(InstallPluginParams {
            source_path: source.to_string_lossy().into_owned(),
            name: None,
        })));
        assert_eq!(v["installed"], true);
        let dest = crate::paths::plugins_directory().unwrap().join("source");
        assert!(dest.join("thurbox-plugin.toml").is_file());
    }

    #[test]
    fn install_plugin_rejects_existing_dest() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let source = make_plugin_dir(temp.path(), "dup", false);

        let server = test_server();
        server.install_plugin(Parameters(InstallPluginParams {
            source_path: source.to_string_lossy().into_owned(),
            name: None,
        }));
        let v = parse_json(&server.install_plugin(Parameters(InstallPluginParams {
            source_path: source.to_string_lossy().into_owned(),
            name: None,
        })));
        assert!(v["error"].as_str().unwrap().contains("already installed"));
    }

    #[test]
    fn uninstall_plugin_requires_confirm() {
        let server = test_server();
        let v = parse_json(&server.uninstall_plugin(Parameters(UninstallPluginParams {
            name: "anything".to_string(),
            confirm: false,
        })));
        assert!(v["error"].as_str().unwrap().contains("confirm = true"));
    }

    #[test]
    fn uninstall_plugin_removes_disk_and_registry() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let dir = crate::paths::plugins_directory().unwrap();
        let plugin_path = make_plugin_dir(&dir, "gamma", true);

        let server = test_server();
        // Register so we exercise both deletes.
        server.register_plugin(Parameters(RegisterPluginParams {
            name: "gamma".to_string(),
            path: plugin_path.to_string_lossy().into_owned(),
        }));

        let v = parse_json(&server.uninstall_plugin(Parameters(UninstallPluginParams {
            name: "gamma".to_string(),
            confirm: true,
        })));
        assert_eq!(v["uninstalled"], true);
        assert_eq!(v["removed_disk"], true);
        assert_eq!(v["removed_registry"], true);
        assert!(!plugin_path.exists());
    }

    #[test]
    fn enable_disable_plugin_roundtrip() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let dir = crate::paths::plugins_directory().unwrap();
        make_plugin_dir(&dir, "togg", false);

        let server = test_server();
        let v = parse_json(&server.disable_plugin(Parameters(DisablePluginParams {
            name: "togg".to_string(),
        })));
        assert_eq!(v["enabled"], false);

        let listed = parse_json(&server.list_plugins());
        let togg = &listed[0];
        assert_eq!(togg["enabled"], false);
        assert_eq!(togg["source"], "registered", "shadow row should appear");

        let v = parse_json(&server.enable_plugin(Parameters(EnablePluginParams {
            name: "togg".to_string(),
        })));
        assert_eq!(v["enabled"], true);
    }

    #[test]
    fn list_plugin_settings_returns_schema_with_defaults() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let dir = crate::paths::plugins_directory().unwrap();
        make_plugin_dir(&dir, "cfg", true);

        let server = test_server();
        let v = parse_json(
            &server.list_plugin_settings(Parameters(ListPluginSettingsParams {
                plugin_name: "cfg".to_string(),
            })),
        );
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["key"], "workers");
        assert_eq!(arr[0]["type"], "int");
        assert_eq!(arr[0]["default"], 3);
        assert_eq!(arr[0]["effective_value"], 3);
        assert!(arr[0].get("user_value").is_none() || arr[0]["user_value"].is_null());
    }

    #[test]
    fn set_plugin_setting_validates_type_and_range() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let dir = crate::paths::plugins_directory().unwrap();
        make_plugin_dir(&dir, "cfg", true);

        let server = test_server();

        // Wrong type — string for int.
        let v = parse_json(
            &server.set_plugin_setting(Parameters(SetPluginSettingParams {
                plugin_name: "cfg".to_string(),
                key: "workers".to_string(),
                value: serde_json::json!("five"),
            })),
        );
        assert!(v["error"].as_str().unwrap().contains("Validation"));

        // Out of range.
        let v = parse_json(
            &server.set_plugin_setting(Parameters(SetPluginSettingParams {
                plugin_name: "cfg".to_string(),
                key: "workers".to_string(),
                value: serde_json::json!(99),
            })),
        );
        assert!(v["error"].as_str().unwrap().contains("Validation"));

        // Valid.
        let v = parse_json(
            &server.set_plugin_setting(Parameters(SetPluginSettingParams {
                plugin_name: "cfg".to_string(),
                key: "workers".to_string(),
                value: serde_json::json!(8),
            })),
        );
        assert_eq!(v, serde_json::json!(8));

        // get returns the new value.
        let v = parse_json(
            &server.get_plugin_setting(Parameters(GetPluginSettingParams {
                plugin_name: "cfg".to_string(),
                key: "workers".to_string(),
            })),
        );
        assert_eq!(v, serde_json::json!(8));

        // reset removes the override.
        let v = parse_json(
            &server.reset_plugin_setting(Parameters(ResetPluginSettingParams {
                plugin_name: "cfg".to_string(),
                key: "workers".to_string(),
            })),
        );
        assert_eq!(v["reset"], true);
        let v = parse_json(
            &server.get_plugin_setting(Parameters(GetPluginSettingParams {
                plugin_name: "cfg".to_string(),
                key: "workers".to_string(),
            })),
        );
        assert_eq!(v, serde_json::json!(3));
    }

    #[test]
    fn set_plugin_setting_rejects_unknown_key() {
        let temp = tempfile::TempDir::new().unwrap();
        let _guard = crate::paths::TestPathGuard::new(temp.path());
        let dir = crate::paths::plugins_directory().unwrap();
        make_plugin_dir(&dir, "cfg", true);

        let server = test_server();
        let v = parse_json(
            &server.set_plugin_setting(Parameters(SetPluginSettingParams {
                plugin_name: "cfg".to_string(),
                key: "unknown".to_string(),
                value: serde_json::json!(1),
            })),
        );
        assert!(v["error"].as_str().unwrap().contains("not declared"));
    }
}
