//! Request parameter and response types for MCP tool handlers.

use std::collections::HashMap;
use std::path::PathBuf;

use rmcp::schemars;
use serde::{Deserialize, Serialize};

// ── Tool Parameters ─────────────────────────────────────────────

/// A role definition for configuring Claude Code session permissions.
///
/// Roles map to Claude Code CLI flags (`--permission-mode`, `--allowed-tools`,
/// `--disallowed-tools`, `--append-system-prompt`), controlling which tools
/// are available and how they behave within a session.
///
/// See `docs/MCP_ROLES.md` for the complete configuration guide.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RoleInput {
    #[schemars(description = "Role name (1-64 chars, unique)")]
    pub name: String,
    #[schemars(description = "Human-readable summary of the role's purpose")]
    pub description: String,
    #[schemars(
        description = "Permission mode: default, plan, acceptEdits, dontAsk, or bypassPermissions"
    )]
    pub permission_mode: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Tools that auto-approve without prompting (e.g. [\"Read\", \"Bash(git:*)\"])"
    )]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    #[schemars(
        description = "Tools that are blocked entirely (e.g. [\"Edit\", \"Write\", \"Bash\"])"
    )]
    pub disallowed_tools: Vec<String>,
    #[schemars(
        description = "Restrict available tool set: \"default\" = all, \"\" = none, or comma-separated"
    )]
    pub tools: Option<String>,
    #[schemars(description = "Text appended to Claude's system prompt for this role")]
    pub append_system_prompt: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Environment variables passed to sessions using this role (e.g. {\"API_KEY\": \"sk-...\", \"PATH_EXTRA\": \"/opt/bin\"})"
    )]
    pub env: HashMap<String, String>,
}

/// Parameters for the `set_roles` tool.
///
/// Atomically replaces all global roles. All existing global roles are deleted
/// and the provided list is inserted in a single database transaction. To add
/// a role, include all existing roles plus the new one. To clear all roles,
/// pass an empty array.
/// Parameters for the `set_editor_command` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetEditorCommandParams {
    #[schemars(
        description = "Editor command to launch when opening a worktree (e.g. \"code --wait\", \"nvim\", \"zed --new\"). The target worktree path is appended as the final argument. Pass an empty string to clear the setting."
    )]
    pub command: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetRolesParams {
    #[schemars(
        description = "Complete list of roles — atomically replaces all existing global roles"
    )]
    pub roles: Vec<RoleInput>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListSessionsParams {
    // No fields — lists all active sessions.
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetSessionParams {
    #[schemars(description = "Session UUID")]
    pub session: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteSessionParams {
    #[schemars(description = "Session UUID")]
    pub session: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RestartSessionParams {
    #[schemars(description = "Session UUID")]
    pub session: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RestoreSessionParams {
    #[schemars(description = "Session UUID of a soft-deleted session")]
    pub session: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct McpServerInput {
    #[schemars(description = "Server name (unique)")]
    pub name: String,
    #[schemars(description = "Command to run the MCP server")]
    pub command: String,
    #[serde(default)]
    #[schemars(description = "Command-line arguments")]
    pub args: Vec<String>,
    #[serde(default)]
    #[schemars(description = "Environment variables (key-value pairs)")]
    pub env: HashMap<String, String>,
}

/// Parameters for the `set_mcp_servers` tool.
///
/// Atomically replaces all global MCP servers. All existing global servers are
/// deleted and the provided list is inserted in a single database transaction.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetMcpServersParams {
    #[schemars(
        description = "Complete list of MCP servers — atomically replaces all global servers"
    )]
    pub servers: Vec<McpServerInput>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListVmsParams {
    // No fields — lists all active VMs.
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetVmParams {
    #[schemars(description = "VM UUID")]
    pub vm: String,
}

// ── Containerfile Template Parameters ───────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetContainerfileTemplateParams {
    #[schemars(description = "Template name (directory name under containerfiles/)")]
    pub name: String,
}

/// A support file to include alongside the Containerfile in a template.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SupportFileInput {
    #[schemars(description = "Filename (e.g. \"init-firewall.sh\")")]
    pub filename: String,
    #[schemars(description = "File content (text)")]
    pub content: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetContainerfileTemplateParams {
    #[schemars(description = "Template name (creates or updates the directory)")]
    pub name: String,
    #[schemars(description = "Content of the Containerfile")]
    pub containerfile_content: String,
    #[schemars(description = "Optional support files to include in the template directory")]
    pub support_files: Option<Vec<SupportFileInput>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteContainerfileTemplateParams {
    #[schemars(description = "Template name to delete")]
    pub name: String,
}

// ── Skill Parameters ───────────────────────────────────────────

/// A skill registry entry — a name and an absolute path to an on-disk
/// skill directory containing a `SKILL.md` file. Thurbox only stores
/// references; it never creates, edits, or deletes skill files.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SkillInput {
    #[schemars(description = "Skill name (1-64 chars, unique)")]
    pub name: String,
    #[schemars(
        description = "Absolute path to the skill directory on disk (must exist and contain a SKILL.md file)"
    )]
    pub path: String,
}

/// Parameters for the `set_skills` tool.
///
/// Atomically replaces all registered skills. All existing skill references
/// are deleted and the provided list is inserted in a single database
/// transaction. Skill files on disk are never touched.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetSkillsParams {
    #[schemars(
        description = "Complete list of skill references — atomically replaces all registered skills"
    )]
    pub skills: Vec<SkillInput>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RegisterSkillParams {
    #[schemars(description = "Skill name (1-64 chars, unique)")]
    pub name: String,
    #[schemars(
        description = "Absolute path to the skill directory on disk (must exist and contain a SKILL.md file)"
    )]
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UnregisterSkillParams {
    #[schemars(description = "Skill name to unregister (disk files are not touched)")]
    pub name: String,
}

