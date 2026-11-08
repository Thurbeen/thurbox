//! Bitbucket Cloud pull requests through an externally authenticated `curl`.
//!
//! Fleet constructs only HTTPS API reads. Credentials remain in curl's own
//! configuration or netrc and never enter Fleet configuration, resources,
//! arguments or logs. Bitbucket Server and Data Center use a different API
//! and are deliberately not served here.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::time::Duration;

use fleet_api::forge::{
    ChangeObservation, ChangeRef, ChangeSnapshot, ChangeState, Checks, Expected, FailingCheck,
    Forge, HeadSeen, Mergeable, Note, NoteKind, ReviewDecision, Threads, Uncertainty,
};
use fleet_api::plan::EffectOutcome;
use fleet_api::{Millis, Observation};
use serde_json::Value;

use crate::cli::{self, Budget, Reply};

pub const KIND: &str = "bitbucket-cloud";
pub const HOST: &str = "bitbucket.org";
const API: &str = "https://api.bitbucket.org/2.0";
const PAGE: usize = 100;

type ReadError = (Uncertainty, String, Option<Millis>);

#[derive(Debug, Clone)]
pub struct BitbucketCloudForge {
    cli: OsString,
    call_timeout: Duration,
}

impl BitbucketCloudForge {
    pub fn new(cli: impl Into<OsString>, call_timeout: Duration) -> Self {
        Self {
            cli: cli.into(),
            call_timeout,
        }
    }

    fn api(&self, url: &str, timeout: Duration) -> Result<Reply, (Uncertainty, String)> {
        if !url.starts_with(&format!("{API}/")) {
            return Err((
                Uncertainty::Unreadable,
                "Bitbucket pagination left the Cloud API origin".into(),
            ));
        }
        let args = vec![
            "--include".into(),
            "--silent".into(),
            "--show-error".into(),
            "--suppress-connect-headers".into(),
            "--netrc-optional".into(),
            "--proto".into(),
            "=https".into(),
            "--request".into(),
            "GET".into(),
            "--header".into(),
            "Accept: application/json".into(),
            "--url".into(),
            url.into(),
        ];
        cli::call(&self.cli, &args, timeout)
    }

    fn get(&self, url: &str, budget: &mut Budget) -> Result<Reply, ReadError> {
        let timeout = budget.next_timeout().ok_or_else(|| {
            (
                Uncertainty::Incomplete,
                "the bound ran out before the Bitbucket Cloud observation was complete".into(),
                None,
            )
        })?;
        self.api(url, timeout)
            .map_err(|(kind, why)| (kind, why, None))
    }

    fn pages(
        &self,
        first: String,
        budget: &mut Budget,
        label: &str,
    ) -> Result<Vec<Value>, ReadError> {
        let mut next = Some(first);
        let mut values = Vec::new();
        let mut announced = None;
        while let Some(url) = next.take() {
            let reply = self.get(&url, budget)?;
            let body = body_of(&reply)?.ok_or_else(|| {
                (
                    Uncertainty::Refused,
                    format!("{label} were not found"),
                    None,
                )
            })?;
            let page = body["values"].as_array().ok_or_else(|| {
                (
                    Uncertainty::Unreadable,
                    format!("{label} have no values array"),
                    None,
                )
            })?;
            if let Some(size) = body["size"].as_u64() {
                if announced.is_some_and(|old| old != size) {
                    return Err((
                        Uncertainty::Incomplete,
                        format!("{label} changed size while being paged"),
                        None,
                    ));
                }
                announced = Some(size);
            }
            values.extend(page.iter().cloned());
            next = match body.get("next") {
                None | Some(Value::Null) => None,
                Some(Value::String(url)) if url.starts_with(&format!("{API}/")) => {
                    Some(url.clone())
                }
                Some(Value::String(_)) => {
                    return Err((
                        Uncertainty::Unreadable,
                        format!("{label} pagination left the Bitbucket Cloud API origin"),
                        None,
                    ));
                }
                Some(_) => {
                    return Err((
                        Uncertainty::Unreadable,
                        format!("{label} have a malformed next link"),
                        None,
                    ));
                }
            };
        }
        if announced.is_some_and(|size| size != values.len() as u64) {
            return Err((
                Uncertainty::Incomplete,
                format!(
                    "{label} were truncated ({} announced, {} returned)",
                    announced.unwrap_or_default(),
                    values.len()
                ),
                None,
            ));
        }
        Ok(values)
    }
}

