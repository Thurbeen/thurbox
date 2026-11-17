//! `fleet service`: Fleet writes and drives the one systemd user unit that
//! runs `fleet up`. A fake `systemctl` on a private PATH records each call,
//! so no test touches the operator's service manager.
#![cfg(target_os = "linux")]

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use serde_json::json;
use support::Env;

struct Service {
    env: Env,
    calls: PathBuf,
}

impl Service {
    fn new(name: &str) -> Service {
        let mut env = Env::new(name);
        let bin = env.root.join("service-bin");
        fs::create_dir_all(&bin).unwrap();
        let calls = env.root.join("systemctl-calls");
        let systemctl = bin.join("systemctl");
        fs::write(
            &systemctl,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nif [ \"$2\" = status ]; then echo \"$3 - Fleet manager\"; exit 3; fi\n",
                calls.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o755)).unwrap();
        env.only_on_path(&bin);
        Service { env, calls }
    }

    fn units(&self) -> PathBuf {
        self.env.root.join(".config/systemd/user")
    }

    fn unit(&self, name: &str) -> String {
        fs::read_to_string(self.units().join(name)).unwrap()
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(&self.calls)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn supervisor(&self) -> serde_json::Value {
        self.env.fleet(&["version", "--json"]).ok().json()["manager"]["supervisor"].clone()
    }
}

#[test]
fn install_writes_the_one_unit_and_version_names_it() {
    let s = Service::new("service-install");
    assert_eq!(s.supervisor(), json!(null));

    s.env.fleet(&["service", "install", "--now"]).ok();

    let unit = s.unit("fleet.service");
    let config = std::path::absolute(&s.env.config)
        .unwrap()
        .display()
        .to_string();
    let exe = fs::canonicalize(env!("CARGO_BIN_EXE_fleet"))
        .unwrap()
        .display()
        .to_string();
    for line in [
        "Type=notify".to_string(),
        format!("Environment=\"FLEET_CONFIG={config}\""),
        format!("ExecStart=\"{exe}\" --config ${{FLEET_CONFIG}} up"),
        "Restart=on-failure".into(),
        "RestartSec=2s".into(),
        "RestartSteps=6".into(),
        "RestartMaxDelaySec=2min".into(),
        "RestartPreventExitStatus=78".into(),
        "TimeoutStartSec=infinity".into(),
        "TimeoutStopSec=30s".into(),
        "KillMode=process".into(),
        "WantedBy=default.target".into(),
    ] {
        assert!(
            unit.lines().any(|l| l == line),
            "unit lacks {line:?}:\n{unit}"
        );
    }
    assert_eq!(
        s.calls(),
        ["--user daemon-reload", "--user enable --now fleet.service"]
    );
    assert_eq!(
        s.supervisor(),
        json!({"kind": "systemd", "unit": "fleet.service"})
    );
}

#[test]
fn install_adopts_a_unit_by_name_and_keeps_its_drop_ins() {
    let s = Service::new("service-adopt");
    fs::create_dir_all(s.units().join("engine.service.d")).unwrap();
    fs::write(
        s.units().join("engine.service"),
        "[Service]\nExecStart=/somewhere/fleet up\nRestartSec=15\n",
    )
    .unwrap();
    let drop_in = s.units().join("engine.service.d/path.conf");
    fs::write(&drop_in, "[Service]\nEnvironment=PATH=/opt/agents\n").unwrap();

    s.env
        .fleet(&["service", "install", "--name", "engine"])
        .ok();

    let unit = s.unit("engine.service");
    assert!(unit.contains("RestartSec=2s\n"), "{unit}");
    assert!(!unit.contains("RestartSec=15"), "{unit}");
    assert_eq!(
        fs::read_to_string(&drop_in).unwrap(),
        "[Service]\nEnvironment=PATH=/opt/agents\n"
    );
    assert_eq!(
        s.calls(),
        ["--user daemon-reload", "--user enable engine.service"]
    );
    assert_eq!(
        s.supervisor(),
        json!({"kind": "systemd", "unit": "engine.service"})
    );
}

#[test]
fn install_refuses_a_second_unit_for_the_same_store() {
    let s = Service::new("service-second");
    s.env.fleet(&["service", "install"]).ok();

    s.env
        .fleet(&["service", "install", "--name", "other"])
        .code(5)
        .stderr_has("fleet.service");

    assert!(!s.units().join("other.service").exists());
    s.env.fleet(&["service", "install"]).ok();
}

#[test]
fn install_refuses_to_take_over_another_store_s_unit() {
    let s = Service::new("service-foreign");
    fs::create_dir_all(s.units()).unwrap();
    let foreign = "[Unit]\nX-Fleet-Store=/elsewhere/store\n\n[Service]\nExecStart=fleet up\n";
    fs::write(s.units().join("fleet.service"), foreign).unwrap();

    s.env
        .fleet(&["service", "install"])
        .code(5)
        .stderr_has("/elsewhere/store");

    assert_eq!(s.unit("fleet.service"), foreign);
    assert!(s.calls().is_empty());
}

#[test]
fn the_verbs_drive_the_store_s_unit_and_uninstall_removes_it() {
    let s = Service::new("service-verbs");
    s.env
        .fleet(&["service", "install", "--name", "engine"])
        .ok();
    fs::create_dir_all(s.units().join("engine.service.d")).unwrap();
    fs::write(s.units().join("engine.service.d/x.conf"), "[Service]\n").unwrap();
    fs::remove_file(&s.calls).unwrap();

    s.env.fleet(&["service", "start"]).ok();
    s.env.fleet(&["service", "restart"]).ok();
    s.env.fleet(&["service", "stop"]).ok();
    s.env
        .fleet(&["service", "status"])
        .code(3)
        .stdout_has("engine.service - Fleet manager");
    s.env.fleet(&["service", "uninstall"]).ok();

    assert_eq!(
        s.calls(),
        [
            "--user start engine.service",
            "--user restart engine.service",
            "--user stop engine.service",
            "--user status engine.service --no-pager",
            "--user disable --now engine.service",
            "--user daemon-reload",
        ]
    );
    assert!(!s.units().join("engine.service").exists());
    assert!(s.units().join("engine.service.d/x.conf").exists());
    assert_eq!(s.supervisor(), json!(null));
    s.env
        .fleet(&["service", "start"])
        .code(3)
        .stderr_has("fleet service install");
}
