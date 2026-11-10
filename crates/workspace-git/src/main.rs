//! `workspace-git make|release`: one JSON request on stdin, one answer on
//! stdout (docs/runtime/README.md, "workspace-git").
//!
//! - `make` takes `{root, key, urls, preserveDays?}` and answers
//!   `{"made": {dirs, adopted}}`, `{"unsupported": {url}}` or
//!   `{"failed": {why}}`.
//! - `release` takes `{root, key}` and answers `"released"`.
//!
//! Anything else is `{"unknown": {why}}`: the request was not answered, and
//! the caller concludes nothing from it.

use std::io::Read;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};
use workspace_git::{Config, DEFAULT_PRESERVE_DAYS, make, release};

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
struct Release {
    root: PathBuf,
    key: String,
}

fn unknown(why: impl std::fmt::Display) -> Value {
    json!({ "unknown": { "why": why.to_string() } })
}

fn answer(verb: &str, request: &[u8]) -> Value {
    let absolute = |root: &PathBuf| {
        if root.is_absolute() {
            Ok(())
        } else {
            Err(format!("root {} is not an absolute path", root.display()))
        }
    };
    match verb {
        "make" => {
            let r: Make = match serde_json::from_slice(request) {
                Ok(r) => r,
                Err(e) => return unknown(format!("bad request: {e}")),
            };
            if let Err(why) = absolute(&r.root) {
                return unknown(why);
            }
            let cfg = Config {
                root: r.root,
                preserve_days: r.preserve_days.unwrap_or(DEFAULT_PRESERVE_DAYS),
            };
            match make(&cfg, &r.key, &r.urls) {
                Ok(answer) => serde_json::to_value(answer).unwrap_or_else(unknown),
                Err(why) => unknown(why),
            }
        }
        "release" => {
            let r: Release = match serde_json::from_slice(request) {
                Ok(r) => r,
                Err(e) => return unknown(format!("bad request: {e}")),
            };
            if let Err(why) = absolute(&r.root) {
                return unknown(why);
            }
            match release(&Config::new(r.root), &r.key) {
                Ok(()) => json!("released"),
                Err(why) => unknown(why),
            }
        }
        other => unknown(format!("unknown verb {other:?}: use make or release")),
    }
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
