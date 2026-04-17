//! Reference process plugin contributing an `mcp-tools` capability.
//!
//! Speaks the JSON-RPC protocol described in `docs/PLUGINS.md`:
//! one request per line on stdin, one response per line on stdout, debug
//! prints to stderr (thurbox captures that into the plugin's log file).
//!
//! The plugin exposes a single tool, `echo`, that returns whatever JSON it
//! was given under `{"echoed": <args>}`. Real plugins (e.g. a spec-aware
//! inspector, a custom search tool, or a project-specific code-action
//! surface) follow the same shape but do real work inside `mcp.call`.
//!
//! This is the counterpart to `sample_backend_plugin.rs` — that one covers
//! the `backend` capability; this one covers `mcp-tools`.

use std::io::{BufRead, BufReader, Write};

use serde_json::{json, Value};

fn main() {
    eprintln!("sample-mcp-plugin: starting");

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                eprintln!("sample-mcp-plugin: stdin closed");
                break;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("sample-mcp-plugin: read error: {e}");
                break;
            }
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("sample-mcp-plugin: bad request {trimmed}: {e}");
                continue;
            }
        };
        let id = req.get("id").and_then(Value::as_u64).unwrap_or(0);
        let op = req
            .get("op")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let params = req.get("params").cloned().unwrap_or(Value::Null);

        let resp = handle(&op, &params);
        let frame = match resp {
            Ok(result) => json!({"id": id, "ok": true, "result": result}),
            Err(msg) => json!({"id": id, "ok": false, "error": msg}),
        };
        let mut s = serde_json::to_string(&frame).expect("serialize");
        s.push('\n');
        if let Err(e) = stdout.write_all(s.as_bytes()) {
            eprintln!("sample-mcp-plugin: write error: {e}");
            break;
        }
        if let Err(e) = stdout.flush() {
            eprintln!("sample-mcp-plugin: flush error: {e}");
            break;
        }

        if op == "stop" {
            eprintln!("sample-mcp-plugin: stop received; exiting");
            break;
        }
    }
}

fn handle(op: &str, params: &Value) -> Result<Value, String> {
    match op {
        "handshake" => Ok(json!({
            "api_version": 1,
            "capabilities": ["mcp-tools"],
        })),
        "stop" => Ok(Value::Null),
        "config.updated" => {
            eprintln!("sample-mcp-plugin: config.updated {params}");
            Ok(Value::Null)
        }
        // Live tool discovery. Returned only when the host didn't find any
        // `[[contributes.mcp_tools]]` rows in the manifest.
        "mcp.list_tools" => Ok(json!([
            {
                "name": "echo",
                "description": "Echo back the input as {\"echoed\": <args>}.",
                "input_schema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": true,
                },
            }
        ])),
        "mcp.call" => {
            let tool = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing 'name' field".to_string())?;
            let args = params.get("params").cloned().unwrap_or(Value::Null);
            match tool {
                "echo" => Ok(json!({ "echoed": args })),
                other => Err(format!("unknown tool: {other}")),
            }
        }
        other => Err(format!("unknown op: {other}")),
    }
}
