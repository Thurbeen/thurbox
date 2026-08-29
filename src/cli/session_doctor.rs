//! `thurbox-cli session doctor` — whether a session's status hooks are
//! actually wired, and whether what they last reported is believable.
//!
//! Every shipped hook command ends in `|| true` (or `;; esac; true`), which is
//! deliberate — a missing `thurbox-cli`, a locked database or a hook firing
//! outside a thurbox session must never break the agent. The cost is that a
//! signal which never lands looks exactly like an agent that simply has not
//! signalled yet, and there was no way to tell the two apart. This is that way:
//! it inspects the wiring rather than the silence, in the spirit of
//! `thurbox-cli notify`.
//!
//! It reads; it never repairs. `thurbox-cli extension reinstall hooks` is the
//! repair, and the report says so.

use serde_json::{json, Value};

use crate::cli::output::{self, CommandOutput};
use crate::session::{Assessment, Corroboration, Coverage, HookDelivery};
use crate::storage::Database;
use crate::sync::SharedSession;

/// A single thing checked, and what was found.
struct Finding {
    /// Short stable key, so a script can branch on the problem rather than
    /// parse the sentence.
    key: &'static str,
    /// Whether this is a problem at all, and how bad.
    level: Level,
    detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    /// Checked and healthy.
    Ok,
    /// Something is limited or unverifiable, but state can still flow.
    Warn,
    /// No state can reach thurbox from this session at all.
    Fail,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Fail => "fail",
        }
    }
}

