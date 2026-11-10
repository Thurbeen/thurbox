//! The `workspace` verb's side of the runtime (docs/runtime/README.md,
//! "Workspaces"): runtime-mux runs the `workspace-git` program on this
//! machine and runs its own configured steps in what that program makes.
//!
//! The runtime's config is a TOML file, at `RUNTIME_MUX_CONFIG` or
//! `${XDG_CONFIG_HOME:-~/.config}/runtime-mux/config.toml`; a missing file is
//! the defaults:
//!
//! ```toml
//! [workspace]
//! root = "/home/me/.local/share/runtime-mux/workspaces"   # the default
//! program = "workspace-git"   # default: beside runtime-mux, else on PATH
//! preserve.days = 30
//!
//! [[workspace.setup]]
//! match = "git+https://github.com/acme/app.git"
//! run = ["npm ci"]
//!
//! [[workspace.teardown]]
//! match = "git+https://github.com/acme/app.git"
//! run = ["docker compose down"]
//! ```

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;
use serde_json::{json, Value};

/// The URL schemes runtime-mux makes a directory for: `workspace-git`'s.
pub const SCHEMES: &[&str] = &["git+https", "git+ssh", "git+file", "file"];

/// Names the runtime's config file in place of the default.
pub const CONFIG_ENV: &str = "RUNTIME_MUX_CONFIG";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    workspace: Section,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Section {
    root: Option<PathBuf>,
    program: Option<PathBuf>,
    #[serde(default)]
    preserve: Preserve,
    #[serde(default)]
    setup: Vec<Step>,
    #[serde(default)]
    teardown: Vec<Step>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Preserve {
    days: Option<u64>,
}

/// Commands run in the directory made for each URL the step matches.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    #[serde(rename = "match")]
    matches: String,
    run: Vec<String>,
}

impl Step {
    /// A step matches a URL as written, or the URL without its query and
    /// fragment: the repository, whatever branch or base it names.
    fn applies_to(&self, url: &str) -> bool {
        let bare = url.split(['?', '#']).next().unwrap_or(url);
        self.matches == url || self.matches == bare
    }
}

/// The runtime's workspace settings, loaded once per call.
#[derive(Debug)]
pub struct Workspaces {
    root: PathBuf,
    program: PathBuf,
    preserve_days: Option<u64>,
    setup: Vec<Step>,
    teardown: Vec<Step>,
}

impl Workspaces {
    /// The settings from the runtime's config file, or the defaults when it
    /// does not exist.
    pub fn load() -> Result<Workspaces, String> {
        let path = config_path().ok_or("no HOME to find the runtime's config in")?;
        let file: File = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => File::default(),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        let section = file.workspace;
        let root = match section.root {
            Some(root) if root.is_absolute() => root,
            Some(root) => {
                return Err(format!(
                    "{}: workspace.root {} is not an absolute path",
                    path.display(),
                    root.display()
                ))
            }
            None => data_home()
                .ok_or("no HOME to put workspaces in")?
                .join("runtime-mux/workspaces"),
        };
        Ok(Workspaces {
            root,
            program: section.program.unwrap_or_else(default_program),
            preserve_days: section.preserve.days,
            setup: section.setup,
            teardown: section.teardown,
        })
    }

