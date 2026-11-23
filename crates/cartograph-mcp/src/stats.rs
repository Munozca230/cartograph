//! Usage stats: one JSON line per MCP tool call in `<data dir>/stats.jsonl`, summarized by
//! `cartograph stats`.
//!
//! Privacy: a record holds only values from closed sets (the tool name, the error kind) and
//! numbers: never arguments, paths, messages or answer text, so the
//! file can be shared. The session id is random per process.
//!
//! Size: when the file reaches [`MAX_BYTES`] it becomes `stats.1.jsonl` (replacing the
//! previous one) and a new file starts, so the log holds the last ~10-25k records in at
//! most 2 × [`MAX_BYTES`].
//!
//! Several processes may append to the same file (MCP servers on one repo): each record is
//! one `write_all` of a whole line in append mode. A failed write or rotation is logged at
//! debug level and never fails or delays the call; the reader skips lines it cannot parse.

use cartograph_query::budget;
use rmcp::model::{CallToolResponse, CallToolResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The current log.
pub const FILE: &str = "stats.jsonl";
/// The previous log, after a rotation.
pub const OLD_FILE: &str = "stats.1.jsonl";
/// Taken (exclusively) by every process for each append, so a rotation never races
/// another process's append or rotation.
pub const LOCK_FILE: &str = "stats.lock";
/// Size at which the log rotates.
pub const MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Record format version.
pub const VERSION: u8 = 1;
/// The tool name recorded for a call to a tool the server does not have (the name the
/// client sent is not recorded).
pub const UNKNOWN_TOOL: &str = "unknown";

/// Who wrote a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Mcp,
    /// Any other writer (records of older versions): read, but left out of the summary.
    #[serde(other)]
    Other,
}

/// How a call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// An answer.
    Ok,
    /// A tool error (`isError`) or a protocol error.
    Error,
    /// No index yet (or the index failed): "indexing N %".
    NotReady,
}

/// One tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub v: u8,
    /// Unix seconds.
    pub ts: u64,
    pub src: Source,
    /// Random per process (8 hex digits): counts sessions, identifies nothing.
    pub sid: String,
    /// Tool name, or `unknown` for a tool the server does not have.
    pub tool: String,
    pub ms: u64,
    /// Characters of the answer text the client receives.
    pub chars: u64,
    /// Estimated tokens of that text (chars / 4, as the budgets count them).
    pub tokens: u64,
    /// The answer left items out ("+N more", "(truncated)").
    pub truncated: bool,
    pub status: Status,
    /// Error kind: `unknown_node`, `bad_argument`, `ambiguous`, `bad_params`, `internal`,
    /// `timeout`, `unknown_tool`, `protocol`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Appends records to `<data dir>/stats.jsonl`.
#[derive(Debug)]
pub struct Recorder {
    dir: PathBuf,
    sid: String,
    lock: Mutex<()>,
}

