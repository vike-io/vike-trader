//! The MCP server as this harness sees it: a spawned `vike-cli mcp` process, newline-delimited
//! JSON-RPC 2.0 over its stdio, and a TRANSCRIPT of every line in both directions.
//!
//! The transcript is the evidence the grader runs on, so it records two things nothing else can
//! reconstruct afterwards: WHO issued each request (the agent under test, or the harness setting a
//! case up / reading state back) and the exact order they were issued in. A grader that could not
//! tell those apart would credit the agent with a `node_snapshot` the harness made.
//!
//! ⚠ Every wait here is bounded. `vike_cli::cmd::mcp::serve` answers exactly one line per request
//! and nothing for a notification, so a missing answer means the server died or wedged — and a
//! blocking read on its stdout would then park this harness forever holding a paper node.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How long one JSON-RPC request may go unanswered before it is a finding.
///
/// Generous on purpose: a `node_snapshot` waits on the node's first published frame, and a write
/// tool's confirm waits on the node's control acknowledgement. Both are bounded inside the server;
/// this bound only has to be longer than theirs, and it exists so a DEAD server is reported rather
/// than waited on.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the server may take to exit after its stdin reaches EOF before it is killed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Who issued a transcript line.
///
/// The grader asserts over [`Actor::Agent`] lines only. A case's SETUP (resting an order the prompt
/// then asks to cancel) and its READ-BACK (the node state a check is graded against) go through the
/// same server on the same connection, and crediting those to the agent would let a case pass on
/// work the harness did for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    /// The driver under evaluation.
    Agent,
    /// The harness itself — case setup, and the node state read back for grading.
    Harness,
}

impl Actor {
    pub fn as_str(self) -> &'static str {
        match self {
            Actor::Agent => "agent",
            Actor::Harness => "harness",
        }
    }
}

/// One recorded line, in the order it crossed the pipe.
#[derive(Debug, Clone)]
pub struct Entry {
    pub seq: usize,
    pub actor: Actor,
    /// `true` for a line this harness WROTE, `false` for one the server answered.
    pub outbound: bool,
    pub message: Value,
}

impl Entry {
    pub fn to_json(&self) -> Value {
        json!({
            "seq": self.seq,
            "actor": self.actor.as_str(),
            "direction": if self.outbound { "->" } else { "<-" },
            "message": self.message,
        })
    }
}

/// What one `tools/call` did: the tool, the arguments as sent, and the result as answered.
///
/// Derived from the transcript by [`Transcript::tool_calls`] rather than recorded separately — the
/// transcript is the one record, and a second one could disagree with it.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub seq: usize,
    pub actor: Actor,
    pub tool: String,
    pub args: Value,
    pub result: ToolResult,
}

/// A `tools/call` answer. Both shapes `crates/vike-cli/src/cmd/mcp.rs`'s `tool_ok` / `tool_err`
/// produce carry `content[0].text` and set `isError` explicitly; only the success shape carries
/// `structuredContent`.
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub is_error: bool,
    pub text: String,
    pub structured: Value,
}

impl ToolResult {
    /// Read one out of a `tools/call` response.
    ///
    /// ⚠ An ABSENT `isError` reads as an error, never as a success: a response shape this harness
    /// does not understand must not be graded as work that happened.
    pub fn from_response(resp: &Value) -> Self {
        let result = &resp["result"];
        Self {
            is_error: result["isError"].as_bool().unwrap_or(true),
            text: result["content"][0]["text"].as_str().unwrap_or_default().to_string(),
            structured: result["structuredContent"].clone(),
        }
    }
}

/// The recorded lines of one case, and the projections the grader reads.
#[derive(Debug, Clone, Default)]
pub struct Transcript {
    pub entries: Vec<Entry>,
}

