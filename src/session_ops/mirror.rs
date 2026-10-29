//! Mirroring a shareable host's database into local rows.
//!
//! The host's `thurbox-cli session list --json` (and `--deleted`) is the
//! record; this module reconciles the local rows on that host's backend to it
//! — adopting what is new, updating what changed, deleting and restoring what
//! the host says was deleted and restored — and writes nothing when nothing
//! moved (past stamping, once, a row mirrored before the host's clock was
//! kept), so an idle mirror does not bump `data_version` for every peer. The
//! observer keeps what is its own: display order, the companion shell, and a
//! pane id the host does not report.
//!
//! One JSON shape serves both directions: [`session_to_json`] is what the CLI
//! prints and what `session register` reads back, so the mirror and the
//! adoption of a legacy row cannot disagree about a field.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

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
    /// Rows the host still lists as active that were deleted here first — the
    /// local tombstone stands, and the delete is pushed to the host. The
    /// symmetric counterpart of `unknown_local`/[`register_unknown`].
    pub tombstoned: Vec<SessionId>,
    /// Local lifecycle selections retained until the queued push is dispatched.
    pub tombstone_selections: Vec<(SessionId, u64)>,
    /// Transitive rows dropped from this database because
    /// [`Transitive::Hide`] is in force. Nothing reached the host: the
    /// session goes on at its owner.
    pub forgotten: Vec<SessionId>,
    /// Rows dropped from this database because `hosts.toml` no longer names
    /// the host (see [`forget_removed_hosts`]). Nothing reached the host.
    pub unconfigured: Vec<SessionId>,
    /// Why the host could not be mirrored, when it could not.
    pub error: Option<String>,
}

impl MirrorReport {
    pub fn changed(&self) -> bool {
        !(self.adopted.is_empty()
            && self.updated.is_empty()
            && self.deleted.is_empty()
            && self.restored.is_empty()
            && self.registered.is_empty()
            && self.tombstoned.is_empty()
            && self.forgotten.is_empty()
            && self.unconfigured.is_empty())
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
            "tombstoned": ids(&self.tombstoned),
            "forgotten": ids(&self.forgotten),
            "unconfigured": ids(&self.unconfigured),
            "error": self.error,
        })
    }
}

/// A session as the host lists it, already on the observer's backend name.
#[derive(Debug, Clone, PartialEq)]
pub struct HostRow {
    pub lifecycle_generation: Option<u64>,
    pub session: SharedSession,
    /// Absent from older peers; do not clear arguments they cannot report.
    pub agent_args: Option<Vec<String>>,
    pub hook_state: Option<String>,
    pub base_branch: Option<String>,
    /// When the host last wrote this row (millis since epoch), when it says.
    /// `None` from a host older than the field — every such row then loses to a
    /// local tombstone, which is the reading that stops a delete from being
    /// undone on the next pass.
    pub updated_at: Option<u64>,
    /// The host lists it under one of its own remote backends: its mirror of a
    /// further host's session (see [`reconcile_with`]). Such a row carries no
    /// pane, since the id the host reports is a pane on that further host's
    /// server, and on this backend's server it names some other agent.
    pub transitive: bool,
}

/// A deleted session as the host lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDeletedRow {
    pub lifecycle_generation: Option<u64>,
    pub purged: bool,
    pub id: SessionId,
    pub force_deleted: bool,
}

