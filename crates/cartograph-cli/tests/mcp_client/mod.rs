//! A tiny MCP stdio client for the integration tests: spawns `cartograph mcp`, writes
//! JSON-RPC lines to its stdin and reads its stdout line by line. Every stdout line must be
//! a JSON-RPC message (stdout carries only the protocol); stderr is collected apart.

#![allow(dead_code)]

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct Client {
    pub child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    pub stderr: Arc<Mutex<String>>,
    next_id: u64,
    /// Messages read while waiting for another id.
    pending: Vec<Value>,
}

/// The protocol version most clients send today.
pub const PROTOCOL: &str = "2025-06-18";

impl Client {
    /// Spawn `cartograph mcp <repo> <extra…>`.
    pub fn spawn(repo: &Path, extra: &[&str]) -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_cartograph"))
            .arg("mcp")
            .arg(repo)
            .args(extra)
            .env_remove("RUST_LOG")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn cartograph mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let err = Arc::new(Mutex::new(String::new()));
        let err2 = err.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                let mut e = err2.lock().unwrap();
                e.push_str(&line);
                e.push('\n');
            }
        });
        Client {
            child,
            stdin,
            lines: rx,
            stderr: err,
            next_id: 1,
            pending: Vec::new(),
        }
    }

    pub fn send(&mut self, msg: &Value) {
        let mut s = serde_json::to_string(msg).unwrap();
        s.push('\n');
        self.stdin.write_all(s.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    /// Send raw bytes (malformed input tests).
    pub fn send_raw(&mut self, raw: &str) {
        self.stdin.write_all(raw.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    /// Next stdout line as JSON; panics on a non-JSON line (protocol pollution).
    pub fn read(&mut self, timeout: Duration) -> Option<Value> {
        match self.lines.recv_timeout(timeout) {
            Ok(line) => Some(
                serde_json::from_str(&line)
                    .unwrap_or_else(|e| panic!("stdout line is not JSON-RPC ({e}): {line:?}")),
            ),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => None,
        }
    }

    /// Send a request and wait for the response with its id.
    pub fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.wait_id(id, timeout)
    }

    /// Send a request without waiting; returns its id.
    pub fn request_async(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    pub fn wait_id(&mut self, id: u64, timeout: Duration) -> Value {
        if let Some(i) = self.pending.iter().position(|m| m["id"] == json!(id)) {
            return self.pending.remove(i);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(msg) = self.read(left) else {
                panic!(
                    "no response to id {id} within {timeout:?}; stderr:\n{}",
                    self.stderr.lock().unwrap()
                );
            };
            if msg["id"] == json!(id) {
                return msg;
            }
            self.pending.push(msg);
        }
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// `initialize` + `notifications/initialized`; returns the initialize result.
    pub fn handshake(&mut self) -> Value {
        let init = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": {"name": "cartograph-test", "version": "0"}
            }),
            Duration::from_secs(20),
        );
        assert!(init.get("error").is_none(), "initialize failed: {init}");
        self.notify("notifications/initialized", json!({}));
        init["result"].clone()
    }

    /// `tools/call` and its result (panics on a protocol error).
    pub fn call(&mut self, name: &str, args: Value, timeout: Duration) -> Value {
        let r = self.request(
            "tools/call",
            json!({"name": name, "arguments": args}),
            timeout,
        );
        assert!(r.get("error").is_none(), "tools/call {name} failed: {r}");
        r["result"].clone()
    }

    /// Close stdin and wait for the process to exit.
    pub fn shutdown(mut self, timeout: Duration) -> std::process::ExitStatus {
        drop(self.stdin);
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(st) = self.child.try_wait().unwrap() {
                return st;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("server did not exit within {timeout:?} after stdin closed");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// A temp copy of `tests/fixtures/<name>`: (guard, repo root, data dir outside the repo).
pub fn fixture(name: &str) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(name);
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name),
        &root,
    );
    let data = tmp.path().join("data");
    (tmp, root, data)
}

pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dst = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dst);
        } else {
            std::fs::copy(entry.path(), &dst).unwrap();
        }
    }
}

/// Resident memory of a process in KiB, when the platform lets us read it.
pub fn rss_kib(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        let line = s.lines().find(|l| l.starts_with("VmRSS:"))?;
        return line.split_whitespace().nth(1)?.parse().ok();
    }
    #[cfg(windows)]
    {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .output()
            .ok()?;
        let s = String::from_utf8_lossy(&out.stdout);
        // "cartograph.exe","1234","Console","1","12,345 K"
        let mem = s.trim().rsplit("\",\"").next()?;
        let digits: String = mem.chars().filter(|c| c.is_ascii_digit()).collect();
        return digits.parse().ok();
    }
    #[allow(unreachable_code)]
    None
}

/// The text of the first content block of a tool result.
pub fn text(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}