impl Transcript {
    /// Every `tools/call` in this transcript, request paired with its response by JSON-RPC id.
    ///
    /// A request whose response never arrived is DROPPED rather than reported as a failed call: the
    /// caller already turned that into an error at the time, and inventing a [`ToolResult`] for it
    /// here would hand the grader a fabricated answer.
    pub fn tool_calls(&self) -> Vec<ToolCall> {
        let mut out = Vec::new();
        for entry in self.entries.iter().filter(|e| e.outbound) {
            if entry.message["method"] != json!("tools/call") {
                continue;
            }
            let id = &entry.message["id"];
            let paired = self
                .entries
                .iter()
                .find(|e| !e.outbound && &e.message["id"] == id)
                .map(|e| &e.message);
            let Some(resp) = paired else { continue };
            out.push(ToolCall {
                seq: entry.seq,
                actor: entry.actor,
                tool: entry.message["params"]["name"].as_str().unwrap_or_default().to_string(),
                args: entry.message["params"]["arguments"].clone(),
                result: ToolResult::from_response(resp),
            });
        }
        out
    }

    /// The tool calls the AGENT made — what every expectation about the agent's behaviour reads.
    pub fn agent_calls(&self) -> Vec<ToolCall> {
        self.tool_calls().into_iter().filter(|c| c.actor == Actor::Agent).collect()
    }

    pub fn to_json(&self) -> Value {
        Value::Array(self.entries.iter().map(Entry::to_json).collect())
    }
}

/// A running `vike-cli mcp` server.
pub struct McpServer {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: i64,
    /// Answers read while waiting for a different id. Nothing here sends notifications and every
    /// request is awaited before the next is sent, so this stays empty in practice — it exists so
    /// an out-of-order answer is PARKED rather than mistaken for the one being waited on.
    parked: Vec<Value>,
    pub transcript: Transcript,
    stderr_path: PathBuf,
}

impl McpServer {
    /// Spawn `vike-cli mcp [--node ADDR]` against the case's settings directory and node keys.
    ///
    /// The environment is the throwaway project's, not the box's: `env_clear` is deliberately NOT
    /// used (the child needs PATH and the platform's own variables to run at all), but every
    /// variable that could change what this server does is set or removed explicitly by
    /// [`apply_case_env`].
    /// ⚠ `datahub_addr` is a port the caller has PROVEN FREE, never the server's default. The
    /// default is a well-known loopback port a real `vike-datahub` may be listening on — the CI box runs
    /// one — and the cases that measure "does the agent report an unreachable server honestly"
    /// would then silently become cases about whatever that server holds.
    pub fn start(
        binary: &Path,
        node_addr: Option<&str>,
        env: &CaseEnv<'_>,
        observe_key: &str,
        control_key: &str,
        stderr_path: &Path,
        datahub_addr: &str,
    ) -> Result<Self, String> {
        let mut cmd = Command::new(binary);
        cmd.arg("mcp").arg("--addr").arg(datahub_addr);
        if let Some(addr) = node_addr {
            cmd.arg("--node").arg(addr);
        }
        apply_case_env(&mut cmd, env);
        cmd.env("VIKE_TRADEHUB_OBSERVE_KEY", observe_key);
        cmd.env("VIKE_TRADEHUB_CONTROL_KEY", control_key);
        Self::launch(cmd, stderr_path)
    }

