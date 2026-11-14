//! `fleet init`: the native config, written once, offline. The store and the
//! Tasks are the store's, not this module's.
use std::fs;
use std::path::{Path, PathBuf};

use fleet_store::durable::{LockGuard, create_exclusive};
use sha2::{Digest, Sha256};

use crate::config::Config;

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn absolute(path: &Path) -> Result<PathBuf, String> {
    std::path::absolute(path).map_err(|e| e.to_string())
}

/// Refuse a symlink anywhere on the config's path, ancestors included: a
/// link is never an invitation to write its target.
fn checked(path: &Path) -> Result<(), String> {
    for parent in path.ancestors() {
        match fs::symlink_metadata(parent) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!("refusing a symlink at {}", parent.display()));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", parent.display())),
        }
    }
    Ok(())
}

/// The config `init` writes for `config_path`, with its store at
/// `store_dir`. `[runtimes.mux]` takes a key prefix of its own per config
/// path, so two configs on one machine never share a session.
fn native(config_path: &Path, store_dir: &Path) -> Result<String, String> {
    let instance = &hash(config_path.to_string_lossy().as_bytes())[..24];
    let mut config: toml::Value = toml::from_str(&format!(
        "version = 1\n\n[data]\ndir = {}\n\n[runtimes.mux]\nprogram = \"runtime-mux\"\ncallTimeoutMs = 10000\nkeyPrefix = \"f{instance}\"\n",
        toml::Value::String(store_dir.to_string_lossy().into_owned())
    ))
    .map_err(|e| e.to_string())?;
    // The three shipped ready policies, as the config page shows them;
    // ordinary entries once written.
    let policies =
        toml::Value::try_from(fleet_api::follow::shipped_policies()).map_err(|e| e.to_string())?;
    config
        .as_table_mut()
        .ok_or("the config must be a table")?
        .insert(
            "forge".into(),
            toml::Value::Table(toml::map::Map::from_iter([(
                "readyPolicies".into(),
                policies,
            )])),
        );
    toml::to_string_pretty(&config).map_err(|e| e.to_string())
}

/// Write the config at `config_path` when it does not exist, and answer
/// whether it did. An existing config is the user's: it is validated and
/// kept as it is.
pub fn init(
    config_path: &Path,
    data_root: &Path,
    store_override: Option<&Path>,
) -> Result<bool, String> {
    let config_path = absolute(config_path)?;
    // Each explicit alternate config gets its own default store.
    let native_path = crate::config::default_config_path()
        .map(|p| absolute(&p))
        .transpose()?;
    let data_root = if native_path.as_ref() == Some(&config_path) {
        absolute(data_root)?
    } else {
        absolute(data_root)?
            .join("instances")
            .join(&hash(config_path.to_string_lossy().as_bytes())[..24])
    };
    let store_dir = match store_override {
        Some(path) => absolute(path)?,
        None => data_root.join("store-v1"),
    };
    if config_path.starts_with(&store_dir) {
        return Err("data.dir must be a dedicated store directory outside the config".into());
    }
    let parent = config_path.parent().ok_or("config must have a parent")?;
    checked(&config_path)?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let lock = parent.join("bootstrap.lock");
    checked(&lock)?;
    let _lock = LockGuard::acquire(&lock).map_err(|e| e.to_string())?;
    if config_path.try_exists().map_err(|e| e.to_string())? {
        // A malformed config is an error, never silently replaced.
        Config::load(&config_path)?;
        return Ok(false);
    }
    let tmp = parent.join(".fleet-bootstrap-tmp");
    checked(&tmp)?;
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    create_exclusive(
        &tmp,
        &config_path,
        native(&config_path, &store_dir)?.as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    Ok(true)
}