fn body_of(reply: &Reply) -> Result<Option<Value>, ReadError> {
    if reply.throttled() {
        return Err((
            Uncertainty::Throttled,
            format!("the forge asked to slow down (HTTP {})", reply.status),
            reply.retry_after(),
        ));
    }
    if reply.status == 401 {
        return Err((
            Uncertainty::NotLoggedIn,
            "the authenticated HTTP client has no usable Bitbucket Cloud login".into(),
            None,
        ));
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
            None,
        ));
    }
    serde_json::from_str(reply.body.trim())
        .map(Some)
        .map_err(|e| {
            (
                Uncertainty::Unreadable,
                format!("unreadable answer: {e}"),
                None,
            )
        })
}

fn split_project(change: &ChangeRef) -> Result<(&str, &str), String> {
    let (workspace, repository) = change
        .project
        .split_once('/')
        .ok_or("project is not workspace/repository")?;
    if workspace.is_empty() || repository.is_empty() || repository.contains('/') {
        return Err("project is not workspace/repository".into());
    }
    Ok((workspace, repository))
}

fn parse_url(url: &str) -> Result<ChangeRef, String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .ok_or("a Bitbucket Cloud pull request URL must use HTTPS")?;
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let mut parts: Vec<&str> = rest.split('/').filter(|part| !part.is_empty()).collect();
    if parts.len() != 5
        || !parts[0].eq_ignore_ascii_case(HOST)
        || !parts[3].eq_ignore_ascii_case("pull-requests")
        || parts[1].is_empty()
        || parts[2].is_empty()
        || parts[4].parse::<u64>().is_err()
    {
        return Err(
            "a Bitbucket Cloud pull request URL is bitbucket.org/workspace/repository/pull-requests/id"
                .into(),
        );
    }
    let valid_slug = |slug: &str| {
        slug.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    if !valid_slug(parts[1]) || !valid_slug(parts[2]) {
        return Err("the Bitbucket Cloud workspace or repository slug is invalid".into());
    }
    let id = parts.pop().unwrap_or_default().to_string();
    Ok(ChangeRef {
        kind: KIND.into(),
        host: HOST.into(),
        project: format!(
            "{}/{}",
            parts[1].to_ascii_lowercase(),
            parts[2].to_ascii_lowercase()
        ),
        id,
    })
}

