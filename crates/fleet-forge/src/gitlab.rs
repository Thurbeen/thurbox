//! The forge contract served by `glab` for GitLab, gitlab.com or
//! self-hosted: bounded `glab api` processes read the merge request, its
//! discussions with pagination and its approvals, mapped onto Fleet's
//! vocabulary by the adapter truth table in the ChangeRequest interface
//! page; and one write, the merge request's title with its draft marker
//! removed, after a fresh read of the head and the draft state.

use std::ffi::OsString;
use std::time::Duration;

use fleet_api::forge::{
    ChangeObservation, ChangeRef, ChangeSnapshot, ChangeState, Checks, Expected, FailingCheck,
    Forge, HeadSeen, Mergeable, Note, NoteKind, Review, ReviewDecision, ReviewState, Threads,
    Uncertainty,
};
use fleet_api::plan::EffectOutcome;
use fleet_api::{Millis, Observation};
use serde_json::Value;

use crate::cli::{self, Budget, Reply};

pub const KIND: &str = "gitlab";

/// Discussions fetched per page.
const PAGE: usize = 100;

#[derive(Debug, Clone)]
pub struct GitlabForge {
    host: String,
    cli: OsString,
    call_timeout: Duration,
}

impl GitlabForge {
    pub fn new(host: &str, cli: impl Into<OsString>, call_timeout: Duration) -> GitlabForge {
        GitlabForge {
            host: host.to_ascii_lowercase(),
            cli: cli.into(),
            call_timeout,
        }
    }

    fn api(&self, endpoint: &str, timeout: Duration) -> Result<Reply, (Uncertainty, String)> {
        self.api_with(&[], endpoint, timeout)
    }

    /// A call with extra flags before the endpoint: a method, a field.
    fn api_with(
        &self,
        extra: &[String],
        endpoint: &str,
        timeout: Duration,
    ) -> Result<Reply, (Uncertainty, String)> {
        let mut args = vec![
            "api".to_string(),
            "--hostname".to_string(),
            self.host.clone(),
            "--include".to_string(),
        ];
        args.extend(extra.iter().cloned());
        args.push(endpoint.to_string());
        cli::call(&self.cli, &args, timeout)
    }
}

/// The JSON of a REST answer, or why it is Unknown. A 404 is handed back as
/// `Ok(None)` for the caller to disambiguate.
fn body_of(reply: &Reply) -> Result<Option<Value>, (Uncertainty, String)> {
    if reply.throttled() {
        return Err((
            Uncertainty::Throttled,
            format!("the forge asked to slow down (HTTP {})", reply.status),
        ));
    }
    if reply.status == 401 {
        return Err((Uncertainty::NotLoggedIn, "HTTP 401".into()));
    }
    if reply.status == 404 {
        return Ok(None);
    }
    if reply.status >= 400 || (reply.status == 0 && reply.exit != Some(0)) {
        return Err((
            Uncertainty::Refused,
            format!(
                "HTTP {}: {}",
                reply.status,
                if reply.stderr.is_empty() {
                    reply.body.trim().chars().take(200).collect::<String>()
                } else {
                    reply.stderr.clone()
                }
            ),
        ));
    }
    serde_json::from_str(reply.body.trim())
        .map(Some)
        .map_err(|e| (Uncertainty::Unreadable, format!("unreadable answer: {e}")))
}

impl Forge for GitlabForge {
    fn operation_bound(&self) -> Millis {
        cli::operation_bound(self.call_timeout)
    }

    /// `https://<host>/<group>/<project>/-/merge_requests/<iid>`; the `/-`
    /// is optional, fragments and queries are dropped.
    fn parse(&self, url: &str) -> Observation<ChangeRef> {
        let Some((host, project, id)) = cli::parse_change_url(url) else {
            return Observation::unknown("the URL does not name a merge request");
        };
        if host != self.host {
            return Observation::unknown(format!(
                "the URL names {host}, not the configured host {}",
                self.host
            ));
        }
        if !url.contains("/merge_requests/") {
            return Observation::unknown(
                "a GitLab merge request URL is <project path>/-/merge_requests/<iid>",
            );
        }
        Observation::Known(ChangeRef {
            kind: KIND.into(),
            host,
            project,
            id,
        })
    }

