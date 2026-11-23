//! `cartograph eval`: a deterministic eval suite of the MCP tools. Each case is a question
//! an agent would ask, the tool calls it would make, facts the answers must (or must not)
//! contain, and a token budget. The runner serves every repo of the suite with the real
//! `cartograph mcp` server over stdio (this same binary) and checks the text the agent
//! would read. No model is involved.
//!
//! Repos of a suite (`[repos.<name>]`, paths relative to the cases file, or absolute):
//! - `path`: a directory served in place, read-only (this repository's working tree, or
//!   any other repository on disk);
//! - `fixture`: a directory copied to a temp dir outside any git repository first;
//! - `commits` (+ `worktree`): a git repository built at run time, one commit per entry
//!   with fixed dates and people, then uncommitted files on top.
//!
//! The index of every repo goes to a temp data dir (under `--data-dir` when given), never
//! into the repo.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitCode, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    #[serde(default)]
    pub repos: BTreeMap<String, RepoSpec>,
    #[serde(default, rename = "case")]
    pub cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoSpec {
    /// Served in place.
    pub path: Option<PathBuf>,
    /// Copied to a temp dir first.
    pub fixture: Option<PathBuf>,
    /// A git repository built from these commits.
    #[serde(default)]
    pub commits: Vec<CommitSpec>,
    /// Uncommitted files written after the last commit (path → content).
    #[serde(default)]
    pub worktree: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitSpec {
    pub message: String,
    /// `git checkout -b <branch>` before this commit.
    pub branch: Option<String>,
    /// Files written (path → content).
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    /// Files deleted.
    #[serde(default)]
    pub remove: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub repo: String,
    /// What the agent wants to know (documentation; not sent).
    pub question: String,
    /// The one tool call of the case (or give `calls`).
    pub tool: Option<String>,
    /// The tool arguments, as the agent would send them.
    #[serde(default)]
    pub args: toml::Table,
    /// Several tool calls, made in order; the checks apply to all their answers together.
    #[serde(default)]
    pub calls: Vec<CallSpec>,
    /// Features the case depends on (free-form tags); the report counts passes per tag.
    #[serde(default)]
    pub needs: Vec<String>,
    /// Substrings the answer must contain.
    #[serde(default)]
    pub contains: Vec<String>,
    /// Substrings it must not contain.
    #[serde(default)]
    pub not_contains: Vec<String>,
    /// `contains` must appear in this order.
    #[serde(default)]
    pub in_order: bool,
    /// The call must be a tool error with this code (`unknown_node`, `ambiguous`, …).
    /// Only for a case with one call.
    pub error: Option<String>,
    /// Token budget of the answer (of all answers together); default: the tools' contracts.
    pub budget: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallSpec {
    pub tool: String,
    #[serde(default)]
    pub args: toml::Table,
    /// The repo this call goes to; default: the case's.
    pub repo: Option<String>,
}

impl Case {
    /// The calls of the case: `(repo, tool, args)`.
    fn calls(&self) -> Vec<(&str, &str, &toml::Table)> {
        match &self.tool {
            Some(t) => vec![(self.repo.as_str(), t.as_str(), &self.args)],
            None => self
                .calls
                .iter()
                .map(|c| {
                    (
                        c.repo.as_deref().unwrap_or(&self.repo),
                        c.tool.as_str(),
                        &c.args,
                    )
                })
                .collect(),
        }
    }

    fn validate(&self, repos: &BTreeMap<String, RepoSpec>) -> Result<()> {
        match (&self.tool, self.calls.is_empty()) {
            (Some(_), false) => bail!("case {}: give tool or calls, not both", self.id),
            (None, true) => bail!("case {}: no tool and no calls", self.id),
            (None, false) if !self.args.is_empty() => {
                bail!("case {}: args go inside each of calls", self.id)
            }
            (None, false) if self.error.is_some() => {
                bail!("case {}: error needs a single tool call", self.id)
            }
            _ => {}
        }
        for (repo, _, _) in self.calls() {
            if !repos.contains_key(repo) {
                bail!("case {}: unknown repo {repo:?}", self.id);
            }
        }
        if !repos.contains_key(&self.repo) {
            bail!("case {}: unknown repo {:?}", self.id, self.repo);
        }
        Ok(())
    }
}

/// The outcome of one case.
#[derive(Debug, Serialize)]
pub struct CaseResult {
    pub id: String,
    pub question: String,
    pub repo: String,
    /// The tools called, joined with `,`.
    pub tool: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    pub pass: bool,
    /// Number of tool calls.
    pub calls: usize,
    /// Tokens of all answers together.
    pub tokens: usize,
    pub budget: Option<usize>,
    pub ms: u64,
    pub failures: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

/// Passes among the cases carrying one `needs` tag (`-` for cases with none).
#[derive(Debug, Serialize)]
pub struct NeedSummary {
    pub need: String,
    pub passed: usize,
    pub total: usize,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub passed: usize,
    pub failed: usize,
    pub cases: Vec<CaseResult>,
    /// Empty when no case has `needs`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub by_need: Vec<NeedSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answers {
    None,
    Failing,
    All,
}

pub struct EvalOpts {
    pub cases: PathBuf,
    pub filter: Option<String>,
    pub json: bool,
    /// Print the answer of failing cases (or of every case).
    pub answers: Answers,
    /// Parent of the temp dir that holds the indexes (default: the system temp dir).
    pub data_dir: Option<PathBuf>,
}

pub fn cmd_eval(o: &EvalOpts) -> Result<ExitCode> {
    let text = std::fs::read_to_string(&o.cases)
        .with_context(|| format!("reading {}", o.cases.display()))?;
    let suite: Suite =
        toml::from_str(&text).with_context(|| format!("parsing {}", o.cases.display()))?;
    let base = o
        .cases
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let cases: Vec<&Case> = suite
        .cases
        .iter()
        .filter(|c| o.filter.as_deref().is_none_or(|f| c.id.contains(f)))
        .collect();
    if cases.is_empty() {
        bail!("no case matches");
    }
    for c in &cases {
        c.validate(&suite.repos)?;
    }
    let mut builder = tempfile::Builder::new();
    builder.prefix("cartograph-eval-");
    let tmp = match &o.data_dir {
        Some(d) => {
            std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
            builder.tempdir_in(d)
        }
        None => builder.tempdir(),
    }
    .context("creating a temp dir")?;
    let exe = std::env::current_exe().context("locating the cartograph binary")?;
    let mut servers = Servers {
        suite: &suite,
        base: &base,
        tmp: tmp.path(),
        exe: &exe,
        running: BTreeMap::new(),
    };
    let mut results = Vec::new();
    // Cases grouped by repo, in the order of first use; a server starts at its first call
    // and stays up until the end (a case may call several repos).
    let mut order: Vec<&str> = Vec::new();
    for c in &cases {
        if !order.contains(&c.repo.as_str()) {
            order.push(&c.repo);
        }
    }
    for name in order {
        for c in cases.iter().filter(|c| c.repo == name) {
            results.push(run_case(&mut servers, c, o.answers)?);
        }
    }
    for (_, s) in std::mem::take(&mut servers.running) {
        s.stop();
    }
    let passed = results.iter().filter(|r| r.pass).count();
    let report = Report {
        passed,
        failed: results.len() - passed,
        by_need: by_need(&results),
        cases: results,
    };
    let mut out = std::io::stdout().lock();
    if o.json {
        serde_json::to_writer_pretty(&mut out, &report)?;
        out.write_all(b"\n")?;
    } else {
        out.write_all(render(&report).as_bytes())?;
    }
    out.flush()?;
    Ok(if report.failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The MCP servers of a run, started on first use.
struct Servers<'a> {
    suite: &'a Suite,
    base: &'a Path,
    tmp: &'a Path,
    exe: &'a Path,
    running: BTreeMap<String, Server>,
}

impl Servers<'_> {
    fn get(&mut self, name: &str) -> Result<&mut Server> {
        if !self.running.contains_key(name) {
            let spec = &self.suite.repos[name];
            let root = prepare(name, spec, self.base, self.tmp)
                .with_context(|| format!("preparing repo {name}"))?;
            let data = self.tmp.join(format!("{name}.data"));
            let server = Server::start(self.exe, &root, &data)
                .with_context(|| format!("starting the MCP server on repo {name}"))?;
            self.running.insert(name.to_string(), server);
        }
        Ok(self.running.get_mut(name).expect("started above"))
    }
}

fn by_need(results: &[CaseResult]) -> Vec<NeedSummary> {
    if results.iter().all(|r| r.needs.is_empty()) {
        return Vec::new();
    }
    let mut m: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for r in results {
        let tags: Vec<&str> = if r.needs.is_empty() {
            vec!["-"]
        } else {
            r.needs.iter().map(String::as_str).collect()
        };
        for t in tags {
            let e = m.entry(t).or_default();
            e.0 += usize::from(r.pass);
            e.1 += 1;
        }
    }
    m.into_iter()
        .map(|(need, (passed, total))| NeedSummary {
            need: need.to_string(),
            passed,
            total,
        })
        .collect()
}

fn render(r: &Report) -> String {
    let mut s = String::new();
    let w = r.cases.iter().map(|c| c.id.len()).max().unwrap_or(4).max(4);
    s.push_str(&format!(
        "{:<6} {:<w$} {:<11} {:<9} {:>5} {:>12} {:>6}\n",
        "result", "case", "repo", "tool", "calls", "tokens", "ms"
    ));
    for c in &r.cases {
        let budget = c.budget.map_or("-".to_string(), |b| b.to_string());
        s.push_str(&format!(
            "{:<6} {:<w$} {:<11} {:<9} {:>5} {:>12} {:>6}\n",
            if c.pass { "PASS" } else { "FAIL" },
            c.id,
            c.repo,
            c.tool,
            c.calls,
            format!("{}/{budget}", c.tokens),
            c.ms
        ));
        if !c.pass {
            s.push_str(&format!("       question: {}\n", c.question));
        }
        for f in &c.failures {
            s.push_str(&format!("       - {f}\n"));
        }
        if let Some(a) = &c.answer {
            for line in a.lines() {
                s.push_str(&format!("       | {line}\n"));
            }
        }
    }
    s.push_str(&format!(
        "\n{} passed, {} failed, {} cases\n",
        r.passed,
        r.failed,
        r.cases.len()
    ));
    if !r.by_need.is_empty() {
        s.push_str("\nby needs (- = none):\n");
        for n in &r.by_need {
            s.push_str(&format!("  {:<20} {}/{}\n", n.need, n.passed, n.total));
        }
    }
    s
}

fn run_case(servers: &mut Servers, c: &Case, answers: Answers) -> Result<CaseResult> {
    let calls = c.calls();
    let many = calls.len() > 1;
    let mut failures = Vec::new();
    let mut texts: Vec<String> = Vec::new();
    let mut shown = String::new();
    let mut ms = 0;
    for (i, (repo, tool, args)) in calls.iter().enumerate() {
        // Failures of a call name it when the case has several.
        let at = if many {
            format!("call {} ({tool} on {repo}): ", i + 1)
        } else {
            String::new()
        };
        let args = serde_json::to_value(args)?;
        let server = servers.get(repo)?;
        let t0 = Instant::now();
        let res = server.call(tool, args.clone())?;
        ms += t0.elapsed().as_millis() as u64;
        let text = match &res {
            Ok(r) => r
                .get("content")
                .and_then(Value::as_array)
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|b| b["text"].as_str())
                        .collect::<String>()
                })
                .unwrap_or_default(),
            Err(e) => {
                failures.push(format!("{at}protocol error: {e}"));
                String::new()
            }
        };
        if let Ok(r) = &res {
            let is_error = r["isError"] == true;
            let code = r["structuredContent"]["error"]["code"].as_str();
            match (&c.error, is_error) {
                (None, true) => failures.push(format!(
                    "{at}unexpected tool error ({})",
                    code.unwrap_or("no code")
                )),
                (Some(want), false) => {
                    failures.push(format!("expected a {want} error, got an answer"))
                }
                (Some(want), true) if code != Some(want.as_str()) => failures.push(format!(
                    "expected a {want} error, got {}",
                    code.unwrap_or("no code")
                )),
                _ => {}
            }
        }
        if many {
            shown.push_str(&format!("── {tool} {args} on {repo}\n"));
        }
        shown.push_str(&text);
        if many && !text.ends_with('\n') {
            shown.push('\n');
        }
        texts.push(text);
    }
    let text = texts.join("\n");
    let mut from = 0;
    for want in &c.contains {
        if c.in_order {
            match text[from..].find(want.as_str()) {
                Some(i) => from += i + want.len(),
                None if text.contains(want.as_str()) => {
                    failures.push(format!("out of order: {want:?}"))
                }
                None => failures.push(format!("missing: {want:?}")),
            }
        } else if !text.contains(want.as_str()) {
            failures.push(format!("missing: {want:?}"));
        }
    }
    for bad in &c.not_contains {
        if text.contains(bad.as_str()) {
            failures.push(format!("must not contain: {bad:?}"));
        }
    }
    let tokens = texts
        .iter()
        .map(|t| cartograph_query::budget::tokens(t))
        .sum();
    // Default budget: the sum of the tools' contracts (none if a tool has no contract).
    let budget = c.budget.or_else(|| {
        calls
            .iter()
            .zip(&texts)
            .map(|((_, tool, _), text)| {
                use cartograph_mcp::stats::{budget_of, stats_tool};
                budget_of(stats_tool(tool, text))
            })
            .sum()
    });
    if let Some(b) = budget
        && tokens > b
    {
        failures.push(format!("over budget: {tokens} > {b} tokens"));
    }
    let pass = failures.is_empty();
    Ok(CaseResult {
        id: c.id.clone(),
        question: c.question.clone(),
        repo: c.repo.clone(),
        tool: calls
            .iter()
            .map(|(_, t, _)| *t)
            .collect::<Vec<_>>()
            .join(","),
        needs: c.needs.clone(),
        pass,
        calls: calls.len(),
        tokens,
        budget,
        ms,
        failures,
        answer: (answers == Answers::All || (answers == Answers::Failing && !pass))
            .then_some(shown),
    })
}

