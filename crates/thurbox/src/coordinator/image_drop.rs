//! Recognising a drop of local image files on a terminal.
//!
//! A terminal delivers a dropped file as a paste of its path. On this machine
//! that path is what the agent needs; on another machine it names nothing, so
//! a drop on a remote session is refused rather than typed (ADR-41: neither the
//! runtime protocol nor Fleet's command line yet carries a file to a Task's
//! host).

use std::path::PathBuf;

/// The extensions a drop counts as an image by.
const IMAGE_EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "heic"];

/// The image files `text` drops, when it is nothing but absolute paths with an
/// image extension, as a terminal pastes a drop; `None` for any other text.
/// Decided from the text alone: this runs on the paste, on the loop, and a
/// filesystem check could stall it on a mount that does not answer.
pub fn dropped_images(text: &str) -> Option<Vec<PathBuf>> {
    let words = split_words(text.trim_matches(|c: char| c.is_ascii_whitespace()))?;
    if words.is_empty() {
        return None;
    }
    words
        .iter()
        .map(|word| {
            let path = PathBuf::from(file_uri_path(word)?);
            let image = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()));
            (image && path.is_absolute()).then_some(path)
        })
        .collect()
}
/// Split like a shell would for the three spellings a terminal uses: single
/// quotes, double quotes and backslash escapes. Backslashes are path
/// separators on Windows, so they escape nothing there. `None` on an
/// unterminated quote.
///
/// Only ASCII whitespace separates words, as in a shell. A terminal escapes
/// the spaces it knows about and leaves Unicode ones bare: macOS names a
/// screenshot `… at 4.33.57\u{202F}PM.png`, and Ghostty drops it with that
/// narrow no-break space unescaped, so `char::is_whitespace` would split it.
fn split_words(text: &str) -> Option<Vec<String>> {
    let escapes = !cfg!(windows);
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') if escapes => current.push(chars.next()?),
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => {
                quote = Some(ch);
                in_word = true;
            }
            (None, '\\') if escapes => {
                current.push(chars.next()?);
                in_word = true;
            }
            (None, c) if c.is_ascii_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            (None, c) => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if in_word {
        words.push(current);
    }
    Some(words)
}

/// A `file://` URI's path, percent-decoded; any other word unchanged.
fn file_uri_path(word: &str) -> Option<String> {
    let Some(rest) = word.strip_prefix("file://") else {
        return Some(word.to_string());
    };
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let mut path = String::from_utf8(out).ok()?;
    // A drive URI spells its path /C:/..., which Windows reads as rooted but
    // not absolute. Strip the URI separator only for a drive's absolute path.
    if cfg!(windows)
        && path.as_bytes().get(..4).is_some_and(|prefix| {
            prefix[0] == b'/'
                && prefix[1].is_ascii_alphabetic()
                && prefix[2] == b':'
                && prefix[3] == b'/'
        })
    {
        path.remove(0);
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::dropped_images;

    /// The decision is made from the text alone: a path under a directory
    /// that does not exist is still a drop, so a stalled mount cannot hold
    /// the paste that asks.
    #[test]
    fn a_drop_is_known_by_its_shape_without_touching_the_filesystem() {
        // An absolute path is this platform's: a drive path on Windows,
        // where a terminal's drop always names the drive.
        let (gone, two) = if cfg!(windows) {
            (
                r"C:\nonexistent-thurbox-dir\sub\shot.png",
                r"'C:\no where\a b.PNG' file:///C:/nope/c%20d.jpg",
            )
        } else {
            (
                "/nonexistent-thurbox-dir/sub/shot.png",
                "'/no where/a b.PNG' file:///nope/c%20d.jpg",
            )
        };
        assert_eq!(dropped_images(gone).map(|p| p.len()), Some(1));
        assert_eq!(dropped_images(two).map(|p| p.len()), Some(2));
        for text in ["shot.png", "/tmp/notes.txt", "hello /tmp/a.png", ""] {
            assert!(dropped_images(text).is_none(), "{text:?}");
        }
        // A rooted path with no drive is not absolute on Windows, and no
        // drop there spells one: it is text, typed as is.
        if cfg!(windows) {
            assert!(dropped_images("/rooted/no-drive.png").is_none());
        }
    }
}
