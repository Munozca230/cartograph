//! JSON with comments (tsconfig.json / jsconfig.json): `//` and `/* */` comments, trailing
//! commas and a leading BOM are accepted, as TypeScript itself accepts them.

use serde_json::Value;

/// Parse JSONC text.
pub fn parse(text: &str) -> Result<Value, String> {
    serde_json::from_str(&strip(text)).map_err(|e| e.to_string())
}

/// Remove comments, trailing commas and a BOM, keeping strings intact. Comments are
/// replaced by spaces (newlines kept) so error positions stay meaningful.
pub fn strip(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let no_comments = strip_comments(text);
    strip_trailing_commas(&no_comments)
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => {
                    if let Some(n) = chars.next() {
                        out.push(n);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    out.push(if n == '\n' { '\n' } else { ' ' });
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn strip_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(&n) = chars.get(i + 1) {
                    out.push(n);
                    i += 1;
                }
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if c == ',' {
            let next = chars[i + 1..].iter().find(|n| !n.is_whitespace());
            if !matches!(next, Some('}') | Some(']')) {
                out.push(c);
            }
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn comments_trailing_commas_and_bom() {
        let text = "\u{feff}{\n  // line\n  \"a\": \"http://x/*y*/\", /* block\n */\n  \"b\": [1, 2,],\n  \"c\": {\"d\": \"\\\"//\",},\n}\n";
        assert_eq!(
            parse(text).unwrap(),
            json!({"a": "http://x/*y*/", "b": [1, 2], "c": {"d": "\"//"}})
        );
        assert!(parse("{").is_err());
    }
}
