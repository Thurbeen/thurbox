//! Creating a session, or answering with the one already there: the one
//! implementation the interface's `create` command and `thurbox-cli session
//! create` both call (#1468), so the same request is refused, named, adopted or
//! spawned the same way whichever of them makes it.
//!
//! The spawn itself is [`super::spawn`]'s. What lives here is everything a
//! creation decides around it: the name an unnamed request takes, the checks a
//! local path must pass, a retry under a caller-minted id, the
//! [`OnExisting`] answer with its name lock and its rollback, the built-in
//! hooks a first spawn needs, and the `reports_as` declaration written once the
//! session exists.

use crate::session::SessionId;
use crate::storage::Database;
use crate::sync::SharedSession;

use super::spawn::{ProgressFn, SpawnRequest, SpawnResult};
use super::OpError;

/// What a creation does when the session it describes is already there.
///
/// Every answer is decided before anything is spawned, so a refusal leaves no
/// window, worktree or row behind. It is a check rather than a lock: two
/// simultaneous creates can still both pass it, which is inherent to a spawn
/// that must make a multiplexer window before it has a row to be unique in.
/// What it does remove is the caller's own list-then-create window.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum OnExisting {
    /// Create another session with the same name. The two are then
    /// addressable only by id, since a name matching several sessions is
    /// refused rather than guessed.
    #[default]
    Allow,
    /// Answer with the existing session instead of creating one.
    Adopt,
    /// Tear the existing session down first (as a force delete would), then
    /// create. Its worktree goes with it.
    Replace,
    /// Refuse, naming the session in the way.
    Fail,
}

impl OnExisting {
    /// The word `session create --on-existing` takes for it.
    pub fn as_str(self) -> &'static str {
        match self {
            OnExisting::Allow => "allow",
            OnExisting::Adopt => "adopt",
            OnExisting::Replace => "replace",
            OnExisting::Fail => "fail",
        }
    }
}

/// What [`create_or_adopt`] did.
#[derive(Debug)]
pub enum Created {
    /// Spawned — here, or by the session's host. A host may answer with a
    /// session it already had (`result.created == false`); `result.found` then
    /// says what it holds. `reports_as` is the declaration written here, `None`
    /// for such a found session, which keeps the host's.
    Spawned {
        result: Box<SpawnResult>,
        reports_as: Option<String>,
    },
    /// An active session this machine already had: the one under the
    /// request's id (a retry), or the one `adopt` matched.
    Existing(Box<SharedSession>),
}

/// The name an unnamed request gets.
///
/// Order: the **worktree directory's** own name when one is being opened, then
/// the branch, then the repository directory. The worktree directory leads
/// because that name is the one a person chose — an agent cutting a worktree
/// from an issue writes `.worktrees/dynamic-tooltips` while the branch it puts
/// there carries a disambiguating suffix, and naming the session after the
/// branch would put that suffix in the session list.
fn default_name(req: &SpawnRequest) -> String {
    let file_name = |path: &std::path::Path| {
        path.file_name()
            .map(|name| name.to_string_lossy().to_string())
    };
    req.existing_worktree
        .as_deref()
        .and_then(file_name)
        .or_else(|| req.worktree_branch.clone())
        .or_else(|| file_name(&req.repo_path))
        .unwrap_or_else(|| "session".to_string())
}

/// Refuse a request whose paths cannot work, before anything is resolved.
///
/// Only a local target is checked — `local` as the resolved backend says, so
/// an empty host is this machine. Statting a *remote* path on this machine
/// refuses a perfectly good repository whenever the two filesystems disagree,
/// which for a WSL distro or an ssh host is always; the spawn reports the
/// truth there.
fn check_paths(req: &SpawnRequest, local: bool) -> Result<(), OpError> {
    if local && !req.repo_path.is_dir() {
        return Err(OpError::Other(format!(
            "not a directory: {}",
            req.repo_path.display()
        )));
    }
    // A worktree to open names the branch checked out there; half the pair
    // would record a session whose branch is blank.
    if req.existing_worktree.is_some() && req.worktree_branch.is_none() {
        return Err(OpError::Usage(
            "opening a worktree needs the branch checked out in it".into(),
        ));
    }
    // An opened worktree becomes the session's cwd, so a path that is not
    // there yields a pane that cannot start rather than a stated error.
    if let Some(worktree) = req.existing_worktree.as_deref().filter(|_| local) {
        if !worktree.is_dir() {
            return Err(OpError::Other(format!(
                "not a worktree: {}",
                worktree.display()
            )));
        }
    }
    Ok(())
}

