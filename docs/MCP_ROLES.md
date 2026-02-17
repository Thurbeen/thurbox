# MCP Role Configuration Guide

Complete reference for configuring roles via the `thurbox-mcp`
MCP server. Roles define Claude Code permission profiles that
control which tools are available, how they behave, and what
system prompt text is appended.

For TUI-based role editing, see the
[Role Editor](FEATURES.md#role-editor) section in FEATURES.md.

---

## Overview

Roles are per-project permission profiles stored in Thurbox's
SQLite database. When a session starts, its assigned role
determines the Claude CLI flags passed at spawn time:

```text
claude --permission-mode <mode> \
       --allowed-tools "<tool1> <tool2>" \
       --disallowed-tools "<tool3>" \
       --append-system-prompt "<text>"
```

Each project can have zero or more roles. Sessions select a role
at creation time. If no roles are defined, sessions spawn with
default (empty) permissions.

---

## Permission Modes

The `permission_mode` field controls Claude Code's base
permission behavior. Valid values:

| Mode | Description |
|------|-------------|
| `default` | Claude asks the user before running tools (standard behavior) |
| `plan` | Claude can only plan; no tool execution until user approves |
| `acceptEdits` | Auto-approve file edits; ask for everything else |
| `dontAsk` | Auto-approve all tool calls without prompting |
| `bypassPermissions` | Skip all permission checks (use with caution) |

When `permission_mode` is omitted or `null`, Claude Code uses
its own default behavior (equivalent to `"default"`).

---

## Allow / Ask / Deny Semantics

Roles use three permission tiers:

| Tier | Behavior | Configured via |
|------|----------|----------------|
| **Allow** | Tool runs without asking the user | `allowed_tools` |
| **Ask** | User is prompted before tool runs | *(default for unlisted tools)* |
| **Deny** | Tool is completely blocked | `disallowed_tools` |

**Interaction rules**:

- Tools in `allowed_tools` auto-approve without user prompt.
- Tools in `disallowed_tools` are removed from Claude's
  available tools entirely — Claude cannot invoke them.
- Tools not in either list follow the `permission_mode` behavior
  (usually "ask the user").
- If a tool appears in both `allowed_tools` and
  `disallowed_tools`, the deny takes precedence.

---

## Tool Name Format

Tool names in `allowed_tools` and `disallowed_tools` follow
Claude Code's tool naming conventions.

### Simple tool names

Use the tool's base name:

| Tool | Description |
|------|-------------|
| `Read` | Read files |
| `Edit` | Edit files |
| `Write` | Write/create files |
| `Bash` | Execute shell commands |
| `Glob` | Find files by pattern |
| `Grep` | Search file contents |
| `WebFetch` | Fetch web content |
| `WebSearch` | Search the web |
| `Task` | Launch sub-agents |
| `NotebookEdit` | Edit Jupyter notebooks |

### Scope patterns

Bash commands can be scoped using `Bash(specifier)` syntax:

```text
Bash(git:*)         # All git subcommands
Bash(npm run *)     # npm run with any script name
Bash(cargo:*)       # All cargo subcommands
Bash(docker:*)      # All docker subcommands
```

File tools can be scoped to paths:

```text
Read(.env*)         # Read .env files
Edit(src/**)        # Edit files in src/
```

### Format in JSON

Tool lists are JSON arrays of strings:

```json
{
  "allowed_tools": ["Read", "Grep", "Bash(git:*)"],
  "disallowed_tools": ["Write", "Bash(rm:*)"]
}
```

---

## Field Reference

### RoleInput fields

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `name` | string | yes | — | Role identifier, unique per project. 1-64 chars, trimmed. |
| `description` | string | yes | — | Human-readable summary of the role's purpose. |
| `permission_mode` | string \| null | no | `null` | One of: `default`, `plan`, `acceptEdits`, `dontAsk`, `bypassPermissions`. |
| `allowed_tools` | string[] | no | `[]` | Tools that auto-approve. See [Tool Name Format](#tool-name-format). |
| `disallowed_tools` | string[] | no | `[]` | Tools that are blocked entirely. See [Tool Name Format](#tool-name-format). |
| `tools` | string \| null | no | `null` | Restrict available tool set. `"default"` = all tools, `""` = none, or comma-separated list. |
| `append_system_prompt` | string \| null | no | `null` | Text appended to Claude's system prompt for this role. |

### Validation rules

- `name` must be non-empty after trimming and at most
  64 characters.
- `name` must be unique within the project. Duplicate names
  in a `set_roles` call will cause a database constraint error.
- `description` can be empty but must be present.
- `permission_mode` is passed verbatim to the Claude CLI
  `--permission-mode` flag. Invalid values will cause Claude
  to reject the flag at session startup.

---

## MCP Tool Reference

### list_roles

List all roles for a project.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `project` | string | yes | Project name (case-insensitive) or UUID |

**Response**: JSON array of role objects.

**Example request**:

```json
{
  "method": "tools/call",
  "params": {
    "name": "list_roles",
    "arguments": {
      "project": "my-app"
    }
  }
}
```

**Example response**:

```json
[
  {
    "name": "developer",
    "description": "Full development access",
    "permission_mode": "acceptEdits",
    "allowed_tools": ["Bash(git:*)", "Bash(cargo:*)"]
  },
  {
    "name": "reviewer",
    "description": "Read-only code review",
    "permission_mode": "plan",
    "allowed_tools": ["Read", "Grep", "Glob"]
  }
]
```

**Error cases**:

- Project not found: `{"error": "Project not found: <name>"}`

### set_roles

Atomically replace all roles for a project.

**Parameters**:

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `project` | string | yes | Project name (case-insensitive) or UUID |
| `roles` | RoleInput[] | yes | Complete list of roles (replaces all existing) |

**Behavior**:

- All existing roles are deleted and replaced in a single
  database transaction.
- To add a role, include all existing roles plus the new one.
- To remove a role, include all roles except the one to remove.
- To clear all roles, pass an empty array.
- The operation is atomic: if any role fails validation,
  no changes are applied.

**Example request**:

```json
{
  "method": "tools/call",
  "params": {
    "name": "set_roles",
    "arguments": {
      "project": "my-app",
      "roles": [
        {
          "name": "developer",
          "description": "Full development access",
          "permission_mode": "acceptEdits",
          "allowed_tools": ["Bash(git:*)", "Bash(cargo:*)"],
          "disallowed_tools": []
        },
        {
          "name": "reviewer",
          "description": "Read-only code review",
          "permission_mode": "plan",
          "allowed_tools": ["Read", "Grep", "Glob"],
          "disallowed_tools": ["Edit", "Write", "Bash"]
        }
      ]
    }
  }
}
```

**Example response**: JSON array of the newly set roles
(same format as `list_roles`).

**Error cases**:

- Project not found: `{"error": "Project not found: <name>"}`
- Database error: `{"error": "<sqlite error message>"}`

---

## Common Role Patterns

### Developer — full access with git auto-approve

```json
{
  "name": "developer",
  "description": "Full development access with git auto-approved",
  "permission_mode": "acceptEdits",
  "allowed_tools": ["Bash(git:*)", "Bash(cargo:*)"],
  "disallowed_tools": []
}
```

### Reviewer — read-only code review

```json
{
  "name": "reviewer",
  "description": "Read-only access for code review",
  "permission_mode": "plan",
  "allowed_tools": ["Read", "Grep", "Glob"],
  "disallowed_tools": ["Edit", "Write", "Bash"]
}
```

### CI Runner — build and test only

```json
{
  "name": "ci-runner",
  "description": "Run builds and tests, no file modifications",
  "permission_mode": "default",
  "allowed_tools": [
    "Read",
    "Grep",
    "Glob",
    "Bash(cargo:*)",
    "Bash(npm:*)"
  ],
  "disallowed_tools": ["Edit", "Write"]
}
```

### Auditor — read everything, change nothing

```json
{
  "name": "auditor",
  "description": "Full read access for security audits",
  "permission_mode": "plan",
  "allowed_tools": ["Read", "Grep", "Glob", "WebFetch"],
  "disallowed_tools": ["Edit", "Write", "Bash", "NotebookEdit"],
  "append_system_prompt": "You are performing a security audit. Report findings but do not modify any files."
}
```

### Guided — ask before everything

```json
{
  "name": "guided",
  "description": "Human-in-the-loop for all actions",
  "permission_mode": "default",
  "allowed_tools": [],
  "disallowed_tools": []
}
```

---

## Integration

### From the Admin Session

The Thurbox admin session has `thurbox-mcp` auto-configured.
Use natural language:

> "Set up developer and reviewer roles for the my-app project.
> Developer should have acceptEdits mode with git auto-approved.
> Reviewer should be plan-only with read access."

### From Claude Code CLI

Configure `thurbox-mcp` in `.mcp.json`:

```json
{
  "mcpServers": {
    "thurbox": {
      "command": "thurbox-mcp",
      "args": []
    }
  }
}
```

Then use the `set_roles` and `list_roles` tools directly.
After setting roles, new sessions can select any configured role.
The role selector appears when creating a session (`Ctrl+N`)
in a project with multiple roles defined.