    /// Spawn an ALREADY-BUILT `vike-cli mcp` command and take over its pipes.
    ///
    /// ⚠ This is the seam [`McpServer::start`] itself goes through, and it exists because the
    /// UNATTENDED runner ([`crate::unattended`]) needs a different command with the same
    /// transport. Its child is pointed at the OPERATOR's project rather than a throwaway one, so it
    /// sets no `VIKE_SETTINGS_DIR` of its own unless asked, mints no node keys (the shipped
    /// credential store already holds the operator's), and carries `--profile`/`--trace-dir`
    /// arguments no case ever passes. Every one of those is a difference in the COMMAND and none is
    /// a difference in the transport — so the argv construction moved out and the spawn, the reader
    /// thread and every deadline below stayed in one place. Duplicating this half is how the two
    /// callers would come to disagree about what a dead server looks like.
    ///
    /// The caller owns the program, the arguments and the environment; this owns the stdio. Stdin
    /// and stdout MUST be pipes (they are the JSON-RPC transport) and stderr MUST be the file
    /// [`McpServer::stderr_tail`] reads, so all three are set here and a caller cannot get them
    /// wrong.
    pub fn launch(mut cmd: Command, stderr_path: &Path) -> Result<Self, String> {
        let err = std::fs::File::create(stderr_path)
            .map_err(|e| format!("create {}: {e}", stderr_path.display()))?;
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::from(err));
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("spawn {}: {e}", cmd.get_program().to_string_lossy()))?;
        let stdin = child.stdin.take().ok_or("the mcp server has no stdin")?;
        let stdout = child.stdout.take().ok_or("the mcp server has no stdout")?;
        let (tx, lines) = channel();
        // A reader THREAD, not a blocking read on the caller's: it is what lets every await below
        // carry a deadline. The thread ends when the server closes its stdout, which closes the
        // channel and turns a later await into a prompt "the server is gone" rather than a hang.
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(l) = line else { return };
                if tx.send(l).is_err() {
                    return;
                }
            }
        });
        Ok(Self {
            child,
            stdin: Some(stdin),
            lines,
            next_id: 0,
            parked: Vec::new(),
            transcript: Transcript::default(),
            stderr_path: stderr_path.to_path_buf(),
        })
    }

    /// The MCP handshake: the server's `instructions`, plus the tool roster it advertises.
    ///
    /// The roster is READ, never written down here: it is what a client would see, and a case that
    /// asserts "the agent called no tool outside the roster" must compare against what the server
    /// actually offered rather than against a list that could rot.
    ///
    /// ⚠ **The `instructions` string is part of the surface under test, exactly as the tool
    /// descriptions are**, and it is returned rather than discarded because a real MCP client shows
    /// it to the model. This harness IS the client for
    /// [`crate::anthropic::Anthropic`] and stands beside one for [`crate::claude_cli::ClaudeCli`],
    /// so a harness that dropped the field would be measuring a smaller surface than the one that
    /// ships — which is how four cases came to be graded on what the model REMEMBERED about this
    /// product rather than on what the product told it. An ABSENT field is not an error here: a
    /// server may legitimately serve none, and the drivers render the empty string as nothing.
    pub fn initialize(&mut self) -> Result<(Vec<Value>, String), String> {
        let init =
            self.request(Actor::Harness, "initialize", json!({ "protocolVersion": "2024-11-05" }))?;
        let instructions = init["result"]["instructions"].as_str().unwrap_or_default().to_string();
        let listed = self.request(Actor::Harness, "tools/list", json!({}))?;
        let tools =
            listed["result"]["tools"].as_array().ok_or("tools/list answered no `tools` array")?;
        if tools.is_empty() {
            return Err("tools/list answered an EMPTY roster — nothing could be evaluated".into());
        }
        Ok((tools.clone(), instructions))
    }

    /// Send one request and return its answer, recording both.
    pub fn request(&mut self, actor: Actor, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.record(actor, true, req.clone());
        let line = format!("{req}\n");
        let stderr = self.stderr_tail();
        let stdin = self.stdin.as_mut().ok_or("the mcp server's stdin is already closed")?;
        // ⚠ GUARDED: the reader on the other end is a process that can die. A failed write means
        // the server is gone, and it must be reported WITH the server's own stderr — the only place
        // a panic message of its own exists.
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("writing {method} to the mcp server failed ({e}); {stderr}"))?;
        let resp = self.await_id(id)?;
        self.record(actor, false, resp.clone());
        Ok(resp)
    }

    /// Call one tool by name.
    pub fn call_tool(
        &mut self,
        actor: Actor,
        name: &str,
        args: &Value,
    ) -> Result<ToolResult, String> {
        let resp = self.request(actor, "tools/call", json!({ "name": name, "arguments": args }))?;
        Ok(ToolResult::from_response(&resp))
    }

    fn record(&mut self, actor: Actor, outbound: bool, message: Value) {
        let seq = self.transcript.entries.len();
        self.transcript.entries.push(Entry { seq, actor, outbound, message });
    }

    fn await_id(&mut self, id: i64) -> Result<Value, String> {
        if let Some(pos) = self.parked.iter().position(|v| v["id"] == json!(id)) {
            return Ok(self.parked.remove(pos));
        }
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!(
                    "no MCP response for id {id} within {}s; {}",
                    RESPONSE_TIMEOUT.as_secs(),
                    self.stderr_tail()
                ));
            }
            match self.lines.recv_timeout(left) {
                Ok(line) if line.trim().is_empty() => continue,
                Ok(line) => match serde_json::from_str::<Value>(&line) {
                    Ok(v) if v["id"] == json!(id) => return Ok(v),
                    Ok(v) => self.parked.push(v),
                    Err(e) => {
                        return Err(format!(
                            "the mcp server wrote a line that is not JSON ({e}): {line}"
                        ));
                    }
                },
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(format!(
                        "the mcp server closed its stdout while id {id} was outstanding; {}",
                        self.stderr_tail()
                    ));
                }
            }
        }
    }

    /// The last of the server's stderr, so an error says more than that something died.
    fn stderr_tail(&self) -> String {
        let text = std::fs::read_to_string(&self.stderr_path).unwrap_or_default();
        let tail: String = text.lines().rev().take(10).collect::<Vec<_>>().join(" | ");
        if tail.is_empty() { "its stderr is empty".into() } else { format!("its stderr: {tail}") }
    }

    /// Close stdin (which is how `serve` returns) and wait, BOUNDED, for the process to go.
    ///
    /// A server still alive after the grace period is killed and reported: `serve` returns when
    /// `reader.lines()` ends, so one that outlives its own EOF is a finding, not a slow exit.
    pub fn shutdown(&mut self) -> Result<(), String> {
        self.stdin = None;
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => return Err(format!("waiting for the mcp server: {e}")),
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        Err(format!(
            "the mcp server was still alive {}s after its stdin reached EOF (serve returns when \
             reader.lines() ends) — killed",
            SHUTDOWN_GRACE.as_secs()
        ))
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What every child of this harness runs under: the throwaway project's settings root, and the
/// variables removed from the environment it would otherwise inherit whole.
///
/// One struct rather than two parameters because both spawn sites take both, and because they are
/// one decision — "this child sees the case's world and nothing of the box's".
pub struct CaseEnv<'a> {
    pub settings_dir: &'a Path,
    /// See [`apply_case_env`].
    pub scrub: &'a [&'a str],
}