    fn whoami(&self, host: &str) -> Observation<String> {
        if host != self.host {
            return Observation::unknown(format!("host {host} is not this adapter's"));
        }
        let reply = match self.api("user", self.call_timeout) {
            Ok(r) => r,
            Err((_, why)) => return Observation::unknown(why),
        };
        match body_of(&reply) {
            Ok(Some(v)) => match v["username"].as_str() {
                Some(u) => Observation::Known(u.to_string()),
                None => Observation::unknown("no username in the answer"),
            },
            Ok(None) => Observation::unknown("HTTP 404"),
            Err((_, why)) => Observation::unknown(why),
        }
    }

    fn change(&self, change: &ChangeRef) -> ChangeObservation {
        let project = cli::encode_segment(&change.project);
        let base = format!("projects/{project}/merge_requests/{}", change.id);
        let mut budget = Budget::new(self.call_timeout);
        let mut fetch = |endpoint: &str| -> Result<Option<Value>, Unreached> {
            let Some(timeout) = budget.next_timeout() else {
                return Err(Unreached {
                    kind: Uncertainty::Incomplete,
                    why: "the bound ran out before every page was read".into(),
                    retry_after: None,
                });
            };
            let reply = self
                .api(endpoint, timeout)
                .map_err(|(kind, why)| Unreached {
                    kind,
                    why,
                    retry_after: None,
                })?;
            let retry_after = reply.retry_after();
            body_of(&reply).map_err(|(kind, why)| Unreached {
                kind,
                why,
                retry_after: if kind == Uncertainty::Throttled {
                    retry_after
                } else {
                    None
                },
            })
        };
        let mr = match fetch(&base) {
            Ok(Some(mr)) => mr,
            Ok(None) => {
                // Not found: proven absent only when the project itself answers.
                return match fetch(&format!("projects/{project}")) {
                    Ok(Some(_)) => ChangeObservation::Known(None),
                    Ok(None) => ChangeObservation::unknown(
                        Uncertainty::Refused,
                        "the project was not found or is not accessible",
                    ),
                    Err(u) => u.into(),
                };
            }
            Err(u) => return u.into(),
        };
        let mut discussions = Vec::new();
        let mut page_no = 1;
        loop {
            let page = match fetch(&format!(
                "{base}/discussions?per_page={PAGE}&page={page_no}"
            )) {
                Ok(Some(Value::Array(items))) => items,
                Ok(Some(_)) => {
                    return ChangeObservation::unknown(
                        Uncertainty::Unreadable,
                        "discussions are not a list",
                    );
                }
                Ok(None) => {
                    return ChangeObservation::unknown(
                        Uncertainty::Refused,
                        "discussions answered 404",
                    );
                }
                Err(u) => return u.into(),
            };
            let n = page.len();
            discussions.extend(page);
            if n < PAGE {
                break;
            }
            page_no += 1;
        }
        let approvals = match fetch(&format!("{base}/approvals")) {
            Ok(Some(v)) => v,
            Ok(None) => Value::Null,
            Err(u) => return u.into(),
        };
        match snapshot(&mr, &discussions, &approvals) {
            Ok(s) => ChangeObservation::Known(Some(s)),
            Err(why) => ChangeObservation::unknown(Uncertainty::Unreadable, why),
        }
    }

