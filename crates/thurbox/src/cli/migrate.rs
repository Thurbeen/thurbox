use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use super::output::CommandOutput;
use super::CommandError;
use crate::fleet_link::FleetLink;

const SESSION_LABEL: &str = "thurbox.dev/v2-session";
const AUTOMATION_LABEL: &str = "thurbox.dev/v2-automation";
const FLEET_TIMEOUT: Duration = Duration::from_secs(30);

const SESSIONS_QUERY: &str = "SELECT s.id, s.name, s.agent, s.backend_type, s.agent_session_id, \
     s.cwd, w.repo_path, w.worktree_path, w.branch FROM sessions s LEFT JOIN worktrees w \
     ON w.session_id = s.id AND w.deleted_at IS NULL WHERE s.deleted_at IS NULL \
     ORDER BY s.created_at, s.id, w.repo_path";

const AUTOMATIONS_QUERY: &str = "SELECT id, name, enabled, schedule_kind, schedule_spec, \
     timezone, action_kind, repo_path, base_branch, agent, prompt, action_extra_repos \
     FROM automations ORDER BY id";

const PLACE_KEYS: [&str; 7] = [
    "kind",
    "destination",
    "ssh_opts",
    "distro",
    "multiplexer",
    "platform",
    "path_prepend",
];

const UTC_ZONES: [&str; 4] = ["UTC", "Etc/UTC", "GMT", "Etc/GMT"];

const KNOWN_RESUME: [(&str, &[&str]); 2] = [
    ("claude", &["claude", "--resume"]),
    ("codex", &["codex", "resume"]),
];

const AGENT_KEYS: [&str; 6] = [
    "name",
    "command",
    "args",
    "resume_args",
    "env",
    "additional_dir_args",
];

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Print the plan and change nothing.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Default)]
struct Report {
    imported: Vec<String>,
    already: Vec<String>,
    not_imported: Vec<String>,
    to_do: Vec<String>,
    resume: Vec<String>,
    places: String,
    harnesses: Vec<(String, String)>,
    settings: Vec<String>,
}

struct Agent {
    command: String,
    resume_args: Vec<String>,
}

pub fn run(
    args: Args,
    settings: &crate::session::settings::Settings,
) -> Result<CommandOutput, CommandError> {
    let dry_run = args.dry_run;
    let mut report = Report::default();
    let config_dir = crate::paths::config_file()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .ok_or("no thurbox config directory")?;
    let data_dir = crate::paths::data_directory().ok_or("no thurbox data directory")?;
    let link = FleetLink::in_force(settings);

    let agents = match read_toml(&config_dir.join("agents.toml"))? {
        Some(doc) => harness_profiles(&doc, &fleet_harnesses(&link), &mut report),
        None => BTreeMap::new(),
    };
    if let Some(doc) = read_toml(&config_dir.join("hosts.toml"))? {
        places(&doc, &mut report);
    }

    let db = data_dir.join("thurbox.db");
    let mut complete = false;
    if db.is_file() {
        match (query(&db, SESSIONS_QUERY), query(&db, AUTOMATIONS_QUERY)) {
            (Ok(sessions), Ok(automations)) => {
                let mut fleet = Fleet::read(link, dry_run, &mut report);
                for task in tasks(&sessions, &agents, &mut report) {
                    fleet.import(task, SESSION_LABEL, &mut report);
                }
                for cron in cron_tasks(&automations, &mut report) {
                    fleet.import(cron, AUTOMATION_LABEL, &mut report);
                }
                complete = fleet.reachable && !fleet.failed;
            }
            (Err(why), _) | (_, Err(why)) => report.to_do.push(why),
        }
    }

    clean_settings(dry_run, &mut report)?;

    if complete && !dry_run {
        if let Some(marker) = crate::paths::v2_migrated_marker() {
            crate::paths::write_atomically(&marker, b"thurbox-cli migrate\n")
                .map_err(|e| format!("{}: {e}", marker.display()))?;
        }
    }
    Ok(output(&report, dry_run))
}

fn read_toml(path: &Path) -> Result<Option<toml::Table>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn query(db: &Path, sql: &str) -> Result<Vec<Value>, String> {
    let out = std::process::Command::new("sqlite3")
        .arg("-readonly")
        .arg("-json")
        .arg(db)
        .arg(sql)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!(
                "sqlite3 is not installed, so {} was not read: install sqlite3 and run \
                 thurbox-cli migrate again to import the 2.x sessions and automations",
                db.display()
            ),
            _ => format!("sqlite3: {e}"),
        })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("-json") {
            return Err(format!(
                "sqlite3 is older than 3.33, which reads JSON, so {} was not read: install a \
                 newer sqlite3 and run thurbox-cli migrate again",
                db.display()
            ));
        }
        return Err(format!(
            "sqlite3 could not read {}: {}",
            db.display(),
            stderr.trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(&text).map_err(|e| format!("sqlite3 output: {e}"))
}

fn text<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row[key].as_str().filter(|s| !s.is_empty())
}

