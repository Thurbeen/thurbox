use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

mod bundled;

pub const HARNESS_ENV: &str = "FLEET_HARNESS";

pub const USER_DIR: &str = "harnesses";

pub const ASSET_DIR: &str = "harness-assets";

pub const HARNESS_DIR_ENV: &str = "FLEET_HARNESS_DIR";

pub const ASSET_DIR_ENV: &str = "FLEET_ASSET_DIR";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Authority {
    Full,
    Partial,
    Session,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trust {
    LaunchOverride,
    Dialog,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub add_dir: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resume {
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inbox {
    #[serde(default)]
    pub address: Option<String>,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustSpec {
    #[serde(default)]
    pub mode: Trust,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Payload {
    #[serde(default = "default_prompt_fields")]
    pub prompt: Vec<String>,
    #[serde(default = "default_conversation_fields")]
    pub conversation: Vec<String>,
    #[serde(default)]
    pub paste: Option<String>,
}

fn default_prompt_fields() -> Vec<String> {
    vec!["prompt".into(), "input-messages".into()]
}

fn default_conversation_fields() -> Vec<String> {
    vec!["session_id".into(), "thread-id".into()]
}

impl Default for Payload {
    fn default() -> Payload {
        Payload {
            prompt: default_prompt_fields(),
            conversation: default_conversation_fields(),
            paste: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenSpec {
    #[serde(default)]
    pub rules: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Install {
    pub version: u32,
    #[serde(default)]
    pub dir_env: Vec<String>,
    pub dir: String,
    #[serde(default)]
    pub windows: bool,
    #[serde(default)]
    pub files: Vec<OwnedFile>,
    #[serde(default)]
    pub blocks: Vec<OwnedBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedFile {
    pub path: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedBlock {
    pub path: String,
    pub text: String,
    #[serde(default = "default_comment")]
    pub comment: String,
}

fn default_comment() -> String {
    "#".into()
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetrySpec {
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collector {
    pub endpoint: String,
    pub protocol: String,
    pub headers: Option<String>,
}

pub fn names_collector(env: &BTreeMap<String, String>) -> bool {
    env.keys().any(|k| {
        let k = k.to_ascii_uppercase();
        k.contains("OTLP") && k.ends_with("ENDPOINT")
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentTelemetry {
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    pub program: String,
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
    #[serde(default)]
    pub install: Option<Install>,
    #[serde(default)]
    pub telemetry: TelemetrySpec,
    #[serde(default)]
    pub inbox: Option<Inbox>,
}

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
    if let Some(install) = &p.install {
        crate::integration::check(install)?;
    }
    if p.telemetry.args.iter().any(|a| a.contains("{headers}")) {
        return Err("telemetry.args may not name {headers}: use telemetry.env".into());
    }
    Ok(p)
}

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
        install: None,
        telemetry: TelemetrySpec::default(),
        inbox: None,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profiles {
    list: Vec<Profile>,
    dir: Option<String>,
    assets: Option<String>,
}

impl Profiles {
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

    pub fn from_dir(mut self, dir: String) -> Profiles {
        self.dir = Some(dir);
        self
    }

    pub fn with_assets(mut self, dir: String) -> Profiles {
        self.assets = Some(dir);
        self
    }

    pub fn dir(&self) -> Option<&str> {
        self.dir.as_deref()
    }

    pub fn assets(&self) -> Option<&str> {
        self.assets.as_deref()
    }

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

    pub fn list(&self) -> &[Profile] {
        &self.list
    }

    pub fn names(&self) -> Vec<String> {
        self.list
            .iter()
            .flat_map(|p| std::iter::once(&p.name).chain(&p.aliases))
            .cloned()
            .collect()
    }
}

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

pub fn install(profiles: Profiles) -> bool {
    INSTALLED.set(profiles).is_ok()
}

pub fn installed() -> &'static Profiles {
    INSTALLED.get_or_init(Profiles::bundled)
}

pub fn profile(harness: &str) -> Profile {
    installed().get(harness)
}

impl Profile {
    pub fn prompt_hook(&self) -> bool {
        matches!(self.authority, Authority::Full | Authority::Partial)
            && !self.payload.prompt.is_empty()
    }

    pub fn trust(&self) -> Trust {
        self.trust.mode
    }

    pub fn screen_rules(&self) -> Option<&'static crate::screen::Rules> {
        crate::screen::rules(self.screen.rules.as_deref().unwrap_or(&self.name))
    }

    pub fn hook_args(&self) -> Vec<String> {
        self.launch.args.clone()
    }

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

    pub fn telemetry(&self, collector: &Collector, task: &str, uid: &str) -> AgentTelemetry {
        let resource = [
            ("service.name", self.name.as_str()),
            ("fleet.task", task),
            ("fleet.uid", uid),
        ]
        .iter()
        .map(|(k, v)| format!("{k}={}", percent_encode(v)))
        .collect::<Vec<_>>()
        .join(",");
        let headers = collector.headers.as_deref().unwrap_or_default();
        let fill = |text: &str, endpoint: &str| {
            text.replace("{endpoint}", endpoint)
                .replace("{protocol}", &collector.protocol)
                .replace("{headers}", headers)
                .replace("{resource}", &resource)
        };
        let args = match escape_root(&collector.endpoint) {
            Some(endpoint) => self
                .telemetry
                .args
                .iter()
                .map(|a| fill(a, &endpoint))
                .collect(),
            None => Vec::new(),
        };
        let env = self
            .telemetry
            .env
            .iter()
            .filter(|(_, v)| collector.headers.is_some() || !v.contains("{headers}"))
            .map(|(k, v)| (k.clone(), fill(v, &collector.endpoint)))
            .collect();
        AgentTelemetry { args, env }
    }

    pub fn inbox(
        &self,
        address: Option<&str>,
        conversation: Option<&str>,
        text: &str,
    ) -> Option<Vec<String>> {
        let inbox = self.inbox.as_ref()?;
        inbox
            .args
            .iter()
            .map(|a| {
                let mut arg = a.replace("{program}", &self.program);
                for (name, value) in [("{address}", address), ("{conversation}", conversation)] {
                    if arg.contains(name) {
                        arg = arg.replace(name, value?);
                    }
                }
                Some(arg.replace("{text}", text))
            })
            .collect()
    }

    pub fn resumes(&self) -> bool {
        !self.resume.args.is_empty()
    }

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

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn escape_root(root: &str) -> Option<String> {
    if root.chars().any(char::is_control) {
        return None;
    }
    Some(root.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn codex_override(root: &str) -> Result<String, String> {
    let args = Profiles::bundled().get("codex").trust_args(root)?;
    Ok(args.last().cloned().unwrap_or_default())
}

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

pub fn codex_process(foreground: &str) -> bool {
    matches!(
        foreground.rsplit(['/', '\\']).next(),
        Some("codex" | "codex.exe")
    )
}

pub fn kimi_initial_composer(output: &str, cursor_row: usize, cursor_col: usize) -> bool {
    let lines: Vec<&str> = output.lines().collect();
    let Some(line) = lines.get(cursor_row) else {
        return false;
    };
    let framed = |at: Option<usize>, corner: char| {
        at.and_then(|at| lines.get(at))
            .is_some_and(|l| l.trim_start().starts_with(corner))
    };
    let empty = line
        .trim()
        .strip_prefix('│')
        .and_then(|l| l.strip_suffix('│'))
        .is_some_and(|l| l.trim() == ">");
    empty
        && line.chars().position(|c| c == '>').map(|at| at + 2) == Some(cursor_col)
        && framed(cursor_row.checked_sub(1), '╭')
        && framed(cursor_row.checked_add(1), '╰')
        && lines[cursor_row..]
            .iter()
            .any(|l| l.trim_start().starts_with("context: "))
}

pub fn kimi_process(foreground: &str) -> bool {
    matches!(
        foreground.rsplit(['/', '\\']).next(),
        Some("kimi" | "kimi.exe" | "kimi-code" | "kimi-code.exe")
    )
}

pub fn copilot_initial_composer(output: &str, cursor_row: usize, cursor_col: usize) -> bool {
    let lines: Vec<&str> = output.lines().collect();
    let rule = |at: Option<usize>| {
        at.and_then(|at| lines.get(at))
            .map(|l| l.trim())
            .is_some_and(|l| !l.is_empty() && l.chars().all(|c| c == '─'))
    };
    cursor_col == 2
        && lines.get(cursor_row).map(|l| l.trim_end()) == Some("❯")
        && rule(cursor_row.checked_sub(1))
        && rule(cursor_row.checked_add(1))
        && lines[cursor_row..]
            .iter()
            .any(|l| l.contains("· / commands"))
}

// Copilot's npm package leads its process group as `node`; the native binary is a child.
pub fn copilot_process(foreground: &str) -> bool {
    matches!(
        foreground.rsplit(['/', '\\']).next(),
        Some("copilot" | "copilot.exe" | "node" | "node.exe")
    )
}

pub fn copilot_trust_dialog(output: &str) -> bool {
    output
        .lines()
        .any(|l| l.contains("Do you trust the files in this folder?"))
        && output
            .lines()
            .any(|l| l.contains("Yes, and remember this folder"))
}

pub fn claude_initial_composer(output: &str, cursor_row: usize, cursor_col: usize) -> bool {
    let lines: Vec<&str> = output.lines().collect();
    let rule = |at: Option<usize>| {
        at.and_then(|at| lines.get(at))
            .map(|l| l.trim())
            .is_some_and(|l| !l.is_empty() && l.chars().all(|c| c == '─'))
    };
    cursor_col == 2
        && lines
            .get(cursor_row)
            .is_some_and(|l| l.starts_with("❯ ") || l.trim_end() == "❯")
        && rule(cursor_row.checked_sub(1))
        && rule(cursor_row.checked_add(1))
        && lines[cursor_row..]
            .iter()
            .any(|l| l.contains("? for shortcuts"))
}

pub fn claude_process(foreground: &str) -> bool {
    matches!(
        foreground.rsplit(['/', '\\']).next(),
        Some("claude" | "claude.exe" | "node" | "node.exe")
    )
}

// Kimi Code's trust dialog asks in the same words as Codex's.
pub fn codex_trust_dialog(output: &str) -> bool {
    let lines: Vec<&str> = output.lines().map(str::trim).collect();
    lines.iter().any(|l| l.starts_with("Trust this folder?"))
        && lines.iter().any(|l| l.contains("Trust and continue"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_composer_is_its_placeholder_between_rules_with_the_cursor_after_its_prompt() {
        let rule = "─".repeat(80);
        let screen = format!(
            " Claude Code v2.1.322\n{rule}\n❯ Try \"create a util logging.py that...\"\n{rule}\n  ⏸ manual mode on · ? for shortcuts\n"
        );
        assert!(claude_initial_composer(&screen, 2, 2));
        assert!(
            !claude_initial_composer(&screen, 2, 9),
            "a draft moved the cursor"
        );
        assert!(!claude_initial_composer(&screen, 1, 2));
        let trust = " Accessing workspace:\n\n ❯ No, exit\n   Yes, I trust this folder\n\n Enter to confirm · Esc to cancel\n";
        assert!(!claude_initial_composer(trust, 2, 2));
        assert!(claude_process("/home/u/.local/bin/claude") && claude_process("node"));
        assert!(!claude_process("codex"));
    }

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
        // Claude Code also notifies after 60 s of input idle (`idle_prompt`).
        assert_eq!(
            settings["hooks"]["Notification"][0]["matcher"],
            "permission_prompt"
        );
        assert_eq!(&claude[3..], ["--add-dir", "/w/docs"]);
        // Claude Code keeps the last of several `--settings`.
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
    fn an_inbox_argv_names_what_the_hooks_reported() {
        assert_eq!(
            profile("claude").inbox(Some("/run/s.sock"), None, "- hi {address}"),
            Some(vec![
                "fleet".into(),
                "inbox-post".into(),
                "/run/s.sock".into(),
                "--".into(),
                "- hi {address}".into()
            ])
        );
        assert_eq!(profile("claude").inbox(None, Some("c1"), "hi"), None);
        assert_eq!(
            profile("codex").inbox(None, Some("t9"), "- hi"),
            Some(
                ["codex", "queue", "--thread", "t9", "--message=- hi"]
                    .map(String::from)
                    .to_vec()
            )
        );
        assert_eq!(profile("codex").inbox(None, None, "hi"), None);
        assert_eq!(profile("aider").inbox(Some("a"), Some("c"), "hi"), None);
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
        for p in [&pi, &opencode] {
            let named = p
                .launch
                .args
                .iter()
                .chain(p.launch.env.values())
                .any(|a| a.contains("/data/harness-assets/"));
            assert!(named, "{}", p.name);
        }
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
        // omp otherwise opens its first-run setup wizard.
        assert_eq!(omp.launch.env["OMP_SKIP_SETUP"], "1");
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
        // qwen merges system, user and project settings; only its system defaults file is replaced.
        assert_eq!(
            qwen.launch.env,
            [(
                "QWEN_CODE_SYSTEM_DEFAULTS_PATH".to_string(),
                "/data/harness-assets/fleet-qwen.json".to_string()
            )]
            .into()
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
    fn copilot_names_the_plugin_directory_fleet_writes() {
        let all = Profiles::bundled().with_assets("/data/harness-assets".into());
        let copilot = all.get("copilot");
        let launch = copilot.launch(&["/w/docs".into()], Some("/w")).unwrap();
        let plugin = "/data/harness-assets/fleet-copilot";
        assert_eq!(
            launch,
            ["copilot", "--plugin-dir", plugin, "--add-dir", "/w/docs"]
        );
        assert!(copilot.launch.env.is_empty());
        assert_eq!(
            copilot.resume(&launch, Some("8f1c")),
            [
                "copilot",
                "--plugin-dir",
                plugin,
                "--add-dir",
                "/w/docs",
                "--resume",
                "8f1c"
            ]
        );
        assert!(copilot.prompt_hook());
        assert_eq!(copilot.payload.conversation, ["sessionId"]);
        assert_eq!(copilot.trust(), Trust::Dialog);
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

    const KIMI_READY: &str =
        " │                                                                            │
 │  ▐█▛█▛█▌  Welcome to Kimi Code!                                            │
 │  ▐█████▌  Send /help for help information.                                 │
 │                                                                            │
 │  Directory: /home/u/work                                                   │
 │  Session:                                                                  │
 │  Model:     fake-model                                                     │
 │  Version:   2.5.1                                                          │
 │                                                                            │
 ╰────────────────────────────────────────────────────────────────────────────╯

 ✦ Kimi K3.1 Pro is ready - use the model in a fresh session for the best
                          results
   Standard: Plus/Moderato and above; Highspeed: Pro/Allegretto and above

   No session yet — one will be created on your first message.
   tmux extended-keys is off. Modified Enter keys may not work. Add `set -g
 extended-keys on` to ~/.tmux.conf and restart tmux.

 ╭────────────────────────────────────────────────────────────────────────────╮
 │ >                                                                          │
 ╰────────────────────────────────────────────────────────────────────────────╯
 fake-model thinking  /home/u/work
                                                           context: 0% (0/256k)
";

    const KIMI_TRUST: &str = " ──────────────────────────────────────────────────
  Trust this folder?
  ↑↓ navigate · Enter select · Esc exit

  /home/u/work

  No project integrations or instructions to activate.

  Trust is remembered for this folder, including future project config changes.
  Tool approvals follow your permission settings.

   ❯ Trust and continue
     Exit
 ──────────────────────────────────────────────────
";

    #[test]
    fn only_kimis_empty_composer_under_the_cursor_is_its_initial_composer() {
        assert!(kimi_initial_composer(KIMI_READY, 20, 5));
        assert!(!kimi_initial_composer(KIMI_READY, 20, 4));
        assert!(!kimi_initial_composer(KIMI_READY, 19, 5));
        let typed = KIMI_READY.replace("│ >  ", "│ > R");
        assert!(!kimi_initial_composer(&typed, 20, 5));
        let no_top = KIMI_READY.replace(" ╭─", " ──");
        assert!(!kimi_initial_composer(&no_top, 20, 5));
        let no_bottom: String = KIMI_READY
            .lines()
            .enumerate()
            .map(|(row, l)| match row {
                21 => format!("{}\n", l.replace('╰', "─")),
                _ => format!("{l}\n"),
            })
            .collect();
        assert!(!kimi_initial_composer(&no_bottom, 20, 5));
        let no_footer = KIMI_READY.replace("context:", "ctx");
        assert!(!kimi_initial_composer(&no_footer, 20, 5));
        assert!(!kimi_initial_composer(KIMI_TRUST, 11, 5));
        assert!(!kimi_initial_composer("", 0, 5));
        assert!(codex_trust_dialog(KIMI_TRUST));
        assert!(!codex_trust_dialog(KIMI_READY));
        assert!(kimi_process("kimi-code"));
        assert!(kimi_process("/usr/bin/kimi"));
        assert!(!kimi_process("codex"));
    }

    const COPILOT_READY: &str = " [Current]  Sessions   Issues   Pull requests   Gists

  ╭─╮╭─╮
  ╰─╯╰─╯  Copilot v1.0.105 uses AI.
  █ ▘▝ █  Check for mistakes.
   ▔▔▔▔

 ● Tip: /app
   └ Prefer a visual workspace? Try out the GitHub Copilot desktop app
      https://github.com/features/ai/github-app 

 ! Model \"fake-model\" is not in the built-in catalog. Using defaults for:
   prompt tokens (COPILOT_PROVIDER_MAX_PROMPT_TOKENS), output tokens
   (COPILOT_PROVIDER_MAX_OUTPUT_TOKENS). Run  copilot help providers  for
   configuration details.

 ● MCP Servers reloaded: 0 servers connected


 ~/work
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
 ← open sidebar · Interactive · Manual Approval · / commands         fake-model
";

    const COPILOT_TRUST: &str = " [Current]  Sessions   Issues   Pull requests   Gists

                                                                               ┃
╭──────────────────────────────────────────────────────────────────────────────╮
│ Confirm folder trust                                                         │
│ ──────────────────────────────────────────────────────────────────────────── │
│ ╭──────────────────────────────────────────────────────────────────────────╮ │
│ │ /home/u/work                                                             │ │
│ ╰──────────────────────────────────────────────────────────────────────────╯ │
│                                                                              │
│ Copilot can read files in this folder and, with your permission, edit them   │
│ or run code and shell commands. It will remember your permissions for the    │
│ rest of this session.                                                        │
│                                                                              │
│ Do you trust the files in this folder?                                       │
│                                                                              │
│ ❯ 1. Yes                                                                     │
│   2. Yes, and remember this folder for future sessions                       │
│   3. No (Esc)                                                                │
│                                                                              │
│ ↑/↓ to navigate · enter to select · esc to cancel                            │
╰──────────────────────────────────────────────────────────────────────────────╯
";

    #[test]
    fn only_copilots_empty_composer_under_the_cursor_is_its_initial_composer() {
        assert!(copilot_initial_composer(COPILOT_READY, 21, 2));
        assert!(!copilot_initial_composer(COPILOT_READY, 21, 3));
        assert!(!copilot_initial_composer(COPILOT_READY, 20, 2));
        let typed = COPILOT_READY.replace("\n❯\n", "\n❯ Run it\n");
        assert!(!copilot_initial_composer(&typed, 21, 2));
        for rule in [20, 22] {
            let no_rule: String = COPILOT_READY
                .lines()
                .enumerate()
                .map(|(row, l)| {
                    if row == rule {
                        "\n".into()
                    } else {
                        format!("{l}\n")
                    }
                })
                .collect();
            assert!(!copilot_initial_composer(&no_rule, 21, 2), "{rule}");
        }
        let no_footer = COPILOT_READY.replace("/ commands", "commands");
        assert!(!copilot_initial_composer(&no_footer, 21, 2));
        assert!(!copilot_initial_composer(COPILOT_TRUST, 18, 2));
        assert!(!copilot_initial_composer("", 0, 2));
        assert!(copilot_trust_dialog(COPILOT_TRUST));
        assert!(!copilot_trust_dialog(COPILOT_READY));
        assert!(!codex_trust_dialog(COPILOT_TRUST));
        assert!(copilot_process("copilot"));
        assert!(copilot_process("node"));
        assert!(copilot_process("/usr/bin/copilot"));
        assert!(!copilot_process("codex"));
    }

    fn collector(headers: Option<&str>) -> Collector {
        Collector {
            endpoint: "http://c:4318".into(),
            protocol: "http/protobuf".into(),
            headers: headers.map(str::to_string),
        }
    }

    #[test]
    fn each_profile_renders_its_own_telemetry_for_fleets_collector() {
        let c = collector(Some("authorization=Bearer s"));
        let render = |h: &str| profile(h).telemetry(&c, "t1", "u-1");
        let resource = |h: &str| format!("service.name={h},fleet.task=t1,fleet.uid=u-1");
        let env = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let claude = render("claude-code");
        assert!(claude.args.is_empty());
        assert_eq!(
            claude.env,
            env(&[
                ("CLAUDE_CODE_ENABLE_TELEMETRY", "1"),
                ("OTEL_METRICS_EXPORTER", "otlp"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf"),
                ("OTEL_EXPORTER_OTLP_HEADERS", "authorization=Bearer s"),
                ("OTEL_SERVICE_NAME", "claude"),
                ("OTEL_RESOURCE_ATTRIBUTES", &resource("claude")),
            ])
        );

        let codex = render("codex");
        assert_eq!(
            codex.args,
            [
                "-c",
                r#"otel.exporter={otlp-http={endpoint="http://c:4318/v1/logs",protocol="binary"}}"#,
                "-c",
                r#"otel.trace_exporter={otlp-http={endpoint="http://c:4318/v1/traces",protocol="binary"}}"#,
                "-c",
                r#"otel.metrics_exporter={otlp-http={endpoint="http://c:4318/v1/metrics",protocol="binary"}}"#,
            ]
        );
        assert_eq!(
            codex.env,
            env(&[("OTEL_RESOURCE_ATTRIBUTES", &resource("codex"))])
        );

        assert_eq!(
            render("opencode"),
            AgentTelemetry {
                args: Vec::new(),
                env: env(&[
                    ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318"),
                    ("OTEL_EXPORTER_OTLP_HEADERS", "authorization=Bearer s"),
                    ("OTEL_RESOURCE_ATTRIBUTES", &resource("opencode")),
                ])
            }
        );
        assert_eq!(
            render("omp").env,
            env(&[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf"),
                ("OTEL_EXPORTER_OTLP_HEADERS", "authorization=Bearer s"),
                ("OTEL_SERVICE_NAME", "omp"),
                ("OTEL_RESOURCE_ATTRIBUTES", &resource("omp")),
            ])
        );
        // qwen logs prompts by default once its telemetry is on.
        assert_eq!(
            render("qwen").env,
            env(&[
                ("QWEN_TELEMETRY_ENABLED", "true"),
                ("QWEN_TELEMETRY_OTLP_ENDPOINT", "http://c:4318"),
                ("QWEN_TELEMETRY_OTLP_PROTOCOL", "http"),
                ("QWEN_TELEMETRY_LOG_PROMPTS", "false"),
                ("OTEL_EXPORTER_OTLP_HEADERS", "authorization=Bearer s"),
                ("OTEL_SERVICE_NAME", "qwen"),
                ("OTEL_RESOURCE_ATTRIBUTES", &resource("qwen")),
            ])
        );
        assert_eq!(
            render("copilot").env,
            env(&[
                ("COPILOT_OTEL_ENABLED", "true"),
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://c:4318"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf"),
                ("OTEL_EXPORTER_OTLP_HEADERS", "authorization=Bearer s"),
                ("OTEL_SERVICE_NAME", "copilot"),
                ("OTEL_RESOURCE_ATTRIBUTES", &resource("copilot")),
            ])
        );
        for none in ["pi", "kilo", "kimi", "aider"] {
            assert_eq!(render(none), AgentTelemetry::default(), "{none}");
        }
        for p in Profiles::bundled().list() {
            let t = p.telemetry(&c, "t1", "u-1");
            for k in t.env.keys() {
                assert!(
                    !k.contains("LOG_USER_PROMPTS")
                        && !k.contains("CAPTURE")
                        && !k.starts_with("OTEL_LOG_")
                        && k != "TRACEPARENT",
                    "{}: {k}",
                    p.name
                );
            }
        }
    }

    #[test]
    fn an_otlp_endpoint_in_any_case_names_a_collector() {
        let env = |k: &str| BTreeMap::from([(k.to_string(), "http://x".to_string())]);
        for k in [
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "otel_exporter_otlp_traces_endpoint",
            "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
            "QWEN_TELEMETRY_OTLP_ENDPOINT",
        ] {
            assert!(names_collector(&env(k)), "{k}");
        }
        for k in [
            "OTEL_EXPORTER_OTLP_HEADERS",
            "OTEL_SERVICE_NAME",
            "ENDPOINT",
        ] {
            assert!(!names_collector(&env(k)), "{k}");
        }
    }

    #[test]
    fn telemetry_without_headers_leaves_the_header_variable_out_and_encodes_the_resource() {
        let claude = profile("claude").telemetry(&collector(None), "a,b=c", "u 1");
        assert!(!claude.env.contains_key("OTEL_EXPORTER_OTLP_HEADERS"));
        assert_eq!(
            claude.env["OTEL_RESOURCE_ATTRIBUTES"],
            "service.name=claude,fleet.task=a%2Cb%3Dc,fleet.uid=u%201"
        );
        let quoted = Collector {
            endpoint: r#"http://c/"x""#.into(),
            ..collector(None)
        };
        assert!(
            profile("codex").telemetry(&quoted, "t", "u").args[1]
                .contains(r#"endpoint="http://c/\"x\"/v1/logs""#)
        );
        let base = "name = \"x\"\nprogram = \"x\"\nauthority = \"none\"\n";
        assert!(parse(&format!("{base}[telemetry]\nargs = [\"h={{headers}}\"]\n")).is_err());
        assert!(
            parse(&format!(
                "{base}[telemetry]\nenv = {{ H = \"{{headers}}\" }}\n"
            ))
            .is_ok()
        );
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
                "qwen",
                "copilot",
                "kimi"
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
        assert!(parse(&format!("{base}[install]\nversion = 1\ndir = \"~/.x\"\n")).is_err());
        let file = "[[install.files]]\npath = \"a\"\ntext = \"t\"\n";
        let install = |version: u32, path: &str| {
            format!(
                "{base}[install]\nversion = {version}\ndir = \"~/.x\"\n{}",
                file.replace("\"a\"", &format!("{path:?}"))
            )
        };
        assert!(parse(&install(1, "hooks/fleet.json")).is_ok());
        assert!(parse(&install(0, "a")).is_err(), "a version starts at 1");
        assert!(parse(&install(1, "../a")).is_err());
        assert!(parse(&install(1, "/etc/a")).is_err());
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