impl Recorder {
    pub fn new(data_dir: &Path) -> Recorder {
        Recorder {
            dir: data_dir.to_path_buf(),
            sid: session_id(),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(FILE)
    }

    /// A record for this process, stamped now.
    pub fn record(&self, src: Source, tool: &str, took: Duration) -> Record {
        Record {
            v: VERSION,
            ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            src,
            sid: self.sid.clone(),
            tool: tool.to_string(),
            ms: took.as_millis().min(u128::from(u64::MAX)) as u64,
            chars: 0,
            tokens: 0,
            truncated: false,
            status: Status::Ok,
            error: None,
        }
    }

    /// Record one MCP tool call from its response. `tool` must already be a known tool name
    /// (or `unknown`): it is written as is.
    pub fn tool_call(
        &self,
        tool: &str,
        took: Duration,
        res: &Result<CallToolResponse, rmcp::ErrorData>,
    ) {
        let mut r = self.record(Source::Mcp, tool, took);
        match res {
            Ok(CallToolResponse::Complete(c)) => describe(c, &mut r),
            Ok(_) => {}
            // The message names the tool or an argument: only the kind is recorded.
            Err(_) => {
                r.status = Status::Error;
                let kind = if tool == UNKNOWN_TOOL {
                    "unknown_tool"
                } else {
                    "protocol"
                };
                r.error = Some(kind.to_string());
            }
        }
        self.append(&r);
    }

    /// Append one record (rotating first when the file is full). Never fails.
    ///
    /// Several processes may append (MCP servers on one repo): each append holds
    /// [`LOCK_FILE`], and the size is read under it, so two processes that both saw a full
    /// log rotate it once (the second sees the new, small one) and the rotated log is never
    /// replaced by a newer, near-empty one. Without the lock (it cannot be created or
    /// taken) the append still happens, and a rotation needs the size read right before it.
    pub fn append(&self, r: &Record) {
        let Ok(mut line) = serde_json::to_string(r) else {
            return;
        };
        line.push('\n');
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let _file_lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.dir.join(LOCK_FILE))
            .and_then(|f| f.lock().map(|()| f))
            .map_err(|e| tracing::debug!(error = %e, "stats lock not taken"))
            .ok();
        let path = self.path();
        if std::fs::metadata(&path).is_ok_and(|m| m.len() >= MAX_BYTES)
            && let Err(e) = {
                #[cfg(test)]
                std::thread::sleep(Duration::from_millis(
                    tests::ROTATE_PAUSE_MS.load(std::sync::atomic::Ordering::Relaxed),
                ));
                std::fs::rename(&path, self.dir.join(OLD_FILE))
            }
        {
            // A reader may hold it open (Windows).
            tracing::debug!(error = %e, "stats rotation skipped");
        }
        let res = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
        if let Err(e) = res {
            tracing::debug!(error = %e, "stats record not written");
        }
    }
}

/// Fill size, truncation, status and error kind from a tool result.
fn describe(c: &CallToolResult, r: &mut Record) {
    let text = result_text(c);
    r.chars = text.chars().count() as u64;
    r.tokens = budget::tokens(&text) as u64;
    r.truncated = is_truncated(&text);
    let structured = c.structured_content.as_ref();
    if c.is_error == Some(true) {
        r.status = Status::Error;
        let code = structured
            .and_then(|s| s["error"]["code"].as_str())
            // Only rmcp's own argument errors come without our structured error.
            .unwrap_or("bad_params");
        r.error = Some(code.to_string());
    } else if structured.is_some_and(|s| s["status"] == "indexing") {
        r.status = Status::NotReady;
    }
}

/// The text blocks of a result, joined as the client shows them.
pub fn result_text(c: &CallToolResult) -> String {
    let mut out = String::new();
    for t in c.content.iter().filter_map(|b| b.as_text()) {
        out.push_str(&t.text);
    }
    out
}

/// Whether an answer says it left items out: "+N more" (every list of the query crate) or
/// "(truncated)" (a text cut to its budget line by line).
pub fn is_truncated(text: &str) -> bool {
    if text.contains("(truncated)") {
        return true;
    }
    let b = text.as_bytes();
    let mut i = 0;
    while let Some(p) = text[i..].find('+') {
        let start = i + p + 1;
        let mut j = start;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > start && text[j..].starts_with(" more") {
            return true;
        }
        i = start;
    }
    false
}

/// 8 hex digits from the process id and the clock.
fn session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut h = blake3::Hasher::new();
    h.update(&std::process::id().to_le_bytes());
    h.update(&nanos.to_le_bytes());
    h.finalize().to_hex()[..8].to_string()
}

/// Every record of the log (the rotated file first), skipping unreadable lines. Also the
/// number of lines skipped.
pub fn load(data_dir: &Path) -> (Vec<Record>, usize) {
    let mut out = Vec::new();
    let mut bad = 0;
    for name in [OLD_FILE, FILE] {
        let Ok(s) = std::fs::read_to_string(data_dir.join(name)) else {
            continue;
        };
        for line in s.lines().filter(|l| !l.trim().is_empty()) {
            match serde_json::from_str::<Record>(line) {
                Ok(r) => out.push(r),
                Err(_) => bad += 1,
            }
        }
    }
    (out, bad)
}

