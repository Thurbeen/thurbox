//! The forge contract served by `gh` for GitHub and GitHub Enterprise: one
//! GraphQL query, paginated over reviews, comments and review threads with
//! bounded `gh api graphql` processes, mapped onto Fleet's vocabulary by the
//! adapter truth table in the ChangeRequest interface page; and one write,
//! `markPullRequestReadyForReview`, after a fresh read of the head and the
//! draft state.

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

pub const KIND: &str = "github";

/// One query for everything; the `with*` flags let a continuation call fetch
/// only the connections that still have pages.
const QUERY: &str = "query($owner: String!, $name: String!, $number: Int!, $withCore: Boolean!, $withReviews: Boolean!, $withComments: Boolean!, $withThreads: Boolean!, $reviewsAfter: String, $commentsAfter: String, $threadsAfter: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      isDraft @include(if: $withCore)
      state @include(if: $withCore)
      reviewDecision @include(if: $withCore)
      mergeable @include(if: $withCore)
      mergeStateStatus @include(if: $withCore)
      headRefOid @include(if: $withCore)
      commits(last: 1) @include(if: $withCore) { nodes { commit { oid statusCheckRollup { contexts(first: 100) { pageInfo { hasNextPage } nodes { __typename ... on CheckRun { name status conclusion detailsUrl } ... on StatusContext { context state targetUrl } } } } } } }
      reviews(first: 100, after: $reviewsAfter) @include(if: $withReviews) { pageInfo { hasNextPage endCursor } nodes { id state body url createdAt lastEditedAt author { login __typename } commit { oid } } }
      comments(first: 100, after: $commentsAfter) @include(if: $withComments) { pageInfo { hasNextPage endCursor } nodes { id body url createdAt lastEditedAt author { login __typename } } }
      reviewThreads(first: 100, after: $threadsAfter) @include(if: $withThreads) { pageInfo { hasNextPage endCursor } nodes { id isResolved comments(first: 100) { pageInfo { hasNextPage } nodes { id body url path line originalLine createdAt lastEditedAt author { login __typename } } } } }
    }
  }
}";

/// The fresh read before a mark-ready: only what the decision rests on.
const HEAD_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) { id isDraft state headRefOid }
  }
}";

/// The one mutation. The intent key rides as `clientMutationId`, so the
/// forge's own record of the mutation names Fleet's intent.
const READY_MUTATION: &str = "mutation($id: ID!, $intent: String) {
  markPullRequestReadyForReview(input: {pullRequestId: $id, clientMutationId: $intent}) {
    clientMutationId
    pullRequest { isDraft }
  }
}";

#[derive(Debug, Clone)]
pub struct GithubForge {
    host: String,
    cli: OsString,
    call_timeout: Duration,
}

impl GithubForge {
    pub fn new(host: &str, cli: impl Into<OsString>, call_timeout: Duration) -> GithubForge {
        GithubForge {
            host: host.to_ascii_lowercase(),
            cli: cli.into(),
            call_timeout,
        }
    }

    fn graphql(
        &self,
        query: &str,
        vars: &[(&str, Var)],
        timeout: Duration,
    ) -> Result<Reply, (Uncertainty, String)> {
        let mut args = vec![
            "api".to_string(),
            "graphql".to_string(),
            "--hostname".to_string(),
            self.host.clone(),
            "--include".to_string(),
            "-f".to_string(),
            format!("query={query}"),
        ];
        for (k, v) in vars {
            match v {
                Var::Str(s) => args.extend(["-f".to_string(), format!("{k}={s}")]),
                Var::Typed(s) => args.extend(["-F".to_string(), format!("{k}={s}")]),
            }
        }
        cli::call(&self.cli, &args, timeout)
    }
}

enum Var {
    /// A string variable (`-f`).
    Str(String),
    /// A typed variable (`-F`): an integer or a boolean.
    Typed(String),
}

