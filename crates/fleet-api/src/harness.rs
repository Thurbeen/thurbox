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
//! - **payload**: the fields of a hook's JSON that `fleet signal` reads;
//! - **screen**: the rule set that reads the agent's state off its screen
//!   when its hooks do not say it ([`crate::screen`]).
//!
//! A profile is data: one TOML document per harness. A file under
//! `<config dir>/harnesses/` replaces the bundled
//! profile of the same name, or adds a harness. Any other harness name is
//! run as a program, with no hooks, trust or resume Fleet knows
//! ([`bare`]). The format is in docs/fleet/controllers/task.md, *Harness profiles*.
//!
//! Fleet ships the profiles of Claude Code, Codex, pi, opencode, kilo, omp and
//! qwen. pi, opencode, kilo, omp and qwen take their hooks as a file: Fleet
//! writes each under its own data dir (`fleet_runtime::assets`), and the
//! launch names it, never the agent's config dir.
//!
//! A harness with no prompt hook cannot prove a delivery: its runtime's
//! answer is all Fleet has.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

mod bundled;

/// The variable Fleet names a session's harness in at spawn, so that
/// `fleet signal` reads the payload fields of that harness's profile.
pub const HARNESS_ENV: &str = "FLEET_HARNESS";

/// The directory under Fleet's config dir that holds the user's profiles.
pub const USER_DIR: &str = "harnesses";

/// The directory under Fleet's data dir that holds the plugin files Fleet
/// ships for the harnesses that take their hooks as a file
/// (`fleet_runtime::assets`).
pub const ASSET_DIR: &str = "harness-assets";

/// The variable Fleet names that directory in at spawn, so `fleet signal`
/// reads the profiles the session was launched with, whatever config the
/// hook's own `fleet` would find.
pub const HARNESS_DIR_ENV: &str = "FLEET_HARNESS_DIR";

/// What the agent's hooks report, which decides how far Fleet trusts them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Authority {
    /// The hooks report working, idle and blocked.
    Full,
    /// The hooks report some states (Codex's `notify`: idle and prompts).
    Partial,
    /// The hooks report the conversation id only.
    Session,
    /// No hooks.
    None,
}

/// How a harness is trusted in its workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trust {
    /// A launch override trusting one directory for the session.
    LaunchOverride,
    /// None Fleet may use: a person trusts the directory once, and Fleet
    /// reads the agent's dialog to say so.
    Dialog,
    /// Fleet knows none for this harness.
    #[default]
    Unknown,
}

/// The `[launch]` table: what the launch argv adds after the program.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    /// The hooks reporting through `fleet signal`, and any other argument.
    #[serde(default)]
    pub args: Vec<String>,
    /// Variables the session starts with. The Task's own environment wins.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// The flag naming each other directory the session spans; none when
    /// the harness takes no such flag.
    #[serde(default)]
    pub add_dir: Option<String>,
}

/// The `[resume]` table: the argv that starts the agent again on its
/// conversation. Each `{program}` and `{conversation}` is replaced, and the
/// argument `{launch_args}` is the launch argv after its program. Empty: a
/// resume is a fresh start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resume {
    #[serde(default)]
    pub args: Vec<String>,
}

/// The `[trust]` table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustSpec {
    #[serde(default)]
    pub mode: Trust,
    /// The arguments that trust the session's root at launch; `{root}` is
    /// replaced with the root, escaped for a double-quoted string.
    #[serde(default)]
    pub args: Vec<String>,
}

/// The `[payload]` table: the fields of a hook's JSON that `fleet signal`
/// reads. Each prompt field is a string or a list of strings, and every one
/// present is a prompt; the first conversation field that is a string is
/// the conversation. An omitted list is the default one.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Payload {
    #[serde(default = "default_prompt_fields")]
    pub prompt: Vec<String>,
    #[serde(default = "default_conversation_fields")]
    pub conversation: Vec<String>,
}

fn default_prompt_fields() -> Vec<String> {
    vec!["prompt".into(), "input-messages".into()]
}

fn default_conversation_fields() -> Vec<String> {
    vec!["session_id".into(), "thread-id".into()]
}

/// The fields a hook is read with when its harness has no profile, or
/// names none: Claude Code's and Codex's.
impl Default for Payload {
    fn default() -> Payload {
        Payload {
            prompt: default_prompt_fields(),
            conversation: default_conversation_fields(),
        }
    }
}