fn stable_identity(value: &Value, what: &str) -> Result<String, String> {
    value["account_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .or_else(|| value["uuid"].as_str().filter(|id| !id.is_empty()))
        .map(str::to_string)
        .ok_or_else(|| format!("{what} has neither an account_id nor a uuid"))
}

fn author(value: &Value) -> Result<(String, bool), String> {
    let id = stable_identity(value, "a comment author")?;
    let bot = match value["type"].as_str() {
        Some("user") => false,
        Some("app_user") => true,
        other => return Err(format!("comment author type {other:?} cannot be placed")),
    };
    Ok((id, bot))
}

fn state_of(pr: &Value) -> Result<ChangeState, String> {
    let draft = pr["draft"]
        .as_bool()
        .ok_or("pull request has no draft flag")?;
    // Only an open pull request is a draft; a declined draft keeps the flag.
    match pr["state"].as_str() {
        Some("OPEN") if draft => Ok(ChangeState::Draft),
        Some("OPEN") => Ok(ChangeState::Open),
        Some("DECLINED" | "SUPERSEDED") => Ok(ChangeState::Closed),
        Some("MERGED") => Ok(ChangeState::Merged),
        other => Err(format!("pull request state {other:?} cannot be placed")),
    }
}

fn repository_identity(pr: &Value, project: &str) -> Result<String, String> {
    let repo = &pr["destination"]["repository"];
    let full_name = repo["full_name"]
        .as_str()
        .ok_or("pull request destination has no repository full_name")?;
    if !full_name.eq_ignore_ascii_case(project) {
        return Err("the pull request URL resolved to another destination repository".into());
    }
    repo["uuid"]
        .as_str()
        .filter(|uuid| !uuid.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "pull request destination repository has no uuid".into())
}

fn review_decision(pr: &Value) -> Result<ReviewDecision, String> {
    let participants = pr["participants"]
        .as_array()
        .ok_or("pull request has no participants array")?;
    let mut approved = false;
    for participant in participants {
        stable_identity(&participant["user"], "a pull request participant")?;
        match participant["state"].as_str() {
            Some("changes_requested") => return Ok(ReviewDecision::ChangesRequested),
            Some("approved") => approved = true,
            None | Some("") => {
                approved |= participant["approved"].as_bool().unwrap_or(false);
            }
            other => return Err(format!("participant state {other:?} cannot be placed")),
        }
    }
    Ok(if approved {
        ReviewDecision::Approved
    } else {
        ReviewDecision::None
    })
}

fn checks_of(
    statuses: &[Value],
    project: &str,
    head: &str,
) -> Result<(Checks, Vec<FailingCheck>), String> {
    let mut failing = Vec::new();
    let mut pending = false;
    let mut passed = false;
    let commit_url = format!("{API}/repositories/{project}/commit/{head}");
    for status in statuses {
        if status["links"]["commit"]["href"].as_str() != Some(commit_url.as_str()) {
            return Err("a commit status does not identify the observed head".into());
        }
        let name = status["name"]
            .as_str()
            .or_else(|| status["key"].as_str())
            .filter(|name| !name.is_empty())
            .ok_or("a commit status has no name or key")?
            .to_string();
        match status["state"].as_str() {
            Some("FAILED" | "STOPPED") => failing.push(FailingCheck {
                name,
                url: status["url"].as_str().map(str::to_string),
            }),
            Some("INPROGRESS") => pending = true,
            Some("SUCCESSFUL") => passed = true,
            other => return Err(format!("commit status state {other:?} cannot be placed")),
        }
    }
    let checks = if !failing.is_empty() {
        Checks::Failed
    } else if pending {
        Checks::Pending
    } else if passed {
        Checks::Passed
    } else {
        Checks::None
    };
    Ok((checks, failing))
}

fn mergeable_of(state: ChangeState, values: &[Value]) -> Result<Mergeable, String> {
    if state == ChangeState::Draft {
        return Ok(Mergeable {
            value: Some(false),
            reason: Some("draft".into()),
        });
    }
    let mut pending = false;
    for check in values {
        let kind = check["type"]
            .as_str()
            .ok_or("a mergeability check has no type")?;
        let blocking = check["blocking"]
            .as_bool()
            .ok_or("a mergeability check has no blocking flag")?;
        match check["status"].as_str() {
            Some("FAILED") if blocking => {
                return Ok(Mergeable {
                    value: Some(false),
                    reason: Some(kind.replace('_', "-")),
                });
            }
            Some("UNKNOWN") => pending = true,
            Some("PENDING") if blocking => pending = true,
            Some("PASSED" | "FAILED" | "PENDING" | "SKIPPED") => {}
            other => return Err(format!("mergeability status {other:?} cannot be placed")),
        }
    }
    Ok(Mergeable {
        value: (!pending).then_some(true),
        reason: None,
    })
}

fn notes_of(comments: &[Value]) -> Result<(Vec<Note>, Threads), String> {
    let structural: Vec<&Value> = comments
        .iter()
        .filter(|comment| comment["pending"] != true)
        .collect();
    let by_id: BTreeMap<u64, &Value> = structural
        .iter()
        .map(|comment| {
            comment["id"]
                .as_u64()
                .map(|id| (id, *comment))
                .ok_or("a pull request comment has no numeric id")
        })
        .collect::<Result<_, _>>()?;
    let visible: Vec<&Value> = structural
        .into_iter()
        .filter(|comment| comment["deleted"] != true)
        .collect();
    let mut unresolved = BTreeSet::new();
    let mut notes = Vec::new();
    for comment in visible {
        let id = comment["id"]
            .as_u64()
            .ok_or("a pull request comment has no numeric id")?;
        let parent = comment["parent"]["id"].as_u64();
        let root = match parent {
            Some(parent) => by_id
                .get(&parent)
                .copied()
                .ok_or("a pull request comment refers to a missing thread root")?,
            None => comment,
        };
        let inline = root["inline"].as_object();
        let (kind, thread, resolved, path, line) = if let Some(inline) = inline {
            if !root
                .as_object()
                .is_some_and(|root| root.contains_key("resolution"))
            {
                return Err("an inline comment has no thread-resolution evidence".into());
            }
            let root_id = root["id"]
                .as_u64()
                .ok_or("an inline comment has no numeric root id")?;
            let resolved = !root["resolution"].is_null();
            if !resolved {
                unresolved.insert(root_id);
            }
            (
                NoteKind::Inline,
                Some(root_id.to_string()),
                Some(resolved),
                inline
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                inline
                    .get("to")
                    .and_then(Value::as_u64)
                    .or_else(|| inline.get("from").and_then(Value::as_u64)),
            )
        } else {
            (NoteKind::Comment, None, None, None, None)
        };
        let (author, bot) = author(&comment["user"])?;
        let at = comment["created_on"]
            .as_str()
            .and_then(cli::iso_millis)
            .ok_or("comment created_on is not a timestamp")?;
        let edited_at = comment["updated_on"]
            .as_str()
            .and_then(cli::iso_millis)
            .filter(|edited| *edited != at);
        notes.push(Note {
            id: id.to_string(),
            kind,
            author,
            bot,
            thread,
            resolved,
            path,
            line,
            at,
            edited_at,
            url: comment["links"]["html"]["href"]
                .as_str()
                .ok_or("a pull request comment has no HTML URL")?
                .to_string(),
            body: comment["content"]["raw"]
                .as_str()
                .ok_or("a pull request comment has no raw body")?
                .to_string(),
        });
    }
    notes.sort_by_key(|note| (note.at, note.id.clone()));
    Ok((
        notes,
        Threads {
            unresolved: unresolved.len() as u64,
        },
    ))
}

fn snapshot(
    project: &str,
    first: &Value,
    final_pr: &Value,
    statuses: &[Value],
    comments: &[Value],
    mergeability: &[Value],
) -> Result<ChangeSnapshot, String> {
    let first_repo = repository_identity(first, project)?;
    let final_repo = repository_identity(final_pr, project)?;
    if first_repo != final_repo {
        return Err("the pull request destination repository changed while being read".into());
    }
    let first_head = first["source"]["commit"]["hash"]
        .as_str()
        .filter(|head| !head.is_empty())
        .ok_or("pull request source has no commit hash")?;
    let head = final_pr["source"]["commit"]["hash"]
        .as_str()
        .filter(|head| !head.is_empty())
        .ok_or("pull request source has no commit hash")?;
    if first_head != head {
        return Err("the pull request head changed while its evidence was being read".into());
    }
    let first_target = first["destination"]["commit"]["hash"]
        .as_str()
        .filter(|target| !target.is_empty())
        .ok_or("pull request destination has no commit hash")?;
    let target = final_pr["destination"]["commit"]["hash"]
        .as_str()
        .filter(|target| !target.is_empty())
        .ok_or("pull request destination has no commit hash")?;
    if first_target != target {
        return Err(
            "the pull request destination changed while its evidence was being read".into(),
        );
    }
    let state = state_of(final_pr)?;
    let (checks, failing) = checks_of(statuses, project, head)?;
    let (notes, threads) = notes_of(comments)?;
    Ok(ChangeSnapshot {
        state,
        head: HeadSeen {
            id: head.to_string(),
            checks,
            failing,
        },
        review_decision: review_decision(final_pr)?,
        mergeable: mergeable_of(state, mergeability)?,
        threads,
        // Participant votes do not identify the head on which they were cast.
        reviews: Vec::new(),
        notes,
    })
}

fn unknown(error: ReadError) -> ChangeObservation {
    ChangeObservation::Unknown {
        kind: error.0,
        why: error.1,
        retry_after: error.2,
    }
}

impl Forge for BitbucketCloudForge {
    fn operation_bound(&self) -> Millis {
        cli::operation_bound(self.call_timeout)
    }

    fn parse(&self, url: &str) -> Observation<ChangeRef> {
        match parse_url(url) {
            Ok(reference) => Observation::Known(reference),
            Err(why) => Observation::unknown(why),
        }
    }

    fn whoami(&self, host: &str) -> Observation<String> {
        if !host.eq_ignore_ascii_case(HOST) {
            return Observation::unknown(format!("host {host} is not Bitbucket Cloud"));
        }
        let reply = match self.api(&format!("{API}/user"), self.call_timeout) {
            Ok(reply) => reply,
            Err((_, why)) => return Observation::unknown(why),
        };
        match body_of(&reply)
            .ok()
            .flatten()
            .and_then(|user| stable_identity(&user, "the current user").ok())
        {
            Some(id) => Observation::Known(id),
            None => Observation::unknown(
                "the authenticated client could not identify its Bitbucket Cloud user",
            ),
        }
    }

    fn change(&self, change: &ChangeRef) -> ChangeObservation {
        if change.kind != KIND || change.host != HOST {
            return ChangeObservation::unknown(
                Uncertainty::Unreadable,
                "the reference is not a Bitbucket Cloud pull request",
            );
        }
        let (workspace, repository) = match split_project(change) {
            Ok(parts) => parts,
            Err(why) => return ChangeObservation::unknown(Uncertainty::Unreadable, why),
        };
        let workspace = cli::encode_segment(workspace);
        let repository = cli::encode_segment(repository);
        let root = format!(
            "{API}/repositories/{workspace}/{repository}/pullrequests/{}",
            change.id
        );
        let mut budget = Budget::new(self.call_timeout);
        let first_reply = match self.get(&root, &mut budget) {
            Ok(reply) => reply,
            Err(error) => return unknown(error),
        };
        let first = match body_of(&first_reply) {
            Ok(Some(pr)) => pr,
            Ok(None) => {
                let repository_url = format!("{API}/repositories/{workspace}/{repository}");
                let reply = match self.get(&repository_url, &mut budget) {
                    Ok(reply) => reply,
                    Err(error) => return unknown(error),
                };
                return match body_of(&reply) {
                    Ok(Some(repo))
                        if repo["uuid"].as_str().is_some_and(|uuid| !uuid.is_empty())
                            && repo["full_name"]
                                .as_str()
                                .is_some_and(|name| name.eq_ignore_ascii_case(&change.project)) =>
                    {
                        ChangeObservation::Known(None)
                    }
                    Ok(_) => ChangeObservation::unknown(
                        Uncertainty::Refused,
                        "the pull request was not found and its repository could not be verified",
                    ),
                    Err(error) => unknown(error),
                };
            }
            Err(error) => return unknown(error),
        };
        let comments = match self.pages(
            format!("{root}/comments?pagelen={PAGE}"),
            &mut budget,
            "pull request comments",
        ) {
            Ok(values) => values,
            Err(error) => return unknown(error),
        };
        let statuses = match self.pages(
            format!("{root}/statuses?pagelen={PAGE}"),
            &mut budget,
            "pull request commit statuses",
        ) {
            Ok(values) => values,
            Err(error) => return unknown(error),
        };
        let mergeability = match self.pages(
            format!("{root}/mergeability/checks?pagelen={PAGE}"),
            &mut budget,
            "pull request mergeability checks",
        ) {
            Ok(values) => values,
            Err(error) => return unknown(error),
        };
        let final_reply = match self.get(&root, &mut budget) {
            Ok(reply) => reply,
            Err(error) => return unknown(error),
        };
        let final_pr = match body_of(&final_reply) {
            Ok(Some(pr)) => pr,
            Ok(None) => {
                return ChangeObservation::unknown(
                    Uncertainty::Incomplete,
                    "the pull request disappeared while being read",
                );
            }
            Err(error) => return unknown(error),
        };
        match snapshot(
            &change.project,
            &first,
            &final_pr,
            &statuses,
            &comments,
            &mergeability,
        ) {
            Ok(snapshot) => ChangeObservation::Known(Some(snapshot)),
            Err(why) => ChangeObservation::unknown(Uncertainty::Unreadable, why),
        }
    }

    fn mark_ready(
        &self,
        _change: &ChangeRef,
        _expect: &Expected,
        _intent_key: &str,
    ) -> Observation<EffectOutcome> {
        Observation::Known(EffectOutcome::Failed {
            why: "Bitbucket Cloud draft publication is unsupported: PUT draft=false has no atomic head-and-state precondition"
                .into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_hosted_cloud_pull_request_urls_are_accepted() {
        let reference =
            parse_url("https://bitbucket.org/Acme/Widgets/pull-requests/7/?tab=activity#comment-1")
                .unwrap();
        assert_eq!(reference.kind, KIND);
        assert_eq!(reference.host, HOST);
        assert_eq!(reference.project, "acme/widgets");
        assert_eq!(reference.id, "7");
        assert!(parse_url("http://bitbucket.org/acme/widgets/pull-requests/7").is_err());
        assert!(parse_url("https://stash.example/acme/widgets/pull-requests/7").is_err());
        assert!(parse_url("https://bitbucket.org/acme/widgets/pull/7").is_err());
    }

    #[test]
    fn only_documented_comment_author_types_are_placed() {
        assert_eq!(
            author(&serde_json::json!({"type": "user", "uuid": "{u}"})),
            Ok(("{u}".into(), false))
        );
        assert_eq!(
            author(&serde_json::json!({"type": "app_user", "account_id": "a"})),
            Ok(("a".into(), true))
        );
        assert!(
            author(&serde_json::json!({"type": "repository_access_token", "uuid": "{t}"})).is_err()
        );
    }

    fn comment(id: u64, parent: Option<u64>, inline: bool, deleted: bool) -> Value {
        serde_json::json!({
            "id": id,
            "created_on": "2026-10-26T12:00:00Z",
            "updated_on": "2026-10-26T12:00:00Z",
            "content": {"raw": format!("comment {id}")},
            "user": {"type": "user", "account_id": format!("u{id}")},
            "deleted": deleted,
            "pending": false,
            "parent": parent.map(|id| serde_json::json!({"id": id})),
            "inline": inline.then(|| serde_json::json!({"path": "src/lib.rs", "to": 7})),
            "resolution": Value::Null,
            "links": {"html": {"href": format!("https://bitbucket.org/acme/widgets/pull-requests/7#comment-{id}")}}
        })
    }

    #[test]
    fn ordinary_replies_are_comments_and_deleted_inline_roots_remain_structural() {
        let ordinary = notes_of(&[
            comment(1, None, false, false),
            comment(2, Some(1), false, false),
        ])
        .unwrap();
        assert!(ordinary.0.iter().all(|note| note.kind == NoteKind::Comment));

        let inline = notes_of(&[
            comment(3, None, true, true),
            comment(4, Some(3), false, false),
        ])
        .unwrap();
        assert_eq!(inline.0.len(), 1);
        assert_eq!(inline.0[0].kind, NoteKind::Inline);
        assert_eq!(inline.0[0].thread.as_deref(), Some("3"));
        assert_eq!(inline.1.unresolved, 1);
    }
}
