//! Request cancellation over the real transport: `notifications/cancelled` aborts a daemon tool
//! (`run_backtest`, `run_sweep`, `run_walk_forward`) that is still in flight, the cancelled request
//! gets NO response, and the session keeps answering everything else.
//!
//! Every case drives [`serve`] — the event loop itself, not `Server::handle` — against a FAKE
//! backtesthub: a loopback listener that answers the handshake, reads the request, and then does
//! what the case asks (hold until the peer closes, answer at once, or answer when the test opens a
//! gate), reporting each request and each peer close on a channel. No real daemon, no store, so the
//! "the client closed its socket" half is observed directly rather than inferred from a timing.
//!
//! The input is a channel the test feeds one line at a time, so a case can wait for the daemon to
//! have the request BEFORE it sends the cancel; the output is parsed line by line as the server
//! writes it, so a case can wait for one response before sending the next request.

use std::io::{self, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use vike_backtesthub_client::proto::{PROTO_VERSION, Request, Response};
use vike_hub_protocol::frame::{read_frame, write_frame};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};

use super::*;

/// How long any one wait in this file may take before the case FAILS rather than hangs. Generous:
/// every wait is for a loopback hop or a thread wake-up, so reaching it means the thing waited for
/// is never coming.
pub(crate) const WAIT: Duration = Duration::from_secs(10);

/// What the fake daemon reports for a connection that closed before its request arrived.
const BEFORE_REQUEST: &str = "<closed before the request>";

/// One thing the fake daemon saw, keyed by the request's LABEL (the profile text the case sent).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    Request(String),
    PeerClosed(String),
}

/// The fake backtesthub. A request whose profile starts with `hold` is never answered: the
/// connection blocks reading until the peer closes. `answer` is answered at once; `gated` is answered
/// when the test sends on the gate. Each connection reports its request and its peer close.
struct FakeDaemon {
    addr: SocketAddr,
    seen: mpsc::Receiver<Seen>,
    gate: mpsc::Sender<()>,
    /// Opens the handshake of the NEXT connection when [`fake_daemon_with_gated_handshake`] made it.
    handshake_gate: Option<mpsc::Sender<()>>,
}

impl FakeDaemon {
    fn next_seen(&self) -> Seen {
        self.seen.recv_timeout(WAIT).expect("the fake daemon reports within the wait")
    }

    /// Let one `gated` connection answer.
    fn open_gate(&self) {
        self.gate.send(()).expect("the gate is held by the fake");
    }

    /// Let the next connection of a [`fake_daemon_with_gated_handshake`] answer its `Hello`.
    fn open_handshake(&self) {
        let gate = self.handshake_gate.as_ref().expect("a daemon made with a handshake gate");
        gate.send(()).expect("the handshake gate is held by the fake");
    }

    /// Nothing more arrives within a short window — for "the request was never sent".
    fn assert_quiet(&self) {
        if let Ok(extra) = self.seen.recv_timeout(Duration::from_millis(300)) {
            panic!("the fake daemon saw more than the case allows: {extra:?}");
        }
    }
}

fn fake_daemon() -> FakeDaemon {
    spawn_fake_daemon(None)
}

/// The same fake, except each connection waits for one send on `handshake_gate` before it answers
/// the `Hello` — so a case can hold a worker BEFORE its client exists.
fn fake_daemon_with_gated_handshake() -> FakeDaemon {
    let (tx, rx) = mpsc::channel();
    let mut daemon = spawn_fake_daemon(Some(rx));
    daemon.handshake_gate = Some(tx);
    daemon
}

fn spawn_fake_daemon(handshake_gate: Option<mpsc::Receiver<()>>) -> FakeDaemon {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let (seen_tx, seen) = mpsc::channel();
    let (gate, gate_rx) = mpsc::channel::<()>();
    let gate_rx = Arc::new(Mutex::new(gate_rx));
    let handshake_gate = Arc::new(Mutex::new(handshake_gate));
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let seen_tx = seen_tx.clone();
            let gate_rx = Arc::clone(&gate_rx);
            let handshake_gate = Arc::clone(&handshake_gate);
            thread::spawn(move || fake_connection(stream, &seen_tx, &gate_rx, &handshake_gate));
        }
    });
    FakeDaemon { addr, seen, gate, handshake_gate: None }
}