/// The `[screen]` table: the bundled rule set that reads the agent's state
/// off its screen. Omitted: the set named as the profile, when Fleet ships
/// one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenSpec {
    #[serde(default)]
    pub rules: Option<String>,
}

/// One harness's profile: one TOML document.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    /// The program the launch argv starts.
    pub program: String,
    /// Other names a Task may give the harness.
    #[serde(default)]
    pub aliases: Vec<String>,
    pub authority: Authority,
    #[serde(default)]
    pub launch: Launch,
    #[serde(default)]
    pub resume: Resume,
    #[serde(default)]
    pub trust: TrustSpec,
    #[serde(default)]
    pub payload: Payload,
    #[serde(default)]
    pub screen: ScreenSpec,
}

/// Parse one profile.
pub fn parse(text: &str) -> Result<Profile, String> {
    let p: Profile = toml::from_str(text).map_err(|e| e.to_string())?;
    if p.name.trim().is_empty() || p.program.trim().is_empty() {
        return Err("a profile needs a name and a program".into());
    }
    if let Some(bad) = p
        .resume
        .args
        .iter()
        .find(|a| a.contains("{launch_args}") && *a != "{launch_args}")
    {
        return Err(format!(
            "resume argument {bad:?}: {{launch_args}} stands alone"
        ));
    }
    if p.trust.mode == Trust::LaunchOverride && p.trust.args.is_empty() {
        return Err("trust mode launch-override needs trust.args".into());
    }
    if let Some(name) = &p.screen.rules
        && crate::screen::rules(name).is_none()
    {
        return Err(format!(
            "screen.rules {name:?} names no rule set Fleet ships"
        ));
    }
    Ok(p)
}

/// The profile of a harness Fleet has none for: its name is run as a
/// program, with no hooks, trust or resume Fleet knows.
pub fn bare(harness: &str) -> Profile {
    Profile {
        name: harness.to_string(),
        program: harness.to_string(),
        aliases: Vec::new(),
        authority: Authority::None,
        launch: Launch::default(),
        resume: Resume::default(),
        trust: TrustSpec::default(),
        payload: Payload::default(),
        screen: ScreenSpec::default(),
    }
}

/// A set of profiles: the bundled ones, each replaced by a user profile of
/// the same name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profiles {
    list: Vec<Profile>,
    dir: Option<String>,
    assets: Option<String>,
}

impl Profiles {
    /// The profiles Fleet ships.
    pub fn bundled() -> Profiles {
        Profiles {
            list: bundled::ALL
                .iter()
                .map(|text| parse(text).expect("a bundled harness profile parses"))
                .collect(),
            dir: None,
            assets: None,
        }
    }

    /// These profiles, read with the user's from `dir`.
    pub fn from_dir(mut self, dir: String) -> Profiles {
        self.dir = Some(dir);
        self
    }

    /// These profiles, with each `{asset:<name>}` naming `<dir>/<name>`:
    /// the directory Fleet wrote its plugin files to.
    pub fn with_assets(mut self, dir: String) -> Profiles {
        self.assets = Some(dir);
        self
    }

    /// The directory the user's profiles were read from, when any was.
    pub fn dir(&self) -> Option<&str> {
        self.dir.as_deref()
    }

    /// These profiles with `user`'s: a user profile replaces the one of the
    /// same name, the last of several user profiles of one name wins, and a
    /// user profile is found before a bundled one.
    pub fn with(self, user: Vec<Profile>) -> Profiles {
        let mut list: Vec<Profile> = Vec::new();
        for p in user {
            list.retain(|q| q.name != p.name);
            list.push(p);
        }
        let shipped: Vec<Profile> = self
            .list
            .into_iter()
            .filter(|b| !list.iter().any(|u| u.name == b.name))
            .collect();
        list.extend(shipped);
        Profiles {
            list,
            dir: self.dir,
            assets: self.assets,
        }
    }

    /// The profile `harness` names, by name and then by alias; a bare one
    /// for a harness none names. Each `{asset:<name>}` in its launch and
    /// resume names the file in the asset directory, when one is set.
    pub fn get(&self, harness: &str) -> Profile {
        let mut p = self
            .list
            .iter()
            .find(|p| p.name == harness)
            .or_else(|| {
                self.list
                    .iter()
                    .find(|p| p.aliases.iter().any(|a| a == harness))
            })
            .cloned()
            .unwrap_or_else(|| bare(harness));
        if let Some(dir) = &self.assets {
            let name = |a: &mut String| *a = name_assets(a, dir);
            p.launch.args.iter_mut().for_each(name);
            p.launch.env.values_mut().for_each(name);
            p.resume.args.iter_mut().for_each(name);
        }
        p
    }