/// The environment every child of this harness runs under.
///
/// `VIKE_SETTINGS_DIR` names the throwaway project outright, so nothing depends on which directory
/// the harness was started from — the walk `crates/vike-secrets/src/dotenv.rs`'s
/// `project_settings_dir` would otherwise perform answers with whatever manifest sits above the
/// current directory. The removals are the variables that would change what is being measured: a
/// REMOVED setting that is SET is a deliberate startup refusal (`crates/vike-config/src/lib.rs`'s
/// `REMOVED_ENV`), the guardrail knob changes the preview payload, and `VIKE_LOG_DIR` outranks
/// every other rung in `crates/vike-log/src/lib.rs`'s `resolve_log_dir`, so a lane that exported one
/// would take the daemon's log out of the throwaway root.
///
/// ⚠ `scrub` is the CALLER's list, and its subject is a secret rather than a setting: the harness
/// binary holds an Anthropic API key in its own environment while a real model is driving, and a
/// spawned child inherits the whole environment by default. `crates/vike-cli/src/lib.rs`'s `run`
/// sweeps `std::env::vars()` into a map on every invocation, so the key would sit inside a process
/// this harness spawns twice per case for no reason at all. It is a PARAMETER rather than a
/// constant here because only the binary knows which variables it read — the rule
/// `crates/vike-ops/tests/settings_registry.rs` states, applied to a name rather than to a value.
pub fn apply_case_env(cmd: &mut Command, env: &CaseEnv<'_>) {
    cmd.env("VIKE_SETTINGS_DIR", env.settings_dir);
    for name in env.scrub {
        cmd.env_remove(name);
    }
    for removed in [
        "VIKE_MAX_ORDER_NOTIONAL",
        "VIKE_TRADEHUB_MAX_ORDER_NOTIONAL",
        "VIKE_SECRETS_PASSPHRASE",
        "VIKE_MAX_ORDER_QTY",
        "VIKE_LOG_DIR",
        "VIKE_USER_DATA_DIR",
        "VIKE_HIST_STORE",
        "RUST_LOG",
        "VIKE_LOG",
    ] {
        cmd.env_remove(removed);
    }
    cmd.env("VIKE_LOG_FILE_LEVEL", "warn");
}
