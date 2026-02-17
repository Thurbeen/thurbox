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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RoleInput {
    #[schemars(description = "Role name")]
    pub name: String,
    #[schemars(description = "Role description")]
    pub description: String,
    #[schemars(description = "Permission mode for Claude CLI")]
    pub permission_mode: Option<String>,
    #[serde(default)]
    #[schemars(description = "List of allowed tool names")]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    #[schemars(description = "List of disallowed tool names")]
    pub disallowed_tools: Vec<String>,
    #[schemars(description = "Tool configuration")]
    pub tools: Option<String>,
    #[schemars(description = "Text to append to the system prompt")]
    pub append_system_prompt: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SetRolesParams {
    #[schemars(description = "Project name or UUID")]
    pub project: String,
    #[schemars(description = "List of role definitions (replaces all existing)")]
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
