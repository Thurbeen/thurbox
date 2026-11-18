use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use fleet_api::{Object, Store, StoreError};
use fleet_store::FileStore;
use fleet_store::durable::{replace, sync_parent};
use serde_json::Value;

use crate::config::{Config, fleet_2_config};

const IMPORTED_KINDS: [&str; 3] = ["Task", "TaskConfig", "EnvProvider"];

const DROPPED_RUNTIME_KEYS: [(&str, &str); 8] = [
    ("kind", "runtime-mux runs the sessions"),
    ("cli", "runtime-mux runs the sessions"),
    ("callTimeoutMs", "[runtimes.mux] has its own"),
    ("keyPrefix", "[runtimes.mux] has a key prefix of its own"),
    (
        "repositories",
        "workspaces name their checkouts as git+file:// URLs",
    ),
    (
        "modelEnv",
        "no runtime passes a model; set it in the harness's own configuration",
    ),
    (
        "workspaceTrust",
        "each harness profile says how its workspace is trusted",
    ),
    (
        "workspaceCleanup",
        "workspace-git removes a Task's worktree and keeps its branch",
    ),
];

const DROPPED_TABLES: [(&str, &str); 3] = [
    ("starter", "fleet up starts no starter Task"),
    ("workspaces", "each runtime makes its own workspaces"),
    ("hosts", "a Task names a place under [runtimes.mux.places]"),
];

const COPIED_TABLES: [&str; 3] = ["forge", "plugins", "gateway"];

pub struct Migration {
    pub config_path: PathBuf,
    pub config_v2: PathBuf,
    pub store: PathBuf,
    pub store_v2: PathBuf,
    staged: PathBuf,
    journal: PathBuf,
    resumed: bool,
    store_2x: bool,
    source_text: String,
    config_text: String,
    objects: Vec<Object>,
    pub imported: Vec<String>,
    pub dropped: Vec<String>,
    pub to_do: Vec<String>,
}

pub enum Plan {
    Done {
        config_v2: Option<PathBuf>,
        journal: Option<PathBuf>,
    },
    Migrate(Box<Migration>),
}

fn v2(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".v2");
    PathBuf::from(name)
}

