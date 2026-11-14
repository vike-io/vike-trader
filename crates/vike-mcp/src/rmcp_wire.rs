//! The hand-written [`Transport`] under rmcp, which keeps what this server has always promised a
//! client where the SDK's own serve loop promises otherwise.
//!
//! The lines are read here, so everything that decides what rmcp ever SEES is decided here:
//!
//! - **One inline request at a time.** [`Transport::receive`] reads no further line while a request
//!   that is not a daemon run is unanswered, the way a blocking loop would. So answers come in request
//!   order, a confirm cannot overtake its preview, and a cancel behind an inline tool is read after
//!   that tool's answer.
//! - **Only a daemon run can be cancelled.** A cancel is forwarded only when it names a run in flight
//!   ([`Wire::runs`]). Anything else finds nothing and does nothing: rmcp would drop that answer while
//!   the tool kept running, and on a confirmed write the order goes out with the agent told nothing
//!   (`crates/vike-mcp/src/node_drop_tests/cancel_write.rs` is the proof).
//! - **rmcp never sees a client's id.** Each request goes in under a private number and its answer
//!   goes out under the client's id, so a cancelled handler's late answer cannot take a reused id's
//!   entry out of the SDK's table, and any JSON value is an id.
//! - **Lines rmcp would refuse or lose are dealt with here**: `-32700` for a malformed line, `-32600`
//!   for a second run under an id in flight, silence for JSON that is no request, `jsonrpc` unchecked,
//!   a `params` that is no object read as none, a byte order mark read past.
//! - **EOF waits for what is in flight**, not the SDK's five seconds, because `receive` does not
//!   report it until the table is empty; but a run's reply read has no bound of its own, so not past
//!   `Server::eof_deadline`, and a dead daemon link leaves no process behind. A failed read ends the
//!   session at once, after every line before it, and a line that is not UTF-8 is such a read.
//! - **A client's burst waits in its pipe, not here**: at most [`LINES_QUEUED`] lines of at most
//!   [`MAX_LINE_BYTES`] wait for `receive`, so no input can run this process out of memory, and a
//!   write's answer is never lost to one. A line over the cap is read past to its newline and answered
//!   `-32700`: it costs that line, and a run in flight keeps its answer.
//! - **The answers go to the loop's thread** ([`Job::Output`]), so stdout keeps its one writer.
//!
//! ⚠ `pending()` below relies on the SDK's loop building a new `receive` future on every turn, which
//! it does (`rmcp`'s `serve_inner`): `send` always runs between two of them, so no wake-up is needed.
//! That is an internal of rmcp 3.9.0, and the reason the root manifest pins it exactly:
//! `crates/vike-mcp/CLAUDE.md` has the suites to repeat on a bump.
//!
//! A handler task that panics still gets its answer, or reading would stop here for good:
//! `crates/vike-mcp/src/rmcp_seam.rs`'s `Guarded`.
//!
//! shortcut: an id is rewritten only on the way back, on the assumption this server originates no
//! request, true while the seam calls no `peer.*` and forbidden by 2026-07-28 anyway.

use std::collections::HashMap;
use std::io::{self, BufRead, Read};
use std::sync::mpsc;
use std::time::Duration;

use rmcp::RoleServer;
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;
use serde_json::{Value, json};
use tokio::sync::mpsc::{Receiver, channel};

use super::Server;
use super::protocol::{DEFERRED_TOOLS, RequestId, rpc_error};
use super::rmcp_seam::Job;

/// The longest input line. A request's whole profile and script are kilobytes, and a hub refuses a
/// frame over `vike_hub_protocol::frame`'s `MAX_FRAME_LEN` anyway.
const MAX_LINE_BYTES: u64 = 16 * 1024 * 1024;

/// How many lines may wait for `receive`. Nothing needs more than a few: past them a line waits in
/// the client's pipe, and an agent that writes without reading its answers is the one that blocks.
const LINES_QUEUED: usize = 8;

/// How long input that has ended may wait for a run in flight (`Server::eof_deadline`). Longer than a
/// real run an unattended pipe would wait for, and short of an orphan process that lives for ever.
pub(super) const EOF_DEADLINE: Duration = Duration::from_secs(60 * 60);

