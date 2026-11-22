//! A small parser for Godot's text formats: scenes (`.tscn`), resources (`.tres`),
//! `project.godot` and `.import` files. They share one shape:
//!
//! ```text
//! ; comment
//! key = value                         ; properties before any section (project.godot)
//! [tag attr=value attr="value"]       ; a section header
//! key = value                         ; properties of that section
//! ```
//!
//! Values can be strings (`"…"`, with `\` escapes, spanning lines), numbers, constructors
//! (`ExtResource("1_abc")`, `Vector2(1, 2)`, `PackedStringArray("a", "b")`), arrays and
//! dictionaries, all possibly spanning lines. The parser keeps every value as raw text and
//! offers helpers to read strings and `ExtResource` ids out of it. It never fails: what it
//! cannot read is reported in [`Doc::errors`] and the rest is still parsed.

/// One `key = value` line (the value may span several lines).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prop {
    /// 1-based line of the key.
    pub line: u32,
    pub key: String,
    /// Raw value text, trimmed.
    pub value: String,
}

/// One `[tag attr=value …]` section with the properties that follow it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// 1-based line of the `[`.
    pub line: u32,
    pub tag: String,
    /// Raw attribute values, in order.
    pub attrs: Vec<(String, String)>,
    pub props: Vec<Prop>,
}

impl Section {
    /// Raw value of an attribute.
    pub fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// An attribute read as a string (quoted, or a bare token such as Godot 3's `id=1`).
    pub fn attr_str(&self, key: &str) -> Option<String> {
        self.attr(key)
            .map(|v| unquote(v).unwrap_or_else(|| v.to_string()))
    }

    /// Raw value of a property.
    pub fn prop(&self, key: &str) -> Option<&Prop> {
        self.props.iter().find(|p| p.key == key)
    }
}

/// A parsed file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Doc {
    /// Properties before the first section.
    pub preamble: Vec<Prop>,
    pub sections: Vec<Section>,
    /// `(line, message)` for every part that could not be read.
    pub errors: Vec<(u32, String)>,
}

struct Scanner<'a> {
    s: &'a [u8],
    pos: usize,
    line: u32,
}

impl Scanner<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.pos += 1;
        if c == b'\n' {
            self.line += 1;
        }
        Some(c)
    }

    fn skip_to_eol(&mut self) {
        while let Some(c) = self.peek() {
            if c == b'\n' {
                break;
            }
            self.bump();
        }
    }

    /// Skip a quoted string whose opening quote is at `pos`. `false` if it never closes.
    fn skip_string(&mut self) -> bool {
        self.bump();
        while let Some(c) = self.bump() {
            match c {
                b'\\' => {
                    self.bump();
                }
                b'"' => return true,
                _ => {}
            }
        }
        false
    }

    fn text(&self, from: usize, to: usize) -> String {
        String::from_utf8_lossy(self.s.get(from..to).unwrap_or(&[]))
            .trim()
            .to_string()
    }

    /// Scan a value. At bracket depth 0 it ends at a newline, and, inside a header
    /// (`in_header`), also at whitespace or the closing `]`. Returns the raw text and an
    /// error message if a string or bracket never closed.
    fn value(&mut self, in_header: bool) -> (String, Option<String>) {
        let start = self.pos;
        let start_line = self.line;
        let mut depth: u32 = 0;
        let mut error = None;
        while let Some(c) = self.peek() {
            match c {
                b'"' => {
                    if !self.skip_string() {
                        error = Some(format!("string opened on line {start_line} never closes"));
                        break;
                    }
                    continue;
                }
                b'(' | b'[' | b'{' => depth += 1,
                b']' if depth == 0 && in_header => break,
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                b'\n' if depth == 0 => break,
                b' ' | b'\t' | b'\r' if depth == 0 && in_header => break,
                _ => {}
            }
            self.bump();
        }
        if error.is_none() && depth > 0 {
            error = Some(format!("bracket opened on line {start_line} never closes"));
        }
        (self.text(start, self.pos), error)
    }
}

