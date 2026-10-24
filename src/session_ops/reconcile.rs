//! One recovery path for resources made before a session commits.

use crate::storage::{intents::CreateIntent, Database};

pub(crate) struct Creating<'a> {
    db: &'a Database,
    backends: &'a crate::backend::BackendRegistry,
    pub intent: CreateIntent,
    armed: bool,
    lease_token: u64,
}

impl<'a> Creating<'a> {
    pub fn begin(
        db: &'a Database,
        backends: &'a crate::backend::BackendRegistry,
        intent: CreateIntent,
    ) -> Result<Self, super::OpError> {
        db.begin_create_intent(&intent)
            .map_err(|e| format!("record create intent: {e}"))?;
        Ok(Self {
            db,
            backends,
            lease_token: intent.expires_at,
            intent,
            armed: true,
        })
    }

    pub fn check(&self) -> Result<(), super::OpError> {
        self.db
            .check_create_fence(self.intent.session.id, self.lease_token)
            .map_err(|_| super::OpError::Gone("creation lease expired or was recovered".into()))
    }

    pub fn record_pane(&mut self, pane: &str) -> Result<(), super::OpError> {
        self.intent.session.backend_id = pane.into();
        self.db
            .update_owned_create_intent(&self.intent, self.lease_token)
            .map_err(|e| format!("record intent pane: {e}"))?;
        Ok(())
    }

    /// A pre-launch refusal hands the prepared directories back for inspection.
    pub fn preserve_prepared(&mut self) -> Result<(), super::OpError> {
        self.db
            .commit_prepared_intent(self.intent.session.id, self.lease_token)
            .map_err(|e| format!("record retained preparation: {e}"))?;
        self.armed = false;
        Ok(())
    }

    pub fn committed(&mut self) {
        self.armed = false;
    }
}

impl Drop for Creating<'_> {
    fn drop(&mut self) {
        if self.armed
            && self
                .db
                .check_create_fence(self.intent.session.id, self.lease_token)
                .is_ok()
        {
            finish(self.db, self.backends, &mut self.intent);
        }
    }
}

fn finish(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    intent: &mut CreateIntent,
) -> bool {
    match rollback(db, backends, intent) {
        Ok(()) => db.clear_create_intent(intent.session.id).is_ok(),
        Err(e) => {
            tracing::warn!(session = %intent.session.id, "creation cleanup is still owed: {e}");
            intent.expires_at = crate::sync::current_time_millis().saturating_add(60_000);
            let _ = db.update_create_intent(intent);
            false
        }
    }
}

fn rollback(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    intent: &CreateIntent,
) -> Result<(), super::OpError> {
    let row = &intent.session;
    if db
        .get_session_by_id(row.id)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Ok(());
    }
    let backend = super::windows::backend_for(backends, &row.backend_type)?;
    let stamp = row.id.to_string();
    let windows = backend.discover()?;
    let mut panes: Vec<String> = windows
        .iter()
        .filter(|w| {
            (w.session == stamp && !intent.before.contains(&w.backend_id))
                || (w.session.is_empty()
                    && w.name
                        == crate::backend::identity::window_name_for(
                            crate::backend::WindowRole::Agent,
                            &row.name,
                        )
                    && !intent.before.contains(&w.backend_id))
        })
        .map(|w| w.backend_id.clone())
        .collect();
    if !row.backend_id.is_empty()
        && !intent.before.contains(&row.backend_id)
        && !panes.contains(&row.backend_id)
    {
        // A remembered id is only a hint: the backend's ownership gate still arbitrates it.
        panes.push(row.backend_id.clone());
    }
    if panes.len() > 1 {
        return Err("creation window ownership is ambiguous".into());
    }
    if let Some(pane) = panes.first() {
        if db
            .list_active_sessions()
            .map_err(|e| e.to_string())?
            .iter()
            .any(|active| {
                super::server_key(&active.backend_type) == super::server_key(&row.backend_type)
                    && (active.backend_id == *pane
                        || active.shell_backend_id.as_ref() == Some(pane))
            })
        {
            return Err("creation pane is now claimed by an active session".into());
        }
        let owner = crate::backend::Owner::new(&stamp, &row.name).remembering(pane, "");
        let located = backend.locate(owner)?;
        if located
            .agent
            .pane()
            .as_ref()
            .is_some_and(|found| found.as_str() == pane)
        {
            backend.kill(pane)?;
        }
        if backend
            .discover()?
            .iter()
            .any(|window| window.backend_id == *pane)
        {
            return Err("creation window teardown could not be confirmed".into());
        }
    }
    let host = super::resolve_host(&row.backend_type).ok_or("creation host is unknown")?;
    let active = db.list_active_sessions().map_err(|e| e.to_string())?;
    for wt in &row.worktrees {
        if active
            .iter()
            .filter(|owner| super::same_machine(&owner.backend_type, &row.backend_type))
            .any(|owner| {
                owner.worktrees.iter().any(|other| {
                    super::lock::worktree_key(&owner.backend_type, &other.worktree_path)
                        == super::lock::worktree_key(&row.backend_type, &wt.worktree_path)
                })
            })
        {
            return Err("worktree is now used by an active session".into());
        }
        let registered = crate::git::list_worktrees_on(host.as_ref(), &wt.repo_path)
            .map_err(|e| e.to_string())?;
        if registered.iter().any(|w| {
            super::lock::worktree_key(&row.backend_type, &w.path)
                == super::lock::worktree_key(&row.backend_type, &wt.worktree_path)
        }) {
            crate::git::remove_worktree_on(host.as_ref(), &wt.repo_path, &wt.worktree_path)
                .map_err(|e| e.to_string())?;
        }
    }
    if intent.workspace_created {
        remove_workspace(db, row)?;
    }
    Ok(())
}

