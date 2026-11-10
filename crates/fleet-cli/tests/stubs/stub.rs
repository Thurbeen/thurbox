//! A bounded stand-in for `az`, `curl`, `gh` and `glab`, so the end-to-end
//! tests drive the real binary against something that behaves like them
//! without any real forge or chat platform. Copied to a file named `gh` or
//! `glab` it answers `api …` in that forge's shapes, GraphQL for GitHub and
//! REST for GitLab, rendered from one forge-neutral model under `changes` in
//! its state. As `curl` it answers Bitbucket Cloud's API, Slack's Web API
//! from the model under `slack`, and Discord's REST API from the model under
//! `discord`. Under any name it also serves as a plugin (`plugin …`), an
//! environment command (`env-output …`) and a program that hangs (`sleep`).
//!
//! Its whole state is one JSON file in `$FLEET_STUB_DIR`, guarded by a lock
//! file so concurrent fleet processes see one host. Every call is appended to
//! `calls.jsonl` with its argv and the NAMES of its environment, so a test can
//! prove what reached a command line. Faults make a call fail, or hang
//! (leaving a grandchild behind, to prove the caller kills the whole tree).

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The state directory: `$FLEET_STUB_DIR`, or the path in the `stub-dir`
/// file beside the executable, for a copy driven by an adapter under test
/// in-process, which sets no environment of its own.
fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("FLEET_STUB_DIR") {
        return PathBuf::from(d);
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join("stub-dir")))
        .and_then(|p| fs::read_to_string(p).ok())
        .expect("FLEET_STUB_DIR is set, or a stub-dir file is beside the stub");
    PathBuf::from(beside.trim())
}

struct Lock(PathBuf);

impl Lock {
    fn take() -> Lock {
        let path = dir().join("state.lock");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => return Lock(path),
                Err(_) if Instant::now() < deadline => sleep(Duration::from_millis(5)),
                Err(e) => panic!("stub lock: {e}"),
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn load() -> Value {
    match fs::read(dir().join("state.json")) {
        Ok(b) => serde_json::from_slice(&b).expect("stub state"),
        Err(_) => json!({}),
    }
}

fn save(state: &Value) {
    let tmp = dir().join("state.json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(state).expect("json")).expect("write state");
    fs::rename(&tmp, dir().join("state.json")).expect("rename state");
}

fn log_call(args: &[String]) {
    let mut names: Vec<String> = std::env::vars_os()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    names.sort();
    let line = json!({"argv": args, "envNames": names});
    let mut f = OpenOptions::new()
        .append(true)
        .create(true)
        .open(dir().join("calls.jsonl"))
        .expect("calls log");
    // One write per line, so a reader never sees half of one.
    f.write_all(format!("{line}\n").as_bytes()).expect("log");
}

/// The fault armed for `call`, consumed once.
fn take_fault(state: &mut Value, call: &str) -> Option<String> {
    let faults = state.get_mut("faults")?.as_array_mut()?;
    let i = faults.iter().position(|f| f["call"] == call)?;
    // `after: N` lets the first N matching calls through.
    if let Some(after) = faults[i]["after"].as_u64().filter(|n| *n > 0) {
        faults[i]["after"] = json!(after - 1);
        return None;
    }
    let mode = faults[i]["mode"].as_str().unwrap_or("fail").to_string();
    let times = faults[i]["times"].as_u64().unwrap_or(1);
    if times <= 1 {
        faults.remove(i);
    } else {
        faults[i]["times"] = json!(times - 1);
    }
    Some(mode)
}

/// Hang like a stuck program: leave a grandchild that would outlive a caller
/// killing only its direct child, record its pid, and never answer.
fn hang() -> ! {
    let me = std::env::current_exe().expect("self");
    let child = Command::new(me)
        .arg("sleep")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("grandchild");
    let mut f = OpenOptions::new()
        .append(true)
        .create(true)
        .open(dir().join("grandchildren"))
        .expect("pids");
    writeln!(f, "{}", child.id()).expect("pid");
    sleep(Duration::from_secs(600));
    std::process::exit(9)
}

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::from(1)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("sleep") {
        sleep(Duration::from_secs(600));
        return ExitCode::SUCCESS;
    }
    if args.first().map(String::as_str) == Some("env-output") {
        if let Some(marker) = args.get(2) {
            std::fs::write(marker, "called").expect("marker");
        }
        let bytes = std::fs::read(&args[1]).expect("source output");
        std::io::Write::write_all(&mut std::io::stdout(), &bytes).expect("output");
        return ExitCode::SUCCESS;
    }
    if args.first().map(String::as_str) == Some("plugin") {
        return plugin(&args);
    }
    log_call(&args);
    let role = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_default();
    match (role.as_str(), args.first().map(String::as_str)) {
        ("gh", Some("api")) => forge::gh(&args),
        ("glab", Some("api")) => forge::glab(&args),
        ("az", Some("devops" | "repos")) => forge::az(&args),
        ("curl", Some("--include")) => forge::curl(&args),
        _ => fail("stub: unknown role"),
    }
}

/// The forge roles: one model, two renderings. A change lives under
/// `changes["<project>#<id>"]` as
/// `{state, head, title, checks: [{name, state, url}], reviewDecision,
/// mergeable, approvedBy, reviews: [{id, author, bot, state, body, head, at,
/// editedAt}], comments: [{id, author, bot, body, at, editedAt}],
/// threads: [{id, resolved, path, line, comments: [...]}]}`; `pageSize`
/// bounds a GitHub page, `login` answers whoami, `noRepo` makes the project
/// itself absent. A mark-ready (the GitHub mutation, or GitLab's title
/// update) moves `state` to `open` and records the intent under
/// `readyIntents`. Faults are armed under the call `gh`, `glab`, `az` or
/// `curl`: `fail`,
/// `hang`, `auth`, `rate-limit` (with `retryAfter` seconds) and `malformed`.
mod forge {
    use std::process::ExitCode;

    use serde_json::{Value, json};

    use super::{Lock, fail, hang, load, save, take_fault};

    const HEAD: &str = "HTTP/2.0 200 OK\r\nX-Ratelimit-Remaining: 4999\r\n\r\n";

    fn iso(ms: &Value) -> Value {
        let Some(ms) = ms.as_u64() else {
            return Value::Null;
        };
        let (secs, millis) = (ms / 1000, ms % 1000);
        let days = i64::try_from(secs / 86_400).unwrap_or(0);
        let rem = secs % 86_400;
        // Civil-from-days, Howard Hinnant's algorithm.
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        json!(format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        ))
    }

    fn var(args: &[String], name: &str) -> Option<String> {
        args.iter()
            .filter(|a| a.contains('='))
            .filter_map(|a| a.split_once('='))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.to_string())
    }

    fn flag(args: &[String], name: &str) -> bool {
        var(args, name).as_deref() == Some("true")
    }

    fn answer(code: ExitCode, head: &str, body: &str) -> ExitCode {
        print!("{head}{body}");
        code
    }

    /// Arm and apply the role's fault; `None` means proceed normally.
    fn faulted(
        state: &mut Value,
        lock: Lock,
        role: &str,
        args: &[String],
    ) -> Result<Lock, ExitCode> {
        let fault = take_fault(state, role);
        let Some(mode) = fault.as_deref() else {
            return Ok(lock);
        };
        let mode = mode.to_string();
        if mode == "truncated" {
            state["truncateNext"] = json!(true);
            return Ok(lock);
        }
        save(state);
        drop(lock);
        let retry = state["retryAfter"].as_u64().unwrap_or(30);
        let _ = args;
        Err(match mode.as_str() {
            "hang" => hang(),
            "auth" if role == "curl" => answer(
                ExitCode::from(22),
                "HTTP/2.0 401 Unauthorized\r\n\r\n",
                "{\"type\":\"error\",\"error\":{\"message\":\"authentication required\"}}",
            ),
            "auth" => {
                eprintln!("To get started with {role}, please run: {role} auth login");
                ExitCode::from(4)
            }
            "rate-limit" => {
                let status = if role == "gh" {
                    "403 Forbidden"
                } else {
                    "429 Too Many Requests"
                };
                answer(
                    ExitCode::from(1),
                    &format!(
                        "HTTP/2.0 {status}\r\nRetry-After: {retry}\r\nX-Ratelimit-Remaining: 0\r\n\r\n"
                    ),
                    "{\"message\":\"API rate limit exceeded for this user.\"}",
                )
            }
            "malformed" => answer(ExitCode::SUCCESS, HEAD, "<html>not json"),
            _ => fail(&format!("stub: {role} refused")),
        })
    }