    /// The `workspace` verb's answer: `workspace-git`'s, then this runtime's
    /// setup steps in a workspace it just made. A failing step discards the
    /// workspace and fails the call.
    pub fn make(&self, key: &str, urls: &[String]) -> Value {
        let mut request = json!({ "root": self.root, "key": key, "urls": urls });
        if let Some(days) = self.preserve_days {
            request["preserveDays"] = json!(days);
        }
        let answer = match self.call("make", &request) {
            Ok(answer) => answer,
            Err(Call::NotRun(why)) => return known(json!({ "failed": { "why": why } })),
            Err(Call::Unanswered(why)) => return unknown(why),
        };
        let Some(made) = answer.get("made") else {
            return match answer.get("unknown") {
                Some(u) => json!({ "unknown": u }),
                None => known(answer),
            };
        };
        if made["adopted"] == true {
            return known(answer);
        }
        let dirs: Vec<String> = made["dirs"]
            .as_array()
            .map(|d| {
                d.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if let Err(why) = run_steps(&self.setup, urls, &dirs) {
            return match self.call("discard", &json!({ "root": self.root, "key": key })) {
                Ok(_) => known(json!({ "failed": { "why": why } })),
                Err(Call::NotRun(e) | Call::Unanswered(e)) => {
                    unknown(format!("{why}; and the workspace was not discarded: {e}"))
                }
            };
        }
        known(answer)
    }

    /// Release the key's workspace: its teardown steps first, whose failures
    /// do not stop the release, then `workspace-git release`.
    pub fn release(&self, key: &str) -> Result<(), String> {
        let request = json!({ "root": self.root, "key": key });
        let found = self.call("inspect", &request).map_err(Call::why)?;
        if let Some(workspace) = found.get("workspace") {
            let strings = |field: &str| -> Vec<String> {
                workspace[field]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let _ = run_steps(&self.teardown, &strings("urls"), &strings("dirs"));
        } else if found != json!("none") {
            return Err(format!("workspace-git inspect answered {found}"));
        }
        match self.call("release", &request).map_err(Call::why)? {
            answer if answer == json!("released") => Ok(()),
            answer => Err(format!("workspace-git release answered {answer}")),
        }
    }

    /// One `workspace-git <verb>` call.
    fn call(&self, verb: &str, request: &Value) -> Result<Value, Call> {
        let mut child = Command::new(&self.program)
            .arg(verb)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Call::NotRun(format!("cannot run {}: {e}", self.program.display())))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(request.to_string().as_bytes())
                .map_err(|e| Call::Unanswered(format!("cannot send the request: {e}")))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| Call::Unanswered(format!("{} {verb}: {e}", self.program.display())))?;
        serde_json::from_slice(&out.stdout).map_err(|e| {
            Call::Unanswered(format!(
                "{} {verb} answered no JSON ({e}): {}",
                self.program.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        })
    }
}

enum Call {
    /// The program did not start: nothing was made.
    NotRun(String),
    /// It ran and gave no answer: anything may have been made.
    Unanswered(String),
}

impl Call {
    fn why(self) -> String {
        match self {
            Call::NotRun(why) | Call::Unanswered(why) => why,
        }
    }
}

fn known(value: Value) -> Value {
    json!({ "known": value })
}

fn unknown(why: impl std::fmt::Display) -> Value {
    json!({ "unknown": { "why": why.to_string() } })
}

/// Each step's commands in the directory made for each URL it matches, in
/// order; the first failure stops them.
fn run_steps(steps: &[Step], urls: &[String], dirs: &[String]) -> Result<(), String> {
    for (url, dir) in urls.iter().zip(dirs) {
        for step in steps.iter().filter(|s| s.applies_to(url)) {
            for command in &step.run {
                run_in(Path::new(dir), command)?;
            }
        }
    }
    Ok(())
}

fn run_in(dir: &Path, command: &str) -> Result<(), String> {
    #[cfg(unix)]
    let mut cmd = {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command]);
        cmd
    };
    #[cfg(not(unix))]
    let mut cmd = {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    };
    let out = cmd
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run `{command}` in {}: {e}", dir.display()))?;
    if out.status.success() {
        return Ok(());
    }
    Err(format!(
        "`{command}` in {} exited {}: {}",
        dir.display(),
        out.status
            .code()
            .map_or_else(|| "on a signal".into(), |c| c.to_string()),
        String::from_utf8_lossy(&out.stderr).trim()
    ))
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn xdg(var: &str, fallback: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .filter(|d| d.is_absolute())
        .or_else(|| home().map(|h| h.join(fallback)))
}

fn config_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(CONFIG_ENV).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    xdg("XDG_CONFIG_HOME", ".config").map(|d| d.join("runtime-mux/config.toml"))
}

fn data_home() -> Option<PathBuf> {
    xdg("XDG_DATA_HOME", ".local/share")
}

/// `workspace-git` beside this program, as one build or package installs
/// them, else the name for `PATH` to resolve.
fn default_program() -> PathBuf {
    let name = format!("workspace-git{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&name)))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_matches_the_repository_whatever_branch_or_base_the_url_names() {
        let step = Step {
            matches: "git+https://h/org/app.git".into(),
            run: vec![],
        };
        assert!(step.applies_to("git+https://h/org/app.git"));
        assert!(step.applies_to("git+https://h/org/app.git?branch=x#main"));
        assert!(!step.applies_to("git+https://h/org/lib.git"));
    }

    #[test]
    fn the_config_takes_the_documented_keys_and_refuses_others() {
        let file: File = toml::from_str(
            "[workspace]\nroot = '/w'\npreserve.days = 7\n\n[[workspace.setup]]\nmatch = 'file:///x'\nrun = ['true']\n",
        )
        .expect("config");
        assert_eq!(file.workspace.preserve.days, Some(7));
        assert_eq!(file.workspace.setup.len(), 1);
        assert!(toml::from_str::<File>("[workspace]\nroots = '/w'\n").is_err());
    }
}
