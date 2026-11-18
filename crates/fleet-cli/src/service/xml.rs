pub fn escaped(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn unescaped(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

pub fn between(text: &str, open: &str, close: &str) -> Option<String> {
    let start = text.find(open)? + open.len();
    let len = text[start..].find(close)?;
    Some(unescaped(&text[start..start + len]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trips() {
        let text = r#"C:\a & b\<x> "y""#;
        assert_eq!(unescaped(&escaped(text)), text);
        assert!(!escaped(text).contains('<'));
        assert_eq!(
            between(&format!("<a>{}</a>", escaped(text)), "<a>", "</a>").as_deref(),
            Some(text)
        );
    }
}