    /// Every name and alias, in order.
    pub fn names(&self) -> Vec<String> {
        self.list
            .iter()
            .flat_map(|p| std::iter::once(&p.name).chain(&p.aliases))
            .cloned()
            .collect()
    }
}

/// `arg` with each `{asset:<name>}` replaced by `<dir>/<name>`, its
/// separators `/`: Node reads them on every platform, and a JSON string
/// (opencode's config) holds them unescaped.
fn name_assets(arg: &str, dir: &str) -> String {
    let mut out = String::new();
    let mut rest = arg;
    while let Some(at) = rest.find("{asset:") {
        let Some(end) = rest[at..].find('}') else {
            break;
        };
        out.push_str(&rest[..at]);
        let name = &rest[at + "{asset:".len()..at + end];
        out.push_str(&format!("{dir}/{name}").replace('\\', "/"));
        rest = &rest[at + end + 1..];
    }
    out.push_str(rest);
    out
}

static INSTALLED: OnceLock<Profiles> = OnceLock::new();

/// Make `profiles` the set [`profile`] reads, once per process and before
/// any profile is read: the CLI installs the bundled profiles with the
/// user's. `false` when a set was already in force.
pub fn install(profiles: Profiles) -> bool {
    INSTALLED.set(profiles).is_ok()
}

/// The installed set, else the bundled one.
pub fn installed() -> &'static Profiles {
    INSTALLED.get_or_init(Profiles::bundled)
}

/// The profile of `harness`, from the installed set, else the bundled one.
pub fn profile(harness: &str) -> Profile {
    installed().get(harness)
}

impl Profile {
    /// Whether the agent's hooks report each prompt it is given, so a
    /// delivery can be proven by the agent rather than by the runtime.
    pub fn prompt_hook(&self) -> bool {
        matches!(self.authority, Authority::Full | Authority::Partial)
            && !self.payload.prompt.is_empty()
    }

    pub fn trust(&self) -> Trust {
        self.trust.mode
    }