fn fake_connection(
    mut stream: TcpStream,
    seen: &mpsc::Sender<Seen>,
    gate: &Mutex<mpsc::Receiver<()>>,
    handshake_gate: &Mutex<Option<mpsc::Receiver<()>>>,
) {
    if let Some(rx) = handshake_gate.lock().expect("handshake gate").as_ref() {
        let _ = rx.recv_timeout(WAIT);
    }
    let hello = read_frame::<_, HandshakeFrame>(&mut stream);
    let welcome = HandshakeReply::Welcome {
        proto_version: PROTO_VERSION,
        features: vec!["backtest".to_string()],
        nonce: None,
    };
    if !matches!(hello, Ok(HandshakeFrame::Hello { .. }))
        || write_frame(&mut stream, &welcome).is_err()
    {
        let _ = seen.send(Seen::PeerClosed(BEFORE_REQUEST.to_string()));
        return;
    }
    let Ok(request) = read_frame::<_, Request>(&mut stream) else {
        let _ = seen.send(Seen::PeerClosed(BEFORE_REQUEST.to_string()));
        return;
    };
    let (label, answer) = match request {
        Request::RunBacktest(profile) => {
            let report = json!({ "label": profile }).to_string();
            (profile, Response::Report(report))
        }
        Request::RunParamscanProfile { profile_toml, .. } => {
            let report = json!({ "label": profile_toml }).to_string();
            (profile_toml, Response::ParamscanReport(report))
        }
        Request::RunWalkforwardProfile { profile_toml } => {
            let report = json!({ "label": profile_toml }).to_string();
            (profile_toml, Response::WalkforwardReport(report))
        }
        other => panic!("the fake backtesthub serves the three run verbs only, got {other:?}"),
    };
    let _ = seen.send(Seen::Request(label.clone()));
    if label.starts_with("answer") {
        let _ = write_frame(&mut stream, &answer);
    } else if label.starts_with("gated") {
        let opened = gate.lock().expect("gate").recv_timeout(WAIT).is_ok();
        if opened {
            let _ = write_frame(&mut stream, &answer);
        }
    }
    // Hold (or linger after answering) until the peer closes: a read of 0 bytes, or an error.
    let mut buf = [0u8; 256];
    while matches!(stream.read(&mut buf), Ok(n) if n > 0) {}
    let _ = seen.send(Seen::PeerClosed(label));
}

/// A reader fed one line per `send` — EOF when the sender is dropped.
struct ChannelLines {
    rx: mpsc::Receiver<String>,
    pending: Vec<u8>,
    pos: usize,
}

impl Read for ChannelLines {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.pending.len() {
            match self.rx.recv() {
                Ok(line) => {
                    self.pending = format!("{line}\n").into_bytes();
                    self.pos = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.pending.len() - self.pos);
        buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// A writer that parses every complete line the server writes and hands it to the test.
struct ChannelWriter {
    tx: mpsc::Sender<Value>,
    buf: Vec<u8>,
}

impl Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        while let Some(end) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=end).collect();
            let text = String::from_utf8(line).expect("the server writes UTF-8");
            if !text.trim().is_empty() {
                let msg = serde_json::from_str(&text).expect("each line is one JSON-RPC message");
                let _ = self.tx.send(msg);
            }
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// One `serve` session on its own thread, fed and read by the test.
pub(crate) struct Session {
    input: Option<mpsc::Sender<String>>,
    output: mpsc::Receiver<Value>,
    done: mpsc::Receiver<io::Result<()>>,
}

impl Session {
    /// Start `serve` over a server whose backtesthub is `backtest_addr`. The server is BUILT on
    /// the serve thread, so this harness asks nothing of `Server` beyond what `serve` does.
    fn start(backtest_addr: SocketAddr) -> Self {
        Self::start_with(move || Server {
            backtest_addr: backtest_addr.to_string(),
            ..test_server()
        })
    }