    /// Read the merge request afresh; refuse with `Moved` when its head or
    /// state is not what the decision expected; else update its title with
    /// the draft marker removed, which is how GitLab takes a merge request
    /// out of draft. Everything before the update that goes wrong is a Known
    /// refusal; past it, only an answer the adapter can read is Known.
    fn mark_ready(
        &self,
        change: &ChangeRef,
        expect: &Expected,
        intent_key: &str,
    ) -> Observation<EffectOutcome> {
        let project = cli::encode_segment(&change.project);
        let base = format!("projects/{project}/merge_requests/{}", change.id);
        let reply = match self.api(&base, self.call_timeout) {
            Ok(r) => r,
            Err((_, why)) => return failed(format!("fresh read: {why}")),
        };
        let mr = match body_of(&reply) {
            Ok(Some(mr)) => mr,
            Ok(None) => return failed("the merge request was not found"),
            Err((_, why)) => return failed(format!("fresh read: {why}")),
        };
        let (head, state) = match head_and_state(&mr) {
            Ok(x) => x,
            Err(why) => return failed(format!("fresh read: {why}")),
        };
        if head != expect.head || state != expect.state {
            return Observation::Known(EffectOutcome::Moved { head, state });
        }
        let title = mr["title"].as_str().unwrap_or("");
        let ready_title = without_draft_marker(title);
        if ready_title == title.trim() {
            return failed(
                "the merge request is a draft without a recognised draft marker in its title",
            );
        }
        // GitLab's REST update has no idempotency slot for the intent key;
        // the key stays on the Task's record, where recovery reads it.
        let _ = intent_key;
        let extra = vec![
            "-X".to_string(),
            "PUT".to_string(),
            "-f".to_string(),
            format!("title={ready_title}"),
        ];
        match self.api_with(&extra, &base, self.call_timeout) {
            Ok(reply) => update_outcome(&reply),
            // The process ran out of time: the update may have landed.
            Err((Uncertainty::Timeout, why)) => Observation::unknown(why),
            Err((_, why)) => failed(why),
        }
    }
}

fn failed(why: impl Into<String>) -> Observation<EffectOutcome> {
    Observation::Known(EffectOutcome::Failed { why: why.into() })
}

/// The state of a merge request. Only an open merge request is a draft:
/// GitLab keeps the draft flag on a draft that is closed or merged.
fn state_of(mr: &Value) -> Result<ChangeState, String> {
    let draft = mr["draft"] == true || mr["work_in_progress"] == true;
    match mr["state"].as_str() {
        Some("opened") if draft => Ok(ChangeState::Draft),
        Some("opened") => Ok(ChangeState::Open),
        Some("closed") => Ok(ChangeState::Closed),
        Some("merged") => Ok(ChangeState::Merged),
        other => Err(format!("state {other:?} cannot be placed")),
    }
}

/// The head and state of a fresh read, by the same rule as the snapshot.
fn head_and_state(mr: &Value) -> Result<(String, ChangeState), String> {
    let state = state_of(mr)?;
    let head = mr["sha"].as_str().ok_or("no sha")?.to_string();
    Ok((head, state))
}

/// A merge request's title with GitLab's draft markers removed: `Draft:`,
/// `[Draft]`, `(Draft)`, `Draft -` and the legacy `WIP:` forms, however
/// cased, repeated, with the whitespace around them.
pub fn without_draft_marker(title: &str) -> String {
    const MARKERS: &[&str] = &[
        "[draft]", "(draft)", "draft:", "draft -", "draft ", "[wip]", "(wip)", "wip:", "wip ",
    ];
    let mut rest = title.trim();
    loop {
        let lower = rest.to_ascii_lowercase();
        let Some(m) = MARKERS.iter().find(|m| lower.starts_with(*m)) else {
            return rest.to_string();
        };
        rest = rest[m.len()..].trim_start();
    }
}

/// What the update's answer means: Known when the forge's answer can be
/// read, Unknown otherwise, because the update may have landed.
pub fn update_outcome(reply: &Reply) -> Observation<EffectOutcome> {
    if reply.throttled() {
        return failed(format!(
            "the forge asked to slow down (HTTP {})",
            reply.status
        ));
    }
    if reply.status == 401 {
        return failed("HTTP 401: the CLI has no login for this host");
    }
    if reply.status == 404 {
        return failed("the merge request was not found");
    }
    if reply.status >= 400 {
        return failed(format!(
            "HTTP {}: {}",
            reply.status,
            if reply.stderr.is_empty() {
                reply.body.trim().chars().take(200).collect::<String>()
            } else {
                reply.stderr.clone()
            }
        ));
    }
    let v: Value = match serde_json::from_str(reply.body.trim()) {
        Ok(v) => v,
        Err(e) => {
            return Observation::unknown(format!(
                "the update's answer could not be read ({e}); it may have landed"
            ));
        }
    };
    match (&v["draft"], &v["work_in_progress"]) {
        (Value::Bool(false), Value::Bool(true)) | (Value::Bool(true), _) => {
            failed("the forge left the merge request a draft")
        }
        (Value::Bool(false), _) => {
            Observation::Known(EffectOutcome::MarkedReady { already: false })
        }
        _ => {
            Observation::unknown("the update's answer says nothing about draft; it may have landed")
        }
    }
}

