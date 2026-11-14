//! The JSON-RPC layer: malformed lines, unknown methods, notifications, ids, and
//! `notifications/cancelled` for a deferred tool and an inline one.

use serde_json::{Value, json};

use super::harness::{FakeDaemon, Golden, Seen, Session, World, request};

fn cancelled(request_id: impl Into<Value>) -> Value {
    let request_id: Value = request_id.into();
    json!({
        "jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": { "requestId": request_id, "reason": "the user pressed stop" }
    })
}

fn run_call(id: i64, tool: &str, profile: &str) -> Value {
    request(id, "tools/call", json!({ "name": tool, "arguments": { "profile": profile } }))
}

/// A server whose daemon tools dial `daemon`.
fn against(case: &str, daemon: &FakeDaemon) -> Session {
    let world = World::empty().scrub(&daemon.addr, "<daemon-addr>");
    Session::start(case, world, &["--backtest-addr", &daemon.addr])
}

/// Every line that is not a well-formed request: which ones get an answer, and which get nothing.
pub(super) fn protocol_errors() -> Vec<Golden> {
    let mut s = Session::start("protocol.errors", World::empty(), &[]);
    // A parse error answers with a null id, and the session survives it.
    s.send_raw("{ not json");
    s.recv();
    s.send_raw("");
    s.expect_silence();
    s.send_raw("   ");
    s.expect_silence();
    for not_a_message in ["[1,2,3]", "42", "\"text\"", "null"] {
        s.send_raw(not_a_message);
        s.expect_silence();
    }
    s.call(request(1, "no/such/method", json!({})));
    s.send(json!({ "jsonrpc": "2.0", "method": "no/such/method" }));
    s.expect_silence();
    s.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    s.expect_silence();
    s.send(cancelled(99));
    s.expect_silence();
    s.send(cancelled("no-such-id"));
    s.expect_silence();
    s.send(json!({ "jsonrpc": "2.0", "method": "notifications/cancelled" }));
    s.expect_silence();
    // A request with no id is a notification, whatever its method: nothing runs and nothing answers.
    s.send(json!({ "jsonrpc": "2.0", "method": "initialize", "params": {} }));
    s.expect_silence();
    s.send(json!({
        "jsonrpc": "2.0", "method": "tools/call",
        "params": { "name": "validate_strategy", "arguments": { "script": "fn on_bar() { }" } }
    }));
    s.expect_silence();
    s.send(json!({
        "jsonrpc": "2.0", "method": "tools/call",
        "params": { "name": "run_backtest", "arguments": { "profile": "x" } }
    }));
    s.expect_silence();
    s.send(json!({ "jsonrpc": "2.0", "id": 2, "method": 5 }));
    s.expect_silence();
    s.send(json!({ "jsonrpc": "2.0", "id": 3 }));
    s.expect_silence();
    // What an id may be, and what is not checked.
    s.call(json!({ "jsonrpc": "2.0", "id": "a-string-id", "method": "ping" }));
    s.call(json!({ "jsonrpc": "2.0", "id": null, "method": "ping" }));
    s.call(json!({ "jsonrpc": "2.0", "id": 1.5, "method": "ping" }));
    s.call(json!({ "jsonrpc": "2.0", "id": {"an": "object"}, "method": "ping" }));
    s.call(json!({ "id": 4, "method": "ping" }));
    s.call(json!({ "jsonrpc": "1.0", "id": 5, "method": "ping" }));
    // Two requests in one write: one line, one answer, in order.
    s.send_raw(concat!(
        r#"{"jsonrpc":"2.0","id":6,"method":"ping"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#
    ));
    s.recv();
    s.recv();
    vec![s.finish().what("the lines that are not a well-formed request").golden()]
}

/// A line that is not UTF-8 is not a parse error: it ends the session. Pinned as it is.
pub(super) fn invalid_utf8_line() -> Vec<Golden> {
    let mut s = Session::start("protocol.invalid_utf8", World::empty(), &[]);
    s.call(request(1, "ping", json!({})));
    s.send_bytes(&[0xff, 0xfe, 0x7b, 0x7d]);
    vec![s.finish().what("a line that is not UTF-8 ends the session, with a failing exit").golden()]
}

