//! `impl ModelDriver for ClaudeCli`: spawn the CLI, serve its bridge connection, bound every wait.

use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::Instant;

use serde_json::{Value, json};

use super::{
    Bridge, ClaudeCli, DRAIN_GRACE, Invocation, POLL, Served, allowed_tool_names, mcp_config,
    outcome_of, parse_stream, serve_rpc,
};
use crate::drivers::anthropic::SYSTEM;
use crate::drivers::{DriveContext, ModelDriver, Outcome, ToolChannel};

/// Kills the CLI on every way out of [`ModelDriver::drive`], the `?` paths included.
///
/// Without it a driver error would return while a model was still working, holding this case's
/// paper node and this case's socket — and the next case is about to want both.
struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn get(&mut self) -> Result<&mut Child, String> {
        self.0.as_mut().ok_or_else(|| "the Claude Code CLI child is gone".to_string())
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// One live bridge connection, and the bytes of a frame that has not finished arriving.
struct Conn {
    stream: TcpStream,
    buf: Vec<u8>,
    /// The nonce line is the FIRST line on the socket; nothing is served before it matches.
    verified: bool,
}

/// A cheap per-run handshake token: the process id and the clock, folded. Uniqueness across a box
/// is not claimed and is not needed — see [`Bridge`].
fn mint_nonce() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}{:x}", std::process::id(), nanos)
}

/// The last of a child's stderr, so an error says more than that something died.
fn stderr_tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let tail: String = text.lines().rev().take(10).collect::<Vec<_>>().join(" | ");
    if tail.is_empty() { "its stderr is empty".into() } else { format!("its stderr: {tail}") }
}

/// Split every COMPLETE newline-terminated frame out of an accumulating buffer.
fn take_lines(buf: &mut Vec<u8>) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(at) = buf.iter().position(|b| *b == b'\n') {
        let line: Vec<u8> = buf.drain(..=at).collect();
        if let Ok(text) = String::from_utf8(line) {
            let text = text.trim().to_string();
            if !text.is_empty() {
                out.push(text);
            }
        }
    }
    out
}

/// `true` when a read error means "nothing arrived", rather than "the peer is gone".
fn is_would_block(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted)
}

impl ModelDriver for ClaudeCli {
    /// The model id the CLI reported once a case has run; before that, what was asked for.
    fn name(&self) -> String {
        self.observed
            .clone()
            .or_else(|| self.requested.clone())
            .unwrap_or_else(|| "claude-code-cli (model not yet reported)".to_string())
    }

    fn drive(
        &mut self,
        ctx: &DriveContext<'_>,
        prompt: &str,
        tools: &[Value],
        channel: &mut dyn ToolChannel,
    ) -> Result<Outcome, String> {
        let allowed = allowed_tool_names(tools);
        if allowed.is_empty() {
            return Err(format!(
                "the MCP server advertised no named tool for case {}, so the CLI would have been \
                 given an empty permission list and NOTHING would have been measured",
                ctx.case
            ));
        }

        // Listening BEFORE the CLI is spawned, so the bridge's connect can never lose a race.
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|e| format!("bind a loopback port for the MCP bridge: {e}"))?;
        let addr =
            listener.local_addr().map_err(|e| format!("read the MCP bridge's own address: {e}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("make the MCP bridge listener pollable: {e}"))?;

        let bridge = Bridge { addr: addr.to_string(), nonce: mint_nonce() };
        let descriptor_path = ctx.work_dir.join("claude-bridge.json");
        std::fs::write(&descriptor_path, bridge.to_json().to_string())
            .map_err(|e| format!("write {}: {e}", descriptor_path.display()))?;
        let exe = match &self.bridge_exe {
            Some(named) => named.clone(),
            None => std::env::current_exe()
                .map_err(|e| format!("locate this harness's own binary for the MCP bridge: {e}"))?,
        };
        let config_path = ctx.work_dir.join("claude-mcp.json");
        std::fs::write(&config_path, mcp_config(&exe, &descriptor_path).to_string())
            .map_err(|e| format!("write {}: {e}", config_path.display()))?;

        let inv = Invocation {
            mcp_config: &config_path,
            allowed_tools: &allowed,
            model: self.requested.as_deref(),
            system: SYSTEM,
        };
        let stderr_path = ctx.work_dir.join("claude.err");
        let stderr = std::fs::File::create(&stderr_path)
            .map_err(|e| format!("create {}: {e}", stderr_path.display()))?;
        let mut cmd = self.command(&inv, ctx.work_dir);
        cmd.stderr(Stdio::from(stderr));
        let mut child = ChildGuard(Some(cmd.spawn().map_err(|e| {
            format!(
                "spawn {} for case {}: {e}. The Claude Code CLI must be installed and on PATH (or \
                 named with --claude); `claude --version` is the check.",
                self.binary.display(),
                ctx.case
            )
        })?));

        // The prompt goes in on stdin and the pipe is then CLOSED — that is how `-p` learns the
        // prompt ended — which also keeps it out of a process listing and out of the way of the
        // variadic `--allowedTools`.
        {
            let mut stdin = child
                .get()?
                .stdin
                .take()
                .ok_or("the Claude Code CLI child has no stdin to write the prompt to")?;
            stdin
                .write_all(prompt.as_bytes())
                .and_then(|()| stdin.flush())
                .map_err(|e| format!("write the prompt for case {} to the CLI: {e}", ctx.case))?;
        }

        // A reader THREAD, so the serve loop below can carry a deadline. It ends when the child
        // closes its stdout, which disconnects the channel.
        let stdout = child.get()?.stdout.take().ok_or("the Claude Code CLI child has no stdout")?;
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("eval-cli-out".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { return };
                    if tx.send(line).is_err() {
                        return;
                    }
                }
            })
            .map_err(|e| format!("spawn the CLI stdout reader thread: {e}"))?;

