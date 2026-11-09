//! Azure DevOps Services pull requests through the authenticated `az` CLI.
//!
//! The adapter accepts the hosted `dev.azure.com` URL and its supported
//! `{organization}.visualstudio.com` aliases.  Its canonical host is the
//! organization-specific legacy hostname: that keeps `whoami` scoped to the
//! organization and keeps equal project/repository names in two organizations
//! from sharing a ChangeRequest identity.

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

pub const KIND: &str = "azure-devops";
pub const SERVICES_HOST: &str = "dev.azure.com";
const API_VERSION: &str = "7.1";
const POLICY_API_VERSION: &str = "7.1-preview.1";
const POLICY_PAGE_SIZE: usize = 1000;

#[derive(Debug, Clone)]
pub struct AzureDevopsForge {
    cli: OsString,
    call_timeout: Duration,
}

impl AzureDevopsForge {
    pub fn new(cli: impl Into<OsString>, call_timeout: Duration) -> AzureDevopsForge {
        AzureDevopsForge {
            cli: cli.into(),
            call_timeout,
        }
    }

    fn invoke(
        &self,
        organization: &str,
        area: &str,
        resource: &str,
        routes: &[(&str, &str)],
        query: &[(&str, String)],
        timeout: Duration,
    ) -> Result<Reply, (Uncertainty, String)> {
        let mut args = vec![
            "devops".into(),
            "invoke".into(),
            "--organization".into(),
            format!("https://dev.azure.com/{organization}"),
            "--area".into(),
            area.into(),
            "--resource".into(),
            resource.into(),
        ];
        if !routes.is_empty() {
            args.push("--route-parameters".into());
            args.extend(routes.iter().map(|(k, v)| format!("{k}={v}")));
        }
        if !query.is_empty() {
            args.push("--query-parameters".into());
            args.extend(query.iter().map(|(k, v)| format!("{k}={v}")));
        }
        let api_version = if resource == "policyEvaluations" {
            POLICY_API_VERSION
        } else {
            API_VERSION
        };
        args.extend([
            "--api-version".into(),
            api_version.into(),
            "--output".into(),
            "json".into(),
            "--only-show-errors".into(),
        ]);
        cli::call(&self.cli, &args, timeout)
    }
}

fn organization(host: &str) -> Option<&str> {
    host.strip_suffix(".visualstudio.com")
        .filter(|s| !s.is_empty() && !s.contains('.'))
}

fn split_project(change: &ChangeRef) -> Result<(&str, &str, &str), String> {
    let org = organization(&change.host).ok_or("host is not an organization identity")?;
    let (project, repository) = change
        .project
        .split_once('/')
        .ok_or("project is not project/repository")?;
    if project.is_empty() || repository.is_empty() || repository.contains('/') {
        return Err("project is not project/repository".into());
    }
    Ok((org, project, repository))
}