/// What one observation has read so far.
#[derive(Default)]
struct Pages {
    core: Option<Value>,
    reviews: Vec<Value>,
    comments: Vec<Value>,
    threads: Vec<Value>,
    reviews_after: Option<String>,
    comments_after: Option<String>,
    threads_after: Option<String>,
    reviews_done: bool,
    comments_done: bool,
    threads_done: bool,
}

/// The JSON of a GraphQL answer, or why it is Unknown; `Ok(None)` is proven
/// absence of the pull request.
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
    let v: Value = serde_json::from_str(reply.body.trim()).map_err(|e| {
        (
            Uncertainty::Unreadable,
            match reply.exit {
                Some(0) | None => format!("unreadable answer: {e}"),
                Some(code) => format!("gh exited {code}: {}", reply.stderr),
            },
        )
    })?;
    let errors = v["errors"].as_array().cloned().unwrap_or_default();
    if errors.iter().any(|e| e["type"] == "RATE_LIMITED") {
        return Err((
            Uncertainty::Throttled,
            "the forge's rate limit is exhausted".into(),
        ));
    }
    let repo = &v["data"]["repository"];
    if repo.is_object() && repo["pullRequest"].is_null() {
        // The repository answered and has no such pull request.
        return Ok(None);
    }
    if let Some(e) = errors.first() {
        let kind = if e["type"] == "NOT_FOUND" || e["type"] == "FORBIDDEN" {
            Uncertainty::Refused
        } else {
            Uncertainty::Unreadable
        };
        return Err((
            kind,
            e["message"]
                .as_str()
                .unwrap_or("the forge reported an error")
                .to_string(),
        ));
    }
    if reply.status >= 400 && reply.status != 0 {
        return Err((
            Uncertainty::Refused,
            format!(
                "HTTP {}: {}",
                reply.status,
                v["message"].as_str().unwrap_or("")
            ),
        ));
    }
    if !repo["pullRequest"].is_object() {
        return Err((
            Uncertainty::Refused,
            "the repository was not found or is not accessible".into(),
        ));
    }
    Ok(Some(v["data"]["repository"]["pullRequest"].clone()))
}

fn page(conn: &Value, nodes: &mut Vec<Value>, after: &mut Option<String>, done: &mut bool) {
    nodes.extend(conn["nodes"].as_array().cloned().unwrap_or_default());
    if conn["pageInfo"]["hasNextPage"] == true {
        *after = conn["pageInfo"]["endCursor"].as_str().map(str::to_string);
    } else {
        *done = true;
    }
}

impl Forge for GithubForge {
    fn operation_bound(&self) -> Millis {
        cli::operation_bound(self.call_timeout)
    }

