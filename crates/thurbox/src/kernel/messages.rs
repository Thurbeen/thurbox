pub fn done(kind: &str, label: Option<&str>) -> Option<String> {
    let verb = match kind {
        "create" => "created",
        "delete" => "deleted",
        "send" => "sent to",
        "start" => "started",
        "pause" => "pausing",
        "restart" => "restarting",
        "bookmark" => "remembered",
        // Visible where it happened, or housekeeping nobody asked for.
        _ => return None,
    };
    Some(match label {
        Some(label) => format!("{verb} {label}"),
        None => verb.to_string(),
    })
}

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
            done("delete", Some("fix-osc52")).as_deref(),
            Some("deleted fix-osc52")
        );
        assert_eq!(
            done("send", Some("fix-osc52")).as_deref(),
            Some("sent to fix-osc52")
        );
        // A command with nothing to name still says what it did.
        assert_eq!(done("create", None).as_deref(), Some("created"));
    }

    #[test]
    fn a_failure_names_the_verb_and_the_subject() {
        let message = failed("delete", Some("fix-osc52"), "no window on that host");
        assert!(message.contains("delete"), "{message}");
        assert!(message.contains("fix-osc52"), "{message}");
        assert!(message.contains("no window on that host"), "{message}");
    }

    #[test]
    fn a_failure_with_nothing_to_name_still_says_what_failed() {
        let message = failed("rename", None, "not a git repository");
        assert!(message.contains("rename"), "{message}");
        assert!(message.contains("not a git repository"), "{message}");
    }
}