fn percent_decode(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let text = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(text, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn parse_url(url: &str) -> Result<ChangeRef, String> {
    let rest = url
        .trim()
        .split_once("://")
        .map(|(_, rest)| rest)
        .ok_or("the URL has no scheme")?;
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let mut parts = rest.split('/').filter(|p| !p.is_empty());
    let host = parts
        .next()
        .ok_or("the URL has no host")?
        .to_ascii_lowercase();
    let mut path: Vec<String> = parts
        .map(percent_decode)
        .collect::<Option<Vec<_>>>()
        .ok_or("the URL has an invalid percent escape")?;
    let org = if host == SERVICES_HOST {
        if path.len() != 6 {
            return Err("an Azure DevOps Services pull request URL is /organization/project/_git/repository/pullrequest/id".into());
        }
        path.remove(0)
    } else if let Some(org) = host.strip_suffix(".visualstudio.com") {
        if path
            .first()
            .is_some_and(|p| p.eq_ignore_ascii_case("DefaultCollection"))
        {
            path.remove(0);
        }
        org.to_string()
    } else {
        return Err("the URL is not on dev.azure.com or visualstudio.com".into());
    };
    if org.is_empty()
        || org.contains('.')
        || path.len() != 5
        || !path[1].eq_ignore_ascii_case("_git")
        || !path[3].eq_ignore_ascii_case("pullrequest")
        || path[0].is_empty()
        || path[2].is_empty()
        || path[4].parse::<u64>().is_err()
    {
        return Err("the URL does not name an Azure DevOps Services pull request".into());
    }
    Ok(ChangeRef {
        kind: KIND.into(),
        host: format!("{}.visualstudio.com", org.to_ascii_lowercase()),
        project: format!("{}/{}", path[0], path[2]),
        id: path[4].clone(),
    })
}

struct AzureAnswer {
    value: Value,
}

fn answer(reply: Reply) -> Result<AzureAnswer, (Uncertainty, String, Option<Millis>)> {
    let error_text = if reply.exit.is_some_and(|code| code != 0) {
        format!("{} {}", reply.stderr, reply.body).to_ascii_lowercase()
    } else {
        reply.stderr.to_ascii_lowercase()
    };
    if reply.status == 429
        || error_text.contains("429")
        || error_text.contains("rate limit")
        || error_text.contains("throttl")
    {
        let retry = retry_after_text(&error_text).or_else(|| reply.retry_after());
        return Err((
            Uncertainty::Throttled,
            "the forge asked to slow down".into(),
            retry,
        ));
    }
    if reply.status == 401
        || error_text.contains("401")
        || error_text.contains("az login")
        || error_text.contains("az devops login")
        || error_text.contains("not logged in")
        || error_text.contains("please run 'az login'")
    {
        return Err((
            Uncertainty::NotLoggedIn,
            "the Azure CLI has no usable login for this organization".into(),
            None,
        ));
    }
    let value: Value = serde_json::from_str(reply.body.trim()).map_err(|e| {
        let why = match reply.exit {
            Some(0) | None => format!("unreadable answer: {e}"),
            Some(code) => format!("az exited {code}: {}", reply.stderr),
        };
        (Uncertainty::Unreadable, why, None)
    })?;
    if reply.exit.is_some_and(|code| code != 0)
        || value["statusCode"].as_u64().is_some_and(|s| s >= 400)
    {
        return Err((
            Uncertainty::Refused,
            value["message"]
                .as_str()
                .unwrap_or("the forge refused the request")
                .to_string(),
            None,
        ));
    }
    Ok(AzureAnswer { value })
}

fn retry_after_text(text: &str) -> Option<Millis> {
    let text = text.to_ascii_lowercase();
    let (at, marker) = ["retry-after", "retry after"]
        .into_iter()
        .filter_map(|marker| text.find(marker).map(|at| (at, marker)))
        .min_by_key(|(at, _)| *at)?;
    let secs = text[at + marker.len()..]
        .split(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())?
        .parse::<u64>()
        .ok()?;
    Some(secs.saturating_mul(1000).max(1000))
}

fn unknown((kind, why, retry_after): (Uncertainty, String, Option<Millis>)) -> ChangeObservation {
    ChangeObservation::Unknown {
        kind,
        why,
        retry_after,
    }
}

fn collection(value: &Value, name: &str) -> Result<Vec<Value>, String> {
    let items = value["value"]
        .as_array()
        .ok_or_else(|| format!("{name} has no value array"))?;
    let count = value["count"]
        .as_u64()
        .ok_or_else(|| format!("{name} has no count"))?;
    if count != items.len() as u64 {
        return Err(format!(
            "{name} was truncated ({count} announced, {} returned)",
            items.len()
        ));
    }
    Ok(items.clone())
}

fn state_of(pr: &Value) -> Result<ChangeState, String> {
    let is_draft = pr["isDraft"]
        .as_bool()
        .ok_or("pull request has no isDraft flag")?;
    // Only an active pull request is a draft; an abandoned draft keeps the flag.
    match pr["status"].as_str() {
        Some("active") if is_draft => Ok(ChangeState::Draft),
        Some("active") => Ok(ChangeState::Open),
        Some("abandoned") => Ok(ChangeState::Closed),
        Some("completed") => Ok(ChangeState::Merged),
        other => Err(format!("status {other:?} cannot be placed")),
    }
}

fn latest_iteration(iterations: &[Value]) -> Result<(u64, &Value), String> {
    let mut latest = None;
    for iteration in iterations {
        let id = iteration["id"]
            .as_u64()
            .ok_or("a pull request iteration has no numeric id")?;
        if latest.is_none_or(|(latest_id, _)| id > latest_id) {
            latest = Some((id, iteration));
        }
    }
    latest.ok_or_else(|| "pull request has no iteration".into())
}

fn snapshot(
    expected_project: &str,
    expected_repository: &str,
    pr: &Value,
    iterations: &[Value],
    statuses: &[Value],
    policies: &[Value],
    threads: &[Value],
) -> Result<ChangeSnapshot, String> {
    let project = pr["repository"]["project"]["name"]
        .as_str()
        .ok_or("pull request has no project name")?;
    let repository = pr["repository"]["name"]
        .as_str()
        .ok_or("pull request has no repository name")?;
    if !project.eq_ignore_ascii_case(expected_project)
        || !repository.eq_ignore_ascii_case(expected_repository)
    {
        return Err("the organization-level pull request id resolved to another repository".into());
    }
    let (_, latest) = latest_iteration(iterations)?;
    let head = latest["sourceRefCommit"]["commitId"]
        .as_str()
        .ok_or("latest iteration has no sourceRefCommit.commitId")?
        .to_string();
    let mut failed = Vec::new();
    let mut pending = false;
    let mut passed = false;
    for status in statuses {
        let name = match (
            status["context"]["genre"].as_str(),
            status["context"]["name"].as_str(),
        ) {
            (Some(genre), Some(name)) if !genre.is_empty() => format!("{genre}/{name}"),
            (_, Some(name)) => name.to_string(),
            _ => return Err("a pull request status has no context name".into()),
        };
        match status["state"].as_str() {
            Some("failed" | "error") => failed.push(FailingCheck {
                name,
                url: status["targetUrl"].as_str().map(str::to_string),
            }),
            Some("pending" | "notSet") => pending = true,
            Some("succeeded") => passed = true,
            Some("notApplicable") => {}
            other => return Err(format!("status state {other:?} cannot be placed")),
        }
    }
    for policy in policies {
        let blocking = policy["configuration"]["isBlocking"]
            .as_bool()
            .ok_or("a policy has no isBlocking flag")?;
        let enabled = policy["configuration"]["isEnabled"]
            .as_bool()
            .ok_or("a policy has no isEnabled flag")?;
        if !blocking || !enabled {
            continue;
        }
        let name = policy["configuration"]["type"]["displayName"]
            .as_str()
            .unwrap_or("policy")
            .to_string();
        match policy["status"].as_str() {
            Some("rejected" | "broken") => failed.push(FailingCheck {
                name,
                url: policy["configuration"]["url"].as_str().map(str::to_string),
            }),
            Some("queued" | "running") => pending = true,
            Some("approved") => passed = true,
            Some("notApplicable") => {}
            other => return Err(format!("policy status {other:?} cannot be placed")),
        }
    }
    let checks = if !failed.is_empty() {
        Checks::Failed
    } else if pending {
        Checks::Pending
    } else if passed {
        Checks::Passed
    } else {
        Checks::None
    };
    let votes: Vec<i64> = pr["reviewers"]
        .as_array()
        .ok_or("pull request has no reviewers array")?
        .iter()
        .map(|r| {
            let vote = r["vote"].as_i64().ok_or("a reviewer has no vote")?;
            matches!(vote, -10 | -5 | 0 | 5 | 10)
                .then_some(vote)
                .ok_or("a reviewer vote cannot be placed")
        })
        .collect::<Result<_, _>>()?;
    let review_decision = if votes.iter().any(|v| *v == -10 || *v == -5) {
        ReviewDecision::ChangesRequested
    } else if votes.iter().any(|v| *v == 10 || *v == 5) {
        ReviewDecision::Approved
    } else {
        ReviewDecision::None
    };
    let state = state_of(pr)?;
    let mergeable = if state == ChangeState::Draft {
        Mergeable {
            value: Some(false),
            reason: Some("draft".into()),
        }
    } else {
        match pr["mergeStatus"].as_str() {
            Some("succeeded") => Mergeable {
                value: Some(true),
                reason: None,
            },
            Some("conflicts") => Mergeable {
                value: Some(false),
                reason: Some("conflicts".into()),
            },
            Some("rejectedByPolicy") => Mergeable {
                value: Some(false),
                reason: Some("policy".into()),
            },
            Some("failure") => Mergeable {
                value: Some(false),
                reason: Some("failure".into()),
            },
            Some("queued" | "notSet") | None => Mergeable {
                value: None,
                reason: None,
            },
            other => return Err(format!("mergeStatus {other:?} cannot be placed")),
        }
    };
    let web = pr["remoteUrl"].as_str().unwrap_or("");
    let mut notes = Vec::new();
    let mut unresolved = 0;
    for thread in threads {
        if thread["isDeleted"] == true {
            continue;
        }
        let thread_id = thread["id"].as_u64().ok_or("a thread has no id")?;
        let inline = thread["threadContext"]["filePath"].as_str().is_some();
        let resolved = match thread["status"].as_str() {
            Some("fixed" | "wontFix" | "closed" | "byDesign") => true,
            Some("active" | "pending") => false,
            None if !inline => false,
            None => return Err("an inline thread has no resolution status".into()),
            Some("unknown") => return Err("a thread has unknown resolution status".into()),
            Some(other) => return Err(format!("thread status {other:?} cannot be placed")),
        };
        if inline && !resolved {
            unresolved += 1;
        }
        for comment in thread["comments"]
            .as_array()
            .ok_or("a thread has no comments")?
        {
            if comment["isDeleted"] == true {
                continue;
            }
            match comment["commentType"].as_str() {
                Some("system" | "codeChange") => continue,
                Some("text") => {}
                Some("unknown") | None => return Err("a comment has an unknown type".into()),
                Some(other) => return Err(format!("comment type {other:?} cannot be placed")),
            }
            let id = comment["id"].as_u64().ok_or("a comment has no id")?;
            let at = comment["publishedDate"]
                .as_str()
                .and_then(cli::iso_millis)
                .ok_or("publishedDate is not a timestamp")?;
            let edited_at = comment["lastContentUpdatedDate"]
                .as_str()
                .and_then(cli::iso_millis)
                .filter(|t| *t != at);
            notes.push(Note {
                id: id.to_string(),
                kind: if inline {
                    NoteKind::Inline
                } else {
                    NoteKind::Comment
                },
                author: comment["author"]["id"]
                    .as_str()
                    .ok_or("a text comment has no author id")?
                    .to_string(),
                bot: false,
                thread: inline.then(|| thread_id.to_string()),
                resolved: inline.then_some(resolved),
                path: thread["threadContext"]["filePath"]
                    .as_str()
                    .map(str::to_string),
                line: thread["threadContext"]["rightFileStart"]["line"].as_u64(),
                at,
                edited_at,
                url: web.to_string(),
                body: comment["content"]
                    .as_str()
                    .ok_or("a text comment has no content")?
                    .to_string(),
            });
        }
    }
    notes.sort_by_key(|n| (n.at, n.id.clone()));
    Ok(ChangeSnapshot {
        state,
        head: HeadSeen {
            id: head,
            checks,
            failing: failed,
        },
        review_decision,
        mergeable,
        threads: Threads { unresolved },
        // Azure DevOps reviewer votes do not identify the head on which the
        // vote was cast, so they cannot satisfy Fleet's Review contract.
        reviews: Vec::new(),
        notes,
    })
}

impl Forge for AzureDevopsForge {
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
        let Some(org) = organization(host) else {
            return Observation::unknown(format!(
                "host {host} is not an Azure DevOps Services organization"
            ));
        };
        let reply = match self.invoke(
            org,
            "profile",
            "profiles",
            &[("id", "me")],
            &[],
            self.call_timeout,
        ) {
            Ok(reply) => reply,
            Err((_, why)) => return Observation::unknown(why),
        };
        match answer(reply)
            .ok()
            .and_then(|a| a.value["id"].as_str().map(str::to_string))
        {
            Some(id) => Observation::Known(id),
            None => Observation::unknown("the organization-scoped profile answer has no id"),
        }
    }

    fn change(&self, change: &ChangeRef) -> ChangeObservation {
        let (org, project, repository) = match split_project(change) {
            Ok(parts) => parts,
            Err(why) => return ChangeObservation::unknown(Uncertainty::Unreadable, why),
        };
        let mut budget = Budget::new(self.call_timeout);
        let base = [
            ("project", project),
            ("repositoryId", repository),
            ("pullRequestId", change.id.as_str()),
        ];
        let Some(timeout) = budget.next_timeout() else {
            return ChangeObservation::unknown(
                Uncertainty::Incomplete,
                "no budget for the pull request read",
            );
        };
        let pr_reply = match self.invoke(org, "git", "pullRequests", &base, &[], timeout) {
            Ok(reply) => reply,
            Err((kind, why)) => return ChangeObservation::unknown(kind, why),
        };
        let not_found = serde_json::from_str::<Value>(pr_reply.body.trim())
            .ok()
            .and_then(|v| v["statusCode"].as_u64())
            == Some(404);
        let pr = match answer(pr_reply) {
            Ok(answer) => answer.value,
            Err((_, _, _)) if not_found => {
                let Some(timeout) = budget.next_timeout() else {
                    return ChangeObservation::unknown(
                        Uncertainty::Incomplete,
                        "no budget to disambiguate the pull request's absence",
                    );
                };
                let repo_routes = [("project", project), ("repositoryId", repository)];
                let repo = self
                    .invoke(org, "git", "repositories", &repo_routes, &[], timeout)
                    .ok()
                    .and_then(|reply| answer(reply).ok())
                    .map(|answer| answer.value);
                return if repo.is_some_and(|repo| repo["id"].is_string()) {
                    ChangeObservation::Known(None)
                } else {
                    ChangeObservation::unknown(
                        Uncertainty::Refused,
                        "the pull request was not found and its repository could not be read",
                    )
                };
            }
            Err((kind, why, retry_after)) => {
                return ChangeObservation::Unknown {
                    kind,
                    why,
                    retry_after,
                };
            }
        };
        let mut get = |area: &str,
                       resource: &str,
                       routes: &[(&str, &str)],
                       query: &[(&str, String)]|
         -> Result<Value, (Uncertainty, String, Option<Millis>)> {
            let Some(timeout) = budget.next_timeout() else {
                return Err((
                    Uncertainty::Incomplete,
                    "the bound ran out before every Azure DevOps collection was read".into(),
                    None,
                ));
            };
            let reply = self
                .invoke(org, area, resource, routes, query, timeout)
                .map_err(|(kind, why)| (kind, why, None))?;
            answer(reply).map(|a| a.value)
        };
        if pr["count"].is_number() || pr["value"].is_array() {
            return ChangeObservation::unknown(
                Uncertainty::Incomplete,
                "the pull request answer was a truncated collection",
            );
        }
        let iteration_doc = match get("git", "pullRequestIterations", &base, &[]) {
            Ok(v) => v,
            Err(e) => return unknown(e),
        };
        let iterations = match collection(&iteration_doc, "iterations") {
            Ok(v) => v,
            Err(why) => return ChangeObservation::unknown(Uncertainty::Incomplete, why),
        };
        let iteration_id = match latest_iteration(&iterations) {
            Ok((id, _)) => id,
            Err(why) => {
                return ChangeObservation::unknown(Uncertainty::Unreadable, why);
            }
        };
        let iteration = iteration_id.to_string();
        let mut status_routes = base.to_vec();
        status_routes.push(("iterationId", iteration.as_str()));
        let status_doc = match get("git", "pullRequestIterationStatuses", &status_routes, &[]) {
            Ok(v) => v,
            Err(e) => return unknown(e),
        };
        let statuses = match collection(&status_doc, "statuses") {
            Ok(v) => v,
            Err(why) => return ChangeObservation::unknown(Uncertainty::Incomplete, why),
        };
        let Some(project_id) = pr["repository"]["project"]["id"].as_str() else {
            return ChangeObservation::unknown(
                Uncertainty::Unreadable,
                "pull request has no project id",
            );
        };
        let artifact = format!(
            "vstfs:///CodeReview/CodeReviewId/{project_id}/{}",
            change.id
        );
        let policy_doc = match get(
            "policy",
            "policyEvaluations",
            &[("project", project)],
            &[
                ("artifactId", artifact),
                ("$top", POLICY_PAGE_SIZE.to_string()),
            ],
        ) {
            Ok(v) => v,
            Err(e) => return unknown(e),
        };
        let policies = match collection(&policy_doc, "policy evaluations") {
            Ok(v) => v,
            Err(why) => return ChangeObservation::unknown(Uncertainty::Incomplete, why),
        };
        if policies.len() == POLICY_PAGE_SIZE {
            return ChangeObservation::unknown(
                Uncertainty::Incomplete,
                "policy evaluations filled the bounded page; another page may exist",
            );
        }
        let thread_doc = match get("git", "pullRequestThreads", &base, &[]) {
            Ok(v) => v,
            Err(e) => return unknown(e),
        };
        let threads = match collection(&thread_doc, "threads") {
            Ok(v) => v,
            Err(why) => return ChangeObservation::unknown(Uncertainty::Incomplete, why),
        };
        match snapshot(
            project,
            repository,
            &pr,
            &iterations,
            &statuses,
            &policies,
            &threads,
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
            why: "Azure DevOps draft publication is unsupported: the provider exposes no atomic head-and-state precondition"
                .into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosted_and_legacy_urls_have_one_organization_scoped_identity() {
        let a =
            parse_url("https://dev.azure.com/Wingtip/Project/_git/Widgets/pullrequest/7").unwrap();
        let b = parse_url(
            "https://wingtip.visualstudio.com/DefaultCollection/Project/_git/Widgets/pullrequest/7",
        )
        .unwrap();
        assert_eq!(a, b);
        assert_ne!(
            a.canonical(),
            parse_url("https://dev.azure.com/other/Project/_git/Widgets/pullrequest/7")
                .unwrap()
                .canonical()
        );
        assert!(parse_url("https://server.example/Project/_git/Widgets/pullrequest/7").is_err());
    }

    #[test]
    fn textual_retry_after_uses_the_standard_header_spelling() {
        assert_eq!(retry_after_text("429 Retry-After: 45"), Some(45_000));
    }
}