/// Parse a Godot text file.
pub fn parse(text: &str) -> Doc {
    let mut sc = Scanner {
        s: text.as_bytes(),
        pos: 0,
        line: 1,
    };
    let mut doc = Doc::default();
    loop {
        while sc.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            sc.bump();
        }
        let Some(c) = sc.peek() else { break };
        match c {
            b';' | b'#' => sc.skip_to_eol(),
            b'[' => {
                let line = sc.line;
                sc.bump();
                match header(&mut sc, line) {
                    Ok(section) => doc.sections.push(section),
                    Err(e) => {
                        doc.errors.push((line, e));
                        sc.skip_to_eol();
                    }
                }
            }
            _ => {
                let line = sc.line;
                let key_start = sc.pos;
                while sc.peek().is_some_and(|c| c != b'=' && c != b'\n') {
                    sc.bump();
                }
                if sc.peek() != Some(b'=') {
                    let text = sc.text(key_start, sc.pos);
                    doc.errors
                        .push((line, format!("line without '=': {}", clip(&text))));
                    continue;
                }
                let key = sc.text(key_start, sc.pos);
                let key = unquote(&key).unwrap_or(key);
                sc.bump();
                while sc.peek().is_some_and(|c| c == b' ' || c == b'\t') {
                    sc.bump();
                }
                let (value, err) = sc.value(false);
                if let Some(e) = err {
                    doc.errors.push((line, format!("{key}: {e}")));
                }
                let prop = Prop { line, key, value };
                match doc.sections.last_mut() {
                    Some(s) => s.props.push(prop),
                    None => doc.preamble.push(prop),
                }
            }
        }
    }
    doc
}

/// Parse a header after its `[`, up to and including the closing `]`.
fn header(sc: &mut Scanner<'_>, line: u32) -> Result<Section, String> {
    let tag_start = sc.pos;
    while sc
        .peek()
        .is_some_and(|c| !c.is_ascii_whitespace() && c != b']')
    {
        sc.bump();
    }
    let tag = sc.text(tag_start, sc.pos);
    let mut attrs = Vec::new();
    loop {
        while sc
            .peek()
            .is_some_and(|c| c == b' ' || c == b'\t' || c == b'\r')
        {
            sc.bump();
        }
        match sc.peek() {
            None | Some(b'\n') => return Err(format!("section [{tag} … never closes")),
            Some(b']') => {
                sc.bump();
                break;
            }
            _ => {}
        }
        let key_start = sc.pos;
        while sc
            .peek()
            .is_some_and(|c| c != b'=' && c != b']' && !c.is_ascii_whitespace())
        {
            sc.bump();
        }
        let key = sc.text(key_start, sc.pos);
        if sc.peek() != Some(b'=') {
            // A bare word: keep it as an attribute without value.
            if key.is_empty() {
                sc.bump();
            } else {
                attrs.push((key, String::new()));
            }
            continue;
        }
        sc.bump();
        let (value, err) = sc.value(true);
        if let Some(e) = err {
            return Err(format!("[{tag}] {key}: {e}"));
        }
        attrs.push((key, value));
    }
    if tag.is_empty() {
        return Err("empty section header".into());
    }
    Ok(Section {
        line,
        tag,
        attrs,
        props: Vec::new(),
    })
}

fn clip(s: &str) -> String {
    s.chars().take(crate::spec::MAX_SNIPPET_CHARS).collect()
}

/// The string a value holds when the whole value is one string literal: `"…"`, `&"…"`
/// (StringName) or `^"…"` (NodePath). Escapes are decoded.
pub fn unquote(value: &str) -> Option<String> {
    let v = value.trim();
    let v = v.strip_prefix(['&', '^']).unwrap_or(v);
    let body = v.strip_prefix('"')?.strip_suffix('"')?;
    if v.len() < 2 {
        return None;
    }
    Some(unescape(body))
}

