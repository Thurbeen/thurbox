#![cfg(not(target_os = "linux"))]

mod support;

use support::Env;

#[test]
fn every_service_verb_says_it_drives_systemd_on_linux_only() {
    let env = Env::new("service-elsewhere");
    for verb in ["install", "start", "stop", "restart", "status", "uninstall"] {
        env.fleet(&["service", verb])
            .code(1)
            .stderr_has("on Linux only");
    }
}