    fn change<'a>(state: &'a Value, project: &str, id: &str) -> Option<&'a Value> {
        state["changes"].get(format!("{project}#{id}"))
    }

    fn author(item: &Value) -> Value {
        json!({"login": item["author"], "__typename": if item["bot"] == true { "Bot" } else { "User" }})
    }

    /// One page of a connection: cursors are indexes.
    fn page(items: &[Value], after: Option<String>, size: usize) -> (Vec<Value>, Value) {
        let start = after
            .and_then(|c| c.parse::<usize>().ok())
            .map_or(0, |i| i + 1);
        let end = (start + size).min(items.len());
        let nodes = items[start..end].to_vec();
        let info = json!({"hasNextPage": end < items.len(), "endCursor": if end > start { json!((end - 1).to_string()) } else { Value::Null }});
        (nodes, info)
    }

    pub fn gh(args: &[String]) -> ExitCode {
        let lock = Lock::take();
        let mut state = load();
        let _lock = match faulted(&mut state, lock, "gh", args) {
            Ok(l) => l,
            Err(code) => return code,
        };
        let query = var(args, "query").unwrap_or_default();
        if query.contains("viewer") {
            let login = state["login"].as_str().unwrap_or("stub-login");
            return answer(
                ExitCode::SUCCESS,
                HEAD,
                &json!({"data": {"viewer": {"login": login}}}).to_string(),
            );
        }
        let (owner, name, number) = (
            var(args, "owner").unwrap_or_default(),
            var(args, "name").unwrap_or_default(),
            var(args, "number").unwrap_or_default(),
        );
        if state["noRepo"] == true {
            return answer(ExitCode::from(1), HEAD, &json!({"data": {"repository": null}, "errors": [{"type": "NOT_FOUND", "path": ["repository"], "message": "Could not resolve to a Repository"}]}).to_string());
        }
        if query.contains("markPullRequestReadyForReview") {
            // The mutation names the pull request by the id the fresh read
            // handed out: `PR_<project>#<number>`.
            let id = var(args, "id").unwrap_or_default();
            let Some(key) = id.strip_prefix("PR_") else {
                return answer(ExitCode::SUCCESS, HEAD, &json!({"data": null, "errors": [{"type": "NOT_FOUND", "message": "Could not resolve to a node"}]}).to_string());
            };
            let key = key.to_string();
            let intent = var(args, "intent").unwrap_or_default();
            let Some(c) = state["changes"].get_mut(&key) else {
                return answer(ExitCode::SUCCESS, HEAD, &json!({"data": null, "errors": [{"type": "NOT_FOUND", "message": "Could not resolve to a node"}]}).to_string());
            };
            if c["state"] != "draft" {
                return answer(ExitCode::SUCCESS, HEAD, &json!({"data": {"markPullRequestReadyForReview": null}, "errors": [{"message": "Pull request is not a draft"}]}).to_string());
            }
            c["state"] = json!("open");
            if !c["readyIntents"].is_array() {
                c["readyIntents"] = json!([]);
            }
            c["readyIntents"]
                .as_array_mut()
                .unwrap()
                .push(json!(intent));
            save(&state);
            return answer(ExitCode::SUCCESS, HEAD, &json!({"data": {"markPullRequestReadyForReview": {"clientMutationId": intent, "pullRequest": {"isDraft": false}}}}).to_string());
        }
        let Some(c) = change(&state, &format!("{owner}/{name}"), &number) else {
            return answer(ExitCode::from(1), HEAD, &json!({"data": {"repository": {"pullRequest": null}}, "errors": [{"type": "NOT_FOUND", "path": ["repository", "pullRequest"], "message": "Could not resolve to a PullRequest"}]}).to_string());
        };
        let size = usize::try_from(state["pageSize"].as_u64().unwrap_or(100)).unwrap_or(100);
        let head = c["head"].as_str().unwrap_or("").to_string();
        if var(args, "withCore").is_none() {
            // The fresh read before a mark-ready: id, draft, state and head.
            let state_word = match c["state"].as_str().unwrap_or("open") {
                "closed" => "CLOSED",
                "merged" => "MERGED",
                _ => "OPEN",
            };
            return answer(ExitCode::SUCCESS, HEAD, &json!({"data": {"repository": {"pullRequest": {
                "id": format!("PR_{owner}/{name}#{number}"), "isDraft": c["state"] == "draft" || c["draft"] == true, "state": state_word, "headRefOid": head
            }}}}).to_string());
        }
        let mut pr = json!({});
        if flag(args, "withCore") {
            let state_word = match c["state"].as_str().unwrap_or("open") {
                "closed" => "CLOSED",
                "merged" => "MERGED",
                _ => "OPEN",
            };
            let (mergeable, merge_state) = match c["mergeable"].as_str().unwrap_or("mergeable") {
                "conflicting" => ("CONFLICTING", "DIRTY".to_string()),
                "unknown" => ("UNKNOWN", "UNKNOWN".to_string()),
                "mergeable" => ("MERGEABLE", "CLEAN".to_string()),
                other => ("MERGEABLE", other.to_ascii_uppercase()),
            };
            let contexts: Vec<Value> = c["checks"].as_array().cloned().unwrap_or_default().iter().map(|k| {
                let st = k["state"].as_str().unwrap_or("success");
                if k["kind"] == "status" {
                    json!({"__typename": "StatusContext", "context": k["name"], "state": st.to_ascii_uppercase(), "targetUrl": k["url"]})
                } else {
                    let (status, conclusion) = match st {
                        "success" => ("COMPLETED", json!("SUCCESS")),
                        "failure" => ("COMPLETED", json!("FAILURE")),
                        "skipped" => ("COMPLETED", json!("SKIPPED")),
                        _ => ("IN_PROGRESS", Value::Null),
                    };
                    json!({"__typename": "CheckRun", "name": k["name"], "status": status, "conclusion": conclusion, "detailsUrl": k["url"]})
                }
            }).collect();
            pr = json!({
                "isDraft": c["state"] == "draft" || c["draft"] == true,
                "state": state_word,
                "reviewDecision": match c["reviewDecision"].as_str() { Some("approved") => json!("APPROVED"), Some("changes-requested") => json!("CHANGES_REQUESTED"), _ => Value::Null },
                "mergeable": mergeable,
                "mergeStateStatus": merge_state,
                "headRefOid": head,
                "commits": {"nodes": [{"commit": {"oid": head, "statusCheckRollup": if contexts.is_empty() { Value::Null } else { json!({"contexts": {"pageInfo": {"hasNextPage": false}, "nodes": contexts}}) }}}]}
            });
        }
        if flag(args, "withReviews") {
            let items: Vec<Value> = c["reviews"].as_array().cloned().unwrap_or_default().iter().map(|r| json!({
                "id": r["id"], "state": r["state"].as_str().unwrap_or("commented").replace('-', "_").to_ascii_uppercase(),
                "body": r["body"], "url": format!("https://github.test/{owner}/{name}/pull/{number}#pullrequestreview-{}", r["id"]),
                "createdAt": iso(&r["at"]), "lastEditedAt": iso(&r["editedAt"]), "author": author(r), "commit": {"oid": r["head"]}
            })).collect();
            let (nodes, info) = page(&items, var(args, "reviewsAfter"), size);
            pr["reviews"] = json!({"pageInfo": info, "nodes": nodes});
        }
        if flag(args, "withComments") {
            let items: Vec<Value> = c["comments"].as_array().cloned().unwrap_or_default().iter().map(|k| json!({
                "id": k["id"], "body": k["body"], "url": format!("https://github.test/{owner}/{name}/pull/{number}#issuecomment-{}", k["id"]),
                "createdAt": iso(&k["at"]), "lastEditedAt": iso(&k["editedAt"]), "author": author(k)
            })).collect();
            let (nodes, info) = page(&items, var(args, "commentsAfter"), size);
            pr["comments"] = json!({"pageInfo": info, "nodes": nodes});
        }
        if flag(args, "withThreads") {
            let items: Vec<Value> = c["threads"].as_array().cloned().unwrap_or_default().iter().map(|t| {
                let comments: Vec<Value> = t["comments"].as_array().cloned().unwrap_or_default().iter().map(|k| json!({
                    "id": k["id"], "body": k["body"], "url": format!("https://github.test/{owner}/{name}/pull/{number}#discussion_r{}", k["id"]),
                    "path": t["path"], "line": t["line"], "originalLine": t["line"],
                    "createdAt": iso(&k["at"]), "lastEditedAt": iso(&k["editedAt"]), "author": author(k)
                })).collect();
                json!({"id": t["id"], "isResolved": t["resolved"] == true, "comments": {"pageInfo": {"hasNextPage": false}, "nodes": comments}})
            }).collect();
            let (nodes, info) = page(&items, var(args, "threadsAfter"), size);
            pr["reviewThreads"] = json!({"pageInfo": info, "nodes": nodes});
        }
        answer(
            ExitCode::SUCCESS,
            HEAD,
            &json!({"data": {"repository": {"pullRequest": pr}}}).to_string(),
        )
    }

    fn not_found() -> ExitCode {
        answer(
            ExitCode::from(1),
            "HTTP/2.0 404 Not Found\r\n\r\n",
            "{\"message\":\"404 Not found\"}",
        )
    }

    fn gl_user(item: &Value) -> Value {
        json!({"username": item["author"], "bot": item["bot"] == true})
    }

    pub fn glab(args: &[String]) -> ExitCode {
        let lock = Lock::take();
        let mut state = load();
        let _lock = match faulted(&mut state, lock, "glab", args) {
            Ok(l) => l,
            Err(code) => return code,
        };
        let endpoint = args.last().cloned().unwrap_or_default();
        let (path, query) = endpoint.split_once('?').unwrap_or((&endpoint, ""));
        let param = |k: &str| {
            query
                .split('&')
                .filter_map(|kv| kv.split_once('='))
                .find(|(a, _)| *a == k)
                .map(|(_, v)| v.to_string())
        };
        if path == "user" {
            let login = state["login"].as_str().unwrap_or("stub-login");
            return answer(
                ExitCode::SUCCESS,
                HEAD,
                &json!({"username": login}).to_string(),
            );
        }
        let Some(rest) = path.strip_prefix("projects/") else {
            return fail("stub: glab endpoint not modelled");
        };
        let mut parts = rest.splitn(2, '/');
        let project = parts.next().unwrap_or("").replace("%2F", "/");
        let tail = parts.next().unwrap_or("");
        if state["noRepo"] == true {
            return not_found();
        }
        if tail.is_empty() {
            return answer(
                ExitCode::SUCCESS,
                HEAD,
                &json!({"id": 1, "path_with_namespace": project}).to_string(),
            );
        }
        let Some(rest) = tail.strip_prefix("merge_requests/") else {
            return fail("stub: glab endpoint not modelled");
        };
        let mut parts = rest.splitn(2, '/');
        let iid = parts.next().unwrap_or("");
        let sub = parts.next().unwrap_or("");
        let Some(c) = change(&state, &project, iid) else {
            return not_found();
        };
        let key = format!("{project}#{iid}");
        let head = c["head"].as_str().unwrap_or("").to_string();
        let web_url = format!("https://gitlab.test/{project}/-/merge_requests/{iid}");
        let title = |c: &Value| {
            c["title"].as_str().map_or_else(
                || {
                    if c["state"] == "draft" {
                        "Draft: stub change".to_string()
                    } else {
                        "stub change".to_string()
                    }
                },
                str::to_string,
            )
        };
        let put = args.iter().any(|a| a == "PUT");
        if put && sub.is_empty() {
            // GitLab takes a merge request out of draft through its title.
            let new_title = var(args, "title").unwrap_or_default();
            let c = state["changes"].get_mut(&key).expect("change");
            c["title"] = json!(new_title);
            let still_draft = new_title.to_ascii_lowercase().starts_with("draft");
            if !still_draft && c["state"] == "draft" {
                c["state"] = json!("open");
                if !c["readyIntents"].is_array() {
                    c["readyIntents"] = json!([]);
                }
                c["readyIntents"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!("title update"));
            }
            let body = json!({"iid": iid.parse::<u64>().unwrap_or(0), "draft": still_draft, "title": new_title, "sha": head});
            save(&state);
            return answer(ExitCode::SUCCESS, HEAD, &body.to_string());
        }
        let body = match sub {
            "" => {
                let checks = c["checks"].as_array().cloned().unwrap_or_default();
                let pipeline = if checks.is_empty() {
                    Value::Null
                } else {
                    let status = if checks.iter().any(|k| k["state"] == "failure") {
                        "failed"
                    } else if checks.iter().any(|k| k["state"] == "pending") {
                        "running"
                    } else {
                        "success"
                    };
                    json!({"status": status, "web_url": format!("{web_url}/pipelines/1")})
                };
                json!({
                    "iid": iid.parse::<u64>().unwrap_or(0),
                    "state": match c["state"].as_str().unwrap_or("open") { "closed" => "closed", "merged" => "merged", _ => "opened" },
                    "draft": c["state"] == "draft" || c["draft"] == true,
                    "title": title(c),
                    "sha": head,
                    "detailed_merge_status": c["mergeable"].as_str().unwrap_or("mergeable"),
                    "head_pipeline": pipeline,
                    "web_url": web_url
                })
            }
            "approvals" => {
                let by: Vec<Value> = c["approvedBy"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|u| json!({"user": {"username": u}}))
                    .collect();
                json!({"approved": !by.is_empty(), "approved_by": by})
            }
            "discussions" => {
                let mut all: Vec<Value> = Vec::new();
                for k in c["comments"].as_array().cloned().unwrap_or_default() {
                    all.push(json!({"id": format!("d{}", k["id"].as_str().unwrap_or("")), "individual_note": true, "notes": [
                        {"id": k["id"], "type": Value::Null, "body": k["body"], "author": gl_user(&k), "system": false,
                         "created_at": iso(&k["at"]), "updated_at": iso(if k["editedAt"].is_null() { &k["at"] } else { &k["editedAt"] }), "resolvable": false}
                    ]}));
                }
                for t in c["threads"].as_array().cloned().unwrap_or_default() {
                    let notes: Vec<Value> = t["comments"].as_array().cloned().unwrap_or_default().iter().map(|k| json!({
                        "id": k["id"], "type": "DiffNote", "body": k["body"], "author": gl_user(k), "system": false,
                        "created_at": iso(&k["at"]), "updated_at": iso(if k["editedAt"].is_null() { &k["at"] } else { &k["editedAt"] }),
                        "resolvable": true, "resolved": t["resolved"] == true,
                        "position": {"new_path": t["path"], "new_line": t["line"]}
                    })).collect();
                    all.push(json!({"id": t["id"], "individual_note": false, "notes": notes}));
                }
                let per: usize = param("per_page").and_then(|v| v.parse().ok()).unwrap_or(20);
                let page_no: usize = param("page")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1)
                    .max(1);
                let start = (page_no - 1) * per;
                let end = (start + per).min(all.len());
                Value::Array(if start < all.len() {
                    all[start..end].to_vec()
                } else {
                    Vec::new()
                })
            }
            _ => return fail("stub: glab endpoint not modelled"),
        };
        answer(ExitCode::SUCCESS, HEAD, &body.to_string())
    }

    pub fn curl(args: &[String]) -> ExitCode {
        let url = super::opt(args, "--url").unwrap_or_default();
        if url.starts_with(super::slack::API) {
            return super::slack::curl(args, &url);
        }
        if url.starts_with(super::discord::API) {
            return super::discord::curl(args, &url);
        }
        let lock = Lock::take();
        let mut state = load();
        let _lock = match faulted(&mut state, lock, "curl", args) {
            Ok(l) => l,
            Err(code) => return code,
        };
        let Some(rest) = url.strip_prefix("https://api.bitbucket.org/2.0/") else {
            return fail("stub: curl URL is not the Bitbucket Cloud API");
        };
        if rest == "user" {
            let login = state["login"].as_str().unwrap_or("stub-login");
            return answer(
                ExitCode::SUCCESS,
                HEAD,
                &json!({"type": "user", "account_id": format!("account-{login}"), "uuid": format!("{{uuid-{login}}}")}).to_string(),
            );
        }
        let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
        let mut parts = path.split('/');
        if parts.next() != Some("repositories") {
            return fail("stub: curl endpoint not modelled");
        }
        let workspace = parts.next().unwrap_or("");
        let repository = parts.next().unwrap_or("");
        let project = format!("{workspace}/{repository}");
        if state["noRepo"] == true {
            return not_found();
        }
        let Some(resource) = parts.next() else {
            if state["repoRateLimit"] == true {
                let retry = state["retryAfter"].as_u64().unwrap_or(30);
                return answer(
                    ExitCode::from(1),
                    &format!(
                        "HTTP/2.0 429 Too Many Requests\r\nRetry-After: {retry}\r\nX-Ratelimit-Remaining: 0\r\n\r\n"
                    ),
                    "{\"message\":\"API rate limit exceeded for this user.\"}",
                );
            }
            return answer(
                ExitCode::SUCCESS,
                HEAD,
                &json!({"type": "repository", "full_name": project, "uuid": format!("{{repo-{workspace}-{repository}}}")}).to_string(),
            );
        };
        if resource != "pullrequests" {
            return fail("stub: curl resource not modelled");
        }
        let id = parts.next().unwrap_or("");
        let sub = parts.collect::<Vec<_>>().join("/");
        let final_read = if sub.is_empty()
            && (state["moveHeadOnFinal"] == true || state["moveTargetOnFinal"] == true)
        {
            let reads = state["bitbucketPrReads"].as_u64().unwrap_or(0) + 1;
            state["bitbucketPrReads"] = json!(reads);
            save(&state);
            reads > 1
        } else {
            false
        };
        let Some(c) = change(&state, &project, id) else {
            return not_found();
        };
        let page_size = usize::try_from(state["pageSize"].as_u64().unwrap_or(100)).unwrap_or(100);
        let cursor = query
            .split('&')
            .filter_map(|item| item.split_once('='))
            .find(|(key, _)| *key == "cursor")
            .and_then(|(_, value)| value.parse::<usize>().ok())
            .unwrap_or(0);
        let paged = |values: Vec<Value>, suffix: &str| {
            let end = (cursor + page_size).min(values.len());
            json!({
                "pagelen": end.saturating_sub(cursor),
                "size": values.len(),
                "values": if cursor < values.len() { values[cursor..end].to_vec() } else { Vec::new() },
                "next": if end < values.len() {
                    json!(format!("https://api.bitbucket.org/2.0/repositories/{project}/pullrequests/{id}/{suffix}?opaque=x&cursor={end}"))
                } else { Value::Null }
            })
        };
        let user = |item: &Value| {
            json!({
                "type": if item["bot"] == true { "app_user" } else { "user" },
                "account_id": format!("account-{}", item["author"].as_str().unwrap_or("unknown")),
                "uuid": format!("{{uuid-{}}}", item["author"].as_str().unwrap_or("unknown"))
            })
        };
        let body = match sub.as_str() {
            "" => {
                let decision = c["reviewDecision"].as_str();
                let participants = match decision {
                    Some("changes-requested") => vec![
                        json!({"user": {"type": "user", "account_id": "account-ann", "uuid": "{uuid-ann}"}, "approved": false, "state": "changes_requested"}),
                    ],
                    Some("approved") => vec![
                        json!({"user": {"type": "user", "account_id": "account-ann", "uuid": "{uuid-ann}"}, "approved": true, "state": "approved"}),
                    ],
                    _ => Vec::new(),
                };
                json!({
                    "type": "pullrequest",
                    "id": id.parse::<u64>().unwrap_or(0),
                    "state": match c["state"].as_str().unwrap_or("open") { "merged" => "MERGED", "closed" => "DECLINED", _ => "OPEN" },
                    "draft": c["state"] == "draft" || c["draft"] == true,
                    "source": {"commit": {"hash": if final_read && state["moveHeadOnFinal"] == true { json!("moved-head") } else { c["head"].clone() }}},
                    "destination": {
                        "commit": {"hash": if final_read && state["moveTargetOnFinal"] == true { "moved-target" } else { c["target"].as_str().unwrap_or("target-head") }},
                        "repository": {"type": "repository", "full_name": if state["wrongRepo"] == true { "other/widgets" } else { project.as_str() }, "uuid": format!("{{repo-{workspace}-{repository}}}")}
                    },
                    "participants": participants,
                    "links": {"html": {"href": format!("https://bitbucket.org/{project}/pull-requests/{id}")}}
                })
            }
            "comments" => {
                let mut values = Vec::new();
                for item in c["comments"].as_array().cloned().unwrap_or_default() {
                    values.push(json!({
                        "type": "pullrequest_comment", "id": item["id"].as_str().unwrap_or("").trim_start_matches('c').parse::<u64>().unwrap_or(0),
                        "created_on": iso(&item["at"]), "updated_on": iso(if item["editedAt"].is_null() { &item["at"] } else { &item["editedAt"] }),
                        "content": {"raw": item["body"]}, "user": user(&item), "deleted": false,
                        "inline": Value::Null, "parent": Value::Null, "resolution": Value::Null, "pending": false,
                        "links": {"html": {"href": format!("https://bitbucket.org/{project}/pull-requests/{id}#comment-{}", item["id"])}}
                    }));
                }
                for thread in c["threads"].as_array().cloned().unwrap_or_default() {
                    let comments = thread["comments"].as_array().cloned().unwrap_or_default();
                    let root = comments
                        .first()
                        .and_then(|v| v["id"].as_str())
                        .unwrap_or("ic0")
                        .trim_start_matches("ic")
                        .parse::<u64>()
                        .unwrap_or(0)
                        + 100;
                    for (index, item) in comments.into_iter().enumerate() {
                        let comment_id = item["id"]
                            .as_str()
                            .unwrap_or("")
                            .trim_start_matches("ic")
                            .parse::<u64>()
                            .unwrap_or(0)
                            + 100;
                        values.push(json!({
                            "type": "pullrequest_comment", "id": comment_id,
                            "created_on": iso(&item["at"]), "updated_on": iso(if item["editedAt"].is_null() { &item["at"] } else { &item["editedAt"] }),
                            "content": {"raw": item["body"]}, "user": user(&item), "deleted": false,
                            "inline": if index == 0 { json!({"path": thread["path"], "to": thread["line"]}) } else { Value::Null },
                            "parent": if index == 0 { Value::Null } else { json!({"id": root}) },
                            "resolution": if index == 0 && thread["resolved"] == true { json!({"type": "pullrequest_comment_resolution", "created_on": iso(&item["at"])}) } else { Value::Null },
                            "pending": false,
                            "links": {"html": {"href": format!("https://bitbucket.org/{project}/pull-requests/{id}#comment-{comment_id}")}}
                        }));
                    }
                }
                if state["missingResolution"] == true
                    && let Some(first_inline) = values.iter_mut().find(|v| v["inline"].is_object())
                {
                    first_inline.as_object_mut().unwrap().remove("resolution");
                }
                paged(values, "comments")
            }
            "statuses" => {
                let status_head = if state["wrongStatusHead"] == true {
                    "stale-head"
                } else {
                    c["head"].as_str().unwrap_or("")
                };
                let values = c["checks"].as_array().cloned().unwrap_or_default().into_iter().map(|item| json!({
                    "type": "build", "key": item["name"], "name": item["name"], "url": item["url"],
                    "state": match item["state"].as_str() { Some("failure") => "FAILED", Some("pending") => "INPROGRESS", Some("skipped") => "STOPPED", _ => "SUCCESSFUL" },
                    "links": {"commit": {"href": format!("https://api.bitbucket.org/2.0/repositories/{project}/commit/{status_head}")}}
                })).collect();
                paged(values, "statuses")
            }
            "mergeability/checks" => {
                if let Some(values) = state["mergeabilityChecks"].as_array().cloned() {
                    return answer(
                        ExitCode::SUCCESS,
                        HEAD,
                        &paged(values, "mergeability/checks").to_string(),
                    );
                }
                let status = match c["mergeable"].as_str() {
                    Some("unknown") => "PENDING",
                    Some("mergeable") => "PASSED",
                    _ => "FAILED",
                };
                json!({"size": 1, "values": [{"type": "git_mergeability_check", "status": status, "required": true, "blocking": status == "FAILED"}]})
            }
            _ => return fail("stub: curl pull request endpoint not modelled"),
        };
        answer(ExitCode::SUCCESS, HEAD, &body.to_string())
    }

    pub fn az(args: &[String]) -> ExitCode {
        let lock = Lock::take();
        let mut state = load();
        let _lock = match faulted(&mut state, lock, "az", args) {
            Ok(l) => l,
            Err(code) => return code,
        };
        let arg = |name: &str| {
            args.iter()
                .position(|a| a == name)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        let values_after = |name: &str| -> Vec<String> {
            let Some(mut i) = args.iter().position(|a| a == name).map(|i| i + 1) else {
                return Vec::new();
            };
            let mut out = Vec::new();
            while let Some(value) = args.get(i) {
                if value.starts_with("--") {
                    break;
                }
                out.push(value.clone());
                i += 1;
            }
            out
        };
        let routes = values_after("--route-parameters");
        let route = |name: &str| {
            routes
                .iter()
                .filter_map(|v| v.split_once('='))
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        };
        let org = arg("--organization")
            .and_then(|u| {
                u.trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .map(str::to_string)
            })
            .unwrap_or_default();
        if args.first().map(String::as_str) == Some("repos") {
            let id = arg("--id").unwrap_or_default();
            let key = state["changes"]
                .as_object()
                .and_then(|changes| {
                    changes.keys().find(|key| {
                        key.starts_with(&format!("{org}/")) && key.ends_with(&format!("#{id}"))
                    })
                })
                .cloned();
            let Some(key) = key else {
                return answer(
                    ExitCode::from(1),
                    "",
                    r#"{"statusCode":404,"message":"pull request not found"}"#,
                );
            };
            let c = state["changes"].get_mut(&key).unwrap();
            c["state"] = json!("open");
            save(&state);
            return answer(
                ExitCode::SUCCESS,
                "",
                &json!({"pullRequestId": id.parse::<u64>().unwrap_or(0), "isDraft": false})
                    .to_string(),
            );
        }
        let resource = arg("--resource").unwrap_or_default();
        if resource == "profiles" {
            return answer(
                ExitCode::SUCCESS,
                "",
                &json!({"id": state["login"].as_str().unwrap_or("identity-1")}).to_string(),
            );
        }
        if state["truncateNext"] == true {
            state["truncateNext"] = json!(false);
            save(&state);
            return answer(ExitCode::SUCCESS, "", r#"{"count":2,"value":[]}"#);
        }
        let project = route("project").unwrap_or_default();
        let repository = route("repositoryId").unwrap_or_default();
        let id = route("pullRequestId").unwrap_or_default();
        if resource == "repositories" {
            if state["noRepo"] == true {
                return answer(
                    ExitCode::from(1),
                    "",
                    r#"{"statusCode":404,"message":"repository not found"}"#,
                );
            }
            return answer(
                ExitCode::SUCCESS,
                "",
                &json!({"id": "repo-id", "name": repository, "project": {"name": project}})
                    .to_string(),
            );
        }
        if resource == "policyEvaluations" {
            let model = state["changes"]
                .as_object()
                .and_then(|changes| {
                    changes
                        .iter()
                        .find(|(key, _)| key.starts_with(&format!("{org}/{project}/")))
                })
                .map(|(_, value)| value);
            let policies: Vec<Value> = model
                .and_then(|c| c["policies"].as_array())
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|policy| {
                    json!({
                        "status": policy["status"],
                        "configuration": {
                            "isBlocking": policy["blocking"],
                            "isEnabled": true,
                            "url": policy["url"],
                            "type": {"displayName": policy["name"]}
                        }
                    })
                })
                .collect();
            return answer(
                ExitCode::SUCCESS,
                "",
                &json!({"count": policies.len(), "value": policies}).to_string(),
            );
        }
        let Some(c) = (state["noRepo"] != true)
            .then(|| change(&state, &format!("{org}/{project}/{repository}"), &id))
            .flatten()
        else {
            return answer(
                ExitCode::from(1),
                "",
                r#"{"statusCode":404,"message":"pull request not found"}"#,
            );
        };
        let web =
            format!("https://dev.azure.com/{org}/{project}/_git/{repository}/pullrequest/{id}");
        let body = match resource.as_str() {
            "pullRequests" => {
                let reviewers: Vec<Value> = c["reviews"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|r| r["state"] != "dismissed")
                    .map(|r| json!({
                        "id": r["author"],
                        "displayName": r["author"],
                        "vote": match r["state"].as_str() { Some("approved") => 10, Some("changes-requested") => -10, _ => 0 }
                    }))
                    .collect();
                json!({
                    "pullRequestId": id.parse::<u64>().unwrap_or(0),
                    "status": match c["state"].as_str() { Some("closed") => "abandoned", Some("merged") => "completed", _ => "active" },
                    "isDraft": c["state"] == "draft" || c["draft"] == true,
                    "lastMergeSourceCommit": {"commitId": "stale-merge-test-head"},
                    "mergeStatus": match c["mergeable"].as_str() { Some("conflicting") => "conflicts", Some("unstable") => "rejectedByPolicy", Some("unknown") => "queued", _ => "succeeded" },
                    "repository": {"id": "repo-id", "name": repository, "project": {"id": "project-id", "name": project}},
                    "reviewers": reviewers,
                    "remoteUrl": web
                })
            }
            "pullRequestIterations" => {
                let mut values = vec![
                    json!({"id": 1, "sourceRefCommit": {"commitId": "h0"}}),
                    json!({"id": 2, "sourceRefCommit": {"commitId": c["head"]}}),
                ];
                if state["malformedIterations"] == true {
                    values.push(json!({"sourceRefCommit": {"commitId": "newer-unknown"}}));
                }
                json!({"count": values.len(), "value": values})
            }
            "pullRequestIterationStatuses" => {
                let statuses: Vec<Value> = c["checks"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|check| json!({
                        "id": check["name"],
                        "state": match check["state"].as_str() { Some("failure") => "failed", Some("pending") => "pending", Some("skipped") => "notApplicable", _ => "succeeded" },
                        "context": {"genre": "fleet-test", "name": check["name"]},
                        "targetUrl": check["url"]
                    }))
                    .collect();
                json!({"count": statuses.len(), "value": statuses})
            }
            "pullRequestThreads" => {
                let mut threads: Vec<Value> = c["comments"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .enumerate()
                    .map(|(i, comment)| json!({
                        "id": i + 1,
                        "status": "active",
                        "comments": [{
                            "id": comment["id"].as_str().unwrap_or("").trim_start_matches('c').parse::<u64>().unwrap_or(i as u64 + 1),
                            "author": {"id": comment["author"]}, "commentType": "text", "content": comment["body"],
                            "publishedDate": iso(&comment["at"]), "lastContentUpdatedDate": iso(if comment["editedAt"].is_null() { &comment["at"] } else { &comment["editedAt"] })
                        }]
                    }))
                    .collect();
                for thread in c["threads"].as_array().cloned().unwrap_or_default() {
                    let comments: Vec<Value> = thread["comments"].as_array().cloned().unwrap_or_default().into_iter().map(|comment| json!({
                        "id": comment["id"].as_str().unwrap_or("").trim_start_matches("ic").parse::<u64>().unwrap_or(0) + 100,
                        "author": {"id": comment["author"]}, "commentType": "text", "content": comment["body"],
                        "publishedDate": iso(&comment["at"]), "lastContentUpdatedDate": iso(if comment["editedAt"].is_null() { &comment["at"] } else { &comment["editedAt"] })
                    })).collect();
                    threads.push(json!({
                        "id": thread["id"].as_str().unwrap_or("").trim_start_matches('t').parse::<u64>().unwrap_or(0) + 1000,
                        "status": if thread["resolved"] == true { "fixed" } else { "active" },
                        "threadContext": {"filePath": thread["path"], "rightFileStart": {"line": thread["line"]}},
                        "comments": comments
                    }));
                }
                json!({"count": threads.len(), "value": threads})
            }
            _ => return fail("stub: az resource not modelled"),
        };
        answer(ExitCode::SUCCESS, "", &body.to_string())
    }
}

/// Protocol fixture: reads only the explicit input directory, never operator state.
fn plugin(args: &[String]) -> ExitCode {
    let root = PathBuf::from(&args[1]);
    let call = args.last().unwrap();
    let scenario: Value =
        serde_json::from_slice(&fs::read(root.join("plugin.json")).unwrap()).unwrap();
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    for line in input.lines() {
        let req: Value = serde_json::from_str(line).unwrap();
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("plugin-calls.jsonl"))
            .unwrap();
        writeln!(log, "{}", json!({"call":call,"input":req,"envNames":std::env::vars().map(|(k,_)|k).collect::<Vec<_>>()})).unwrap();
        if scenario["failCall"] == *call {
            if scenario["failure"] == "exit" {
                return ExitCode::FAILURE;
            }
            if scenario["failure"] == "malformed" {
                println!("{}", json!({"id":req["id"],"ok":true,"plan":42}));
                continue;
            }
            if scenario["failure"] == "flood" {
                std::io::stdout()
                    .write_all(&vec![b'x'; 9 * 1024 * 1024])
                    .unwrap();
            }
            sleep(Duration::from_secs(600));
        }
        let payload = match call.as_str() {
            "manifest" => json!({"manifest":scenario["manifest"]}),
            "probes" => {
                json!({"probes": if req["observations"].as_object().unwrap().is_empty() {scenario.get("probes").cloned().unwrap_or(json!([]))} else {json!([])}})
            }
            "reconcile" if scenario["reflect"] == true => {
                json!({"plan":{"status":{"observedGeneration":req["object"]["metadata"]["generation"],"observations":req["observations"]}}})
            }
            "reconcile" => json!({"plan": scenario["plan"]}),
            "observe" if scenario["observeUnknown"] == true => {
                json!({"unknown":{"why":"Throttled","retryAfterMs":1000}})
            }
            "observe" => {
                json!({"known": {"seen": true,"providerWon":std::env::var("PLUGIN_SECRET").ok().as_deref()==Some("provider-value")}})
            }
            "perform" => json!({"outcome": {"done": {"posted": true}}}),
            _ => panic!("unexpected call"),
        };
        let mut answer = payload;
        answer["id"] = req["id"].clone();
        answer["ok"] = json!(true);
        println!("{answer}");
    }
    ExitCode::SUCCESS
}

