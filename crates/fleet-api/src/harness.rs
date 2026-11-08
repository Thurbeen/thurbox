//! Harness profiles: how Fleet launches, resumes and trusts each coding
//! agent, so that a runtime knows no harness and only starts an argv.
//!
//! A Task names its harness (`spec.harness`). Its profile gives:
//!
//! - **launch**: the argv that starts the agent in its workspace, with the
//!   hooks that report its state through `fleet signal` ([`crate::signal`])
//!   and the trust Fleet can give it at launch;
//! - **resume**: the argv that starts it again on the conversation its
//!   hooks reported;
//! - **trust**: how the agent is trusted in its workspace, and how to read
//!   a trust dialog on its screen, which Fleet never answers.
//!
//! | Harness | Program | Hooks | Trust | Resume |
//! | --- | --- | --- | --- | --- |
//! | `claude-code`, `claude` | `claude` | one `--settings` with hooks: start, prompt, working, idle | its dialog, read, never answered | `--resume <id>` |
//! | `codex` | `codex` | `notify`: idle, with the turn's prompts | `-c projects=…` for the session | `codex resume <id>` |
//! | any other | the harness name | none | none Fleet knows | a fresh start |
//!
//! A harness with no prompt hook cannot prove a delivery: its runtime's
//! answer is all Fleet has.

/// What a profile knows of a harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Claude,
    Codex,
    Other,
}

/// How a harness is trusted in its workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// A launch override trusting one directory for the session.
    LaunchOverride,
    /// None Fleet may use: a person trusts the directory once, and Fleet
    /// reads the agent's dialog to say so.
    Dialog,
    /// Fleet knows none for this harness.
    Unknown,
}

/// One harness's profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub kind: Kind,
    /// The program the launch argv starts.
    pub program: String,
}

/// The profile of `harness`.
pub fn profile(harness: &str) -> Profile {
    let (kind, program) = match harness {
        "claude-code" | "claude" => (Kind::Claude, "claude"),
        "codex" => (Kind::Codex, "codex"),
        other => (Kind::Other, other),
    };
    Profile {
        kind,
        program: program.to_string(),
    }
}

/// The command a hook runs: `fleet signal <state>`, never failing the
/// agent's turn, which a hook's own failure could block.
fn signal_command(state: &str) -> String {
    format!("fleet signal {state} 2>/dev/null || true")
}

impl Profile {
    /// Whether the agent's hooks report each prompt it is given, so a
    /// delivery can be proven by the agent rather than by the runtime.
    pub fn prompt_hook(&self) -> bool {
        matches!(self.kind, Kind::Claude | Kind::Codex)
    }

    pub fn trust(&self) -> Trust {
        match self.kind {
            Kind::Claude => Trust::Dialog,
            Kind::Codex => Trust::LaunchOverride,
            Kind::Other => Trust::Unknown,
        }
    }

