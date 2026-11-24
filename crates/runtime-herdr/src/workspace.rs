use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use crate::records::data_home;

const WORKSPACE_RECORD: &str = ".fleet-workspace.json";

pub const SCHEMES: &[&str] = &["git+https", "git+ssh", "git+file", "file"];

fn root() -> Result<PathBuf, String> {
    data_home()
        .map(|d| d.join("runtime-herdr").join("workspaces"))
        .ok_or_else(|| "neither XDG_DATA_HOME nor HOME is set".to_string())
}

fn program() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("workspace-git")))
        .filter(|beside| beside.is_file())
        .unwrap_or_else(|| PathBuf::from("workspace-git"))
}

fn call(verb: &str, request: &Value) -> Result<Value, String> {
    let program = program();
    let mut child = Command::new(&program)
        .arg(verb)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "cannot run {}: {e}; it ships beside runtime-herdr",
                program.display()
            )
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(request.to_string().as_bytes())
            .map_err(|e| format!("cannot send workspace-git the request: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("workspace-git {verb}: {e}"))?;
    serde_json::from_slice(&out.stdout).map_err(|e| {
        format!(
            "workspace-git {verb} answered no JSON ({e}): {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
    })
}

pub fn keys() -> Result<Vec<String>, String> {
    let root = root()?;
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot list {}: {e}", root.display())),
    };
    Ok(entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| root.join(name).join(WORKSPACE_RECORD).is_file())
        .collect())
}

pub fn make(key: &str, urls: &[String]) -> Result<Value, String> {
    call(
        "make",
        &json!({ "root": root()?, "key": key, "urls": urls }),
    )
}

pub fn release(key: &str) -> Result<(), String> {
    match call("release", &json!({ "root": root()?, "key": key }))? {
        answer if answer == "released" => Ok(()),
        answer => Err(format!("workspace-git release answered {answer}")),
    }
}
