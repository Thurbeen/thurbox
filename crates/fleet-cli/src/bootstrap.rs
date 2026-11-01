//! Local starter files are separate from Task/store state. No Git or download.
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fleet_store::durable::{LockGuard, create_exclusive, replace};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::Config;

include!(concat!(env!("OUT_DIR"), "/starter.rs"));

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Baseline {
    version: u32,
    files: BTreeMap<String, String>,
    source: Value,
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|e| e.to_string())
}

/// Refuse symlinks in managed paths, including ancestor directories. A
/// customized link is never an invitation to overwrite its target.
fn checked(path: &Path) -> Result<(), String> {
    for parent in path.ancestors() {
        match fs::symlink_metadata(parent) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!("refusing managed symlink {}", parent.display()));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", parent.display())),
        }
    }
    Ok(())
}

/// Codex reads project skills from `.agents/skills`, Claude Code only from
/// `.claude/skills`. This one relative link is the single managed symlink:
/// both harnesses then load the one canonical copy of each skill.
fn claude_skills_target() -> PathBuf {
    Path::new("..").join(".agents").join("skills")
}

fn link_claude_skills(workspace: &Path) -> Option<String> {
    let dir = workspace.join(".claude");
    let link = dir.join("skills");
    let target = claude_skills_target();
    let preserved = |path: &Path| {
        Some(format!(
            "preserved {}; Claude Code does not load VEGA's skills from {}",
            path.display(),
            workspace.join(".agents/skills").display()
        ))
    };
    // A `.claude` init cannot read leaves Claude Code without VEGA's skills
    // but never fails init.
    let unreadable = |path: &Path, e: std::io::Error| {
        Some(format!(
            "could not inspect {}: {e}; Claude Code does not find VEGA's skills",
            path.display()
        ))
    };
    // A user-owned `.claude` that is a link or a file is kept as it is.
    match fs::symlink_metadata(&dir) {
        Ok(meta) if !meta.is_dir() => return preserved(&dir),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return unreadable(&dir, e),
    }
    match fs::symlink_metadata(&link) {
        Ok(meta) => {
            let ours = meta.file_type().is_symlink()
                && fs::read_link(&link).is_ok_and(|found| found == target);
            return if ours { None } else { preserved(&link) };
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return unreadable(&link, e),
    }
    // A filesystem without symlinks, or a `.claude` the user cannot write,
    // leaves Claude Code without VEGA's skills but never fails init.
    #[cfg(unix)]
    use std::os::unix::fs::symlink as link_dir;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_dir as link_dir;
    if let Err(e) = fs::create_dir_all(&dir).and_then(|()| link_dir(&target, &link)) {
        return Some(format!(
            "could not link {} to {}: {e}; Claude Code does not find VEGA's skills until it exists",
            link.display(),
            target.display()
        ));
    }
    None
}

fn bytes(path: &Path) -> Result<Option<Vec<u8>>, String> {
    fs::read(path).map(Some).or_else(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(format!("{}: {e}", path.display()))
        }
    })
}