fn place_name(host: &str) -> String {
    crate::kernel::command::fleet::task_name(host)
}

fn host_of(backend_type: &str) -> Option<String> {
    let mut parts = backend_type.splitn(3, ':');
    match (parts.next(), parts.next()) {
        (Some("ssh" | "wsl"), Some(host)) if !host.is_empty() => {
            Some(format!("mux/{}", place_name(host)))
        }
        _ => None,
    }
}

fn short_id(id: &str) -> String {
    let id = crate::kernel::command::fleet::task_name(id);
    id.chars().take(8).collect()
}

fn tasks(rows: &[Value], agents: &BTreeMap<String, Agent>, report: &mut Report) -> Vec<Value> {
    let mut sessions: Vec<(&str, Vec<&Value>)> = Vec::new();
    for row in rows {
        let id = text(row, "id").unwrap_or_default();
        match sessions.iter_mut().find(|(sid, _)| *sid == id) {
            Some((_, group)) => group.push(row),
            None => sessions.push((id, vec![row])),
        }
    }
    let mut names = BTreeSet::new();
    let mut out = Vec::new();
    for (id, mut group) in sessions {
        let first = group[0];
        let name = text(first, "name").unwrap_or(id);
        let cwd = text(first, "cwd");
        group.sort_by_key(|row| text(row, "worktree_path") != cwd);
        let mut task = crate::kernel::command::fleet::task_name(name);
        if task.is_empty() || !names.insert(task.clone()) {
            task = format!(
                "{}-{}",
                if task.is_empty() { "session" } else { &task },
                short_id(id)
            );
            names.insert(task.clone());
        }
        let key = format!("Task/{task}");
        let mut workspace: Vec<String> = group
            .iter()
            .filter_map(|row| {
                let repo = text(row, "repo_path")?;
                let mut url = crate::kernel::command::fleet::dir_url("git+file", Path::new(repo));
                if let Some(branch) = text(row, "branch") {
                    url.push_str("?branch=");
                    url.push_str(&crate::kernel::command::fleet::encode(branch));
                }
                Some(url)
            })
            .collect();
        if workspace.is_empty() {
            match cwd {
                Some(cwd) => workspace.push(crate::kernel::command::fleet::dir_url(
                    "file",
                    Path::new(cwd),
                )),
                None => {
                    report
                        .not_imported
                        .push(format!("session {name:?}: it names no directory"));
                    continue;
                }
            }
        }
        let host = text(first, "backend_type")
            .and_then(host_of)
            .unwrap_or_else(|| "local".into());
        if host != "local" {
            report.to_do.push(format!(
                "{key}: runs on {host}; add [runtimes.{}] to Fleet's config (below)",
                host.replacen('/', ".places.", 1)
            ));
        }
        let mut spec = json!({
            "host": host,
            "brief": "",
            "workspace": workspace,
            "dispatch": false,
        });
        let agent = text(first, "agent");
        if let Some(agent) = agent {
            spec["harness"] = json!(agent);
        }
        if let (Some(agent), Some(conversation)) = (agent, text(first, "agent_session_id")) {
            report.resume.push(format!(
                "{key}: {}",
                resume_command(agent, conversation, cwd, agents)
            ));
        }
        let mut labels = serde_json::Map::new();
        labels.insert(
            crate::kernel::snapshot::fleet::NAME_LABEL.into(),
            json!(name),
        );
        labels.insert(SESSION_LABEL.into(), json!(id));
        out.push(json!({
            "apiVersion": "fleet/v1alpha1",
            "kind": "Task",
            "metadata": {"name": task, "labels": labels},
            "spec": spec,
        }));
    }
    out
}

fn resume_command(
    agent: &str,
    conversation: &str,
    cwd: Option<&str>,
    agents: &BTreeMap<String, Agent>,
) -> String {
    let quote = crate::shell::posix_quote;
    let argv: Vec<String> = match agents.get(agent).filter(|a| !a.resume_args.is_empty()) {
        Some(defined) => std::iter::once(defined.command.clone())
            .chain(
                defined
                    .resume_args
                    .iter()
                    .map(|a| a.replace("{id}", conversation)),
            )
            .collect(),
        None => match KNOWN_RESUME.iter().find(|(name, _)| *name == agent) {
            Some((_, words)) => words
                .iter()
                .map(|w| w.to_string())
                .chain([conversation.to_string()])
                .collect(),
            None => return format!("{agent} conversation {}", quote(conversation)),
        },
    };
    let command = argv.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ");
    match cwd {
        Some(cwd) => format!("cd {} && {command}", quote(cwd)),
        None => command,
    }
}