/// The Slack role of `curl`: the Web API behind the Slack gateway adapter,
/// from the model under `slack` in the state: `{team, botUser, botId,
/// botName, clock, pageSize, messages: {"<channel>": [message, …]},
/// tombstones: [ts, …], postError, postErrorAfter, posts, calls}`. Each message is in
/// Slack's own shape (`ts`, `user` or `bot_id`, `text`, `thread_ts`,
/// `metadata`, `subtype`). `conversations.replies` answers a thread's
/// messages oldest first, paged by `pageSize` or the call's `limit`, with
/// the parent only when `oldest` does not exclude it, and a tombstone for a
/// parent under `tombstones`. `conversations.history` answers a channel's
/// top-level messages newest first. `chat.postMessage` appends a message at
/// the clock, which advances one second per post, and records it under
/// `posts`, or refuses with `postError` when set. Every call is recorded
/// under `calls` with its method and JSON body, read from stdin. Faults are
/// armed under the call `curl` as for the forges, rendered in Slack's
/// shapes: `auth` is `invalid_auth`, `rate-limit` a 429 with `Retry-After`
/// and `ratelimited`, `malformed` a non-JSON body, `fail` a failed curl,
/// `hang` a hung process.
mod slack {
    use std::io::Read;
    use std::process::ExitCode;