/// What one read of the input found.
#[derive(Debug, PartialEq)]
enum Line {
    Text(String),
    /// A line over [`MAX_LINE_BYTES`], read past to its newline and kept nowhere.
    OverTheCap,
}

pub(super) struct Wire {
    lines: Receiver<io::Result<Line>>,
    jobs: mpsc::Sender<Job>,
    /// Taken at the first EOF with something in flight: see [`Wire::end_at_the_deadline`].
    eof_deadline: Option<Duration>,
    /// The [`DEFERRED_TOOLS`] this session admits: `Server::classify`'s question, asked up front.
    deferred: Vec<&'static str>,
    next: i64,
    /// Every request rmcp holds: its private number -> the id the client gave it.
    in_flight: HashMap<i64, Value>,
    /// The daemon runs among them: the client's id -> the private number.
    runs: HashMap<RequestId, i64>,
}

impl Wire {
    /// Start reading `reader` on a thread of its own.
    pub(super) fn open(
        mut reader: impl BufRead + Send + 'static,
        jobs: mpsc::Sender<Job>,
        server: &Server,
    ) -> io::Result<Self> {
        let (line, lines) = channel(LINES_QUEUED);
        std::thread::Builder::new().name("mcp-stdin".into()).spawn(move || {
            while let Some(read) = capped_line(&mut reader) {
                let last = read.is_err();
                if line.blocking_send(read).is_err() || last {
                    return;
                }
            }
        })?;
        let deferred = DEFERRED_TOOLS.into_iter().filter(|t| server.access.admits(t)).collect();
        Ok(Self {
            lines,
            jobs,
            eof_deadline: Some(server.eof_deadline),
            deferred,
            next: 0,
            in_flight: HashMap::new(),
            runs: HashMap::new(),
        })
    }

    /// Input ended with something in flight: give it until the deadline, then end the session, loudly.
    /// A thread rather than a timer of the SDK's runtime, whose loop this does not own; it does nothing
    /// to a session that ended first, whose inbox is gone.
    fn end_at_the_deadline(&mut self) {
        let Some(deadline) = self.eof_deadline.take() else { return };
        let jobs = self.jobs.clone();
        let _ = std::thread::Builder::new().name("mcp-eof".into()).spawn(move || {
            std::thread::sleep(deadline);
            let why = format!(
                "input ended {} s ago and a run has not answered: giving up on it",
                deadline.as_secs()
            );
            let _ = jobs.send(Job::Ended(Err(io::Error::new(io::ErrorKind::TimedOut, why))));
        });
    }

