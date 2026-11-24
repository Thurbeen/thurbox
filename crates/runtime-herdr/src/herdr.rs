use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

const SERVER_START_WAIT: Duration = Duration::from_secs(10);
const API_REPLY_WAIT: Duration = Duration::from_secs(8);

pub enum Failure {
    NotInstalled,
    Unanswered(String),
}

pub struct ApiError {
    pub code: String,
    pub message: String,
}

#[derive(Deserialize)]
struct ServerStatus {
    running: bool,
    socket: PathBuf,
    compatible: Option<bool>,
}

pub struct Server {
    pub socket: PathBuf,
    pub running: bool,
}

pub struct Pane {
    pub id: String,
    pub label: Option<String>,
    pub terminal: Option<String>,
}

fn herdr(session: Option<&str>) -> Command {
    let mut command = Command::new("herdr");
    if let Some(session) = session {
        command.args(["--session", session]);
    }
    command
}

fn status(session: Option<&str>) -> Result<ServerStatus, Failure> {
    let out = herdr(session)
        .args(["status", "server", "--json"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Failure::NotInstalled,
            _ => Failure::Unanswered(format!("cannot run herdr: {e}")),
        })?;
    let status: ServerStatus = serde_json::from_slice(&out.stdout).map_err(|e| {
        Failure::Unanswered(format!(
            "herdr status answered no status ({e}): {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    })?;
    if status.running && status.compatible == Some(false) {
        return Err(Failure::Unanswered(format!(
            "the herdr server at {} speaks another API than this herdr; restart it with `herdr server stop` and `herdr`",
            status.socket.display()
        )));
    }
    Ok(status)
}

pub fn observe(session: Option<&str>) -> Result<Server, Failure> {
    let status = status(session)?;
    Ok(Server {
        socket: status.socket,
        running: status.running,
    })
}

pub fn serve(session: Option<&str>) -> Result<Server, Failure> {
    let first = status(session)?;
    if first.running {
        return Ok(Server {
            socket: first.socket,
            running: true,
        });
    }
    spawn_server(session)?;
    let deadline = Instant::now() + SERVER_START_WAIT;
    loop {
        let now = status(session)?;
        if now.running {
            return Ok(Server {
                socket: now.socket,
                running: true,
            });
        }
        if Instant::now() >= deadline {
            return Err(Failure::Unanswered(format!(
                "the herdr server at {} did not start within {} s",
                now.socket.display(),
                SERVER_START_WAIT.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn spawn_server(session: Option<&str>) -> Result<(), Failure> {
    let mut command = herdr(session);
    command
        .arg("server")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command
        .spawn()
        .map(drop)
        .map_err(|e| Failure::Unanswered(format!("cannot start the herdr server: {e}")))
}

impl Server {
    pub fn call(&self, method: &str, params: Value) -> Result<Result<Value, ApiError>, String> {
        let reply =
            self.exchange(&json!({ "id": "runtime-herdr", "method": method, "params": params }))?;
        let reply: Value = serde_json::from_str(&reply)
            .map_err(|e| format!("herdr answered {method} with no JSON ({e})"))?;
        if let Some(error) = reply.get("error") {
            return Ok(Err(ApiError {
                code: error["code"].as_str().unwrap_or_default().to_string(),
                message: error["message"].as_str().unwrap_or_default().to_string(),
            }));
        }
        reply
            .get("result")
            .cloned()
            .map(Ok)
            .ok_or_else(|| format!("herdr answered {method} with neither a result nor an error"))
    }

    pub fn expect(&self, method: &str, params: Value) -> Result<Value, String> {
        self.call(method, params)?
            .map_err(|e| format!("herdr {method}: {} ({})", e.message, e.code))
    }

    #[cfg(unix)]
    fn exchange(&self, request: &Value) -> Result<String, String> {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;
        let unreachable =
            |e: std::io::Error| format!("cannot reach herdr at {}: {e}", self.socket.display());
        let mut stream = UnixStream::connect(&self.socket).map_err(unreachable)?;
        stream
            .set_read_timeout(Some(API_REPLY_WAIT))
            .map_err(unreachable)?;
        stream
            .write_all(format!("{request}\n").as_bytes())
            .map_err(unreachable)?;
        let mut reply = String::new();
        BufReader::new(stream)
            .read_line(&mut reply)
            .map_err(unreachable)?;
        Ok(reply)
    }

    #[cfg(not(unix))]
    fn exchange(&self, _request: &Value) -> Result<String, String> {
        Err("runtime-herdr reaches herdr's socket on POSIX systems only".to_string())
    }

    pub fn panes(&self) -> Result<Vec<Pane>, String> {
        if !self.running {
            return Ok(Vec::new());
        }
        let listed = self.expect("pane.list", json!({}))?;
        Ok(listed["panes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|pane| {
                Some(Pane {
                    id: pane["pane_id"].as_str()?.to_string(),
                    label: pane["label"].as_str().map(String::from),
                    terminal: pane["terminal_id"].as_str().map(String::from),
                })
            })
            .collect())
    }

    pub fn panes_labelled(&self, label: &str) -> Result<Vec<Pane>, String> {
        Ok(self
            .panes()?
            .into_iter()
            .filter(|pane| pane.label.as_deref() == Some(label))
            .collect())
    }

    pub fn close(&self, pane: &str) -> Result<(), String> {
        match self.call("pane.close", json!({ "pane_id": pane }))? {
            Ok(_) => Ok(()),
            Err(e) if e.code == "pane_not_found" => Ok(()),
            Err(e) => Err(format!("herdr pane.close: {} ({})", e.message, e.code)),
        }
    }

    pub fn workspace_labelled(&self, label: &str, cwd: Option<&str>) -> Result<String, String> {
        let listed = self.expect("workspace.list", json!({}))?;
        let existing = listed["workspaces"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|w| w["label"].as_str() == Some(label))
            .and_then(|w| w["workspace_id"].as_str());
        if let Some(id) = existing {
            return Ok(id.to_string());
        }
        let created = self.expect(
            "workspace.create",
            json!({ "label": label, "cwd": cwd, "focus": false }),
        )?;
        created["workspace"]["workspace_id"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| "herdr workspace.create named no workspace".to_string())
    }
}
