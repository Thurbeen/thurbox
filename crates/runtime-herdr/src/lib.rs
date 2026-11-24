//! `runtime-herdr <verb>`: Fleet's runtime protocol (docs/runtime/README.md)
//! over herdr-core. Each key's process is a herdr-core terminal labelled with
//! the key, which herdr's desktop app shows (docs/runtime/herdr.md).

mod core;
mod exec;
mod records;
mod workspace;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::core::{Core, Failure, Terminal};
use records::{Records, Started};

const PROTOCOL: u64 = 2;
const KEY_LABEL: &str = "fleet.key";
const PLACE_LABEL: &str = "fleet.place";
const ROLE_LABEL: &str = "fleet.role";
const SHELL_ROLE: &str = "shell";
const DEFAULT_SPACE: &str = "fleet";
const VERBS: &[&str] = &[
    "manifest",
    "start",
    "inspect",
    "stop",
    "pause",
    "list",
    "input",
    "capture",
    "exec",
    "workspace",
    "attach",
    "shell",
];

/// One answer: `{"known": …}` or `{"unknown": {"why": …}}`.
pub fn answer(verb: &str, request: &[u8]) -> Value {
    let request: Value = if request.iter().all(u8::is_ascii_whitespace) {
        json!({})
    } else {
        match serde_json::from_slice(request) {
            Ok(Value::Object(fields)) => Value::Object(fields),
            Ok(_) => return unknown("the request is not a JSON object"),
            Err(e) => return unknown(format!("the request is not JSON: {e}")),
        }
    };
    if verb == "manifest" {
        return known(manifest(&request));
    }
    if !VERBS.contains(&verb) {
        return unknown(format!("unknown verb: {verb}"));
    }
    if request["protocol"].as_u64() != Some(PROTOCOL) {
        return unknown(format!("runtime-herdr speaks protocol {PROTOCOL}"));
    }
    let place = match Place::from_request(&request) {
        Ok(place) => place,
        Err(why) => return unknown(why),
    };
    let answered = match verb {
        "list" => list(&place),
        "exec" => exec::run(&request),
        "workspace" => key_of(&request).and_then(|key| make_workspace(&place, &key, &request)),
        _ => key_of(&request).and_then(|key| match verb {
            "start" => start(&place, &key, &request),
            "inspect" => inspect(&place, &key),
            "pause" => pause(&place, &key),
            "input" => input(&place, &key, &request),
            "capture" => capture(&place, &key, &request),
            "attach" => attach(&place, &key),
            "shell" => shell(&place, &key),
            _ => stop(&place, &key, request["release"] == true),
        }),
    };
    match answered {
        Ok(answer) => known(answer),
        Err(why) => unknown(why),
    }
}

/// `{"unknown": {"why": …}}`: the request was not answered.
pub fn unknown(why: impl std::fmt::Display) -> Value {
    json!({ "unknown": { "why": why.to_string() } })
}

