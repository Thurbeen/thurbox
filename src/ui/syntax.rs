//! Minimal, dependency-free syntax highlighting for the code-review diff.
//!
//! Not a full grammar engine — a fast, language-agnostic lexer that colours the
//! things that matter for skimming a diff: comments, strings, numbers,
//! keywords, and (capitalised) type names. Comment style + a shared keyword
//! union are picked from the file extension. Colours come from the active theme
//! so highlighting stays on-palette. Used by [`crate::ui::code_review`] for the
//! unified diff body (the add/remove signal stays on the gutter sign + row
//! tint, so this only colours the code text).

use ratatui::style::Color;

use crate::ui::theme::Theme;

/// Per-language lexing knobs (just the line-comment marker for now).
#[derive(Clone, Copy)]
pub(crate) struct Lang {
    line_comment: &'static str,
}

/// Pick a [`Lang`] from a file path's extension.
pub(crate) fn lang_for(path: &str) -> Lang {
    let ext = path.rsplit(['.', '/']).next().unwrap_or("");
    let line_comment = match ext {
        "py" | "rb" | "sh" | "bash" | "zsh" | "fish" | "toml" | "yaml" | "yml" | "ini" | "cfg"
        | "conf" | "nix" | "pl" | "r" | "ex" | "exs" | "rake" | "gemspec" => "#",
        "sql" | "lua" | "hs" | "elm" | "adb" | "ads" => "--",
        _ => "//",
    };
    Lang { line_comment }
}

/// A small union of keywords across common languages — good enough to make the
/// control-flow / declaration words pop without per-language grammars.
const KEYWORDS: &[&str] = &[
    "fn",
    "let",
    "mut",
    "pub",
    "use",
    "mod",
    "impl",
    "struct",
    "enum",
    "trait",
    "match",
    "if",
    "else",
    "for",
    "while",
    "loop",
    "return",
    "const",
    "static",
    "async",
    "await",
    "move",
    "ref",
    "where",
    "as",
    "dyn",
    "self",
    "Self",
    "crate",
    "super",
    "break",
    "continue",
    "in",
    "def",
    "class",
    "function",
    "var",
    "import",
    "from",
    "export",
    "public",
    "private",
    "protected",
    "void",
    "new",
    "delete",
    "try",
    "catch",
    "finally",
    "throw",
    "throws",
    "extends",
    "implements",
    "interface",
    "package",
    "func",
    "defer",
    "chan",
    "go",
    "range",
    "type",
    "nil",
    "null",
    "none",
    "true",
    "false",
    "and",
    "or",
    "not",
    "is",
    "lambda",
    "yield",
    "with",
    "pass",
    "elif",
    "then",
    "do",
    "begin",
    "module",
    "require",
    "local",
    "val",
    "when",
    "case",
    "switch",
    "default",
    "print",
    "echo",
    "end",
    "unless",
    "until",
];

/// Tokenise `text` into `(slice, colour)` runs for rendering. Adjacent runs of
/// the same colour are not merged (the caller renders each as a span).
pub(crate) fn highlight(text: &str, lang: &Lang) -> Vec<(String, Color)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<(String, Color)> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];

        // Line comment: the marker and everything after it.
        if starts_with_at(&chars, i, lang.line_comment) {
            out.push((chars[i..].iter().collect(), Theme::text_muted()));
            break;
        }

        // String literal (handles `\`-escapes); covers ", ', and backtick.
        if c == '"' || c == '\'' || c == '`' {
            let mut j = i + 1;
            while j < chars.len() {
                if chars[j] == '\\' {
                    j += 2;
                    continue;
                }
                if chars[j] == c {
                    j += 1;
                    break;
                }
                j += 1;
            }
            let end = j.min(chars.len());
            out.push((chars[i..end].iter().collect(), Theme::branch_name()));
            i = end;
            continue;
        }

        // Number (incl. hex `0x…`, floats, digit separators).
        if c.is_ascii_digit() {
            let mut j = i;
            while j < chars.len()
                && (chars[j].is_ascii_alphanumeric() || chars[j] == '.' || chars[j] == '_')
            {
                j += 1;
            }
            out.push((chars[i..j].iter().collect(), Theme::status_working()));
            i = j;
            continue;
        }

        // Identifier / keyword / type.
        if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word: String = chars[i..j].iter().collect();
            let color = if KEYWORDS.contains(&word.as_str()) {
                Theme::accent()
            } else if word.chars().next().is_some_and(char::is_uppercase) {
                Theme::accent_bright()
            } else {
                Theme::text_primary()
            };
            out.push((word, color));
            i = j;
            continue;
        }

        // A run of "plain" characters (operators, punctuation, whitespace) up to
        // the next token start.
        let mut j = i;
        while j < chars.len() {
            let cj = chars[j];
            if cj == '"'
                || cj == '\''
                || cj == '`'
                || cj.is_alphanumeric()
                || cj == '_'
                || starts_with_at(&chars, j, lang.line_comment)
            {
                break;
            }
            j += 1;
        }
        if j == i {
            j = i + 1;
        }
        out.push((chars[i..j].iter().collect(), Theme::text_primary()));
        i = j;
    }
    out
}

/// Whether `chars[i..]` starts with `pat`.
fn starts_with_at(chars: &[char], i: usize, pat: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    !p.is_empty() && i + p.len() <= chars.len() && chars[i..i + p.len()] == p[..]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(text: &str, lang: &Lang) -> Vec<String> {
        highlight(text, lang).into_iter().map(|(s, _)| s).collect()
    }

    #[test]
    fn lang_for_picks_comment_style() {
        assert_eq!(lang_for("src/x.rs").line_comment, "//");
        assert_eq!(lang_for("a/b/run.py").line_comment, "#");
        assert_eq!(lang_for("q.sql").line_comment, "--");
    }

    #[test]
    fn tokenises_keyword_string_number_comment() {
        let lang = lang_for("x.rs");
        // The whole comment (incl. the //) is one trailing run.
        let runs = highlight("let x = 42; // note", &lang);
        let joined: String = runs.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(joined, "let x = 42; // note");
        // `let` is a keyword run; `42` a number run; the comment is one run.
        assert!(texts("let x = 42; // note", &lang).contains(&"let".to_string()));
        assert!(texts("let x = 42; // note", &lang).contains(&"42".to_string()));
        assert!(runs.last().unwrap().0.starts_with("// note"));
    }

    #[test]
    fn string_swallows_comment_marker() {
        let lang = lang_for("x.js");
        // The `//` inside the string must not start a comment.
        let runs = highlight("let s = \"a//b\";", &lang);
        assert!(runs.iter().any(|(s, _)| s == "\"a//b\""));
    }

    #[test]
    fn round_trips_arbitrary_text() {
        let lang = lang_for("x.rs");
        for line in ["", "   ", "a+b", "fn main() {}", "#[derive(Debug)]"] {
            let joined: String = highlight(line, &lang).into_iter().map(|(s, _)| s).collect();
            assert_eq!(joined, line);
        }
    }
}
