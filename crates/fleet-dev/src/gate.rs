//! One gate command for contributors, hooks and every native CI runner.
use crate::{Result, exec, workflow};
use serde_json::Value;
use std::fs;
use std::path::Path;

pub const AREAS: &[&str] = &[
    "markdown",
    "docs",
    "yaml",
    "workflow",
    "rust-fmt",
    "rust-lint",
    "rust-test",
    "rust-doc",
    "rust-deny",
];
const TEST_FLAGS: &[&str] = &["--workspace", "--all-features", "--locked"];
const PROFILE: &[&str] = &["--profile", "ci"];
fn cargo(args: &[&str], env: &[(&str, &str)]) -> Result<String> {
    exec("cargo", args, env)
}
fn files() -> Result<Vec<String>> {
    Ok(exec("git", &["ls-files", "-z", "--cached"], &[])?
        .split('\0')
        .filter(|p| !p.is_empty() && Path::new(p).is_file())
        .map(str::to_string)
        .collect())
}
pub fn suites(listing: &Value) -> Result {
    let all = listing["rust-suites"]
        .as_object()
        .ok_or("nextest listing has no rust-suites")?;
    let names = all
        .values()
        .filter(|s| {
            s["kind"] == "test"
                && s["testcases"].as_object().is_some_and(|ts| {
                    ts.values()
                        .any(|t| t["ignored"] != true && t["filter-match"]["status"] == "matches")
                })
        })
        .filter_map(|s| s["binary-name"].as_str())
        .collect::<Vec<_>>();
    for pattern in ["architecture", "e2e_"] {
        if !names.iter().any(|s| s.starts_with(pattern)) {
            return Err(format!("no test runs in required suite {pattern}* (ignored and filtered tests do not count)").into());
        }
    }
    Ok(())
}
fn test() -> Result {
    let mut args = vec!["nextest", "list"];
    args.extend_from_slice(TEST_FLAGS);
    args.extend_from_slice(PROFILE);
    args.extend(["--message-format", "json"]);
    suites(&serde_json::from_str(&cargo(&args, &[])?)?)?;
    let mut args = vec!["nextest", "run"];
    args.extend_from_slice(TEST_FLAGS);
    args.extend_from_slice(PROFILE);
    print!("{}", cargo(&args, &[])?);
    let mut args = vec!["test", "--doc"];
    args.extend_from_slice(TEST_FLAGS);
    print!("{}", cargo(&args, &[])?);
    Ok(())
}
fn docs() -> Result {
    // Check Markdown file links without reaching the network or operator data.
    let root = fs::canonicalize(".")?;
    let tracked = files()?;
    for file in &tracked {
        if let Some(relative) = file
            .strip_prefix("docs/")
            .and_then(|path| path.split('/').next())
            .filter(|directory| directory.ends_with("-controller"))
        {
            let name = relative.trim_end_matches("-controller");
            return Err(format!(
                "{file}: controller documentation belongs in docs/controllers/{name}"
            )
            .into());
        }
    }
    if tracked
        .iter()
        .any(|file| file == "docs/controllers/README.md")
        && tracked.iter().any(|file| file == "AGENTS.md")
    {
        let agents = fs::read_to_string("AGENTS.md")?;
        if !agents.contains("](docs/controllers/README.md)") {
            return Err(
                "AGENTS.md: link the controller index at docs/controllers/README.md rather than one controller alone"
                    .into(),
            );
        }
    }
    // Every controller guide appears in the diagram as <Name>Controller; one
    // marked as design appears as "planned <Name>Controller", and only then.
    if tracked.iter().any(|file| file == "docs/architecture.svg") {
        let architecture = fs::read_to_string("docs/architecture.svg")?;
        let mut guides = tracked
            .iter()
            .filter_map(|file| {
                file.strip_prefix("docs/controllers/")?
                    .strip_suffix("/README.md")
                    .filter(|name| !name.contains('/'))
                    .map(|name| (file, name))
            })
            .collect::<Vec<_>>();
        guides.sort();
        for (guide, name) in guides {
            let controller = name
                .split('-')
                .map(|word| {
                    let mut chars = word.chars();
                    chars
                        .next()
                        .map(|first| first.to_uppercase().chain(chars).collect::<String>())
                        .unwrap_or_default()
                })
                .collect::<String>()
                + "Controller";
            let planned = fs::read_to_string(guide)?.contains("Design, not implemented");
            if !architecture.contains(&controller)
                || architecture.contains(&format!("planned {controller}")) != planned
            {
                return Err(format!(
                    "docs/architecture.svg: show {controller} with the implementation status described by the controller guide"
                )
                .into());
            }
        }
    }
    for file in tracked.iter().filter(|p| p.ends_with(".md")) {
        let p = Path::new(&file);
        let text = fs::read_to_string(p)?;
        for tail in text.split("](").skip(1) {
            let link = tail
                .split(')')
                .next()
                .unwrap_or("")
                .split_whitespace()
                .next()
                .unwrap_or("");
            let local = link.split('#').next().unwrap_or("");
            if local.is_empty() || local.contains("://") || local.starts_with("mailto:") {
                continue;
            }
            let path = p.parent().unwrap_or(Path::new(".")).join(local);
            let resolved =
                fs::canonicalize(&path).map_err(|_| format!("{file}: missing link {local}"))?;
            if !resolved.starts_with(&root) {
                return Err(format!("{file}: link outside checkout: {local}").into());
            }
            if resolved.extension().is_some_and(|s| s == "svg") {
                let svg = fs::read_to_string(&resolved)?;
                roxmltree::Document::parse(&svg)
                    .map_err(|e| format!("{file}: malformed SVG {local}: {e}"))?;
            }
        }
    }
    for file in tracked.iter().filter(|p| p.ends_with(".svg")) {
        let svg = fs::read_to_string(file)?;
        roxmltree::Document::parse(&svg).map_err(|e| format!("malformed SVG {file}: {e}"))?;
        if !svg.contains("prefers-color-scheme: light")
            || !svg.contains("prefers-color-scheme: dark")
        {
            return Err(format!("{file}: SVG must support light and dark modes").into());
        }
    }
    Ok(())
}
pub fn check(areas: &[String], fix: bool, list: bool) -> Result {
    if list {
        println!("{}", AREAS.join("\n"));
        return Ok(());
    }
    if !Path::new("Cargo.toml").is_file() {
        return Err("no Cargo.toml: missing Rust workspace".into());
    }
    let names = if areas.is_empty() {
        AREAS.iter().map(|s| s.to_string()).collect::<Vec<_>>()
    } else {
        areas.to_vec()
    };
    for name in &names {
        if !AREAS.contains(&name.as_str()) {
            return Err(format!("unknown gate area {name}").into());
        }
    }
    for name in names {
        eprintln!("check: {name}");
        match name.as_str() {
            "rust-fmt" => {
                let mut args = vec!["fmt", "--all"];
                if !fix {
                    args.push("--check");
                }
                print!("{}", cargo(&args, &[])?);
            }
            "rust-lint" => {
                print!(
                    "{}",
                    cargo(
                        &[
                            "clippy",
                            "--workspace",
                            "--all-features",
                            "--locked",
                            "--all-targets",
                            "--",
                            "-D",
                            "warnings"
                        ],
                        &[]
                    )?
                );
            }
            "rust-test" => test()?,
            "rust-doc" => {
                print!(
                    "{}",
                    cargo(
                        &[
                            "doc",
                            "--workspace",
                            "--all-features",
                            "--locked",
                            "--no-deps"
                        ],
                        &[("RUSTDOCFLAGS", "-D warnings")]
                    )?
                );
            }
            "rust-deny" => {
                print!("{}", cargo(&["deny", "--locked", "check"], &[])?);
            }
            "workflow" => workflow::check()?,
            "yaml" => {
                for file in files()?
                    .into_iter()
                    .filter(|f| f.ends_with(".yaml") || f.ends_with(".yml"))
                {
                    workflow::yaml(Path::new(&file))?;
                }
            }
            "docs" => docs()?,
            "markdown" => {
                let files = files()?
                    .into_iter()
                    .filter(|p| p.ends_with(".md"))
                    .collect::<Vec<_>>();
                if files.is_empty() {
                    return Err("no Markdown files".into());
                }
                let mut args = vec!["check"];
                if fix {
                    args.push("--fix");
                }
                args.extend(files.iter().map(String::as_str));
                print!("{}", exec("rumdl", &args, &[])?);
            }
            _ => unreachable!(),
        }
        println!("{name}: ok");
    }
    Ok(())
}
