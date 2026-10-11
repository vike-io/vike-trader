//! The stop path, and the shutdown deadline against the unit's `TimeoutStopSec=`.

use super::*;

// -- the stop path: what the stdio control channel does, and what it deliberately does not ----

/// Drive [`control_loop`] over a fixed script, with `send` as given, and report
/// `(stop raised?, protocol output)`: the bytes the loop wrote to its `out`.
fn drive_with(
    input: &str,
    is_tty: bool,
    send: impl FnMut(Command) -> Result<(), vike_exec::CoreGone>,
) -> (bool, String) {
    let stop = AtomicBool::new(false);
    let mut out: Vec<u8> = Vec::new();
    control_loop(std::io::Cursor::new(input.as_bytes()), is_tty, &stop, &mut out, send, || {
        "{}".to_string()
    });
    (stop.load(Ordering::SeqCst), String::from_utf8(out).expect("protocol output is UTF-8"))
}

/// Drive [`control_loop`] over a fixed script and report `(stop raised?, commands accepted)`.
fn drive(input: &str, is_tty: bool) -> (bool, usize) {
    let mut sent = 0usize;
    let (stopped, _out) = drive_with(input, is_tty, |_cmd| {
        sent += 1;
        Ok(())
    });
    (stopped, sent)
}

/// ⚠ **The regression this daemon cannot afford.** Under systemd stdin is `/dev/null`, which
/// reads EOF the instant the daemon starts. If EOF meant "stop", the daemon would exit at
/// startup on every box, every start — so a non-tty EOF must leave the flag DOWN and the daemon
/// trading headless until a signal arrives. `tests/sigterm_stop.rs` proves the same property
/// against the real process; this pins the rule it rests on.
#[test]
fn a_non_tty_eof_does_not_stop_the_daemon() {
    assert_eq!(
        drive("", false),
        (false, 0),
        "a non-tty EOF must NOT raise the stop flag — systemd wires stdin to /dev/null, so this \
             would exit at startup on every service box"
    );
}

/// …and the mirror image, so the rule above is not bought by ignoring EOF entirely: Ctrl-D from
/// a human at a terminal IS an explicit stop.
#[test]
fn a_tty_eof_stops_the_daemon() {
    assert_eq!(drive("", true), (true, 0), "Ctrl-D on a TTY is a stop");
}

/// Every stop word raises the flag on either channel — the word is explicit, so whether the
/// channel is a terminal has nothing to add.
#[test]
fn every_stop_word_raises_the_flag_on_either_channel() {
    for word in ["shutdown", "quit", "exit"] {
        for is_tty in [true, false] {
            assert!(
                drive(&format!("{word}\n"), is_tty).0,
                "`{word}` must stop the daemon (is_tty={is_tty})"
            );
        }
    }
}

/// A JSON command is lowered and the channel keeps running — a stop is a WORD, never a side
/// effect of having been sent something.
#[test]
fn a_command_is_lowered_and_does_not_stop_the_daemon() {
    let req = vike_model::OrderRequest {
        client_order_id: "stdio-1".to_string(),
        venue: "polymarket".to_string(),
        symbol: "TOK".to_string(),
        side: 1,
        qty: 1.0,
        order_type: "limit".to_string(),
        price: Some(0.4),
        ..Default::default()
    };
    let json =
        serde_json::to_string(&Command::Order(vike_exec::OrderIntent::Submit(Box::new(req))))
            .expect("serialize an operator command");
    assert_eq!(
        drive(&format!("{json}\n"), false),
        (false, 1),
        "the command must reach the core lane, and must not be read as a stop"
    );
}

/// Garbage on the control channel is reported, never obeyed: a typo must not stop a daemon that
/// is holding a live book, and it must not be mistaken for a command either.
#[test]
fn junk_neither_stops_the_daemon_nor_reaches_the_core() {
    assert_eq!(drive("\n   \nnot json\nhalt\n", false), (false, 0));
}

/// A well-formed operator command for the reply tests below.
fn one_command_line() -> String {
    let req = vike_model::OrderRequest {
        client_order_id: "stdio-2".to_string(),
        venue: "polymarket".to_string(),
        symbol: "TOK".to_string(),
        side: 1,
        qty: 1.0,
        order_type: "limit".to_string(),
        price: Some(0.4),
        ..Default::default()
    };
    let json =
        serde_json::to_string(&Command::Order(vike_exec::OrderIntent::Submit(Box::new(req))))
            .expect("serialize an operator command");
    format!("{json}\n")
}

