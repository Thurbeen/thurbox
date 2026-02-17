//! Request parameter and response types for MCP tool handlers.

use std::path::PathBuf;

use rmcp::schemars;
use serde::{Deserialize, Serialize};

// ── Tool Parameters ─────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetProjectParams {
    #[schemars(description = "Project name or UUID")]
    pub project: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateProjectParams {
    #[schemars(description = "Project name")]
    pub name: String,
    #[schemars(description = "List of repository directory paths")]
    pub repos: Vec<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UpdateProjectParams {
    #[schemars(description = "Project name or UUID")]
    pub project: String,
    #[schemars(description = "New project name")]
    pub name: Option<String>,
    #[schemars(description = "New list of repository directory paths (replaces existing)")]
    pub repos: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteProjectParams {
    #[schemars(description = "Project name or UUID")]
    pub project: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListRolesParams {
    #[schemars(description = "Project name or UUID")]
    pub project: String,
}

/// A role definition for configuring Claude Code session permissions.
///
/// Roles map to Claude Code CLI flags (`--permission-mode`, `--allowed-tools`,
/// `--disallowed-tools`, `--append-system-prompt`), controlling which tools
/// are available and how they behave within a session.
///
/// See `docs/MCP_ROLES.md` for the complete configuration guide.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RoleInput {
    #[schemars(description = "Role name (1-64 chars, unique per project)")]
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
}

/// Parameters for the `set_roles` tool.
///
/// Atomically replaces all roles for a project. All existing roles are deleted
/// and the provided list is inserted in a single database transaction. To add
/// a role, include all existing roles plus the new one. To clear all roles,
/// pass an empty array.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetRolesParams {
    #[schemars(description = "Project name or UUID")]
    pub project: String,
    #[schemars(
        description = "Complete list of roles — atomically replaces all existing roles for the project"
    )]
    pub roles: Vec<RoleInput>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListSessionsParams {
    #[schemars(description = "Optional project name or UUID to filter sessions")]
    pub project: Option<String>,
}

// ── Response Types ──────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ProjectResponse {
    pub id: String,
    pub name: String,
    pub repos: Vec<PathBuf>,
    pub roles: Vec<RoleResponse>,
}

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
}

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub id: String,
    pub name: String,
    pub project_id: String,
    pub role: String,
    pub backend_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeResponse>,
}

#[derive(Debug, Serialize)]
pub struct WorktreeResponse {
    pub repo_path: PathBuf,
    pub worktree_path: PathBuf,
    pub branch: String,
}
