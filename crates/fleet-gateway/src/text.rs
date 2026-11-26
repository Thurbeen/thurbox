pub fn split(text: &str, limit: usize) -> Vec<String> {
    const FENCE: &str = "```";
    if text.chars().count() <= limit {
        return vec![text.to_string()];
    }
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    let mut in_fence = false;
    let mut reopened = false;
    let mut flush = |current: &mut String, current_len: &mut usize, in_fence: bool| -> bool {
        if current.is_empty() {
            return false;
        }
        let mut chunk = std::mem::take(current);
        chunk.truncate(chunk.trim_end_matches('\n').len());
        if in_fence {
            chunk.push('\n');
            chunk.push_str(FENCE);
        }
        chunks.push(chunk);
        *current_len = 0;
        if in_fence {
            current.push_str(FENCE);
            current.push('\n');
            *current_len = FENCE.len() + 1;
        }
        in_fence
    };
    for line in text.split('\n') {
        let room = if in_fence {
            limit.saturating_sub(FENCE.len() + 1).max(1)
        } else {
            limit.max(1)
        };
        let piece_room = if in_fence {
            room.saturating_sub(FENCE.len() + 1).max(1)
        } else {
            room
        };
        let count = line.chars().count();
        let pieces: Vec<String> = if count > room {
            let chars: Vec<char> = line.chars().collect();
            chars
                .chunks(piece_room)
                .map(|c| c.iter().collect())
                .collect()
        } else {
            vec![line.to_string()]
        };
        for piece in pieces {
            let piece_len = piece.chars().count();
            let separator = usize::from(!current.is_empty() && !reopened);
            if !current.is_empty() && current_len + separator + piece_len > room {
                reopened = flush(&mut current, &mut current_len, in_fence);
            }
            if !current.is_empty() && !reopened {
                current.push('\n');
                current_len += 1;
            }
            reopened = false;
            current.push_str(&piece);
            current_len += piece_len;
        }
        if line.trim_start().starts_with(FENCE) {
            in_fence = !in_fence;
        }
    }
    let _ = flush(&mut current, &mut current_len, false);
    chunks
}

#[cfg(test)]
mod tests {
    use super::split;

    #[test]
    fn a_long_line_in_a_fence_stays_within_the_limit() {
        let limit = 20;
        let text = format!("```\n{}\n```", "x".repeat(50));
        let chunks = split(&text, limit);
        assert!(chunks.len() > 1, "{chunks:?}");
        for chunk in &chunks {
            assert!(chunk.chars().count() <= limit, "{chunk:?} is over {limit}");
            assert!(
                chunk.starts_with("```") && chunk.ends_with("```"),
                "{chunk:?}"
            );
        }
        let body: String = chunks
            .iter()
            .flat_map(|c| c.lines())
            .filter(|l| !l.starts_with("```"))
            .collect();
        assert_eq!(body, "x".repeat(50));
    }
}
