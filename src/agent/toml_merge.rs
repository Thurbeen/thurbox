//! Reversible, non-destructive **TOML** deep-merge — [`crate::agent::json_merge`]'s
//! sibling for a `[[config_merges]]` whose target is TOML rather than JSON.
//!
//! It exists because kimi (Kimi Code CLI) reads its hooks from one shared file,
//! `~/.kimi-code/config.toml`, and offers no drop-in hooks directory: dropping a
//! managed file there would clobber the user's whole configuration, and the JSON
//! merge cannot read it. The semantics are deliberately the same as the JSON
//! one, so a manifest author reasons about one mechanism:
//!
//! - [`merge`]: tables merge recursively; arrays (both `[[array of tables]]` and
//!   inline arrays) union by rendered equality — an entry the target already has
//!   is not appended twice. Idempotent, and a **type conflict with the user's
//!   value is left alone** (never overwritten).
//! - [`prune_marked`]: remove our entries on uninstall by the **marker** every
//!   shipped hook command carries, so it stays correct across payload changes.
//!
//! The one behaviour JSON does not need: the target is parsed with `toml_edit`
//! rather than `toml`, so the user's comments, key order and spacing survive a
//! merge into a file they wrote by hand. The consequence for a manifest author
//! is that the *shape* has to match — a target spelling kimi's hooks as an
//! inline `hooks = [{…}]` array where the payload ships `[[hooks]]` tables is a
//! type conflict, so it is left untouched and the agent goes unreported rather
//! than having its config rewritten into the other shape.

use toml_edit::{DocumentMut, Item, Value};

/// Deep-merge `source` into `target` in place (see module docs).
pub fn merge(target: &mut DocumentMut, source: &DocumentMut) {
    merge_item(target.as_item_mut(), source.as_item());
}

fn merge_item(target: &mut Item, source: &Item) {
    match (target, source) {
        (Item::Table(t), Item::Table(s)) => {
            for (key, sv) in s.iter() {
                match t.get_mut(key) {
                    Some(tv) => merge_item(tv, sv),
                    None => {
                        t.insert(key, sv.clone());
                    }
                }
            }
        }
        (Item::ArrayOfTables(t), Item::ArrayOfTables(s)) => {
            for entry in s.iter() {
                let rendered = entry.to_string();
                if !t.iter().any(|e| e.to_string() == rendered) {
                    t.push(entry.clone());
                }
            }
        }
        (Item::Value(Value::Array(t)), Item::Value(Value::Array(s))) => {
            for entry in s.iter() {
                let rendered = entry.to_string();
                if !t.iter().any(|e| e.to_string() == rendered) {
                    t.push_formatted(entry.clone());
                }
            }
        }
        // Type conflict (the user has a different shape here): leave their
        // value untouched rather than corrupt it.
        _ => {}
    }
}

/// Remove every array entry whose rendered TOML contains `marker`, anywhere in
/// `doc`, then drop keys whose array *we* emptied. Reverses a [`merge`] of hook
/// entries on uninstall: every entry we ship carries the marker in its command,
/// so this removes exactly (and only) ours.
pub fn prune_marked(doc: &mut DocumentMut, marker: &str) {
    prune_item(doc.as_item_mut(), marker);
}

fn prune_item(item: &mut Item, marker: &str) {
    match item {
        Item::Table(table) => {
            let keys: Vec<String> = table.iter().map(|(k, _)| k.to_string()).collect();
            for key in keys {
                let Some(value) = table.get_mut(&key) else {
                    continue;
                };
                // Only drop a key that *we* emptied: a user's pre-existing empty
                // array must survive untouched.
                let was_empty = is_empty_array(value);
                prune_item(value, marker);
                if !was_empty && is_empty_array(value) {
                    table.remove(&key);
                }
            }
        }
        Item::ArrayOfTables(entries) => {
            entries.retain(|e| !e.to_string().contains(marker));
        }
        Item::Value(Value::Array(entries)) => {
            entries.retain(|e| !e.to_string().contains(marker));
        }
        _ => {}
    }
}

fn is_empty_array(item: &Item) -> bool {
    match item {
        Item::ArrayOfTables(entries) => entries.is_empty(),
        Item::Value(Value::Array(entries)) => entries.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: &str = "thurbox-cli session signal";

    fn doc(text: &str) -> DocumentMut {
        text.parse().expect("valid TOML")
    }

    fn ours() -> DocumentMut {
        doc(&format!(
            "[[hooks]]\nevent = \"Stop\"\ncommand = \"{M} --state done || true\"\n"
        ))
    }

    #[test]
    fn merge_into_empty_takes_source() {
        let mut target = doc("");
        merge(&mut target, &ours());
        assert!(target.to_string().contains("--state done"));
    }

    #[test]
    fn merge_preserves_user_entries_comments_and_unrelated_keys() {
        let mut target = doc("# my config\nmodel = \"kimi-for-coding\"\n\n\
             [[hooks]]\nevent = \"Stop\"\ncommand = \"notify-send done\"\n");
        merge(&mut target, &ours());
        let out = target.to_string();
        assert!(out.contains("# my config"), "comment survived: {out}");
        assert!(out.contains("model = \"kimi-for-coding\""));
        assert!(out.contains("notify-send done"), "user hook survived");
        assert!(out.contains("--state done"), "ours appended");
        assert_eq!(doc(&out)["hooks"].as_array_of_tables().unwrap().len(), 2);
    }

    #[test]
    fn merge_is_idempotent() {
        let mut target = doc("");
        merge(&mut target, &ours());
        let once = target.to_string();
        merge(&mut target, &ours());
        assert_eq!(target.to_string(), once);
    }

    #[test]
    fn a_type_conflict_leaves_the_users_value_alone() {
        // The user spells hooks as an inline array of inline tables; we ship
        // `[[hooks]]`. Rewriting their file into the other shape is not ours to
        // do, so nothing is merged.
        let mut target = doc("hooks = [{ event = \"Stop\", command = \"mine\" }]\n");
        merge(&mut target, &ours());
        assert!(!target.to_string().contains("--state done"));
        assert!(target.to_string().contains("mine"));
    }

    #[test]
    fn prune_removes_exactly_our_marked_entries() {
        let mut target = doc("model = \"kimi-for-coding\"\n\n\
             [[hooks]]\nevent = \"Stop\"\ncommand = \"notify-send done\"\n");
        merge(&mut target, &ours());
        prune_marked(&mut target, M);
        let out = target.to_string();
        assert!(!out.contains(M), "ours gone: {out}");
        assert!(out.contains("notify-send done"), "user hook survived");
        assert!(out.contains("model = \"kimi-for-coding\""));
    }

    #[test]
    fn prune_drops_a_key_it_emptied_but_not_one_the_user_left_empty() {
        let mut target = doc("");
        merge(&mut target, &ours());
        prune_marked(&mut target, M);
        assert!(!target.to_string().contains("hooks"));

        let mut users_empty = doc("hooks = []\n");
        prune_marked(&mut users_empty, M);
        assert!(users_empty.to_string().contains("hooks = []"));
    }

    #[test]
    fn prune_is_a_no_op_for_marker_free_content() {
        let mut target = doc("[[hooks]]\nevent = \"Stop\"\ncommand = \"mine\"\n");
        let before = target.to_string();
        prune_marked(&mut target, M);
        assert_eq!(target.to_string(), before);
    }
}
