//! MCP server editor logic for the App.

use std::collections::HashMap;

use tracing::error;

use crate::session::McpServerConfig;

use super::App;

/// Which field is focused in the MCP editor modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpEditorField {
    Name,
    Command,
    Args,
    Env,
}

impl App {
    /// Reset MCP editor fields to prepare for adding a new server.
    pub(crate) fn prepare_new_mcp_editor(&mut self) {
        self.mcp_editor_editing_index = None;
        self.mcp_editor_name.clear();
        self.mcp_editor_command.clear();
        self.mcp_editor_args.reset();
        self.mcp_editor_env.reset();
        self.mcp_editor_field = McpEditorField::Name;
        self.mcp_editor_snapshot = Some(self.capture_mcp_editor_snapshot());
    }

    /// Populate the MCP editor from an existing global server config.
    pub(crate) fn open_mcp_server_for_editing(&mut self, idx: usize) {
        let Some(server) = self.global_mcp_servers.get(idx) else {
            return;
        };
        let name = server.name.clone();
        let command = server.command.clone();
        let args = server.args.clone();
        let mut env_strings: Vec<String> =
            server.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        env_strings.sort();

        self.mcp_editor_editing_index = Some(idx);
        self.mcp_editor_name.set(&name);
        self.mcp_editor_command.set(&command);
        self.mcp_editor_args.load(&args);
        self.mcp_editor_env.load(&env_strings);

        self.mcp_editor_field = McpEditorField::Name;
        self.mcp_editor_snapshot = Some(self.capture_mcp_editor_snapshot());
    }

    /// Validate and save the MCP editor state to the global MCP servers list.
    pub(crate) fn submit_mcp_editor(&mut self) {
        let name = self.mcp_editor_name.value().trim().to_string();
        let command = self.mcp_editor_command.value().trim().to_string();

        if name.is_empty() || command.is_empty() {
            return;
        }

        // Check duplicate names (excluding the server being edited)
        let is_duplicate = self
            .global_mcp_servers
            .iter()
            .enumerate()
            .any(|(i, s)| s.name == name && Some(i) != self.mcp_editor_editing_index);
        if is_duplicate {
            return;
        }

        // Parse env entries from "KEY=VALUE" strings
        let env: HashMap<String, String> = self
            .mcp_editor_env
            .items
            .iter()
            .filter_map(|entry| {
                let (k, v) = entry.split_once('=')?;
                Some((k.to_string(), v.to_string()))
            })
            .collect();

        let server = McpServerConfig {
            name,
            command,
            args: self.mcp_editor_args.items.clone(),
            env,
        };

        if let Some(idx) = self.mcp_editor_editing_index {
            self.global_mcp_servers[idx] = server;
        } else {
            self.global_mcp_servers.push(server);
            self.mcp_server_list_index = self.global_mcp_servers.len().saturating_sub(1);
        }

        // Persist to DB
        if let Err(e) = self.db.replace_global_mcp_servers(&self.global_mcp_servers) {
            error!("Failed to save global MCP servers to DB: {e}");
        }

        self.show_mcp_editor = false;
        self.mcp_editor_snapshot = None;
        self.mcp_editor_field = McpEditorField::Name;
    }
}
