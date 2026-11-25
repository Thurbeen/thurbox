mod bus;
mod execute;
pub(crate) mod fleet;
pub(crate) use fleet::task_name;

pub use bus::{CommandBus, InFlight, Phase};

trait Executor: Send + Sync {
    fn execute(
        &self,
        command: &Command,
        id: u64,
        progress: &std::sync::mpsc::Sender<bus::Progress>,
    ) -> Result<execute::Outcome, String>;
}

use super::registry::Value as SettingValue;

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Guarded {
        inner: Box<Command>,
        session: String,
        backend_id: Option<String>,
        cwd: Option<std::path::PathBuf>,
        member_dirs: Vec<std::path::PathBuf>,
    },
    Delete { session: String, force: bool },
    Send { session: String, text: String },
    Start { session: String },
    Pause { session: String },
    Restart { session: String, fresh: bool },
    Order { list: Vec<String> },
    Create {
        name: String,
        repo: String,
        agent: Option<String>,
        host: Option<String>,
        fleet: Option<String>,
        worktree: Option<bool>,
        branch: Option<String>,
        base: Option<String>,
        extras: Vec<ExtraMember>,
    },
    Bookmark {
        host: String,
        path: String,
        edit: BookmarkEdit,
    },
    Rename {
        session: String,
        name: String,
    },
    Copy {
        session: String,
    },
    Diff {
        session: String,
    },
    Focus {
        mod_name: String,
        toggle: bool,
    },
    Shell {
        session: String,
    },
    Program {
        owner: String,
        name: String,
        program: String,
        argv: Vec<String>,
        close: bool,
        keys: Option<Vec<u8>>,
    },
    Editor {
        session: String,
    },
    OpenLink {
        url: String,
    },
    Setting {
        key: String,
        value: Option<SettingValue>,
    },
    Theme {
        name: String,
    },
    Configure {
        settings: Box<crate::session::settings::Settings>,
    },
    Mod {
        file: String,
        edit: ModEdit,
    },
    Emit {
        owner: String,
        name: String,
        payload: Vec<(String, super::events::Field)>,
    },
    Action {
        fallback: String,
        action: String,
    },
    ActionTarget {
        fallback: String,
        action: String,
        argument: String,
        value: String,
    },
    Message {
        text: String,
        level: super::bands::Level,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookmarkEdit {
    Add,
    Remove,
    Parent,
    Create,
    Init,
    Clone { url: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModEdit {
    Restore,
    Remove,
}

impl Command {
    pub fn kind(&self) -> &'static str {
        match self {
            Command::Guarded { inner, .. } => inner.kind(),
            Command::Delete { .. } => "delete",
            Command::Send { .. } => "send",
            Command::Start { .. } => "start",
            Command::Pause { .. } => "pause",
            Command::Restart { .. } => "restart",
            Command::Create { .. } => "create",
            Command::Rename { .. } => "rename",
            Command::Copy { .. } => "copy",
            Command::Diff { .. } => "diff",
            Command::Shell { .. } => "shell",
            Command::Program { .. } => "program",
            Command::Editor { .. } => "editor",
            Command::Focus { .. } => "focus",
            Command::OpenLink { .. } => "open",
            Command::Theme { .. } => "theme",
            Command::Mod { .. } => "plugin",
            Command::Bookmark { .. } => "bookmark",
            Command::Configure { .. } => "configure",
            Command::Order { .. } => "order",
            Command::Setting { .. } => "set",
            Command::Emit { .. } => "emit",
            Command::Action { .. } | Command::ActionTarget { .. } => "action",
            Command::Message { .. } => "message",
        }
    }

    pub fn session(&self) -> &str {
        match self {
            Command::Guarded { session, .. } => session,
            Command::Delete { session, .. }
            | Command::Send { session, .. }
            | Command::Start { session }
            | Command::Pause { session }
            | Command::Restart { session, .. }
            | Command::Rename { session, .. }
            | Command::Copy { session }
            | Command::Diff { session }
            | Command::Editor { session }
            | Command::Shell { session } => session,
            Command::ActionTarget {
                argument, value, ..
            } if argument == "session_id" => value,
            Command::ActionTarget { .. } => "",
            Command::Create { .. } => "",
            Command::Theme { .. }
            | Command::Mod { .. }
            | Command::Bookmark { .. }
            | Command::Configure { .. }
            | Command::Setting { .. }
            | Command::OpenLink { .. }
            | Command::Order { .. }
            | Command::Program { .. }
            | Command::Emit { .. }
            | Command::Action { .. }
            | Command::Message { .. }
            | Command::Focus { .. } => "",
        }
    }

    pub fn applied_on_ui_thread(&self) -> bool {
        matches!(
            self,
            Command::Theme { .. }
                | Command::Setting { .. }
                | Command::Copy { .. }
                | Command::OpenLink { .. }
                | Command::Shell { .. }
                | Command::Program { .. }
                | Command::Editor { .. }
                | Command::Focus { .. }
                | Command::Mod { .. }
                | Command::Emit { .. }
                | Command::Action { .. }
                | Command::ActionTarget { .. }
                | Command::Message { .. }
        )
    }

    pub fn requires(&self) -> Option<super::host::Capability> {
        use super::host::Capability;
        match self {
            Command::Guarded { inner, .. } => inner.requires(),
            Command::OpenLink { .. } => Some(Capability::Run),
            Command::Program { .. } => Some(Capability::Program),
            Command::Send { .. }
            | Command::Pause { .. }
            | Command::Restart { .. }
            | Command::Create { .. }
            | Command::Mod { .. }
            | Command::Delete { .. } => Some(Capability::Sessions),
            Command::Order { .. }
            | Command::Rename { .. }
            | Command::Start { .. }
            | Command::Bookmark { .. } => None,
            Command::Focus { .. }
            | Command::Shell { .. }
            | Command::Editor { .. }
            | Command::Copy { .. }
            | Command::Diff { .. }
            | Command::Theme { .. }
            | Command::Setting { .. }
            | Command::Message { .. }
            | Command::Emit { .. } => None,
            Command::Action { action, .. } | Command::ActionTarget { action, .. }
                if action == super::clipboard::PASTE_ACTION =>
            {
                Some(Capability::Sessions)
            }
            Command::Action { .. } | Command::ActionTarget { .. } => None,
            Command::Configure { .. } => None,
        }
    }

    pub fn runs_for(&self, chain: &[String]) -> Option<Vec<String>> {
        matches!(self, Command::Action { .. } | Command::ActionTarget { .. })
            .then(|| chain.to_vec())
    }

    pub fn refusal(
        &self,
        owner: &str,
        may: impl Fn(&str, super::host::Capability) -> bool,
    ) -> Option<String> {
        self.refusal_by(&[owner.to_string()], may)
    }

    pub fn refusal_by(
        &self,
        chain: &[String],
        may: impl Fn(&str, super::host::Capability) -> bool,
    ) -> Option<String> {
        let need = self.requires()?;
        let owner = chain
            .iter()
            .find(|member| !may(member, need))
            .map(String::as_str)
            .or_else(|| chain.is_empty().then_some(""))?;
        let who = if owner.is_empty() {
            "Lua outside any mod"
        } else {
            owner
        };
        let doing = match self.kind() {
            "send" => "type into a session",
            "create" => "create a session",
            "plugin" => "change a file of the interface",
            "open" => "open a link",
            "program" => "run a program in a pane",
            "action" => "paste into a session",
            _ => "delete a session for good",
        };
        Some({
            format!(
                "{who} may not {doing} without the {:?} capability — declare it and trust \
                 the file in settings → Interface",
                need.as_str()
            )
        })
    }

    pub fn subject(&self) -> Option<String> {
        match self {
            Command::Create { repo, .. } => std::path::Path::new(repo)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .or_else(|| Some(repo.clone())),
            Command::Bookmark { path, .. } => Some(path.clone()),
            _ => None,
        }
    }

    // MUST KILL: type — a `HostName` newtype would settle the bare vs `ssh:`-prefixed spelling once
    pub fn host(&self) -> Option<String> {
        match self {
            Command::Create { host, .. } => host.as_deref().map(|name| {
                crate::session::host_of_key(name)
                    .unwrap_or(name)
                    .to_string()
            }),
            _ => None,
        }
    }

    pub fn parse(kind: &str, args: Args) -> Result<Self, String> {
        match kind {
            "emit" => Self::parse_emit(args),
            "action" => Self::parse_action(args),
            "message" => Self::parse_message(args),
            "mod" | "plugin" => Self::parse_mod(args),
            "set" => Self::parse_setting(args),
            "create" => Self::parse_create(args),
            "bookmark" => Self::parse_bookmark(args),
            "focus" => match args.text.filter(|t| !t.is_empty()) {
                Some(mod_name) => Ok(Command::Focus {
                    mod_name,
                    toggle: args.toggle,
                }),
                None => Err("command \"focus\" needs a mod name".to_string()),
            },
            "open" => match args.text.filter(|t| !t.is_empty()) {
                Some(url) => Ok(Command::OpenLink { url }),
                None => Err("command \"open\" needs a url in text".to_string()),
            },
            "theme" => match args.text {
                Some(name) if !name.is_empty() => Ok(Command::Theme { name }),
                _ => Err("command \"theme\" needs a name".to_string()),
            },
            "order" => {
                if args.list.is_empty() {
                    Err("command \"order\" needs a list of session ids".to_string())
                } else {
                    Ok(Command::Order { list: args.list })
                }
            }
            "program" => Self::parse_program(args),
            _ => Self::parse_session_command(kind, args),
        }
    }

    fn parse_emit(args: Args) -> Result<Self, String> {
        let Some(name) = args.text.filter(|t| !t.is_empty()) else {
            return Err("command \"emit\" needs an event name in text".to_string());
        };
        let name = super::events::user_event_name(&name)?;
        Ok(Command::Emit {
            owner: args.owner,
            name,
            payload: args.payload,
        })
    }

    fn parse_action(args: Args) -> Result<Self, String> {
        let Some(action) = args.text.filter(|t| !t.is_empty()) else {
            return Err("command \"action\" needs an action id in text".to_string());
        };
        if !args.session.is_empty() {
            let value = args.session;
            Ok(Command::ActionTarget {
                fallback: args.owner.clone(),
                action,
                argument: "session_id".into(),
                value,
            })
        } else if let Some(value) = args.target {
            Ok(Command::ActionTarget {
                fallback: args.owner.clone(),
                action,
                argument: "target".into(),
                value,
            })
        } else {
            Ok(Command::Action {
                fallback: args.owner,
                action,
            })
        }
    }

    fn parse_message(args: Args) -> Result<Self, String> {
        let Some(text) = args.text.filter(|t| !t.is_empty()) else {
            return Err("command \"message\" needs text".to_string());
        };
        let level = match args.level.as_deref() {
            None => super::bands::Level::Info,
            Some(name) => super::bands::Level::parse(name).ok_or_else(|| {
                format!(
                    "command \"message\" got level {name:?} — try \"info\", \
                     \"success\" or \"error\""
                )
            })?,
        };
        Ok(Command::Message { text, level })
    }

    fn parse_mod(args: Args) -> Result<Self, String> {
        let Some(file) = args.file.filter(|f| !f.is_empty()) else {
            return Err("command \"mod\" needs a file".to_string());
        };
        let edit = match args.action.as_deref() {
            Some("restore") => ModEdit::Restore,
            Some("remove") => ModEdit::Remove,
            _ => return Err("command \"mod\" needs action = \"restore\" or \"remove\"".to_string()),
        };
        Ok(Command::Mod { file, edit })
    }

    fn parse_setting(args: Args) -> Result<Self, String> {
        let Some(key) = args.text.filter(|t| !t.is_empty()) else {
            return Err("command \"set\" needs a plugin.setting key".to_string());
        };
        let value = if args.reset {
            None
        } else if let Some(flag) = args.flag {
            Some(SettingValue::Bool(flag))
        } else if let Some(number) = args.number {
            Some(SettingValue::Number(number))
        } else if let Some(value) = args.value {
            Some(SettingValue::Text(value))
        } else {
            return Err("command \"set\" needs a flag, number, value, or reset = true".to_string());
        };
        Ok(Command::Setting { key, value })
    }

    fn parse_create(args: Args) -> Result<Self, String> {
        let Some(repo) = args.repo.filter(|r| !r.is_empty()) else {
            return Err("command \"create\" needs a repo".to_string());
        };
        Ok(Command::Create {
            name: args.text.unwrap_or_default(),
            repo,
            agent: args.agent.filter(|a| !a.is_empty()),
            host: args.host.filter(|h| !h.is_empty()),
            fleet: args.fleet.filter(|f| !f.is_empty()),
            worktree: args.worktree,
            branch: args.branch.filter(|b| !b.is_empty()),
            base: args.base.filter(|b| !b.is_empty()),
            extras: args.extras,
        })
    }

    fn parse_bookmark(args: Args) -> Result<Self, String> {
        let Some(path) = args
            .repo
            .map(|path| path.trim().to_string())
            .filter(|p| !p.is_empty())
        else {
            return Err("command \"bookmark\" needs a repo path".to_string());
        };
        let edit = match args.action.as_deref() {
            Some("add") => BookmarkEdit::Add,
            Some("remove") => BookmarkEdit::Remove,
            Some("parent") => BookmarkEdit::Parent,
            Some("create") => BookmarkEdit::Create,
            Some("init") => BookmarkEdit::Init,
            Some("clone") => match args.text.as_deref().map(str::trim) {
                Some(url) if !url.is_empty() && !url.starts_with('-') => BookmarkEdit::Clone {
                    url: url.to_string(),
                },
                _ => return Err("command \"bookmark\" clone needs a url in text".to_string()),
            },
            _ => {
                return Err("command \"bookmark\" needs action = \"add\", \"remove\", \
                     \"parent\", \"create\", \"init\" or \"clone\""
                    .to_string())
            }
        };
        Ok(Command::Bookmark {
            host: args.host.unwrap_or_default(),
            path,
            edit,
        })
    }

    fn parse_program(args: Args) -> Result<Self, String> {
        let name = args.text.unwrap_or_default();
        super::terminal::validate_program_name(&name)?;
        let close = matches!(args.action.as_deref(), Some("close") | Some("stop"));
        let program = args.repo.unwrap_or_default();
        let keys = args.keys.filter(|keys| !keys.is_empty());
        if keys.is_some() && close {
            return Err(
                "command \"program\" cannot type into a pane and close it at once".to_string(),
            );
        }
        if keys.is_none() && !close && program.trim().is_empty() {
            return Err(
                "command \"program\" needs a program to run (or action = \"close\", or keys)"
                    .to_string(),
            );
        }
        Ok(Command::Program {
            owner: args.owner,
            name,
            program,
            argv: args.argv,
            close,
            keys,
        })
    }

    fn parse_session_command(kind: &str, args: Args) -> Result<Self, String> {
        let Args {
            session,
            text,
            force,
            fresh,
            ..
        } = args;
        if session.is_empty() {
            return Err(format!("command {kind:?} needs a session"));
        }
        match kind {
            "delete" => Ok(Command::Delete { session, force }),
            "start" => Ok(Command::Start { session }),
            "pause" => Ok(Command::Pause { session }),
            "restart" => Ok(Command::Restart { session, fresh }),
            "send" => match text {
                Some(text) => Ok(Command::Send { session, text }),
                None => Err("command \"send\" needs text".to_string()),
            },

            "rename" => Ok(Command::Rename {
                session,
                name: text.unwrap_or_default(),
            }),
            "copy" => Ok(Command::Copy { session }),
            "diff" => Ok(Command::Diff { session }),
            "shell" => Ok(Command::Shell { session }),
            "editor" => Ok(Command::Editor { session }),
            other => Err(format!(
                "unknown command {other:?} — try create, rename, copy, diff, delete, \
                 start, send, order, theme, set, emit, action or message"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraMember {
    pub path: String,
    pub worktree: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Args {
    pub session: String,
    pub target: Option<String>,
    pub text: Option<String>,
    pub value: Option<String>,
    pub force: bool,
    /// `restart`: start the agent on a new conversation.
    pub fresh: bool,
    pub flag: Option<bool>,
    pub toggle: bool,
    pub number: Option<f64>,
    pub reset: bool,
    pub repo: Option<String>,
    pub agent: Option<String>,
    pub host: Option<String>,
    pub fleet: Option<String>,
    pub worktree: Option<bool>,
    pub branch: Option<String>,
    pub base: Option<String>,
    pub extras: Vec<ExtraMember>,
    pub list: Vec<String>,
    pub file: Option<String>,
    pub action: Option<String>,
    pub owner: String,
    pub argv: Vec<String>,
    pub keys: Option<Vec<u8>>,
    pub payload: Vec<(String, super::events::Field)>,
    pub level: Option<String>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_creations_host_is_the_bare_machine_whichever_spelling_it_arrived_in() {
        let asked = |host: &str| {
            Command::parse(
                "create",
                Args {
                    repo: Some("/srv/repo".into()),
                    host: Some(host.into()),
                    ..Default::default()
                },
            )
            .expect("parse")
            .host()
        };
        assert_eq!(asked("ssh:devbox").as_deref(), Some("devbox"));
        assert_eq!(asked("wsl:Ubuntu").as_deref(), Some("Ubuntu"));
        assert_eq!(asked("devbox").as_deref(), Some("devbox"));
        assert_eq!(
            Command::parse(
                "create",
                Args {
                    repo: Some("/srv/repo".into()),
                    ..Default::default()
                }
            )
            .expect("parse")
            .host(),
            None
        );
    }

    use super::*;

    #[test]
    fn kinds_parse_from_what_a_mod_writes() {
        assert_eq!(
            Command::parse(
                "delete",
                Args {
                    session: "s1".into(),
                    ..Args::default()
                }
            ),
            Ok(Command::Delete {
                session: "s1".into(),
                force: false
            })
        );
        assert_eq!(
            Command::parse(
                "send",
                Args {
                    session: "s1".into(),
                    text: Some("hi".into()),
                    ..Args::default()
                }
            ),
            Ok(Command::Send {
                session: "s1".into(),
                text: "hi".into()
            })
        );
        for (text, name) in [(Some("renamed"), "renamed"), (None, "")] {
            let parsed = Command::parse(
                "rename",
                Args {
                    session: "s1".into(),
                    text: text.map(str::to_string),
                    ..Args::default()
                },
            );
            assert_eq!(
                parsed,
                Ok(Command::Rename {
                    session: "s1".into(),
                    name: name.into()
                })
            );
            assert_eq!(parsed.as_ref().map(Command::kind), Ok("rename"));
            assert_eq!(parsed.as_ref().map(Command::session), Ok("s1"));
        }
    }

    #[test]
    fn each_kind_reads_its_own_fields_and_names_what_is_missing() {
        fn text(value: &str) -> Option<String> {
            Some(value.to_string())
        }
        let session = |s: &str| Args {
            session: s.into(),
            ..Args::default()
        };
        let err = |message: &str| Err(message.to_string());
        let cases: Vec<(&str, Args, Result<Command, String>)> = vec![
            (
                "emit",
                Args {
                    text: text("deploy"),
                    owner: "mods/x.lua".into(),
                    ..Args::default()
                },
                Ok(Command::Emit {
                    owner: "mods/x.lua".into(),
                    name: "user.deploy".into(),
                    payload: Vec::new(),
                }),
            ),
            (
                "emit",
                Args {
                    text: text(""),
                    ..Args::default()
                },
                err("command \"emit\" needs an event name in text"),
            ),
            (
                "emit",
                Args {
                    text: text("user.deploy"),
                    ..Args::default()
                },
                err("command \"emit\": name \"user.deploy\" without the \"user.\" prefix — it is added for you"),
            ),
            (
                "mod",
                Args {
                    file: text("mods/a.lua"),
                    action: text("restore"),
                    ..Args::default()
                },
                Ok(Command::Mod {
                    file: "mods/a.lua".into(),
                    edit: ModEdit::Restore,
                }),
            ),
            (
                "plugin",
                Args {
                    file: text("mods/a.lua"),
                    action: text("remove"),
                    ..Args::default()
                },
                Ok(Command::Mod {
                    file: "mods/a.lua".into(),
                    edit: ModEdit::Remove,
                }),
            ),
            (
                "mod",
                Args {
                    file: text(""),
                    action: text("remove"),
                    ..Args::default()
                },
                err("command \"mod\" needs a file"),
            ),
            (
                "mod",
                Args {
                    file: text("mods/a.lua"),
                    ..Args::default()
                },
                err("command \"mod\" needs action = \"restore\" or \"remove\""),
            ),
            (
                "set",
                Args {
                    text: text("p.k"),
                    reset: true,
                    flag: Some(true),
                    ..Args::default()
                },
                Ok(Command::Setting {
                    key: "p.k".into(),
                    value: None,
                }),
            ),
            (
                "set",
                Args {
                    text: text("p.k"),
                    flag: Some(false),
                    number: Some(2.0),
                    ..Args::default()
                },
                Ok(Command::Setting {
                    key: "p.k".into(),
                    value: Some(SettingValue::Bool(false)),
                }),
            ),
            (
                "set",
                Args {
                    text: text("p.k"),
                    number: Some(2.5),
                    ..Args::default()
                },
                Ok(Command::Setting {
                    key: "p.k".into(),
                    value: Some(SettingValue::Number(2.5)),
                }),
            ),
            (
                "set",
                Args {
                    text: text("p.k"),
                    value: text("one"),
                    ..Args::default()
                },
                Ok(Command::Setting {
                    key: "p.k".into(),
                    value: Some(SettingValue::Text("one".into())),
                }),
            ),
            (
                "set",
                Args {
                    text: text("p.k"),
                    ..Args::default()
                },
                err("command \"set\" needs a flag, number, value, or reset = true"),
            ),
            (
                "set",
                Args {
                    flag: Some(true),
                    ..Args::default()
                },
                err("command \"set\" needs a plugin.setting key"),
            ),
            (
                "create",
                Args {
                    repo: text("/srv/repo"),
                    text: text("named"),
                    agent: text(""),
                    host: text("box"),
                    ..Args::default()
                },
                Ok(Command::Create {
                    name: "named".into(),
                    repo: "/srv/repo".into(),
                    agent: None,
                    host: Some("box".into()),
                    fleet: None,
                    worktree: None,
                    branch: None,
                    base: None,
                    extras: Vec::new(),
                }),
            ),
            (
                "create",
                Args {
                    repo: text(""),
                    ..Args::default()
                },
                err("command \"create\" needs a repo"),
            ),
            (
                "bookmark",
                Args {
                    repo: text("  /srv/repo  "),
                    action: text("add"),
                    ..Args::default()
                },
                Ok(Command::Bookmark {
                    host: String::new(),
                    path: "/srv/repo".into(),
                    edit: BookmarkEdit::Add,
                }),
            ),
            (
                "bookmark",
                Args {
                    repo: text("/srv/repo"),
                    host: text("box"),
                    action: text("remove"),
                    ..Args::default()
                },
                Ok(Command::Bookmark {
                    host: "box".into(),
                    path: "/srv/repo".into(),
                    edit: BookmarkEdit::Remove,
                }),
            ),
            (
                "bookmark",
                Args {
                    repo: text("/srv/repo"),
                    action: text("parent"),
                    ..Args::default()
                },
                Ok(Command::Bookmark {
                    host: String::new(),
                    path: "/srv/repo".into(),
                    edit: BookmarkEdit::Parent,
                }),
            ),
            (
                "bookmark",
                Args {
                    repo: text("   "),
                    action: text("add"),
                    ..Args::default()
                },
                err("command \"bookmark\" needs a repo path"),
            ),
            (
                "bookmark",
                Args {
                    repo: text("/srv/repo"),
                    action: text("forget"),
                    ..Args::default()
                },
                err(
                    "command \"bookmark\" needs action = \"add\", \"remove\", \"parent\", \
                     \"create\", \"init\" or \"clone\"",
                ),
            ),
            (
                "focus",
                Args {
                    text: text(""),
                    ..Args::default()
                },
                err("command \"focus\" needs a mod name"),
            ),
            (
                "open",
                Args {
                    text: text("https://example.com"),
                    ..Args::default()
                },
                Ok(Command::OpenLink {
                    url: "https://example.com".into(),
                }),
            ),
            (
                "open",
                Args::default(),
                err("command \"open\" needs a url in text"),
            ),
            (
                "theme",
                Args {
                    text: text("nord"),
                    ..Args::default()
                },
                Ok(Command::Theme {
                    name: "nord".into(),
                }),
            ),
            (
                "theme",
                Args {
                    text: text(""),
                    ..Args::default()
                },
                err("command \"theme\" needs a name"),
            ),
            (
                "theme",
                Args::default(),
                err("command \"theme\" needs a name"),
            ),
            (
                "order",
                Args {
                    list: vec!["a".into(), "b".into()],
                    ..Args::default()
                },
                Ok(Command::Order {
                    list: vec!["a".into(), "b".into()],
                }),
            ),
            (
                "order",
                Args::default(),
                err("command \"order\" needs a list of session ids"),
            ),
            (
                "program",
                Args {
                    text: text("watch"),
                    repo: text("cargo"),
                    argv: vec!["test".into()],
                    keys: Some(Vec::new()),
                    owner: "mods/x.lua".into(),
                    ..Args::default()
                },
                Ok(Command::Program {
                    owner: "mods/x.lua".into(),
                    name: "watch".into(),
                    program: "cargo".into(),
                    argv: vec!["test".into()],
                    close: false,
                    keys: None,
                }),
            ),
            (
                "program",
                Args {
                    text: text("watch"),
                    action: text("stop"),
                    ..Args::default()
                },
                Ok(Command::Program {
                    owner: String::new(),
                    name: "watch".into(),
                    program: String::new(),
                    argv: Vec::new(),
                    close: true,
                    keys: None,
                }),
            ),
            (
                "program",
                Args {
                    text: text("watch"),
                    action: text("close"),
                    keys: Some(b"q".to_vec()),
                    ..Args::default()
                },
                err("command \"program\" cannot type into a pane and close it at once"),
            ),
            (
                "program",
                Args {
                    text: text("watch"),
                    repo: text("  "),
                    ..Args::default()
                },
                err("command \"program\" needs a program to run (or action = \"close\", or keys)"),
            ),
            (
                "program",
                Args::default(),
                err("a program pane needs a name"),
            ),
            (
                "copy",
                session("s1"),
                Ok(Command::Copy {
                    session: "s1".into(),
                }),
            ),
            (
                "shell",
                session("s1"),
                Ok(Command::Shell {
                    session: "s1".into(),
                }),
            ),
            (
                "editor",
                session("s1"),
                Ok(Command::Editor {
                    session: "s1".into(),
                }),
            ),
            (
                "delete",
                Args {
                    force: true,
                    ..session("s1")
                },
                Ok(Command::Delete {
                    session: "s1".into(),
                    force: true,
                }),
            ),
            ("send", session("s1"), err("command \"send\" needs text")),
        ];
        for (kind, args, expected) in cases {
            assert_eq!(
                Command::parse(kind, args.clone()),
                expected,
                "{kind} {args:?}"
            );
        }
    }

    #[test]
    fn every_verb_states_what_it_requires() {
        use super::super::host::Capability::{Program, Run, Sessions};
        let with = |args: Args| Args {
            session: "s1".into(),
            owner: "mods/30_x.lua".into(),
            ..args
        };
        let verbs: Vec<(&str, Args, Option<super::super::host::Capability>)> = vec![
            (
                "send",
                with(Args {
                    text: Some("x".into()),
                    ..Args::default()
                }),
                Some(Sessions),
            ),
            (
                "create",
                with(Args {
                    repo: Some("/r".into()),
                    text: Some("n".into()),
                    ..Args::default()
                }),
                Some(Sessions),
            ),
            (
                "delete",
                with(Args {
                    force: true,
                    ..Args::default()
                }),
                Some(Sessions),
            ),
            ("delete", with(Args::default()), Some(Sessions)),
            ("pause", with(Args::default()), Some(Sessions)),
            ("restart", with(Args::default()), Some(Sessions)),
            ("start", with(Args::default()), None),
            (
                "mod",
                with(Args {
                    file: Some("mods/x.lua".into()),
                    action: Some("remove".into()),
                    ..Args::default()
                }),
                Some(Sessions),
            ),
            (
                "open",
                with(Args {
                    text: Some("https://x".into()),
                    ..Args::default()
                }),
                Some(Run),
            ),
            (
                "program",
                with(Args {
                    text: Some("p".into()),
                    repo: Some("top".into()),
                    ..Args::default()
                }),
                Some(Program),
            ),
            (
                "order",
                with(Args {
                    list: vec!["s1".into()],
                    ..Args::default()
                }),
                None,
            ),
            (
                "rename",
                with(Args {
                    text: Some("n".into()),
                    ..Args::default()
                }),
                None,
            ),
            (
                "focus",
                with(Args {
                    text: Some("p".into()),
                    ..Args::default()
                }),
                None,
            ),
            ("shell", with(Args::default()), None),
            ("editor", with(Args::default()), None),
            ("copy", with(Args::default()), None),
            ("diff", with(Args::default()), None),
            (
                "theme",
                with(Args {
                    text: Some("t".into()),
                    ..Args::default()
                }),
                None,
            ),
            (
                "set",
                with(Args {
                    text: Some("p.k".into()),
                    flag: Some(true),
                    ..Args::default()
                }),
                None,
            ),
            (
                "message",
                with(Args {
                    text: Some("m".into()),
                    ..Args::default()
                }),
                None,
            ),
            (
                "emit",
                with(Args {
                    text: Some("e".into()),
                    ..Args::default()
                }),
                None,
            ),
            (
                "action",
                with(Args {
                    text: Some("help.open".into()),
                    ..Args::default()
                }),
                None,
            ),
            (
                "action",
                with(Args {
                    text: Some(super::super::clipboard::PASTE_ACTION.into()),
                    ..Args::default()
                }),
                Some(Sessions),
            ),
        ];
        for (verb, args, expected) in verbs {
            let command = Command::parse(verb, args).unwrap_or_else(|e| panic!("{verb}: {e}"));
            assert_eq!(command.requires(), expected, "{verb}: {command:?}");
        }
    }

    #[test]
    fn an_action_command_carries_the_action_and_the_mod_that_asked() {
        assert_eq!(
            Command::parse(
                "action",
                Args {
                    text: Some("help.open".into()),
                    owner: "mods/10_sessions.lua".into(),
                    ..Args::default()
                }
            ),
            Ok(Command::Action {
                fallback: "mods/10_sessions.lua".into(),
                action: "help.open".into(),
            })
        );
        let error = Command::parse("action", Args::default()).unwrap_err();
        assert!(error.contains("text"), "{error}");
    }

    #[test]
    fn a_message_names_a_level_the_band_can_badge() {
        assert_eq!(
            Command::parse(
                "message",
                Args {
                    text: Some("nothing to undo".into()),
                    ..Args::default()
                }
            ),
            Ok(Command::Message {
                text: "nothing to undo".into(),
                level: crate::kernel::bands::Level::Info,
            })
        );
        assert_eq!(
            Command::parse(
                "message",
                Args {
                    text: Some("gone".into()),
                    level: Some("error".into()),
                    ..Args::default()
                }
            ),
            Ok(Command::Message {
                text: "gone".into(),
                level: crate::kernel::bands::Level::Error,
            })
        );
        let error = Command::parse(
            "message",
            Args {
                text: Some("gone".into()),
                level: Some("critical".into()),
                ..Args::default()
            },
        )
        .unwrap_err();
        assert!(error.contains("critical"), "{error}");
        assert!(error.contains("success"), "{error}");
    }

    #[test]
    fn an_unknown_kind_is_refused_with_the_alternatives() {
        let error = Command::parse(
            "explode",
            Args {
                session: "s1".into(),
                ..Args::default()
            },
        )
        .unwrap_err();
        assert!(error.contains("unknown command"), "{error}");
        assert!(error.contains("delete"), "{error}");
    }

    #[test]
    fn focus_carries_whether_it_toggles() {
        let plain = Command::parse(
            "focus",
            Args {
                text: Some("review".into()),
                ..Args::default()
            },
        )
        .expect("focus parses");
        assert!(matches!(plain, Command::Focus { toggle: false, .. }));

        let toggling = Command::parse(
            "focus",
            Args {
                text: Some("review".into()),
                toggle: true,
                ..Args::default()
            },
        )
        .expect("focus parses");
        match toggling {
            Command::Focus { mod_name, toggle } => {
                assert_eq!(mod_name, "review");
                assert!(toggle);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_diff_refresh_parses_and_names_its_session() {
        let command = Command::parse(
            "diff",
            Args {
                session: "s1".into(),
                ..Args::default()
            },
        )
        .expect("diff parses");
        assert_eq!(command.kind(), "diff");
        assert_eq!(command.session(), "s1");
        assert!(matches!(command, Command::Diff { .. }));
        assert!(Command::parse("diff", Args::default()).is_err());
    }

    #[test]
    fn mod_and_its_deprecated_spelling_publish_the_same_kind() {
        for verb in ["mod", "plugin"] {
            let command = Command::parse(
                verb,
                Args {
                    file: Some("mods/a.lua".into()),
                    action: Some("remove".into()),
                    ..Args::default()
                },
            )
            .expect("parses");
            assert_eq!(command.kind(), "plugin", "{verb}");
        }
    }

    #[test]
    fn a_command_without_a_session_is_refused() {
        let error = Command::parse("delete", Args::default()).unwrap_err();
        assert!(error.contains("needs a session"), "{error}");
    }

    #[test]
    fn send_needs_text_and_a_removed_verb_is_refused() {
        assert!(Command::parse(
            "send",
            Args {
                session: "s1".into(),
                ..Args::default()
            }
        )
        .is_err());
        for gone in ["reorder", "restore", "fork", "sync", "task", "automation"] {
            assert!(Command::parse(
                gone,
                Args {
                    session: "s1".into(),
                    number: Some(1.0),
                    ..Args::default()
                }
            )
            .is_err());
        }
    }

    #[test]
    fn restart_names_its_session_and_whether_the_conversation_is_fresh() {
        let restart = |fresh| {
            Command::parse(
                "restart",
                Args {
                    session: "s1".into(),
                    fresh,
                    ..Args::default()
                },
            )
        };
        assert_eq!(
            restart(false),
            Ok(Command::Restart {
                session: "s1".into(),
                fresh: false
            })
        );
        assert_eq!(
            restart(true),
            Ok(Command::Restart {
                session: "s1".into(),
                fresh: true
            })
        );
        assert!(Command::parse("restart", Args::default()).is_err());
    }

    #[test]
    fn dispatch_returns_immediately_and_reports_the_command_in_flight() {
        let bus = CommandBus::fleet(crate::fleet_link::FleetLink::local("thurbox-test-no-fleet"));
        let started = std::time::Instant::now();
        bus.dispatch(Command::Delete {
            session: "not-a-uuid".into(),
            force: false,
        });
        assert!(
            started.elapsed() < std::time::Duration::from_millis(200),
            "dispatch must return immediately"
        );
        assert_eq!(bus.inflight().len(), 1);
        assert_eq!(bus.inflight()[0].kind, "delete");
    }

    #[test]
    fn a_failure_surfaces_through_the_inflight_list_not_the_call() {
        let mut bus =
            CommandBus::fleet(crate::fleet_link::FleetLink::local("thurbox-test-no-fleet"));
        bus.dispatch(Command::Delete {
            session: "not-a-uuid".into(),
            force: false,
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            bus.poll();
            let inflight = bus.inflight();
            if inflight.iter().any(|entry| entry.phase == Phase::Failed) {
                let failed = inflight
                    .iter()
                    .find(|entry| entry.phase == Phase::Failed)
                    .expect("failed entry");
                assert!(
                    failed.error.as_deref().unwrap_or("").contains("fleet"),
                    "{:?}",
                    failed.error
                );
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("the failure never surfaced");
    }

    #[test]
    fn a_failed_command_is_not_reported_as_work_in_flight() {
        let mut bus =
            CommandBus::fleet(crate::fleet_link::FleetLink::local("thurbox-test-no-fleet"));
        let id = bus.dispatch(Command::Delete {
            session: "not-a-uuid".into(),
            force: false,
        });
        bus.finish_for_test(id, Some("bad session id".into()));
        bus.poll();

        assert_eq!(bus.inflight().len(), 1, "the failed row is still drawable");
        assert!(
            bus.first_running().is_none(),
            "a failure must not be described as work in progress"
        );
    }

    #[test]
    fn a_failure_is_skipped_to_find_the_work_still_running_behind_it() {
        let mut bus =
            CommandBus::fleet(crate::fleet_link::FleetLink::local("thurbox-test-no-fleet"));
        let failed = bus.dispatch(Command::Delete {
            session: "not-a-uuid".into(),
            force: false,
        });
        bus.finish_for_test(failed, Some("bad session id".into()));
        bus.poll();
        let running = bus.dispatch(Command::Rename {
            session: "s1".into(),
            name: "renamed".into(),
        });

        let found = bus.first_running().expect("the running command");
        assert_eq!(found.id, running);
        assert_ne!(found.phase, Phase::Failed);
    }

    #[test]
    fn a_worker_announcing_itself_cannot_revive_a_finished_row() {
        let mut entry = InFlight {
            id: 1,
            kind: "delete",
            session: "s1".into(),
            subject: None,
            host: None,
            phase: Phase::Failed,
            error: Some("bad session id".into()),
        };
        entry.started();
        assert_eq!(entry.phase, Phase::Failed, "a phase only moves forward");

        let mut queued = InFlight {
            phase: Phase::Queued,
            ..entry
        };
        queued.started();
        assert_eq!(queued.phase, Phase::Running);
    }

    #[test]
    fn work_in_flight_is_reported_as_running() {
        let bus = CommandBus::fleet(crate::fleet_link::FleetLink::local("thurbox-test-no-fleet"));
        assert!(bus.first_running().is_none(), "nothing dispatched yet");
        let id = bus.dispatch(Command::Rename {
            session: "s1".into(),
            name: "renamed".into(),
        });
        let found = bus.first_running().expect("the dispatched command");
        assert_eq!(found.id, id);
        assert_eq!(found.kind, "rename");
    }

    #[test]
    fn a_session_with_work_in_flight_reads_as_busy() {
        let bus = CommandBus::fleet(crate::fleet_link::FleetLink::local("thurbox-test-no-fleet"));
        assert!(!bus.is_busy("s1"));
        bus.dispatch(Command::Rename {
            session: "s1".into(),
            name: "renamed".into(),
        });
        assert!(bus.is_busy("s1"));
        assert!(!bus.is_busy("other"));
    }

    #[test]
    fn phases_have_stable_names() {
        assert_eq!(Phase::Queued.as_str(), "queued");
        assert_eq!(Phase::Running.as_str(), "running");
        assert_eq!(Phase::Failed.as_str(), "failed");
    }

    #[test]
    fn a_command_that_succeeds_is_gone_from_the_list_by_the_time_poll_returns() {
        let mut bus =
            CommandBus::fleet(crate::fleet_link::FleetLink::local("thurbox-test-no-fleet"));
        let id = bus.dispatch(Command::Focus {
            mod_name: "agent".into(),
            toggle: false,
        });
        assert!(
            bus.inflight().iter().any(|item| item.id == id),
            "it is in the list while it is running"
        );

        bus.finish_for_test(id, None);
        assert!(bus.poll(), "poll reports that something finished");
        assert!(
            !bus.inflight().iter().any(|item| item.id == id),
            "and it is already gone — there is no 'done' left to observe"
        );
    }

    #[test]
    fn a_command_that_fails_lingers_so_a_pane_can_draw_it() {
        let mut bus =
            CommandBus::fleet(crate::fleet_link::FleetLink::local("thurbox-test-no-fleet"));
        let id = bus.dispatch(Command::Focus {
            mod_name: "agent".into(),
            toggle: false,
        });
        bus.finish_for_test(id, Some("no such pane".to_string()));
        assert!(bus.poll());
        let item = bus
            .inflight()
            .into_iter()
            .find(|item| item.id == id)
            .expect("a failure stays visible");
        assert_eq!(item.error.as_deref(), Some("no such pane"));
    }

    #[test]
    fn a_program_command_names_no_session() {
        let program = Command::Program {
            owner: "mods/90_watch.lua".into(),
            name: "watch".into(),
            program: "watch".into(),
            argv: Vec::new(),
            close: false,
            keys: None,
        };
        assert_eq!(program.session(), "");
        assert_eq!(program.kind(), "program");
        assert!(program.applied_on_ui_thread());
    }

    #[test]
    fn a_program_command_needs_a_program_or_a_close() {
        let ask = |program: &str, action: Option<&str>| {
            Command::parse(
                "program",
                Args {
                    owner: "mods/90_watch.lua".into(),
                    text: Some("watch".into()),
                    repo: (!program.is_empty()).then(|| program.to_string()),
                    action: action.map(str::to_string),
                    ..Args::default()
                },
            )
        };
        assert!(ask("watch", None).is_ok());
        assert!(ask("", Some("close")).is_ok());
        let error = ask("", None).expect_err("should refuse");
        assert!(error.contains("program"), "{error}");
    }

    #[test]
    fn a_program_command_can_type_into_a_pane_it_already_started() {
        let ask = |keys: Option<&[u8]>, program: &str, action: Option<&str>| {
            Command::parse(
                "program",
                Args {
                    owner: "mods/50_editor.lua".into(),
                    text: Some("editor".into()),
                    repo: (!program.is_empty()).then(|| program.to_string()),
                    keys: keys.map(<[u8]>::to_vec),
                    action: action.map(str::to_string),
                    ..Args::default()
                },
            )
        };

        let typed = ask(Some(b":e /tmp/x\r"), "", None).expect("keys need no program");
        match typed {
            Command::Program {
                keys, close, name, ..
            } => {
                assert_eq!(keys.as_deref(), Some(b":e /tmp/x\r".as_slice()));
                assert!(!close);
                assert_eq!(name, "editor");
            }
            other => panic!("expected a program command, got {other:?}"),
        }

        let error = ask(Some(b""), "", None).expect_err("should refuse");
        assert!(error.contains("program"), "{error}");

        let error = ask(Some(b"q"), "", Some("close")).expect_err("should refuse");
        assert!(error.contains("close"), "{error}");

        let both = ask(Some(b":e /tmp/x\r"), "nvim", None).expect("keys may carry a fallback");
        match both {
            Command::Program {
                keys,
                program,
                close,
                ..
            } => {
                assert_eq!(keys.as_deref(), Some(b":e /tmp/x\r".as_slice()));
                assert_eq!(program, "nvim");
                assert!(!close);
            }
            other => panic!("expected a program command, got {other:?}"),
        }
    }

    #[test]
    fn a_program_command_refuses_a_name_that_would_not_survive_a_window_name() {
        for bad in ["", "watch#2", "a b", "../x"] {
            let refused = Command::parse(
                "program",
                Args {
                    owner: "mods/90_watch.lua".into(),
                    text: Some(bad.to_string()),
                    repo: Some("watch".into()),
                    ..Args::default()
                },
            );
            assert!(refused.is_err(), "{bad:?} should be refused");
        }
    }
}
