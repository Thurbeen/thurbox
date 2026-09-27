//! The lifecycle contract must be usable without importing a tmux backend.

#[test]
fn shared_backend_contract_has_no_tmux_type_dependency() {
    let contract = include_str!("../src/agent/backend.rs");
    for forbidden in [
        "use crate::agent::tmux::WindowRole",
        "crate::agent::tmux::WindowRole",
        "crate::agent::tmux::Located",
    ] {
        assert!(
            !contract.contains(forbidden),
            "backend contract still depends on tmux type: {forbidden}"
        );
    }
}

#[test]
fn headless_capabilities_must_be_implemented_explicitly() {
    let contract = include_str!("../src/agent/backend.rs");
    for (method, result) in [
        ("spawn_headless", "Result<String>"),
        ("headless_discover", "Result<Vec<DiscoveredSession>>"),
    ] {
        let after_name = contract.split_once(&format!("fn {method}(")).unwrap().1;
        let after_result = after_name.split_once(result).unwrap().1;
        assert!(
            after_result.trim_start().starts_with(';'),
            "{method} must be required by the backend contract"
        );
    }
}