/// The one JSON shape of a session: what `session list`/`get` print, what a
/// peer's mirror reads, and what `session register` accepts.
///
/// `updated_at` is when this database last wrote the row. A peer reads it
/// against its own tombstones (see [`apply`]), which is the only ordering the
/// two sides share.
pub fn session_to_json(
    s: &SharedSession,
    hook_state: Option<&str>,
    base_branch: Option<&str>,
    updated_at: Option<u64>,
) -> Value {
    json!({
        "id": s.id.to_string(),
        "updated_at": updated_at,
        "name": s.name,
        // The session's address, in the wire format because the host's record
        // of it is the one that counts (ADR-24): a peer mirroring this row takes
        // the key with it, and `session register` gives it back.
        "key": s.key,
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
    updated_at: Option<u64>,
) -> Value {
    let mut value = session_to_json(s, hook.hook_state.as_deref(), base_branch, updated_at);
    let Some(obj) = value.as_object_mut() else {
        return value;
    };
    let mut put = |key: &str, v: Value| {
        obj.insert(key.to_string(), v);
    };
    // Which machine the row is on, by the `hosts.toml` name (`null` here):
    // what `key:<host>/<key>` is spelled with, so a row found by `list --key`
    // carries its own address. Not in the mirror shape — a host's own rows are
    // all `null` there, and the observer places them itself.
    put("host", json!(super::names::key_host(&s.backend_type)));
    put("hook_state_at", json!(hook.state_at));
    put("hook_state_age_secs", json!(hook.age_secs));
    put("hook_reported", json!(hook.reported));
    put("hook_coverage", json!(hook.coverage.as_str()));
    put(
        "hook_coverage_source",
        json!(hook.coverage_source.map(|s| s.as_str())),
    );
    put("hook_states_reportable", json!(hook.states_reportable()));
    put("hook_delivery", json!(hook.delivery().map(|d| d.as_str())));
    put(
        "hook_blocked_is_heuristic",
        json!(hook.blocked_is_heuristic()),
    );
    put(
        "hook_corroboration",
        json!(hook.corroboration.as_ref().map(|c| c.as_str())),
    );
    // Which agent the pane was found to be running, when it is not the one the
    // row was created with. A live observation, distinct from `agent` and from
    // `reports_as` — see `Assessment::detected_agent`.
    put("detected_agent", json!(hook.detected_agent()));
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
    // Why a `failed` session failed — null otherwise. A stable, documented
    // record (docs/ORCHESTRATION.md → Launch failures): `reason` (`setup` or
    // `exited`), `exit_code`, `output` (the pane's last lines), `pane` and
    // `at`.
    put("launch_failure", json!(hook.launch_failure));
    // Whether the session can take input now, for every agent alike — the
    // readiness contract (`Assessment::ready`, docs/ORCHESTRATION.md).
    put("ready", json!(hook.ready()));
    // Null unless a driver declared one: the agent the hook fields above were
    // resolved against, when that is not the agent the row was created with.
    // Without it a `--command` session's `hook_coverage: "full"` reads as a
    // claim about the shell it launched.
    put(
        "reports_as",
        json!((hook.agent != s.agent).then(|| hook.agent.clone())),
    );
    value
}

/// Read one session out of [`session_to_json`]'s shape, placing it on
/// `backend_type` — the observer's name for the host's machine, never the
/// host's own (`local-tmux` there). The multiplexer the listing recorded
/// travels with the row (`placed`). Fields a host older than this one does
/// not print are simply empty; the id and the name are required.
pub fn session_from_json(
    value: &Value,
    backend_type: &str,
) -> Result<HostRow, crate::session_ops::OpError> {
    let string = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    let backend_type = placed(backend_type, string("backend_type").as_deref());
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
    let agent_args = value
        .get("agent_args")
        .filter(|v| !v.is_null())
        .map(|v| {
            serde_json::from_value::<Vec<String>>(v.clone()).map_err(|e| format!("agent_args: {e}"))
        })
        .transpose()?;
    Ok(HostRow {
        agent_args,
        lifecycle_generation: value.get("lifecycle_generation").and_then(Value::as_u64),
        session: SharedSession {
            id,
            name,
            agent: string("agent").unwrap_or_else(|| crate::session::DEFAULT_AGENT_NAME.into()),
            backend_id: string("backend_id").unwrap_or_default(),
            backend_type,
            agent_session_id: string("agent_session_id"),
            cwd: string("cwd").map(PathBuf::from),
            additional_dirs,
            worktrees,
            shell_backend_id: None,
            parent_session_id: string("parent_session_id").and_then(|s| s.parse().ok()),
            display_order: None,
            tombstone: false,
            tombstone_at: None,
            // Absent from a host older than keys, which therefore has none.
            key: string("key"),
        },
        hook_state: string("hook_state"),
        base_branch: string("base_branch"),
        updated_at: value.get("updated_at").and_then(Value::as_u64),
        transitive: false,
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

/// A listed row's route here: the machine `place` names, served by the
/// multiplexer the listing recorded for the session — which is the same
/// server whichever side names it. `ssh:devbox` and a listed `local-rmux` make
/// `ssh:devbox:rmux`; a listed row written before routes named a multiplexer
/// stays unqualified, and keeps its legacy reading.
fn placed(place: &str, listed: Option<&str>) -> String {
    let Ok(mut route) = crate::session::Route::parse(place) else {
        return place.to_string();
    };
    route.mux = listed
        .and_then(|listed| crate::session::Route::parse(listed).ok())
        .and_then(|listed| listed.mux);
    route.format()
}

/// Every session in a `session list --deleted --json` answer.
pub fn parse_deleted(value: &Value) -> Vec<HostDeletedRow> {
    value
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(HostDeletedRow {
                        lifecycle_generation: row
                            .get("lifecycle_generation")
                            .and_then(Value::as_u64),
                        purged: row.get("purged_at").and_then(Value::as_u64).is_some(),
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
        .filter(|s| super::same_machine(&s.backend_type, backend_type))
        .map(|s| (s.id, s))
        .collect();
    // Tombstones with the instant each was taken, and the host's own
    // `updated_at` as last known before that (schema v45): the host's row
    // only outranks one when *the host's own clock* shows it was written
    // after that snapshot. `deleted_at` is this machine's clock and cannot be
    // compared to the host's `updated_at` directly — see `plan_row`.
    let local_deleted: Tombstones = db
        .list_session_tombstones(None)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| super::same_machine(&s.backend_type, backend_type))
        .map(|s| {
            (
                s.id,
                (
                    s.deleted_at,
                    s.force_deleted,
                    s.host_updated_at,
                    s.lifecycle_generation,
                    s.host_lifecycle_generation,
                ),
            )
        })
        .collect();
    let generations = db.load_lifecycle_generations().unwrap_or_default();
    let hook_rows = db.load_hook_states().unwrap_or_default();
    let bases = db.load_base_branches().unwrap_or_default();
    // `None` when it cannot be read: then no row is taken for unstamped, so a
    // failed read never turns into a write per row per pass.
    let clocked = db.mirrored_session_ids().ok();

    for row in active {
        let id = row.session.id;
        let Ok(Some(_lease)) =
            super::lock::lock(db, super::lock::Subject::Session(id), "mirror row")
        else {
            continue;
        };
        if newer_host_observation(db, id, backend_type, row.lifecycle_generation) {
            continue;
        }
        let selected = generations
            .get(&id)
            .copied()
            .or_else(|| local_deleted.get(&id).map(|r| r.3));
        if db.lifecycle_generation(id).ok().flatten() != selected {
            continue;
        }
        let plan = plan_row(&local_active, &local_deleted, row);
        // A row mirrored before the host's clock was kept (schema v45) and
        // unchanged since is never rewritten, so it is stamped here, once:
        // the clock is what marks a row as the host's to forget with it.
        if matches!(plan, RowPlan::Unchanged) && clocked.as_ref().is_some_and(|c| !c.contains(&id))
        {
            record_host_clock(db, id, row.updated_at);
        }
        if !apply_plan(db, row, plan, &mut report) {
            continue;
        }
        if let Some(generation) = row.lifecycle_generation {
            let _ = db.set_host_lifecycle_generation(id, generation);
        }
        record_host_facts(
            db,
            row,
            hook_rows.get(&id).and_then(|r| r.state.as_deref()),
            bases.get(&id).map(String::as_str),
        );
    }

    for gone in deleted {
        apply_host_delete(
            db,
            backend_type,
            gone,
            &local_active,
            &local_deleted,
            generations.get(&gone.id).copied(),
            &mut report,
        );
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
    report.tombstoned.sort_by_key(|id| id.to_string());
    report
}

/// Local tombstones on one host: when each was taken, whether it was forced,
/// and the host's own `updated_at` as last known before it.
type Tombstones = HashMap<SessionId, (u64, bool, Option<u64>, u64, Option<u64>)>;

/// What one row the host lists as active asks of this database.
enum RowPlan {
    /// Held here and already what the merge would write.
    Unchanged,
    /// Held here, and the host's facts change it.
    Update(Box<SharedSession>),
    /// Tombstoned here, and the host has not written the row since.
    KeepTombstone(u64),
    /// Tombstoned here, but the host wrote the row after its delete.
    Restore(u64),
    /// Not held here at all.
    Adopt,
}

fn plan_row(
    local_active: &HashMap<SessionId, SharedSession>,
    local_deleted: &Tombstones,
    row: &HostRow,
) -> RowPlan {
    let id = row.session.id;
    if let Some(local) = local_active.get(&id) {
        let mut merged = merge(local, &row.session);
        if row.transitive {
            merged.backend_id.clear();
        }
        return if merged == *local {
            RowPlan::Unchanged
        } else {
            RowPlan::Update(Box::new(merged))
        };
    }
    let Some(&(deleted_at, _, host_updated_at, generation, host_generation)) =
        local_deleted.get(&id)
    else {
        return RowPlan::Adopt;
    };
    // Lifecycle generations order host transitions without comparing clocks.
    // Older peers retain the existing same-clock updated_at fallback.
    let host_has_moved = match (host_generation, row.lifecycle_generation) {
        (Some(known), Some(current)) => current > known,
        _ => match host_updated_at {
            Some(known) => row.updated_at.is_some_and(|at| at > known),
            None => row.updated_at.is_some_and(|at| at > deleted_at),
        },
    };
    if host_has_moved {
        RowPlan::Restore(generation)
    } else {
        RowPlan::KeepTombstone(generation)
    }
}

/// Carry out one row's plan and note it in `report`. `false` when nothing more
/// is to be written for the row: its tombstone stands, or the write failed.
fn apply_plan(db: &Database, row: &HostRow, plan: RowPlan, report: &mut MirrorReport) -> bool {
    let id = row.session.id;
    let written = match plan {
        RowPlan::Unchanged => return true,
        RowPlan::KeepTombstone(generation) => {
            if let Some(observed) = row.lifecycle_generation {
                let _ = db.set_host_lifecycle_generation(id, observed);
            }
            report.tombstoned.push(id);
            report.tombstone_selections.push((id, generation));
            return false;
        }
        RowPlan::Update(merged) => {
            if let Err(e) = db.upsert_session(&merged) {
                tracing::warn!("mirror: could not update {id}: {e}");
                return false;
            }
            &mut report.updated
        }
        RowPlan::Restore(generation) => {
            if let Err(e) = db.restore_mirrored_session(&row.session, generation) {
                tracing::warn!("mirror: could not restore {id}: {e}");
                return false;
            }
            &mut report.restored
        }
        // Adopted, not spawned: the host launched it and this database is
        // taking it on, which is what a watcher's `registered` reason says.
        RowPlan::Adopt => {
            if let Err(e) =
                db.upsert_session_as(&row.session, crate::storage::EventReason::Registered)
            {
                tracing::warn!("mirror: could not adopt {id}: {e}");
                return false;
            }
            &mut report.adopted
        }
    };
    written.push(id);
    record_host_clock(db, id, row.updated_at);
    true
}

/// Write the host's status and base branch for a row where they differ from
/// what this database holds.
///
/// Status is the host's: its hooks wrote it. Only a *different* value is
/// written, so an acknowledged `done` is not re-reported as new, and a host
/// that says nothing leaves whatever the live channel set.
fn record_host_facts(
    db: &Database,
    row: &HostRow,
    held_state: Option<&str>,
    held_base: Option<&str>,
) {
    let id = row.session.id;
    if let Some(args) = &row.agent_args {
        if let Err(e) = db.set_agent_args(id, args) {
            tracing::warn!("mirror: could not record agent arguments of {id}: {e}");
        }
    }
    if let Some(state) = row.hook_state.as_deref().filter(|s| held_state != Some(*s)) {
        if let Err(e) = db.set_hook_state(id, state) {
            tracing::warn!("mirror: could not record status of {id}: {e}");
        }
    }
    if let Some(base) = row.base_branch.as_deref().filter(|b| held_base != Some(*b)) {
        if let Err(e) = db.set_session_base_branch(id, base) {
            tracing::warn!("mirror: could not record base branch of {id}: {e}");
        }
    }
}

fn newer_host_observation(
    db: &Database,
    id: SessionId,
    backend: &str,
    incoming: Option<u64>,
) -> bool {
    incoming.is_some_and(|generation| {
        db.session_backend(id)
            .ok()
            .flatten()
            .is_some_and(|held| super::same_machine(&held, backend))
            && db
                .host_lifecycle_generation(id)
                .ok()
                .flatten()
                .is_some_and(|known| known > generation)
    })
}

/// A row the host lists as deleted: delete it here when it is still active, or
/// carry the host's force mark onto a soft tombstone already taken here.
fn apply_host_delete(
    db: &Database,
    backend_type: &str,
    gone: &HostDeletedRow,
    local_active: &HashMap<SessionId, SharedSession>,
    local_deleted: &Tombstones,
    active_generation: Option<u64>,
    report: &mut MirrorReport,
) {
    let id = gone.id;
    let Ok(Some(_lease)) =
        super::lock::lock(db, super::lock::Subject::Session(id), "mirror deleted row")
    else {
        return;
    };
    if db
        .session_backend(id)
        .ok()
        .flatten()
        .is_some_and(|route| !super::same_machine(&route, backend_type))
    {
        return;
    }
    if newer_host_observation(db, id, backend_type, gone.lifecycle_generation) {
        return;
    }
    let selected = active_generation.or_else(|| local_deleted.get(&id).map(|r| r.3));
    if db.lifecycle_generation(id).ok().flatten() != selected {
        return;
    }
    if local_active.contains_key(&id) {
        let deleted = if gone.force_deleted {
            db.force_delete_session(id)
        } else {
            db.soft_delete_session(id)
        };
        if let Err(e) = deleted {
            tracing::warn!("mirror: could not delete {id}: {e}");
            return;
        }
        report.deleted.push(id);
    } else if let Some((_, false, _, _, _)) = local_deleted.get(&id) {
        if gone.force_deleted {
            let _ = db.mark_session_force_deleted(id);
        }
    }
    if let Some(generation) = gone.lifecycle_generation {
        let _ = db.set_host_lifecycle_generation(id, generation);
    }
    if gone.force_deleted {
        let _ = db.set_teardown_owed(id, false);
    }
    if gone.purged {
        let _ = db.insert_purge_tombstone(id, backend_type);
        if let Ok(Some(row)) = db.get_deleted_session_by_id(id) {
            let _ = db.purge_deleted_session(id, row.deleted_at);
        }
    }
}

/// Snapshot the host's self-reported `updated_at` on the row this pass just
/// wrote, best-effort. Only called after an actual local write, so an idle
/// pass still writes nothing (see the module doc).
fn record_host_clock(db: &Database, id: SessionId, host_updated_at: Option<u64>) {
    if let Some(at) = host_updated_at {
        if let Err(e) = db.set_host_updated_at(id, at) {
            tracing::warn!("mirror: could not record host clock for {id}: {e}");
        }
    }
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
        // The host's: the machine is this one's name for it either way, and the
        // multiplexer is what the host now records.
        backend_type: host.backend_type.clone(),
        agent_session_id: host.agent_session_id.clone(),
        cwd: host.cwd.clone(),
        additional_dirs: host.additional_dirs.clone(),
        worktrees: host.worktrees.clone(),
        shell_backend_id: local.shell_backend_id.clone(),
        parent_session_id: host.parent_session_id,
        display_order: local.display_order,
        tombstone: local.tombstone,
        tombstone_at: local.tombstone_at,
        // The host's: it enforced the key's uniqueness among its own rows, so a
        // duplicate this pass makes beside a stale one is gone next pass.
        key: host.key.clone(),
    }
}

/// One mirror pass for `host` through its usable CLI.
pub fn mirror_host(
    db: &Database,
    host: &HostDef,
    cli: &CliInfo,
) -> Result<MirrorReport, crate::session_ops::OpError> {
    let backend = host.backend_name();
    let (active, deleted) = host_lists(host, cli, &backend)?;
    let transitive = if crate::agent::settings_config::load_quiet()
        .remote
        .transitive_sessions
    {
        Transitive::Show
    } else {
        Transitive::Hide
    };
    let report = reconcile_with(db, &backend, &active, &deleted, transitive);
    push_tombstones(db, host, cli, &report.tombstone_selections);
    Ok(report)
}

/// A host's two lists as it last answered them, and the `session revision` it
/// reported just before.
struct Listed {
    revision: i64,
    active: Value,
    deleted: Value,
}

/// The last [`Listed`] per backend name. The revision is the generation: the
/// host's own triggers move it on every write that could change either list,
/// so an entry is current exactly while the host still reports its number.
fn listed() -> &'static Mutex<HashMap<String, Listed>> {
    static LISTED: OnceLock<Mutex<HashMap<String, Listed>>> = OnceLock::new();
    LISTED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Drop every cached listing, so a test's scripted host starts unseen.
#[cfg(test)]
pub(crate) fn forget_lists() {
    listed().lock().unwrap_or_else(|e| e.into_inner()).clear();
    deleted_histories()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// The host's `session list` and `session list --deleted` answers.
///
/// Both grow with the host's history and the mirror asks every few seconds,
/// so a host that understands `session revision` is asked that first, and the
/// lists it answered last time are reused while the number has not moved.
/// The revision is read *before* the lists: a write landing between the two
/// moves it past what is cached, so the next pass lists again rather than
/// keeping an answer older than its number. Reconciling still runs every
/// pass, against the reused lists, so a change on this side is handled exactly
/// as before.
fn host_lists(
    host: &HostDef,
    cli: &CliInfo,
    backend: &str,
) -> Result<(Value, Value), crate::session_ops::OpError> {
    let revision = cli
        .session_revision
        .then(|| host_cli::run(host, cli, &["session", "revision"]).ok())
        .flatten()
        .and_then(|answer| answer.get("revision")?.as_i64());
    if let Some(revision) = revision {
        let cache = listed().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = cache.get(backend).filter(|l| l.revision == revision) {
            return Ok((hit.active.clone(), hit.deleted.clone()));
        }
    }
    let active = host_cli::run(host, cli, &["session", "list"])?;
    let deleted = host_deleted(host, cli, backend, &active)?;
    if let Some(revision) = revision {
        listed().lock().unwrap_or_else(|e| e.into_inner()).insert(
            backend.to_string(),
            Listed {
                revision,
                active: active.clone(),
                deleted: deleted.clone(),
            },
        );
    }
    Ok((active, deleted))
}

/// A host's deleted rows as this side last assembled them, by id, and the
/// newest deletion or update among them — what the next pass asks `--since`.
struct DeletedHistory {
    rows: std::collections::BTreeMap<String, Value>,
    newest: u64,
    /// When the whole list was last read. A delta never says a row is gone
    /// from the host's history — a purge there leaves no trace in it — so the
    /// whole list is read again past [`DELETED_HISTORY_TTL`].
    read_whole_at: std::time::Instant,
}

impl Default for DeletedHistory {
    fn default() -> Self {
        Self {
            rows: Default::default(),
            newest: 0,
            read_whole_at: std::time::Instant::now(),
        }
    }
}

/// How long a [`DeletedHistory`] is extended by deltas before it is read whole
/// again: one full listing an hour rather than one a pass.
const DELETED_HISTORY_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

impl DeletedHistory {
    fn absorb(&mut self, listing: Value) {
        let Value::Array(rows) = listing else {
            return;
        };
        for row in rows {
            let Some(id) = row.get("id").and_then(Value::as_str).map(str::to_string) else {
                continue;
            };
            let at = row
                .get("deleted_at")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .max(row.get("updated_at").and_then(Value::as_u64).unwrap_or(0));
            self.newest = self.newest.max(at);
            self.rows.insert(id, row);
        }
    }

    /// Drop the rows the host lists as active again: restored since they were
    /// read, and a restore leaves no trace in a `--since` answer.
    fn forget_restored(&mut self, active: &Value) {
        for id in active
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|row| row.get("id")?.as_str())
        {
            self.rows.remove(id);
        }
    }
}

/// The [`DeletedHistory`] per backend name, for a host that answers `--since`.
fn deleted_histories() -> &'static Mutex<HashMap<String, DeletedHistory>> {
    static HISTORIES: OnceLock<Mutex<HashMap<String, DeletedHistory>>> = OnceLock::new();
    HISTORIES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The host's deleted rows: its whole history the first time, and after that
/// only what was deleted or changed since the newest timestamp already held.
///
/// The history grows for the life of the host's database — ~1600 rows came to
/// ~900 KB a listing — and every write there moves `session revision`, so the
/// whole of it crossed ssh on nearly every pass, which made it the answer most
/// often cut off by a dropped connection. The rows are still all needed: a
/// session held here that the host deleted long ago must read as deleted, not
/// as unknown, or [`register_unknown`] would hand it back to the host. So the
/// history is kept here and only the change is asked for. `--since` is
/// inclusive, so a delete stamped in the same millisecond as the newest one
/// held is not lost; one purged on the host stays held until the next whole
/// read.
fn host_deleted(
    host: &HostDef,
    cli: &CliInfo,
    backend: &str,
    active: &Value,
) -> Result<Value, crate::session_ops::OpError> {
    let list = |extra: &[&str]| {
        let mut args = vec!["session", "list", "--deleted"];
        if cli.purged_tombstones {
            args.push("--include-purged");
        }
        args.extend_from_slice(extra);
        host_cli::run(host, cli, &args)
    };
    if !cli.deleted_changes_since {
        return list(&[]);
    }
    // Taken out rather than held: the lock must not span an ssh round trip
    // that another host's pass is waiting on.
    let held = deleted_histories()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(backend)
        .filter(|history| history.read_whole_at.elapsed() < DELETED_HISTORY_TTL);
    let history = match held {
        Some(mut history) => {
            // The active answer precedes the deleted answer: remove only
            // cached tombstones before applying the newer deletes.
            history.forget_restored(active);
            let since = history.newest.to_string();
            history.absorb(list(&["--since", &since])?);
            history
        }
        None => {
            let mut history = DeletedHistory::default();
            history.absorb(list(&[])?);
            history
        }
    };
    let rows = Value::Array(history.rows.values().cloned().collect());
    deleted_histories()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(backend.to_string(), history);
    Ok(rows)
}

/// What a mirror pass does with a host's **transitive** rows: the sessions it
/// lists that are not its own but its mirror of a further host (`ssh:`/`wsl:`
/// in its own `backend_type`). `[remote] transitive_sessions` in
/// `settings.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transitive {
    /// Mirror them, once each: a session this database already holds on any
    /// other path — the owner reached directly, or our own — stays there.
    Show,
    /// Mirror only the host's own sessions, and forget the transitive rows an
    /// earlier pass took on.
    Hide,
}

/// The database half of a mirror pass: reconcile the rows on `backend` to the
/// host's `session list --json` (`active`) and `session list --deleted --json`
/// (`deleted`) answers, exactly as they came over the wire.
///
/// A session keeps its id on every hop, so the id is its identity and a row is
/// a *path* to it. One database holds one row per id, so a session reached two
/// ways would be relabelled by whichever pass ran last — and a host that
/// mirrors this instance back would relabel this instance's own sessions as
/// the host's. The rule that settles it: the host's own sessions are the
/// host's, while a transitive one is taken only when no other path here holds
/// its id already. A direct pass relabels a transitive row it finds on its own
/// (its [`apply`] adopts over it), so the direct path is the one that sticks,
/// and every action on the row — delete, restart, send — goes to its owner.
pub fn reconcile_with(
    db: &Database,
    backend: &str,
    active: &Value,
    deleted: &Value,
    transitive: Transitive,
) -> MirrorReport {
    let foreign = transitive_ids(active)
        .chain(transitive_ids(deleted))
        .collect::<HashSet<_>>();
    let forgotten = match transitive {
        Transitive::Show => Vec::new(),
        Transitive::Hide => forget_transitive(db, backend, &foreign),
    };
    let elsewhere: HashSet<SessionId> = match transitive {
        Transitive::Hide => HashSet::new(),
        Transitive::Show => {
            let active = db.list_active_sessions().unwrap_or_default();
            let deleted = db.list_deleted_sessions().unwrap_or_default();
            active
                .iter()
                .map(|s| (s.id, s.backend_type.as_str()))
                .chain(deleted.iter().map(|s| (s.id, s.backend_type.as_str())))
                .filter(|(_, on)| !super::same_machine(on, backend))
                .map(|(id, _)| id)
                .collect()
        }
    };
    let taken = |id: &SessionId| {
        !foreign.contains(id) || (transitive == Transitive::Show && !elsewhere.contains(id))
    };
    let active: Vec<HostRow> = parse_active(active, backend)
        .into_iter()
        .filter(|r| taken(&r.session.id))
        .map(|mut row| {
            if foreign.contains(&row.session.id) {
                as_transitive(&mut row);
            }
            row
        })
        .collect();
    let deleted: Vec<HostDeletedRow> = parse_deleted(deleted)
        .into_iter()
        .filter(|r| taken(&r.id))
        .collect();
    let mut report = apply(db, backend, &active, &deleted);
    report.forgotten = forgotten;
    report
}

/// Make a host's row safe to hold on its backend when the session is not the
/// host's own. Its pane id names a pane on the further host's server, so it is
/// dropped rather than attached to on this one. Its checkouts are the further
/// host's, so they are marked borrowed: a teardown that falls back to running
/// here (the host did not answer) then keeps them instead of removing
/// whatever sits at those paths on the host in between. Every action on the
/// row reaches the owner through the host's own CLI, as with any shared row.
fn as_transitive(row: &mut HostRow) {
    row.transitive = true;
    row.session.backend_id.clear();
    // Its multiplexer is the further host's, served by nothing on this one's.
    if let Ok(mut route) = crate::session::Route::parse(&row.session.backend_type) {
        route.mux = None;
        row.session.backend_type = route.format();
    }
    for worktree in &mut row.session.worktrees {
        worktree.created_by_thurbox = false;
    }
}

/// The ids of the rows in a `session list` answer that the host lists under
/// one of *its* remote backends — its mirror of a further host.
fn transitive_ids(listing: &Value) -> impl Iterator<Item = SessionId> + '_ {
    listing.as_array().into_iter().flatten().filter_map(|row| {
        let on = row.get("backend_type")?.as_str()?;
        crate::session::Route::is_remote_key(on)
            .then(|| row.get("id")?.as_str()?.parse().ok())
            .flatten()
    })
}

/// Every row this database holds, active and deleted, with the backend it is
/// on: what a forget sweeps, since a deleted row is as much a mirror's as a
/// live one.
fn held_rows(db: &Database) -> impl Iterator<Item = (SessionId, String)> {
    db.list_active_sessions()
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s.id, s.backend_type))
        .chain(
            db.list_deleted_sessions()
                .unwrap_or_default()
                .into_iter()
                .map(|s| (s.id, s.backend_type)),
        )
}

/// Drop this database's rows on `backend` that the host lists as transitive.
///
/// Forgotten rather than deleted: the session lives on at its owner, and a
/// tombstone here would be pushed back to the host as a delete of it (see
/// [`push_tombstones`]) the moment the setting was turned back on.
fn forget_transitive(db: &Database, backend: &str, foreign: &HashSet<SessionId>) -> Vec<SessionId> {
    let held = held_rows(db);
    let mut forgotten: Vec<SessionId> = held
        .filter(|(id, on)| super::same_machine(on, backend) && foreign.contains(id))
        .filter_map(|(id, _)| match db.forget_session(id) {
            Ok(()) => Some(id),
            Err(e) => {
                tracing::warn!("mirror: could not forget {id}: {e}");
                None
            }
        })
        .collect();
    forgotten.sort_by_key(|id| id.to_string());
    forgotten
}

/// Tell the host about the deletes it has not heard: the rows it still lists as
/// active that were deleted here first.
///
/// The counterpart of [`register_unknown`], and unconditional where that one is
/// opt-in — a delete the host never hears is a window left running there
/// forever, and the local row cannot express it any other way. Soft, so the
/// host's own undo window still applies. Best-effort per row: an unreachable
/// host is retried next pass, since the tombstone does not go away.
///
/// Forced as it was taken here: a force delete has already torn the worktrees
/// down on its side, and pushing it softly would leave the host holding a
/// restorable row for a session whose checkouts are gone.
fn push_tombstones(db: &Database, host: &HostDef, cli: &CliInfo, selections: &[(SessionId, u64)]) {
    for (id, selected_generation) in selections {
        let Ok(Some(_lease)) =
            super::lock::lock(db, super::lock::Subject::Session(*id), "mirror delete")
        else {
            continue;
        };
        let Ok(Some(row)) = db.get_deleted_session_by_id(*id) else {
            continue;
        };
        if row.lifecycle_generation != *selected_generation
            || !super::same_machine(&row.backend_type, &host.backend_name())
        {
            continue;
        }
        let Some(generation) = row
            .host_lifecycle_generation
            .filter(|_| cli.lifecycle_preconditions)
        else {
            continue;
        };
        let generation = generation.to_string();
        let forced = row.force_deleted;
        let id = id.to_string();
        let mut args = vec!["session", "delete", &id, "--if-generation", &generation];
        if forced {
            args.push("--force");
        } else if cli.soft_delete_choice {
            args.push("--soft");
        }
        if let Err(e) = host_cli::run(host, cli, &args) {
            tracing::warn!("could not push the delete of {id} to '{}': {e}", host.name);
        }
    }
}

/// Register the local rows the host does not know (`unknown_local`) in the
/// host's database, so they become shared. Returns accepted rows and explicit
/// failures, including peers that cannot preserve telemetry labels.
pub fn register_unknown(
    db: &Database,
    host: &HostDef,
    cli: &CliInfo,
    ids: &[SessionId],
) -> (Vec<SessionId>, Vec<String>) {
    let hooks = db.load_hook_states().unwrap_or_default();
    let bases = db.load_base_branches().unwrap_or_default();
    let updated = db.load_updated_at().unwrap_or_default();
    let mut registered = Vec::new();
    let mut errors = Vec::new();
    for id in ids {
        let Ok(Some(row)) = db.get_session_by_id(*id) else {
            continue;
        };
        let agent_args = match db.load_agent_args(*id) {
            Ok(args) => args,
            Err(e) => {
                tracing::warn!("could not read agent arguments of {id}: {e}");
                continue;
            }
        };
        if !agent_args.is_empty() && !cli.agent_args {
            tracing::warn!(
                "host '{}' has an older CLI that cannot register per-session agent arguments",
                host.name
            );
            continue;
        }
        let attrs = match db.load_telemetry_attrs(*id) {
            Ok(attrs) => attrs,
            Err(e) => {
                errors.push(format!("{}: read telemetry attributes: {e}", row.name));
                continue;
            }
        };
        if !attrs.is_empty() && !cli.telemetry_attrs {
            errors.push(format!("host '{}' has an older thurbox-cli that cannot preserve telemetry attributes when registering '{}'; update that CLI", host.name, row.name));
            continue;
        }
        let mut body = session_to_json(
            &row,
            hooks.get(id).and_then(|r| r.state.as_deref()),
            bases.get(id).map(String::as_str),
            updated.get(id).copied(),
        );
        body["agent_args"] = json!(agent_args);
        if !attrs.is_empty() {
            body["telemetry_attrs"] = json!(attrs);
        }
        let body = body.to_string();
        match host_cli::run(host, cli, &["session", "register", "--json-row", &body]) {
            Ok(_) => registered.push(*id),
            Err(e) => errors.push(format!(
                "could not register '{}' on '{}': {e}",
                row.name, host.name
            )),
        }
    }
    (registered, errors)
}

/// Mirror one host (`only`) or every shareable one, optionally registering
/// the local rows a host does not know. A host that cannot be used lands in
/// its report's `error`; an unknown `only` is the one hard failure.
pub fn sync(
    db: &Database,
    only: Option<&str>,
    adopt: bool,
) -> Result<Vec<MirrorReport>, crate::session_ops::OpError> {
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
                        let (registered, errors) =
                            register_unknown(db, host, &cli, &report.unknown_local);
                        report.registered = registered;
                        if !errors.is_empty() {
                            report.error = Some(errors.join("; "));
                        }
                    }
                    report
                }
                Err(e) => MirrorReport {
                    host: host.backend_name(),
                    error: Some(e.to_string()),
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
    reports.extend(forget_removed_hosts(db));
    Ok(reports)
}

/// Drop the rows a mirror wrote on every remote host the registry no longer
/// serves, one report per such host.
///
/// Only a mirror's rows, since the host holds their record: one that comes
/// back brings them back with its next pass, so a `hosts.toml` read without a
/// host (a half-written save, an entry commented out for a while) loses
/// nothing. A row this database is the only record of — a `share_sessions =
/// false` host's, or one from before sharing — stays whatever the file says.
///
/// Forgotten rather than deleted, for `forget_transitive`'s reason: a
/// tombstone is pushed to the host as a delete once it is configured again,
/// and the sessions live on there — taking a host out of `hosts.toml` stops
/// this machine watching it, it does not end its work. So nothing reaches the
/// host and no worktree is touched.
pub fn forget_removed_hosts(db: &Database) -> Vec<MirrorReport> {
    let served = match crate::agent::host_config::served_host_names() {
        Ok(served) => served,
        Err(e) => {
            tracing::warn!("rows of removed hosts kept — the host registry could not be read: {e}");
            return Vec::new();
        }
    };
    match db.mirrored_session_ids() {
        Ok(mirrored) => forget_unserved(db, &served, &mirrored),
        Err(e) => {
            tracing::warn!("rows of removed hosts kept — could not tell which a mirror wrote: {e}");
            Vec::new()
        }
    }
}

fn forget_unserved(
    db: &Database,
    served: &HashSet<String>,
    mirrored: &HashSet<SessionId>,
) -> Vec<MirrorReport> {
    let held = held_rows(db).filter(|(id, _)| mirrored.contains(id));
    let mut by_host: std::collections::BTreeMap<String, Vec<SessionId>> = Default::default();
    for (id, on) in held {
        let Ok(route) = crate::session::Route::parse(&on) else {
            continue;
        };
        let (Some(via), Some(host)) = (route.via(), route.host()) else {
            continue;
        };
        if served.contains(host) {
            continue;
        }
        match db.forget_session(id) {
            Ok(()) => by_host
                .entry(crate::session::Route::remote(via, host, None).format())
                .or_default()
                .push(id),
            Err(e) => tracing::warn!("could not forget {id} of removed host '{host}': {e}"),
        }
    }
    by_host
        .into_iter()
        .map(|(host, mut unconfigured)| {
            unconfigured.sort_by_key(|id| id.to_string());
            MirrorReport {
                host,
                unconfigured,
                ..MirrorReport::default()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_deleted_row_the_host_restored_is_dropped_and_a_newer_delete_kept() {
        let mut history = DeletedHistory::default();
        history.absorb(serde_json::json!([
            { "id": "a", "deleted_at": 10 },
            { "id": "b", "deleted_at": 20 },
        ]));
        history.absorb(serde_json::json!([{ "id": "c", "deleted_at": 30 }]));
        history.forget_restored(&serde_json::json!([{ "id": "b" }]));
        assert_eq!(history.newest, 30);
        assert_eq!(history.rows.keys().collect::<Vec<_>>(), ["a", "c"]);
    }

    const BACKEND: &str = "ssh:devbox";

    #[test]
    fn pushing_a_soft_tombstone_keeps_the_hosts_delete_undoable() {
        let db = Database::open_in_memory().unwrap();
        let id = SessionId::default();
        db.upsert_session(&local_row(id, "soft-probe")).unwrap();
        db.soft_delete_session(id).unwrap();
        let host = HostDef {
            name: "devbox".into(),
            ..Default::default()
        };
        db.set_host_lifecycle_generation(id, 7).unwrap();
        for understands_soft in [true, false] {
            let mut cli = host_cli::fake::cli();
            cli.soft_delete_choice = understands_soft;
            cli.lifecycle_preconditions = true;
            host_cli::fake::install_runner(Box::new(move |_, args| {
                assert_eq!(args.iter().any(|arg| arg == "--soft"), understands_soft);
                assert!(!args.iter().any(|arg| arg == "--force"));
                Ok(json!({"deleted": true}))
            }));
            push_tombstones(
                &db,
                &host,
                &cli,
                &[(id, db.lifecycle_generation(id).unwrap().unwrap())],
            );
            host_cli::fake::clear();
        }
    }

    #[test]
    fn an_old_host_reply_cannot_overwrite_a_newer_lifecycle_observation() {
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(root.path());
        let db = Database::open_in_memory().unwrap();
        let id = SessionId::default();
        db.upsert_session(&local_row(id, "fresh")).unwrap();
        db.set_host_lifecycle_generation(id, 9).unwrap();
        apply(
            &db,
            BACKEND,
            &[],
            &[HostDeletedRow {
                lifecycle_generation: Some(7),
                purged: false,
                id,
                force_deleted: false,
            }],
        );
        assert!(
            db.get_session_by_id(id).unwrap().is_some(),
            "an old host deletion overwrote a newer restore"
        );
        let mut stale = host_row(id, "obsolete");
        stale.lifecycle_generation = Some(7);
        apply(&db, BACKEND, &[stale], &[]);
        assert_eq!(db.get_session_by_id(id).unwrap().unwrap().name, "fresh");
    }

    #[test]
    fn a_queued_mirror_delete_cannot_outlive_a_local_restore() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let root = tempfile::tempdir().unwrap();
        let _guard = crate::paths::TestPathGuard::new(root.path());
        let db = Database::open_in_memory().unwrap();
        let id = SessionId::default();
        db.upsert_session(&local_row(id, "queued")).unwrap();
        db.set_host_lifecycle_generation(id, 7).unwrap();
        db.soft_delete_session(id).unwrap();
        let mut incoming = host_row(id, "queued");
        incoming.updated_at = Some(0);
        incoming.lifecycle_generation = Some(7);
        let report = apply(&db, BACKEND, &[incoming], &[]);
        assert_eq!(report.tombstoned, [id]);
        db.restore_session(id).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let mut cli = host_cli::fake::cli();
        cli.lifecycle_preconditions = true;
        host_cli::fake::install_runner(Box::new(move |_, _| {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"deleted": true}))
        }));
        push_tombstones(
            &db,
            &HostDef {
                name: "devbox".into(),
                ..Default::default()
            },
            &cli,
            &report.tombstone_selections,
        );
        host_cli::fake::clear();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "queued mirror delete reached the host after local restore"
        );
    }

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
                // Later than any tombstone a test here could take, so the
                // fixture reads as "the host has touched this since". The
                // tests about that ordering set their own value.
                "updated_at": u64::MAX,
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
            key: None,
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
                row.updated_at,
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
                row.updated_at,
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
                lifecycle_generation: None,
                purged: false,
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
                lifecycle_generation: None,
                purged: false,
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
                lifecycle_generation: None,
                purged: false,
                id: SessionId::default(),
                force_deleted: false,
            }],
        );
        assert!(!report.changed());
        assert!(db.list_deleted_sessions().unwrap().is_empty());
    }

    /// A tombstone taken here, with the host still listing the row as active
    /// and nothing written there since.
    fn tombstoned_locally(db: &Database, id: SessionId) -> HostRow {
        db.upsert_session(&local_row(id, "foo")).unwrap();
        db.soft_delete_session(id).unwrap();
        let deleted_at = db
            .get_deleted_session_by_id(id)
            .unwrap()
            .unwrap()
            .deleted_at;
        let mut row = host_row(id, "foo");
        row.updated_at = Some(deleted_at - 1);
        row
    }

    #[test]
    fn a_local_tombstone_beats_a_host_row_the_host_has_not_touched_since() {
        // The delete landed here while the host's CLI was unusable, so the host
        // still lists the row as active. Reading that as "restore me" undid the
        // delete on every pass for as long as the two disagreed.
        let db = Database::open_in_memory().unwrap();
        let id = SessionId::default();
        let row = tombstoned_locally(&db, id);

        let report = apply(&db, BACKEND, &[row], &[]);

        assert_eq!(report.tombstoned, vec![id]);
        assert!(report.restored.is_empty());
        assert!(db.get_session_by_id(id).unwrap().is_none());
        assert!(db.get_deleted_session_by_id(id).unwrap().is_some());
    }

    #[test]
    fn a_host_that_reports_no_timestamp_does_not_outrank_a_tombstone() {
        // A peer older than the field says nothing about when it wrote the row,
        // and "I cannot tell" must not be the answer that resurrects a session.
        let db = Database::open_in_memory().unwrap();
        let id = SessionId::default();
        let mut row = tombstoned_locally(&db, id);
        row.updated_at = None;

        let report = apply(&db, BACKEND, &[row], &[]);

        assert_eq!(report.tombstoned, vec![id]);
        assert!(db.get_session_by_id(id).unwrap().is_none());
    }

    #[test]
    fn a_host_row_written_after_the_tombstone_is_a_restore() {
        // The other half of the ordering: a peer that restored the session on
        // the host wrote its row after the delete here, and that outranks the
        // tombstone — otherwise the mirror would just re-delete it.
        let db = Database::open_in_memory().unwrap();
        let id = SessionId::default();
        let mut row = tombstoned_locally(&db, id);
        row.updated_at = row.updated_at.map(|at| at + 2);

        let report = apply(&db, BACKEND, &[row], &[]);

        assert_eq!(report.restored, vec![id]);
        assert!(report.tombstoned.is_empty());
        assert!(db.get_session_by_id(id).unwrap().is_some());
    }

    #[test]
    fn a_host_restore_survives_a_slower_host_clock() {
        // The host's `updated_at` and this machine's `deleted_at` are two
        // different clocks. Comparing them directly means a host whose clock
        // merely runs behind this machine's can write a genuine restore whose
        // `updated_at` still reads below `deleted_at` — and a comparison that
        // trusted that ordering would push the delete right back, destroying
        // the very session the host's user just restored. Ordering the
        // restore against the host's own last-known reading instead (schema
        // v45) is immune to the offset between the two clocks.
        let db = Database::open_in_memory().unwrap();
        let id = SessionId::default();

        let mut adopted = host_row(id, "foo");
        adopted.updated_at = Some(100);
        apply(&db, BACKEND, &[adopted], &[]);
        assert!(db.get_session_by_id(id).unwrap().is_some());

        db.soft_delete_session(id).unwrap();
        let deleted_at = db
            .get_deleted_session_by_id(id)
            .unwrap()
            .unwrap()
            .deleted_at;

        // The host's clock is far behind this one: its new reading for the
        // restore is only just past its own last-known value, nowhere near
        // `deleted_at` (a real wall-clock timestamp).
        let mut restored = host_row(id, "foo");
        restored.updated_at = Some(101);
        assert!(restored.updated_at.unwrap() < deleted_at);

        let report = apply(&db, BACKEND, &[restored], &[]);

        assert_eq!(report.restored, vec![id]);
        assert!(report.tombstoned.is_empty());
        assert!(db.get_session_by_id(id).unwrap().is_some());
        assert!(db.get_deleted_session_by_id(id).unwrap().is_none());
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
    fn a_base_branch_is_written_only_when_the_host_names_a_different_one() {
        let db = Database::open_in_memory().unwrap();
        let id = SessionId::default();
        let base = |db: &Database| db.load_base_branches().unwrap().get(&id).cloned();
        apply(&db, BACKEND, &[host_row(id, "foo")], &[]);
        assert_eq!(base(&db).as_deref(), Some("main"));
        let mut moved = host_row(id, "foo");
        moved.base_branch = Some("develop".into());
        apply(&db, BACKEND, &[moved], &[]);
        assert_eq!(base(&db).as_deref(), Some("develop"));
        let mut silent = host_row(id, "foo");
        silent.base_branch = None;
        apply(&db, BACKEND, &[silent], &[]);
        assert_eq!(
            base(&db).as_deref(),
            Some("develop"),
            "a host that names no base leaves the recorded one alone"
        );
    }

    #[test]
    fn a_host_force_delete_marks_a_row_already_soft_deleted_here() {
        let db = Database::open_in_memory().unwrap();
        let id = SessionId::default();
        db.upsert_session(&local_row(id, "gone")).unwrap();
        db.soft_delete_session(id).unwrap();
        let report = apply(
            &db,
            BACKEND,
            &[],
            &[HostDeletedRow {
                lifecycle_generation: None,
                purged: false,
                id,
                force_deleted: true,
            }],
        );
        assert!(report.deleted.is_empty(), "{report:?}");
        let row = db
            .list_deleted_sessions()
            .unwrap()
            .into_iter()
            .find(|row| row.id == id)
            .expect("still a tombstone");
        assert!(row.force_deleted);
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
