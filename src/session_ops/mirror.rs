//! Mirroring a shareable host's database into local rows.
//!
//! The host's `thurbox-cli session list --json` (and `--deleted`) is the
//! record; this module reconciles the local rows on that host's backend to it
//! — adopting what is new, updating what changed, deleting and restoring what
//! the host says was deleted and restored — and writes nothing when nothing
//! moved, so an idle mirror does not bump `data_version` for every peer. The
//! observer keeps what is its own: display order, the companion shell, and a
//! pane id the host does not report.
//!
//! One JSON shape serves both directions: [`session_to_json`] is what the CLI
//! prints and what `session register` reads back, so the mirror and the
//! adoption of a legacy row cannot disagree about a field.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde_json::{json, Value};

use super::host_cli::{self, CliInfo, Usable};
use crate::session::{Assessment, HostDef, SessionId};
use crate::storage::Database;
use crate::sync::{SharedSession, SharedWorktree};

/// What one mirror pass did for one host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MirrorReport {
    pub host: String,
    pub adopted: Vec<SessionId>,
    pub updated: Vec<SessionId>,
    pub deleted: Vec<SessionId>,
    pub restored: Vec<SessionId>,
    /// Local rows on this backend the host knows nothing about: sessions
    /// created here before the host's database became the record. Left alone;
    /// `sync --adopt` registers them on the host.
    pub unknown_local: Vec<SessionId>,
    /// Of `unknown_local`, the ones `--adopt` registered this pass.
    pub registered: Vec<SessionId>,
    /// Why the host could not be mirrored, when it could not.
    pub error: Option<String>,
}

impl MirrorReport {
    pub fn changed(&self) -> bool {
        !(self.adopted.is_empty()
            && self.updated.is_empty()
            && self.deleted.is_empty()
            && self.restored.is_empty()
            && self.registered.is_empty())
    }

    pub fn to_json(&self) -> Value {
        let ids =
            |v: &Vec<SessionId>| -> Vec<String> { v.iter().map(|id| id.to_string()).collect() };
        json!({
            "host": self.host,
            "adopted": ids(&self.adopted),
            "updated": ids(&self.updated),
            "deleted": ids(&self.deleted),
            "restored": ids(&self.restored),
            "unknown_local": ids(&self.unknown_local),
            "registered": ids(&self.registered),
            "error": self.error,
        })
    }
}

/// A session as the host lists it, already on the observer's backend name.
#[derive(Debug, Clone, PartialEq)]
pub struct HostRow {
    pub session: SharedSession,
    pub hook_state: Option<String>,
    pub base_branch: Option<String>,
}

/// A deleted session as the host lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDeletedRow {
    pub id: SessionId,
    pub force_deleted: bool,
}

/// The one JSON shape of a session: what `session list`/`get` print, what a
/// peer's mirror reads, and what `session register` accepts.
pub fn session_to_json(
    s: &SharedSession,
    hook_state: Option<&str>,
    base_branch: Option<&str>,
) -> Value {
    json!({
        "id": s.id.to_string(),
        "name": s.name,
        "agent": s.agent,
        "backend_type": s.backend_type,
        "backend_id": s.backend_id,
        "agent_session_id": s.agent_session_id,
        "cwd": s.cwd.as_ref().map(|p| p.display().to_string()),
        "additional_dirs": s.additional_dirs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "parent_session_id": s.parent_session_id.map(|id| id.to_string()),
        "display_order": s.display_order,
        "base_branch": base_branch,
        "hook_state": hook_state,
        "worktrees": s.worktrees.iter().map(|w| json!({
            "repo_path": w.repo_path.display().to_string(),
            "worktree_path": w.worktree_path.display().to_string(),
            "branch": w.branch,
            "created_by_thurbox": w.created_by_thurbox,
        })).collect::<Vec<_>>(),
    })
}