/// Decode `\"`, `\\`, `\n`, `\t`, `\r` and `\uXXXX`; anything else keeps the escaped char.
pub fn unescape(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(ch) => out.push(ch),
                    None => {
                        out.push_str("\\u");
                        out.push_str(&hex);
                    }
                }
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Every string literal inside a raw value (decoded), in order.
pub fn strings_in(value: &str) -> Vec<String> {
    let b = value.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            let start = i + 1;
            let mut j = start;
            while j < b.len() && b[j] != b'"' {
                if b[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            if let Some(body) = value.get(start..j.min(b.len())) {
                out.push(unescape(body));
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// Ids of every `ExtResource(…)` in a raw value, outside string literals: `ExtResource("1_a")`
/// gives `1_a`, Godot 3's `ExtResource( 1 )` gives `1`.
pub fn ext_refs(value: &str) -> Vec<String> {
    const CALL: &str = "ExtResource(";
    let b = value.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if value.get(i..).is_some_and(|rest| rest.starts_with(CALL)) {
            let args_start = i + CALL.len();
            if let Some(close) = value.get(args_start..).and_then(|r| r.find(')')) {
                let arg = value
                    .get(args_start..args_start + close)
                    .unwrap_or("")
                    .trim();
                let id = unquote(arg).unwrap_or_else(|| arg.to_string());
                if !id.is_empty() {
                    out.push(id);
                }
                i = args_start + close + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_headers_props_and_values() {
        let doc = parse(
            "[gd_scene load_steps=3 format=3 uid=\"uid://c1\"]\r\n\r\n\
             [ext_resource type=\"Script\" path=\"res://a b].gd\" id=\"1_a\"]\r\n\
             [node name=\"Root\" type=\"Node2D\"]\r\n\
             script = ExtResource(\"1_a\")\r\n\
             text = \"line one\nline [two]\n\\\"q\\\"\"\r\n\
             points = PackedVector2Array(0, 0,\n 1, 1)\r\n\
             [node name=\"Kid\" parent=\".\" instance=ExtResource( 2 )]\n",
        );
        assert!(doc.errors.is_empty(), "{:?}", doc.errors);
        assert_eq!(doc.sections.len(), 4);
        let ext = &doc.sections[1];
        assert_eq!((ext.line, ext.tag.as_str()), (3, "ext_resource"));
        assert_eq!(ext.attr_str("path").as_deref(), Some("res://a b].gd"));
        assert_eq!(ext.attr_str("id").as_deref(), Some("1_a"));
        let root = &doc.sections[2];
        assert_eq!(root.props.len(), 3);
        assert_eq!(ext_refs(&root.prop("script").unwrap().value), vec!["1_a"]);
        assert_eq!(
            unquote(&root.prop("text").unwrap().value).as_deref(),
            Some("line one\nline [two]\n\"q\"")
        );
        assert_eq!(root.prop("points").unwrap().line, 9);
        let kid = &doc.sections[3];
        assert_eq!(kid.line, 11);
        assert_eq!(ext_refs(kid.attr("instance").unwrap()), vec!["2"]);
    }

    #[test]
    fn project_files_have_a_preamble_and_comments() {
        let doc = parse(
            "; Engine configuration file.\nconfig_version=5\n\n[autoload]\n\n\
             Events=\"*res://events.gd\"\n[editor_plugins]\n\
             enabled=PackedStringArray(\"res://addons/a/plugin.cfg\", \"res://addons/b/plugin.cfg\")\n",
        );
        assert!(doc.errors.is_empty());
        assert_eq!(doc.preamble[0].key, "config_version");
        let auto = &doc.sections[0];
        assert_eq!(
            unquote(&auto.prop("Events").unwrap().value).as_deref(),
            Some("*res://events.gd")
        );
        assert_eq!(
            strings_in(&doc.sections[1].props[0].value),
            vec!["res://addons/a/plugin.cfg", "res://addons/b/plugin.cfg"]
        );
    }

    #[test]
    fn broken_input_is_reported_not_fatal() {
        let doc = parse("[ext_resource path=\"res://a.gd\"\nkey = \"never closes\n");
        assert_eq!(doc.errors.len(), 2, "{:?}", doc.errors);
        assert_eq!(doc.errors[0].0, 1);
        let doc = parse("just words\n[node name=\"A\"]\nx = 1\n");
        assert_eq!(doc.errors.len(), 1);
        assert_eq!(doc.sections[0].props[0].value, "1");
    }

    #[test]
    fn ext_refs_skip_strings() {
        assert_eq!(
            ext_refs("[ExtResource(\"1\"), \"ExtResource(2)\", ExtResource( 3 )]"),
            vec!["1", "3"]
        );
    }
}