fn exists(path: &Path) -> Result<bool, String> {
    path.try_exists()
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn read(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

pub fn plan(config_path: &Path) -> Result<Plan, String> {
    let config_v2 = v2(config_path);
    let journal = sibling(config_path, ".migrating");
    let resumed = read(&journal)?;
    let source = match read(config_path)? {
        Some(text) if fleet_2_config(&text, config_path).is_some() => text,
        Some(text) => {
            if resumed.is_some() {
                Config::parse_settings(&text, config_path)
                    .map_err(|why| format!("{why}; {} stays", journal.display()))?;
            }
            return Ok(Plan::Done {
                config_v2: exists(&config_v2)?.then_some(config_v2),
                journal: resumed.is_some().then_some(journal),
            });
        }
        None => return Err(format!("no Fleet 2 config at {}", config_path.display())),
    };
    let (_, store) = fleet_2_config(&source, config_path).expect("checked above");
    let store = crate::bootstrap::absolute(&store)?;
    let store_v2 = v2(&store);
    let staged = sibling(&store, ".v3");
    let store_2x = match &resumed {
        Some(text) => serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|j| j["store2x"].as_bool())
            .ok_or_else(|| format!("{} is not a migration journal", journal.display()))?,
        None => {
            for path in [&config_v2, &store_v2, &staged] {
                left_over(path)?;
            }
            exists(&store)?
        }
    };
    let mut migration = Migration {
        config_path: config_path.to_path_buf(),
        config_v2,
        store_v2,
        staged,
        journal,
        resumed: resumed.is_some(),
        store_2x,
        source_text: source.clone(),
        config_text: String::new(),
        objects: Vec::new(),
        imported: Vec::new(),
        dropped: Vec::new(),
        to_do: Vec::new(),
        store,
    };
    let table: toml::Table = toml::from_str(&source).map_err(|e| e.to_string())?;
    migration.config_text = migration.config(&table)?;
    if store_2x {
        let source_store = if exists(&migration.store_v2)? {
            migration.store_v2.clone()
        } else {
            migration.store.clone()
        };
        no_manager(&source_store)?;
        let repositories = table
            .get("runtime")
            .and_then(|r| r.get("repositories"))
            .and_then(toml::Value::as_table)
            .map(|t| {
                t.iter()
                    .filter_map(|(url, dir)| Some((url.clone(), dir.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let key_prefix = table
            .get("runtime")
            .and_then(|r| r.get("keyPrefix"))
            .and_then(toml::Value::as_str)
            .unwrap_or("fleet")
            .to_string();
        migration.objects(&source_store, &repositories, &key_prefix)?;
    }
    Ok(Plan::Migrate(Box::new(migration)))
}

fn left_over(path: &Path) -> Result<(), String> {
    if exists(path)? {
        return Err(format!(
            "{} exists; move it away, then run fleet migrate again",
            path.display()
        ));
    }
    Ok(())
}

fn no_manager(store: &Path) -> Result<(), String> {
    let Some(text) = read(&store.join("lease.json"))? else {
        return Ok(());
    };
    let lease: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let expires = lease["expiresAt"].as_u64().unwrap_or(0);
    if expires > now_ms() {
        return Err(format!(
            "a Fleet 2 manager ({}) holds the store {}; stop it, then run fleet migrate again",
            lease["holder"].as_str().unwrap_or("unknown"),
            store.display()
        ));
    }
    Ok(())
}

impl Migration {
    fn config(&mut self, source: &toml::Table) -> Result<String, String> {
        let native = crate::bootstrap::native(&self.config_path, &self.store)?;
        let mut config: toml::Table = toml::from_str(&native).map_err(|e| e.to_string())?;
        if let Some(runtime) = source.get("runtime").and_then(toml::Value::as_table) {
            for key in runtime.keys() {
                let why = DROPPED_RUNTIME_KEYS
                    .iter()
                    .find(|(k, _)| k == key)
                    .map_or("Fleet 3 has no such key", |(_, why)| why);
                self.dropped.push(format!("[runtime] {key}: {why}"));
            }
        }
        for (key, value) in source {
            if ["version", "data", "runtime"].contains(&key.as_str()) {
                continue;
            }
            if COPIED_TABLES.contains(&key.as_str()) {
                let mut value = value.clone();
                if key == "forge"
                    && let (Some(copied), Some(shipped)) = (
                        value.as_table_mut(),
                        config.get("forge").and_then(|f| f.get("readyPolicies")),
                    )
                {
                    copied
                        .entry("readyPolicies")
                        .or_insert_with(|| shipped.clone());
                }
                config.insert(key.clone(), value);
                continue;
            }
            let why = DROPPED_TABLES
                .iter()
                .find(|(k, _)| k == key)
                .map_or("Fleet 3 has no such table", |(_, why)| why);
            self.dropped.push(format!("[{key}]: {why}"));
        }
        let mut text = toml::to_string_pretty(&config).map_err(|e| e.to_string())?;
        if let Err(why) = Config::parse_settings(&text, &self.config_path)
            && config.remove("gateway").is_some()
        {
            self.to_do.push(format!(
                "[gateway] was not copied ({why}); copy it from {} by hand",
                self.config_v2.display()
            ));
            text = toml::to_string_pretty(&config).map_err(|e| e.to_string())?;
        }
        Config::parse_settings(&text, &self.config_path)?;
        Ok(text)
    }

    fn objects(
        &mut self,
        store: &Path,
        repositories: &BTreeMap<String, String>,
        key_prefix: &str,
    ) -> Result<(), String> {
        let root = store.join("objects");
        let kinds = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("{}: {e}", root.display())),
        };
        let mut paths: Vec<PathBuf> = Vec::new();
        for kind in kinds {
            let kind = kind.map_err(|e| e.to_string())?.path();
            for file in fs::read_dir(&kind).map_err(|e| format!("{}: {e}", kind.display()))? {
                paths.push(file.map_err(|e| e.to_string())?.path());
            }
        }
        paths.sort();
        for path in paths
            .iter()
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
        {
            let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let source: Value = serde_json::from_str(&text)
                .map_err(|e| format!("{}: not JSON: {e}", path.display()))?;
            self.object(source, repositories, key_prefix);
        }
        Ok(())
    }

    fn object(&mut self, source: Value, repositories: &BTreeMap<String, String>, prefix: &str) {
        let kind = source["kind"].as_str().unwrap_or_default().to_string();
        let name = source["metadata"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let key = format!("{kind}/{name}");
        let kept = format!("its record stays in {}", self.store_v2.display());
        if !IMPORTED_KINDS.contains(&kind.as_str()) {
            self.dropped
                .push(format!("{key}: Fleet 3 does not import a {kind}; {kept}"));
            return;
        }
        if source["metadata"]["deletionTimestamp"].is_number() {
            self.dropped
                .push(format!("{key}: it was being deleted; {kept}"));
            return;
        }
        if kind == "Task" && source["status"]["phase"] == "Concluded" {
            self.dropped.push(format!("{key}: it concluded; {kept}"));
            return;
        }
        let mut spec = source["spec"].as_object().cloned().unwrap_or_default();
        if spec.remove("model").is_some() {
            self.to_do.push(format!(
                "{key}: spec.model was removed; set the model in the harness's own configuration"
            ));
        }
        if let Some(host) = spec.get("host").and_then(Value::as_str)
            && host != fleet_api::task::LOCAL_HOST
            && !host.contains('/')
        {
            let place = format!("mux/{host}");
            self.to_do.push(format!(
                "{key}: runs on {place}; add [runtimes.mux.places.{host}] to the config"
            ));
            spec.insert("host".into(), Value::String(place));
        }
        if kind == "Task" {
            if spec
                .remove("amendments")
                .is_some_and(|a| a != Value::Array(vec![]))
            {
                self.to_do.push(format!(
                    "{key}: its brief's amendments were not imported; {kept}"
                ));
            }
            let branch = source["status"]["session"]["key"]
                .as_str()
                .map(|session| format!("{prefix}/{session}"));
            match workspace(spec.get("workspace"), repositories, branch.as_deref()) {
                Some(urls) => {
                    spec.insert("workspace".into(), urls);
                }
                None => {
                    self.dropped
                        .push(format!("{key}: its workspace has no Fleet 3 form; {kept}"));
                    return;
                }
            }
            spec.insert("dispatch".into(), Value::Bool(false));
        }
        let mut labels = source["metadata"]["labels"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        labels.retain(|label, _| !label.starts_with("gateway.fleet.dev/"));
        let obj = serde_json::json!({
            "apiVersion": fleet_api::API_VERSION,
            "kind": kind,
            "metadata": {"name": name, "labels": labels},
            "spec": spec,
        });
        match serde_json::from_value::<Object>(obj) {
            Ok(obj) => {
                self.imported.push(match kind.as_str() {
                    "Task" => format!("{key}: {}", obj.spec["workspace"]),
                    _ => key,
                });
                self.objects.push(obj);
            }
            Err(e) => self.dropped.push(format!("{key}: {e}; {kept}")),
        }
    }

    pub fn run(&mut self, seed: &dyn Fn(&dyn Store) -> Result<(), String>) -> Result<(), String> {
        let parent = self
            .config_path
            .parent()
            .ok_or("config must have a parent")?
            .to_path_buf();
        if !self.resumed {
            let journal = serde_json::json!({"store2x": self.store_2x}).to_string();
            write_new(&self.journal, journal.as_bytes())?;
        }
        if exists(&self.staged)? && !(self.store_2x && exists(&self.store_v2)?) {
            fs::remove_dir_all(&self.staged)
                .map_err(|e| format!("{}: {e}", self.staged.display()))?;
        }
        let installed = !exists(&self.staged)?
            && exists(if self.store_2x {
                &self.store_v2
            } else {
                &self.store
            })?;
        if !installed && !exists(&self.staged)? {
            self.stage(seed)?;
        }
        if read(&self.config_v2)?.as_deref() != Some(self.source_text.as_str()) {
            copy(&self.config_path, &self.config_v2)?;
        }
        if exists(&self.staged)? {
            if self.store_2x && !exists(&self.store_v2)? {
                rename(&self.store, &self.store_v2)?;
            }
            rename(&self.staged, &self.store)?;
        }
        replace(&parent, &self.config_path, self.config_text.as_bytes())
            .map_err(|e| format!("{}: {e}", self.config_path.display()))?;
        fs::remove_file(&self.journal).map_err(|e| format!("{}: {e}", self.journal.display()))
    }

    fn stage(&mut self, seed: &dyn Fn(&dyn Store) -> Result<(), String>) -> Result<(), String> {
        let mut partial = self.staged.as_os_str().to_owned();
        partial.push(".partial");
        let partial = PathBuf::from(partial);
        if exists(&partial)? {
            fs::remove_dir_all(&partial).map_err(|e| format!("{}: {e}", partial.display()))?;
        }
        let store = FileStore::open(&partial).map_err(|e| e.to_string())?;
        let policies: BTreeSet<String> =
            Config::parse_settings(&self.config_text, &self.config_path)?
                .forge
                .map(|f| f.ready_policies.keys().cloned().collect())
                .unwrap_or_default();
        for obj in std::mem::take(&mut self.objects) {
            let key = obj.key();
            match fleet_manager::verbs::apply(&store, obj, &policies, true) {
                Ok(_) => {}
                Err(StoreError::Invalid(why)) => {
                    let named = format!("{key}:");
                    self.imported
                        .retain(|line| *line != key && !line.starts_with(&named));
                    self.dropped.push(format!(
                        "{key}: {why}; its record stays in {}",
                        self.store_v2.display()
                    ));
                }
                Err(e) => return Err(format!("{key}: {e}")),
            }
        }
        seed(&store)?;
        drop(store);
        rename(&partial, &self.staged)
    }
}

fn workspace(
    source: Option<&Value>,
    repositories: &BTreeMap<String, String>,
    branch: Option<&str>,
) -> Option<Value> {
    let source = source?.as_object()?;
    if let Some(dir) = source.get("dir").and_then(Value::as_str) {
        return Some(Value::Array(vec![Value::String(
            fleet_api::task::file_url(dir),
        )]));
    }
    let urls = source
        .get("repositories")?
        .as_array()?
        .iter()
        .map(|repository| {
            let url = repository["url"].as_str()?;
            let mut out = match repositories.get(url) {
                Some(checkout) => {
                    format!("git+{}", fleet_api::task::file_url(checkout))
                }
                None => format!("git+{url}"),
            };
            if let Some(branch) = branch {
                out.push_str(&format!("?branch={branch}"));
            }
            if let Some(reference) = repository["ref"].as_str() {
                out.push_str(&format!("#{reference}"));
            }
            Some(Value::String(out))
        })
        .collect::<Option<Vec<_>>>()?;
    (!urls.is_empty()).then_some(Value::Array(urls))
}

fn synced(path: &Path, result: std::io::Result<()>) -> Result<(), String> {
    result
        .and_then(|()| sync_parent(path))
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        });
    synced(path, written)
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    let copied = fs::copy(from, to).and_then(|_| fs::File::open(to)?.sync_all());
    synced(to, copied)
}

fn rename(from: &Path, to: &Path) -> Result<(), String> {
    fs::rename(from, to)
        .and_then(|()| sync_parent(to))
        .map_err(|e| format!("cannot move {} to {}: {e}", from.display(), to.display()))
}
pub fn report(m: &Migration, dry_run: bool) -> String {
    let mut out = String::new();
    if dry_run {
        out.push_str("fleet migrate: dry run, nothing was changed\n\n");
    }
    let verb = |done: &str, will: &str| {
        if dry_run {
            will.to_string()
        } else {
            done.to_string()
        }
    };
    out.push_str(&format!(
        "The config {} {} Fleet 3's; the 2.x config is kept as {}\n",
        m.config_path.display(),
        verb("is", "becomes"),
        m.config_v2.display()
    ));
    out.push_str(&format!(
        "The store {} {} Fleet 3's",
        m.store.display(),
        verb("is", "becomes")
    ));
    if m.store_2x {
        out.push_str(&format!(
            "; the 2.x store is kept as {}\n",
            m.store_v2.display()
        ));
    } else {
        out.push_str("; the 2.x config named no store that exists\n");
    }
    let section = |out: &mut String, title: &str, lines: &[String]| {
        if lines.is_empty() {
            return;
        }
        out.push_str(&format!("\n{title}:\n"));
        for line in lines {
            out.push_str(&format!("  {line}\n"));
        }
    };
    section(&mut out, &verb("Imported", "Imports"), &m.imported);
    section(&mut out, &verb("Dropped", "Drops"), &m.dropped);
    let mut to_do = m.to_do.clone();
    if m.imported.iter().any(|line| line.starts_with("Task/")) {
        to_do.push(
            "each imported Task has spec.dispatch = false: set it to true and fleet apply it \
             to start its session"
                .into(),
        );
    }
    section(&mut out, "For you to do", &to_do);
    out
}