/// The directory to serve for a repo spec.
fn prepare(name: &str, spec: &RepoSpec, base: &Path, tmp: &Path) -> Result<PathBuf> {
    let kinds = usize::from(spec.path.is_some())
        + usize::from(spec.fixture.is_some())
        + usize::from(!spec.commits.is_empty());
    if kinds != 1 {
        bail!("give exactly one of path, fixture or commits");
    }
    if let Some(p) = &spec.path {
        let p = base.join(p);
        return dunce::canonicalize(&p).with_context(|| format!("{}", p.display()));
    }
    let root = tmp.join(name);
    if let Some(f) = &spec.fixture {
        copy_dir(&base.join(f), &root)?;
        return Ok(root);
    }
    let git = Git::new(tmp, &root)?;
    for (i, c) in spec.commits.iter().enumerate() {
        if let Some(b) = &c.branch {
            git.run(&["checkout", "-q", "-b", b])?;
        }
        write_files(&root, &c.files)?;
        for r in &c.remove {
            std::fs::remove_file(root.join(r)).with_context(|| format!("removing {r}"))?;
        }
        git.commit(i, &c.message)?;
    }
    write_files(&root, &spec.worktree)?;
    Ok(root)
}

fn write_files(root: &Path, files: &BTreeMap<String, String>) -> Result<()> {
    for (rel, content) in files {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, content).with_context(|| format!("writing {rel}"))?;
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from).with_context(|| format!("{}", from.display()))? {
        let entry = entry?;
        let dst = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dst)?;
        } else {
            std::fs::copy(entry.path(), &dst)?;
        }
    }
    Ok(())
}