/// A command the lane ACCEPTED is acked, and nothing else is written.
#[test]
fn a_delivered_command_is_acked() {
    let (_, out) = drive_with(&one_command_line(), false, |_cmd| Ok(()));
    assert_eq!(out.trim(), r#"{"kind":"ack"}"#);
}

/// ⚠ **The regression this pins.** When the sink's send fails (the core thread has exited, so the
/// ingest lane is closed) the loop used to print `{"kind":"ack"}` all the same: the operator read an
/// ack for an order the core never got. The reply must be an error naming the cause, never an ack,
/// and the loop must keep running (a later stop word still stops it).
#[test]
fn a_command_the_core_never_got_is_an_error_not_an_ack() {
    let mut calls = 0usize;
    let input = format!("{}shutdown\n", one_command_line());
    let (stopped, out) = drive_with(&input, false, |_cmd| {
        calls += 1;
        Err(vike_exec::CoreGone)
    });
    assert_eq!(calls, 1, "the command was offered to the sink exactly once");
    assert!(!out.contains(r#""ack""#), "a failed send must never be acked, got: {out}");
    let line: serde_json::Value =
        serde_json::from_str(out.trim()).expect("the reply is one protocol JSON line");
    assert_eq!(line["kind"], "error", "got: {out}");
    assert_eq!(line["error"], "core_gone", "the reply must name the cause, got: {out}");
    assert!(stopped, "the loop keeps reading after a failed send, so the stop word still lands");
}

/// EVERY shipped unit that starts this daemon, as `(repo-relative path, contents)`.
///
/// ⚠ **THERE IS ONE SINCE 2026-09-16, AND THERE WERE TWO — WHICH IS WHY THIS IS STILL A
/// TABLE.** The second row was the ONE-PROJECT-FOLDER unit the CI box actually ran, and for a while
/// only the first row was read: that file carried its own `TimeoutStopSec=` hand copy, its
/// comment claimed the test below checked it, and nothing did. The unit collapse deleted it —
/// a daemon has ONE unit file now, named without a suffix, and a box's real root is a
/// substitution rather than a second tracked file — so the class of defect this table was
/// widened for cannot currently exist.
///
/// It stays a TABLE rather than collapsing into a lone `include_str!` because the widening was
/// the fix and the shape is the memory of it: a SECOND unit added later must be added here by
/// hand, `include_str!` needing a literal path, and that is the intended cost.
const SHIPPED_UNITS: [(&str, &str); 1] =
    [("deploy/vike-tradehub.service", include_str!("../../../../../deploy/vike-tradehub.service"))];

/// `TimeoutStopSec=` from a unit's text, skipping COMMENTED lines — the unit discusses the
/// directive in prose right above setting it, so a naive prefix match on an untrimmed line
/// would read the commentary and pass on a unit that never sets the directive at all.
fn stop_timeout_secs(path: &str, unit: &str) -> u64 {
    unit.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| l.strip_prefix("TimeoutStopSec="))
        .unwrap_or_else(|| panic!("`{path}` must set TimeoutStopSec= explicitly"))
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("`{path}`: TimeoutStopSec= is a plain number of seconds: {e}"))
}

/// The teardown budget must fit inside EVERY shipped unit's `TimeoutStopSec=`, or systemd
/// SIGKILLs the daemon mid-teardown and the graceful stop buys nothing. Both numbers are READ
/// (the profile default from `DaemonSettings`, the timeout from each unit) rather than restated,
/// so the two cannot drift apart in a later edit.
#[test]
fn the_default_shutdown_deadline_fits_inside_the_units_stop_timeout() {
    let deadline = crate::config::DaemonSettings::default().shutdown_deadline_ms;
    for (path, unit) in SHIPPED_UNITS {
        let timeout = stop_timeout_secs(path, unit);
        // ⚠ ONE step of this daemon's stop is still OUTSIDE `run_with_deadline`: stopping the
        // observe publisher, which closes mailboxes and does not join the detached accept loop,
        // so it is bounded by `publish::POLL_INTERVAL`. STRICT `<` is what leaves room for it.
        //
        // ⚠ This comment used to name TWO such steps and call both "sub-second by
        // construction". That was FALSE of the second one: the summary-thread join sat here
        // with no timeout while the same thread delivers alerts INLINE through a
        // `WebhookSink<UreqTransport>` whose `timeout_global` is 10 s — so a single in-flight
        // alert could blow a `TimeoutStopSec=10` on its own, and this assertion was comparing
        // two numbers while excluding the term that broke the sum. The fix was to move that
        // join INTO `tasks`, where the deadline below actually covers it; the assertion is
        // unchanged, but it is now true. (The sibling recorder's version of this test made the
        // same class of mistake with a 12 s feed-stop prefix and still claimed the flush was
        // safe from SIGKILL. Say what is compared, so the next reader can check the claim
        // rather than inherit it.)
        assert!(
            deadline < timeout * 1_000,
            "the default [daemon] shutdown_deadline_ms ({deadline}) must be strictly under \
                 `{path}`'s TimeoutStopSec={timeout}s — otherwise SIGKILL wins the race and the \
                 teardown is cut in half. This bounds the hard-capped teardown, which now INCLUDES \
                 the summary-thread join; the publisher stop that precedes it is bounded by one \
                 poll interval and rides the difference."
        );
    }
}

/// The loser of a teardown claim waits on the SAME budget the winner's teardown runs under.
///
/// It is one number by construction — `deadline` is resolved once, above the claim, and passed
/// to both `StopSignal::await_teardown` and `run_with_deadline`. This asserts the property that
/// makes that safe: whichever thread ends the process, the stop still fits inside the unit's
/// `TimeoutStopSec=`, because both arms are bounded by the same profile deadline. A loser
/// waiting on a LARGER bound would let a second stop route hold the process open past SIGKILL;
/// one waiting on a smaller bound would exit while the winner was still cancelling.
#[test]
fn a_losing_claim_waits_within_the_same_stop_timeout_the_teardown_does() {
    let deadline = crate::config::DaemonSettings::default().shutdown_deadline_ms;
    for (path, unit) in SHIPPED_UNITS {
        let timeout = stop_timeout_secs(path, unit);
        // The wait the losing arm performs is `stop.await_teardown(deadline)` — the same value.
        assert!(
            deadline < timeout * 1_000,
            "a loser parked for the winner's budget ({deadline} ms) must still be released \
                 before `{path}`'s TimeoutStopSec={timeout}s, or a second stop route turns a \
                 graceful stop into a SIGKILL"
        );
    }
}