// ── Scheduled Command Parameters ───────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScheduleCommandParams {
    #[schemars(description = "Session UUID")]
    pub session: String,
    #[schemars(description = "Text to type into the session terminal")]
    pub command_text: String,
    #[schemars(description = "Unix millisecond timestamp at which to send the command")]
    pub scheduled_at: u64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListScheduledCommandsParams {
    #[schemars(description = "Optional session UUID to filter by")]
    pub session: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetScheduledCommandParams {
    #[schemars(description = "Scheduled command ID")]
    pub id: i64,
}

// ── Orchestrator Parameters ────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SendPromptParams {
    #[schemars(description = "Session UUID")]
    pub session: String,
    #[schemars(
        description = "Text to type into the session terminal. Enter is pressed automatically after a short delay."
    )]
    pub text: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CaptureSessionOutputParams {
    #[schemars(description = "Session UUID")]
    pub session: String,
    #[schemars(
        description = "How many lines of scrollback to include before the visible region. Default 200, max 10000."
    )]
    pub lines: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateSessionParams {
    #[schemars(description = "Session name. Must be 1-64 chars, no slashes or leading '.'.")]
    pub name: String,
    #[schemars(
        description = "Absolute path to the repository (or directory) the session should cwd into."
    )]
    pub repo_path: String,
    #[schemars(
        description = "Optional role name. If omitted and there is exactly one global role it is used; otherwise the default developer role."
    )]
    pub role: Option<String>,
    #[schemars(
        description = "If set, creates a git worktree on this new branch off `base_branch` inside repo_path and uses the worktree as the session cwd."
    )]
    pub worktree_branch: Option<String>,
    #[schemars(
        description = "Base branch for the worktree (default: main). Only used when worktree_branch is set."
    )]
    pub base_branch: Option<String>,
    #[serde(default)]
    #[schemars(description = "Optional list of global MCP server names to attach.")]
    pub mcp_servers: Vec<String>,
    #[serde(default)]
    #[schemars(description = "Optional list of global skill names to stage into the session.")]
    pub skills: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CancelScheduledCommandParams {
    #[schemars(description = "Scheduled command ID")]
    pub id: i64,
}

// ── VM Image Parameters ────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DownloadVmImageParams {
    #[schemars(description = "HTTPS URL to download the VM image from")]
    pub url: String,
    #[schemars(description = "Filename to save as (default: derived from URL)")]
    pub filename: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteVmImageParams {
    #[schemars(description = "Filename of the image to delete")]
    pub filename: String,
}

// ── Response Types ──────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct RoleResponse {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disallowed_tools: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub append_system_prompt: Option<String>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct McpServerResponse {
    pub name: String,
    pub command: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub id: String,
    pub name: String,
    pub role: String,
    pub backend_type: String,
    #[serde(skip_serializing_if = "Option::is_none", alias = "claude_session_id")]
    pub agent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub worktrees: Vec<WorktreeResponse>,
}

#[derive(Debug, Serialize)]
pub struct WorktreeResponse {
    pub repo_path: PathBuf,
    pub worktree_path: PathBuf,
    pub branch: String,
}

#[derive(Debug, Serialize)]
pub struct VmResponse {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub state: String,
    pub ssh_port: u16,
    pub base_image: String,
    pub cpus: u32,
    pub memory_mb: u32,
    pub disk_gb: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_msg: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SkillResponse {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct ContainerfileTemplateResponse {
    pub name: String,
    pub containerfile_content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub support_files: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ContainerfileTemplateSummary {
    pub name: String,
    pub files: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct VmImageResponse {
    pub filename: String,
    pub size_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct ScheduledCommandResponse {
    pub id: i64,
    pub session_id: String,
    pub command_text: String,
    pub scheduled_at: u64,
    pub created_at: u64,
    pub status: String,
}