fn cron_tasks(rows: &[Value], report: &mut Report) -> Vec<Value> {
    let mut out = Vec::new();
    for row in rows {
        let name = text(row, "name").unwrap_or("automation");
        let id = row["id"].to_string();
        let action = text(row, "action_kind").unwrap_or_default();
        let schedule = text(row, "schedule_spec").unwrap_or_default();
        let timezone = text(row, "timezone");
        let why = if text(row, "schedule_kind") != Some("cron") {
            Some("it runs once; a CronTask repeats".to_string())
        } else if action != "spawn" {
            Some(format!(
                "its action is {action}; a CronTask only starts a Task"
            ))
        } else if !timezone.is_some_and(|zone| UTC_ZONES.contains(&zone)) {
            Some(format!(
                "it runs in {}; a CronTask schedule is UTC",
                timezone.unwrap_or("the machine's local time zone")
            ))
        } else if schedule.split_whitespace().count() != 5 && !schedule.starts_with('@') {
            Some(format!(
                "its schedule {schedule:?} is not a 5-field cron expression"
            ))
        } else if text(row, "repo_path").is_none() {
            Some("it names no repository".to_string())
        } else {
            None
        };
        if let Some(why) = why {
            report.not_imported.push(format!(
                "automation {name:?}: {why}; write a CronTask by hand"
            ));
            continue;
        }
        let cron = crate::kernel::command::fleet::task_name(name);
        let key = format!("CronTask/{cron}");
        if row["enabled"].as_i64() == Some(0) {
            report
                .to_do
                .push(format!("{key}: it was disabled in 2.x; keep it suspended"));
        }
        if text(row, "action_extra_repos").is_some_and(|extra| extra != "[]") {
            report.to_do.push(format!(
                "{key}: its other repositories were not carried over; add them to \
                 spec.template.spec.workspace"
            ));
        }
        let mut url = crate::kernel::command::fleet::dir_url(
            "git+file",
            Path::new(text(row, "repo_path").unwrap_or_default()),
        );
        if let Some(base) = text(row, "base_branch") {
            url.push('#');
            url.push_str(&crate::kernel::command::fleet::encode(base));
        }
        let mut template = json!({
            "host": "local",
            "brief": text(row, "prompt").unwrap_or_default(),
            "workspace": [url],
            "dispatch": true,
        });
        if let Some(agent) = text(row, "agent") {
            template["harness"] = json!(agent);
        }
        out.push(json!({
            "apiVersion": "fleet/v1alpha1",
            "kind": "CronTask",
            "metadata": {
                "name": cron,
                "labels": serde_json::Map::from_iter([(AUTOMATION_LABEL.to_string(), json!(id))]),
            },
            "spec": {
                "schedule": schedule,
                "suspend": true,
                "template": {"spec": template},
            },
        }));
    }
    out
}

struct Fleet {
    link: FleetLink,
    dry_run: bool,
    imported: BTreeSet<(String, String)>,
    reachable: bool,
    failed: bool,
}

impl Fleet {
    fn read(link: FleetLink, dry_run: bool, report: &mut Report) -> Fleet {
        let mut fleet = Fleet {
            link,
            dry_run,
            imported: BTreeSet::new(),
            reachable: true,
            failed: false,
        };
        for (kind, label) in [("task", SESSION_LABEL), ("crontask", AUTOMATION_LABEL)] {
            match fleet.list(kind) {
                Ok(objects) => {
                    for obj in objects {
                        if let Some(id) = obj["metadata"]["labels"][label].as_str() {
                            fleet.imported.insert((label.to_string(), id.to_string()));
                        }
                    }
                }
                Err(why) => {
                    fleet.reachable = false;
                    report.to_do.push(format!(
                        "fleet: {why}; nothing was imported into Fleet, so run thurbox-cli \
                         migrate again once fleet answers"
                    ));
                    break;
                }
            }
        }
        fleet
    }

    fn list(&self, kind: &str) -> Result<Vec<Value>, String> {
        let out = self
            .link
            .bounded_output(&["list", kind, "--json"], FLEET_TIMEOUT)?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        serde_json::from_slice::<Value>(&out.stdout)
            .map_err(|e| format!("fleet list {kind}: {e}"))
            .map(|v| v.as_array().cloned().unwrap_or_default())
    }