/// Diagnose one session, or every active session when `uuid` is `None`.
///
/// Exits non-zero when any session's wiring is `Fail` — something that must
/// work for state to reach thurbox does not. A `Warn` (partial coverage, an
/// unreadable remote pane, a pane that disagrees with the last report, an agent
/// thurbox ships no hooks for that is signalling anyway) prints and exits 0:
/// those are facts to know, not breakage to fix, and a permanent non-zero exit
/// for `aider`'s one-state coverage — or for a driver reporting its own state
/// exactly as documented — would be noise rather than signal.
pub fn run(db: &Database, uuid: Option<&str>) -> Result<CommandOutput, String> {
    let sessions = match uuid {
        Some(uuid) => vec![super::sessions::resolve(db, uuid)?],
        None => db
            .list_active_sessions()
            .map_err(|e| format!("list_active_sessions: {e}"))?,
    };
    let states = db.load_hook_states().unwrap_or_default();
    let registry = crate::agent::agent_config::load_or_seed();
    let hooks_active = crate::session_ops::builtin_hooks::hooks_enabled(db);
    // Resolved once: the same answer for every session, and each probe is a
    // directory walk.
    let cli_on_path = thurbox_cli_on_path();

    let mut reports = Vec::new();
    for session in &sessions {
        let hook = super::sessions::assess(&registry, session, &states, true);
        reports.push(diagnose(
            session,
            &hook,
            hooks_active,
            cli_on_path.as_deref(),
        ));
    }

    let broken: Vec<&str> = reports
        .iter()
        .zip(&sessions)
        .filter(|(r, _)| r.verdict == Level::Fail)
        .map(|(_, s)| s.name.as_str())
        .collect();
    // Named for the *wiring*, not the outcome: a session can be reporting right
    // now through a route this build did not install (a driver calling `session
    // signal` itself) while a check below it is still broken, and claiming no
    // state reaches thurbox would be false for exactly that row.
    let failure = match broken.len() {
        0 => None,
        _ => Some(format!(
            "hook wiring is broken for: {} — see the FAIL checks above",
            broken.join(", ")
        )),
    };

    let json = Value::Array(
        reports
            .iter()
            .zip(&sessions)
            .map(|(r, s)| r.to_json(s))
            .collect(),
    );
    let human = if reports.is_empty() {
        "No active sessions.".to_string()
    } else {
        reports
            .iter()
            .zip(&sessions)
            .map(|(r, s)| r.render(s))
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    Ok(match failure {
        Some(msg) => CommandOutput::failed(json, human, msg),
        None => CommandOutput::new(json, human),
    })
}

/// One session's diagnosis.
struct Report {
    verdict: Level,
    findings: Vec<Finding>,
    hook: Assessment,
}

impl Report {
    fn to_json(&self, s: &SharedSession) -> Value {
        json!({
            "session_id": s.id.to_string(),
            "session_name": s.name,
            "agent": s.agent,
            "verdict": self.verdict.as_str(),
            "hook_state": self.hook.hook_state,
            "hook_state_age_secs": self.hook.age_secs,
            "hook_reported": self.hook.reported,
            "hook_coverage": self.hook.coverage.as_str(),
            "hook_states_reportable": self.hook.states_reportable(),
            "hook_corroboration": self.hook.corroboration.map(|c| c.as_str()),
            "hook_state_contradicted": self.hook.contradicted,
            "checks": self.findings.iter().map(|f| json!({
                "check": f.key,
                "level": f.level.as_str(),
                "detail": f.detail,
            })).collect::<Vec<_>>(),
        })
    }

    fn render(&self, s: &SharedSession) -> String {
        let mut out = format!(
            "{} ({}) — {}\n",
            s.name,
            s.agent,
            self.verdict.as_str().to_uppercase()
        );
        for f in &self.findings {
            let mark = match f.level {
                Level::Ok => "ok  ",
                Level::Warn => "warn",
                Level::Fail => "FAIL",
            };
            out.push_str(&format!("  {mark}  {:<14} {}\n", f.key, f.detail));
        }
        out.trim_end().to_string()
    }
}

/// Everything checkable about one session's wiring, in the order a reader
/// would ask it: is the machinery on, does this agent have any, is its payload
/// where the agent will look, can the hook command find the binary it names,
/// has anything actually arrived, and does the pane agree.
fn diagnose(
    session: &SharedSession,
    hook: &Assessment,
    hooks_active: bool,
    cli_on_path: Option<&str>,
) -> Report {
    let mut findings = Vec::new();
    let remote = crate::session::is_remote_backend(&session.backend_type);

    findings.push(if hooks_active {
        Finding {
            key: "extension",
            level: Level::Ok,
            detail: "the built-in hooks extension is active".into(),
        }
    } else {
        Finding {
            key: "extension",
            level: Level::Fail,
            detail: "the hooks extension is deactivated — no agent is wired to report \
                     (`thurbox-cli extension activate hooks`)"
                .into(),
        }
    });

    findings.push(match hook.coverage {
        // Nothing thurbox ships wires this agent — but a driver that owns the
        // agent launch is *documented* to call `session signal` itself, and
        // when it does, state is demonstrably reaching thurbox. Failing that
        // session would hand the one integration shape this exists for a
        // permanently non-zero `doctor`.
        Coverage::None if hook.reported => Finding {
            key: "coverage",
            level: Level::Warn,
            detail: format!(
                "thurbox ships no status hooks for agent '{}', but signals are arriving — \
                 something in the pane is calling `thurbox-cli session signal`, so this \
                 session reports what that caller chooses to report",
                session.agent
            ),
        },
        Coverage::None => Finding {
            key: "coverage",
            level: Level::Fail,
            detail: format!(
                "thurbox ships no status hooks for agent '{}' — set `hook_schema` in \
                 agents.toml if it speaks a built-in's hook format, or have your driver call \
                 `thurbox-cli session signal --state <s>` (identity comes from the injected \
                 $THURBOX_SESSION, so it needs no arguments)",
                session.agent
            ),
        },
        Coverage::Partial => Finding {
            key: "coverage",
            level: Level::Warn,
            detail: format!(
                "'{}' can only report {} — silence about any other state means nothing",
                session.agent,
                hook.states_reportable().join(", ")
            ),
        },
        Coverage::Full => Finding {
            key: "coverage",
            level: Level::Ok,
            detail: format!("'{}' can report every state", session.agent),
        },
    });

    if let Some(finding) = payload_finding(hook, remote) {
        findings.push(finding);
    }

    findings.push(match cli_on_path {
        Some(_) if remote => Finding {
            key: "cli",
            level: Level::Warn,
            detail: "a hook on this host would find `thurbox-cli`, but this session's hooks \
                     run on its own host, which cannot be checked from here"
                .into(),
        },
        Some(path) => Finding {
            key: "cli",
            level: Level::Ok,
            detail: format!("hook commands resolve `thurbox-cli` to {path}"),
        },
        None => Finding {
            key: "cli",
            level: Level::Fail,
            detail: "`thurbox-cli` is not on PATH — every hook command is `… || true`, so its \
                     signals fail silently"
                .into(),
        },
    });

    findings.push(match (&hook.hook_state, hook.age_secs) {
        (Some(state), Some(age)) => Finding {
            key: "last-signal",
            level: Level::Ok,
            detail: format!("{state}, {} ago", output::duration_short(age)),
        },
        _ => Finding {
            key: "last-signal",
            level: Level::Warn,
            detail: "nothing has ever signalled for this session".into(),
        },
    });

    if let Some(corroboration) = hook.corroboration {
        let process = hook.foreground_process.as_deref().unwrap_or("nothing");
        findings.push(if hook.contradicted == Some(true) {
            Finding {
                key: "pane",
                level: Level::Warn,
                detail: format!(
                    "the row says '{}' but {process} holds the pane — the agent that reported \
                     it is gone",
                    hook.hook_state.as_deref().unwrap_or("?")
                ),
            }
        } else {
            Finding {
                key: "pane",
                // "Nothing could be resolved" is honest, but it is not a clean
                // bill of health: it means the one check that could have
                // falsified the row could not be run.
                level: match corroboration {
                    Corroboration::Unavailable | Corroboration::Unknown | Corroboration::Dead => {
                        Level::Warn
                    }
                    _ => Level::Ok,
                },
                detail: match corroboration {
                    Corroboration::Unknown => {
                        "no live pane for this session, so its state cannot be checked".into()
                    }
                    Corroboration::Dead => "the pane's command has exited (its frame is kept \
                         by remain-on-exit)"
                        .into(),
                    Corroboration::Unavailable => "this session's pane is on its own host, so \
                         its state cannot be checked from here"
                        .into(),
                    _ => format!("{} ({process})", corroboration.as_str()),
                },
            }
        });
    }

    let verdict = findings.iter().map(|f| f.level).max().unwrap_or(Level::Ok);
    Report {
        verdict,
        findings,
        hook: hook.clone(),
    }
}

/// Whether this agent's hook payload is where the agent will read it.
///
/// The check that separates "the agent has not signalled yet" from "nothing was
/// ever installed for it to signal with". Presence alone is not enough — a file
/// can sit at that path for reasons of the user's own — so the payload must
/// also carry the signal marker every thurbox-managed hook command has.
///
/// `None` for an agent with no file to check (aider's whole wiring is a launch
/// arg, and the launch already happened), and a warning rather than a verdict
/// for a remote session: the payload lives on the host, where it is either the
/// host's own hooks extension's business (a shared host) or was shipped at
/// spawn time — neither readable from here.
fn payload_finding(hook: &Assessment, remote: bool) -> Option<Finding> {
    let path = hook_file_path(hook)?;
    if remote {
        return Some(Finding {
            key: "payload",
            level: Level::Warn,
            detail: format!(
                "this session's hooks live on its own host (expected at {}); \
                 not readable from here",
                path.display()
            ),
        });
    }
    let marker = crate::session_ops::builtin_hooks::SIGNAL_MARKER;
    Some(match std::fs::read_to_string(&path) {
        Ok(body) if body.contains(marker) => Finding {
            key: "payload",
            level: Level::Ok,
            detail: format!("hooks installed at {}", path.display()),
        },
        Ok(_) => Finding {
            key: "payload",
            level: Level::Fail,
            detail: format!(
                "{} exists but carries no thurbox hook — a file of your own is there, so \
                 thurbox refused to write over it (`thurbox-cli extension reinstall hooks`)",
                path.display()
            ),
        },
        Err(e) => Finding {
            key: "payload",
            level: Level::Fail,
            detail: format!(
                "{} is unreadable ({e}) — this agent has no hooks installed \
                 (`thurbox-cli extension reinstall hooks`)",
                path.display()
            ),
        },
    })
}

/// Where this agent's hook payload should be on *this* machine.
///
/// Two anchors, because the two delivery shapes differ: a config-dir payload is
/// `~`-anchored against the user's home, while claude's travels by
/// `--settings` and so lives inside the hooks extension's own install home —
/// which is this build's config dir, keeping a dev build off the release copy.
fn hook_file_path(hook: &Assessment) -> Option<std::path::PathBuf> {
    let file = hook.hook_file()?;
    if hook.delivery() == Some(HookDelivery::Args) {
        let home = crate::session_ops::builtin::builtin_extension(
            crate::session_ops::builtin_hooks::HOOKS_EXTENSION_NAME,
        )?
        .home()?;
        return Some(std::path::PathBuf::from(home).join(file));
    }
    Some(crate::paths::expand_tilde(file))
}

/// The `thurbox-cli` a hook command would run, found the way the hook finds it:
/// by name, on `PATH`.
///
/// Deliberately not [`crate::agent::tmux::resolve_cli_binary`], which prefers
/// the sibling of the running executable — a hook command carries the bare name
/// and gets whatever `PATH` gives it, which is precisely the failure being
/// looked for.
fn thurbox_cli_on_path() -> Option<String> {
    let name = format!("thurbox-cli{}", std::env::consts::EXE_SUFFIX);
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(&name))
            .find(|candidate| candidate.is_file())
            .map(|found| found.display().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{AgentRegistry, SessionId};

    fn registry() -> AgentRegistry {
        crate::agent::agent_config::builtin_registry()
    }

    fn row(name: &str, agent: &str, backend: &str) -> SharedSession {
        SharedSession {
            id: SessionId::default(),
            name: name.into(),
            agent: agent.into(),
            backend_id: String::new(),
            backend_type: backend.into(),
            agent_session_id: None,
            cwd: None,
            additional_dirs: Vec::new(),
            worktrees: Vec::new(),
            shell_backend_id: None,
            parent_session_id: None,
            display_order: None,
            tombstone: false,
            tombstone_at: None,
        }
    }

    fn level_of(report: &Report, key: &str) -> Level {
        report
            .findings
            .iter()
            .find(|f| f.key == key)
            .unwrap_or_else(|| panic!("no {key} finding"))
            .level
    }

    #[test]
    fn an_agent_with_no_wiring_fails_and_says_how_to_wire_it() {
        let hook = Assessment::from_hooks(&registry(), "mine", None, None, 0);
        let report = diagnose(&row("s", "mine", "local-tmux"), &hook, true, Some("/bin/x"));
        assert_eq!(report.verdict, Level::Fail);
        assert_eq!(level_of(&report, "coverage"), Level::Fail);
        // The point of failing rather than shrugging: there *is* a route, and
        // an integrator has no reason to know it exists.
        let detail = &report
            .findings
            .iter()
            .find(|f| f.key == "coverage")
            .unwrap()
            .detail;
        assert!(detail.contains("session signal"), "got {detail}");
        assert!(detail.contains("THURBOX_SESSION"), "got {detail}");
    }

    #[test]
    fn an_uncovered_agent_that_is_actually_signalling_warns_rather_than_fails() {
        // The firstmate shape: the driver owns the agent launch (so thurbox
        // wired nothing) and reports state itself through the documented
        // `session signal`. State is demonstrably arriving, so a `fail` verdict
        // — and the non-zero exit with it — would be false for this row.
        let hook = Assessment::from_hooks(&registry(), "shell", Some("working"), Some(0), 5_000);
        let report = diagnose(
            &row("s", "shell", "local-tmux"),
            &hook,
            true,
            Some("/bin/x"),
        );
        assert_eq!(level_of(&report, "coverage"), Level::Warn);
        assert_eq!(level_of(&report, "last-signal"), Level::Ok);
        assert_ne!(report.verdict, Level::Fail);
        let detail = &report
            .findings
            .iter()
            .find(|f| f.key == "coverage")
            .unwrap()
            .detail;
        assert!(detail.contains("signals are arriving"), "got {detail}");
    }

    #[test]
    fn a_missing_binary_is_reported_rather_than_swallowed() {
        // Every hook command is `… || true`, so a `thurbox-cli` that is not on
        // PATH looks exactly like an agent that has not signalled. This is the
        // whole reason the subcommand exists.
        let hook = Assessment::from_hooks(&registry(), "claude", Some("working"), Some(0), 0);
        let report = diagnose(&row("s", "claude", "local-tmux"), &hook, true, None);
        assert_eq!(level_of(&report, "cli"), Level::Fail);
        assert_eq!(report.verdict, Level::Fail);
    }

    #[test]
    fn a_deactivated_extension_is_a_failure_not_a_silence() {
        let hook = Assessment::from_hooks(&registry(), "claude", None, None, 0);
        let report = diagnose(&row("s", "claude", "local-tmux"), &hook, false, Some("/x"));
        assert_eq!(level_of(&report, "extension"), Level::Fail);
    }

    #[test]
    fn a_partial_agent_warns_without_failing() {
        // aider can only ever report `blocked`; that is a fact to know, not
        // breakage to fix, so it must not exit non-zero forever.
        let hook = Assessment::from_hooks(&registry(), "aider", None, None, 0);
        let report = diagnose(&row("s", "aider", "local-tmux"), &hook, true, Some("/x"));
        assert_eq!(level_of(&report, "coverage"), Level::Warn);
        assert_ne!(report.verdict, Level::Fail);
        // aider's wiring is a launch arg alone, so there is no file to check.
        assert!(report.findings.iter().all(|f| f.key != "payload"));
    }

    #[test]
    fn a_pane_that_disagrees_is_called_out() {
        let known = vec!["claude".to_string()];
        let hook = Assessment::from_hooks(&registry(), "claude", Some("working"), Some(0), 1_000)
            .with_pane("claude", &known, Some("bash"), Some("bash"), Some(false));
        let report = diagnose(&row("s", "claude", "local-tmux"), &hook, true, Some("/x"));
        assert_eq!(level_of(&report, "pane"), Level::Warn);
        let detail = &report
            .findings
            .iter()
            .find(|f| f.key == "pane")
            .unwrap()
            .detail;
        assert!(detail.contains("bash"), "got {detail}");
    }

    #[test]
    fn a_remote_session_is_unchecked_rather_than_declared_broken() {
        // Its payload lives on the host — either the host's own hooks
        // extension's business or shipped at spawn — and neither is readable
        // here. Reporting a local file as missing would be a false failure.
        let hook = Assessment::from_hooks(&registry(), "claude", Some("done"), Some(0), 1_000)
            .pane_unavailable();
        let report = diagnose(&row("s", "claude", "ssh:devbox"), &hook, true, Some("/x"));
        assert_eq!(level_of(&report, "payload"), Level::Warn);
        assert_eq!(level_of(&report, "cli"), Level::Warn);
        assert_ne!(report.verdict, Level::Fail);
        assert_eq!(hook.corroboration, Some(Corroboration::Unavailable));
    }
}
