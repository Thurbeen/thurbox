//! Reversible, non-destructive **TOML** deep-merge — [`crate::agent::json_merge`]'s
//! sibling for a `[[config_merges]]` whose target is TOML rather than JSON.
//!
//! It exists because kimi (Kimi Code CLI) reads its hooks from one shared file,
//! `~/.kimi-code/config.toml`, and offers no drop-in hooks directory: dropping a
//! managed file there would clobber the user's whole configuration, and the JSON
//! merge cannot read it.
//!
//! - [`merge`]: tables merge recursively; arrays (both `[[array of tables]]` and
//!   inline arrays) union by rendered equality — an entry the target already has
//!   is not appended twice. Idempotent, and a **type conflict with the user's
//!   value is left alone** (never overwritten).
//! - [`prune_owned`]: remove our entries — and only ours — by an **ownership
//!   marker carried in each entry's own comment**.
//!
//! # Why ownership is a comment and not a content match
//!
//! [`crate::agent::json_merge::prune_marked`] identifies our entries by finding
//! a marker *in the entry's content* (every shipped hook command contains
//! `thurbox-cli session signal`). That is the only thing JSON offers — it has no
//! comments, and an ownership *key* inside the entry is not available either:
//! kimi accepts exactly four keys per hook and refuses to load the entire config
//! file when it sees a fifth. But a content match cannot answer either half of
//! the question it is asked:
//!
//! - It cannot tell **our** entry from the user's. `extensions/hooks/README.md`
//!   tells a user with an uninstrumented agent to call `thurbox-cli session
//!   signal` from their own hook; a content match then deletes that hook on
//!   uninstall — destroying configuration we never wrote.
//! - It cannot recognise **our own previous entry** once its content changes. A
//!   payload that renames an event or edits a command leaves the old entry
//!   behind, so updates accumulate duplicates.
//!
//! TOML has one thing JSON does not: comment decor that `toml_edit` binds to the
//! entry and carries through a render/re-parse round trip. So an entry is ours
//! **iff its own comment says so**, which answers both halves — a user hook that
//! merely mentions the signal command is not ours, and an entry of ours whose
//! content has since changed still is. Callers pair [`prune_owned`] with
//! [`merge`] (prune, then merge) so an update replaces our entries rather than
//! stacking a second copy beside them.
//!
//! The marker's one failure mode is a third-party tool that strips comments
//! while rewriting the file: our entries stop being recognised, so the next
//! install appends a second copy instead of replacing. That degrades to a
//! duplicate signal — the same state written twice, which is idempotent — and is
//! deliberately preferred to a content rule that deletes the user's own hooks.
//!
//! The target is parsed with `toml_edit` rather than `toml`, so the user's
//! comments, key order and spacing survive a merge into a file they wrote by
//! hand. The consequence for a manifest author is that the *shape* has to match
//! — a target spelling kimi's hooks as an inline `hooks = [{…}]` array where the
//! payload ships `[[hooks]]` tables is a type conflict, so it is left untouched
//! and the agent goes unreported rather than having its config rewritten into
//! the other shape. An inline array also has no comment to own, which is the
//! second reason the payload ships `[[hooks]]` tables.

use toml_edit::{DocumentMut, Item, Table, Value};

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

/// Remove every `[[array of tables]]` entry **thurbox owns** — one whose own
/// comment carries `marker` — anywhere in `doc`, then drop a key whose array
/// *we* emptied.
///
/// Ownership is the entry's comment, never its content: a user hook that calls
/// `thurbox-cli session signal` itself is not ours and survives, and an entry of
/// ours whose event or command changed in a later payload is still ours and is
/// replaced. See the module docs for why the JSON sibling cannot do this.
///
/// Reverses a [`merge`] on uninstall, and pairs with it on install
/// (prune, then merge) so an update replaces rather than accumulates.
pub fn prune_owned(doc: &mut DocumentMut, marker: &str) {
    prune_item(doc.as_item_mut(), marker);
}

