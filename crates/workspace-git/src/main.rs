//! `workspace-git make|release|discard|inspect`: one JSON request on stdin,
//! one answer on stdout (docs/runtime/README.md, "workspace-git").
//!
//! - `make` takes `{root, key, urls, preserveDays?}` and answers
//!   `{"made": {dirs, adopted}}`, `{"unsupported": {url}}` or
//!   `{"failed": {why}}`.
//! - `release` takes `{root, key}` and answers `"released"`.
//! - `discard` takes `{root, key}` and answers `"discarded"`.
//! - `inspect` takes `{root, key}` and answers `{"workspace": {urls, dirs}}`
//!   or `"none"`.
//!
//! Anything else is `{"unknown": {why}}`: the request was not answered, and
//! the caller concludes nothing from it.

use std::io::Read;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};
use workspace_git::{Config, DEFAULT_PRESERVE_DAYS, discard, inspect, make, release};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Make {
    root: PathBuf,
    key: String,
    urls: Vec<String>,
    preserve_days: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Keyed {
    root: PathBuf,
    key: String,
}

fn unknown(why: impl std::fmt::Display) -> Value {
    json!({ "unknown": { "why": why.to_string() } })
}

fn parse<T: for<'de> Deserialize<'de>>(
    request: &[u8],
    root: fn(&T) -> &PathBuf,
) -> Result<T, Value> {
    let r: T = serde_json::from_slice(request).map_err(|e| unknown(format!("bad request: {e}")))?;
    if !root(&r).is_absolute() {
        return Err(unknown(format!(
            "root {} is not an absolute path",
            root(&r).display()
        )));
    }
    Ok(r)
}

fn answer(verb: &str, request: &[u8]) -> Value {
    if verb == "make" {
        let r = match parse::<Make>(request, |r| &r.root) {
            Ok(r) => r,
            Err(answer) => return answer,
        };
        let cfg = Config {
            root: r.root,
            preserve_days: r.preserve_days.unwrap_or(DEFAULT_PRESERVE_DAYS),
        };
        return match make(&cfg, &r.key, &r.urls) {
            Ok(answer) => serde_json::to_value(answer).unwrap_or_else(unknown),
            Err(why) => unknown(why),
        };
    }
    if !matches!(verb, "release" | "discard" | "inspect") {
        return unknown(format!(
            "unknown verb {verb:?}: use make, release, discard or inspect"
        ));
    }
    let r = match parse::<Keyed>(request, |r| &r.root) {
        Ok(r) => r,
        Err(answer) => return answer,
    };
    let cfg = Config::new(r.root);
    let done = match verb {
        "release" => release(&cfg, &r.key).map(|()| json!("released")),
        "discard" => discard(&cfg, &r.key).map(|()| json!("discarded")),
        _ => inspect(&cfg, &r.key).map(|found| match found {
            Some(w) => json!({ "workspace": { "urls": w.urls, "dirs": w.dirs } }),
            None => json!("none"),
        }),
    };
    done.unwrap_or_else(unknown)
}

fn main() {
    let verb = std::env::args().nth(1).unwrap_or_default();
    let mut request = Vec::new();
    let out = match std::io::stdin().read_to_end(&mut request) {
        Ok(_) => answer(&verb, &request),
        Err(e) => unknown(format!("cannot read the request: {e}")),
    };
    println!("{out}");
}
