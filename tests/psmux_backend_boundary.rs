//! Config to persisted backend route for a native Windows psmux host.

use std::sync::Arc;
use thurbox::agent::BackendRegistry;
use thurbox::session::{BackendChoice, HostDef, HostRegistry};

#[test]
fn a_psmux_host_registers_under_its_own_persisted_route() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("hosts.toml"),
        "[[hosts]]\nname = 'native-win'\ndestination = 'unused'\nmultiplexer = 'psmux'\n",
    )
    .unwrap();
    std::env::set_var("THURBOX_CONFIG_DIR", &config);

    let (backends, hosts, warnings) = BackendRegistry::from_configured_hosts();
    assert!(warnings.is_empty(), "{warnings:?}");
    let host = hosts.get("native-win").unwrap().clone();
    let choice = BackendChoice::resolve(Some(host), None, None).unwrap();

    assert_eq!(choice.backend_type, "ssh:native-win:psmux");
    assert!(backends.supports_choice(&choice));
    assert!(backends.has("ssh:native-win:psmux"));
    assert!(backends.has("ssh:native-win:tmux"));
    assert!(backends.has("ssh:native-win"));
    let psmux = backends.get("ssh:native-win:psmux").unwrap();
    assert_eq!(psmux.name(), "ssh:native-win:psmux");
    assert!(Arc::ptr_eq(psmux, backends.get("ssh:native-win").unwrap()));
    assert_eq!(
        hosts
            .resolved_by_backend("ssh:native-win:psmux")
            .unwrap()
            .mux(),
        "psmux"
    );
    assert_eq!(
        hosts.resolved_by_backend("ssh:native-win").unwrap().mux(),
        "psmux"
    );
    assert!(psmux.needs_liveness_poll());
    assert!(!psmux.supports_snapshots());
    #[cfg(windows)]
    {
        let local = BackendChoice::resolve(None, None, None).unwrap();
        assert_eq!(local.backend_type, "local-psmux");
        assert!(backends.supports_choice(&local));
        assert!(backends.has("local-tmux"));
        assert!(Arc::ptr_eq(
            backends.get("local-psmux").unwrap(),
            backends.get("local-tmux").unwrap()
        ));
    }
}

#[test]
fn a_host_preference_change_keeps_explicit_mux_routes_available() {
    let hosts = HostRegistry {
        hosts: vec![HostDef {
            name: "changed-host".into(),
            destination: "unused".into(),
            multiplexer: Some("tmux".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let backends = BackendRegistry::from_host_registry(&hosts);
    let host = hosts.get("changed-host").unwrap().clone();
    let tmux = BackendChoice::resolve(Some(host.clone()), None, None).unwrap();
    assert_eq!(tmux.backend_type, "ssh:changed-host:tmux");
    assert!(backends.supports_choice(&tmux));
    let psmux = BackendChoice::resolve(Some(host), Some("psmux"), None).unwrap();
    assert_eq!(psmux.backend_type, "ssh:changed-host:psmux");
    assert!(backends.supports_choice(&psmux));
    assert_eq!(
        hosts
            .resolved_by_backend(&psmux.backend_type)
            .unwrap()
            .mux(),
        "psmux"
    );
    assert!(backends.has("ssh:changed-host"));
    assert!(backends.has("ssh:changed-host:tmux"));
}

#[test]
fn a_literal_suffix_host_keeps_its_legacy_route() {
    let hosts = HostRegistry {
        hosts: vec![
            HostDef {
                name: "box:psmux".into(),
                destination: "unused".into(),
                ..Default::default()
            },
            HostDef {
                name: "box".into(),
                destination: "unused".into(),
                multiplexer: Some("tmux".into()),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let backends = BackendRegistry::from_host_registry(&hosts);
    let literal =
        BackendChoice::resolve(Some(hosts.get("box:psmux").unwrap().clone()), None, None).unwrap();
    assert!(backends.supports_choice(&literal));
    let qualified =
        BackendChoice::resolve(Some(hosts.get("box").unwrap().clone()), Some("psmux"), None)
            .unwrap();
    assert_eq!(literal.backend_type, "ssh:box:psmux:tmux");
    assert_eq!(qualified.backend_type, "ssh:box:psmux");
    assert!(Arc::ptr_eq(
        backends.get("ssh:box:psmux").unwrap(),
        backends.get(&literal.backend_type).unwrap()
    ));
    assert!(!backends.supports_choice(&qualified));
}