/// Whether `table` is an entry thurbox wrote: its own comment carries `marker`.
///
/// The comment is the table's *prefix decor*, which `toml_edit` preserves across
/// a render and re-parse. Deliberately not `Table::to_string`, which renders the
/// body without any decor at all.
fn is_owned(table: &Table, marker: &str) -> bool {
    table
        .decor()
        .prefix()
        .and_then(|d| d.as_str())
        .is_some_and(|prefix| prefix.contains(marker))
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
            entries.retain(|t| !is_owned(t, marker));
        }
        // An inline array carries no comment, so nothing in one can be proven
        // ours. Left alone rather than guessed at from its content.
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

    /// The signal command every shipped hook carries — and, deliberately, the
    /// thing a user may put in a hook of their own.
    const SIGNAL: &str = "thurbox-cli session signal";
    /// The ownership marker the payload stamps on each entry it ships.
    const OWNED: &str = "thurbox `extension install`";

    fn doc(text: &str) -> DocumentMut {
        text.parse().expect("valid TOML")
    }

    /// A payload entry shaped like the shipped one: an ownership comment above
    /// an otherwise ordinary `[[hooks]]` table.
    fn ours(event: &str, state: &str, timeout: u32) -> DocumentMut {
        doc(&format!(
            "# managed by {OWNED}\n[[hooks]]\nevent = \"{event}\"\n\
             command = \"{SIGNAL} --state {state} || true\"\ntimeout = {timeout}\n"
        ))
    }

    fn hooks_len(text: &str) -> usize {
        doc(text)["hooks"].as_array_of_tables().unwrap().len()
    }

    #[test]
    fn merge_into_empty_takes_source() {
        let mut target = doc("");
        merge(&mut target, &ours("Stop", "done", 10));
        assert!(target.to_string().contains("--state done"));
    }

    #[test]
    fn merge_preserves_user_entries_comments_and_unrelated_keys() {
        let mut target = doc("# my config\nmodel = \"kimi-for-coding\"\n\n\
             [[hooks]]\nevent = \"Stop\"\ncommand = \"notify-send done\"\n");
        merge(&mut target, &ours("Stop", "done", 10));
        let out = target.to_string();
        assert!(out.contains("# my config"), "comment survived: {out}");
        assert!(out.contains("model = \"kimi-for-coding\""));
        assert!(out.contains("notify-send done"), "user hook survived");
        assert!(out.contains("--state done"), "ours appended");
        assert_eq!(hooks_len(&out), 2);
    }

    #[test]
    fn merge_is_idempotent() {
        let mut target = doc("");
        merge(&mut target, &ours("Stop", "done", 10));
        let once = target.to_string();
        merge(&mut target, &ours("Stop", "done", 10));
        assert_eq!(target.to_string(), once);
    }

    #[test]
    fn a_type_conflict_leaves_the_users_value_alone() {
        // The user spells hooks as an inline array of inline tables; we ship
        // `[[hooks]]`. Rewriting their file into the other shape is not ours to
        // do, so nothing is merged.
        let mut target = doc("hooks = [{ event = \"Stop\", command = \"mine\" }]\n");
        merge(&mut target, &ours("Stop", "done", 10));
        assert!(!target.to_string().contains("--state done"));
        assert!(target.to_string().contains("mine"));
    }

    /// The destructive case ownership-by-comment exists to prevent: a user who
    /// wired their *own* hook to `thurbox-cli session signal` — which
    /// `extensions/hooks/README.md` tells them to do for an agent thurbox does
    /// not instrument — must not have it deleted when thurbox uninstalls.
    #[test]
    fn uninstall_keeps_a_user_hook_that_calls_the_signal_command_itself() {
        let users_own = format!(
            "model = \"kimi-for-coding\"\n\n\
             [[hooks]]\nevent = \"Stop\"\ncommand = \"{SIGNAL} --state done || true\"\n"
        );
        let mut target = doc(&users_own);
        merge(&mut target, &ours("Stop", "done", 10));
        assert_eq!(hooks_len(&target.to_string()), 2, "ours sits beside theirs");

        prune_owned(&mut target, OWNED);
        let out = target.to_string();
        assert_eq!(hooks_len(&out), 1, "exactly one entry removed: {out}");
        assert!(
            out.contains(SIGNAL),
            "the user's own signal hook survived: {out}"
        );
        // Nothing of ours is left, and their file is back as it was.
        assert!(!out.contains(OWNED));
        assert_eq!(out, users_own);
    }

    /// The other half of the same rule: our entry stays ours after its content
    /// changes, so an update replaces it instead of stacking a second copy.
    #[test]
    fn a_changed_payload_replaces_our_previous_entry_instead_of_accumulating() {
        let mut target = doc("[[hooks]]\nevent = \"Stop\"\ncommand = \"mine\"\n");
        merge(&mut target, &ours("Stop", "done", 10));

        // A later payload renames the event and changes the timeout — nothing
        // of its content matches the entry already on disk.
        let installed = target.to_string();
        let mut updated = doc(&installed);
        prune_owned(&mut updated, OWNED);
        merge(&mut updated, &ours("SessionEnd", "idle", 30));

        let out = updated.to_string();
        assert_eq!(hooks_len(&out), 2, "the user's plus exactly one of ours");
        assert!(!out.contains("--state done"), "stale entry gone: {out}");
        assert!(!out.contains("timeout = 10"));
        assert!(out.contains("--state idle") && out.contains("timeout = 30"));
        assert!(out.contains("command = \"mine\""), "user hook untouched");

        // And re-applying the same payload changes nothing (no churn on the
        // startup/heartbeat re-install).
        let mut again = doc(&out);
        prune_owned(&mut again, OWNED);
        merge(&mut again, &ours("SessionEnd", "idle", 30));
        assert_eq!(again.to_string(), out);
    }

    #[test]
    fn prune_drops_a_key_it_emptied_but_not_one_the_user_left_empty() {
        let mut target = doc("");
        merge(&mut target, &ours("Stop", "done", 10));
        prune_owned(&mut target, OWNED);
        assert!(!target.to_string().contains("hooks"));

        let mut users_empty = doc("hooks = []\n");
        prune_owned(&mut users_empty, OWNED);
        assert!(users_empty.to_string().contains("hooks = []"));
    }

    #[test]
    fn prune_is_a_no_op_for_content_we_do_not_own() {
        let mut target = doc("[[hooks]]\nevent = \"Stop\"\ncommand = \"mine\"\n");
        let before = target.to_string();
        prune_owned(&mut target, OWNED);
        assert_eq!(target.to_string(), before);
    }
}