    use serde_json::{Value, json};

    use super::{Lock, fail, hang, load, save, take_fault};

    pub const API: &str = "https://slack.com/api/";
    const HEAD: &str = "HTTP/2 200 \r\ncontent-type: application/json; charset=utf-8\r\n\r\n";

    fn answer(head: &str, body: &Value) -> ExitCode {
        print!("{head}{body}");
        ExitCode::SUCCESS
    }

    fn ok_false(error: &str) -> ExitCode {
        answer(HEAD, &json!({"ok": false, "error": error}))
    }

    /// A Slack timestamp as (seconds, microseconds), for ordering.
    fn key(ts: &Value) -> (u64, u64) {
        let ts = ts.as_str().unwrap_or("0.0");
        let (secs, frac) = ts.split_once('.').unwrap_or((ts, "0"));
        (
            secs.parse().unwrap_or(0),
            format!("{frac:0<6}").parse().unwrap_or(0),
        )
    }

    fn string(v: &Value) -> String {
        v.as_str().unwrap_or("").to_string()
    }

    /// A form body as an object: `true`, `false` and digits typed, the
    /// rest strings, so a model reads either encoding the same way.
    fn form(text: &str) -> Value {
        fn decode(s: &str) -> String {
            let b = s.as_bytes();
            let mut out = Vec::with_capacity(b.len());
            let mut i = 0;
            while i < b.len() {
                match b[i] {
                    b'+' => out.push(b' '),
                    b'%' if i + 2 < b.len() => {
                        let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
                        out.push(u8::from_str_radix(hex, 16).unwrap_or(b'?'));
                        i += 2;
                    }
                    c => out.push(c),
                }
                i += 1;
            }
            String::from_utf8_lossy(&out).into_owned()
        }
        let mut o = serde_json::Map::new();
        for pair in text.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let v = decode(v);
            let typed = match v.as_str() {
                "true" => json!(true),
                "false" => json!(false),
                d if !d.is_empty() && d.bytes().all(|c| c.is_ascii_digit()) => {
                    json!(d.parse::<u64>().unwrap_or(0))
                }
                _ => json!(v),
            };
            o.insert(decode(k), typed);
        }
        Value::Object(o)
    }

    pub fn curl(args: &[String], url: &str) -> ExitCode {
        let method = url.trim_start_matches(API).to_string();
        let mut text = String::new();
        if args.iter().any(|a| a == "@-") {
            std::io::stdin().read_to_string(&mut text).expect("stdin");
        }
        // Slack's read methods take form arguments only: a JSON body reaches
        // them as no arguments at all, as the real API answers.
        let json_body = args
            .iter()
            .any(|a| a.starts_with("Content-Type: application/json"));
        let body: Value = if json_body {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        } else {
            form(&text)
        };
        if json_body
            && !method.starts_with("chat.")
            && body.as_object().is_some_and(|o| !o.is_empty())
        {
            return answer(
                HEAD,
                &json!({"ok": false, "error": "invalid_arguments",
                        "response_metadata": {"messages": ["[ERROR] missing required field: channel"]}}),
            );
        }
        let lock = Lock::take();
        let mut state = load();
        if !state["slack"]["calls"].is_array() {
            state["slack"]["calls"] = json!([]);
        }
        state["slack"]["calls"]
            .as_array_mut()
            .expect("calls")
            .push(json!({"method": method, "body": body}));
        if let Some(mode) = take_fault(&mut state, "curl") {
            save(&state);
            drop(lock);
            let retry = state["retryAfter"].as_u64().unwrap_or(30);
            return match mode.as_str() {
                "hang" => hang(),
                "auth" => ok_false("invalid_auth"),
                "rate-limit" => answer(
                    &format!("HTTP/2 429 \r\nretry-after: {retry}\r\n\r\n"),
                    &json!({"ok": false, "error": "ratelimited"}),
                ),
                "malformed" => {
                    print!("{HEAD}<html>not json");
                    ExitCode::SUCCESS
                }
                _ => fail("curl: (7) stub: curl refused"),
            };
        }
        save(&state);
        let _lock = lock;
        let slack = &state["slack"];
        let team = slack["team"].as_str().unwrap_or("T0123ABCD").to_string();
        let bot_user = slack["botUser"].as_str().unwrap_or("U0BOT0001").to_string();
        let bot_id = slack["botId"].as_str().unwrap_or("B0FLEET001").to_string();
        let bot_name = slack["botName"].as_str().unwrap_or("fleet").to_string();
        match method.as_str() {
            "auth.test" => answer(
                HEAD,
                &json!({"ok": true, "url": "https://example.slack.com/", "team": "Example",
                        "user": bot_name, "team_id": team, "user_id": bot_user, "bot_id": bot_id}),
            ),
            "conversations.replies" => replies(&state, &body),
            "conversations.history" => history(&state, &body),
            "chat.postMessage" => post(&mut state, &body, &bot_user, &bot_id),
            "reactions.add" => react(&mut state, &body, true),
            "reactions.remove" => react(&mut state, &body, false),
            _ => ok_false("unknown_method"),
        }
    }

    /// `reactions.add` and `reactions.remove` on a message of the model:
    /// `slack.reactions["<channel>/<ts>"]` lists the bot's reactions there.
    /// Adding one already there is `already_reacted`; removing one that is
    /// not is `no_reaction`, as Slack answers.
    fn react(state: &mut Value, body: &Value, add: bool) -> ExitCode {
        let channel = string(&body["channel"]);
        let ts = string(&body["timestamp"]);
        let name = string(&body["name"]);
        let Some(all) = channel_messages(state, &channel) else {
            return ok_false("channel_not_found");
        };
        if !all.iter().any(|m| m["ts"] == ts) {
            return ok_false("message_not_found");
        }
        if name.is_empty() || name.contains(':') {
            return ok_false("invalid_name");
        }
        let at = format!("{channel}/{ts}");
        if !state["slack"]["reactions"][&at].is_array() {
            state["slack"]["reactions"][&at] = json!([]);
        }
        let list = state["slack"]["reactions"][&at]
            .as_array_mut()
            .expect("reactions");
        let there = list.iter().any(|r| *r == name);
        match (add, there) {
            (true, true) => return ok_false("already_reacted"),
            (false, false) => return ok_false("no_reaction"),
            (true, false) => list.push(json!(name)),
            (false, true) => list.retain(|r| *r != name),
        }
        save(state);
        answer(HEAD, &json!({"ok": true}))
    }

    /// The messages of `channel`, or `None` for a channel the model lacks.
    fn channel_messages(state: &Value, channel: &str) -> Option<Vec<Value>> {
        state["slack"]["messages"][channel].as_array().cloned()
    }

    /// One page of `all`, under the model's page size or the call's limit,
    /// after `oldest` (inclusive or not), from the call's cursor.
    fn page(state: &Value, body: &Value, mut all: Vec<Value>) -> ExitCode {
        if let Some(oldest) = body["oldest"].as_str() {
            let bound = key(&json!(oldest));
            let inclusive = body["inclusive"] == true;
            all.retain(|m| {
                let k = key(&m["ts"]);
                k > bound || (inclusive && k == bound)
            });
        }
        if body["include_all_metadata"] != true {
            for m in &mut all {
                if let Some(o) = m.as_object_mut() {
                    o.remove("metadata");
                }
            }
        }
        let size = state["slack"]["pageSize"]
            .as_u64()
            .or_else(|| body["limit"].as_u64())
            .unwrap_or(200)
            .max(1) as usize;
        let from: usize = body["cursor"]
            .as_str()
            .and_then(|c| c.strip_prefix("c:"))
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        let to = (from + size).min(all.len());
        let has_more = to < all.len();
        let page: Vec<Value> = if from < all.len() {
            all[from..to].to_vec()
        } else {
            Vec::new()
        };
        answer(
            HEAD,
            &json!({
                "ok": true,
                "messages": page,
                "has_more": has_more,
                "response_metadata": {"next_cursor": if has_more { format!("c:{to}") } else { String::new() }},
            }),
        )
    }

    fn replies(state: &Value, body: &Value) -> ExitCode {
        let channel = string(&body["channel"]);
        let parent = string(&body["ts"]);
        let Some(all) = channel_messages(state, &channel) else {
            return ok_false("channel_not_found");
        };
        let mut thread: Vec<Value> = all
            .into_iter()
            .filter(|m| m["ts"] == parent || m["thread_ts"] == parent)
            .collect();
        if thread.is_empty() {
            return ok_false("thread_not_found");
        }
        let deleted = state["slack"]["tombstones"]
            .as_array()
            .is_some_and(|t| t.iter().any(|ts| *ts == parent));
        if deleted {
            thread.retain(|m| m["ts"] != parent);
            thread.push(
                json!({"type": "message", "subtype": "tombstone", "ts": parent, "user": "USLACKBOT",
                               "text": "This message was deleted.", "thread_ts": parent}),
            );
        }
        thread.sort_by_key(|m| key(&m["ts"]));
        page(state, body, thread)
    }

    fn history(state: &Value, body: &Value) -> ExitCode {
        let channel = string(&body["channel"]);
        let Some(all) = channel_messages(state, &channel) else {
            return ok_false("channel_not_found");
        };
        let mut top: Vec<Value> = all
            .into_iter()
            .filter(|m| m["thread_ts"].is_null() || m["thread_ts"] == m["ts"])
            .collect();
        top.sort_by_key(|m| std::cmp::Reverse(key(&m["ts"])));
        page(state, body, top)
    }

    fn post(state: &mut Value, body: &Value, bot_user: &str, bot_id: &str) -> ExitCode {
        let channel = string(&body["channel"]);
        if channel_messages(state, &channel).is_none() {
            return ok_false("channel_not_found");
        }
        if let Some(error) = state["slack"]["postError"].as_str() {
            return ok_false(error);
        }
        let clock = state["slack"]["clock"].as_u64().unwrap_or(1_730_000_500);
        state["slack"]["clock"] = json!(clock + 1);
        let ts = format!("{clock}.000000");
        let mut message = json!({"type": "message", "ts": ts, "user": bot_user, "bot_id": bot_id,
                                 "text": body["text"], "team": state["slack"]["team"]});
        for field in ["thread_ts", "metadata"] {
            if !body[field].is_null() {
                message[field] = body[field].clone();
            }
        }
        state["slack"]["messages"][&channel]
            .as_array_mut()
            .expect("channel")
            .push(message.clone());
        if !state["slack"]["posts"].is_array() {
            state["slack"]["posts"] = json!([]);
        }
        state["slack"]["posts"]
            .as_array_mut()
            .expect("posts")
            .push(json!({
                "channel": channel, "ts": ts, "thread_ts": body["thread_ts"], "text": body["text"],
                "metadata": body["metadata"], "mrkdwn": body["mrkdwn"], "blocks": body["blocks"],
            }));
        save(state);
        // `postErrorAfter`: the post landed, and Slack answers an error
        // anyway, as its `internal_error` and `fatal_error` may.
        if let Some(error) = state["slack"]["postErrorAfter"].as_str() {
            return ok_false(error);
        }
        answer(
            HEAD,
            &json!({"ok": true, "channel": channel, "ts": ts, "message": message}),
        )
    }
}