/// The summary of `cartograph stats`.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub records: usize,
    pub skipped_lines: usize,
    pub sessions: usize,
    pub first_ts: Option<u64>,
    pub last_ts: Option<u64>,
    pub calls: usize,
    pub tools: Vec<ToolStats>,
    /// Error kind → count, over every tool.
    pub errors: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolStats {
    pub tool: String,
    pub calls: usize,
    pub errors: usize,
    pub not_ready: usize,
    pub p50_ms: u64,
    pub p95_ms: u64,
    /// Over the answers (status ok).
    pub avg_tokens: u64,
    pub max_tokens: u64,
    /// Share of answers that left items out, 0-1.
    pub truncated_rate: f64,
    /// Answers above the tool's token budget (the text the client got, notes included).
    pub over_budget: usize,
    pub budget: Option<usize>,
}

/// The token budget of a tool (its documented contract), if it has one.
pub fn budget_of(tool: &str) -> Option<usize> {
    use budget::limits::*;
    Some(match tool {
        "overview" => OVERVIEW,
        "explain" => EXPLAIN,
        "deps" => DEPS,
        "impact" => IMPACT,
        "find" => FIND,
        "path" => PATH,
        "findings" => FINDINGS,
        "check" => CHECK,
        "symbols" => SYMBOLS,
        "callers" => CALLERS,
        "hierarchy" => HIERARCHY,
        "bridges" => BRIDGES,
        "entries" => ENTRIES,
        "hotspots" => HOTSPOTS,
        "diff" => DIFF,
        _ => return None,
    })
}

/// Nearest-rank percentile of sorted values.
fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

fn avg(v: &[u64]) -> u64 {
    if v.is_empty() {
        0
    } else {
        v.iter().sum::<u64>() / v.len() as u64
    }
}

/// Summarize `records` (only those at or after `since`, unix seconds, when given).
pub fn summarize(records: &[Record], skipped_lines: usize, since: Option<u64>) -> Summary {
    let recs: Vec<&Record> = records
        .iter()
        .filter(|r| since.is_none_or(|s| r.ts >= s))
        .collect();
    let mut sessions = std::collections::BTreeSet::new();
    let mut by_tool: BTreeMap<&str, Vec<&Record>> = BTreeMap::new();
    let mut errors = BTreeMap::new();
    for r in recs.iter().filter(|r| r.src == Source::Mcp) {
        sessions.insert(r.sid.as_str());
        by_tool.entry(&r.tool).or_default().push(r);
        if let Some(e) = &r.error {
            *errors.entry(e.clone()).or_insert(0) += 1;
        }
    }
    let mut tools: Vec<ToolStats> = by_tool
        .into_iter()
        .map(|(tool, rs)| {
            let mut ms: Vec<u64> = rs.iter().map(|r| r.ms).collect();
            ms.sort_unstable();
            let ok: Vec<&&Record> = rs.iter().filter(|r| r.status == Status::Ok).collect();
            let toks: Vec<u64> = ok.iter().map(|r| r.tokens).collect();
            let budget = budget_of(tool);
            ToolStats {
                tool: tool.to_string(),
                calls: rs.len(),
                errors: rs.iter().filter(|r| r.status == Status::Error).count(),
                not_ready: rs.iter().filter(|r| r.status == Status::NotReady).count(),
                p50_ms: percentile(&ms, 0.50),
                p95_ms: percentile(&ms, 0.95),
                avg_tokens: avg(&toks),
                max_tokens: toks.iter().copied().max().unwrap_or(0),
                truncated_rate: if ok.is_empty() {
                    0.0
                } else {
                    ok.iter().filter(|r| r.truncated).count() as f64 / ok.len() as f64
                },
                over_budget: budget.map_or(0, |b| toks.iter().filter(|&&t| t > b as u64).count()),
                budget,
            }
        })
        .collect();
    tools.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.tool.cmp(&b.tool)));
    Summary {
        records: recs.len(),
        skipped_lines,
        sessions: sessions.len(),
        first_ts: recs.iter().map(|r| r.ts).min(),
        last_ts: recs.iter().map(|r| r.ts).max(),
        calls: tools.iter().map(|t| t.calls).sum(),
        tools,
        errors,
    }
}