    /// One input line: the message rmcp should read, or `None` when this layer has dealt with it.
    fn admit(&mut self, line: &str) -> Option<Value> {
        // A pipe from Windows PowerShell 5.1 under `[Text.Encoding]::UTF8` opens with a byte order
        // mark, and with two when its console is UTF-8 as well (measured: `ef bb bf ef bb bf 7b`).
        let line = line.trim_start_matches('\u{feff}');
        if line.trim().is_empty() {
            return None;
        }
        let mut msg: Value = match serde_json::from_str(line) {
            Ok(msg) => msg,
            Err(e) => {
                let _ = self.write(&rpc_error(Value::Null, -32700, &format!("parse error: {e}")));
                return None;
            }
        };
        let method = msg.get("method").and_then(Value::as_str)?.to_string();
        // A test-only method that panics inside the SDK's own loop, which runs `receive`.
        #[cfg(test)]
        if method == "__test_loop_panic" {
            panic!("kaboom");
        }
        // What the SDK's types refuse and this server has always read past. Past the line above,
        // `msg` is an object.
        if let Some(fields) = msg.as_object_mut() {
            fields.retain(|k, v| (k != "params" || v.is_object()) && k != "result" && k != "error");
        }
        if let Some(params) = msg.get_mut("params").and_then(Value::as_object_mut) {
            params.retain(|k, v| k != "_meta" || v.is_object());
        }
        if method == "notifications/initialized" {
            return None;
        }
        if method == "notifications/cancelled" {
            let id = msg.pointer("/params/requestId")?;
            let number = self.runs.remove(&RequestId::of(id))?;
            self.in_flight.remove(&number);
            // Rebuilt, not edited: a field the SDK's type refuses would make this a custom
            // notification, the run would keep running, and its answer would carry no client's id.
            let params = json!({ "requestId": number });
            return Some(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
        }
        if let Some(id) = msg.get("id").cloned() {
            let name = msg.pointer("/params/name").and_then(Value::as_str).unwrap_or_default();
            let run = method == "tools/call" && self.deferred.contains(&name);
            let key = RequestId::of(&id);
            if run && self.runs.contains_key(&key) {
                let why = "invalid request: this id is already in use by a request still in flight";
                let _ = self.write(&rpc_error(id, -32600, why));
                return None;
            }
            self.next += 1;
            self.in_flight.insert(self.next, id);
            if run {
                self.runs.insert(key, self.next);
            }
            msg["id"] = json!(self.next);
        }
        msg["jsonrpc"] = json!("2.0");
        Some(msg)
    }

    /// An answer rmcp wrote: back under the client's id, and out of the tables.
    fn answer(&mut self, mut msg: Value) -> io::Result<()> {
        if let Some(number) = msg["id"].as_i64()
            && let Some(id) = self.in_flight.remove(&number)
        {
            let key = RequestId::of(&id);
            if self.runs.get(&key) == Some(&number) {
                self.runs.remove(&key);
            }
            msg["id"] = id;
        }
        self.write(&msg)
    }

    /// One whole line for the loop's thread. ⚠ A number here is what the CLIENT sent, unless it came
    /// through [`Wire::answer`], so an answer of this layer's own never goes through that rewrite.
    fn write(&self, msg: &Value) -> io::Result<()> {
        let line = format!("{msg}\n").into_bytes();
        self.jobs.send(Job::Output(line)).map_err(|_| io::ErrorKind::BrokenPipe.into())
    }
}

/// The next line, its ending off as `BufRead::lines` takes it (`\n` or `\r\n`), `None` at EOF. A line over
/// [`MAX_LINE_BYTES`] is read past to its newline, no more of it kept in memory.
fn capped_line(reader: &mut impl BufRead) -> Option<io::Result<Line>> {
    let mut bytes = Vec::new();
    match reader.by_ref().take(MAX_LINE_BYTES + 1).read_until(b'\n', &mut bytes) {
        Ok(0) => return None,
        Ok(_) => {}
        Err(e) => return Some(Err(e)),
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    } else if bytes.len() as u64 > MAX_LINE_BYTES {
        drop(bytes);
        return Some(reader.skip_until(b'\n').map(|_| Line::OverTheCap));
    }
    Some(String::from_utf8(bytes).map(Line::Text).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "stream did not contain valid UTF-8")
    }))
}

