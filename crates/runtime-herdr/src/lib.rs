//! `runtime-herdr <verb>`: Fleet's runtime protocol (docs/runtime/README.md)
//! over herdr. Each key's process runs in a pane of the user's own herdr,
//! labelled with the key, in a herdr workspace of its own (docs/runtime/herdr.md).

mod herdr;
mod records;
mod workspace;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use herdr::{Failure, Server};
use records::{Records, Started};

const PROTOCOL: u64 = 2;
const DEFAULT_WORKSPACE_LABEL: &str = "fleet";
const VERBS: &[&str] = &["manifest", "start", "inspect", "stop", "list", "workspace"];
const RECORD_EXIT_CODE: &str = r#"exit_file=$0; "$@"; code=$?; printf '%s\n' "$code" > "$exit_file.partial" && mv "$exit_file.partial" "$exit_file"; exit "$code""#;

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
        "workspace" => key_of(&request).and_then(|key| make_workspace(&key, &request)),
        _ => key_of(&request).and_then(|key| match verb {
            "start" => start(&place, &key, &request),
            "inspect" => inspect(&place, &key),
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
    session: Option<String>,
    workspace: Option<String>,
}

impl Place {
    fn from_request(request: &Value) -> Result<Place, String> {
        let place = request
            .get("place")
            .ok_or("a protocol 2 request names its place")?;
        let name = place["name"].as_str().ok_or("the place has no name")?;
        Place::configured(name, &place["config"])
    }

    fn configured(name: &str, config: &Value) -> Result<Place, String> {
        match config {
            Value::Null => Ok(Place::default()),
            config => serde_json::from_value(config.clone())
                .map_err(|e| format!("place {name:?}: {e}; its keys are session and workspace")),
        }
    }

    fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    fn workspace_label(&self) -> &str {
        self.workspace.as_deref().unwrap_or(DEFAULT_WORKSPACE_LABEL)
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
    match herdr::observe(place.session()) {
        Ok(_) => Ok(()),
        Err(Failure::NotInstalled) => Err(not_installed()),
        Err(Failure::Unanswered(why)) => Err(why),
    }
}

fn not_installed() -> String {
    "herdr is not installed: install it from https://herdr.dev".to_string()
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

fn observed(place: &Place) -> Result<Server, String> {
    match herdr::observe(place.session()) {
        Ok(server) => Ok(server),
        Err(Failure::NotInstalled) => Err(not_installed()),
        Err(Failure::Unanswered(why)) => Err(why),
    }
}

enum Holder {
    Running(String),
    Gone,
}

fn holder(server: &Server, records: &Records, key: &str) -> Result<Holder, String> {
    let Some(started) = records.started(key)? else {
        return Ok(Holder::Gone);
    };
    let same_process = server
        .panes_labelled(key)?
        .into_iter()
        .any(|pane| pane.id == started.pane && pane.terminal == started.terminal);
    Ok(match same_process {
        true => Holder::Running(started.pane),
        false => Holder::Gone,
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
    let server = match herdr::serve(place.session()) {
        Ok(server) => server,
        Err(Failure::NotInstalled) => return Ok(json!({ "failed": { "why": not_installed() } })),
        Err(Failure::Unanswered(why)) => return Err(why),
    };
    let records = Records::for_server(&server.socket)?;
    let _lock = records.lock()?;
    if let Holder::Running(pane) = holder(&server, &records, key)? {
        return Ok(json!({ "started": { "id": pane, "adopted": true } }));
    }
    for stale in server.panes_labelled(key)? {
        server.close(&stale.id)?;
    }
    records.forget_exit(key)?;
    let workspace = server.workspace_labelled(place.workspace_label(), dir)?;
    let mut command = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        RECORD_EXIT_CODE.to_string(),
        records.exit_file(key).to_string_lossy().into_owned(),
    ];
    command.extend(argv);
    let applied = server.expect(
        "layout.apply",
        json!({
            "workspace_id": workspace,
            "tab_label": key,
            "focus": false,
            "root": { "type": "pane", "label": key, "command": command, "cwd": dir, "env": env },
        }),
    )?;
    let pane = applied["layout"]["root"]["pane_id"]
        .as_str()
        .ok_or("herdr layout.apply named no pane")?
        .to_string();
    let terminal = server
        .panes()?
        .into_iter()
        .find(|p| p.id == pane)
        .and_then(|p| p.terminal);
    records.record_start(
        key,
        &Started {
            pane: pane.clone(),
            terminal,
        },
    )?;
    Ok(json!({ "started": { "id": pane, "adopted": false } }))
}

fn inspect(place: &Place, key: &str) -> Result<Value, String> {
    let server = observed(place)?;
    let records = Records::for_server(&server.socket)?;
    if let Holder::Running(pane) = holder(&server, &records, key)? {
        return Ok(json!({ "running": { "id": pane } }));
    }
    Ok(match records.started(key)? {
        Some(started) => {
            json!({ "exited": { "id": started.pane, "code": records.exit_code(key) } })
        }
        None => json!("none"),
    })
}

fn stop(place: &Place, key: &str, release: bool) -> Result<Value, String> {
    let server = observed(place)?;
    let records = Records::for_server(&server.socket)?;
    let lock = records.lock()?;
    for pane in server.panes_labelled(key)? {
        server.close(&pane.id)?;
    }
    records.forget(key)?;
    drop(lock);
    if release {
        workspace::release(key)?;
    }
    Ok(json!("stopped"))
}

fn list(place: &Place) -> Result<Value, String> {
    let server = observed(place)?;
    let records = Records::for_server(&server.socket)?;
    Ok(json!({ "keys": records.keys()? }))
}

fn make_workspace(key: &str, request: &Value) -> Result<Value, String> {
    let urls: Vec<String> = request["urls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|url| url.as_str().map(String::from))
        .collect::<Option<_>>()
        .ok_or("urls: a list of strings")?;
    let made = workspace::make(key, &urls)?;
    match made.get("unknown") {
        Some(why) => Err(why["why"].as_str().unwrap_or("workspace-git").to_string()),
        None => Ok(made),
    }
}
