//! Every bundled screen rule against the agent screens it was written from.
//!
//! Each fixture under `fixtures/screens/<harness>/` is one real screen of
//! that agent, captured through runtime-mux: its text and title, the state
//! it shows and the rule that must say so. Every bundled rule is covered by
//! at least one fixture, so a rule no real screen shows cannot ship.

use std::collections::BTreeSet;

use fleet_api::harness::profile;
use fleet_api::screen::{Screen, ScreenState, bundled};
use serde::Deserialize;

macro_rules! fixtures {
    ($($harness:literal / $name:literal),* $(,)?) => {
        &[$(($harness, $name, include_str!(concat!("fixtures/screens/", $harness, "/", $name, ".toml")))),*]
    };
}

const FIXTURES: &[(&str, &str, &str)] = fixtures![
    "claude" / "idle",
    "claude" / "idle-after-turn",
    "claude" / "working",
    "claude" / "permission",
    "codex" / "idle",
    "codex" / "idle-after-deny",
    "codex" / "working",
    "codex" / "working-streaming",
    "codex" / "approval",
    "copilot" / "idle",
    "copilot" / "idle-after-deny",
    "copilot" / "working",
    "copilot" / "permission",
    "copilot" / "folder-trust",
    "opencode" / "idle",
    "opencode" / "working",
    "opencode" / "permission",
    "pi" / "idle",
    "pi" / "retrying",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    state: ScreenState,
    rule: String,
    title: String,
    text: String,
}

#[test]
fn each_captured_screen_shows_its_state_through_its_rule() {
    let mut failed = Vec::new();
    for (harness, name, text) in FIXTURES {
        let f: Fixture = toml::from_str(text).unwrap_or_else(|e| panic!("{harness}/{name}: {e}"));
        let screen = Screen {
            text: f.text,
            title: f.title,
        };
        let seen = profile(harness)
            .screen_rules()
            .and_then(|rules| rules.evaluate(&screen));
        let got = seen.as_ref().map(|s| (s.state, s.rule.as_str()));
        if got != Some((f.state, f.rule.as_str())) {
            failed.push(format!(
                "{harness}/{name}: want {:?} by {}, got {got:?}",
                f.state, f.rule
            ));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

#[test]
fn every_bundled_rule_is_shown_by_a_captured_screen() {
    let covered: BTreeSet<String> = FIXTURES
        .iter()
        .map(|(_, _, text)| toml::from_str::<Fixture>(text).expect("fixture").rule)
        .collect();
    let uncovered: Vec<String> = bundled()
        .iter()
        .flat_map(|set| {
            set.rules
                .iter()
                .map(move |r| format!("{}/{}", set.name, r.id))
        })
        .filter(|id| !covered.contains(id))
        .collect();
    assert!(
        uncovered.is_empty(),
        "no captured screen shows {uncovered:?}"
    );
    assert!(!bundled().is_empty(), "Fleet ships screen rules");
}
