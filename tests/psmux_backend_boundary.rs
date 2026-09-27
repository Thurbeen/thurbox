//! Config to persisted backend route for a native Windows psmux host.

use std::sync::Arc;
use thurbox::agent::BackendRegistry;
use thurbox::session::BackendChoice;

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
