//! A session's windows, through the backend its route names.
//!
//! Every lifecycle verb — create, restart, stop, restore, delete, reap, owed
//! teardown, rename — and every pane verb — send, key, capture, a pane's state
//! — asks the injected registry for the one backend that
//! serves a row and acts through the contract. Nothing here names an adapter,
//! so a backend that registers for a route is driven exactly as tmux is, and a
//! route nothing is registered for is refused rather than driven somewhere
//! else.

/// The backend that drives `backend_type`'s windows: exactly the one
/// registered for the route it names, once an unqualified route is settled the
/// way it has always been read — or why there is none.
///
/// Never another route's. A key naming no route, a host `hosts.toml` no
/// longer describes, and a route no backend here serves are all refusals: the
/// windows are somewhere this process cannot reach, and acting on this
/// machine's server instead would act on the wrong windows.
pub(crate) fn backend_for<'r>(
    backends: &'r crate::backend::BackendRegistry,
    backend_type: &str,
) -> Result<&'r std::sync::Arc<dyn crate::backend::SessionBackend>, String> {
    let route = crate::session::Route::parse(backend_type).map_err(|e| e.to_string())?;
    let (hosts, _warnings) = crate::agent::host_config::cached_registry();
    if let Some(name) = route.host() {
        if hosts.host_of(&route).is_none() {
            return Err(format!("host '{name}' is not in hosts.toml"));
        }
    }
    let served = hosts.qualify(&route);
    backends.get(&served).ok_or_else(|| {
        format!("no backend here serves {served}, so nothing drives '{backend_type}'")
    })
}

/// A backend, and a pane on it.
pub type BackendPane<'r> = (
    &'r std::sync::Arc<dyn crate::backend::SessionBackend>,
    String,
);

/// The backend `session`'s route names, and the pane its agent is in there —
/// `Ok(None)` when that backend positively holds no window of the row's.
///
/// One vocabulary for every pane verb: locate by the row, then act on the pane.
/// A listing that cannot say which of several namesakes is the row's is an
/// error, never a name to send to: text typed into a namesake is text another
/// session's agent reads as its own.
pub fn agent_pane<'r>(
    backends: &'r crate::backend::BackendRegistry,
    session: &crate::sync::SharedSession,
) -> Result<Option<BackendPane<'r>>, String> {
    let backend = backend_for(backends, &session.backend_type)?;
    let id = session.id.to_string();
    let owner = crate::backend::Owner::new(&id, &session.name).remembering(
        &session.backend_id,
        session.shell_backend_id.as_deref().unwrap_or(""),
    );
    let placed = backend
        .locate(owner)
        .map_err(|e| format!("could not list the windows on {}: {e:#}", backend.name()))?;
    match placed.agent {
        crate::backend::Located::At(pane) => Ok(Some((backend, pane))),
        crate::backend::Located::Absent => Ok(None),
        crate::backend::Located::Unknown => Err(format!(
            "several windows on {} answer to '{}' and none is stamped as this session's, \
             so there is no telling which one is its own",
            backend.name(),
            session.name
        )),
    }
}

/// [`agent_pane`], where a session with no window is an error: what every
/// verb that must reach the agent wants.
pub fn require_agent_pane<'r>(
    backends: &'r crate::backend::BackendRegistry,
    session: &crate::sync::SharedSession,
) -> Result<
    (
        &'r std::sync::Arc<dyn crate::backend::SessionBackend>,
        String,
    ),
    String,
> {
    agent_pane(backends, session)?
        .ok_or_else(|| format!("session '{}' has no window of its own here", session.name))
}

/// Kill the windows `owner` has on `backend` — its agent and its companion
/// shell — and nothing a namesake owns. Returns whether the agent's came down.
///
/// `Err` is the backend not answering, which leaves the windows standing: a
/// caller that owes the teardown writes it down rather than reading this as
/// done.
pub(crate) fn kill_owned(
    backend: &dyn crate::backend::SessionBackend,
    owner: crate::backend::Owner<'_>,
) -> anyhow::Result<bool> {
    let placed = backend.locate(owner)?;
    // Absent or Unknown: already gone, or never this session's to begin with.
    let killed = match &placed.agent {
        crate::backend::Located::At(agent) => backend.kill(agent).map(|()| true)?,
        crate::backend::Located::Absent | crate::backend::Located::Unknown => false,
    };
    if let crate::backend::Located::At(shell) = &placed.shell {
        backend.kill(shell)?;
    }
    Ok(killed)
}

/// Whether `owner`'s agent is running on `backend` — the question every
/// relaunch asks. `Unknown` counts as running: "I cannot tell" must not be the
/// answer that launches a second agent.
pub(crate) fn agent_running(
    backend: &dyn crate::backend::SessionBackend,
    owner: crate::backend::Owner<'_>,
) -> anyhow::Result<bool> {
    Ok(!live_agent(backend, owner)?.is_absent())
}

/// Where `owner`'s *running* agent window is on `backend`, by the listing's
/// own rule: a stamp first, a lone unstamped namesake second, never a window
/// stamped for another row.
pub(crate) fn live_agent(
    backend: &dyn crate::backend::SessionBackend,
    owner: crate::backend::Owner<'_>,
) -> anyhow::Result<crate::backend::Located> {
    let index = crate::backend::identity::WindowIndex::from_listing(backend.discover()?);
    Ok(index.live_agent_window(owner.session_id, owner.name))
}

/// Claim the running agent window a row names, on the backend its route
/// names, and return its pane: `session register`'s half of the lifecycle.
///
/// The window must already be running and be unambiguously this row's —
/// register records a session, it never launches one — and it is stamped for
/// the row so that from here on it is found by id rather than by a name a
/// later namesake could take.
pub fn claim_running_window(
    backends: &crate::backend::BackendRegistry,
    backend_type: &str,
    session_id: &str,
    name: &str,
) -> Result<String, String> {
    let backend = backend_for(backends, backend_type)?;
    let pane = live_agent(
        backend.as_ref(),
        crate::backend::Owner::new(session_id, name),
    )
    .map_err(|e| format!("could not list windows: {e:#}"))?
    .pane()
    .ok_or_else(|| {
        format!(
            "no live window for '{name}' on {} that is unambiguously its own; \
                 register records a running session, it does not launch one",
            backend.name()
        )
    })?;
    if let Err(e) = backend.stamp_window(&pane, session_id, crate::backend::WindowRole::Agent) {
        tracing::debug!("could not stamp the window registered for '{name}': {e:#}");
    }
    Ok(pane)
}
