//! Shared wire types for the control socket between `thurbox-mcp` and the TUI.
//!
//! The frame shape is deliberately identical to the plugin-stdio RPC frame
//! (`src/plugin/rpc.rs`): line-delimited JSON with `{id, op, params}` requests
//! and `{id, ok, result|error}` responses. Plugin authors who've already
//! learned the plugin-side protocol do not have to learn a second one — the
//! control socket speaks the same frame, only with a different op vocabulary.
//!
//! Ops are string constants so both the listener's match arms and the
//! client's call sites can reference the same symbol. Renaming one and
//! forgetting the other becomes a compile error instead of a 404.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Enumerate plugins running in the TUI that declared the `mcp-tools`
/// capability. Pure registry read — no activation, no side effects.
pub const OP_PLUGIN_LIST: &str = "plugin.list";

/// Return the list of tools a specific plugin exposes. Reads
/// `[[contributes.mcp_tools]]` from the plugin manifest when present;
/// falls back to asking the plugin at runtime (`mcp.list_tools`) if the
/// manifest declares none.
pub const OP_PLUGIN_LIST_TOOLS: &str = "plugin.list_tools";

/// Invoke a tool on a plugin. The plugin must already be running — typically
/// via `activation_events = ["onStartupFinished"]`. Lazy `OnCommand`
/// activation in the control-socket path is a follow-up.
pub const OP_PLUGIN_CALL: &str = "plugin.call";

/// Outbound request frame on the control socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub op: String,
    #[serde(default = "empty_object")]
    pub params: Value,
}

/// Inbound response frame on the control socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: u64, error: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(error.into()),
        }
    }
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips() {
        let req = Request {
            id: 7,
            op: OP_PLUGIN_CALL.to_string(),
            params: serde_json::json!({"plugin": "p", "tool": "t", "args": {"x": 1}}),
        };
        let line = serde_json::to_string(&req).unwrap();
        let decoded: Request = serde_json::from_str(&line).unwrap();
        assert_eq!(decoded.id, 7);
        assert_eq!(decoded.op, "plugin.call");
        assert_eq!(decoded.params["plugin"], "p");
    }

    #[test]
    fn request_defaults_params_to_empty_object() {
        let decoded: Request =
            serde_json::from_str(r#"{"id":1,"op":"plugin.list"}"#).unwrap();
        assert_eq!(decoded.params, serde_json::json!({}));
    }

    #[test]
    fn response_ok_omits_error_field() {
        let r = Response::ok(3, serde_json::json!({"v": 1}));
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"ok\":true"));
        assert!(!s.contains("\"error\""));
    }

    #[test]
    fn response_err_omits_result_field() {
        let r = Response::err(4, "boom");
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"ok\":false"));
        assert!(s.contains("\"error\":\"boom\""));
        assert!(!s.contains("\"result\""));
    }

    #[test]
    fn response_parses_error_shape() {
        let r: Response =
            serde_json::from_str(r#"{"id":4,"ok":false,"error":"x"}"#).unwrap();
        assert_eq!(r.id, 4);
        assert!(!r.ok);
        assert_eq!(r.error.as_deref(), Some("x"));
        assert!(r.result.is_none());
    }

    #[test]
    fn op_constants_match_documented_strings() {
        // These strings are part of the public wire contract. Changing them
        // breaks every deployed thurbox-mcp client.
        assert_eq!(OP_PLUGIN_LIST, "plugin.list");
        assert_eq!(OP_PLUGIN_LIST_TOOLS, "plugin.list_tools");
        assert_eq!(OP_PLUGIN_CALL, "plugin.call");
    }
}
