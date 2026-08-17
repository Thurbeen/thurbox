//! What the message band says about a command that finished.
//!
//! Pure vocabulary, kept out of the loop for two reasons. It is the part worth
//! testing — which outcomes are announced and which are deliberately silent is a
//! judgement that can regress without anything failing to compile — and the loop
//! has no business holding a table of words.
//!
//! The rule the table encodes: **announce what the screen does not already
//! show.** A reorder moves the row you are looking at; a theme repaints; a
//! setting shows its new value. Announcing those trains the reader to ignore the
//! band, which is exactly what makes the messages that matter unreadable.

/// What a finished command says it did, or `None` to stay quiet.
///
/// Quiet is the right answer for anything whose result is *already on screen*:
/// a reorder moves the row you are looking at, a theme repaints, a setting shows
/// its new value. Announcing those is noise that trains the reader to ignore the
/// band — which is what makes the messages that matter unreadable.
///
/// Everything else changed something invisible (a worktree rebased, an agent
/// relaunched, a row revived) and is worth a line.
pub fn done(kind: &str, label: Option<&str>) -> Option<String> {
    let verb = match kind {
        "create" => "created",
        "delete" => "deleted",
        "restore" => "restored",
        "restart" => "restarted",
        "fork" => "forked",
        "sync" => "synced",
        "send" => "sent to",
        "task" => "saved task",
        "automation" => "saved automation",
        "bookmark" => "remembered",
        // Visible where it happened, or housekeeping nobody asked for.
        _ => return None,
    };
    Some(match label {
        Some(label) => format!("{verb} {label}"),
        None => verb.to_string(),
    })
}

/// What a failed command says went wrong.
///
/// Names the verb AND the subject: `restart` alone leaves the reader to work out
/// which of six sessions it was, and the error on its own ("Session not found")
/// reads like a bug in thurbox rather than something about their session.
pub fn failed(kind: &str, label: Option<&str>, error: &str) -> String {
    match label {
        Some(label) => format!("could not {kind} {label}: {error}"),
        None => format!("could not {kind}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outcome_reads_as_a_sentence_about_the_thing_it_happened_to() {
        assert_eq!(
            done("restart", Some("fix-osc52")).as_deref(),
            Some("restarted fix-osc52")
        );
        assert_eq!(
            done("sync", Some("fix-osc52")).as_deref(),
            Some("synced fix-osc52")
        );
        // A command with nothing to name still says what it did.
        assert_eq!(done("create", None).as_deref(), Some("created"));
    }

    #[test]
    fn a_failure_names_the_verb_and_the_subject() {
        let message = failed("restart", Some("fix-osc52"), "no window on that host");
        assert!(message.contains("restart"), "{message}");
        assert!(message.contains("fix-osc52"), "{message}");
        assert!(message.contains("no window on that host"), "{message}");
    }

    #[test]
    fn a_failure_with_nothing_to_name_still_says_what_failed() {
        let message = failed("sync", None, "not a git repository");
        assert!(message.contains("sync"), "{message}");
        assert!(message.contains("not a git repository"), "{message}");
    }
}
