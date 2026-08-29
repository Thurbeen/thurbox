//! TOON encoder conformance, differential against the reference implementation.
//!
//! `tests/fixtures/toon-conformance.jsonl` is the official TOON v4.1 spec test
//! suite (<https://github.com/toon-format/spec>, MIT) re-encoded by the
//! reference JavaScript encoder `@toon-format/toon`: one JSON object per line
//! carrying the input, the encoder options, and the reference output. The
//! corpus covers every section of the spec — primitives and number
//! canonicalization, quoting and escaping, objects, inline/list/tabular/keyed
//! tabular arrays, all three delimiters and a non-default indent.
//!
//! Regenerate it with `node scripts/dev/gen-toon-fixtures.mjs <spec>/tests/
//! fixtures/encode tests/fixtures/toon-conformance.jsonl` after cloning the
//! spec repository. That script deep-sorts each input's keys before handing it
//! to the reference encoder, because `serde_json::Map` is a `BTreeMap` and
//! yields keys alphabetically; sorting both sides makes the comparison about
//! encoding rather than about key order, which the spec leaves to the encoder.

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

    // A corpus that silently emptied would otherwise pass with zero coverage.
    assert!(checked > 150, "only {checked} cases loaded from the corpus");
    assert!(
        failures.is_empty(),
        "{} of {checked} cases diverge from the reference encoder:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn default_encoding_is_comma_delimited_and_two_space_indented() {
    // `encode` is the document default the CLI uses; `encode_with` is the
    // parameterized core the corpus above drives.
    let value = serde_json::json!({ "items": [{ "a": 1, "b": "x" }, { "a": 2, "b": "y" }] });
    assert_eq!(toon::encode(&value), toon::encode_with(&value, ',', 2));
    assert_eq!(toon::encode(&value), "items[2]{a,b}:\n  1,x\n  2,y");
}