/// [`session_to_json`] plus everything [`Assessment`] knows about the session's
/// agent state: how old the report is, what its agent is able to report at all,
/// and — when the caller looked — what the pane's foreground process says.
///
/// A superset of the mirror's wire shape rather than a second one, so a peer
/// reading it with [`session_from_json`] still finds every field it knows and
/// simply ignores the rest. `hook_state` keeps its exact meaning and value:
/// nothing here is derived into it, so a consumer that only ever read that word
/// is unaffected.
pub fn session_to_json_assessed(
    s: &SharedSession,
    hook: &Assessment,
    base_branch: Option<&str>,
) -> Value {
    let mut value = session_to_json(s, hook.hook_state.as_deref(), base_branch);
    let Some(obj) = value.as_object_mut() else {
        return value;
    };
    let mut put = |key: &str, v: Value| {
        obj.insert(key.to_string(), v);
    };
    put("hook_state_at", json!(hook.state_at));
    put("hook_state_age_secs", json!(hook.age_secs));
    put("hook_reported", json!(hook.reported));
    put("hook_coverage", json!(hook.coverage.as_str()));
    put(
        "hook_coverage_source",
        json!(hook.coverage_source.map(|s| match s {
            crate::session::CoverageSource::ByName => "name",
            crate::session::CoverageSource::BySchema => "hook_schema",
        })),
    );
    put("hook_states_reportable", json!(hook.states_reportable()));
    put("hook_delivery", json!(hook.delivery().map(|d| d.as_str())));
    put(
        "hook_blocked_is_heuristic",
        json!(hook.blocked_is_heuristic()),
    );
    put(
        "hook_corroboration",
        json!(hook.corroboration.map(|c| c.as_str())),
    );
    put("hook_state_contradicted", json!(hook.contradicted));
    put("foreground_process", json!(hook.foreground_process));
    put("foreground_command", json!(hook.foreground_command));
    // Always a word, never null: `uncovered` and `unreported` are the two
    // silences spelled apart (`Assessment::state_word`). `state_source` stays
    // null for both, which is what tells them from an agent's own report.
    put("state", json!(hook.state_word()));
    put("state_source", json!(hook.state_source.map(|s| s.as_str())));
    // The parked mark, under the same name and type `thurbox-cli watch`
    // already publishes it — so a driver polling `get`/`list` and one reading
    // the stream learn the same fact from the same key. Without it the two
    // verbs describe a parked session exactly as they describe a running one.
    put("stopped", json!(hook.stopped));
    value
}

