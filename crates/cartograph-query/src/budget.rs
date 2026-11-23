//! Token budgets. A token is estimated as 4 characters. Every query fits its text answer
//! (and its JSON) inside its budget by shrinking the number of items it lists; what is
//! left out is always counted ("+N more").

use serde::Serialize;

/// Characters per estimated token.
pub const CHARS_PER_TOKEN: usize = 4;

/// Budgets per query, in estimated tokens (each tool's documented contract).
pub mod limits {
    pub const OVERVIEW: usize = 1500;
    pub const EXPLAIN: usize = 1000;
    pub const DEPS: usize = 1500;
    pub const IMPACT: usize = 1500;
    pub const FIND: usize = 800;
    /// `find` of a string key: its trace (definitions, runners, catalogs, missing, uses).
    pub const TRACE: usize = 1500;
    pub const PATH: usize = 800;
    /// `path` to a client whose calls cross a bridge, continued past it down to the data
    /// (see `crate::slice`).
    pub const SLICE: usize = 1500;
    pub const FINDINGS: usize = 1500;
    pub const CHECK: usize = 1000;
    pub const SYMBOLS: usize = 1000;
    pub const CALLERS: usize = 1000;
    pub const ENTRIES: usize = 1000;
    pub const HOTSPOTS: usize = 1000;
    pub const DIFF: usize = 1500;
    pub const HIERARCHY: usize = 800;
    pub const BRIDGES: usize = 1000;
}

/// Estimated tokens of a text.
pub fn tokens(s: &str) -> usize {
    s.chars().count().div_ceil(CHARS_PER_TOKEN)
}

/// Estimated tokens of a value as compact JSON.
pub fn json_tokens<T: Serialize>(v: &T) -> usize {
    serde_json::to_string(v).map_or(0, |s| tokens(&s))
}

/// Cut `s` to at most `max` characters (on a char boundary), marking the cut with `…`.
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

/// A text answer and its data.
#[derive(Debug, Clone)]
pub struct Answer<T> {
    pub data: T,
    pub text: String,
}

/// How much larger than the text budget the JSON of an answer may be. The text is what an
/// agent reads; the JSON (`--structured`) repeats every id and path in full and costs about
/// twice the tokens, so holding it to the text's budget made the JSON, not the text, decide
/// how many items an answer lists.
pub const JSON_BUDGET_FACTOR: usize = 2;

/// Build with the largest item limit of `steps` (descending) whose text fits in `budget`
/// tokens and whose JSON fits in `budget` × [`JSON_BUDGET_FACTOR`]. If even the smallest
/// does not fit, the text is cut line by line with an explicit "+N more lines" marker (the
/// JSON keeps the smallest build).
pub fn fit<T, B, R>(budget: usize, steps: &[usize], build: B, render: R) -> Answer<T>
where
    T: Serialize,
    B: Fn(usize) -> T,
    R: Fn(&T) -> String,
{
    let mut last = None;
    for &limit in steps {
        let data = build(limit);
        let text = render(&data);
        if tokens(&text) <= budget && json_tokens(&data) <= budget * JSON_BUDGET_FACTOR {
            return Answer { data, text };
        }
        last = Some((data, text));
    }
    let (data, text) = match last {
        Some(x) => x,
        None => {
            let data = build(0);
            let text = render(&data);
            (data, text)
        }
    };
    Answer {
        text: cut_lines(&text, budget),
        data,
    }
}

/// Keep whole lines while they fit in `budget` tokens, then say how many were left out.
pub fn cut_lines(text: &str, budget: usize) -> String {
    if tokens(text) <= budget {
        return text.to_string();
    }
    let max_chars = budget * CHARS_PER_TOKEN;
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::new();
    let mut used = 0;
    for (i, line) in lines.iter().enumerate() {
        let len = line.chars().count() + 1;
        // Leave room for the marker line.
        if used + len + 32 > max_chars {
            out.push_str(&format!("… +{} more lines (truncated)\n", lines.len() - i));
            return out;
        }
        out.push_str(line);
        out.push('\n');
        used += len;
    }
    out
}

/// " (+N more)" when `n > 0`.
pub fn more(n: usize) -> String {
    if n > 0 {
        format!(" (+{n} more)")
    } else {
        String::new()
    }
}

/// Item limits tried by [`fit`], largest first.
pub const STEPS: &[usize] = &[40, 30, 24, 18, 14, 10, 8, 6, 5, 4, 3, 2, 1];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_and_clips_on_char_boundaries() {
        assert_eq!(tokens(""), 0);
        assert_eq!(tokens("abcd"), 1);
        assert_eq!(tokens("abcde"), 2);
        assert_eq!(clip("ñandú-ñandú", 5), "ñand…");
        assert_eq!(clip("abc", 5), "abc");
    }

    #[test]
    fn fit_shrinks_until_it_fits() {
        let a = fit(
            10,
            &[100, 10, 2],
            |n| (0..n).collect::<Vec<usize>>(),
            |v| format!("{v:?}"),
        );
        assert!(tokens(&a.text) <= 10);
        assert_eq!(a.data, (0..10).collect::<Vec<usize>>());
    }

    #[test]
    fn cut_lines_marks_the_cut() {
        let text = "line\n".repeat(200);
        let cut = cut_lines(&text, 20);
        assert!(tokens(&cut) <= 20, "{cut}");
        assert!(cut.contains("more lines"));
    }
}
