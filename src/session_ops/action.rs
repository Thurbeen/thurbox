//! Reading what a task or an automation does: the one parser `thurbox-cli task
//! create`, `thurbox-cli automation create` and the interface's automation
//! editor share (#1468), so the same target is accepted or refused with the
//! same words whichever of them names it.

use crate::session::{AutomationAction, ExtraRepo, SessionId};
use crate::storage::Database;

use super::OpError;

/// The targets a caller named, each as it was given. At most one of
/// `session` (send), `repo` (spawn) and `command` (exec) may be set; the
/// spawn's own fields only mean something next to `repo`.
#[derive(Debug, Clone, Default)]
pub struct Target {
    pub session: Option<String>,
    pub repo: Option<String>,
    /// The worktree branch a spawn creates or reuses.
    pub branch: Option<String>,
    pub base: Option<String>,
    pub agent: Option<String>,
    pub extra_repos: Vec<ExtraRepo>,
    pub command: Option<String>,
}

/// The action `target` names, or `None` when it names none — a plain todo for
/// a task, a refusal for an automation, which is the caller's to say.
pub fn parse(db: &Database, target: Target) -> Result<Option<AutomationAction>, OpError> {
    let Target {
        session,
        repo,
        branch,
        base,
        agent,
        extra_repos,
        command,
    } = target;
    let named = [session.is_some(), repo.is_some(), command.is_some()];
    if named.iter().filter(|named| **named).count() > 1 {
        return Err(OpError::Usage(
            "name only one target: a session, a repository or a command".into(),
        ));
    }
    if repo.is_none() && !extra_repos.is_empty() {
        return Err(OpError::Usage(
            "extra repositories only apply to a spawn, which names a repository".into(),
        ));
    }
    if let Some(session) = session {
        let session_id: SessionId = session
            .parse()
            .map_err(|_| OpError::Usage(format!("not a session id: {session}")))?;
        db.get_session_by_id(session_id)
            .map_err(|e| OpError::Other(format!("get session: {e}")))?
            .ok_or_else(|| OpError::NotFound(format!("no session {session} to send to")))?;
        return Ok(Some(AutomationAction::Send { session_id }));
    }
    if let Some(repo) = repo {
        return Ok(Some(AutomationAction::Spawn {
            repo_path: repo.into(),
            worktree_branch: branch,
            base_branch: base,
            agent,
            extra_repos,
        }));
    }
    match command {
        Some(command) if command.trim().is_empty() => {
            Err(OpError::Usage("the command must not be empty".into()))
        }
        Some(command) => Ok(Some(AutomationAction::Exec { command })),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_target_at_most() {
        let db = Database::open_in_memory().unwrap();
        let both = Target {
            repo: Some("/r".into()),
            command: Some("true".into()),
            ..Default::default()
        };
        assert!(parse(&db, both).unwrap_err().contains("only one target"));
        let stray = Target {
            command: Some("true".into()),
            extra_repos: vec![ExtraRepo {
                repo_path: "/x".into(),
                worktree: false,
                base_branch: None,
            }],
            ..Default::default()
        };
        assert!(parse(&db, stray)
            .unwrap_err()
            .contains("extra repositories"));
        assert_eq!(parse(&db, Target::default()), Ok(None));
    }

    #[test]
    fn a_send_needs_a_session_that_exists() {
        let db = Database::open_in_memory().unwrap();
        let unknown = SessionId::default().to_string();
        let target = |session: &str| Target {
            session: Some(session.into()),
            ..Default::default()
        };
        assert_eq!(
            parse(&db, target("nope")),
            Err(OpError::Usage("not a session id: nope".into()))
        );
        assert_eq!(
            parse(&db, target(&unknown)),
            Err(OpError::NotFound(format!(
                "no session {unknown} to send to"
            )))
        );
    }
}