/// A cancel naming an INLINE request: already answered, so it does nothing. The ids are reused on
/// purpose, because nothing here remembers a finished id.
pub(super) fn cancel_inline() -> Vec<Golden> {
    let mut s = Session::start("protocol.cancel_inline", World::empty(), &[]);
    s.call(run_inline_call(4));
    s.send(cancelled(4));
    s.expect_silence();
    s.call(run_inline_call(4));
    s.call(request(4, "ping", json!({})));
    vec![
        s.finish()
            .what("cancel for an inline tool is a no-op, and a finished id may be reused")
            .golden(),
    ]
}

fn run_inline_call(id: i64) -> Value {
    request(
        id,
        "tools/call",
        json!({ "name": "validate_strategy", "arguments": { "script": "fn on_bar() { }" } }),
    )
}

/// A cancel naming a daemon RUN in flight: no answer for it, its daemon socket closed, the next
/// request answered. All three run verbs.
pub(super) fn cancel_deferred() -> Vec<Golden> {
    ["run_backtest", "run_sweep", "run_walk_forward"]
        .into_iter()
        .map(|tool| {
            let daemon = FakeDaemon::keyless();
            let mut s = against(&format!("protocol.cancel_deferred.{tool}"), &daemon);
            s.send(run_call(7, tool, "hold-it"));
            assert_eq!(daemon.next_seen(), Seen::Request("hold-it".to_string()));
            s.note("the daemon holds the request");
            s.send(cancelled(7));
            assert_eq!(daemon.next_seen(), Seen::PeerClosed("hold-it".to_string()));
            s.note("the daemon saw its peer close");
            s.expect_silence();
            s.finish().what("cancel for a deferred tool: no answer, its socket closed").golden()
        })
        .collect()
}

/// An id still in flight, refused for a second run; the same id on an INLINE request, which is not
/// checked and is answered; a cancel; the id reused for a new run, which gets its one answer.
pub(super) fn id_in_flight_and_reused() -> Vec<Golden> {
    let daemon = FakeDaemon::keyless();
    let mut s = against("protocol.id_in_flight", &daemon);
    s.send(run_call(7, "run_sweep", "hold-first"));
    assert_eq!(daemon.next_seen(), Seen::Request("hold-first".to_string()));
    s.note("the daemon holds the first request");
    s.call(run_call(7, "run_sweep", "answer-refused"));
    s.call(run_call(7, "run_backtest", "answer-refused-too"));
    s.call(request(7, "ping", json!({})));
    // A number and a string are two ids.
    s.send(request(
        "7",
        "tools/call",
        json!({ "name": "run_sweep", "arguments": { "profile": "answer-string-id" } }),
    ));
    assert_eq!(daemon.next_seen(), Seen::Request("answer-string-id".to_string()));
    s.recv();
    assert_eq!(daemon.next_seen(), Seen::PeerClosed("answer-string-id".to_string()));
    s.send(cancelled(7));
    assert_eq!(daemon.next_seen(), Seen::PeerClosed("hold-first".to_string()));
    s.note("the daemon saw the first request's peer close");
    s.send(run_call(7, "run_sweep", "answer-second"));
    s.recv();
    s.expect_silence();
    vec![s.finish().what("an id in flight, and the same id reused after its cancel").golden()]
}

/// Input that ends while a run is in flight still gets that run's answer.
pub(super) fn eof_with_a_run_in_flight() -> Vec<Golden> {
    let daemon = FakeDaemon::keyless();
    let mut s = against("protocol.eof_in_flight", &daemon);
    s.send(run_call(2, "run_walk_forward", "gated-walk"));
    assert_eq!(daemon.next_seen(), Seen::Request("gated-walk".to_string()));
    s.close_stdin();
    s.note("stdin closed with the run in flight");
    daemon.open_gate();
    vec![s.finish().what("EOF with a run in flight still delivers its answer, then exits").golden()]
}

/// A run that answers, and the resource that is a second way in to its report.
pub(super) fn last_backtest_resource() -> Vec<Golden> {
    let daemon = FakeDaemon::keyless();
    let mut s = against("protocol.last_backtest", &daemon);
    s.send(run_call(1, "run_backtest", "answer-backtest"));
    s.recv();
    s.call(request(2, "resources/read", json!({ "uri": "vike://backtest/last" })));
    s.send(run_call(3, "run_sweep", "answer-sweep"));
    s.recv();
    s.call(request(4, "resources/read", json!({ "uri": "vike://backtest/last" })));
    vec![s.finish().what("a deferred run's answer, and `vike://backtest/last` after it").golden()]
}