impl Transport<RoleServer> for Wire {
    type Error = io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = io::Result<()>> + Send + 'static {
        let msg = serde_json::to_value(&item).map_err(io::Error::other);
        std::future::ready(msg.and_then(|msg| self.answer(msg)))
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        loop {
            if self.in_flight.len() > self.runs.len() {
                return std::future::pending().await;
            }
            let line = match self.lines.recv().await {
                Some(Ok(Line::Text(line))) => line,
                Some(Ok(Line::OverTheCap)) => {
                    let why = format!(
                        "parse error: an input line is over the limit of {MAX_LINE_BYTES} bytes, and \
                         it was read past"
                    );
                    let _ = self.write(&rpc_error(Value::Null, -32700, &why));
                    continue;
                }
                Some(Err(e)) => {
                    let _ = self.jobs.send(Job::Ended(Err(e)));
                    return std::future::pending().await;
                }
                None if self.in_flight.is_empty() => return None,
                None => {
                    self.end_at_the_deadline();
                    return std::future::pending().await;
                }
            };
            let Some(msg) = self.admit(&line) else { continue };
            let number = msg.get("id").cloned();
            match serde_json::from_value(msg) {
                Ok(message) => return Some(message),
                // JSON the SDK has no type for: a request is still answered, never lost.
                Err(e) => {
                    if let Some(number) = number {
                        let why = format!("invalid request: {e}");
                        let _ = self.answer(rpc_error(number, -32600, &why));
                    }
                }
            }
        }
    }

    async fn close(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Input with no end, counting what it was asked for.
    struct Endless(Arc<AtomicUsize>);

    impl Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.fetch_add(1, Ordering::Relaxed);
            let line = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n";
            buf[..line.len()].copy_from_slice(line);
            Ok(line.len())
        }
    }

    /// **A client's burst waits in the pipe, not in this process**: with nothing taking lines off the
    /// queue (`receive` holding the next one behind an inline call), reading stops. It used to go on
    /// for as long as the client wrote, until the allocator aborted the process, a confirmed write's
    /// answer included.
    #[test]
    fn lines_nobody_is_taking_are_not_read() {
        let lines = Arc::new(AtomicUsize::new(0));
        let (jobs, _inbox) = mpsc::channel();
        let reader = BufReader::new(Endless(Arc::clone(&lines)));
        let _wire = Wire::open(reader, jobs, &crate::test_server()).expect("the reader thread");
        std::thread::sleep(Duration::from_millis(500));
        let read = lines.load(Ordering::Relaxed);
        assert!(read <= LINES_QUEUED + 2, "{read} lines were read with nothing taking them");
    }

    /// **One line past the cap costs that line and nothing else**: it is read past up to its newline and
    /// answered `-32700`, the answer of any line that is no JSON, and every line around it is served. It used to
    /// end the session, a run in flight with it.
    #[test]
    fn a_line_over_the_cap_is_answered_and_the_session_goes_on() {
        let ping = |id: i64| json!({ "jsonrpc": "2.0", "id": id, "method": "ping" }).to_string();
        let huge = "x".repeat(MAX_LINE_BYTES as usize + 1);
        let input = format!("{}\n{huge}\n{}\n", ping(1), ping(2));
        let mut out = Vec::new();
        crate::serve(Cursor::new(input), &mut out, &mut crate::test_server())
            .expect("the session ends cleanly at EOF");
        let answered: Vec<Value> = String::from_utf8(out)
            .expect("UTF-8")
            .lines()
            .map(|line| serde_json::from_str(line).expect("one message a line"))
            .collect();
        assert_eq!(answered.len(), 3, "{answered:?}");
        assert_eq!(answered[0]["id"], json!(1), "{answered:?}");
        assert_eq!(answered[1]["id"], Value::Null, "{answered:?}");
        assert_eq!(answered[1]["error"]["code"], json!(-32700), "{answered:?}");
        assert!(answered[1]["error"]["message"].as_str().is_some_and(|m| m.contains("over")));
        assert_eq!(answered[2]["id"], json!(2), "{answered:?}");
    }

    /// What `BufRead::lines` did, read by the capped reader: both line endings, a last line with no
    /// ending, a blank line, and a line of exactly the cap.
    #[test]
    fn the_capped_read_takes_lines_as_lines_did() {
        let cap = MAX_LINE_BYTES as usize;
        let text = format!("a\nb\r\n\n\r\nc\rd\n{}\ne", "x".repeat(cap));
        let mut reader = Cursor::new(text);
        let mut read = Vec::new();
        while let Some(line) = capped_line(&mut reader) {
            read.push(line.expect("every line here is short enough"));
        }
        let text_of = |lines: &[&str]| -> Vec<Line> {
            lines.iter().map(|line| Line::Text((*line).to_string())).collect()
        };
        let cap_line = "x".repeat(cap);
        assert_eq!(read, text_of(&["a", "b", "", "", "c\rd", cap_line.as_str(), "e"]));

        let mut over = Cursor::new(format!("{}\nnext\n", "x".repeat(cap + 1)));
        let first = capped_line(&mut over).expect("a line").expect("a read");
        assert_eq!(first, Line::OverTheCap, "one byte over the cap");
        let next = capped_line(&mut over).expect("a line").expect("a read");
        assert_eq!(next, Line::Text("next".to_string()), "read past to its newline, no further");
        let mut at_eof = Cursor::new(vec![b'x'; cap + 1]);
        let last = capped_line(&mut at_eof).expect("a line").expect("a read");
        assert_eq!(last, Line::OverTheCap, "with no newline to find");

        let mut not_text = Cursor::new(vec![0xff, 0xfe, b'\n']);
        let why = capped_line(&mut not_text).expect("a line").expect_err("not UTF-8");
        assert_eq!(why.to_string(), "stream did not contain valid UTF-8");
    }
}
