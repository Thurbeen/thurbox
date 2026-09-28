//! Backend identity and location are public contract types, independent of tmux.

use thurbox::agent::backend::{DiscoveredSession, Located, WindowRole};

#[test]
fn shared_backend_identity_and_location_are_usable_without_tmux_types() {
    let found = DiscoveredSession {
        backend_id: "pane-7".into(),
        name: "session".into(),
        is_alive: true,
        session: "row-7".into(),
        role: WindowRole::Agent,
    };

    assert_eq!(found.role.as_str(), "agent");
    assert_eq!(
        Located::At(found.backend_id).pane().as_deref(),
        Some("pane-7")
    );
    assert!(Located::Absent.is_absent());
    assert!(!Located::Unknown.is_absent());
}