/// Read one session out of [`session_to_json`]'s shape, placing it on
/// `backend_type` — the observer's name for the host, never the host's own
/// (`local-tmux` there). Fields a host older than this one does not print are
/// simply empty; the id and the name are required.
pub fn session_from_json(value: &Value, backend_type: &str) -> Result<HostRow, String> {
    let string = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    let id: SessionId = string("id")
        .ok_or("session without an id")?
        .parse()
        .map_err(|e| format!("session id: {e}"))?;
    let name = string("name").ok_or_else(|| format!("session {id} without a name"))?;
    let worktrees = value
        .get("worktrees")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|w| {
                    Some(SharedWorktree {
                        repo_path: PathBuf::from(w.get("repo_path")?.as_str()?),
                        worktree_path: PathBuf::from(w.get("worktree_path")?.as_str()?),
                        branch: w.get("branch")?.as_str()?.to_string(),
                        // Absent from a peer running a build that predates the
                        // field. Those peers only ever created their worktrees,
                        // so "ours" is the accurate reading, not a guess.
                        created_by_thurbox: w
                            .get("created_by_thurbox")
                            .and_then(Value::as_bool)
                            .unwrap_or(true),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let additional_dirs = value
        .get("additional_dirs")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default();
    Ok(HostRow {
        session: SharedSession {
            id,
            name,
            agent: string("agent").unwrap_or_else(|| crate::session::DEFAULT_AGENT_NAME.into()),
            backend_id: string("backend_id").unwrap_or_default(),
            backend_type: backend_type.to_string(),
            agent_session_id: string("agent_session_id"),
            cwd: string("cwd").map(PathBuf::from),
            additional_dirs,
            worktrees,
            shell_backend_id: None,
            parent_session_id: string("parent_session_id").and_then(|s| s.parse().ok()),
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        },
        hook_state: string("hook_state"),
        base_branch: string("base_branch"),
    })
}

/// Every session in a `session list --json` answer, on `backend_type`.
pub fn parse_active(value: &Value, backend_type: &str) -> Vec<HostRow> {
    value
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| match session_from_json(row, backend_type) {
                    Ok(row) => Some(row),
                    Err(e) => {
                        tracing::warn!("skipping a session the host listed: {e}");
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every session in a `session list --deleted --json` answer.
pub fn parse_deleted(value: &Value) -> Vec<HostDeletedRow> {
    value
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(HostDeletedRow {
                        id: row.get("id")?.as_str()?.parse().ok()?,
                        force_deleted: row
                            .get("force_deleted")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Reconcile the local rows on `backend_type` to what the host listed.
pub fn apply(
    db: &Database,
    backend_type: &str,
    active: &[HostRow],
    deleted: &[HostDeletedRow],
) -> MirrorReport {
    let mut report = MirrorReport {
        host: backend_type.to_string(),
        ..MirrorReport::default()
    };
    let local_active: HashMap<SessionId, SharedSession> = db
        .list_active_sessions()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.backend_type == backend_type)
        .map(|s| (s.id, s))
        .collect();
    let local_deleted: HashMap<SessionId, bool> = db
        .list_deleted_sessions()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.backend_type == backend_type)
        .map(|s| (s.id, s.force_deleted))
        .collect();
    let hook_rows = db.load_hook_states().unwrap_or_default();
    let bases = db.load_base_branches().unwrap_or_default();

    for row in active {
        let id = row.session.id;
        if let Some(local) = local_active.get(&id) {
            let merged = merge(local, &row.session);
            if merged != *local {
                if let Err(e) = db.upsert_session(&merged) {
                    tracing::warn!("mirror: could not update {id}: {e}");
                    continue;
                }
                report.updated.push(id);
            }
        } else if local_deleted.contains_key(&id) {
            // The host restored it (or the delete here never reached the host);
            // a row the host lists as active is active.
            if let Err(e) = db
                .restore_session(id)
                .and_then(|()| db.upsert_session(&row.session))
            {
                tracing::warn!("mirror: could not restore {id}: {e}");
                continue;
            }
            report.restored.push(id);
        } else {
            if let Err(e) = db.upsert_session(&row.session) {
                tracing::warn!("mirror: could not adopt {id}: {e}");
                continue;
            }
            report.adopted.push(id);
        }
        // Status is the host's: its hooks wrote it. Only a *different* value is
        // written, so an acknowledged `done` is not re-reported as new, and a
        // host that says nothing leaves whatever the live channel set.
        if let Some(state) = row.hook_state.as_deref() {
            let local_state = hook_rows.get(&id).and_then(|r| r.state.as_deref());
            if local_state != Some(state) {
                if let Err(e) = db.set_hook_state(id, state) {
                    tracing::warn!("mirror: could not record status of {id}: {e}");
                }
            }
        }
        if let Some(base) = row.base_branch.as_deref() {
            if bases.get(&id).map(String::as_str) != Some(base) {
                if let Err(e) = db.set_session_base_branch(id, base) {
                    tracing::warn!("mirror: could not record base branch of {id}: {e}");
                }
            }
        }
    }

    for gone in deleted {
        let id = gone.id;
        if local_active.contains_key(&id) {
            if let Err(e) = db.soft_delete_session(id) {
                tracing::warn!("mirror: could not delete {id}: {e}");
                continue;
            }
            if gone.force_deleted {
                let _ = db.mark_session_force_deleted(id);
            }
            report.deleted.push(id);
        } else if let Some(false) = local_deleted.get(&id) {
            if gone.force_deleted {
                let _ = db.mark_session_force_deleted(id);
            }
        }
    }

    let host_knows: HashSet<SessionId> = active
        .iter()
        .map(|r| r.session.id)
        .chain(deleted.iter().map(|d| d.id))
        .collect();
    report.unknown_local = local_active
        .keys()
        .filter(|id| !host_knows.contains(id))
        .copied()
        .collect();
    report.unknown_local.sort_by_key(|id| id.to_string());
    report
}

/// The host's facts on top of what is the observer's own. The pane id is the
/// host's when it reports one — the two share that tmux server, so it is the
/// same pane — and the observer's (resolved by window name) otherwise.
fn merge(local: &SharedSession, host: &SharedSession) -> SharedSession {
    SharedSession {
        id: local.id,
        name: host.name.clone(),
        agent: host.agent.clone(),
        backend_id: if host.backend_id.is_empty() {
            local.backend_id.clone()
        } else {
            host.backend_id.clone()
        },
        backend_type: local.backend_type.clone(),
        agent_session_id: host.agent_session_id.clone(),
        cwd: host.cwd.clone(),
        additional_dirs: host.additional_dirs.clone(),
        worktrees: host.worktrees.clone(),
        shell_backend_id: local.shell_backend_id.clone(),
        parent_session_id: host.parent_session_id,
        display_order: local.display_order,
        tombstone: local.tombstone,
        tombstone_at: local.tombstone_at,
    }
}

/// One mirror pass for `host` through its usable CLI.
pub fn mirror_host(db: &Database, host: &HostDef, cli: &CliInfo) -> Result<MirrorReport, String> {
    let backend = host.backend_name();
    let active = host_cli::run(host, cli, &["session", "list"])?;
    let deleted = host_cli::run(host, cli, &["session", "list", "--deleted"])?;
    Ok(apply(
        db,
        &backend,
        &parse_active(&active, &backend),
        &parse_deleted(&deleted),
    ))
}

/// Register the local rows the host does not know (`unknown_local`) in the
/// host's database, so they become shared. Best-effort per row; returns the
/// ones the host accepted.
pub fn register_unknown(
    db: &Database,
    host: &HostDef,
    cli: &CliInfo,
    ids: &[SessionId],
) -> Vec<SessionId> {
    let hooks = db.load_hook_states().unwrap_or_default();
    let bases = db.load_base_branches().unwrap_or_default();
    let mut registered = Vec::new();
    for id in ids {
        let Ok(Some(row)) = db.get_session_by_id(*id) else {
            continue;
        };
        let body = session_to_json(
            &row,
            hooks.get(id).and_then(|r| r.state.as_deref()),
            bases.get(id).map(String::as_str),
        )
        .to_string();
        match host_cli::run(host, cli, &["session", "register", "--json-row", &body]) {
            Ok(_) => registered.push(*id),
            Err(e) => tracing::warn!("could not register '{}' on '{}': {e}", row.name, host.name),
        }
    }
    registered
}

/// Mirror one host (`only`) or every shareable one, optionally registering
/// the local rows a host does not know. A host that cannot be used lands in
/// its report's `error`; an unknown `only` is the one hard failure.
pub fn sync(db: &Database, only: Option<&str>, adopt: bool) -> Result<Vec<MirrorReport>, String> {
    let hosts = crate::agent::host_config::load_all();
    let targets: Vec<HostDef> = match only {
        Some(name) => vec![hosts.resolve(name).cloned().ok_or_else(|| {
            format!(
                "Unknown host '{name}'. Configured hosts: {}",
                if hosts.is_empty() {
                    "(none — add one in hosts.toml)".to_string()
                } else {
                    hosts.names().join(", ")
                }
            )
        })?],
        None => hosts
            .hosts
            .iter()
            .filter(|h| h.shareable())
            .cloned()
            .collect(),
    };
    let mut reports = Vec::new();
    for host in &targets {
        if only.is_some() {
            // A hand-run sync usually follows a fix; ask the host afresh.
            host_cli::forget(host);
        }
        let report = match host_cli::usable(host) {
            Usable::Yes(cli) => match mirror_host(db, host, &cli) {
                Ok(mut report) => {
                    if adopt && !report.unknown_local.is_empty() {
                        report.registered =
                            register_unknown(db, host, &cli, &report.unknown_local.clone());
                    }
                    report
                }
                Err(e) => MirrorReport {
                    host: host.backend_name(),
                    error: Some(e),
                    ..MirrorReport::default()
                },
            },
            Usable::No(reason) => MirrorReport {
                host: host.backend_name(),
                error: Some(reason),
                ..MirrorReport::default()
            },
        };
        reports.push(report);
    }
    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BACKEND: &str = "ssh:devbox";

    fn host_row(id: SessionId, name: &str) -> HostRow {
        session_from_json(
            &json!({
                "id": id.to_string(),
                "name": name,
                "agent": "codex",
                "backend_type": "local-tmux",
                "backend_id": "%7",
                "agent_session_id": "conv-1",
                "cwd": "/srv/repo",
                "additional_dirs": ["/srv/other"],
                "parent_session_id": null,
                "display_order": 3,
                "base_branch": "main",
                "hook_state": "blocked",
                "worktrees": [{
                    "repo_path": "/srv/repo",
                    "worktree_path": "/home/me/.local/share/thurbox/worktrees/repo/feat",
                    "branch": "feat/x",
                }],
            }),
            BACKEND,
        )
        .unwrap()
    }

    fn local_row(id: SessionId, name: &str) -> SharedSession {
        SharedSession {
            id,
            name: name.into(),
            agent: "codex".into(),
            backend_id: "%2".into(),
            backend_type: BACKEND.into(),
            agent_session_id: Some("conv-1".into()),
            cwd: Some(PathBuf::from("/srv/repo")),
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: Some("%9".into()),
            parent_session_id: None,
            display_order: Some(5),
            tombstone: false,
            tombstone_at: None,
        }
    }

    #[test]
    fn the_json_shape_round_trips() {
        let id = SessionId::default();
        let row = host_row(id, "foo");
        let again = session_from_json(
            &session_to_json(
                &row.session,
                row.hook_state.as_deref(),
                row.base_branch.as_deref(),
            ),
            BACKEND,
        )
        .unwrap();
        assert_eq!(again, row);
        // The observer's backend name, never the host's own.
        assert_eq!(row.session.backend_type, BACKEND);
        assert_eq!(row.session.display_order, None);
        assert_eq!(
            row.session.additional_dirs,
            vec![PathBuf::from("/srv/other")]
        );
    }

    #[test]
    fn a_borrowed_worktree_stays_borrowed_across_the_wire() {
        // The peer that tears a mirrored session down reads this flag off the
        // JSON, not off its own database. `created_by_thurbox` is absent-means-
        // true for older hosts, so a writer that stopped emitting the key would
        // turn every borrowed worktree back into one force-delete may
        // `git worktree remove --force` — taking the user's uncommitted work
        // with it — while `the_json_shape_round_trips` stayed green, since its
        // fixture never sets the flag false.
        let id = SessionId::default();
        let mut row = host_row(id, "borrowed");
        row.session.worktrees[0].created_by_thurbox = false;

        let again = session_from_json(
            &session_to_json(
                &row.session,
                row.hook_state.as_deref(),
                row.base_branch.as_deref(),
            ),
            BACKEND,
        )
        .unwrap();

        assert!(!again.session.worktrees[0].created_by_thurbox);
        assert_eq!(again, row);
    }

    #[test]
    fn an_older_host_that_prints_fewer_fields_still_parses() {
        let id = SessionId::default();
        let row = session_from_json(
            &json!({ "id": id.to_string(), "name": "old", "agent": "claude" }),
            BACKEND,
        )
        .unwrap();
        assert_eq!(row.session.backend_id, "");
        assert!(row.session.worktrees.is_empty());
        assert_eq!(row.hook_state, None);
        assert!(session_from_json(&json!({ "name": "no id" }), BACKEND).is_err());
    }

    #[test]
    fn a_host_row_is_adopted_with_its_id_facts_and_status() {
        let db = Database::open_in_memory().unwrap();
        let id = SessionId::default();
        let report = apply(&db, BACKEND, &[host_row(id, "foo")], &[]);
        assert_eq!(report.adopted, vec![id]);
        assert!(report.changed());
        let row = db.get_session_by_id(id).unwrap().unwrap();
        assert_eq!(row.name, "foo");
        assert_eq!(row.backend_type, BACKEND);
        assert_eq!(row.backend_id, "%7");
        assert_eq!(row.worktrees.len(), 1);
        assert_eq!(
            db.load_hook_state(id).unwrap().unwrap().state.as_deref(),
            Some("blocked")
        );
        assert_eq!(
            db.get_session_base_branch(id).unwrap().as_deref(),
            Some("main")
        );
    }

    #[test]
    fn a_second_pass_with_nothing_new_writes_nothing() {
        let db = Database::open_in_memory().unwrap();
        let id = SessionId::default();
        apply(&db, BACKEND, &[host_row(id, "foo")], &[]);
        let before = db.data_version().unwrap();
        let report = apply(&db, BACKEND, &[host_row(id, "foo")], &[]);
        assert!(!report.changed(), "{report:?}");
        assert_eq!(db.data_version().unwrap(), before);
    }

    #[test]
    fn host_facts_win_but_the_observers_own_fields_stay() {
        let db = Database::open_in_memory().unwrap();
        let id = SessionId::default();
        db.upsert_session(&local_row(id, "foo")).unwrap();
        let report = apply(&db, BACKEND, &[host_row(id, "renamed")], &[]);
        assert_eq!(report.updated, vec![id]);
        let row = db.get_session_by_id(id).unwrap().unwrap();
        assert_eq!(row.name, "renamed");
        assert_eq!(row.backend_id, "%7", "the host's pane on the shared server");
        assert_eq!(row.shell_backend_id.as_deref(), Some("%9"));
        assert_eq!(row.display_order, Some(5));
        assert_eq!(row.worktrees.len(), 1);
    }

    #[test]
    fn a_host_row_without_a_pane_keeps_the_observers_pane() {
        let db = Database::open_in_memory().unwrap();
        let id = SessionId::default();
        db.upsert_session(&local_row(id, "foo")).unwrap();
        let mut row = host_row(id, "foo");
        row.session.backend_id.clear();
        apply(&db, BACKEND, &[row], &[]);
        assert_eq!(db.get_session_by_id(id).unwrap().unwrap().backend_id, "%2");
    }

    #[test]
    fn a_host_deletion_soft_deletes_here_with_the_force_mark() {
        let db = Database::open_in_memory().unwrap();
        let id = SessionId::default();
        db.upsert_session(&local_row(id, "foo")).unwrap();
        let report = apply(
            &db,
            BACKEND,
            &[],
            &[HostDeletedRow {
                id,
                force_deleted: true,
            }],
        );
        assert_eq!(report.deleted, vec![id]);
        assert!(db.get_session_by_id(id).unwrap().is_none());
        let gone = db.get_deleted_session_by_id(id).unwrap().unwrap();
        assert!(gone.force_deleted);
        // Nothing to do twice.
        let again = apply(
            &db,
            BACKEND,
            &[],
            &[HostDeletedRow {
                id,
                force_deleted: true,
            }],
        );
        assert!(!again.changed());
    }

    #[test]
    fn a_deletion_of_a_session_never_held_here_is_ignored() {
        let db = Database::open_in_memory().unwrap();
        let report = apply(
            &db,
            BACKEND,
            &[],
            &[HostDeletedRow {
                id: SessionId::default(),
                force_deleted: false,
            }],
        );
        assert!(!report.changed());
        assert!(db.list_deleted_sessions().unwrap().is_empty());
    }

    #[test]
    fn a_host_restore_restores_here() {
        let db = Database::open_in_memory().unwrap();
        let id = SessionId::default();
        db.upsert_session(&local_row(id, "foo")).unwrap();
        db.soft_delete_session(id).unwrap();
        db.mark_session_force_deleted(id).unwrap();
        let report = apply(&db, BACKEND, &[host_row(id, "foo")], &[]);
        assert_eq!(report.restored, vec![id]);
        let row = db.get_session_by_id(id).unwrap().unwrap();
        assert_eq!(row.name, "foo");
        assert!(db.get_deleted_session_by_id(id).unwrap().is_none());
    }

    #[test]
    fn a_local_row_the_host_does_not_know_is_reported_not_touched() {
        let db = Database::open_in_memory().unwrap();
        let legacy = SessionId::default();
        db.upsert_session(&local_row(legacy, "legacy")).unwrap();
        let elsewhere = SessionId::default();
        let mut other = local_row(elsewhere, "local-one");
        other.backend_type = "local-tmux".into();
        db.upsert_session(&other).unwrap();
        let report = apply(&db, BACKEND, &[], &[]);
        assert_eq!(report.unknown_local, vec![legacy]);
        assert!(!report.changed());
        assert!(db.get_session_by_id(legacy).unwrap().is_some());
    }

    #[test]
    fn status_is_written_only_when_it_differs() {
        let db = Database::open_in_memory().unwrap();
        let id = SessionId::default();
        apply(&db, BACKEND, &[host_row(id, "foo")], &[]);
        let first = db.load_hook_state(id).unwrap().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        apply(&db, BACKEND, &[host_row(id, "foo")], &[]);
        let second = db.load_hook_state(id).unwrap().unwrap();
        assert_eq!(
            first.state_at, second.state_at,
            "an equal state is not re-stamped"
        );
        let mut row = host_row(id, "foo");
        row.hook_state = Some("done".into());
        apply(&db, BACKEND, &[row], &[]);
        assert_eq!(
            db.load_hook_state(id).unwrap().unwrap().state.as_deref(),
            Some("done")
        );
    }

    #[test]
    fn the_report_serialises_ids_as_strings() {
        let id = SessionId::default();
        let report = MirrorReport {
            host: BACKEND.into(),
            adopted: vec![id],
            ..MirrorReport::default()
        };
        let json = report.to_json();
        assert_eq!(json["host"], BACKEND);
        assert_eq!(json["adopted"][0], id.to_string());
        assert!(json["error"].is_null());
    }
}
