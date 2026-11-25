use serde_json::Value;
use thurbox::cli::toon;

#[test]
fn matches_the_reference_encoder_on_the_spec_suite() {
    let corpus = include_str!("fixtures/toon-conformance.jsonl");
    let mut checked = 0;
    let mut failures = Vec::new();

    for line in corpus.lines().filter(|l| !l.trim().is_empty()) {
        let case: Value = serde_json::from_str(line).expect("fixture line is JSON");
        let delimiter = case["delimiter"]
            .as_str()
            .and_then(|d| d.chars().next())
            .expect("delimiter");
        let indent = case["indent"].as_u64().expect("indent") as usize;
        let expected = case["expected"].as_str().expect("expected");

        let encoded = toon::encode_with(&case["input"], delimiter, indent);
        checked += 1;
        if encoded != expected {
            failures.push(format!(
                "[§{}] {}\n    want {expected:?}\n    got  {encoded:?}",
                case["section"], case["name"]
            ));
        }
    }

    assert!(checked > 150, "only {checked} cases loaded from the corpus; an emptied corpus would pass with no coverage"
    );
    assert!(
        failures.is_empty(),
        "{} of {checked} cases diverge from the reference encoder:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn default_encoding_is_comma_delimited_and_two_space_indented() {
    let value = serde_json::json!({ "items": [{ "a": 1, "b": "x" }, { "a": 2, "b": "y" }] });
    assert_eq!(toon::encode(&value), toon::encode_with(&value, ',', 2));
    assert_eq!(toon::encode(&value), "items[2]{a,b}:\n  1,x\n  2,y");
}