/// `YYYY-MM-DD` of unix seconds (UTC).
pub fn date(ts: u64) -> String {
    // Civil from days (Howard Hinnant's algorithm).
    let z = (ts / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The summary as text.
pub fn render(s: &Summary) -> String {
    let mut out = String::new();
    if s.records == 0 {
        out.push_str("no usage recorded yet (the MCP server writes stats.jsonl in the data dir)\n");
        return out;
    }
    let span = match (s.first_ts, s.last_ts) {
        (Some(a), Some(b)) => format!(", {} → {}", date(a), date(b)),
        _ => String::new(),
    };
    out.push_str(&format!(
        "{} tool calls in {} MCP sessions{span}\n",
        s.calls, s.sessions
    ));
    if !s.tools.is_empty() {
        out.push_str(&format!(
            "\n{:<10} {:>6} {:>6} {:>7} {:>7} {:>8} {:>8} {:>6} {:>6} {:>6}\n",
            "tool",
            "calls",
            "errors",
            "p50 ms",
            "p95 ms",
            "avg tok",
            "max tok",
            "budget",
            "over",
            "trunc"
        ));
        for t in &s.tools {
            let (budget, over) = match t.budget {
                Some(b) => (b.to_string(), t.over_budget.to_string()),
                None => ("-".to_string(), "-".to_string()),
            };
            out.push_str(&format!(
                "{:<10} {:>6} {:>6} {:>7} {:>7} {:>8} {:>8} {:>6} {:>6} {:>5.0}%\n",
                t.tool,
                t.calls,
                t.errors,
                t.p50_ms,
                t.p95_ms,
                t.avg_tokens,
                t.max_tokens,
                budget,
                over,
                t.truncated_rate * 100.0
            ));
        }
        out.push_str(
            "(tokens = chars/4 of the text sent; over = answers above the budget; trunc = \
             answers that left items out, \"+N more\")\n",
        );
        let not_ready: usize = s.tools.iter().map(|t| t.not_ready).sum();
        if not_ready > 0 {
            out.push_str(&format!(
                "{not_ready} call(s) answered \"indexing\" (no index yet)\n"
            ));
        }
    }
    if s.errors.is_empty() {
        out.push_str("\nerrors: none\n");
    } else {
        let list: Vec<String> = s.errors.iter().map(|(k, n)| format!("{k} {n}")).collect();
        out.push_str(&format!("\nerrors: {}\n", list.join(", ")));
    }
    if s.skipped_lines > 0 {
        out.push_str(&format!("{} unreadable line(s) skipped\n", s.skipped_lines));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pause between reading the log's size and rotating it, to make a race between
    /// processes likely in a test.
    pub(super) static ROTATE_PAUSE_MS: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);

    #[test]
    fn truncation_markers() {
        assert!(is_truncated("a\n  +12 more of 40 files\n"));
        assert!(is_truncated("x (+3 more)"));
        assert!(is_truncated("… +5 more lines (truncated)\n"));
        assert!(!is_truncated("a + b more"));
        assert!(!is_truncated("C++ more or less"));
        assert!(!is_truncated("nothing here"));
    }

    #[test]
    fn percentiles_and_dates() {
        let v: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&v, 0.5), 50);
        assert_eq!(percentile(&v, 0.95), 95);
        assert_eq!(percentile(&[7], 0.95), 7);
        assert_eq!(percentile(&[], 0.5), 0);
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_735_689_600), "2025-01-01");
        assert_eq!(date(1_772_323_200), "2026-03-01");
    }

    #[test]
    fn append_load_rotate_and_skip_bad_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = Recorder::new(tmp.path());
        for i in 0..5 {
            let mut r = rec.record(Source::Mcp, "impact", Duration::from_millis(i));
            r.tokens = 100;
            rec.append(&r);
        }
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(rec.path())
            .unwrap();
        f.write_all(b"{not json\n").unwrap();
        drop(f);
        let (rs, bad) = load(tmp.path());
        assert_eq!((rs.len(), bad), (5, 1));
        // Rotation: a full file moves to stats.1.jsonl and both are read.
        let big = std::fs::OpenOptions::new()
            .write(true)
            .open(rec.path())
            .unwrap();
        big.set_len(MAX_BYTES).unwrap();
        drop(big);
        rec.append(&rec.record(Source::Mcp, "impact", Duration::ZERO));
        assert!(tmp.path().join(OLD_FILE).is_file());
        let fresh = std::fs::read_to_string(rec.path()).unwrap();
        assert_eq!(fresh.lines().count(), 1);
    }

    /// Recorders of several processes (one each, so their in-process mutexes do not help)
    /// appending to a full log: it rotates once, and no record is lost.
    #[test]
    fn concurrent_rotation_keeps_every_record() {
        let tmp = tempfile::tempdir().unwrap();
        let first = Recorder::new(tmp.path());
        let mut line =
            serde_json::to_string(&first.record(Source::Mcp, "x", Duration::ZERO)).unwrap();
        line.push('\n');
        let before = (MAX_BYTES as usize).div_ceil(line.len());
        std::fs::write(first.path(), line.repeat(before)).unwrap();
        let (writers, each) = (8, 40);
        // Every writer sees the full log before any rotates it, unless the lock orders them.
        ROTATE_PAUSE_MS.store(50, std::sync::atomic::Ordering::Relaxed);
        std::thread::scope(|s| {
            for _ in 0..writers {
                s.spawn(|| {
                    let rec = Recorder::new(tmp.path());
                    for _ in 0..each {
                        rec.append(&rec.record(Source::Mcp, "callers", Duration::ZERO));
                    }
                });
            }
        });
        let lines = |f: &str| {
            std::fs::read_to_string(tmp.path().join(f))
                .unwrap_or_default()
                .lines()
                .count()
        };
        assert_eq!(lines(FILE) + lines(OLD_FILE), before + writers * each);
        assert!(lines(OLD_FILE) >= before);
    }

    #[test]
    fn summary_counts() {
        let rec = Recorder::new(Path::new("."));
        let mut rs = Vec::new();
        for ms in [1, 2, 3, 40] {
            let mut r = rec.record(Source::Mcp, "callers", Duration::from_millis(ms));
            r.tokens = 2000;
            r.truncated = ms == 40;
            rs.push(r);
        }
        let mut e = rec.record(Source::Mcp, "explain", Duration::from_millis(1));
        e.status = Status::Error;
        e.error = Some("unknown_node".into());
        rs.push(e);
        // A record of another writer (an older version) is read but not summarized.
        rs.push(Record {
            src: Source::Other,
            ..rec.record(Source::Mcp, "session-start", Duration::from_millis(60))
        });
        let s = summarize(&rs, 0, None);
        assert_eq!(s.calls, 5);
        assert_eq!(s.sessions, 1);
        let c = &s.tools[0];
        assert_eq!(
            (c.tool.as_str(), c.calls, c.p50_ms, c.p95_ms),
            ("callers", 4, 2, 40)
        );
        assert_eq!(c.over_budget, 4);
        assert!((c.truncated_rate - 0.25).abs() < 1e-9);
        assert_eq!(s.errors["unknown_node"], 1);
        assert!(s.tools.iter().all(|t| t.tool != "session-start"));
        let old = r#"{"v":1,"ts":1,"src":"hook","sid":"0","tool":"x","ms":1,"chars":0,"tokens":0,"truncated":false,"status":"ok","outcome":"map"}"#;
        assert_eq!(
            serde_json::from_str::<Record>(old).unwrap().src,
            Source::Other
        );
        let text = render(&s);
        assert!(text.contains("callers"), "{text}");
        assert!(text.contains("unknown_node 1"), "{text}");
    }
}