    /// `https://<host>/<owner>/<repo>/pull/<number>`, fragments, queries and
    /// trailing slashes dropped.
    fn parse(&self, url: &str) -> Observation<ChangeRef> {
        let Some((host, project, id)) = cli::parse_change_url(url) else {
            return Observation::unknown("the URL does not name a pull request");
        };
        if host != self.host {
            return Observation::unknown(format!(
                "the URL names {host}, not the configured host {}",
                self.host
            ));
        }
        if project.split('/').count() != 2 || !url.contains("/pull") {
            return Observation::unknown(
                "a GitHub pull request URL is <owner>/<repo>/pull/<number>",
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
        let reply = match self.graphql("query { viewer { login } }", &[], self.call_timeout) {
            Ok(r) => r,
            Err((_, why)) => return Observation::unknown(why),
        };
        let v: Value = match serde_json::from_str(reply.body.trim()) {
            Ok(v) => v,
            Err(e) => return Observation::unknown(format!("unreadable answer: {e}")),
        };
        match v["data"]["viewer"]["login"].as_str() {
            Some(login) => Observation::Known(login.to_string()),
            None => Observation::unknown("no login in the answer"),
        }
    }

    fn change(&self, change: &ChangeRef) -> ChangeObservation {
        let Some((owner, name)) = change.project.split_once('/') else {
            return ChangeObservation::unknown(
                Uncertainty::Unreadable,
                "project is not owner/repo",
            );
        };
        let mut budget = Budget::new(self.call_timeout);
        let mut pages = Pages::default();
        loop {
            let Some(timeout) = budget.next_timeout() else {
                return ChangeObservation::unknown(
                    Uncertainty::Incomplete,
                    "the bound ran out before every page was read",
                );
            };
            let first = pages.core.is_none();
            let mut vars = vec![
                ("owner", Var::Str(owner.to_string())),
                ("name", Var::Str(name.to_string())),
                ("number", Var::Typed(change.id.clone())),
                ("withCore", Var::Typed(first.to_string())),
                ("withReviews", Var::Typed((!pages.reviews_done).to_string())),
                (
                    "withComments",
                    Var::Typed((!pages.comments_done).to_string()),
                ),
                ("withThreads", Var::Typed((!pages.threads_done).to_string())),
            ];
            for (k, v) in [
                ("reviewsAfter", &pages.reviews_after),
                ("commentsAfter", &pages.comments_after),
                ("threadsAfter", &pages.threads_after),
            ] {
                if let Some(c) = v {
                    vars.push((k, Var::Str(c.clone())));
                }
            }
            let reply = match self.graphql(QUERY, &vars, timeout) {
                Ok(r) => r,
                Err((kind, why)) => return ChangeObservation::unknown(kind, why),
            };
            let retry_after = reply.retry_after();
            let pr = match body_of(&reply) {
                Ok(Some(pr)) => pr,
                Ok(None) => return ChangeObservation::Known(None),
                Err((kind, why)) => {
                    return ChangeObservation::Unknown {
                        kind,
                        why,
                        retry_after: if kind == Uncertainty::Throttled {
                            retry_after
                        } else {
                            None
                        },
                    };
                }
            };
            if first {
                pages.core = Some(pr.clone());
            }
            if !pages.reviews_done {
                page(
                    &pr["reviews"],
                    &mut pages.reviews,
                    &mut pages.reviews_after,
                    &mut pages.reviews_done,
                );
            }
            if !pages.comments_done {
                page(
                    &pr["comments"],
                    &mut pages.comments,
                    &mut pages.comments_after,
                    &mut pages.comments_done,
                );
            }
            if !pages.threads_done {
                page(
                    &pr["reviewThreads"],
                    &mut pages.threads,
                    &mut pages.threads_after,
                    &mut pages.threads_done,
                );
            }
            if pages.reviews_done && pages.comments_done && pages.threads_done {
                break;
            }
        }
        let core = pages.core.take().unwrap_or(Value::Null);
        match snapshot(&core, &pages.reviews, &pages.comments, &pages.threads) {
            Ok(s) => ChangeObservation::Known(Some(s)),
            Err(why) => ChangeObservation::unknown(Uncertainty::Unreadable, why),
        }
    }

    /// Read `isDraft`, `state` and `headRefOid` afresh; refuse with `Moved`
    /// when they are not what the decision expected; else run the
    /// ready-for-review mutation. Everything before the mutation that goes
    /// wrong is a Known refusal, since nothing was mutated; past it, only an
    /// answer the adapter can read is Known.
    fn mark_ready(
        &self,
        change: &ChangeRef,
        expect: &Expected,
        intent_key: &str,
    ) -> Observation<EffectOutcome> {
        let Some((owner, name)) = change.project.split_once('/') else {
            return failed("project is not owner/repo");
        };
        let vars = [
            ("owner", Var::Str(owner.to_string())),
            ("name", Var::Str(name.to_string())),
            ("number", Var::Typed(change.id.clone())),
        ];
        let reply = match self.graphql(HEAD_QUERY, &vars, self.call_timeout) {
            Ok(r) => r,
            Err((_, why)) => return failed(format!("fresh read: {why}")),
        };
        let pr = match body_of(&reply) {
            Ok(Some(pr)) => pr,
            Ok(None) => return failed("the pull request no longer exists"),
            Err((_, why)) => return failed(format!("fresh read: {why}")),
        };
        let (head, state) = match head_and_state(&pr) {
            Ok(x) => x,
            Err(why) => return failed(format!("fresh read: {why}")),
        };
        if head != expect.head || state != expect.state {
            return Observation::Known(EffectOutcome::Moved { head, state });
        }
        let Some(id) = pr["id"].as_str() else {
            return failed("fresh read: the pull request has no id");
        };
        let vars = [
            ("id", Var::Str(id.to_string())),
            ("intent", Var::Str(intent_key.to_string())),
        ];
        match self.graphql(READY_MUTATION, &vars, self.call_timeout) {
            Ok(reply) => mutation_outcome(&reply),
            // The process ran out of time: the mutation may have landed.
            Err((Uncertainty::Timeout, why)) => Observation::unknown(why),
            Err((_, why)) => failed(why),
        }
    }
}

fn failed(why: impl Into<String>) -> Observation<EffectOutcome> {
    Observation::Known(EffectOutcome::Failed { why: why.into() })
}

/// The state of a pull request. Only an open pull request is a draft: GitHub
/// keeps `isDraft` on a draft that is closed without being marked ready.
fn state_of(pr: &Value) -> Result<ChangeState, String> {
    match pr["state"].as_str() {
        Some("OPEN") if pr["isDraft"] == true => Ok(ChangeState::Draft),
        Some("OPEN") => Ok(ChangeState::Open),
        Some("CLOSED") => Ok(ChangeState::Closed),
        Some("MERGED") => Ok(ChangeState::Merged),
        other => Err(format!("state {other:?} cannot be placed")),
    }
}

/// The head and state of a fresh read, by the same rule as the snapshot.
fn head_and_state(pr: &Value) -> Result<(String, ChangeState), String> {
    let state = state_of(pr)?;
    let head = pr["headRefOid"]
        .as_str()
        .ok_or("no headRefOid")?
        .to_string();
    Ok((head, state))
}

/// What the mutation's answer means. A refusal the forge spelled out is
/// Known; an answer that cannot be read is Unknown, because the mutation
/// may have landed.
pub fn mutation_outcome(reply: &Reply) -> Observation<EffectOutcome> {
    if reply.throttled() {
        return failed(format!(
            "the forge asked to slow down (HTTP {})",
            reply.status
        ));
    }
    if reply.status == 401 {
        return failed("HTTP 401: the CLI has no login for this host");
    }
    let v: Value = match serde_json::from_str(reply.body.trim()) {
        Ok(v) => v,
        Err(e) => {
            return Observation::unknown(format!(
                "the mutation's answer could not be read ({e}); it may have landed"
            ));
        }
    };
    let errors = v["errors"].as_array().cloned().unwrap_or_default();
    if let Some(e) = errors.first() {
        let message = e["message"]
            .as_str()
            .unwrap_or("the forge reported an error");
        let lower = message.to_ascii_lowercase();
        if lower.contains("not a draft") || lower.contains("not in draft") {
            // Someone took it out of draft inside the window: the outcome
            // Fleet wanted, by another hand.
            return Observation::Known(EffectOutcome::MarkedReady { already: true });
        }
        return failed(message.to_string());
    }
    match v["data"]["markPullRequestReadyForReview"]["pullRequest"]["isDraft"] {
        Value::Bool(false) => Observation::Known(EffectOutcome::MarkedReady { already: false }),
        Value::Bool(true) => failed("the forge left the pull request a draft"),
        _ => {
            Observation::unknown("the mutation's answer names no pull request; it may have landed")
        }
    }
}

fn author(v: &Value) -> (String, bool) {
    let a = &v["author"];
    (
        a["login"].as_str().unwrap_or("ghost").to_string(),
        a["__typename"] == "Bot",
    )
}

fn millis(v: &Value, field: &str) -> Result<Millis, String> {
    v[field]
        .as_str()
        .and_then(cli::iso_millis)
        .ok_or_else(|| format!("{field} is not a timestamp"))
}

fn edited(v: &Value) -> Result<Option<Millis>, String> {
    match v["lastEditedAt"].as_str() {
        None => Ok(None),
        Some(s) => cli::iso_millis(s)
            .map(Some)
            .ok_or_else(|| "lastEditedAt is not a timestamp".to_string()),
    }
}

/// The truth table: GitHub's answer in Fleet's words, or why it cannot be
/// placed, which makes the whole observation Unknown.
pub fn snapshot(
    core: &Value,
    reviews: &[Value],
    comments: &[Value],
    threads: &[Value],
) -> Result<ChangeSnapshot, String> {
    let state = state_of(core)?;
    let head_id = core["headRefOid"]
        .as_str()
        .ok_or("no headRefOid")?
        .to_string();
    let rollup = &core["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"];
    if rollup["pageInfo"]["hasNextPage"] == true {
        return Err("more than 100 checks on the head; the read is incomplete".into());
    }
    let (mut failed, mut pending, mut passed) = (Vec::new(), false, false);
    for c in rollup["nodes"].as_array().into_iter().flatten() {
        let (name, url, verdict) = if c["__typename"] == "CheckRun" {
            let verdict = match (c["status"].as_str(), c["conclusion"].as_str()) {
                (Some("COMPLETED"), Some("SUCCESS")) => "passed",
                (
                    Some("COMPLETED"),
                    Some(
                        "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED"
                        | "STARTUP_FAILURE",
                    ),
                ) => "failed",
                (Some("COMPLETED"), _) => "absent",
                (Some(_), _) => "pending",
                (None, _) => return Err("a check run without a status".into()),
            };
            (c["name"].clone(), c["detailsUrl"].clone(), verdict)
        } else {
            let verdict = match c["state"].as_str() {
                Some("SUCCESS") => "passed",
                Some("ERROR" | "FAILURE") => "failed",
                Some("PENDING" | "EXPECTED") => "pending",
                other => return Err(format!("status state {other:?} cannot be placed")),
            };
            (c["context"].clone(), c["targetUrl"].clone(), verdict)
        };
        match verdict {
            "failed" => failed.push(FailingCheck {
                name: name.as_str().unwrap_or("").to_string(),
                url: url.as_str().map(str::to_string),
            }),
            "pending" => pending = true,
            "passed" => passed = true,
            _ => {}
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
    let review_decision = match core["reviewDecision"].as_str() {
        Some("APPROVED") => ReviewDecision::Approved,
        Some("CHANGES_REQUESTED") => ReviewDecision::ChangesRequested,
        _ => ReviewDecision::None,
    };
    let mergeable = match core["mergeable"].as_str() {
        Some("CONFLICTING") => Mergeable {
            value: Some(false),
            reason: Some("conflicts".into()),
        },
        Some("UNKNOWN") | None => Mergeable {
            value: None,
            reason: None,
        },
        Some("MERGEABLE") => match core["mergeStateStatus"].as_str() {
            Some(s @ ("BLOCKED" | "BEHIND" | "UNSTABLE" | "DRAFT")) => Mergeable {
                value: Some(false),
                reason: Some(s.to_ascii_lowercase()),
            },
            Some("DIRTY") => Mergeable {
                value: Some(false),
                reason: Some("conflicts".into()),
            },
            _ => Mergeable {
                value: Some(true),
                reason: None,
            },
        },
        Some(other) => return Err(format!("mergeable {other:?} cannot be placed")),
    };
    let mut out_reviews = Vec::new();
    let mut notes = Vec::new();
    for r in reviews {
        let state = match r["state"].as_str() {
            Some("APPROVED") => ReviewState::Approved,
            Some("CHANGES_REQUESTED") => ReviewState::ChangesRequested,
            Some("COMMENTED") => ReviewState::Commented,
            Some("DISMISSED" | "PENDING") => continue,
            other => return Err(format!("review state {other:?} cannot be placed")),
        };
        let id = r["id"]
            .as_str()
            .ok_or("a review without an id")?
            .to_string();
        let (login, bot) = author(r);
        let head = r["commit"]["oid"].as_str().unwrap_or("").to_string();
        if head == head_id {
            out_reviews.push(Review {
                id: id.clone(),
                author: login.clone(),
                bot,
                state,
                head,
            });
        }
        notes.push(Note {
            id,
            kind: NoteKind::Review,
            author: login,
            bot,
            thread: None,
            resolved: None,
            path: None,
            line: None,
            at: millis(r, "createdAt")?,
            edited_at: edited(r)?,
            url: r["url"].as_str().unwrap_or("").to_string(),
            body: r["body"].as_str().unwrap_or("").to_string(),
        });
    }
    for c in comments {
        let (login, bot) = author(c);
        notes.push(Note {
            id: c["id"]
                .as_str()
                .ok_or("a comment without an id")?
                .to_string(),
            kind: NoteKind::Comment,
            author: login,
            bot,
            thread: None,
            resolved: None,
            path: None,
            line: None,
            at: millis(c, "createdAt")?,
            edited_at: edited(c)?,
            url: c["url"].as_str().unwrap_or("").to_string(),
            body: c["body"].as_str().unwrap_or("").to_string(),
        });
    }
    let mut unresolved = 0;
    for t in threads {
        let thread = t["id"]
            .as_str()
            .ok_or("a thread without an id")?
            .to_string();
        let resolved = t["isResolved"]
            .as_bool()
            .ok_or("a thread without isResolved")?;
        if !resolved {
            unresolved += 1;
        }
        if t["comments"]["pageInfo"]["hasNextPage"] == true {
            return Err("a thread with more than 100 comments; the read is incomplete".into());
        }
        for c in t["comments"]["nodes"].as_array().into_iter().flatten() {
            let (login, bot) = author(c);
            notes.push(Note {
                id: c["id"]
                    .as_str()
                    .ok_or("an inline note without an id")?
                    .to_string(),
                kind: NoteKind::Inline,
                author: login,
                bot,
                thread: Some(thread.clone()),
                resolved: Some(resolved),
                path: c["path"].as_str().map(str::to_string),
                line: c["line"].as_u64().or_else(|| c["originalLine"].as_u64()),
                at: millis(c, "createdAt")?,
                edited_at: edited(c)?,
                url: c["url"].as_str().unwrap_or("").to_string(),
                body: c["body"].as_str().unwrap_or("").to_string(),
            });
        }
    }
    Ok(ChangeSnapshot {
        state,
        head: HeadSeen {
            id: head_id,
            checks,
            failing: failed,
        },
        review_decision,
        mergeable,
        threads: Threads { unresolved },
        reviews: out_reviews,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn core(extra: Value) -> Value {
        let mut v = json!({
            "isDraft": false, "state": "OPEN", "reviewDecision": null,
            "mergeable": "MERGEABLE", "mergeStateStatus": "CLEAN", "headRefOid": "h1",
            "commits": {"nodes": [{"commit": {"oid": "h1", "statusCheckRollup": null}}]}
        });
        if let Some(o) = extra.as_object() {
            v.as_object_mut().unwrap().extend(o.clone());
        }
        v
    }

    #[test]
    fn a_terminal_state_wins_over_the_draft_flag() {
        let state = |pr: Value| snapshot(&core(pr), &[], &[], &[]).unwrap().state;
        assert_eq!(
            state(json!({"isDraft": true, "state": "CLOSED"})),
            ChangeState::Closed
        );
        assert_eq!(
            state(json!({"isDraft": true, "state": "MERGED"})),
            ChangeState::Merged
        );
        assert_eq!(
            state(json!({"isDraft": true, "state": "OPEN"})),
            ChangeState::Draft
        );
    }

    #[test]
    fn checks_fold_failed_over_pending_over_passed_and_ignore_skipped() {
        let rollup = |nodes: Value| {
            core(
                json!({"commits": {"nodes": [{"commit": {"oid": "h1", "statusCheckRollup": {"contexts": {"pageInfo": {"hasNextPage": false}, "nodes": nodes}}}}]}}),
            )
        };
        let s = snapshot(&rollup(json!([
            {"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"},
            {"__typename": "CheckRun", "name": "lint", "status": "IN_PROGRESS", "conclusion": null},
            {"__typename": "StatusContext", "context": "deploy", "state": "FAILURE", "targetUrl": "u"}
        ])), &[], &[], &[]).unwrap();
        assert_eq!(s.head.checks, Checks::Failed);
        assert_eq!(
            s.head.failing,
            vec![FailingCheck {
                name: "deploy".into(),
                url: Some("u".into())
            }]
        );
        let s = snapshot(&rollup(json!([
            {"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "SKIPPED"},
            {"__typename": "CheckRun", "name": "x", "status": "COMPLETED", "conclusion": "NEUTRAL"}
        ])), &[], &[], &[]).unwrap();
        assert_eq!(
            s.head.checks,
            Checks::None,
            "skipped and neutral are absent"
        );
        let s = snapshot(
            &rollup(json!([
                {"__typename": "CheckRun", "name": "ci", "status": "QUEUED", "conclusion": null},
                {"__typename": "StatusContext", "context": "x", "state": "SUCCESS"}
            ])),
            &[],
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(s.head.checks, Checks::Pending);
    }

    #[test]
    fn mergeable_follows_the_truth_table() {
        let m = |mergeable: &str, state: &str| {
            snapshot(
                &core(json!({"mergeable": mergeable, "mergeStateStatus": state})),
                &[],
                &[],
                &[],
            )
            .unwrap()
            .mergeable
        };
        assert_eq!(
            m("MERGEABLE", "CLEAN"),
            Mergeable {
                value: Some(true),
                reason: None
            }
        );
        assert_eq!(
            m("CONFLICTING", "DIRTY"),
            Mergeable {
                value: Some(false),
                reason: Some("conflicts".into())
            }
        );
        assert_eq!(
            m("UNKNOWN", "UNKNOWN"),
            Mergeable {
                value: None,
                reason: None
            }
        );
        assert_eq!(
            m("MERGEABLE", "BLOCKED"),
            Mergeable {
                value: Some(false),
                reason: Some("blocked".into())
            }
        );
        assert_eq!(
            m("MERGEABLE", "DRAFT"),
            Mergeable {
                value: Some(false),
                reason: Some("draft".into())
            }
        );
    }

    #[test]
    fn reviews_on_the_head_count_and_dismissed_ones_vanish() {
        let reviews = vec![
            json!({"id": "r1", "state": "APPROVED", "body": "", "url": "u1", "createdAt": "2026-10-24T12:00:00Z", "lastEditedAt": null, "author": {"login": "ann", "__typename": "User"}, "commit": {"oid": "h1"}}),
            json!({"id": "r2", "state": "CHANGES_REQUESTED", "body": "no", "url": "u2", "createdAt": "2026-10-24T11:00:00Z", "lastEditedAt": null, "author": {"login": "bot", "__typename": "Bot"}, "commit": {"oid": "h0"}}),
            json!({"id": "r3", "state": "DISMISSED", "body": "", "url": "u3", "createdAt": "2026-10-24T10:00:00Z", "lastEditedAt": null, "author": {"login": "x", "__typename": "User"}, "commit": {"oid": "h1"}}),
        ];
        let s = snapshot(
            &core(json!({"reviewDecision": "APPROVED"})),
            &reviews,
            &[],
            &[],
        )
        .unwrap();
        assert_eq!(s.review_decision, ReviewDecision::Approved);
        assert_eq!(
            s.reviews.len(),
            1,
            "only the head's reviews: {:?}",
            s.reviews
        );
        assert_eq!(s.reviews[0].author, "ann");
        let kinds: Vec<_> = s.notes.iter().map(|n| (n.id.as_str(), n.bot)).collect();
        assert_eq!(kinds, vec![("r1", false), ("r2", true)]);
    }

    #[test]
    fn threads_give_inline_notes_and_the_unresolved_count() {
        let threads = vec![
            json!({"id": "t1", "isResolved": false, "comments": {"pageInfo": {"hasNextPage": false}, "nodes": [
                {"id": "c1", "body": "fix", "url": "u", "path": "src/a.rs", "line": 3, "originalLine": 2, "createdAt": "2026-10-24T12:00:00Z", "lastEditedAt": "2026-10-24T12:30:00Z", "author": {"login": "ann", "__typename": "User"}}
            ]}}),
        ];
        let s = snapshot(&core(json!({})), &[], &[], &threads).unwrap();
        assert_eq!(s.threads.unresolved, 1);
        let n = &s.notes[0];
        assert_eq!(
            (n.kind, n.thread.as_deref(), n.resolved, n.line),
            (NoteKind::Inline, Some("t1"), Some(false), Some(3))
        );
        assert_eq!(n.edited_at, Some(1_792_845_000_000));
    }

    #[test]
    fn an_answer_the_table_cannot_place_is_an_error() {
        assert!(snapshot(&core(json!({"state": "LOCKED"})), &[], &[], &[]).is_err());
        assert!(snapshot(&core(json!({"mergeable": "MAYBE"})), &[], &[], &[]).is_err());
    }

    #[test]
    fn a_mutations_answer_is_known_only_when_it_can_be_read() {
        let reply = |body: &str| Reply {
            body: body.into(),
            exit: Some(0),
            ..Reply::default()
        };
        assert_eq!(
            mutation_outcome(&reply(
                r#"{"data":{"markPullRequestReadyForReview":{"clientMutationId":"k","pullRequest":{"isDraft":false}}}}"#
            )),
            Observation::Known(EffectOutcome::MarkedReady { already: false })
        );
        assert_eq!(
            mutation_outcome(&reply(
                r#"{"data":{"markPullRequestReadyForReview":null},"errors":[{"message":"Pull request is not a draft"}]}"#
            )),
            Observation::Known(EffectOutcome::MarkedReady { already: true })
        );
        assert_eq!(
            mutation_outcome(&reply(
                r#"{"data":null,"errors":[{"type":"FORBIDDEN","message":"Resource not accessible"}]}"#
            )),
            Observation::Known(EffectOutcome::Failed {
                why: "Resource not accessible".into()
            })
        );
        assert!(mutation_outcome(&reply("<html>gateway</html>")).is_unknown());
        let (head, state) =
            head_and_state(&json!({"isDraft": true, "state": "OPEN", "headRefOid": "h1"})).unwrap();
        assert_eq!((head.as_str(), state), ("h1", ChangeState::Draft));
        let (_, state) =
            head_and_state(&json!({"isDraft": true, "state": "CLOSED", "headRefOid": "h1"}))
                .unwrap();
        assert_eq!(
            state,
            ChangeState::Closed,
            "a closed draft is never marked ready"
        );
    }

    #[test]
    fn absence_is_a_repository_with_no_such_pull_request() {
        let reply = |body: &str| Reply {
            body: body.into(),
            exit: Some(1),
            ..Reply::default()
        };
        assert_eq!(
            body_of(&reply(r#"{"data":{"repository":{"pullRequest":null}},"errors":[{"type":"NOT_FOUND","path":["repository","pullRequest"],"message":"Could not resolve"}]}"#)).unwrap(),
            None
        );
        let err = body_of(&reply(r#"{"data":{"repository":null},"errors":[{"type":"NOT_FOUND","path":["repository"],"message":"Could not resolve"}]}"#)).unwrap_err();
        assert_eq!(
            err.0,
            Uncertainty::Refused,
            "a missing repository is ambiguous"
        );
        let err = body_of(&reply(
            r#"{"errors":[{"type":"RATE_LIMITED","message":"slow"}]}"#,
        ))
        .unwrap_err();
        assert_eq!(err.0, Uncertainty::Throttled);
    }
}
