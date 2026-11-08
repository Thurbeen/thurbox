//! The workflow gate consumes parsed YAML; mutations exercise the same path as CI.
use crate::{Result, gate};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

pub fn yaml(path: &Path) -> Result<Value> {
    let root = fs::canonicalize(".")?;
    let path = fs::canonicalize(path)?;
    if !path.starts_with(root) {
        return Err("workflow/YAML path outside checkout".into());
    }
    Ok(serde_saphyr::from_str(&fs::read_to_string(path)?)?)
}
fn strings(v: &Value) -> Vec<String> {
    if let Some(s) = v.as_str() {
        vec![s.into()]
    } else {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }
}
fn jobs(doc: &Value) -> Result<&serde_json::Map<String, Value>> {
    doc["jobs"]
        .as_object()
        .ok_or_else(|| "workflow has no jobs".into())
}
fn runners(job: &Value) -> Result<Vec<String>> {
    let run = &job["runs-on"];
    if let Some(s) = run.as_str() {
        if s.starts_with("${{ matrix.") && s.ends_with(" }}") {
            let key = s.trim_start_matches("${{ matrix.").trim_end_matches(" }}");
            let m = &job["strategy"]["matrix"];
            if let Some(a) = m[key].as_array() {
                return Ok(a
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect());
            }
            if let Some(a) = m["include"].as_array() {
                let r = a
                    .iter()
                    .filter_map(|v| v[key].as_str().map(str::to_string))
                    .collect::<Vec<_>>();
                if r.len() == a.len() && !r.is_empty() {
                    return Ok(r);
                }
            }
            return Err(format!("unresolved runner {s}").into());
        }
        if s.contains("${{") {
            return Err(format!("unresolved runner {s}").into());
        }
        return Ok(vec![s.into()]);
    }
    let labels = strings(run);
    if labels.is_empty() {
        return Err("missing runs-on".into());
    }
    Ok(vec![labels.join("+")])
}
fn steps(job: &Value) -> impl Iterator<Item = &Value> {
    job["steps"].as_array().into_iter().flatten()
}
fn checks(run: &str) -> Result<Option<BTreeSet<String>>> {
    let Some(rest) = run.trim_start().strip_prefix("cargo xtask check") else {
        return Ok(None);
    };
    // A gate command is a command, not arbitrary shell. Fail on an expression
    // we cannot resolve rather than crediting it to every matrix row.
    if rest.trim().contains([';', '|', '&', '>', '<', '\n', '\r']) {
        return Err(
            "gate validation must be a standalone command; a shell chain may swallow failure"
                .into(),
        );
    }
    let words = rest.split_whitespace();
    let mut areas = BTreeSet::new();
    for word in words {
        if ["--fix", "--offline"].contains(&word) {
            continue;
        }
        if !gate::AREAS.contains(&word) {
            return Err(format!("unknown gate area/expression {word}").into());
        }
        areas.insert(word.to_string());
    }
    if areas.is_empty() {
        areas = gate::AREAS.iter().map(|s| s.to_string()).collect();
    }
    Ok(Some(areas))
}
fn continues(v: &Value) -> bool {
    !v.is_null() && v != false
}
fn shell_checks(name: &str, doc: &Value, errors: &mut Vec<String>) -> Result {
    for (id, job) in jobs(doc)? {
        for step in steps(job) {
            if let Some(action) = step["uses"].as_str() {
                let pinned = action.starts_with("./")
                    || action.rsplit_once('@').is_some_and(|(_, sha)| {
                        sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit())
                    });
                if !pinned {
                    errors.push(format!("{name}/{id}: action {action} requires a full SHA"));
                }
            }
            if let Some(run) = step["run"].as_str() {
                for part in run.split("${{").skip(1) {
                    let expr = part.split("}}").next().unwrap_or(part);
                    // GitHub property names are case-insensitive and support
                    // bracket access. Refuse dynamic GitHub indexing as well as
                    // dotted contributor contexts, including nested functions.
                    let compact = expr
                        .chars()
                        .filter(|c| !c.is_whitespace())
                        .collect::<String>()
                        .to_ascii_lowercase();
                    let untrusted = [
                        "inputs",
                        "github.event",
                        "github.head_ref",
                        "github.ref_name",
                        "github[",
                    ];
                    if untrusted.iter().any(|key| compact.contains(key)) {
                        errors.push(format!("{name}/{id}: unsafe interpolation: {expr}"));
                    }
                }
                if name == "cd.yml" {
                    let shell = step["shell"]
                        .as_str()
                        .or(job["defaults"]["run"]["shell"].as_str())
                        .or(doc["defaults"]["run"]["shell"].as_str());
                    if !shell.is_some_and(|s| {
                        s == "bash" || s.contains("bash") && s.contains("pipefail")
                    }) {
                        errors.push(format!(
                            "{name}/{id}: defaults.run.shell must be bash (pipefail)"
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}
pub fn check() -> Result {
    let root = Path::new(".github/workflows");
    let ci = yaml(&root.join("ci.yml"))?;
    let all = jobs(&ci)?;
    let mut errors = Vec::new();
    let gate = &all.get("all-checks").ok_or("missing all-checks job")?;
    if !matches!(gate["if"].as_str(), Some("always()" | "${{ always() }}")) {
        errors.push("All Checks must run if: always()".into());
    }
    let needs = strings(&gate["needs"]);
    for (name, job) in all {
        if name != "all-checks" && !needs.contains(name) {
            errors.push(format!("All Checks needs missing job {name}"));
        }
        if !job["timeout-minutes"].as_u64().is_some_and(|n| n > 0) {
            errors.push(format!("{name}: missing timeout-minutes"));
        }
        if continues(&job["continue-on-error"]) {
            errors.push(format!(
                "{name}: continue-on-error can swallow a failed gate"
            ));
        }
        if name != "all-checks" && job.get("if").is_some() {
            errors.push(format!("{name}: conditional CI job can skip the gate"));
        }
    }
    for need in &needs {
        if !all.contains_key(need) || need == "all-checks" {
            errors.push(format!("invalid needs: {need}"));
        }
    }
    if ci["on"]["pull_request"].get("paths").is_some()
        || ci["on"]["pull_request"].get("paths-ignore").is_some()
    {
        errors.push("CI must not filter paths".into());
    }
    let mut coverage: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (name, job) in all {
        if name == "all-checks" {
            continue;
        }
        let rs = runners(job)?;
        let mut covered = BTreeSet::new();
        for step in steps(job) {
            if continues(&step["continue-on-error"]) {
                errors.push(format!("{name}: gate step continue-on-error"));
            }
            if step.get("if").is_some()
                && step["run"]
                    .as_str()
                    .is_some_and(|s| s.contains("cargo xtask check"))
            {
                errors.push(format!("{name}: conditional gate step"));
            }
            if let Some(run) = step["run"].as_str()
                && let Some(cs) = checks(run)?
            {
                covered.extend(cs);
            }
        }
        for r in rs {
            coverage.entry(r).or_default().extend(covered.clone());
        }
    }
    if !coverage
        .keys()
        .any(|r| r.split('+').any(|s| s == "ubuntu-latest"))
    {
        errors.push("missing full gate runner ubuntu-latest".into());
    }
    for (runner, covered) in &coverage {
        let full = runner == "ubuntu-latest" || covered.iter().any(|s| !s.starts_with("rust-"));
        for &area in gate::AREAS {
            if (full || area.starts_with("rust-")) && !covered.contains(area) {
                errors.push(format!("missing {area} on {runner}"));
            }
        }
    }
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if !matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("yml" | "yaml")
        ) {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("invalid workflow name")?;
        let doc = yaml(&path)?;
        shell_checks(name, &doc, &mut errors)?;
        if name == "cd.yml" {
            for runner in runners(&doc["jobs"]["build"])? {
                if !coverage.get(&runner).is_some_and(|c| {
                    gate::AREAS
                        .iter()
                        .filter(|s| s.starts_with("rust-"))
                        .all(|s| c.contains(*s))
                }) {
                    errors.push(format!("release builds on untested runner {runner}"));
                }
            }
            let publish = &doc["jobs"]["publish"];
            if publish["environment"] != "release" {
                errors.push("release publish needs environment: release".into());
            }
            let build = &doc["jobs"]["build"];
            let mut tested = false;
            let mut packaged = false;
            if continues(&build["continue-on-error"]) {
                errors.push("release build cannot continue-on-error".into());
            }
            for step in steps(build) {
                let Some(run) = step["run"].as_str() else {
                    continue;
                };
                if let Some(areas) = checks(run)?
                    && areas.contains("rust-test")
                {
                    if step.get("if").is_some() || continues(&step["continue-on-error"]) {
                        errors.push(
                            "release rust-test cannot be conditional or continue-on-error".into(),
                        );
                    } else {
                        tested = true;
                    }
                }
                if run.contains("release package") {
                    packaged = true;
                    if !tested {
                        errors.push("release must run rust-test before packaging".into());
                    }
                }
            }
            if !packaged {
                errors.push("release has no packaging step".into());
            }
        }
        if name == "pr-title.yml" {
            let events = strings(&doc["on"]["pull_request"]["types"]);
            for event in ["opened", "edited", "synchronize", "reopened"] {
                if !events.iter().any(|s| s == event) {
                    errors.push(format!("PR Title missing trigger {event}"));
                }
            }
            if !jobs(&doc)?.values().flat_map(steps).any(|s| {
                s["run"]
                    .as_str()
                    .is_some_and(|s| s.contains("cargo xtask release title"))
            }) {
                errors.push("PR Title must validate title".into());
            }
        }
    }
    if errors.is_empty() {
        println!("workflow: ok");
        Ok(())
    } else {
        Err(errors.join("\n").into())
    }
}
pub fn verdict() -> Result {
    let ci = yaml(Path::new(".github/workflows/ci.yml"))?;
    let needs: Value =
        serde_json::from_str(&std::env::var("NEEDS").map_err(|_| "NEEDS is missing")?)?;
    let mut errors = Vec::new();
    for name in jobs(&ci)?.keys().filter(|s| s.as_str() != "all-checks") {
        let result = needs[name]["result"].as_str().unwrap_or("missing");
        if result != "success" {
            errors.push(format!("job {name}: {result}"));
        }
    }
    if errors.is_empty() {
        println!("All Checks: every job succeeded");
        Ok(())
    } else {
        Err(errors.join("\n").into())
    }
}