    /// The rule set that reads the agent's state off its screen: the one
    /// `[screen]` names, else the one named as the profile; `None` when
    /// Fleet ships neither, and the screen is never read.
    pub fn screen_rules(&self) -> Option<&'static crate::screen::Rules> {
        crate::screen::rules(self.screen.rules.as_deref().unwrap_or(&self.name))
    }

    /// The arguments that install the hooks reporting through
    /// `fleet signal`.
    pub fn hook_args(&self) -> Vec<String> {
        self.launch.args.clone()
    }

    /// The arguments that trust `root` for this session, when the harness
    /// takes one at launch.
    pub fn trust_args(&self, root: &str) -> Result<Vec<String>, String> {
        if self.trust.mode != Trust::LaunchOverride {
            return Ok(Vec::new());
        }
        let escaped = escape_root(root)
            .ok_or_else(|| format!("cannot name {root:?} in a {} trust override", self.name))?;
        Ok(self
            .trust
            .args
            .iter()
            .map(|a| a.replace("{root}", &escaped))
            .collect())
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
        if let Some(flag) = &self.launch.add_dir {
            for d in extra_dirs {
                argv.extend([flag.clone(), d.clone()]);
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
        if self.resume.args.is_empty() {
            return launch.to_vec();
        }
        let mut argv = Vec::new();
        for a in &self.resume.args {
            if a == "{launch_args}" {
                argv.extend(rest.iter().cloned());
            } else {
                argv.push(
                    a.replace("{program}", program)
                        .replace("{conversation}", id),
                );
            }
        }
        argv
    }
}

/// `root` escaped for a double-quoted TOML or JSON string; `None` when it
/// holds a control character, which no such string may.
fn escape_root(root: &str) -> Option<String> {
    if root.chars().any(char::is_control) {
        return None;
    }
    Some(root.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Codex's launch override trusting `root`, as its bundled profile's
/// `[trust]` gives it.
pub fn codex_override(root: &str) -> Result<String, String> {
    let args = Profiles::bundled().get("codex").trust_args(root)?;
    Ok(args.last().cloned().unwrap_or_default())
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

/// Whether a plain visible screen `output` is Codex's initial empty
/// composer under its welcome screen, the cursor at its start: Codex takes
/// input, and reports nothing until its first turn ends. A trust dialog can
/// follow the composer, so the caller also rules out
/// [`codex_trust_dialog`].
pub fn codex_initial_composer(output: &str, cursor_row: usize, cursor_col: usize) -> bool {
    cursor_col == 2
        && output.lines().nth(cursor_row).map(str::trim_end) == Some("› Ask Codex to do anything")
        && output
            .lines()
            .any(|l| l.trim_start().starts_with(">_ OpenAI Codex ("))
        && output
            .lines()
            .any(|l| l.trim() == "To get started, describe a task or try one of these commands:")
}

/// Whether the screen's foreground process is Codex.
pub fn codex_process(foreground: &str) -> bool {
    matches!(
        foreground.rsplit(['/', '\\']).next(),
        Some("codex" | "codex.exe")
    )
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
            // Claude Code's notification is a permission prompt or a wait
            // for input: the agent needs a person.
            ("Notification", "blocked"),
        ] {
            let command = settings["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap();
            assert!(
                command.starts_with(&format!("fleet signal {state}")),
                "{command}"
            );
        }
        // Only a permission prompt needs a person: Claude Code also notifies
        // after 60 s of input idle (`idle_prompt`), which is no attention.
        assert_eq!(
            settings["hooks"]["Notification"][0]["matcher"],
            "permission_prompt"
        );
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
    fn pi_and_opencode_name_the_plugin_files_fleet_writes() {
        let all = Profiles::bundled().with_assets("/data/harness-assets".into());
        let pi = all.get("pi");
        let launch = pi.launch(&["/w/docs".into()], Some("/w")).unwrap();
        let extension = "/data/harness-assets/fleet-pi.ts";
        assert_eq!(launch, ["pi", "-e", extension, "--approve"]);
        assert_eq!(
            pi.resume(&launch, Some("s1")),
            ["pi", "-e", extension, "--approve", "--session", "s1"]
        );
        assert_eq!(pi.trust(), Trust::LaunchOverride);
        let opencode = all.get("opencode");
        let launch = opencode.launch(&[], None).unwrap();
        assert_eq!(launch, ["opencode", "--standalone"]);
        assert_eq!(
            opencode.launch.env["OPENCODE_CONFIG_CONTENT"],
            r#"{"plugins":["/data/harness-assets/fleet-opencode"]}"#
        );
        assert_eq!(
            opencode.resume(&launch, Some("ses_1")),
            ["opencode", "--standalone", "--session", "ses_1"]
        );
        assert!(pi.prompt_hook() && opencode.prompt_hook());
        // Each names a file under the asset directory.
        for p in [&pi, &opencode] {
            let named = p
                .launch
                .args
                .iter()
                .chain(p.launch.env.values())
                .any(|a| a.contains("/data/harness-assets/"));
            assert!(named, "{}", p.name);
        }
        // Without an asset directory a profile is left as written.
        assert_eq!(profile("pi").launch.args, ["-e", "{asset:fleet-pi.ts}"]);
        assert_eq!(
            name_assets(r"{asset:a} {asset:b/c}{asset:", r"C:\data"),
            "C:/data/a C:/data/b/c{asset:"
        );
    }

    #[test]
    fn kilo_names_its_server_and_terminal_plugins_in_its_own_variables() {
        let all = Profiles::bundled().with_assets("/data/harness-assets".into());
        let kilo = all.get("kilo");
        let launch = kilo.launch(&[], None).unwrap();
        assert_eq!(launch, ["kilo"]);
        assert_eq!(
            kilo.launch.env["KILO_CONFIG_CONTENT"],
            r#"{"plugin":["/data/harness-assets/fleet-kilo/index.js"]}"#
        );
        assert_eq!(
            kilo.launch.env["KILO_TUI_CONFIG"],
            "/data/harness-assets/fleet-kilo/tui.json"
        );
        assert_eq!(kilo.launch.env["KILO_NO_DAEMON"], "1");
        assert_eq!(
            kilo.resume(&launch, Some("ses_1")),
            ["kilo", "--session", "ses_1"]
        );
        assert!(kilo.prompt_hook());
        assert_eq!(kilo.trust(), Trust::Unknown);
    }

    #[test]
    fn omp_and_qwen_name_the_hook_files_fleet_writes() {
        let all = Profiles::bundled().with_assets("/data/harness-assets".into());
        let omp = all.get("omp");
        let launch = omp.launch(&["/w/docs".into()], Some("/w")).unwrap();
        let extension = "/data/harness-assets/fleet-omp.ts";
        assert_eq!(launch, ["omp", "-e", extension, "--add-dir", "/w/docs"]);
        assert_eq!(
            omp.resume(&launch, Some("01a1")),
            [
                "omp",
                "-e",
                extension,
                "--add-dir",
                "/w/docs",
                "--resume",
                "01a1"
            ]
        );
        let qwen = all.get("qwen");
        let launch = qwen.launch(&["/w/docs".into()], Some("/w")).unwrap();
        assert_eq!(launch, ["qwen", "--add-dir", "/w/docs"]);
        // Merged with the user's and the project's settings, hooks included.
        assert_eq!(
            qwen.launch.env["QWEN_CODE_SYSTEM_SETTINGS_PATH"],
            "/data/harness-assets/fleet-qwen.json"
        );
        assert_eq!(
            qwen.resume(&launch, Some("8031")),
            ["qwen", "--add-dir", "/w/docs", "--resume", "8031"]
        );
        for p in [&omp, &qwen] {
            assert!(p.prompt_hook(), "{}", p.name);
            assert_eq!(p.trust(), Trust::Unknown, "{}", p.name);
        }
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

    #[test]
    fn each_bundled_profile_parses_and_lists_its_names() {
        let all = Profiles::bundled();
        assert_eq!(
            all.names(),
            [
                "claude",
                "claude-code",
                "codex",
                "pi",
                "opencode",
                "kilo",
                "omp",
                "qwen"
            ]
        );
        assert_eq!(all.get("claude-code").name, "claude");
        assert_eq!(all.get("aider"), bare("aider"));
        assert!(profile("claude").prompt_hook());
        assert_eq!(profile("codex").trust(), Trust::LaunchOverride);
        assert_eq!(profile("claude").trust(), Trust::Dialog);
        assert_eq!(profile("aider").trust(), Trust::Unknown);
    }

    #[test]
    fn a_user_profile_replaces_the_bundled_one_of_its_name_or_adds_a_harness() {
        let stub = parse(
            r#"
name = "stub"
program = "stub-agent"
authority = "full"
[launch]
args = ["--hooks"]
env = { STUB_HOOKS = "on" }
[resume]
args = ["{program}", "--session={conversation}", "{launch_args}"]
[payload]
conversation = ["sessionID"]
"#,
        )
        .unwrap();
        let codex =
            parse("name = \"codex\"\nprogram = \"my-codex\"\nauthority = \"none\"\n").unwrap();
        let all = Profiles::bundled().with(vec![stub, codex]);
        let stub = all.get("stub");
        let launch = stub.launch(&["/x".into()], Some("/w")).unwrap();
        assert_eq!(launch, ["stub-agent", "--hooks"]);
        assert_eq!(stub.launch.env["STUB_HOOKS"], "on");
        assert_eq!(
            stub.resume(&launch, Some("s1")),
            ["stub-agent", "--session=s1", "--hooks"]
        );
        assert_eq!(stub.payload.prompt, Payload::default().prompt);
        assert_eq!(stub.payload.conversation, ["sessionID"]);
        let codex = all.get("codex");
        assert_eq!(codex.program, "my-codex");
        assert!(!codex.prompt_hook());
        assert_eq!(codex.resume(&["my-codex".into()], Some("t")), ["my-codex"]);
        assert_eq!(all.get("claude").program, "claude");
    }

    #[test]
    fn a_profile_with_an_unknown_table_or_a_misplaced_launch_args_is_refused() {
        let base = "name = \"x\"\nprogram = \"x\"\nauthority = \"full\"\n";
        assert!(parse(base).is_ok());
        assert!(parse(&format!("{base}[install]\nversion = 1\n")).is_err());
        assert!(parse(&format!("{base}[screen]\nmanifest = \"x\"\n")).is_err());
        assert!(parse(&format!("{base}[screen]\nrules = \"no-such-set\"\n")).is_err());
        assert!(parse(&format!("{base}[resume]\nargs = [\"a{{launch_args}}\"]\n")).is_err());
        assert!(parse(&format!("{base}[trust]\nmode = \"launch-override\"\n")).is_err());
        assert!(parse("name = \"\"\nprogram = \"x\"\nauthority = \"full\"\n").is_err());
        assert!(
            parse("name = \"x\"\nprogram = \"x\"\n").is_err(),
            "authority is required"
        );
    }
}