        let deadline = Instant::now() + self.timeout;
        let mut lines: Vec<String> = Vec::new();
        let mut conn: Option<Conn> = None;
        let mut calls_left = self.max_tool_calls;
        let mut budget_exhausted = false;

        let status = loop {
            while let Ok(line) = rx.try_recv() {
                lines.push(line);
            }
            match child.get()?.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(e) => {
                    return Err(format!("waiting for the CLI on case {}: {e}", ctx.case));
                }
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "the Claude Code CLI did not finish case {} within {}s and was KILLED, so the \
                     case has no answer; {}",
                    ctx.case,
                    self.timeout.as_secs(),
                    stderr_tail(&stderr_path)
                ));
            }
            if conn.is_none() {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream
                            .set_nonblocking(false)
                            .and_then(|()| stream.set_read_timeout(Some(POLL)))
                            .map_err(|e| format!("configure the MCP bridge connection: {e}"))?;
                        conn = Some(Conn { stream, buf: Vec::new(), verified: false });
                    }
                    Err(e) if is_would_block(&e) => {}
                    Err(e) => return Err(format!("accept the MCP bridge connection: {e}")),
                }
            }
            // ⚠ The connection is dropped through a FLAG rather than by assigning `conn = None`
            // inside the borrow: `active` borrows `conn` for the whole block below, so the direct
            // spelling does not compile. `abort` is the same shape for the one error that must end
            // the drive — the real MCP server on the far side of the channel being gone.
            let mut drop_conn = false;
            let mut abort: Option<String> = None;
            if let Some(active) = conn.as_mut() {
                let mut chunk = [0u8; 8192];
                match active.stream.read(&mut chunk) {
                    // The bridge is gone. Keep listening: the CLI shuts its MCP servers down at the
                    // end of a run, and that is not a failure of the case.
                    Ok(0) => drop_conn = true,
                    Ok(n) => active.buf.extend_from_slice(&chunk[..n]),
                    Err(e) if is_would_block(&e) => {}
                    Err(_) => drop_conn = true,
                }
                if !drop_conn {
                    for frame in take_lines(&mut active.buf) {
                        let Ok(msg) = serde_json::from_str::<Value>(&frame) else {
                            // Not JSON: this is not our bridge, or the pipe is corrupt. Drop it
                            // rather than answering something that never spoke the protocol.
                            drop_conn = true;
                            break;
                        };
                        if !active.verified {
                            if msg["nonce"].as_str() == Some(bridge.nonce.as_str()) {
                                active.verified = true;
                                continue;
                            }
                            drop_conn = true;
                            break;
                        }
                        // ⚠ Read BEFORE the call, never after: `calls_left` reaches zero on the
                        // LAST call the budget allowed, and reading it afterwards would report a
                        // run that fitted its budget exactly as one that blew it.
                        let over_budget = msg["method"] == json!("tools/call") && calls_left == 0;
                        match serve_rpc(
                            &msg,
                            tools,
                            ctx.instructions,
                            &mut *channel,
                            &mut calls_left,
                        ) {
                            Ok(served) => {
                                if over_budget {
                                    budget_exhausted = true;
                                }
                                if let Served::Answer(answer) = served {
                                    let line = format!("{answer}\n");
                                    if active.stream.write_all(line.as_bytes()).is_err() {
                                        drop_conn = true;
                                        break;
                                    }
                                    let _ = active.stream.flush();
                                }
                            }
                            Err(e) => {
                                abort = Some(e);
                                break;
                            }
                        }
                    }
                }
            } else {
                // Nothing to read from and nothing accepted yet: pace the loop rather than spin.
                std::thread::sleep(POLL);
            }
            if let Some(e) = abort {
                return Err(e);
            }
            if drop_conn {
                conn = None;
            }
        };

        // The child has exited; its remaining stdout is still in flight. Bounded, like every other
        // wait here: the reader thread disconnects the channel when the pipe closes.
        let drain = Instant::now() + DRAIN_GRACE;
        loop {
            match rx.recv_timeout(POLL) {
                Ok(line) => lines.push(line),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if Instant::now() >= drain {
                        break;
                    }
                }
            }
        }

        let summary = parse_stream(&lines).map_err(|e| {
            format!(
                "{e} (case {}, the CLI exited {status}); {}",
                ctx.case,
                stderr_tail(&stderr_path)
            )
        })?;
        // What actually answered, for the report — read even when the run then fails, because a
        // failure by a named model is more useful than a failure by an unnamed one.
        if let Some(model) = &summary.model {
            self.observed = Some(model.clone());
        }
        if budget_exhausted {
            return Err(format!(
                "the agent was still calling tools after {} tool call(s) (--max-steps) on case {}; \
                 the case has no answer. Last text: {}",
                self.max_tool_calls, ctx.case, summary.final_text
            ));
        }
        outcome_of(&summary)
            .map_err(|e| format!("{e} (case {}, the CLI exited {status})", ctx.case))
    }
}