fn substitute(value: &mut Value, workspace: &str) -> Result<(), String> {
    match value {
        Value::String(s) => {
            *s = s.replace("{{workspace}}", workspace);
            if s.contains("{{") {
                return Err(format!("unresolved starter token {s:?}"));
            }
        }
        Value::Array(items) => {
            for item in items {
                substitute(item, workspace)?;
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                substitute(item, workspace)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn init(
    config_path: &Path,
    data_root: &Path,
    store_override: Option<&Path>,
    update: bool,
) -> Result<Vec<String>, String> {
    let config_path = absolute(config_path)?;
    // Each explicit alternate config gets its own default data/workspace
    // root. Sharing a workspace would also share boot instructions and keys.
    let native = crate::config::default_config_path()
        .map(|p| absolute(&p))
        .transpose()?;
    let data_root = if native.as_ref() == Some(&config_path) {
        absolute(data_root)?
    } else {
        absolute(data_root)?
            .join("instances")
            .join(&hash(config_path.to_string_lossy().as_bytes())[..24])
    };
    let data_root = data_root.as_path();
    let parent = config_path.parent().ok_or("config must have a parent")?;
    checked(&config_path)?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    checked(&parent.join("bootstrap.lock"))?;
    let _lock = LockGuard::acquire(&parent.join("bootstrap.lock")).map_err(|e| e.to_string())?;
    let config_tmp = parent.join(".fleet-bootstrap-tmp");
    checked(&config_tmp)?;
    fs::create_dir_all(&config_tmp).map_err(|e| e.to_string())?;
    // Resolve any existing config before creating other files; malformed
    // config is an error and never silently replaced.
    let existing = if bytes(&config_path)?.is_some() {
        Some(Config::load(&config_path)?)
    } else {
        None
    };
    let workspace = match &existing {
        Some(cfg) => cfg
            .starter
            .as_ref()
            .ok_or("existing config has no [starter] workspace; choose a fresh --config path")?
            .workspace
            .clone(),
        None => absolute(data_root)?.join("vega"),
    };
    let store_dir = match store_override {
        Some(path) => absolute(path)?,
        None => match &existing {
            Some(config) => config
                .data
                .dir
                .clone()
                .unwrap_or(absolute(data_root)?.join("store-v1")),
            None => absolute(data_root)?.join("store-v1"),
        },
    };
    if config_path.starts_with(&store_dir)
        || workspace.starts_with(&store_dir)
        || store_dir.starts_with(&workspace)
    {
        return Err(
            "data.dir must be a dedicated store directory outside the config and starter workspace"
                .into(),
        );
    }
    checked(&workspace)?;
    let meta = workspace.join(".fleet");
    checked(&meta)?;
    fs::create_dir_all(&meta).map_err(|e| e.to_string())?;
    // Two configs may intentionally point at one workspace: serialize those
    // operations too. Lock order is always config, then workspace.
    checked(&meta.join("lock"))?;
    let _workspace_lock = LockGuard::acquire(&meta.join("lock")).map_err(|e| e.to_string())?;
    let tmp = meta.join("tmp");
    checked(&tmp)?;
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let instance_path = meta.join("instance");
    checked(&instance_path)?;
    let candidate = hash(
        format!(
            "{}:{}:{}",
            config_path.display(),
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos()
        )
        .as_bytes(),
    );
    create_exclusive(&tmp, &instance_path, &candidate.as_bytes()[..24])
        .map_err(|e| e.to_string())?;
    let instance = fs::read_to_string(&instance_path).map_err(|e| e.to_string())?;
    if instance.len() != 24 || !instance.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid starter instance identity".into());
    }
    let state_path = meta.join("baseline.json");
    checked(&state_path)?;
    let mut state: Baseline = match bytes(&state_path)? {
        Some(b) => serde_json::from_slice(&b).map_err(|e| format!("starter baseline: {e}"))?,
        None => Baseline {
            version: 1,
            ..Baseline::default()
        },
    };
    if state.version != 1 {
        return Err("unsupported starter baseline version".into());
    }
    let mut notices = Vec::new();
    for (name, content) in FILES {
        let (key, path, rendered) = if *name == "config/fleet.toml" {
            let mut config: toml::Value =
                toml::from_str(std::str::from_utf8(content).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            let store_dir = match store_override {
                Some(path) => absolute(path)?,
                None => absolute(data_root)?.join("store-v1"),
            };
            config["data"]["dir"] = toml::Value::String(store_dir.to_string_lossy().into_owned());
            config["runtime"]["keyPrefix"] = toml::Value::String(format!("f{instance}"));
            // The three shipped ready policies, as the config page shows
            // them; ordinary entries once written.
            let policies = toml::Value::try_from(fleet_api::follow::shipped_policies())
                .map_err(|e| e.to_string())?;
            config
                .as_table_mut()
                .ok_or("config template must be a table")?
                .insert(
                    "forge".into(),
                    toml::Value::Table(toml::map::Map::from_iter([(
                        "readyPolicies".into(),
                        policies,
                    )])),
                );
            config
                .as_table_mut()
                .ok_or("config template must be a table")?
                .insert(
                    "starter".into(),
                    toml::Value::Table(toml::map::Map::from_iter([(
                        "workspace".into(),
                        toml::Value::String(workspace.to_string_lossy().into_owned()),
                    )])),
                );
            (
                "config".to_string(),
                config_path.clone(),
                toml::to_string_pretty(&config)
                    .map_err(|e| e.to_string())?
                    .into_bytes(),
            )
        } else if *name == "tasks/vega.json" {
            let mut task: Value = serde_json::from_slice(content).map_err(|e| e.to_string())?;
            substitute(
                &mut task,
                workspace.to_str().ok_or("workspace path is not UTF-8")?,
            )?;
            let tasks = task
                .as_array_mut()
                .ok_or("starter Tasks must be a JSON array")?;
            if tasks.len() != 1
                || tasks[0]["kind"] != "Task"
                || tasks[0]["metadata"]["name"] != "vega"
            {
                return Err("starter must contain exactly Task/vega".into());
            }
            let brief = tasks[0]["spec"]["brief"]
                .as_str()
                .ok_or("starter brief must be text")?;
            tasks[0]["spec"]["brief"] = Value::String(format!(
                "{brief}\n\nFleet configuration path (JSON string): {}. Use this exact file with fleet --config for every Task operation, and pass it to child workers. Do not initialize another store.",
                serde_json::to_string(&config_path).map_err(|e| e.to_string())?
            ));
            let obj: fleet_api::Object =
                serde_json::from_value(tasks[0].clone()).map_err(|e| e.to_string())?;
            fleet_api::validate::validate_object(&obj).map_err(|e| e.to_string())?;
            (
                "task.json".to_string(),
                workspace.join("task.json"),
                serde_json::to_vec_pretty(&task).map_err(|e| e.to_string())?,
            )
        } else {
            let relative = name.strip_prefix("workspace/").unwrap_or(name);
            if !Path::new(relative)
                .components()
                .all(|c| matches!(c, Component::Normal(_)))
                || relative.starts_with(".fleet/")
            {
                return Err("unsafe starter destination".into());
            }
            (
                relative.to_string(),
                workspace.join(relative),
                content.to_vec(),
            )
        };
        checked(&path)?;
        let current = bytes(&path)?;
        let wanted = hash(&rendered);
        if current.as_deref() == Some(&rendered) {
            state.files.insert(key, wanted);
            continue;
        }
        let baseline = state.files.get(&key);
        let pristine = current
            .as_ref()
            .zip(baseline)
            .is_some_and(|(b, h)| hash(b) == *h);
        // Config is user-owned after creation. A missing tracked file also
        // counts as customization. Updates never delete files.
        let install =
            (current.is_none() && baseline.is_none()) || (update && key != "config" && pristine);
        if install {
            fs::create_dir_all(path.parent().ok_or("starter path has no parent")?)
                .map_err(|e| e.to_string())?;
            if current.is_none() {
                let local_tmp = if key == "config" { &config_tmp } else { &tmp };
                create_exclusive(local_tmp, &path, &rendered).map_err(|e| e.to_string())?;
            } else {
                replace(&tmp, &path, &rendered).map_err(|e| e.to_string())?;
            }
            state.files.insert(key, wanted);
        } else if update {
            let conflict = meta.join("conflicts").join(&wanted).join(&key);
            checked(&conflict)?;
            fs::create_dir_all(conflict.parent().ok_or("conflict parent")?)
                .map_err(|e| e.to_string())?;
            create_exclusive(&tmp, &conflict, &rendered).map_err(|e| e.to_string())?;
            notices.push(format!(
                "preserved customization {}; candidate {}",
                path.display(),
                conflict.display()
            ));
        }
        // After each file, persist the baseline. A crash before this write
        // leaves an unowned file, which is only adopted if bytes match.
        state.source = serde_json::from_str(PROVENANCE).map_err(|e| e.to_string())?;
        replace(
            &tmp,
            &state_path,
            &serde_json::to_vec_pretty(&state).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    }
    notices.extend(link_claude_skills(&workspace));
    for (name, content) in [("starter.toml", MANIFEST), ("provenance.json", PROVENANCE)] {
        let path = meta.join(name);
        checked(&path)?;
        replace(&tmp, &path, content.as_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(notices)
}
