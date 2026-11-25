#![cfg(target_os = "macos")]

mod support;

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use serde_json::{Value, json};
use support::Env;

const NOT_LOADED: u64 = 113;

struct Agent {
    env: Env,
    domain: String,
}

impl Agent {
    fn new(name: &str) -> Agent {
        let env = Env::service_stub(name, "launchctl");
        let uid = fs::metadata(&env.root).unwrap().uid();
        let agent = Agent {
            env,
            domain: format!("gui/{uid}"),
        };
        agent.answer("print", json!({"code": NOT_LOADED}));
        agent
    }

    fn answer(&self, verb: &str, answer: Value) {
        self.env.edit_stub(|s| s["service"][verb] = answer);
    }

    fn running(&self) -> Value {
        json!({"stdout": format!("{}/fleet = {{\n\tstate = running\n\tpid = 42\n}}\n", self.domain)})
    }

    fn plist(&self) -> PathBuf {
        self.env.root.join("Library/LaunchAgents/fleet.plist")
    }

    fn calls(&self) -> Vec<String> {
        self.env
            .stub_calls()
            .iter()
            .map(|c| {
                c["argv"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| a.as_str().unwrap())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    fn forget_calls(&self) {
        let _ = fs::remove_file(self.env.stub_dir().join("calls.jsonl"));
    }

    fn supervisor(&self) -> Value {
        self.env.fleet(&["version", "--json"]).ok().json()["manager"]["supervisor"].clone()
    }
}

#[test]
fn install_writes_the_one_agent_and_version_names_it() {
    let a = Agent::new("launchd-install");
    assert_eq!(a.supervisor(), json!(null));

    a.env.fleet(&["service", "install", "--now"]).ok();

    let plist = fs::read_to_string(a.plist()).unwrap();
    let config = std::path::absolute(&a.env.config).unwrap();
    for part in [
        "<key>Label</key>\n  <string>fleet</string>".into(),
        "fleet</string>\n    <string>--config</string>".to_string(),
        format!(
            "<key>FLEET_CONFIG</key>\n    <string>{}</string>",
            config.display()
        ),
        "<key>KeepAlive</key>\n  <dict>\n    <key>SuccessfulExit</key>\n    <false/>".into(),
        "<key>AbandonProcessGroup</key>\n  <true/>".into(),
        "<key>ExitTimeOut</key>\n  <integer>30</integer>".into(),
        "<key>XFleetStore</key>".into(),
    ] {
        assert!(plist.contains(&part), "plist lacks {part:?}:\n{plist}");
    }
    let target = format!("{}/fleet", a.domain);
    assert_eq!(
        a.calls(),
        [
            format!("enable {target}"),
            format!("print {target}"),
            format!("bootstrap {} {}", a.domain, a.plist().display()),
        ]
    );
    assert_eq!(a.supervisor(), json!({"kind": "launchd", "unit": "fleet"}));
}

#[test]
fn the_verbs_drive_the_store_s_agent_and_uninstall_removes_it() {
    let a = Agent::new("launchd-verbs");
    a.env.fleet(&["service", "install"]).ok();
    a.answer("print", a.running());
    a.forget_calls();

    a.env.fleet(&["service", "start"]).ok();
    a.env.fleet(&["service", "restart"]).ok();
    a.env.fleet(&["service", "stop"]).ok();
    a.env
        .fleet(&["service", "status"])
        .ok()
        .stdout_has("state = running");
    a.env.fleet(&["service", "uninstall"]).ok();

    let target = format!("{}/fleet", a.domain);
    let calls = a.calls();
    for call in [
        format!("kickstart {target}"),
        format!("kickstart -k {target}"),
        format!("bootout {target}"),
    ] {
        assert!(calls.contains(&call), "{call:?} not in {calls:?}");
    }
    assert!(!a.plist().exists());
    assert_eq!(a.supervisor(), json!(null));
    a.env
        .fleet(&["service", "start"])
        .code(3)
        .stderr_has("fleet service install");
}

#[test]
fn start_loads_an_agent_launchd_has_not_loaded() {
    let a = Agent::new("launchd-start");
    a.env.fleet(&["service", "install"]).ok();
    a.forget_calls();

    a.env.fleet(&["service", "start"]).ok();
    a.env
        .fleet(&["service", "status"])
        .code(3)
        .stdout_has("not loaded");

    assert!(
        a.calls()
            .contains(&format!("bootstrap {} {}", a.domain, a.plist().display())),
        "{:?}",
        a.calls()
    );
}

#[test]
fn restart_expect_waits_for_the_new_version_to_serve() {
    let a = Agent::new("launchd-expect");
    a.env.fleet(&["service", "install"]).ok();
    fs::create_dir_all(&a.env.data).unwrap();
    a.answer("print", a.running());
    a.answer(
        "kickstart",
        json!({
            "writes": a.env.data.join("manager.json"),
            "text": r#"{"pid":1,"boot":null,"version":"9.9.9","exe":"/f","config":"/c","startedAt":99999999999999,"state":"serving"}"#,
        }),
    );

    a.env
        .fleet(&["service", "restart", "--expect", "9.9.9", "--timeout", "10"])
        .ok()
        .stdout_has("fleet serves 9.9.9");

    assert!(
        a.calls()
            .contains(&format!("kickstart -k {}/fleet", a.domain)),
        "{:?}",
        a.calls()
    );
}

#[test]
fn restart_expect_reports_a_manager_that_refuses_to_start() {
    let a = Agent::new("launchd-expect-refuses");
    a.env.fleet(&["service", "install"]).ok();
    fs::create_dir_all(&a.env.data).unwrap();
    a.answer(
        "print",
        json!([
            a.running(),
            {"stdout": "gui/1/fleet = {\n\tstate = not running\n\tlast exit code = 78: EX_CONFIG\n}\n"},
        ]),
    );

    a.env
        .fleet(&["service", "restart", "--expect", "9.9.9", "--timeout", "10"])
        .code(1)
        .stderr_has("last exit code 78");
}
