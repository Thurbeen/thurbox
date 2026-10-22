//! A session created on a host that shares its sessions is created by that
//! host's own `thurbox-cli` (ADR-24) — and must come out the way the caller
//! asked for it (#1471).
//!
//! Two real instances: the caller, and a "host" with its own config, data,
//! home and tmux server. The stand-in `ssh` on the caller's `PATH` records its
//! argv and then runs the command it was given against the host instance,
//! stdin included — so only the network hop is not real, and every flag the
//! caller forwards is parsed by the real host CLI.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::Value;
use thurbox::session::SessionId;

#[path = "support/tmux_server.rs"]
mod tmux_server;

use tmux_server::TmuxServer;

const SETTINGS: &str =
    "[features]\nautomations = false\nversion_check = false\nauto_update = false\n";

/// The host's registry: a `shell` default that idles, and an agent that pins
/// a conversation id through `resume_args`.
const HOST_AGENTS: &str = "default = \"shell\"\n\n\
     [[agents]]\nname = \"shell\"\ncommand = \"sh\"\nargs = [\"-c\", \"exec sleep 600\"]\n\n\
     [[agents]]\nname = \"resumer\"\ncommand = \"sh\"\nargs = []\n\
     resume_args = [\"-c\", \"exec sleep 600 {id}\"]\nresume_latest = false\n";

struct Instance {
    root: tempfile::TempDir,
    server: TmuxServer,
}

impl Instance {
    fn new(socket: &str) -> Self {
        let root = tempfile::TempDir::new().expect("tempdir");
        for sub in ["home", "config", "data", "work"] {
            std::fs::create_dir_all(root.path().join(sub)).expect("mkdir");
        }
        std::fs::write(root.path().join("config/settings.toml"), SETTINGS).unwrap();
        Self {
            root,
            server: TmuxServer::private(socket),
        }
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.root.path().join(sub)
    }

    fn db(&self) -> thurbox::storage::Database {
        thurbox::storage::Database::open(&self.path("data").join("thurbox.db"))
            .expect("open the instance database")
    }
}

/// The caller, wired to reach `host` through the stand-in `ssh`.
struct Pair {
    caller: Instance,
    host: Instance,
}