/// Why one call left the observation Unknown: small, so the fetch closure's
/// error stays small; the whole answer is built once, at the end.
struct Unreached {
    kind: Uncertainty,
    why: String,
    retry_after: Option<Millis>,
}

impl From<Unreached> for ChangeObservation {
    fn from(u: Unreached) -> ChangeObservation {
        ChangeObservation::Unknown {
            kind: u.kind,
            why: u.why,
            retry_after: u.retry_after,
        }
    }
}

fn is_bot(user: &Value) -> bool {
    user["bot"] == true
        || user["username"]
            .as_str()
            .is_some_and(|u| u.starts_with("project_") && u.contains("_bot"))
        || user["username"]
            .as_str()
            .is_some_and(|u| u.starts_with("group_") && u.contains("_bot"))
}

fn millis(v: &Value, field: &str) -> Result<Millis, String> {
    v[field]
        .as_str()
        .and_then(cli::iso_millis)
        .ok_or_else(|| format!("{field} is not a timestamp"))
}

/// The truth table: GitLab's answer in Fleet's words, or why it cannot be
/// placed, which makes the whole observation Unknown.
pub fn snapshot(
    mr: &Value,
    discussions: &[Value],
    approvals: &Value,
) -> Result<ChangeSnapshot, String> {
    let state = state_of(mr)?;
    let head_id = mr["sha"].as_str().ok_or("no sha")?.to_string();
    let pipeline = &mr["head_pipeline"];
    let (checks, failing) = if pipeline.is_null() {
        (Checks::None, Vec::new())
    } else {
        match pipeline["status"].as_str() {
            Some("success") => (Checks::Passed, Vec::new()),
            Some("failed" | "canceled" | "canceling") => (
                Checks::Failed,
                vec![FailingCheck {
                    name: "pipeline".into(),
                    url: pipeline["web_url"].as_str().map(str::to_string),
                }],
            ),
            Some(
                "running"
                | "pending"
                | "created"
                | "manual"
                | "waiting_for_resource"
                | "preparing"
                | "scheduled"
                | "waiting_for_callback",
            ) => (Checks::Pending, Vec::new()),
            Some("skipped") => (Checks::None, Vec::new()),
            other => return Err(format!("pipeline status {other:?} cannot be placed")),
        }
    };
    let approved = approvals["approved"] == true;
    let review_decision = if approved {
        ReviewDecision::Approved
    } else {
        ReviewDecision::None
    };
    let mergeable = match mr["detailed_merge_status"].as_str() {
        Some("mergeable") => Mergeable {
            value: Some(true),
            reason: None,
        },
        Some("checking" | "unchecked") | None => Mergeable {
            value: None,
            reason: None,
        },
        Some(other) => Mergeable {
            value: Some(false),
            reason: Some(other.to_string()),
        },
    };
    let web_url = mr["web_url"].as_str().unwrap_or("");
    let mut reviews = Vec::new();
    for a in approvals["approved_by"].as_array().into_iter().flatten() {
        let user = &a["user"];
        let Some(username) = user["username"].as_str() else {
            return Err("an approval without a user".into());
        };
        reviews.push(Review {
            id: format!("approval:{username}"),
            author: username.to_string(),
            bot: is_bot(user),
            state: ReviewState::Approved,
            head: head_id.clone(),
        });
    }
    let mut notes = Vec::new();
    let mut unresolved = 0;
    for d in discussions {
        let thread = d["id"]
            .as_str()
            .ok_or("a discussion without an id")?
            .to_string();
        let individual = d["individual_note"] == true;
        let items = d["notes"].as_array().ok_or("a discussion without notes")?;
        let resolvable = items.iter().any(|n| n["resolvable"] == true);
        let resolved = items
            .iter()
            .filter(|n| n["resolvable"] == true)
            .all(|n| n["resolved"] == true);
        if resolvable && !resolved {
            unresolved += 1;
        }
        for n in items {
            if n["system"] == true {
                continue;
            }
            let id = match &n["id"] {
                Value::Number(x) => x.to_string(),
                Value::String(s) => s.clone(),
                _ => return Err("a note without an id".into()),
            };
            let inline = n["type"] == "DiffNote" && n["position"].is_object();
            let at = millis(n, "created_at")?;
            let edited_at = match n["updated_at"].as_str() {
                Some(s) => {
                    let t = cli::iso_millis(s).ok_or("updated_at is not a timestamp")?;
                    (t != at).then_some(t)
                }
                None => None,
            };
            let position = &n["position"];
            notes.push(Note {
                id: id.clone(),
                kind: if inline {
                    NoteKind::Inline
                } else {
                    NoteKind::Comment
                },
                author: n["author"]["username"]
                    .as_str()
                    .unwrap_or("ghost")
                    .to_string(),
                bot: is_bot(&n["author"]),
                thread: (inline && !individual).then(|| thread.clone()),
                resolved: if inline && !individual {
                    Some(n["resolved"] == true)
                } else {
                    None
                },
                path: if inline {
                    position["new_path"]
                        .as_str()
                        .or(position["old_path"].as_str())
                        .map(str::to_string)
                } else {
                    None
                },
                line: if inline {
                    position["new_line"]
                        .as_u64()
                        .or_else(|| position["old_line"].as_u64())
                } else {
                    None
                },
                at,
                edited_at,
                url: format!("{web_url}#note_{id}"),
                body: n["body"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    Ok(ChangeSnapshot {
        state,
        head: HeadSeen {
            id: head_id,
            checks,
            failing,
        },
        review_decision,
        mergeable,
        threads: Threads { unresolved },
        reviews,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn mr(extra: Value) -> Value {
        let mut v = json!({"state": "opened", "draft": false, "sha": "h1", "detailed_merge_status": "mergeable", "head_pipeline": null, "web_url": "https://gitlab.test/g/p/-/merge_requests/3"});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    }

    #[test]
    fn state_then_draft_and_the_pipeline_folds() {
        let state = |m: Value| snapshot(&mr(m), &[], &Value::Null).unwrap().state;
        assert_eq!(
            state(json!({"draft": true, "state": "merged"})),
            ChangeState::Merged
        );
        assert_eq!(
            state(json!({"work_in_progress": true, "state": "closed"})),
            ChangeState::Closed
        );
        assert_eq!(
            state(json!({"draft": true, "state": "opened"})),
            ChangeState::Draft
        );
        let s = snapshot(
            &mr(json!({"head_pipeline": {"status": "failed", "web_url": "u"}})),
            &[],
            &Value::Null,
        )
        .unwrap();
        assert_eq!(s.head.checks, Checks::Failed);
        assert_eq!(s.head.failing[0].url.as_deref(), Some("u"));
        let s = snapshot(
            &mr(json!({"head_pipeline": {"status": "manual"}})),
            &[],
            &Value::Null,
        )
        .unwrap();
        assert_eq!(s.head.checks, Checks::Pending);
        let s = snapshot(
            &mr(json!({"head_pipeline": {"status": "skipped"}})),
            &[],
            &Value::Null,
        )
        .unwrap();
        assert_eq!(s.head.checks, Checks::None);
        assert!(snapshot(&mr(json!({"state": "locked"})), &[], &Value::Null).is_err());
    }

    #[test]
    fn mergeable_and_approvals_follow_the_truth_table() {
        let s = snapshot(
            &mr(json!({"detailed_merge_status": "ci_must_pass"})),
            &[],
            &json!({"approved": true, "approved_by": [{"user": {"username": "ann"}}]}),
        )
        .unwrap();
        assert_eq!(
            s.mergeable,
            Mergeable {
                value: Some(false),
                reason: Some("ci_must_pass".into())
            }
        );
        assert_eq!(s.review_decision, ReviewDecision::Approved);
        assert_eq!(s.reviews[0].state, ReviewState::Approved);
        assert_eq!(s.reviews[0].head, "h1");
        let s = snapshot(
            &mr(json!({"detailed_merge_status": "checking"})),
            &[],
            &json!({"approved": false}),
        )
        .unwrap();
        assert_eq!(s.mergeable.value, None);
        assert_eq!(s.review_decision, ReviewDecision::None);
    }

    #[test]
    fn the_draft_marker_is_removed_however_it_is_spelled() {
        assert_eq!(without_draft_marker("Draft: add it"), "add it");
        assert_eq!(
            without_draft_marker("  [DRAFT] (draft) Draft - add it"),
            "add it"
        );
        assert_eq!(without_draft_marker("WIP: add it"), "add it");
        assert_eq!(
            without_draft_marker("Drafting the plan"),
            "Drafting the plan"
        );
        assert_eq!(without_draft_marker("add it"), "add it");
        let (head, state) =
            head_and_state(&json!({"draft": true, "state": "opened", "sha": "h1"})).unwrap();
        assert_eq!((head.as_str(), state), ("h1", ChangeState::Draft));
    }

    #[test]
    fn an_updates_answer_is_known_only_when_it_can_be_read() {
        let reply = |status: u16, body: &str| Reply {
            status,
            body: body.into(),
            exit: Some(0),
            ..Reply::default()
        };
        assert_eq!(
            update_outcome(&reply(
                200,
                r#"{"iid": 3, "draft": false, "title": "add it"}"#
            )),
            Observation::Known(EffectOutcome::MarkedReady { already: false })
        );
        assert_eq!(
            update_outcome(&reply(200, r#"{"iid": 3, "draft": true}"#)),
            Observation::Known(EffectOutcome::Failed {
                why: "the forge left the merge request a draft".into()
            })
        );
        assert!(matches!(
            update_outcome(&reply(403, r#"{"message": "403 Forbidden"}"#)),
            Observation::Known(EffectOutcome::Failed { .. })
        ));
        assert!(update_outcome(&reply(200, "<html>gateway</html>")).is_unknown());
    }

    #[test]
    fn discussions_become_notes_and_unresolved_threads_are_counted() {
        let discussions = vec![
            json!({"id": "d1", "individual_note": true, "notes": [
                {"id": 1, "body": "hi", "author": {"username": "ann"}, "system": false, "created_at": "2026-10-24T12:00:00Z", "updated_at": "2026-10-24T12:00:00Z", "resolvable": false}
            ]}),
            json!({"id": "d2", "individual_note": false, "notes": [
                {"id": 2, "type": "DiffNote", "body": "fix", "author": {"username": "project_7_bot_a", "bot": true}, "system": false, "created_at": "2026-10-24T12:00:00Z", "updated_at": "2026-10-24T13:00:00Z", "resolvable": true, "resolved": false, "position": {"new_path": "a.rs", "new_line": 4}}
            ]}),
            json!({"id": "d3", "individual_note": false, "notes": [
                {"id": 3, "body": "assigned", "author": {"username": "ann"}, "system": true, "created_at": "2026-10-24T12:00:00Z", "updated_at": "2026-10-24T12:00:00Z"}
            ]}),
        ];
        let s = snapshot(&mr(json!({})), &discussions, &Value::Null).unwrap();
        assert_eq!(s.threads.unresolved, 1);
        assert_eq!(s.notes.len(), 2, "system notes are not notes");
        let (a, b) = (&s.notes[0], &s.notes[1]);
        assert_eq!(
            (a.kind, a.thread.as_deref(), a.edited_at),
            (NoteKind::Comment, None, None)
        );
        assert_eq!(
            (b.kind, b.thread.as_deref(), b.resolved, b.bot, b.line),
            (NoteKind::Inline, Some("d2"), Some(false), true, Some(4))
        );
        assert!(b.edited_at.is_some());
        assert_eq!(b.url, "https://gitlab.test/g/p/-/merge_requests/3#note_2");
    }
}
