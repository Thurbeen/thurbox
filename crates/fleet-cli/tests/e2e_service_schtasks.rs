#![cfg(windows)]

mod support;

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};
use support::Env;

struct Task {
    env: Env,
}

impl Task {
    fn new(name: &str) -> Task {
        let task = Task {
            env: Env::service_stub(name, "schtasks"),
        };
        task.answer("/Query", task.status("Ready", "0"));
        task
    }

    fn answer(&self, verb: &str, answer: Value) {
        self.env.edit_stub(|s| s["service"][verb] = answer);
    }

    fn status(&self, status: &str, last: &str) -> Value {
        json!({"stdout": format!(
            "\r\nFolder: \\\r\nHostName:      HOST\r\nTaskName:      \\fleet\r\nStatus:        {status}\r\nLast Result:   {last}\r\n"
        )})
    }

    fn xml(&self) -> PathBuf {
        ["AppData", "Local", "fleet", "service", "fleet.xml"]
            .iter()
            .fold(self.env.root.clone(), |p, part| p.join(part))
    }

    fn xml_text(&self) -> String {
        let bytes = fs::read(self.xml()).unwrap();
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        String::from_utf16(&units).unwrap()
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
fn install_registers_the_one_logon_task_and_version_names_it() {
    let t = Task::new("schtasks-install");
    assert_eq!(t.supervisor(), json!(null));

    t.env.fleet(&["service", "install", "--now"]).ok();

    let xml = t.xml_text();
    assert!(xml.starts_with('\u{feff}'), "no byte order mark");
    for part in [
        "fleet.exe</Command>".to_string(),
        "<LogonTrigger>".into(),
        "<RestartOnFailure>".into(),
        "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>".into(),
        "<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>".into(),
        "X-Fleet-Store=".into(),
    ] {
        assert!(xml.contains(&part), "task lacks {part:?}:\n{xml}");
    }
    assert!(xml.contains(" up</Arguments>"), "{xml}");
    assert_eq!(
        t.calls(),
        [
            format!("/Create /TN fleet /XML {} /F", t.xml().display()),
            "/Run /TN fleet".to_string(),
        ]
    );
    assert_eq!(
        t.supervisor(),
        json!({"kind": "task-scheduler", "unit": "fleet"})
    );
}

#[test]
fn the_verbs_drive_the_store_s_task_and_uninstall_removes_it() {
    let t = Task::new("schtasks-verbs");
    t.env.fleet(&["service", "install"]).ok();
    t.answer("/Query", t.status("Running", "267009"));
    t.forget_calls();

    t.env.fleet(&["service", "start"]).ok();
    t.env.fleet(&["service", "restart"]).ok();
    t.env.fleet(&["service", "stop"]).ok();
    t.env
        .fleet(&["service", "status"])
        .ok()
        .stdout_has("Running");
    t.env.fleet(&["service", "uninstall"]).ok();

    let calls = t.calls();
    for call in ["/Run /TN fleet", "/End /TN fleet", "/Delete /TN fleet /F"] {
        assert!(calls.iter().any(|c| c == call), "{call:?} not in {calls:?}");
    }
    assert!(!t.xml().exists());
    assert_eq!(t.supervisor(), json!(null));
    t.env
        .fleet(&["service", "start"])
        .code(3)
        .stderr_has("fleet service install");
}

#[test]
fn restart_expect_waits_for_the_new_version_to_serve() {
    let t = Task::new("schtasks-expect");
    t.env.fleet(&["service", "install"]).ok();
    fs::create_dir_all(&t.env.data).unwrap();
    t.answer("/Query", t.status("Running", "267009"));
    t.answer(
        "/Run",
        json!({
            "writes": t.env.data.join("manager.json"),
            "text": r#"{"pid":1,"boot":null,"version":"9.9.9","exe":"/f","config":"/c","startedAt":99999999999999,"state":"serving"}"#,
        }),
    );

    t.env
        .fleet(&["service", "restart", "--expect", "9.9.9", "--timeout", "10"])
        .ok()
        .stdout_has("fleet serves 9.9.9");
}

#[test]
fn restart_expect_reports_a_manager_that_refuses_to_start() {
    let t = Task::new("schtasks-expect-refuses");
    t.env.fleet(&["service", "install"]).ok();
    fs::create_dir_all(&t.env.data).unwrap();
    t.answer(
        "/Query",
        json!([t.status("Running", "267009"), t.status("Ready", "78")]),
    );

    t.env
        .fleet(&["service", "restart", "--expect", "9.9.9", "--timeout", "10"])
        .code(1)
        .stderr_has("last result 78");
}