    /// The arguments that install the hooks reporting through
    /// `fleet signal`.
    pub fn hook_args(&self) -> Vec<String> {
        match self.kind {
            Kind::Claude => {
                let hook = |state: &str| serde_json::json!([{"hooks": [{"type": "command", "command": signal_command(state)}]}]);
                let settings = serde_json::json!({"hooks": {
                    "SessionStart": hook("idle"),
                    "UserPromptSubmit": hook("prompt"),
                    "PreToolUse": hook("working"),
                    "Stop": hook("idle"),
                }});
                vec!["--settings".into(), settings.to_string()]
            }
            // Codex runs `notify` at each turn's end with the turn as JSON,
            // its prompts included, as the last argument.
            Kind::Codex => vec!["-c".into(), r#"notify=["fleet","signal","idle"]"#.into()],
            Kind::Other => Vec::new(),
        }
    }

    /// The arguments that trust `root` for this session, when the harness
    /// takes one at launch.
    pub fn trust_args(&self, root: &str) -> Result<Vec<String>, String> {
        match self.kind {
            Kind::Codex => Ok(vec!["-c".into(), codex_override(root)?]),
            Kind::Claude | Kind::Other => Ok(Vec::new()),
        }
    }

    /// The whole argv that starts the agent: the program, its hooks, the
    /// trust of `trust_root`, and each other directory it spans. The
    /// working directory is the runtime's to set.
    pub fn launch(
        &self,
        extra_dirs: &[String],
        trust_root: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let mut argv = vec![self.program.clone()];
        argv.extend(self.hook_args());
        if let Some(root) = trust_root {
            argv.extend(self.trust_args(root)?);
        }
        if self.kind != Kind::Other {
            for d in extra_dirs {
                argv.extend(["--add-dir".to_string(), d.clone()]);
            }
        }
        Ok(argv)
    }

    /// The argv that starts the agent again on `conversation`, from the one
    /// it was launched with. Without a conversation, or for a harness Fleet
    /// cannot resume, it is the launch argv: a fresh start.
    pub fn resume(&self, launch: &[String], conversation: Option<&str>) -> Vec<String> {
        let (Some(id), Some((program, rest))) = (conversation, launch.split_first()) else {
            return launch.to_vec();
        };
        let mut argv = vec![program.clone()];
        match self.kind {
            Kind::Claude => {
                argv.extend(rest.iter().cloned());
                argv.extend(["--resume".to_string(), id.to_string()]);
            }
            Kind::Codex => {
                argv.extend(["resume".to_string(), id.to_string()]);
                argv.extend(rest.iter().cloned());
            }
            Kind::Other => return launch.to_vec(),
        }
        argv
    }
}

/// Codex's launch override trusting `root`. One `projects` key with a TOML
/// inline table: Codex splits a dotted `-c` key on every `.`, and a path has
/// them.
pub fn codex_override(root: &str) -> Result<String, String> {
    if root.chars().any(char::is_control) {
        return Err(format!("cannot name {root:?} in a Codex trust override"));
    }
    let escaped = root.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(format!(
        "projects={{\"{escaped}\"={{trust_level=\"trusted\"}}}}"
    ))
}

/// Claude Code's workspace-trust dialog on a plain screen `output`, current
/// at the cursor: its header, both choices and its footer, the cursor
/// between them. Old text in the scrollback is not a dialog.
pub fn claude_trust_dialog(output: &str, cursor_row: usize) -> bool {
    let lines: Vec<&str> = output.lines().map(str::trim).collect();
    let Some(header) = lines.iter().rposition(|l| *l == "Accessing workspace:") else {
        return false;
    };
    let Some(footer) = lines[header..]
        .iter()
        .position(|l| l.starts_with("Enter to confirm"))
        .map(|n| header + n)
    else {
        return false;
    };
    let choice = |want: &str| {
        lines[header..footer]
            .iter()
            .any(|l| l.trim_start_matches('❯').trim() == want)
    };
    cursor_row > header
        && cursor_row < footer
        && choice("No, exit")
        && choice("Yes, I trust this folder")
}

/// Whether Codex's trust dialog is on a plain visible screen `output`: its
/// question and its trusting default. It can follow the composer, so it is
/// looked for on the whole screen, not only at the cursor.
pub fn codex_trust_dialog(output: &str) -> bool {
    let lines: Vec<&str> = output.lines().map(str::trim).collect();
    lines.iter().any(|l| l.starts_with("Trust this folder?"))
        && lines.iter().any(|l| l.contains("Trust and continue"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_profile_launches_with_its_hooks_trust_and_directories() {
        let claude = profile("claude-code")
            .launch(&["/w/docs".into()], Some("/w/app"))
            .unwrap();
        assert_eq!(claude[0], "claude");
        assert_eq!(claude[1], "--settings");
        let settings: serde_json::Value = serde_json::from_str(&claude[2]).unwrap();
        for (event, state) in [
            ("SessionStart", "idle"),
            ("UserPromptSubmit", "prompt"),
            ("Stop", "idle"),
            ("PreToolUse", "working"),
        ] {
            let command = settings["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap();
            assert!(
                command.starts_with(&format!("fleet signal {state}")),
                "{command}"
            );
        }
        assert_eq!(&claude[3..], ["--add-dir", "/w/docs"]);
        // Claude Code keeps the last of several `--settings`, so the hooks
        // are one, and a resume keeps them one.
        let resumed = profile("claude").resume(&claude, Some("c1"));
        assert_eq!(resumed.iter().filter(|a| *a == "--settings").count(), 1);

        let codex = profile("codex").launch(&[], Some("/w/app")).unwrap();
        assert_eq!(
            codex,
            [
                "codex",
                "-c",
                r#"notify=["fleet","signal","idle"]"#,
                "-c",
                r#"projects={"/w/app"={trust_level="trusted"}}"#
            ]
        );
        assert_eq!(
            profile("aider").launch(&["/x".into()], Some("/w")).unwrap(),
            ["aider"]
        );
        assert!(profile("codex").prompt_hook() && !profile("aider").prompt_hook());
    }

    #[test]
    fn a_resume_names_the_reported_conversation() {
        let p = profile("claude");
        let launch = p.launch(&[], None).unwrap();
        let resumed = p.resume(&launch, Some("c1"));
        assert_eq!(&resumed[resumed.len() - 2..], ["--resume", "c1"]);
        assert_eq!(p.resume(&launch, None), launch);
        let codex = profile("codex");
        let launch = codex.launch(&[], None).unwrap();
        assert_eq!(
            &codex.resume(&launch, Some("t9"))[..3],
            ["codex", "resume", "t9"]
        );
        assert_eq!(
            profile("aider").resume(&["aider".into()], Some("x")),
            ["aider"]
        );
    }

    #[test]
    fn a_codex_trust_override_names_one_directory_as_toml() {
        assert_eq!(
            codex_override("/home/u/.cache/a b").unwrap(),
            r#"projects={"/home/u/.cache/a b"={trust_level="trusted"}}"#
        );
        assert_eq!(
            codex_override(r#"C:\work\say "hi""#).unwrap(),
            r#"projects={"C:\\work\\say \"hi\""={trust_level="trusted"}}"#
        );
        assert!(codex_override("/a\nb").is_err());
    }

    #[test]
    fn only_a_cursor_current_claude_trust_dialog_is_one() {
        let screen =
            "Accessing workspace:\n/w\n❯ Yes, I trust this folder\n  No, exit\nEnter to confirm";
        assert!(claude_trust_dialog(screen, 2));
        assert!(!claude_trust_dialog(screen, 0));
        assert!(!claude_trust_dialog(screen, 4));
        assert!(!claude_trust_dialog("Accessing workspace:\nNo, exit", 1));
        assert!(codex_trust_dialog(
            "Trust this folder?\n› 1. Trust and continue"
        ));
        assert!(!codex_trust_dialog("› Ask Codex to do anything"));
    }
}