    fn import(&mut self, obj: Value, label: &str, report: &mut Report) {
        let key = format!(
            "{}/{}",
            obj["kind"].as_str().unwrap_or_default(),
            obj["metadata"]["name"].as_str().unwrap_or_default()
        );
        let id = obj["metadata"]["labels"][label]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if !self.reachable {
            return;
        }
        if self.imported.contains(&(label.to_string(), id)) {
            report.already.push(key);
            return;
        }
        let summary = match obj["kind"].as_str() {
            Some("Task") => format!("{key}: {}", obj["spec"]["workspace"]),
            _ => format!("{key}: {}", obj["spec"]["schedule"]),
        };
        if self.dry_run {
            report.imported.push(summary);
            return;
        }
        match self.link.run(
            &["apply", "--create-only", "-f", "-"],
            Some(&obj.to_string()),
        ) {
            Ok(()) => report.imported.push(summary),
            Err(why) => {
                self.failed = true;
                report.not_imported.push(format!("{key}: {why}"));
            }
        }
    }
}

fn fleet_harnesses(link: &FleetLink) -> BTreeSet<String> {
    link.bounded_output(&["version", "--json"], FLEET_TIMEOUT)
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| serde_json::from_slice::<Value>(&out.stdout).ok())
        .and_then(|v| v["harnesses"].as_array().cloned())
        .map(|names| {
            names
                .iter()
                .filter_map(|n| n.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn harness_profiles(
    doc: &toml::Table,
    shipped: &BTreeSet<String>,
    report: &mut Report,
) -> BTreeMap<String, Agent> {
    let mut agents = BTreeMap::new();
    let strings = |value: Option<&toml::Value>| -> Vec<String> {
        value
            .and_then(toml::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    for entry in doc
        .get("agents")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_table)
    {
        let Some(name) = entry.get("name").and_then(toml::Value::as_str) else {
            continue;
        };
        let command = entry.get("command").and_then(toml::Value::as_str);
        let resume_args = strings(entry.get("resume_args"));
        if let Some(command) = command {
            agents.insert(
                name.to_string(),
                Agent {
                    command: command.to_string(),
                    resume_args: resume_args.clone(),
                },
            );
        }
        if name == "shell" || shipped.contains(name) {
            continue;
        }
        let Some(command) = command else {
            report.to_do.push(format!(
                "agents.toml {name:?}: it names no command (extends another?); write its \
                 harness profile by hand"
            ));
            continue;
        };
        let mut left: Vec<String> = entry
            .keys()
            .filter(|k| !AGENT_KEYS.contains(&k.as_str()))
            .cloned()
            .collect();
        let mut profile = toml::Table::new();
        profile.insert("name".into(), name.into());
        profile.insert("program".into(), command.into());
        profile.insert("authority".into(), "none".into());
        let mut launch = toml::Table::new();
        let args = strings(entry.get("args"));
        if args.iter().any(|a| a.contains('{')) {
            left.push("a {placeholder} in args".into());
        }
        if !args.is_empty() {
            launch.insert("args".into(), args.into());
        }
        if let Some(env) = entry.get("env").and_then(toml::Value::as_table) {
            launch.insert("env".into(), toml::Value::Table(env.clone()));
        }
        match strings(entry.get("additional_dir_args")).as_slice() {
            [] => {}
            [flag, dir] if dir == "{dir}" => {
                launch.insert("add_dir".into(), flag.as_str().into());
            }
            _ => left.push("additional_dir_args".into()),
        }
        if !launch.is_empty() {
            profile.insert("launch".into(), toml::Value::Table(launch));
        }
        if !resume_args.is_empty() {
            let args: Vec<String> = ["{program}", "{launch_args}"]
                .into_iter()
                .map(str::to_string)
                .chain(
                    resume_args
                        .iter()
                        .map(|a| a.replace("{id}", "{conversation}")),
                )
                .collect();
            profile.insert(
                "resume".into(),
                toml::Value::Table(toml::Table::from_iter([("args".to_string(), args.into())])),
            );
        }
        if !left.is_empty() {
            report.to_do.push(format!(
                "agents.toml {name:?}: not carried over: {}; a setup step belongs in a wrapper \
                 script, the rest in its harness profile by hand",
                left.join(", ")
            ));
        }
        report.harnesses.push((
            format!("harnesses/{name}.toml"),
            toml::to_string(&profile).unwrap_or_default(),
        ));
    }
    agents
}

fn places(doc: &toml::Table, report: &mut Report) {
    let mut runtimes = toml::Table::new();
    for entry in doc
        .get("hosts")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_table)
    {
        let Some(name) = entry.get("name").and_then(toml::Value::as_str) else {
            continue;
        };
        let place = place_name(name);
        let mut table = toml::Table::new();
        table.insert("kind".into(), "ssh".into());
        if entry.get("kind").and_then(toml::Value::as_str) == Some("wsl")
            && !entry.contains_key("distro")
        {
            table.insert("distro".into(), name.into());
        }
        for (key, value) in entry {
            if key == "name" || key == "config_version" {
                continue;
            }
            if PLACE_KEYS.contains(&key.as_str()) {
                table.insert(key.clone(), value.clone());
                continue;
            }
            let why = match key.as_str() {
                "socket" | "session" => "it named the 2.x tmux server; runtime-mux uses its own",
                _ => "a runtime-mux place has no such key",
            };
            report.to_do.push(format!(
                "hosts.toml {name:?}: {key} was not carried over: {why}"
            ));
        }
        runtimes.insert(place, toml::Value::Table(table));
    }
    if runtimes.is_empty() {
        return;
    }
    let doc = toml::Table::from_iter([(
        "runtimes".to_string(),
        toml::Value::Table(toml::Table::from_iter([(
            "mux".to_string(),
            toml::Value::Table(toml::Table::from_iter([(
                "places".to_string(),
                toml::Value::Table(runtimes),
            )])),
        )])),
    )]);
    report.places = toml::to_string(&doc).unwrap_or_default();
}

fn clean_settings(dry_run: bool, report: &mut Report) -> Result<(), String> {
    let Some(path) = crate::config::settings::settings_config_path() else {
        return Ok(());
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let Ok(mut doc) = text.parse::<toml_edit::DocumentMut>() else {
        return Ok(());
    };
    let dropped = crate::config::settings::drop_retired_keys(&mut doc);
    if dropped.is_empty() {
        return Ok(());
    }
    let mut kept = path.clone().into_os_string();
    kept.push(".v2");
    let kept = PathBuf::from(kept);
    report.settings = dropped.iter().map(|k| (*k).to_string()).collect();
    report
        .settings
        .push(format!("the file before is kept as {}", kept.display()));
    if dry_run {
        return Ok(());
    }
    if !kept.exists() {
        crate::paths::write_atomically(&kept, &text)
            .map_err(|e| format!("{}: {e}", kept.display()))?;
    }
    crate::paths::write_atomically(&path, doc.to_string())
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn output(report: &Report, dry_run: bool) -> CommandOutput {
    let mut human = String::new();
    if dry_run {
        human.push_str("thurbox-cli migrate: dry run, nothing was changed\n");
    }
    let mut section = |title: &str, lines: &[String]| {
        if lines.is_empty() {
            return;
        }
        human.push_str(&format!("\n{title}:\n"));
        for line in lines {
            human.push_str(&format!("  {line}\n"));
        }
    };
    section(
        if dry_run { "Imports" } else { "Imported" },
        &report.imported,
    );
    section("Already imported", &report.already);
    section("Not imported", &report.not_imported);
    section(
        if dry_run {
            "Removes from settings.toml"
        } else {
            "Removed from settings.toml"
        },
        &report.settings,
    );
    section(
        "Resume a 2.x conversation in its 2.x directory",
        &report.resume,
    );
    let mut to_do = report.to_do.clone();
    if !report.imported.is_empty() {
        to_do.push(
            "each imported Task has spec.dispatch = false and each CronTask spec.suspend = \
             true: change it and fleet apply it to start"
                .into(),
        );
    }
    section("For you to do", &to_do);
    if !report.places.is_empty() {
        human.push_str("\nPaste into Fleet's config (fleet doctor names its path):\n\n");
        human.push_str(&report.places);
    }
    for (file, text) in &report.harnesses {
        human.push_str(&format!(
            "\nWrite as {file} beside Fleet's config:\n\n{text}"
        ));
    }
    if human.trim().is_empty() {
        human.push_str("No thurbox 2.x state found: nothing to migrate\n");
    }
    let json = json!({
        "dry_run": dry_run,
        "imported": report.imported,
        "already_imported": report.already,
        "not_imported": report.not_imported,
        "settings_removed": report.settings,
        "resume": report.resume,
        "to_do": to_do,
        "places": report.places,
        "harnesses": report
            .harnesses
            .iter()
            .map(|(file, text)| json!({"file": file, "toml": text}))
            .collect::<Vec<_>>(),
    });
    CommandOutput::new(json, human)
}
