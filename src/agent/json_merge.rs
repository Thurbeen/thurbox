//! Reversible, non-destructive JSON deep-merge — the primitive behind the
//! `[[config_merges]]` extension directive, used to inject agent hook entries
//! into an agent's *own* shared config file (e.g. `~/.gemini/settings.json`)
//! without clobbering the user's other settings.
//!
//! - [`merge`]: objects merge recursively; arrays union by deep-equality (append
//!   source items not already present). Idempotent, and a type conflict with the
//!   user's value is left alone (never overwritten/corrupted).
//! - [`prune_marked`]: remove our entries on uninstall by a **marker** the
//!   shipped entries carry (our hook commands all contain the `session signal`
//!   marker). Marker-based — not value-based — so it stays correct even after the
//!   shipped payload's schema changes across an extension update (no orphans).

use serde_json::Value;

/// Deep-merge `source` into `target` in place (see module docs).
pub fn merge(target: &mut Value, source: &Value) {
    match (target, source) {
        (Value::Object(t), Value::Object(s)) => {
            for (k, sv) in s {
                merge(t.entry(k.clone()).or_insert(Value::Null), sv);
            }
        }
        (Value::Array(t), Value::Array(s)) => {
            for sv in s {
                if !t.iter().any(|tv| tv == sv) {
                    t.push(sv.clone());
                }
            }
        }
        // Target absent (a freshly-inserted `Null`): take the source.
        (t @ Value::Null, s) => *t = s.clone(),
        // Type conflict (the user has a different shape here): leave their value
        // untouched rather than corrupt it.
        _ => {}
    }
}

/// Remove every array element whose serialized JSON contains `marker`, anywhere
/// in `value`, then prune object keys whose value became an empty object/array.
/// Used to reverse a [`merge`] of hook entries on uninstall: every entry we ship
/// carries the marker, so this removes exactly (and only) ours, regardless of
/// which payload version added them.
pub fn prune_marked(value: &mut Value, marker: &str) {
    match value {
        Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            for k in keys {
                if let Some(v) = map.get_mut(&k) {
                    // Only prune a key that *we* emptied (it held marked entries):
                    // a user's pre-existing empty `{}`/`[]` must survive untouched.
                    let was_empty = is_empty_container(v);
                    prune_marked(v, marker);
                    if !was_empty && is_empty_container(v) {
                        map.remove(&k);
                    }
                }
            }
        }
        Value::Array(arr) => {
            arr.retain(|item| {
                !serde_json::to_string(item)
                    .map(|s| s.contains(marker))
                    .unwrap_or(false)
            });
            for item in arr.iter_mut() {
                prune_marked(item, marker);
            }
        }
        _ => {}
    }
}

/// An empty object or empty array.
fn is_empty_container(v: &Value) -> bool {
    matches!(v, Value::Object(o) if o.is_empty()) || matches!(v, Value::Array(a) if a.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const M: &str = "thurbox-cli session signal";

    fn ours(state: &str) -> Value {
        json!({"hooks": [{"type": "command", "command": format!("{M} --state {state} || true")}]})
    }

    #[test]
    fn merge_into_empty_takes_source() {
        let mut t = json!({});
        let s = json!({"hooks": {"Stop": [ours("done")]}});
        merge(&mut t, &s);
        assert_eq!(t, s);
    }

    #[test]
    fn merge_preserves_user_entries_and_unions_arrays() {
        let mut t = json!({
            "theme": "dark",
            "hooks": {"Stop": [{"command": "user"}], "Other": [1]}
        });
        merge(&mut t, &json!({"hooks": {"Stop": [ours("done")]}}));
        assert_eq!(t["theme"], json!("dark"));
        assert_eq!(t["hooks"]["Other"], json!([1]));
        let stop = t["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2); // user's + ours
        assert_eq!(stop[0], json!({"command": "user"}));
    }

    #[test]
    fn merge_is_idempotent() {
        let mut t = json!({});
        let s = json!({"hooks": {"Stop": [ours("done")]}});
        merge(&mut t, &s);
        merge(&mut t, &s);
        assert_eq!(t["hooks"]["Stop"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn merge_does_not_corrupt_a_type_conflict() {
        // User has `hooks` as a string; we must not overwrite it with our object.
        let mut t = json!({"hooks": "disabled"});
        merge(&mut t, &json!({"hooks": {"Stop": [ours("done")]}}));
        assert_eq!(t, json!({"hooks": "disabled"}));
    }

    #[test]
    fn prune_removes_only_marked_and_prunes_empties() {
        let mut t = json!({"hooks": {"Stop": [{"command": "user"}, ours("done")]}});
        prune_marked(&mut t, M);
        // ours gone, user kept, Stop not pruned (still has the user entry).
        assert_eq!(t, json!({"hooks": {"Stop": [{"command": "user"}]}}));
    }

    #[test]
    fn merge_then_prune_restores_original() {
        let original = json!({"theme": "dark", "hooks": {"PreToolUse": [{"command": "user"}]}});
        let source = json!({"hooks": {"Stop": [ours("done")], "PreToolUse": [ours("working")]}});
        let mut doc = original.clone();
        merge(&mut doc, &source);
        prune_marked(&mut doc, M);
        assert_eq!(doc, original);
    }

    #[test]
    fn prune_keeps_user_preexisting_empties() {
        // Empty `{}`/`[]` are common in real settings.json — we must never
        // destroy the user's own, only ones we emptied.
        let original = json!({"mcpServers": {}, "excludeTools": [], "hooks": {}});
        let mut doc = original.clone();
        prune_marked(&mut doc, M);
        assert_eq!(doc, original);
    }

    #[test]
    fn prune_handles_empty_and_nonempty_siblings_at_merged_path() {
        let mut doc = json!({
            "hooks": {
                "OurEvent": [ours("done")],     // we own this
                "UserEmpty": [],                // user's empty sibling — keep
                "UserKept": [{"command": "x"}]  // user's real sibling — keep
            }
        });
        prune_marked(&mut doc, M);
        assert_eq!(
            doc,
            json!({"hooks": {"UserEmpty": [], "UserKept": [{"command": "x"}]}})
        );
    }

    #[test]
    fn prune_is_version_skew_proof() {
        // Old payload merged schema A; later payload ships schema B. Prune by the
        // shared marker removes BOTH (no orphans), leaving only the user's entry.
        let mut doc = json!({"hooks": {"Stop": [{"command": "user"}]}});
        merge(&mut doc, &json!({"hooks": {"OldEvent": [ours("done")]}})); // vA
        merge(&mut doc, &json!({"hooks": {"NewEvent": [ours("done")]}})); // vB
        prune_marked(&mut doc, M);
        assert_eq!(doc, json!({"hooks": {"Stop": [{"command": "user"}]}}));
    }
}
