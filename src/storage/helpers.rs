//! Shared serialization helpers for storage modules.

use std::collections::HashMap;

/// Convert a comma-separated string to a Vec<String>, filtering empty entries.
pub(crate) fn csv_to_vec(csv: &str) -> Vec<String> {
    if csv.is_empty() {
        Vec::new()
    } else {
        csv.split(',').map(|s| s.to_string()).collect()
    }
}

/// Convert a Vec<String> to a comma-separated string.
pub(crate) fn vec_to_csv(v: &[String]) -> String {
    v.join(",")
}

/// Deserialize a JSON string to a HashMap of environment variables.
///
/// Returns an empty map on empty input or on a JSON parse error — these
/// values are seeded by the application, so a bad row falls back to "no
/// env" rather than failing the whole row read.
pub(crate) fn json_to_env(json: &str) -> HashMap<String, String> {
    if json.is_empty() {
        HashMap::new()
    } else {
        serde_json::from_str(json).unwrap_or_default()
    }
}

/// Serialize a HashMap of environment variables to a JSON string.
pub(crate) fn env_to_json(env: &HashMap<String, String>) -> String {
    if env.is_empty() {
        String::new()
    } else {
        serde_json::to_string(env).unwrap_or_default()
    }
}