pub fn creating(db: &Database, backends: &crate::backend::BackendRegistry) -> usize {
    let Ok(intents) = db.expired_create_intents(crate::sync::current_time_millis()) else {
        return 0;
    };
    let mut finished = 0;
    let mut silent = std::collections::HashSet::new();
    for mut intent in intents {
        if crate::session::Route::is_remote_key(&intent.session.backend_type) {
            if silent.contains(&intent.session.backend_type) {
                continue;
            }
            if super::delete::listed_windows_on(db, backends, &intent.session.backend_type)
                .is_none()
            {
                silent.insert(intent.session.backend_type.clone());
                continue;
            }
        }
        let Ok(Some(_session)) = super::lock::lock(
            db,
            super::lock::Subject::Session(intent.session.id),
            "recover creation",
        ) else {
            continue;
        };
        let Ok(Some(current)) = db.create_intent(intent.session.id) else {
            continue;
        };
        if current.expires_at > crate::sync::current_time_millis() {
            continue;
        }
        intent = current;
        let Ok(_workspace) = super::lock::workspace(
            db,
            &intent.session.backend_type,
            intent.session.agent_session_id.as_deref(),
            "recover workspace",
        ) else {
            continue;
        };
        let Ok(_held) = super::lock::worktrees(
            db,
            &intent.session.backend_type,
            intent
                .session
                .worktrees
                .iter()
                .map(|wt| wt.worktree_path.clone()),
            "recover worktrees",
        ) else {
            continue;
        };
        if finish(db, backends, &mut intent) {
            finished += 1;
        }
    }
    finished
}

/// The caller holds the workspace lease; check claims again after slow teardown.
pub(super) fn remove_workspace(
    db: &Database,
    session: &crate::sync::SharedSession,
) -> Result<(), super::OpError> {
    if crate::session::Route::is_remote_key(&session.backend_type) {
        return Ok(());
    }
    let Some(id) = session.agent_session_id.as_deref() else {
        return Ok(());
    };
    workspace_unclaimed(db, session.id, &session.backend_type, id)?;
    crate::workspace::remove_workspace(id).map_err(|e| e.to_string().into())
}

pub(super) fn workspace_unclaimed(
    db: &Database,
    session: crate::session::SessionId,
    backend_type: &str,
    id: &str,
) -> Result<(), super::OpError> {
    let namespace = crate::paths::sanitize_workspace_segment(id);
    if db
        .list_active_sessions()
        .map_err(|e| e.to_string())?
        .iter()
        .any(|owner| {
            owner.id != session
                && super::same_machine(&owner.backend_type, backend_type)
                && owner
                    .agent_session_id
                    .as_deref()
                    .is_some_and(|id| crate::paths::sanitize_workspace_segment(id) == namespace)
        })
    {
        return Err(super::OpError::Conflict(
            "workspace is now used by an active session".into(),
        ));
    }
    Ok(())
}

pub struct Report {
    pub reaped: Vec<String>,
    pub teardowns: Vec<String>,
    pub lost: usize,
}

/// The interface worker and the headless heartbeat run this same sweep.
pub fn tick(db: &Database, backends: &crate::backend::BackendRegistry) -> Report {
    let teardowns = super::retry_owed_remote_teardowns(db, backends);
    let reaped = super::reap_overdue_soft_deletes(db, backends);
    let lost = super::lost::LostSweep::persistent(db, backends);
    Report {
        reaped,
        teardowns,
        lost,
    }
}
