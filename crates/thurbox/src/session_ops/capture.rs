//! Reading a session's pane: the one implementation `thurbox-cli session
//! capture` calls, with the route decided here (#1468) as [`super::send`]
//! decides it for a send.

use serde_json::Value;

use crate::session::SharedSession;
use crate::storage::Database;

use super::OpError;

/// What [`capture`] read.
#[derive(Debug, Clone)]
pub enum Captured {
    /// Read from this machine: the rendered text, and the pane's live state
    /// read right after it, so the state describes the text.
    Local {
        output: String,
        state: crate::backend::PaneState,
    },
    /// The session's host read it with its own CLI; its whole answer.
    Host(Value),
}

/// Read the last `lines` of `session`'s agent pane, with its escapes when
/// `ansi`. A parked session has no pane and is refused; a session on a host
/// that takes delegation is read by the host's own CLI.
pub fn capture(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    session: &SharedSession,
    lines: u32,
    ansi: bool,
) -> Result<Captured, OpError> {
    super::send::refuse_if_parked(db, session)?;
    if let Some((host, cli)) = super::host_cli_for(session)? {
        let id = session.id.to_string();
        let lines = lines.to_string();
        let mut args = vec!["session", "capture", &id, "--lines", &lines];
        if ansi {
            args.push("--ansi");
        }
        return super::host_cli::run(&host, &cli, &args).map(Captured::Host);
    }
    let (backend, pane) = super::windows::require_agent_pane(backends, session)?;
    let output = backend
        .capture(&pane, lines, ansi)
        .map_err(|e| OpError::from(e).context(format!("capture: session '{}'", session.name)))?;
    // Read after the capture, so a pane that is simply not there fails as it
    // always has rather than reporting a screenful of nothing with null state.
    let state = backend.pane_state(&pane).unwrap_or_default();
    Ok(Captured::Local { output, state })
}
