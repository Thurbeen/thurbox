use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde::Deserialize;
use serde_json::{Value, json};

pub const PROGRAM: &str = "herdr-core";
const GENERATION: u64 = 1;

pub enum Failure {
    NotInstalled,
    Unanswered(String),
}

pub struct Core {
    pub id: String,
    child: Child,
    requests: ChildStdin,
    replies: BufReader<ChildStdout>,
    next_id: u64,
}

#[derive(Deserialize)]
pub struct Terminal {
    pub address: String,
    pub state: String,
    pub pid: Option<u32>,
    pub exit_code: Option<i64>,
    pub created_ms: Option<u64>,
}

impl Terminal {
    pub fn is_live(&self) -> bool {
        self.state == "live"
    }
}

impl Core {
    pub fn connect(session: Option<&str>) -> Result<Core, Failure> {
        let mut command = Command::new(PROGRAM);
        if let Some(session) = session {
            command.env("HC_SESSION", session);
        }
        let mut child = command
            .arg("connect")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Failure::NotInstalled,
                _ => Failure::Unanswered(format!("cannot run {PROGRAM} connect: {e}")),
            })?;
        let (Some(requests), Some(replies)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(Failure::Unanswered(format!(
                "{PROGRAM} connect has no pipes"
            )));
        };
        let mut core = Core {
            id: String::new(),
            child,
            requests,
            replies: BufReader::new(replies),
            next_id: 0,
        };
        let hello = core
            .expect("hello", json!({ "generation": GENERATION }))
            .map_err(Failure::Unanswered)?;
        if hello["generation"].as_u64() != Some(GENERATION) {
            return Err(Failure::Unanswered(format!(
                "herdr-core speaks protocol generation {}, and runtime-herdr generation {GENERATION}",
                hello["generation"]
            )));
        }
        let id = hello["core"]["id"].as_str().unwrap_or_default();
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(Failure::Unanswered(format!(
                "herdr-core named itself {:?}",
                hello["core"]["id"]
            )));
        }
        core.id = id.to_string();
        Ok(core)
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Result<Value, String>, String> {
        self.next_id += 1;
        let request =
            json!({ "jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params });
        let lost = |e: std::io::Error| format!("herdr-core connect: {e}");
        self.requests
            .write_all(format!("{request}\n").as_bytes())
            .map_err(lost)?;
        self.requests.flush().map_err(lost)?;
        let reply = loop {
            let mut line = String::new();
            if self.replies.read_line(&mut line).map_err(lost)? == 0 {
                return Err(format!(
                    "herdr-core connect closed before it answered {method}"
                ));
            }
            let message: Value = serde_json::from_str(&line)
                .map_err(|e| format!("herdr-core answered {method} with no JSON ({e})"))?;
            if message["id"] == self.next_id {
                break message;
            }
        };
        if let Some(error) = reply.get("error") {
            return Ok(Err(error["data"]["kind"]
                .as_str()
                .unwrap_or_default()
                .to_string()
                + ": "
                + error["message"].as_str().unwrap_or_default()));
        }
        reply.get("result").cloned().map(Ok).ok_or_else(|| {
            format!("herdr-core answered {method} with neither a result nor an error")
        })
    }

    pub fn expect(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.call(method, params)?
            .map_err(|why| format!("herdr-core {method}: {why}"))
    }

    pub fn terminals_labelled(&mut self, labels: &Value) -> Result<Vec<Terminal>, String> {
        let listed = self.expect("terminal.list", json!({ "all": true, "labels": labels }))?;
        let mut terminals: Vec<Terminal> = serde_json::from_value(listed["terminals"].clone())
            .map_err(|e| format!("herdr-core terminal.list: {e}"))?;
        terminals.sort_by_key(|t| t.created_ms);
        Ok(terminals)
    }

    pub fn run(&mut self, params: Value) -> Result<Terminal, String> {
        let started = self.expect("terminal.run", params)?;
        serde_json::from_value(started).map_err(|e| format!("herdr-core terminal.run: {e}"))
    }

    pub fn end(&mut self, terminal: &Terminal) -> Result<(), String> {
        if terminal.is_live() {
            self.expect("terminal.kill", json!({ "terminal": terminal.address }))?;
        }
        match self.call("terminal.rm", json!({ "terminal": terminal.address }))? {
            Ok(_) => Ok(()),
            Err(why) if why.starts_with("not_found:") => Ok(()),
            Err(why) => Err(format!("herdr-core terminal.rm: {why}")),
        }
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
