use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::HostDef;

const PLACE_ROOT: &str = ".local/share/runtime-mux/workspaces";

pub const SCHEMES: &[&str] = &["git+https", "git+ssh", "git+file", "file"];

pub const CONFIG_ENV: &str = "RUNTIME_MUX_CONFIG";

const SETUP_DONE: &str = ".runtime-mux-setup";

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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    #[serde(rename = "match")]
    matches: String,
    run: Vec<String>,
}

impl Step {
    fn applies_to(&self, url: &str) -> bool {
        let bare = url.split(['?', '#']).next().unwrap_or(url);
        self.matches == url || self.matches == bare
    }
}

#[derive(Debug)]
pub struct Workspaces {
    place: Option<HostDef>,
    root: PathBuf,
    program: PathBuf,
    preserve_days: Option<u64>,
    setup: Vec<Step>,
    teardown: Vec<Step>,
}

impl Workspaces {
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
            place: None,
            root,
            program: section.program.unwrap_or_else(default_program),
            preserve_days: section.preserve.days,
            setup: section.setup,
            teardown: section.teardown,
        })
    }

    pub fn at(mut self, place: Option<&HostDef>) -> Result<Workspaces, String> {
        if let Some(host) = place {
            let home = crate::place::home(host)?;
            // `wsl.exe` runs this program on Windows, so the place's path is joined with `/`.
            self.root = PathBuf::from(format!("{}/{PLACE_ROOT}", home.trim_end_matches('/')));
            self.place = Some(host.clone());
        }
        Ok(self)
    }

    pub fn make(&self, key: &str, urls: &[String]) -> Value {
        let mut request = json!({ "root": self.root, "key": key, "urls": urls });
        if let Some(days) = self.preserve_days {
            request["preserveDays"] = json!(days);
        }
        let answer = match self.call("make", &request) {
            Ok(answer) => answer,
            Err(Call::NotRun(why)) => {
                return match self
                    .place
                    .as_ref()
                    .and_then(|h| crate::place::file_dirs(h, urls))
                {
                    Some(Ok(Ok(dirs))) => {
                        known(json!({ "made": { "dirs": dirs, "adopted": false } }))
                    }
                    Some(Ok(Err(failed))) => known(json!({ "failed": { "why": failed } })),
                    Some(Err(unanswered)) => unknown(unanswered),
                    None => known(json!({ "failed": { "why": why } })),
                }
            }
            Err(Call::Unanswered(why)) => return unknown(why),
        };
        let Some(made) = answer.get("made") else {
            return match answer.get("unknown") {
                Some(u) => json!({ "unknown": u }),
                None => known(answer),
            };
        };
        if !self
            .setup
            .iter()
            .any(|step| urls.iter().any(|url| step.applies_to(url)))
        {
            return known(answer);
        }
        let done = PathBuf::from(format!("{}/{key}/{SETUP_DONE}", self.root.display()));
        let adopted = made["adopted"] == true;
        match setup_needed(adopted, || self.is_file(&done)) {
            Ok(true) => {}
            Ok(false) => return known(answer),
            Err(why) => {
                return unknown(format!(
                    "whether the workspace's setup finished cannot be told: {why}"
                ))
            }
        }
        let dirs: Vec<String> = made["dirs"]
            .as_array()
            .map(|d| {
                d.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if let Err(why) = self.run_steps(&self.setup, urls, &dirs) {
            return match self.release(key) {
                Ok(()) => known(json!({ "failed": { "why": why } })),
                Err(e) => unknown(format!("{why}; and the workspace was not released: {e}")),
            };
        }
        match &self.place {
            Some(host) => crate::place::touch(host, &done.to_string_lossy()),
            None => {
                let _ = std::fs::write(&done, "");
            }
        }
        known(answer)
    }

    pub fn release(&self, key: &str) -> Result<(), String> {
        let request = json!({ "root": self.root, "key": key });
        let found = match (self.call("inspect", &request), &self.place) {
            (Err(Call::NotRun(why)), Some(host)) => {
                let dir = format!("{}/{key}", self.root.display());
                return match crate::place::exists(host, &dir) {
                    Ok(false) => Ok(()),
                    Ok(true) => Err(format!(
                        "{dir} holds a workspace, and {why}; it is released once workspace-git runs there"
                    )),
                    Err(e) => Err(e),
                };
            }
            (found, _) => found.map_err(Call::why)?,
        };
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
            let _ = self.run_steps(&self.teardown, &strings("urls"), &strings("dirs"));
        } else if found != json!("none") {
            return Err(format!("workspace-git inspect answered {found}"));
        }
        match self.call("release", &request).map_err(Call::why)? {
            answer if answer == json!("released") => Ok(()),
            answer => Err(format!("workspace-git release answered {answer}")),
        }
    }

    fn call(&self, verb: &str, request: &Value) -> Result<Value, Call> {
        if let Some(host) = &self.place {
            let (code, stdout, stderr) =
                crate::place::workspace_git(host, verb, request.to_string().as_bytes())
                    .map_err(Call::Unanswered)?;
            // `sh` exits 127 when it cannot find the program.
            if code == Some(127) {
                return Err(Call::NotRun(format!(
                    "cannot run workspace-git on place {:?}: {}; a git+ workspace needs it on the place's PATH (it ships beside runtime-mux), or add its directory to path_prepend",
                    host.name,
                    stderr.trim()
                )));
            }
            return serde_json::from_str(&stdout).map_err(|e| {
                Call::Unanswered(format!(
                    "workspace-git {verb} on place {:?} answered no JSON ({e}): {}",
                    host.name,
                    stderr.trim()
                ))
            });
        }
        let mut cmd = Command::new(&self.program);
        cmd.env_remove("OTEL_SERVICE_NAME")
            .env_remove(otel_env::TRACEPARENT);
        if let Some(parent) = otel_env::traceparent() {
            cmd.env(otel_env::TRACEPARENT, parent);
        }
        let mut child = cmd
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

impl Workspaces {
    fn run_steps(&self, steps: &[Step], urls: &[String], dirs: &[String]) -> Result<(), String> {
        for (url, dir) in urls.iter().zip(dirs) {
            for step in steps.iter().filter(|s| s.applies_to(url)) {
                for command in &step.run {
                    match &self.place {
                        Some(host) => crate::place::run_in(host, dir, command)?,
                        None => run_in(Path::new(dir), command)?,
                    }
                }
            }
        }
        Ok(())
    }

    fn is_file(&self, path: &Path) -> Result<bool, String> {
        match &self.place {
            Some(host) => crate::place::is_file(host, &path.to_string_lossy()),
            None => path
                .try_exists()
                .map(|there| there && path.is_file())
                .map_err(|e| format!("{}: {e}", path.display())),
        }
    }
}

fn setup_needed(
    adopted: bool,
    marker: impl FnOnce() -> Result<bool, String>,
) -> Result<bool, String> {
    Ok(!adopted || !marker()?)
}

enum Call {
    NotRun(String),
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

pub fn data_dir() -> Option<PathBuf> {
    data_home().map(|d| d.join("runtime-mux"))
}

fn data_home() -> Option<PathBuf> {
    xdg("XDG_DATA_HOME", ".local/share")
}

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
    fn setup_reruns_only_when_the_marker_is_proven_absent() {
        assert_eq!(setup_needed(false, || Ok(true)), Ok(true));
        assert_eq!(setup_needed(true, || Ok(true)), Ok(false));
        assert_eq!(setup_needed(true, || Ok(false)), Ok(true));
        assert!(setup_needed(true, || Err("no answer".to_string())).is_err());
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
