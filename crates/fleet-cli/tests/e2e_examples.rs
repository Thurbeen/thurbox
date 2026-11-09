//! The shipped fresh-config example is executable without Python or external tools.
mod support;
use support::Env;

#[test]
fn shipped_example_applies_and_reconciles_in_a_fresh_isolated_store() {
    let mut e = Env::without_tools("shipped-example");
    e.fake = e.root.join("fake-host");
    std::fs::write(
        &e.config,
        include_str!("../../../examples/fleet/fleet.toml"),
    )
    .expect("example config");
    let mut resources: serde_json::Value =
        serde_json::from_str(include_str!("../../../examples/fleet/resources.json"))
            .expect("example manifest");
    e.apply(resources.clone()).ok();
    e.fleet(&["list", "Task", "--json"])
        .ok()
        .stdout_has("example");
    e.reconcile().ok();
    assert_eq!(e.calls("spawn"), 0, "suspended example started work");
    resources[0]["spec"]["dispatch"] = true.into();
    e.apply(resources).ok();
    e.reconcile().ok();
    assert_eq!(e.calls("spawn"), 1);
    let generation = e.get("example")["metadata"]["generation"]
        .as_u64()
        .expect("generation")
        .to_string();
    e.fleet(&[
        "result",
        "example",
        "--generation",
        &generation,
        "--outcome",
        "shipped",
        "--summary",
        "isolated example complete",
    ])
    .ok();
    e.reconcile().ok();
    e.host()
        .set_at_rest(&e.session_key("example"), true)
        .expect("worker at rest");
    e.fleet(&["delete", "Task", "example"]).ok();
    e.reconcile().ok();
    e.fleet(&["get", "Task", "example"]).code(3);
    e.assert_no_temp();
}