impl Pair {
    fn new() -> Self {
        let caller = Instance::new("tbx-dc-caller");
        let host = Instance::new("tbx-dc-host");
        std::fs::write(host.path("config/agents.toml"), HOST_AGENTS).unwrap();
        std::fs::write(
            caller.path("config/hosts.toml"),
            "[[hosts]]\nname = 'probe'\ndestination = 'probe'\nshare_sessions = true\n",
        )
        .unwrap();
        // Where the host's probe looks first, so it finds this build.
        let host_bin = host
            .path("home/.local/share")
            .join(thurbox::paths::app_dir_name())
            .join("bin");
        std::fs::create_dir_all(&host_bin).unwrap();
        std::os::unix::fs::symlink(
            env!("CARGO_BIN_EXE_thurbox-cli"),
            host_bin.join("thurbox-cli"),
        )
        .unwrap();

        let bin = caller.path("bin");
        std::fs::create_dir(&bin).unwrap();
        let ssh = bin.join("ssh");
        std::fs::write(
            &ssh,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{argv}'\n\
                 while [ \"$#\" -gt 0 ]; do\n\
                 \x20 case \"$1\" in\n\
                 \x20   -o) shift; shift ;;\n\
                 \x20   -*) shift ;;\n\
                 \x20   *) break ;;\n\
                 \x20 esac\n\
                 done\n\
                 shift\n\
                 export HOME='{home}' USERPROFILE='{home}' THURBOX_CONFIG_DIR='{config}' \
                 THURBOX_DATA_DIR='{data}' TMUX_TMPDIR='{tmpdir}' THURBOX_SOCKET='{socket}'\n\
                 unset THURBOX_SOCKET_FOR THURBOX_SESSION THURBOX_SESSION_ID\n\
                 case \"$*\" in\n\
                 \x20 *' session create '*)\n\
                 \x20   if [ -e '{drop}' ]; then eval \"$*\" >/dev/null 2>&1; exit 255; fi ;;\n\
                 esac\n\
                 eval \"exec $*\"\n",
                argv = caller.path("ssh-argv").display(),
                home = host.path("home").display(),
                config = host.path("config").display(),
                data = host.path("data").display(),
                tmpdir = host.server.tmpdir().display(),
                socket = host.server.socket(),
                drop = caller.path("drop-answer").display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { caller, host }
    }

    fn create(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.args(["session", "create", "--host", "probe", "--repo-path"])
            .arg(self.host.path("work"))
            .args(args)
            .arg("--json");
        let home = self.caller.path("home");
        cmd.env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("THURBOX_CONFIG_DIR", self.caller.path("config"))
            .env("THURBOX_DATA_DIR", self.caller.path("data"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.caller.path("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env_remove("THURBOX_SESSION")
            .env_remove("THURBOX_SESSION_ID");
        self.caller.server.scope(&mut cmd);
        cmd.output().expect("run thurbox-cli")
    }

    /// `session create` run by the host's own CLI, as a thurbox there would.
    fn create_on_host(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_thurbox-cli"));
        cmd.args(["session", "create", "--repo-path"])
            .arg(self.host.path("work"))
            .args(args)
            .arg("--json");
        let home = self.host.path("home");
        cmd.env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("THURBOX_CONFIG_DIR", self.host.path("config"))
            .env("THURBOX_DATA_DIR", self.host.path("data"))
            .env_remove("THURBOX_SESSION")
            .env_remove("THURBOX_SESSION_ID");
        self.host.server.scope(&mut cmd);
        cmd.output().expect("run the host's thurbox-cli")
    }

    fn ssh_argv(&self) -> String {
        std::fs::read_to_string(self.caller.path("ssh-argv")).unwrap_or_default()
    }
}

fn have_tmux() -> bool {
    thurbox::paths::resolve_on_path("tmux").is_some()
}

fn answer(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is JSON ({e}): {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn created_id(out: &Output) -> SessionId {
    let doc = answer(out);
    assert!(out.status.success(), "create failed: {doc}");
    doc["id"].as_str().expect("an id").parse().expect("a uuid")
}

/// Wait for `path` to hold something, for as long as a pane takes to start.
fn wait_for_file(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if !text.is_empty() {
                return text;
            }
        }
        assert!(
            Instant::now() < deadline,
            "{} was never written",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `--command`, `--arg`, `--env` and `--reports-as` reach the host's CLI; the
/// environment's values never reach a command line on the way.
#[test]
fn a_delegated_create_carries_the_command_its_args_and_its_env() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let pair = Pair::new();
    let seen = pair.host.path("seen");
    let script = format!(
        "printf '%s' \"$FM_SECRET\" > '{}'; exec sleep 600",
        seen.display()
    );
    let out = pair.create(&[
        "--name",
        "carried",
        "--command",
        "sh",
        "--arg",
        "-c",
        "--arg",
        &script,
        "--env",
        "FM_SECRET=env-secret",
        "--reports-as",
        "claude",
    ]);
    let id = created_id(&out);

    assert_eq!(
        wait_for_file(&seen),
        "env-secret",
        "the host must launch the caller's command, in the caller's environment"
    );
    let host = pair.host.db();
    assert!(
        host.get_session_by_id(id).unwrap().is_some(),
        "the host's own row"
    );
    let recipe = host
        .load_launch_recipe(id)
        .unwrap()
        .expect("a command session records its recipe on the host");
    assert_eq!(recipe.command, "sh");
    assert_eq!(
        host.load_reports_as().unwrap().get(&id).map(String::as_str),
        Some("claude"),
        "the declaration belongs where the hooks that read it run"
    );
    let caller = pair.caller.db();
    assert!(
        caller.get_session_by_id(id).unwrap().is_some(),
        "the caller mirrors the host's row under the same id"
    );
    assert_eq!(
        caller
            .load_reports_as()
            .unwrap()
            .get(&id)
            .map(String::as_str),
        Some("claude"),
        "and reads the row against the same agent the host does"
    );
    let argv = pair.ssh_argv();
    assert!(
        !argv.contains("env-secret"),
        "an --env value reached ssh's command line, readable in ps: {argv}"
    );
}

/// `--resume` reaches the host: the session continues the conversation it was
/// asked to, rather than starting a fresh one under a new id.
#[test]
fn a_delegated_create_resumes_the_conversation_it_was_given() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let pair = Pair::new();
    let out = pair.create(&[
        "--name", "resumed", "--agent", "resumer", "--resume", "conv-7",
    ]);
    let id = created_id(&out);
    let row = pair.host.db().get_session_by_id(id).unwrap().expect("row");
    assert_eq!(row.agent, "resumer");
    assert_eq!(row.agent_session_id.as_deref(), Some("conv-7"));
}

/// The link drops after the host created the session: the caller finds it by
/// the id it minted, rather than reporting a failure that a retry would turn
/// into a second session.
#[test]
fn a_create_whose_answer_was_lost_is_found_by_its_id_not_made_twice() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let pair = Pair::new();
    std::fs::write(pair.caller.path("drop-answer"), "").unwrap();
    let out = pair.create(&["--name", "dropped"]);
    let id = created_id(&out);
    std::fs::remove_file(pair.caller.path("drop-answer")).unwrap();

    let host = pair.host.db();
    let named: Vec<_> = host
        .find_sessions_by_name("dropped")
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(
        named,
        vec![id],
        "exactly the one session, under the reported id"
    );

    // A driver retrying with the id it was given is answered, not duplicated.
    let again = pair.create(&["--name", "dropped", "--id", &id.to_string()]);
    let doc = answer(&again);
    assert!(again.status.success(), "{doc}");
    assert_eq!(doc["id"], id.to_string());
    assert_eq!(doc["created"], false, "{doc}");
    assert_eq!(host.find_sessions_by_name("dropped").unwrap().len(), 1);
}

/// `--on-existing` is the host's to apply: its rows are the record, and the
/// caller's mirror may not have seen the session yet. Here the host already
/// holds the key, the caller has never mirrored it, and `adopt` must answer
/// with the host's session rather than refuse or make another.
#[test]
fn on_existing_is_applied_by_the_host_against_its_own_rows() {
    if !have_tmux() {
        eprintln!("skipping: tmux is not installed");
        return;
    }
    let pair = Pair::new();
    let theirs = created_id(&pair.create_on_host(&["--name", "keyed", "--key", "k1"]));
    assert!(
        pair.caller
            .db()
            .get_session_by_id(theirs)
            .unwrap()
            .is_none(),
        "the caller has not mirrored it yet"
    );

    let out = pair.create(&["--name", "keyed", "--key", "k1", "--on-existing", "adopt"]);
    let doc = answer(&out);
    assert!(out.status.success(), "{doc}");
    assert_eq!(doc["id"], theirs.to_string(), "{doc}");
    assert_eq!(doc["created"], false, "{doc}");
    assert_eq!(
        pair.host.db().find_sessions_by_name("keyed").unwrap().len(),
        1
    );
}