    pub(crate) fn start_with(build: impl FnOnce() -> Server + Send + 'static) -> Self {
        let (input, rx) = mpsc::channel::<String>();
        let (tx, output) = mpsc::channel::<Value>();
        let (done_tx, done) = mpsc::channel();
        thread::spawn(move || {
            let mut server = build();
            let reader = BufReader::new(ChannelLines { rx, pending: Vec::new(), pos: 0 });
            let writer = ChannelWriter { tx, buf: Vec::new() };
            let _ = done_tx.send(serve(reader, writer, &mut server));
        });
        Self { input: Some(input), output, done }
    }

    pub(crate) fn send(&self, msg: Value) {
        self.input.as_ref().expect("input still open").send(msg.to_string()).expect("serve reads");
    }

    pub(crate) fn next_response(&self) -> Value {
        self.output.recv_timeout(WAIT).expect("the server answers within the wait")
    }

    /// Close the input (EOF), wait for `serve` to RETURN, and collect every response not yet read.
    /// A `serve` that is still blocked when the wait runs out FAILS the case — that is the old
    /// synchronous loop, parked inside a daemon tool, never reading the cancel.
    pub(crate) fn finish(mut self) -> Vec<Value> {
        self.input = None;
        match self.done.recv_timeout(WAIT) {
            Ok(result) => result.expect("serve returns without an io error"),
            Err(_) => panic!(
                "serve did not return after EOF: the loop is still blocked inside a daemon tool \
                 call, so it never read the cancel or the requests behind it"
            ),
        }
        self.output.try_iter().collect()
    }
}

pub(crate) fn line(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn tool_call(id: i64, tool: &str, profile: &str) -> Value {
    line(id, "tools/call", json!({ "name": tool, "arguments": { "profile": profile } }))
}

pub(crate) fn cancelled(request_id: i64) -> Value {
    json!({
        "jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": { "requestId": request_id, "reason": "the user pressed stop" }
    })
}

fn ids(responses: &[Value]) -> Vec<Value> {
    responses.iter().map(|r| r["id"].clone()).collect()
}

/// **THE PROPERTY.** A `run_sweep` the daemon is computing is cancelled: the server sends no
/// response for it, answers the request behind it, and CLOSES the daemon socket (so the daemon can
/// notice its client is gone and stop).
///
/// On the old synchronous loop this fails at [`Session::finish`]: `serve` sat inside id 7's
/// `read_frame` for as long as the daemon computed, so the cancel and id 8 were never read.
#[test]
fn a_cancelled_paramscan_gets_no_response_and_the_next_request_is_answered() {
    let daemon = fake_daemon();
    let session = Session::start(daemon.addr);

    session.send(line(1, "initialize", json!({})));
    assert_eq!(session.next_response()["id"], json!(1));
    session.send(tool_call(7, "run_sweep", "hold-the-sweep"));
    assert_eq!(daemon.next_seen(), Seen::Request("hold-the-sweep".to_string()));

    session.send(cancelled(7));
    session.send(line(8, "tools/list", json!({})));
    let rest = session.finish();

    assert_eq!(ids(&rest), vec![json!(8)], "exactly id 8 after initialize, none for 7: {rest:?}");
    assert!(rest[0]["result"]["tools"].is_array(), "id 8 is the tools/list answer: {rest:?}");
    assert_eq!(
        daemon.next_seen(),
        Seen::PeerClosed("hold-the-sweep".to_string()),
        "the cancel closed the daemon socket, so the daemon can stop computing"
    );
}

/// Review focus (a): a cancel naming an id nobody knows, or a daemon tool that already ANSWERED,
/// writes nothing and breaks nothing — the next request is answered as usual.
#[test]
fn a_cancel_for_an_unknown_or_finished_id_is_ignored() {
    let daemon = fake_daemon();
    let session = Session::start(daemon.addr);

    session.send(line(1, "initialize", json!({})));
    assert_eq!(session.next_response()["id"], json!(1));
    session.send(cancelled(99));
    session.send(tool_call(2, "run_sweep", "answer-at-once"));
    let answered = session.next_response();
    assert_eq!(answered["id"], json!(2), "{answered}");
    assert_eq!(answered["result"]["isError"], json!(false), "{answered}");
    assert_eq!(
        answered["result"]["structuredContent"]["sweep"]["label"],
        json!("answer-at-once"),
        "a deferred tool answers in the shape the inline one did: {answered}"
    );
    session.send(cancelled(2));
    session.send(line(3, "ping", json!({})));
    assert_eq!(session.next_response()["id"], json!(3));

    let rest = session.finish();
    assert!(rest.is_empty(), "neither cancel produced a line: {rest:?}");
}

/// Review focus (b): a cancel naming an INLINE request (answered before the cancel was read) is
/// ignored — inline requests never enter the in-flight table.
#[test]
fn a_cancel_for_an_inline_request_is_ignored() {
    let session = Session::start_with(test_server);
    session.send(tool_call(4, "list_templates", ""));
    assert_eq!(session.next_response()["id"], json!(4));
    session.send(cancelled(4));
    session.send(line(5, "tools/list", json!({})));
    assert_eq!(session.next_response()["id"], json!(5));
    assert!(session.finish().is_empty());
}

/// Review focus (c): input that ENDS while a daemon tool is in flight still gets that tool's
/// response — the transcript tests send their input and then EOF, and so does any client that pipes
/// a file in. `serve` must not return before the answer is written.
#[test]
fn eof_while_a_daemon_tool_is_in_flight_still_delivers_its_response() {
    let daemon = fake_daemon();
    let mut session = Session::start(daemon.addr);

    session.send(tool_call(2, "run_walk_forward", "gated-walk"));
    assert_eq!(daemon.next_seen(), Seen::Request("gated-walk".to_string()));
    session.input = None; // EOF, with id 2 still computing
    assert!(
        session.done.recv_timeout(Duration::from_millis(300)).is_err(),
        "serve returned at EOF with a daemon tool still in flight"
    );

    daemon.open_gate();
    let rest = session.finish();
    assert_eq!(ids(&rest), vec![json!(2)], "{rest:?}");
    assert_eq!(rest[0]["result"]["structuredContent"]["walkforward"]["label"], json!("gated-walk"));
}

/// The bound on that promise: a run whose daemon link died has a reply read with no end, and the
/// client that closed its input is gone. So past `Server::eof_deadline` the session ends, loudly, and
/// no process is left behind holding it.
#[test]
fn a_run_with_no_answer_cannot_keep_the_process_past_the_eof_deadline() {
    let daemon = fake_daemon();
    let addr = daemon.addr;
    let mut session = Session::start_with(move || Server {
        backtest_addr: addr.to_string(),
        eof_deadline: Duration::from_millis(300),
        ..test_server()
    });
    session.send(tool_call(2, "run_walk_forward", "hold-dead-link"));
    assert_eq!(daemon.next_seen(), Seen::Request("hold-dead-link".to_string()));
    session.input = None;
    let ended = session.done.recv_timeout(WAIT).expect("serve returned past the deadline");
    let why = ended.expect_err("giving up on a run is not a clean end");
    assert_eq!(why.kind(), io::ErrorKind::TimedOut, "{why}");
    assert!(session.output.try_recv().is_err(), "the run never answered, so nothing was written");
}

/// Review focus (d): the cancel arrives BEFORE the worker has a connection (the daemon has not even
/// answered the `Hello`). No response, no hang — and the request is NEVER SENT: the worker's late
/// registration is refused and its fresh connection closed with nothing written after the `Hello`.
#[test]
fn a_cancel_before_the_worker_connects_sends_nothing_and_answers_nothing() {
    let daemon = fake_daemon_with_gated_handshake();
    let session = Session::start(daemon.addr);

    session.send(tool_call(2, "run_sweep", "hold-never-sent"));
    session.send(cancelled(2));
    session.send(line(3, "ping", json!({})));
    assert_eq!(session.next_response()["id"], json!(3));
    let rest = session.finish();
    assert!(rest.is_empty(), "the cancelled call got no response: {rest:?}");

    // Only now does the daemon answer the handshake: the worker connects, finds its token
    // cancelled, and drops the client without sending the request.
    daemon.open_handshake();
    assert_eq!(daemon.next_seen(), Seen::PeerClosed(BEFORE_REQUEST.to_string()));
    daemon.assert_quiet();
}

/// Review focus (e): two daemon tools in flight, one cancelled — the other still answers, and its
/// report still reaches `vike://backtest/last` (the loop, not the worker, owns that state).
#[test]
fn cancelling_one_of_two_daemon_tools_leaves_the_other_answering() {
    let daemon = fake_daemon();
    let session = Session::start(daemon.addr);

    session.send(tool_call(2, "run_sweep", "hold-the-sweep"));
    session.send(tool_call(3, "run_backtest", "gated-backtest"));
    let mut requests = vec![daemon.next_seen(), daemon.next_seen()];
    requests.sort_by_key(|s| format!("{s:?}"));
    assert_eq!(
        requests,
        vec![
            Seen::Request("gated-backtest".to_string()),
            Seen::Request("hold-the-sweep".to_string())
        ]
    );

    session.send(cancelled(2));
    assert_eq!(daemon.next_seen(), Seen::PeerClosed("hold-the-sweep".to_string()));
    daemon.open_gate();
    let answered = session.next_response();
    assert_eq!(answered["id"], json!(3), "{answered}");
    assert_eq!(answered["result"]["structuredContent"]["report"]["label"], json!("gated-backtest"));

    session.send(line(4, "resources/read", json!({ "uri": "vike://backtest/last" })));
    let last = session.next_response();
    let text = last["result"]["contents"][0]["text"].as_str().expect("the resource is served");
    assert!(text.contains("gated-backtest"), "the deferred report became the last one: {last}");

    let rest = session.finish();
    assert!(rest.is_empty(), "nothing for the cancelled id 2: {rest:?}");
}

/// A cancelled call gets no response, so the transcript (`--trace`) is the only place its end is
/// written: one record, an `error` whose detail says the client cancelled it.
#[test]
fn a_cancelled_call_is_recorded_in_the_transcript() {
    let daemon = fake_daemon();
    let dir = tempfile::Builder::new().prefix("vike-mcp-cancel").tempdir().expect("scratch");
    let trace_dir = dir.path().to_path_buf();
    let addr = daemon.addr;
    let session = Session::start_with(move || Server {
        backtest_addr: addr.to_string(),
        trace: Some(McpTrace::new(trace_dir)),
        ..test_server()
    });

    session.send(tool_call(7, "run_sweep", "hold-traced"));
    assert_eq!(daemon.next_seen(), Seen::Request("hold-traced".to_string()));
    session.send(cancelled(7));
    assert_eq!(daemon.next_seen(), Seen::PeerClosed("hold-traced".to_string()));
    // The worker's `Done` reaches the loop after the socket closed; a ping behind it, answered,
    // does not order the two — so wait for the record itself.
    let mut records = Vec::new();
    for _ in 0..100 {
        records = transcript_records(dir.path());
        if !records.is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(session.finish().is_empty());

    assert_eq!(records.len(), 1, "one call, one record: {records:?}");
    assert_eq!(records[0]["tool"], json!("run_sweep"));
    assert_eq!(records[0]["verdict"], json!("error"));
    assert_eq!(records[0]["detail"], json!(super::super::protocol::CANCELLED_DETAIL));
}

/// Every transcript record under `dir`, parsed.
fn transcript_records(dir: &std::path::Path) -> Vec<Value> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("the transcript directory exists").flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a readable record file");
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            out.push(serde_json::from_str(line).expect("one JSON object per line"));
        }
    }
    out
}

/// A panic in a tool, over the real transport: one `isError` answer, and the session goes on. The
/// guard is `Server::handle`'s `catch_unwind`; what this holds is that the SDK's path still reaches it.
#[test]
fn a_panicking_tool_gets_one_answer_and_the_session_goes_on() {
    let session = Session::start_with(test_server);
    session.send(line(1, "tools/call", json!({ "name": "__test_panic", "arguments": {} })));
    let answered = session.next_response();
    assert_eq!(answered["id"], json!(1), "{answered}");
    assert_eq!(answered["result"]["isError"], json!(true), "{answered}");
    let text = answered["result"]["content"][0]["text"].as_str().unwrap_or_default();
    assert!(text.contains("panicked"), "{answered}");
    session.send(line(2, "ping", json!({})));
    assert_eq!(session.next_response()["id"], json!(2));
    assert!(session.finish().is_empty(), "one answer each");
}

/// A panic in an SDK handler task, past `Server::handle`'s `catch_unwind`: one `-32603` under the
/// client's id, the line behind it is read, and EOF still ends the session. Unguarded, the SDK drops
/// the task's `JoinHandle`, so the client waits for that answer for ever and `Wire` reads no more.
#[test]
fn a_panic_on_the_sdks_path_gets_one_answer_and_the_session_goes_on() {
    let session = Session::start_with(test_server);
    session.send(line(1, "__test_sdk_panic", json!({})));
    let answered = session.next_response();
    assert_eq!(answered["id"], json!(1), "{answered}");
    assert_eq!(answered["error"]["code"], json!(-32603), "{answered}");
    let message = answered["error"]["message"].as_str().unwrap_or_default();
    assert!(message.contains("panic") && message.contains("write"), "{answered}");
    session.send(line(2, "ping", json!({})));
    assert_eq!(session.next_response()["id"], json!(2));
    assert!(session.finish().is_empty(), "one answer each");
}

/// A panic in the SDK's own loop, which no request owns: `serve` returns the failure, which is what
/// `run` turns into a failing exit, so the client sees the server go rather than wait for it.
#[test]
fn a_panic_in_the_sdks_own_loop_ends_the_session_with_a_failure() {
    let session = Session::start_with(test_server);
    session.send(line(1, "__test_loop_panic", json!({})));
    let ended = session.done.recv_timeout(WAIT).expect("`serve` returns without waiting for EOF");
    assert!(ended.is_err(), "the session ended as if nothing had happened");
}

/// An id REUSED after its request was cancelled belongs to the new request. The cancelled worker's
/// late `Done` must neither answer the new request nor unregister it — so the new request gets
/// exactly its own answer.
#[test]
fn a_late_done_from_a_cancelled_worker_does_not_answer_a_reused_id() {
    let daemon = fake_daemon();
    let session = Session::start(daemon.addr);

    session.send(tool_call(7, "run_sweep", "hold-first"));
    assert_eq!(daemon.next_seen(), Seen::Request("hold-first".to_string()));
    session.send(cancelled(7));
    session.send(tool_call(7, "run_sweep", "gated-second"));
    let mut seen = vec![daemon.next_seen(), daemon.next_seen()];
    seen.sort_by_key(|s| format!("{s:?}"));
    assert_eq!(
        seen,
        vec![Seen::PeerClosed("hold-first".to_string()), Seen::Request("gated-second".to_string())]
    );
    // The first worker's `Done` follows its socket error by a thread wake-up; let it land before
    // the second request can finish, which is the order that used to lose the second answer.
    thread::sleep(Duration::from_millis(300));

    daemon.open_gate();
    let answered = session.next_response();
    assert_eq!(answered["id"], json!(7), "{answered}");
    assert_eq!(answered["result"]["structuredContent"]["sweep"]["label"], json!("gated-second"));
    assert!(session.finish().is_empty(), "one answer for the reused id, and only one");
}

// ---- background runs: `background: true`, `run_status`, `cancel_run` (`crate::background_runs`) ----------------

fn background_call(id: i64, tool: &str, profile: &str) -> Value {
    let arguments = json!({ "profile": profile, "background": true });
    line(id, "tools/call", json!({ "name": tool, "arguments": arguments }))
}

fn run_call(id: i64, tool: &str, run_id: &Value) -> Value {
    line(id, "tools/call", json!({ "name": tool, "arguments": { "run_id": run_id } }))
}

/// `run_status`, asked again until the run is no longer running (or the wait ran out).
fn status_once_ended(session: &Session, id: i64, run_id: &Value) -> Value {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        session.send(run_call(id, "run_status", run_id));
        let status = session.next_response()["result"]["structuredContent"].clone();
        if status["state"] != json!("running") || std::time::Instant::now() > deadline {
            return status;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// **THE PROPERTY of a long run**: `background: true` answers a run id while the daemon is still computing,
/// `run_status` says `running` until the daemon answers, then `finished` with exactly the foreground answer.
#[test]
fn a_background_run_answers_a_run_id_at_once_and_run_status_reads_its_answer() {
    let daemon = fake_daemon();
    let session = Session::start(daemon.addr);

    session.send(background_call(2, "run_backtest", "gated-background"));
    let started = session.next_response()["result"]["structuredContent"].clone();
    assert_eq!(started["state"], json!("running"), "{started}");
    assert_eq!(started["tool"], json!("run_backtest"), "{started}");
    let run_id = started["run_id"].clone();
    assert_eq!(daemon.next_seen(), Seen::Request("gated-background".to_string()));
    session.send(run_call(3, "run_status", &run_id));
    let running = session.next_response()["result"]["structuredContent"].clone();
    assert_eq!(
        (running["run_id"].clone(), running["state"].clone()),
        (run_id.clone(), json!("running"))
    );

    daemon.open_gate();
    let ended = status_once_ended(&session, 4, &run_id);
    assert_eq!(ended["state"], json!("finished"), "{ended}");
    assert_eq!(ended["answer"], json!({ "report": { "label": "gated-background" } }), "{ended}");
    assert!(session.finish().is_empty(), "one answer each");
}

/// `cancel_run` closes the run's daemon socket and answers it `cancelled`; `run_status` still reads it, and a second
/// cancel is refused by id.
#[test]
fn cancel_run_closes_the_daemon_socket_and_an_ended_run_is_refused_by_id() {
    let daemon = fake_daemon();
    let session = Session::start(daemon.addr);

    session.send(background_call(2, "run_sweep", "hold-background"));
    let run_id = session.next_response()["result"]["structuredContent"]["run_id"].clone();
    assert_eq!(daemon.next_seen(), Seen::Request("hold-background".to_string()));
    session.send(run_call(3, "cancel_run", &run_id));
    let stopped = session.next_response()["result"]["structuredContent"].clone();
    assert_eq!(stopped["state"], json!("cancelled"), "{stopped}");
    assert_eq!(daemon.next_seen(), Seen::PeerClosed("hold-background".to_string()));

    session.send(run_call(4, "run_status", &run_id));
    let status = session.next_response()["result"]["structuredContent"].clone();
    assert_eq!(status["state"], json!("cancelled"), "{status}");
    session.send(run_call(5, "cancel_run", &run_id));
    let again = session.next_response();
    assert_eq!(again["result"]["isError"], json!(true), "{again}");
    let text = again["result"]["content"][0]["text"].as_str().unwrap_or_default();
    assert!(text.starts_with("error [mcp.run.already_ended]: "), "{text}");
    assert!(session.finish().is_empty(), "one answer each");
}

/// At most `MAX_RUNNING` at once: one more is refused by id and reaches no daemon. The server's end then stops the
/// ones running.
#[test]
fn a_background_run_past_the_limit_is_refused_and_sends_nothing() {
    let limit = crate::background_runs::MAX_RUNNING;
    let daemon = fake_daemon();
    let session = Session::start(daemon.addr);

    for (id, n) in (10_i64..).zip(0..limit) {
        session.send(background_call(id, "run_walk_forward", &format!("hold-{n}")));
        assert_eq!(session.next_response()["result"]["isError"], json!(false));
    }
    let mut seen: Vec<Seen> = (0..limit).map(|_| daemon.next_seen()).collect();
    seen.sort_by_key(|s| format!("{s:?}"));
    let held: Vec<Seen> = (0..limit).map(|n| Seen::Request(format!("hold-{n}"))).collect();
    assert_eq!(seen, held);
    session.send(background_call(20, "run_walk_forward", "hold-one-too-many"));
    let refused = session.next_response();
    let text = refused["result"]["content"][0]["text"].as_str().unwrap_or_default();
    assert!(text.starts_with("error [mcp.run.too_many_running]: "), "{refused}");
    daemon.assert_quiet();

    assert!(session.finish().is_empty(), "one answer each");
    let closed = (0..limit).filter(|_| matches!(daemon.next_seen(), Seen::PeerClosed(_))).count();
    assert_eq!(closed, limit, "the server's end closed every run's daemon socket");
}

/// EOF waits for no background run (each was answered, by its run id), and the server's end stops it: its daemon
/// socket closes, so the daemon stops computing for a client that is gone.
#[test]
fn eof_cancels_every_background_run() {
    let daemon = fake_daemon();
    let session = Session::start(daemon.addr);

    session.send(background_call(2, "run_backtest", "hold-past-eof"));
    assert_eq!(session.next_response()["result"]["structuredContent"]["state"], json!("running"));
    assert_eq!(daemon.next_seen(), Seen::Request("hold-past-eof".to_string()));
    assert!(
        session.finish().is_empty(),
        "serve returned at EOF without waiting for the background run"
    );
    assert_eq!(daemon.next_seen(), Seen::PeerClosed("hold-past-eof".to_string()));
}

/// What needs no daemon, each refusal by its id: no run named (absent or no text), a run this process never started,
/// a `background` that is no boolean (the foreground call's, before it dials), and what the tool refuses before it
/// dials, answered at once with nothing started.
#[test]
fn a_background_call_that_cannot_run_is_refused_at_once() {
    let text = |answer: Value| answer["result"]["content"][0]["text"].as_str().map(str::to_string);
    for tool in ["run_status", "cancel_run"] {
        let no_run = format!(
            "error [mcp.run.no_run_id]: {tool} requires a `run_id` string argument: the id a `background: true` \
             call answered"
        );
        assert_eq!(text(call(tool, json!({}))), Some(no_run.clone()));
        assert_eq!(text(call(tool, json!({ "run_id": 7 }))), Some(no_run));
        let unknown = text(call(tool, json!({ "run_id": "run-0-1" }))).unwrap_or_default();
        assert!(
            unknown.starts_with("error [mcp.run.unknown]: no background run \"run-0-1\""),
            "{unknown}"
        );
    }
    assert_eq!(
        text(call("run_backtest", json!({ "profile": "x", "background": "yes" }))),
        Some(
            "error [mcp.run.background_not_a_boolean]: `background` must be true or false, got \"yes\""
                .to_string()
        )
    );
    assert_eq!(
        text(call("run_sweep", json!({ "background": true }))),
        Some(
            "run_sweep requires a `profile` string argument (a backtest profile TOML)".to_string()
        )
    );
}

/// A run id carries its process's start, so a later process's first run is not an earlier one's: an agent asking
/// after a restart is told its run is unknown rather than handed another run's answer.
#[test]
fn a_later_process_mints_other_run_ids() {
    let first_run = || {
        let start =
            json!({ "name": "run_backtest", "arguments": { "profile": "x", "background": true } });
        let answer = server().handle(&line(1, "tools/call", start)).expect("a request is answered");
        answer["result"]["structuredContent"]["run_id"].clone()
    };
    let earlier = first_run();
    thread::sleep(Duration::from_millis(5));
    let later = first_run();
    assert!(earlier.is_string() && later.is_string(), "{earlier} {later}");
    assert_ne!(earlier, later);
}