/// The Discord role of `curl`: the REST API behind the Discord gateway
/// adapter, from the model under `discord` in the state: `{guild, botUser,
/// botName, appFlags, clock, channels: {"<id>": {parent, messages: [message,
/// …]}}, loseNextPost, posts, threads, calls}`. Each message is in
/// Discord's own shape (`id`, `channel_id`, `author`, `content`,
/// `timestamp`, `type`, `mentions`, `nonce`). A channel with a `parent` is a
/// thread. `GET …/messages` answers newest first, at most `limit`, after
/// `after` or before `before`; `POST …/messages` appends a message at the clock, which advances
/// one second per post, and returns the bot's earlier message with the same
/// nonce when `enforce_nonce` is set. `loseNextPost` lets one post land and
/// then hangs, a lost answer. `POST …/messages/{id}/threads` starts a
/// thread whose id is the message's, once: a second start answers 160004.
/// Errors are Discord's JSON errors with their HTTP status. Every call is
/// recorded under `calls` with its method, path and JSON body.
mod discord {
    use std::io::Read;
    use std::process::ExitCode;

    use serde_json::{Value, json};

    use super::{Lock, fail, hang, load, opt, save, take_fault};

    pub const API: &str = "https://discord.com/api/v10/";
    const EPOCH: u64 = 1_420_070_400_000;