/// The `git` CLI with no user or system config, fixed people and dates.
struct Git {
    root: PathBuf,
    config: PathBuf,
}

/// 2025-01-01T00:00:00Z.
const EPOCH: i64 = 1_735_689_600;

impl Git {
    fn new(tmp: &Path, root: &Path) -> Result<Git> {
        std::fs::create_dir_all(root)?;
        let config = tmp.join("eval.gitconfig");
        std::fs::write(&config, "")?;
        let g = Git {
            root: root.to_path_buf(),
            config,
        };
        g.run(&["init", "-q", "-b", "main"])?;
        Ok(g)
    }

    fn command(&self) -> Command {
        let mut c = Command::new("git");
        c.current_dir(&self.root)
            .env("GIT_CONFIG_GLOBAL", &self.config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .args(["-c", "core.autocrlf=false", "-c", "commit.gpgsign=false"]);
        c
    }

    fn run(&self, args: &[&str]) -> Result<()> {
        let out = self
            .command()
            .args(args)
            .output()
            .context("running git (is it installed?)")?;
        if !out.status.success() {
            bail!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// Commit everything as the `i`-th commit, one day apart.
    fn commit(&self, i: usize, message: &str) -> Result<()> {
        self.run(&["add", "-A"])?;
        let date = format!("{} +0000", EPOCH + i as i64 * 86_400);
        let out = self
            .command()
            .env("GIT_AUTHOR_NAME", "Eval Author")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "Eval Author")
            .env("GIT_COMMITTER_EMAIL", "author@example.com")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .args(["commit", "-q", "--allow-empty", "-m", message])
            .output()?;
        if !out.status.success() {
            bail!(
                "git commit: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }
}

/// `cartograph mcp` on one repo, spoken to over stdio.
struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: u64,
}

/// Longest a call may take, including the first index of the repo.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

impl Server {
    fn start(exe: &Path, root: &Path, data: &Path) -> Result<Server> {
        // Index first (files and git history), so the server's history is complete before
        // any answer: it serves this index, re-indexes once at startup, and the warm-up
        // below waits for that run.
        let opts = cartograph_index::IndexOptions {
            data_dir: Some(data.to_path_buf()),
            ..cartograph_index::IndexOptions::default()
        };
        cartograph_index::index_to_store(root, &opts)
            .with_context(|| format!("indexing {}", root.display()))?;
        let mut store = cartograph_core::Store::open(data.join(cartograph_index::DB_FILE))?;
        if let Err(e) = cartograph_index::git::update(root, &mut store) {
            eprintln!("warning: git history of {} not read: {e}", root.display());
        }
        drop(store);
        let mut child = Command::new(exe)
            .arg("mcp")
            .arg(root)
            .arg("--data-dir")
            .arg(data)
            .args(["--no-watch", "--no-stats", "--structured"])
            .args(["--fresh-wait-ms", "60000", "--tool-timeout", "60"])
            .env_remove("RUST_LOG")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("spawning cartograph mcp")?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut s = Server {
            child,
            stdin,
            lines: rx,
            next_id: 1,
        };
        let init = s.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "cartograph-eval", "version": env!("CARGO_PKG_VERSION")}
            }),
        )?;
        if init.get("error").is_some() {
            bail!("initialize failed: {init}");
        }
        s.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}))?;
        // Warm-up: until the startup re-index has replaced the index loaded from disk.
        let deadline = Instant::now() + CALL_TIMEOUT;
        loop {
            let r = s.call("overview", json!({}))?;
            let text = r
                .as_ref()
                .ok()
                .and_then(|r| r["content"][0]["text"].as_str())
                .unwrap_or_default()
                .to_string();
            let settled = !text.contains("is indexing") && !text.starts_with("(index from");
            if settled {
                break;
            }
            if Instant::now() > deadline {
                bail!("the server did not finish its startup index in time");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(s)
    }

    fn send(&mut self, msg: &Value) -> Result<()> {
        let stdin = self.stdin.as_mut().ok_or_else(|| anyhow!("stdin closed"))?;
        let mut line = serde_json::to_string(msg)?;
        line.push('\n');
        stdin.write_all(line.as_bytes())?;
        stdin.flush()?;
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + CALL_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .map_err(|_| anyhow!("no answer to {method} within {CALL_TIMEOUT:?}"))?;
            let msg: Value = serde_json::from_str(&line)
                .with_context(|| format!("stdout line is not JSON-RPC: {line:?}"))?;
            if msg["id"] == json!(id) {
                return Ok(msg);
            }
        }
    }

    /// `tools/call`: the result, or the protocol error.
    fn call(&mut self, tool: &str, args: Value) -> Result<Result<Value, String>> {
        let r = self.request("tools/call", json!({"name": tool, "arguments": args}))?;
        Ok(match r.get("error") {
            Some(e) => Err(e.to_string()),
            None => Ok(r["result"].clone()),
        })
    }

    fn stop(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