/// Refuse a `reports_as` agent thurbox ships no hooks for.
///
/// The declaration exists for the coverage it unlocks, so one that unlocks
/// nothing is a typo rather than a preference — and a silent one, since the
/// only symptom is the coverage the caller was trying to fix staying `none`.
pub fn check_reports_as(
    registry: &crate::session::AgentRegistry,
    agent: &str,
) -> Result<(), OpError> {
    if crate::session::coverage_for(registry, agent).is_some() {
        return Ok(());
    }
    Err(OpError::Other(format!(
        "thurbox ships no status hooks for agent '{agent}', so declaring it would change \
         nothing. Covered agents: {}. A custom agent asserts a family with `hook_schema` in \
         agents.toml.",
        crate::session::AGENT_HOOK_COVERAGE
            .iter()
            .map(|c| c.agent)
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

/// The backend a creation lands on, and whether the host's own CLI carries it
/// out. The host is read off the resolved backend, as the spawn reads it, so
/// every spelling of a host (`box`, `ssh:box`, an empty one for this machine)
/// reaches the same answer.
fn route(
    backends: &crate::backend::BackendRegistry,
    host: Option<&str>,
    multiplexer: Option<&str>,
) -> Result<(String, bool), OpError> {
    let backend = super::spawn::backend_type_for_choice(backends, host, multiplexer)?;
    let delegated = super::resolve_host(&backend).flatten().is_some_and(|host| {
        matches!(
            super::host_cli::usable(&host),
            super::host_cli::Usable::Yes(_)
        )
    });
    Ok((backend, delegated))
}

/// The active session a retry under `session_id` already made, for a creation
/// this machine carries out.
///
/// The id is the caller's, so a session under it is the one an earlier attempt
/// made — whose answer the caller never saw. It is answered as it is, before
/// anything only a new session needs: an `OnExisting` rule, a parent deleted
/// since, a declaration. A caller that resolves such inputs itself asks this
/// first ([`create_or_adopt`] asks it again, which is cheap). An id a deleted
/// session holds is refused, before `replace` could tear anything down for a
/// creation that cannot happen.
pub fn retried(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    host: Option<&str>,
    multiplexer: Option<&str>,
    session_id: Option<SessionId>,
) -> Result<Option<SharedSession>, OpError> {
    if session_id.is_none() {
        return Ok(None);
    }
    let (_, delegated) = route(backends, host, multiplexer)?;
    retried_on(db, delegated, session_id)
}

fn retried_on(
    db: &Database,
    delegated: bool,
    session_id: Option<SessionId>,
) -> Result<Option<SharedSession>, OpError> {
    let Some(id) = session_id.filter(|_| !delegated) else {
        return Ok(None);
    };
    if let Some(existing) = db
        .get_session_by_id(id)
        .map_err(|e| format!("get_session_by_id: {e}"))?
    {
        return Ok(Some(existing));
    }
    if db
        .get_deleted_session_by_id(id)
        .map_err(|e| format!("get_deleted_session_by_id: {e}"))?
        .is_some()
    {
        return Err(OpError::Conflict(format!(
            "session id {id} is already taken by a deleted session"
        )));
    }
    Ok(None)
}

/// Create the session `req` describes, or answer with the one already there.
///
/// A creation the host's own CLI carries out is the host's to arbitrate
/// (ADR-24): its rows are the record, and this machine's mirror of them can be
/// a pass behind. So the id retry, the name lock and [`OnExisting`] are applied
/// here only for a creation this machine carries out; a delegated one forwards
/// them in the request, and the host applies them as it applies its own.
pub fn create_or_adopt(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    mut req: SpawnRequest,
    progress: Option<ProgressFn<'_>>,
) -> Result<Created, OpError> {
    if req.name.is_empty() {
        req.name = default_name(&req);
    }
    let (backend, delegated) = route(backends, req.host.as_deref(), req.multiplexer.as_deref())?;
    if let Some(found) = retried_on(db, delegated, req.session_id)? {
        return Ok(Created::Existing(Box::new(found)));
    }
    // Checked before anything is spawned: a typo here is silent otherwise,
    // and the session it would have described is already running by the time
    // the caller could notice.
    if let Some(declared) = &req.reports_as {
        check_reports_as(&crate::agent::agent_config::load_or_seed(), declared)?;
    }
    check_paths(&req, super::resolve_host(&backend) == Some(None))?;
    super::spawn::validate_request(db, &req)?;
    // `fail`, `adopt` and `replace` each promise the caller the only session
    // of that name, so the name is locked from the lookup to the row — the
    // lock self-heal and restore take. `allow` accepts a namesake, so it waits
    // on nobody. A key is locked by the spawn itself, and a keyed lookup never
    // asks about the name.
    let name_held = match (&req.key, req.on_existing) {
        _ if delegated => None,
        (None, OnExisting::Fail | OnExisting::Adopt | OnExisting::Replace) => Some(
            super::names::hold(db, &req.name, &backend, "create")?.ok_or_else(|| {
                format!(
                    "a session named '{}' is being created on {backend} right now; try again \
                     once that has finished",
                    req.name
                )
            })?,
        ),
        _ => None,
    };
    let existing = match delegated {
        true => Existing::None,
        false => resolve_existing(
            db,
            backends,
            match &req.key {
                Some(key) => Match::Key(key),
                None => Match::Name(&req.name),
            },
            req.on_existing,
            &backend,
            req.reports_as.as_deref(),
        )?,
    };
    if let Existing::Adopted(found) = existing {
        return Ok(Created::Existing(found));
    }
    let declared = req.reports_as.clone();
    // A machine driven by `thurbox-cli` alone reaches its first spawn before
    // the interface's boot or the heartbeat's tick has wired the built-in
    // extensions; the request already validated, so a refused one never
    // changes the agent config.
    super::deliver::ensure_hooks_wired(db, backends);
    let result =
        match super::spawn::spawn_session_headless_with_progress(db, backends, req, progress) {
            Ok(result) => result,
            // `replace` tore the old session down first, so a spawn that fails
            // here would otherwise leave the caller with neither session. Put back
            // what can be put back — with the name released, since the restore
            // takes the same lock.
            Err(e) => {
                drop(name_held);
                return Err(rollback_replace(db, backends, &existing, e));
            }
        };
    // Written once the session exists, so a failed write is reported as
    // exactly that and never rolls a `replace` back. A session the host
    // answered as already there keeps the declaration the host holds for it;
    // this machine's mirror copies that one.
    let reports_as = match &result.found {
        Some(found) => {
            if let Err(e) = db.set_reports_as(result.session_id, found.reports_as.as_deref()) {
                tracing::warn!("'{}': mirroring reports-as failed: {e}", result.name);
            }
            None
        }
        None => declared,
    };
    if let Some(declared) = &reports_as {
        if let Err(e) = db.set_reports_as(result.session_id, Some(declared)) {
            return Err(OpError::Other(format!(
                "session '{}' ({}) was created, but declaring it as '{declared}' failed: \
                 set_reports_as: {e}. Retry with `thurbox-cli session reports-as {} {declared}`",
                result.name, result.session_id, result.session_id
            )));
        }
    }
    Ok(Created::Spawned {
        result: Box::new(result),
        reports_as,
    })
}

/// What [`resolve_existing`] looks for: the session with the request's key on
/// the target host, or — for a request with none — the legacy name match.
#[derive(Clone, Copy)]
enum Match<'a> {
    Key(&'a str),
    Name(&'a str),
}

/// What [`resolve_existing`] decided, and what the creation still owes it.
#[derive(Debug)]
enum Existing {
    /// Nothing in the way — spawn.
    None,
    /// `adopt` found it; the creation must not run.
    Adopted(Box<SharedSession>),
    /// `replace` tore this session down. A spawn that now fails has to put it
    /// back, or the caller is left with neither.
    Replaced(SessionId),
}

/// Apply the request's [`OnExisting`] answer before anything is spawned.
///
/// The match is scoped to the backend the new session lands on. The name
/// namespace is *not* per-machine: a database mirroring a shareable host
/// (ADR-24) holds that host's rows beside its own, and two machines may
/// legitimately each have a session called `build`.
///
/// Ambiguity blocks `adopt` and `replace` but not `allow`: adopting one of two
/// same-named sessions, or destroying one of them, is a guess about which was
/// meant.
///
/// With a key the question is the key's, on the target **host** (#1408): the
/// session with that key, whatever it is called now — which is what lets
/// `adopt` survive a rename. Without one, a keyed session the name match finds
/// is adopted with its key, replaced (key and all), or refused, like any other.
fn resolve_existing(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    matching: Match<'_>,
    mode: OnExisting,
    backend: &str,
    reports_as: Option<&str>,
) -> Result<Existing, OpError> {
    if mode == OnExisting::Allow {
        return Ok(Existing::None);
    }
    let (found, what, label, noun) = match matching {
        Match::Key(key) => (
            super::names::live_key_holders(db, key, backend)?,
            format!("a session with key '{key}'"),
            key,
            "key",
        ),
        Match::Name(name) => (
            super::names::live_namesakes(db, name, backend)?,
            format!("a session named '{name}'"),
            name,
            "name",
        ),
    };
    match (mode, found.as_slice()) {
        (OnExisting::Allow, _) | (_, []) => Ok(Existing::None),
        (OnExisting::Fail, _) => Err(OpError::Conflict(format!(
            "{what} is already active on {backend} ({}). Use `--on-existing adopt` to take it \
             as it is, `--on-existing replace` to tear it down first, or pick another {noun}",
            found
                .iter()
                .map(|s| s.id.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        ))),
        (OnExisting::Adopt, [one]) => {
            if let Some(declared) = reports_as {
                db.set_reports_as(one.id, Some(declared))
                    .map_err(|e| format!("set_reports_as: {e}"))?;
            }
            Ok(Existing::Adopted(Box::new(one.clone())))
        }
        (OnExisting::Replace, [one]) => {
            super::delete::delete_session_headless(db, backends, one.id, true)?;
            Ok(Existing::Replaced(one.id))
        }
        (OnExisting::Adopt | OnExisting::Replace, many) => Err(OpError::Ambiguous(format!(
            "'{label}' matches {} active sessions on {backend}, so there is no single one to \
             {}. Address them by id, or pick another name:\n{}",
            many.len(),
            mode.as_str(),
            many.iter()
                .map(|s| format!("  {}  {}", s.id, s.agent))
                .collect::<Vec<_>>()
                .join("\n")
        ))),
    }
}

/// Put back what `replace` tore down when the replacement could not be spawned.
///
/// `replace` is a force delete followed by a create, and it cannot be the other
/// way round: the new session wants the branch and the checkout the old one is
/// holding. The undo is the same best-effort restore the interface runs, which
/// brings back the row, the branch and the agent; uncommitted work went with
/// the force delete and does not come back, and the message says so.
fn rollback_replace(
    db: &Database,
    backends: &crate::backend::BackendRegistry,
    existing: &Existing,
    spawn_error: OpError,
) -> OpError {
    let Existing::Replaced(id) = existing else {
        return spawn_error;
    };
    // The rollback adds to the sentence; the kind stays the spawn's.
    let message = match super::restore_session_headless(db, backends, *id, true) {
        Ok(report) => format!(
            "{spawn_error} — the replacement could not be spawned, so '{}' ({id}) was restored \
             best-effort: committed branch state is back, uncommitted work went with the \
             force delete",
            report.name
        ),
        Err(e) => format!(
            "{spawn_error} — the replacement could not be spawned and the session it replaced \
             ({id}) could not be restored either ({e}). `thurbox-cli session restore {id} \
             --best-effort` is the retry"
        ),
    };
    OpError::from_kind(spawn_error.kind(), message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn make_test_session(name: &str) -> SharedSession {
        SharedSession {
            id: SessionId::default(),
            name: name.into(),
            agent: "claude".into(),
            backend_id: String::new(),
            backend_type: "local-tmux".into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
            key: None,
        }
    }

    /// A failed create reports the kind its spawn failed with, so a host's
    /// conflict still exits 5 and an undetermined call still says so.
    #[test]
    fn a_failed_spawn_keeps_its_kind_through_the_rollback() {
        let backends = crate::backend::registry::inert();
        for error in [
            OpError::Conflict("key taken on the host".into()),
            OpError::Undetermined("host went quiet".into()),
        ] {
            let kind = error.kind();
            let reported = rollback_replace(&db(), &backends, &Existing::None, error);
            assert_eq!(reported.kind(), kind, "{reported}");
        }
    }

    /// The name namespace spans machines (ADR-24 mirrors a shareable host's
    /// rows into this database), so `--on-existing` has to ask about the
    /// backend the creation will land on. Matching across all of them let a
    /// local `replace` force-delete a session on another host, `fail` refuse a
    /// local create because of a remote namesake, and `adopt` hand back an id
    /// whose pane is on a different machine.
    #[test]
    fn on_existing_never_sees_a_namesake_on_another_host() {
        let db = db();
        let mut remote = make_test_session("build");
        remote.backend_type = "ssh:devbox".into();
        db.upsert_session(&remote).unwrap();

        for mode in [OnExisting::Fail, OnExisting::Adopt, OnExisting::Replace] {
            assert!(
                matches!(
                    resolve_existing(
                        &db,
                        &crate::backend::registry::inert(),
                        Match::Name("build"),
                        mode,
                        "local-tmux",
                        None
                    ),
                    Ok(Existing::None)
                ),
                "{mode:?} must not act on a row belonging to another host"
            );
        }
        // The row is untouched — nothing was replaced on the other machine.
        assert!(db.get_session_by_id(remote.id).unwrap().is_some());

        // A create *for that host* does see it, on the same terms.
        assert!(resolve_existing(
            &db,
            &crate::backend::registry::inert(),
            Match::Name("build"),
            OnExisting::Fail,
            "ssh:devbox",
            None
        )
        .is_err());
    }

    /// Ambiguity here is the same failure the reference resolver reports, so it
    /// earns the same exit code: a driver can act on "none" and only an
    /// operator can settle "several".
    #[test]
    fn an_ambiguous_name_refuses_with_the_ambiguity_exit_code() {
        let db = db();
        db.upsert_session(&make_test_session("twin")).unwrap();
        db.upsert_session(&make_test_session("twin")).unwrap();
        let err = resolve_existing(
            &db,
            &crate::backend::registry::inert(),
            Match::Name("twin"),
            OnExisting::Adopt,
            "local-tmux",
            None,
        )
        .unwrap_err();
        assert!(matches!(err, OpError::Ambiguous(_)), "{err:?}");
        assert!(err.contains("matches 2"), "got {err}");
    }

    fn keyed_test_session(name: &str, key: &str) -> SharedSession {
        let mut s = make_test_session(name);
        s.key = Some(key.into());
        s
    }

    fn answered(existing: Existing) -> SharedSession {
        match existing {
            Existing::Adopted(found) => *found,
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    /// `--on-existing` with `--key`: "existing" is the session with that key
    /// on the target host, whatever it is called now — which is what lets
    /// `adopt` survive a rename. A namesake with another key, or none, is not
    /// it, and neither is the same key on another host.
    #[test]
    fn on_existing_with_a_key_matches_the_key_and_not_the_name() {
        let db = db();
        let backends = crate::backend::registry::inert();
        let mine = keyed_test_session("renamed-by-the-user", "fleet.lead");
        db.upsert_session(&mine).unwrap();
        db.upsert_session(&make_test_session("worker")).unwrap();
        db.upsert_session(&keyed_test_session("worker", "fleet.other"))
            .unwrap();
        let mut remote = keyed_test_session("lead", "fleet.remote");
        remote.backend_type = "ssh:devbox".into();
        db.upsert_session(&remote).unwrap();

        let adopted = answered(
            resolve_existing(
                &db,
                &backends,
                Match::Key("fleet.lead"),
                OnExisting::Adopt,
                "local-tmux",
                None,
            )
            .unwrap(),
        );
        assert_eq!(adopted.id, mine.id);
        assert_eq!(adopted.key.as_deref(), Some("fleet.lead"));

        let err = resolve_existing(
            &db,
            &backends,
            Match::Key("fleet.lead"),
            OnExisting::Fail,
            "local-tmux",
            None,
        )
        .unwrap_err();
        assert!(err.contains("key 'fleet.lead'"), "{err}");

        // The name `worker` is taken twice over, by a keyless session and by
        // one with another key; neither is the session this key names.
        for mode in [OnExisting::Fail, OnExisting::Adopt, OnExisting::Replace] {
            assert!(matches!(
                resolve_existing(
                    &db,
                    &backends,
                    Match::Key("fleet.worker"),
                    mode,
                    "local-tmux",
                    None
                ),
                Ok(Existing::None)
            ));
            assert!(
                matches!(
                    resolve_existing(
                        &db,
                        &backends,
                        Match::Key("fleet.remote"),
                        mode,
                        "local-tmux",
                        None
                    ),
                    Ok(Existing::None)
                ),
                "{mode:?}: a key is per host"
            );
        }
    }

    /// Without `--key`, the legacy name match is unchanged, keyed sessions
    /// included: `adopt` hands one back with its key showing (so the caller
    /// sees whose it is), `fail` counts it as taken, and `replace` tears it
    /// down, key and all.
    #[test]
    fn on_existing_without_a_key_still_matches_names_keyed_or_not() {
        let db = db();
        let backends = crate::backend::registry::inert();
        let theirs = keyed_test_session("worker", "fleet.worker");
        db.upsert_session(&theirs).unwrap();

        let adopted = answered(
            resolve_existing(
                &db,
                &backends,
                Match::Name("worker"),
                OnExisting::Adopt,
                "local-tmux",
                None,
            )
            .unwrap(),
        );
        assert_eq!(adopted.id, theirs.id);
        assert_eq!(
            adopted.key.as_deref(),
            Some("fleet.worker"),
            "the key stays and shows"
        );
        assert_eq!(
            db.get_session_by_id(theirs.id).unwrap().unwrap().key,
            theirs.key
        );

        assert!(resolve_existing(
            &db,
            &backends,
            Match::Name("worker"),
            OnExisting::Fail,
            "local-tmux",
            None
        )
        .is_err());

        let replaced = resolve_existing(
            &db,
            &backends,
            Match::Name("worker"),
            OnExisting::Replace,
            "local-tmux",
            None,
        )
        .unwrap();
        assert!(matches!(replaced, Existing::Replaced(id) if id == theirs.id));
        assert!(db.find_sessions_by_key("fleet.worker").unwrap().is_empty());
    }

    fn request(repo: &str) -> SpawnRequest {
        SpawnRequest {
            repo_path: repo.into(),
            ..Default::default()
        }
    }

    #[test]
    fn an_opened_worktree_names_the_session_before_its_branch() {
        let mut req = request("/r/thurbox");
        req.worktree_branch = Some("feat/tooltips-1530".into());
        req.existing_worktree = Some("/r/thurbox/.worktrees/tooltips".into());
        assert_eq!(default_name(&req), "tooltips");
        req.existing_worktree = None;
        assert_eq!(default_name(&req), "feat/tooltips-1530");
        req.worktree_branch = None;
        assert_eq!(default_name(&req), "thurbox");
        assert_eq!(default_name(&request("/")), "session");
    }

    #[test]
    fn a_remote_path_is_not_checked_on_this_machine() {
        let req = request("/no/such/path/anywhere");
        assert!(check_paths(&req, true).is_err());
        assert_eq!(check_paths(&req, false), Ok(()));
    }

    #[test]
    fn opening_a_worktree_needs_its_branch() {
        let dir = tempfile::tempdir().unwrap();
        let mut req = request(&dir.path().display().to_string());
        req.existing_worktree = Some(dir.path().into());
        assert!(matches!(check_paths(&req, true), Err(OpError::Usage(_))));
        req.worktree_branch = Some("main".into());
        assert_eq!(check_paths(&req, true), Ok(()));
    }
}
