//! A configured psmux host must be checked with psmux's server rules.

#[cfg(unix)]
#[test]
fn psmux_host_rejects_a_server_too_old_to_spawn_agents() {
    use std::os::unix::fs::PermissionsExt;
    use thurbox::agent::{psmux::PsmuxBackend, SessionBackend};
    use thurbox::session::HostDef;

    let dir = tempfile::tempdir().unwrap();
    let ssh = dir.path().join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\nprintf 'tmux 3.3.5\\n'\n").unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let old_path = std::env::var_os("PATH");
    std::env::set_var("PATH", format!("{}:/usr/bin:/bin", dir.path().display()));

    let host = HostDef {
        name: "winbox".into(),
        destination: "unused".into(),
        multiplexer: Some("psmux".into()),
        ..Default::default()
    };
    let result = PsmuxBackend::from_host(&host).check_available();
    if let Some(path) = old_path {
        std::env::set_var("PATH", path);
    }
    let err = result.expect_err("psmux 3.3.5 cannot safely spawn agents");
    assert!(err.to_string().contains("too old"), "{err:#}");
}
