/// Result of a successful fuzzy match.
pub(crate) struct FuzzyMatch {
    /// Byte positions of matched characters in the haystack.
    pub positions: Vec<usize>,
}

/// Greedy left-to-right fuzzy match (case-insensitive).
///
/// Returns `Some` if every character in `query` appears in `haystack` in order.
/// An empty query matches everything with an empty positions vec.
pub(crate) fn fuzzy_match(query: &str, haystack: &str) -> Option<FuzzyMatch> {
    if query.is_empty() {
        return Some(FuzzyMatch {
            positions: Vec::new(),
        });
    }

    let mut positions = Vec::with_capacity(query.len());
    let mut haystack_chars = haystack.char_indices().peekable();
    for qc in query.chars() {
        let qc_lower = qc.to_lowercase().next()?;
        loop {
            match haystack_chars.next() {
                Some((byte_pos, hc)) => {
                    if hc.to_lowercase().next() == Some(qc_lower) {
                        positions.push(byte_pos);
                        break;
                    }
                }
                None => return None,
            }
        }
    }

    Some(FuzzyMatch { positions })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_match() {
        let m = fuzzy_match("fb", "foo-bar").unwrap();
        assert_eq!(m.positions, vec![0, 4]);
    }

    #[test]
    fn no_match() {
        assert!(fuzzy_match("xyz", "foo-bar").is_none());
    }

    #[test]
    fn case_insensitive() {
        let m = fuzzy_match("FB", "Foo-Bar").unwrap();
        assert_eq!(m.positions, vec![0, 4]);
    }

    #[test]
    fn empty_query_matches_everything() {
        let m = fuzzy_match("", "anything").unwrap();
        assert!(m.positions.is_empty());
    }

    #[test]
    fn empty_haystack_no_match() {
        assert!(fuzzy_match("a", "").is_none());
    }

    #[test]
    fn exact_match() {
        let m = fuzzy_match("abc", "abc").unwrap();
        assert_eq!(m.positions, vec![0, 1, 2]);
    }

    #[test]
    fn partial_match_fails() {
        assert!(fuzzy_match("abz", "abc").is_none());
    }

    #[test]
    fn multibyte_characters() {
        let m = fuzzy_match("aé", "café").unwrap();
        // 'c'=0, 'a'=1, 'f'=2, 'é'=3 (byte pos 3, 2-byte char)
        assert_eq!(m.positions, vec![1, 3]);
    }
}
