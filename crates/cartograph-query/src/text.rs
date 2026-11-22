//! Small text helpers shared by the renderers.

/// `820`, `12.4k`, `1.3M`.
pub fn num(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 10_000 {
        format!("{:.0}k", n as f64 / 1e3)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

/// `1 file`, `3 files`.
pub fn files(n: usize) -> String {
    if n == 1 {
        "1 file".to_string()
    } else {
        format!("{n} files")
    }
}

/// Display form of an id (`dir:src/ui` → `src/ui/`).
pub(crate) fn short(id: &str) -> String {
    if id == "repo:" {
        "./".to_string()
    } else if let Some(p) = id.strip_prefix("dir:") {
        format!("{p}/")
    } else if let Some(p) = id.strip_prefix("file:") {
        p.to_string()
    } else if let Some(p) = id.strip_prefix("external:") {
        p.to_string()
    } else {
        id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(num(820), "820");
        assert_eq!(num(1_234), "1.2k");
        assert_eq!(num(12_400), "12k");
        assert_eq!(num(1_300_000), "1.3M");
    }
}
