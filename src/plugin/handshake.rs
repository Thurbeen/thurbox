//! `handshake` op — first message exchanged after a plugin spawns.
//!
//! Thurbox sends its API version and the plugin's effective configuration;
//! the plugin replies with its declared capabilities and tool list. A
//! mismatched API version aborts the spawn.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

use super::rpc::{RpcClient, RpcError};

/// API version we speak. Must match `thurbox_plugin_api` in the manifest.
pub const PLUGIN_API_VERSION: u32 = 1;

/// Default ceiling for the handshake call. A plugin that takes longer than
/// this to respond on its very first message is almost certainly broken.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Outbound payload thurbox sends in the handshake.
#[derive(Debug, Clone, Serialize)]
pub struct HandshakeRequest<'a> {
    pub api_version: u32,
    /// Plugin name from the manifest — lets the plugin self-identify in logs.
    pub plugin_name: &'a str,
    /// Effective configuration values (merged user overrides + manifest defaults),
    /// keyed by `[[contributes.configuration]].key`. JSON-encoded so that any
    /// `ConfigurationType` round-trips losslessly.
    pub effective_configuration: Value,
}

/// Inbound payload the plugin sends back.
#[derive(Debug, Clone, Deserialize)]
pub struct HandshakeResponse {
    pub api_version: u32,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Errors specific to handshake — wraps RPC errors plus version-mismatch.
#[derive(Debug)]
pub enum HandshakeError {
    Rpc(RpcError),
    VersionMismatch {
        host: u32,
        plugin: u32,
    },
    /// Response payload didn't match the expected schema.
    BadResponse(String),
}

impl std::fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandshakeError::Rpc(e) => write!(f, "{e}"),
            HandshakeError::VersionMismatch { host, plugin } => {
                write!(f, "plugin api_version {plugin} does not match host {host}")
            }
            HandshakeError::BadResponse(s) => write!(f, "bad handshake response: {s}"),
        }
    }
}

impl std::error::Error for HandshakeError {}

impl From<RpcError> for HandshakeError {
    fn from(e: RpcError) -> Self {
        HandshakeError::Rpc(e)
    }
}

/// Send the handshake op and validate the response.
pub async fn perform_handshake(
    client: &RpcClient,
    plugin_name: &str,
    effective_configuration: Value,
) -> Result<HandshakeResponse, HandshakeError> {
    let payload = serde_json::to_value(HandshakeRequest {
        api_version: PLUGIN_API_VERSION,
        plugin_name,
        effective_configuration,
    })
    .map_err(|e| HandshakeError::BadResponse(e.to_string()))?;

    let result = client.call("handshake", payload, HANDSHAKE_TIMEOUT).await?;

    let response: HandshakeResponse =
        serde_json::from_value(result).map_err(|e| HandshakeError::BadResponse(e.to_string()))?;

    if response.api_version != PLUGIN_API_VERSION {
        return Err(HandshakeError::VersionMismatch {
            host: PLUGIN_API_VERSION,
            plugin: response.api_version,
        });
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips() {
        let req = HandshakeRequest {
            api_version: 1,
            plugin_name: "demo",
            effective_configuration: serde_json::json!({"poll_interval": "15s"}),
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"api_version\":1"));
        assert!(s.contains("\"plugin_name\":\"demo\""));
        assert!(s.contains("\"poll_interval\":\"15s\""));
    }

    #[test]
    fn response_parses_with_capabilities() {
        let r: HandshakeResponse =
            serde_json::from_str(r#"{"api_version":1,"capabilities":["backend","mcp-tools"]}"#)
                .unwrap();
        assert_eq!(r.api_version, 1);
        assert_eq!(r.capabilities, vec!["backend", "mcp-tools"]);
    }

    #[test]
    fn response_parses_without_capabilities() {
        let r: HandshakeResponse = serde_json::from_str(r#"{"api_version":1}"#).unwrap();
        assert_eq!(r.api_version, 1);
        assert!(r.capabilities.is_empty());
    }
}
