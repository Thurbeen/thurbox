//! Fleet's documentation index names every kind this build serves.
//!
//! `docs/fleet/README.md` is where a reader learns what each resource is
//! for. A kind added to [`fleet_api::kinds::BUILT_IN`] fails this test until
//! that table has a row for it, linked to the kind's own page.

const README: &str = include_str!("../../../docs/fleet/README.md");

#[test]
fn the_index_has_a_row_for_every_built_in_kind() {
    let missing: Vec<&str> = fleet_api::kinds::BUILT_IN
        .iter()
        .copied()
        .filter(|kind| {
            let row = format!("| [{kind}](resources/");
            !README.lines().any(|line| line.starts_with(&row))
        })
        .collect();
    assert!(
        missing.is_empty(),
        "docs/fleet/README.md has no resource row `| [Kind](resources/…)` for {missing:?}"
    );
}
