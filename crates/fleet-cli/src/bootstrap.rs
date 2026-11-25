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

// A symlinked ancestor is ordinary layout: `/home` on Fedora Atomic, `/var` on macOS.
fn checked(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err(format!("refusing a symlink at {}", path.display()))
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

pub(crate) fn native(config_path: &Path, store_dir: &Path) -> Result<String, String> {
    let instance = &hash(config_path.to_string_lossy().as_bytes())[..24];
    let mut config: toml::Value = toml::from_str(&format!(
        "version = 1\n\n[data]\ndir = {}\n\n[runtimes.mux]\nprogram = \"runtime-mux\"\ncallTimeoutMs = 10000\nkeyPrefix = \"f{instance}\"\n",
        toml::Value::String(store_dir.to_string_lossy().into_owned())
    ))
    .map_err(|e| e.to_string())?;
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

pub fn init(
    config_path: &Path,
    data_root: &Path,
    store_override: Option<&Path>,
) -> Result<bool, String> {
    let config_path = absolute(config_path)?;
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
