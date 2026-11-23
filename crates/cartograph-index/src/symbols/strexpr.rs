//! String expressions ([`StrExpr`]): how a string argument or a constant is built, kept
//! as parts so a later pass can fill in what the parse could not see (a constant of
//! another file, a parameter a wrapper passes on).
//!
//! Parts come from literals, template literals, f-strings and `+`; a name or a dotted
//! name is a [`Part::Ref`] (resolved through the file's constants and imports), a
//! parameter of the function around the expression a [`Part::Param`]; anything else (a
//! call, a ternary, `encodeURIComponent(x)`) is a [`Part::Hole`].

use serde::{Deserialize, Serialize};

/// One part of a [`StrExpr`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Part {
    /// Literal text.
    #[serde(rename = "l")]
    Lit(String),
    /// Something that is not known before run time; the text as written (shortened).
    #[serde(rename = "h")]
    Hole(String),
    /// A name or dotted name (`IPC.USERS_GET`, `BASE`, `this.base`).
    #[serde(rename = "r")]
    Ref(String),
    /// The n-th parameter (0-based) of the function around the expression.
    #[serde(rename = "p")]
    Param(u8),
}

/// A string as an expression builds it (at most [`StrExpr::MAX_PARTS`] parts; adjacent
/// literals merged).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StrExpr(pub Vec<Part>);

/// Longest hole text kept.
const HOLE_TEXT: usize = 40;

impl StrExpr {
    pub const MAX_PARTS: usize = 16;

    pub fn lit(s: impl Into<String>) -> StrExpr {
        let mut e = StrExpr::default();
        e.push(Part::Lit(s.into()));
        e
    }

    pub fn hole(text: &str) -> StrExpr {
        let mut e = StrExpr::default();
        e.push(Part::Hole(short(text)));
        e
    }

    pub fn one(p: Part) -> StrExpr {
        let mut e = StrExpr::default();
        e.push(p);
        e
    }

    /// Append a part: literals merge; past the limit the rest is one hole.
    pub fn push(&mut self, p: Part) {
        if let (Part::Lit(s), Some(Part::Lit(last))) = (&p, self.0.last_mut()) {
            last.push_str(s);
            return;
        }
        if self.0.len() + 1 >= Self::MAX_PARTS {
            if !matches!(self.0.last(), Some(Part::Hole(_))) {
                self.0.push(Part::Hole("…".into()));
            }
            return;
        }
        self.0.push(p);
    }

    pub fn extend(&mut self, other: StrExpr) {
        for p in other.0 {
            self.push(p);
        }
    }

    /// The text, when every part is literal.
    pub fn literal(&self) -> Option<String> {
        match self.0.as_slice() {
            [] => Some(String::new()),
            [Part::Lit(s)] => Some(s.clone()),
            _ => None,
        }
    }

    /// Nothing but holes (no literal, name or parameter to work from).
    pub fn is_opaque(&self) -> bool {
        self.0.iter().all(|p| matches!(p, Part::Hole(_)))
    }

    /// A single name or parameter, or a literal: something worth recording as an argument.
    pub fn is_stringish(&self) -> bool {
        self.0.iter().any(|p| matches!(p, Part::Lit(_)))
            || matches!(self.0.as_slice(), [Part::Ref(r)] if looks_constant(r))
    }
}

/// `UPPER_CASE` or `Thing.UPPER_CASE`: a name that reads as a constant.
pub fn looks_constant(r: &str) -> bool {
    let last = r.rsplit('.').next().unwrap_or(r);
    !last.is_empty()
        && last.chars().any(|c| c.is_ascii_uppercase())
        && !last.chars().any(|c| c.is_ascii_lowercase())
}

fn short(t: &str) -> String {
    let t: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > HOLE_TEXT {
        let mut s: String = t.chars().take(HOLE_TEXT - 1).collect();
        s.push('…');
        s
    } else {
        t
    }
}

/// A module-level string constant: `const X = "a"`, `X.Y` of `const X = { Y: "a" }`, `E.A`
/// of a TS string enum, `C.X` of a Java `static final String`, a Python module constant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConstDef {
    /// Dotted name (`IPC.USERS_GET`, `Channels.SAVE`).
    #[serde(rename = "n")]
    pub name: String,
    #[serde(rename = "v")]
    pub value: StrExpr,
    #[serde(rename = "l")]
    pub line: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_merge_and_the_limit_ends_in_one_hole() {
        let mut e = StrExpr::lit("a");
        e.push(Part::Lit("b".into()));
        assert_eq!(e.literal().as_deref(), Some("ab"));
        for i in 0..40 {
            e.push(Part::Ref(format!("r{i}")));
        }
        assert_eq!(e.0.len(), StrExpr::MAX_PARTS);
        assert_eq!(e.0.last(), Some(&Part::Hole("…".into())));
        assert!(StrExpr::hole("f(x)").is_opaque());
        assert!(StrExpr::one(Part::Ref("IPC.USERS_GET".into())).is_stringish());
        assert!(!StrExpr::one(Part::Ref("channel".into())).is_stringish());
        assert_eq!(
            StrExpr::hole(&"x".repeat(100)).0[0],
            Part::Hole(format!("{}…", "x".repeat(39)))
        );
    }
}