    fn answer(status: u16, body: &Value) -> ExitCode {
        print!("HTTP/2 {status} \r\ncontent-type: application/json\r\n\r\n{body}");
        ExitCode::SUCCESS
    }

    fn error(status: u16, code: u64, message: &str) -> ExitCode {
        answer(status, &json!({"code": code, "message": message}))
    }

    fn id_of(v: &Value) -> u64 {
        v.as_str().and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    /// A UTC ISO 8601 time for Unix milliseconds, as Discord writes it.
    fn iso(ms: u64) -> String {
        let secs = ms / 1000;
        let (days, rem) = (secs / 86_400, secs % 86_400);
        // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
        let z = days as i64 + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + i64::from(m <= 2);
        format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}000+00:00",
            rem / 3600,
            rem % 3600 / 60,
            rem % 60,
            ms % 1000
        )
    }

    pub fn curl(args: &[String], url: &str) -> ExitCode {
        let method = opt(args, "--request").unwrap_or_else(|| "GET".into());
        let rest = url.trim_start_matches(API);
        let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
        let path = path.to_string();
        let param = |k: &str| {
            query
                .split('&')
                .find_map(|kv| kv.strip_prefix(&format!("{k}=")))
                .map(str::to_string)
        };
        let mut text = String::new();
        if args.iter().any(|a| a == "@-") {
            std::io::stdin().read_to_string(&mut text).expect("stdin");
        }
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let lock = Lock::take();
        let mut state = load();
        if !state["discord"]["calls"].is_array() {
            state["discord"]["calls"] = json!([]);
        }
        state["discord"]["calls"]
            .as_array_mut()
            .expect("calls")
            .push(json!({"method": method, "path": path, "query": query, "body": body}));
        if let Some(mode) = take_fault(&mut state, "curl") {
            save(&state);
            drop(lock);
            let retry = state["retryAfter"].as_f64().unwrap_or(1.5);
            return match mode.as_str() {
                "hang" => hang(),
                "auth" => error(401, 0, "401: Unauthorized"),
                "rate-limit" => answer(
                    429,
                    &json!({"message": "You are being rate limited.", "retry_after": retry, "global": false}),
                ),
                "malformed" => {
                    print!("HTTP/2 502 \r\n\r\n<html>bad gateway");
                    ExitCode::SUCCESS
                }
                _ => fail("curl: (7) stub: curl refused"),
            };
        }
        save(&state);
        let d = &state["discord"];
        let guild = d["guild"].as_str().unwrap_or("").to_string();
        let bot = d["botUser"].as_str().unwrap_or("").to_string();
        let parts: Vec<&str> = path.split('/').collect();
        match (method.as_str(), parts.as_slice()) {
            ("GET", ["users", "@me"]) => answer(
                200,
                &json!({"id": bot, "username": d["botName"].as_str().unwrap_or("fleet"), "bot": true}),
            ),
            ("GET", ["applications", "@me"]) => answer(
                200,
                &json!({"id": "1100000000000000000", "name": "Fleet", "flags": d["appFlags"].as_u64().unwrap_or(0)}),
            ),
            ("GET", ["guilds", g]) if *g == guild => {
                answer(200, &json!({"id": guild, "name": "Example"}))
            }
            ("GET", ["guilds", _]) => error(404, 10004, "Unknown Guild"),
            ("GET", ["channels", c, "messages"]) => {
                let Some(all) = d["channels"][*c]["messages"].as_array() else {
                    return error(404, 10003, "Unknown Channel");
                };
                let after = param("after").and_then(|a| a.parse::<u64>().ok());
                let before = param("before").and_then(|b| b.parse::<u64>().ok());
                let limit = param("limit").map_or(50, |l| l.parse().unwrap_or(50));
                let mut page: Vec<Value> = all
                    .iter()
                    .filter(|m| after.is_none_or(|a| id_of(&m["id"]) > a))
                    .filter(|m| before.is_none_or(|b| id_of(&m["id"]) < b))
                    .cloned()
                    .collect();
                page.sort_by_key(|m| id_of(&m["id"]));
                // After a message: the ones right after it. Otherwise the
                // newest. Either way newest first, as Discord answers.
                if after.is_some() {
                    page.truncate(limit);
                } else {
                    page.drain(..page.len().saturating_sub(limit));
                }
                page.reverse();
                answer(200, &json!(page))
            }
            ("GET", ["channels", c, "messages", m]) => {
                match d["channels"][*c]["messages"]
                    .as_array()
                    .and_then(|a| a.iter().find(|x| x["id"] == *m))
                {
                    Some(found) => answer(200, found),
                    None => error(404, 10008, "Unknown Message"),
                }
            }
            ("POST", ["channels", c, "messages"]) => {
                let channel = c.to_string();
                post(&mut state, &channel, &body, &bot, lock)
            }
            ("POST", ["channels", c, "messages", m, "threads"]) => {
                start_thread(&mut state, c, m, &body)
            }
            (verb @ ("PUT" | "DELETE"), ["channels", c, "messages", m, "reactions", e, "@me"]) => {
                react(&mut state, c, m, e, verb == "PUT")
            }
            _ => error(404, 0, "404: Not Found"),
        }
    }

    fn post(state: &mut Value, channel: &str, body: &Value, bot: &str, lock: Lock) -> ExitCode {
        let Some(messages) = state["discord"]["channels"][channel]["messages"].as_array() else {
            return error(404, 10003, "Unknown Channel");
        };
        if body["enforce_nonce"] == true
            && let Some(same) = messages
                .iter()
                .find(|m| m["author"]["id"] == bot && m["nonce"] == body["nonce"])
        {
            return answer(200, same);
        }
        let clock = state["discord"]["clock"]
            .as_u64()
            .unwrap_or(1_730_000_500_000);
        state["discord"]["clock"] = json!(clock + 1000);
        let id = ((clock - EPOCH) << 22).to_string();
        let message = json!({
            "id": id, "channel_id": channel, "type": 0,
            "author": {"id": bot, "username": "fleet", "bot": true},
            "content": body["content"], "timestamp": iso(clock), "edited_timestamp": null,
            "mentions": [], "nonce": body["nonce"],
        });
        state["discord"]["channels"][channel]["messages"]
            .as_array_mut()
            .expect("messages")
            .push(message.clone());
        if !state["discord"]["posts"].is_array() {
            state["discord"]["posts"] = json!([]);
        }
        state["discord"]["posts"]
            .as_array_mut()
            .expect("posts")
            .push(json!({"channel": channel, "id": id, "body": body}));
        let lose = state["discord"]["loseNextPost"] == true;
        state["discord"]["loseNextPost"] = json!(false);
        save(state);
        if lose {
            // The post landed; its answer never comes.
            drop(lock);
            hang();
        }
        answer(200, &message)
    }

    /// `%XX` decoded.
    fn unescape(s: &str) -> String {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' && i + 2 < b.len() {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
                out.push(u8::from_str_radix(hex, 16).unwrap_or(b'?'));
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// Create or delete the bot's own reaction: `204` with no body either
    /// way, as Discord answers. `discord.reactions["<channel>/<message>"]`
    /// lists the emoji there. An emoji that is neither Unicode nor
    /// `name:id` is `10014 Unknown Emoji`.
    fn react(state: &mut Value, channel: &str, message: &str, emoji: &str, add: bool) -> ExitCode {
        let found = state["discord"]["channels"][channel]["messages"]
            .as_array()
            .is_some_and(|a| a.iter().any(|m| m["id"] == message));
        if !found {
            return error(404, 10008, "Unknown Message");
        }
        if emoji.contains(':') || emoji.contains('\u{1F440}') {
            // Not URL-encoded: Discord answers Unknown Emoji.
            return error(400, 10014, "Unknown Emoji");
        }
        let emoji = unescape(emoji);
        let custom = emoji
            .split_once(':')
            .is_some_and(|(n, id)| !n.is_empty() && id.bytes().all(|c| c.is_ascii_digit()));
        if !custom && emoji.is_ascii() {
            return error(400, 10014, "Unknown Emoji");
        }
        let at = format!("{channel}/{message}");
        if !state["discord"]["reactions"][&at].is_array() {
            state["discord"]["reactions"][&at] = json!([]);
        }
        let list = state["discord"]["reactions"][&at]
            .as_array_mut()
            .expect("reactions");
        list.retain(|r| *r != emoji);
        if add {
            list.push(json!(emoji));
        }
        save(state);
        print!("HTTP/2 204 \r\n\r\n");
        ExitCode::SUCCESS
    }

    fn start_thread(state: &mut Value, channel: &str, message: &str, body: &Value) -> ExitCode {
        if !state["discord"]["channels"][message].is_null() {
            return error(
                400,
                160_004,
                "A thread has already been created for this message",
            );
        }
        let found = state["discord"]["channels"][channel]["messages"]
            .as_array()
            .is_some_and(|a| a.iter().any(|m| m["id"] == message));
        if !found {
            return error(404, 10008, "Unknown Message");
        }
        let starter = json!({
            "id": (id_of(&json!(message)) + 1).to_string(), "channel_id": message, "type": 21, "content": "",
            "author": {"id": "0", "username": "system"}, "mentions": [],
            "timestamp": iso((id_of(&json!(message)) >> 22) + EPOCH),
            "message_reference": {"message_id": message, "channel_id": channel},
        });
        state["discord"]["channels"][message] = json!({"parent": channel, "messages": [starter]});
        if !state["discord"]["threads"].is_array() {
            state["discord"]["threads"] = json!([]);
        }
        state["discord"]["threads"]
            .as_array_mut()
            .expect("threads")
            .push(json!({"parent": channel, "id": message, "name": body["name"]}));
        save(state);
        answer(
            201,
            &json!({"id": message, "parent_id": channel, "type": 11, "name": body["name"]}),
        )
    }
}