fn known(answer: Value) -> Value {
    json!({ "known": answer })
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Place {
    #[serde(skip)]
    name: String,
    session: Option<String>,
    space: Option<String>,
}

impl Place {
    fn from_request(request: &Value) -> Result<Place, String> {
        let place = request
            .get("place")
            .ok_or("a protocol 2 request names its place")?;
        let name = place["name"].as_str().ok_or("the place has no name")?;
        let path_safe = !name.is_empty()
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
        if !path_safe {
            return Err(format!(
                "place {name:?}: use ASCII letters, digits, '.', '-' or '_'"
            ));
        }
        Ok(Place {
            name: name.to_string(),
            ..Place::configured(name, &place["config"])?
        })
    }

    fn configured(name: &str, config: &Value) -> Result<Place, String> {
        match config {
            Value::Null => Ok(Place::default()),
            config => serde_json::from_value(config.clone())
                .map_err(|e| format!("place {name:?}: {e}; its keys are session and space")),
        }
    }

    fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    fn space(&self) -> &str {
        self.space.as_deref().unwrap_or(DEFAULT_SPACE)
    }

    fn records(&self, core: &Core) -> Result<Records, String> {
        Records::for_place(&core.id, &self.name)
    }

    fn connect(&self) -> Result<Core, String> {
        Core::connect(self.session()).map_err(|failure| match failure {
            Failure::NotInstalled => not_installed(),
            Failure::Unanswered(why) => why,
        })
    }
}

fn manifest(request: &Value) -> Value {
    let asked: Vec<(String, Value)> = match request["places"].as_array() {
        Some(places) if !places.is_empty() => places
            .iter()
            .map(|p| {
                (
                    p["name"].as_str().unwrap_or_default().to_string(),
                    p["config"].clone(),
                )
            })
            .collect(),
        _ => vec![("local".to_string(), Value::Object(Map::new()))],
    };
    let places: Vec<Value> = asked
        .iter()
        .map(|(name, config)| {
            let readiness = Place::configured(name, config).and_then(|place| readiness(&place));
            json!({
                "name": name,
                "platform": "posix",
                "dynamic": false,
                "ready": readiness.is_ok(),
                "why": readiness.err(),
            })
        })
        .collect();
    json!({
        "protocol": PROTOCOL,
        "version": env!("CARGO_PKG_VERSION"),
        "verbs": VERBS,
        "workspace": { "schemes": workspace::SCHEMES, "survivesStop": true },
        "places": places,
    })
}

fn readiness(place: &Place) -> Result<(), String> {
    if cfg!(not(unix)) {
        return Err("runtime-herdr runs on Linux and macOS only".to_string());
    }
    place.connect().map(drop)
}

fn not_installed() -> String {
    format!(
        "{} is not installed: install it from https://herdr.dev",
        core::PROGRAM
    )
}

fn key_of(request: &Value) -> Result<String, String> {
    let key = request["key"].as_str().unwrap_or_default();
    let valid = (1..=128).contains(&key.len())
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    if valid {
        Ok(key.to_string())
    } else {
        Err("key: use 1 to 128 ASCII letters, digits, '.', '-' or '_'".to_string())
    }
}

fn key_labels(place: &Place, key: &str) -> Value {
    json!({ KEY_LABEL: key, PLACE_LABEL: place.name })
}

enum Holder {
    Running(Terminal),
    Exited(Started, Option<i64>),
    None,
}

fn holder(core: &mut Core, place: &Place, key: &str) -> Result<Holder, String> {
    let records = place.records(core)?;
    let Some(started) = records.started(key)? else {
        return Ok(Holder::None);
    };
    let terminals = agent_terminals(core, place, key)?;
    let ours = match &started.terminal {
        Some(address) => terminals.into_iter().find(|t| &t.address == address),
        None => terminals.into_iter().rev().find(Terminal::is_live),
    };
    Ok(match ours {
        Some(t) if t.is_live() && (started.terminal.is_none() || t.pid == started.pid) => {
            Holder::Running(t)
        }
        Some(t) if !t.is_live() => {
            let code = t.exit_code;
            Holder::Exited(started, code)
        }
        _ => Holder::Exited(started, None),
    })
}

fn agent_terminals(core: &mut Core, place: &Place, key: &str) -> Result<Vec<Terminal>, String> {
    Ok(core
        .terminals_labelled(&key_labels(place, key))?
        .into_iter()
        .filter(|t| !t.has_label(ROLE_LABEL, SHELL_ROLE))
        .collect())
}

fn running(core: &mut Core, place: &Place, key: &str) -> Result<Option<Terminal>, String> {
    Ok(match holder(core, place, key)? {
        Holder::Running(terminal) => Some(terminal),
        Holder::Exited(..) | Holder::None => None,
    })
}

fn start(place: &Place, key: &str, request: &Value) -> Result<Value, String> {
    let argv: Vec<String> = request["argv"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|word| word.as_str().map(String::from))
        .collect::<Option<_>>()
        .ok_or("argv: a list of strings")?;
    if argv.is_empty() {
        return Err("argv names no program".to_string());
    }
    let env = match &request["env"] {
        Value::Null => Map::new(),
        Value::Object(env) if env.values().all(Value::is_string) => env.clone(),
        _ => return Err("env: an object of strings".to_string()),
    };
    let dir = request["dir"].as_str();
    if dir.is_some_and(|d| !d.starts_with('/')) {
        return Err("dir: an absolute path".to_string());
    }
    let mut core = match Core::connect(place.session()) {
        Ok(core) => core,
        Err(Failure::NotInstalled) => return Ok(json!({ "failed": { "why": not_installed() } })),
        Err(Failure::Unanswered(why)) => return Err(why),
    };
    let records = place.records(&core)?;
    let _lock = records.lock()?;
    if let Holder::Running(terminal) = holder(&mut core, place, key)? {
        record_terminal(&records, key, &terminal)?;
        records.unmark_paused(key)?;
        return Ok(json!({ "started": { "id": terminal.address, "adopted": true } }));
    }
    for stale in agent_terminals(&mut core, place, key)? {
        core.end(&stale)?;
    }
    records.record_start(key, &Started::intended())?;
    let terminal = core.run(json!({
        "argv": argv,
        "cwd": dir,
        "env": env,
        "labels": {
            KEY_LABEL: key,
            PLACE_LABEL: place.name,
            "herdr.space": place.space(),
            "herdr.tab": key,
        },
        "no_parent": true,
        "no_notify": true,
    }))?;
    record_terminal(&records, key, &terminal)?;
    records.unmark_paused(key)?;
    Ok(json!({ "started": { "id": terminal.address, "adopted": false } }))
}

fn record_terminal(records: &Records, key: &str, terminal: &Terminal) -> Result<(), String> {
    records.record_start(
        key,
        &Started {
            terminal: Some(terminal.address.clone()),
            pid: terminal.pid,
        },
    )
}

fn inspect(place: &Place, key: &str) -> Result<Value, String> {
    let mut core = place.connect()?;
    let paused = place.records(&core)?.is_paused(key);
    Ok(match holder(&mut core, place, key)? {
        Holder::Running(terminal) => json!({ "running": { "id": terminal.address } }),
        _ if paused => json!("paused"),
        Holder::Exited(started, code) => {
            json!({ "exited": { "id": started.terminal.unwrap_or_else(|| key.to_string()), "code": code } })
        }
        Holder::None => json!("none"),
    })
}

fn stop(place: &Place, key: &str, release: bool) -> Result<Value, String> {
    let mut core = place.connect()?;
    let records = place.records(&core)?;
    let lock = records.lock()?;
    for terminal in core.terminals_labelled(&key_labels(place, key))? {
        core.end(&terminal)?;
    }
    records.forget(key)?;
    drop(lock);
    if release {
        workspace::release(&place.name, key)?;
    }
    Ok(json!("stopped"))
}

fn pause(place: &Place, key: &str) -> Result<Value, String> {
    let mut core = place.connect()?;
    let records = place.records(&core)?;
    let _lock = records.lock()?;
    let live = running(&mut core, place, key)?.is_some();
    if !live {
        return Ok(json!(if records.is_paused(key) {
            "paused"
        } else {
            "none"
        }));
    }
    records.mark_paused(key)?;
    for terminal in core.terminals_labelled(&key_labels(place, key))? {
        core.end(&terminal)?;
    }
    Ok(json!("paused"))
}

fn input(place: &Place, key: &str, request: &Value) -> Result<Value, String> {
    let text = request["text"].as_str().ok_or("text: a string")?;
    if request["submit"] != true {
        return Err(
            "runtime-herdr types only with submit: herdr-core pastes and presses Enter".into(),
        );
    }
    let mut core = place.connect()?;
    let Some(terminal) = running(&mut core, place, key)? else {
        return Ok(json!("none"));
    };
    core.expect(
        "terminal.send",
        json!({ "terminal": terminal.address, "text": text, "force": true }),
    )?;
    Ok(json!("sent"))
}

fn capture(place: &Place, key: &str, request: &Value) -> Result<Value, String> {
    let mut core = place.connect()?;
    let Some(terminal) = running(&mut core, place, key)? else {
        return Ok(json!("none"));
    };
    let read = core.expect(
        "terminal.read",
        json!({ "terminal": terminal.address, "lines": request["lines"].as_u64().unwrap_or(0) }),
    )?;
    Ok(json!({ "screen": {
        "text": read["text"].as_str().unwrap_or_default(),
        "cursorRow": null,
        "cursorCol": null,
        "foreground": null,
        "title": terminal.title.filter(|t| !t.is_empty()),
    } }))
}

fn attach_argv(place: &Place, terminal: &str) -> Value {
    let mut argv: Vec<String> = Vec::new();
    if let Some(session) = place.session() {
        argv.extend(["env".to_string(), format!("HC_SESSION={session}")]);
    }
    argv.extend([
        core::PROGRAM.to_string(),
        "attach".to_string(),
        terminal.to_string(),
    ]);
    json!({ "tty": { "argv": argv } })
}

fn attach(place: &Place, key: &str) -> Result<Value, String> {
    let mut core = place.connect()?;
    Ok(match running(&mut core, place, key)? {
        Some(terminal) => attach_argv(place, &terminal.address),
        None => json!("none"),
    })
}

fn shell(place: &Place, key: &str) -> Result<Value, String> {
    let mut core = place.connect()?;
    let records = place.records(&core)?;
    let _lock = records.lock()?;
    let Some(agent) = running(&mut core, place, key)? else {
        return Ok(json!("none"));
    };
    let open = core
        .terminals_labelled(&key_labels(place, key))?
        .into_iter()
        .find(|t| t.is_live() && t.has_label(ROLE_LABEL, SHELL_ROLE));
    let shell = match open {
        Some(shell) => shell,
        None => core.run(json!({
            "argv": [std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())],
            "cwd": agent.cwd,
            "labels": {
                KEY_LABEL: key,
                PLACE_LABEL: place.name,
                ROLE_LABEL: SHELL_ROLE,
                "herdr.space": place.space(),
                "herdr.tab": key,
            },
            "no_parent": true,
            "no_notify": true,
        }))?,
    };
    Ok(attach_argv(place, &shell.address))
}

fn list(place: &Place) -> Result<Value, String> {
    let core = place.connect()?;
    let mut keys = place.records(&core)?.keys()?;
    keys.extend(workspace::keys(&place.name)?);
    keys.sort();
    keys.dedup();
    Ok(json!({ "keys": keys }))
}

fn make_workspace(place: &Place, key: &str, request: &Value) -> Result<Value, String> {
    let urls: Vec<String> = request["urls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|url| url.as_str().map(String::from))
        .collect::<Option<_>>()
        .ok_or("urls: a list of strings")?;
    let made = workspace::make(&place.name, key, &urls)?;
    match made.get("unknown") {
        Some(why) => Err(why["why"].as_str().unwrap_or("workspace-git").to_string()),
        None => Ok(made),
    }
}
