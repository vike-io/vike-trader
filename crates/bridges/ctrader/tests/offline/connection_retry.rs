//! Bounded connect-retry integration tests: the INITIAL connect+auth rides out a TRANSIENT
//! network/TLS blip during startup (so a blip does not leave the account NOT RUNNING) but fails
//! FAST on a PERMANENT auth rejection (retrying a bad credential only delays the failed start).
//! Both run network-free against the in-process fake cTrader server (plaintext, no TLS); each uses
//! a deliberately tiny backoff so the retry logic is exercised without a slow test. One attempt meets a
//! peer that takes the dial and says nothing (`vike_bridge_core::silent_peer`), on the row's real window.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_ctrader::transport::connection::{
    ConnectRetry, ConnectionConfig, ConnectionError, connect_and_auth,
};

use vike_ctrader::test_support::{FakeCtrader, NoopSink};

/// A fast, still-bounded retry policy — same shape as the default, just short sleeps so the test
/// exercises the backoff/attempt machinery in milliseconds rather than seconds.
fn fast_retry() -> ConnectRetry {
    ConnectRetry {
        max_attempts: 4,
        initial_backoff: Duration::from_millis(20),
        max_backoff: Duration::from_millis(50),
    }
}

#[test]
fn rides_out_transient_failures_then_connects() {
    // The fake server drops the first TWO connections mid-handshake (transient `ConnectionError::Io`),
    // then serves the full handshake on the third. With max_attempts=4 the retry must reach it.
    let server = FakeCtrader::start_transient_then_serve(2);
    let mut config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    config.connect_retry = fast_retry();

    let handle = connect_and_auth(config, Arc::new(NoopSink))
        .expect("connect must succeed after 2 transient blips");

    // Proof the retry reached a genuinely-authenticated session, not a half-open socket.
    assert_eq!(handle.ctid, 99);
    assert_eq!(handle.symbols.id_of("EURUSD"), Some(1));
    server.assert_saw(&["APPLICATION_AUTH_REQ", "ACCOUNT_AUTH_REQ", "SYMBOL_BY_ID_REQ"]);

    handle.shutdown();
}

#[test]
fn exhausts_bounded_retries_and_surfaces_the_terminal_error() {
    // EVERY connection is dropped mid-handshake, so the retry can never succeed. With
    // max_attempts=2 the call must give up quickly and surface the terminal transient error
    // (the paper-fallback contract: more chances, but still a bounded terminal failure).
    let server = FakeCtrader::start_transient_then_serve(usize::MAX);
    let mut config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    config.connect_retry = ConnectRetry {
        max_attempts: 2,
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(20),
    };

    match connect_and_auth(config, Arc::new(NoopSink)) {
        Err(ConnectionError::Io(_)) => {} // the unchanged terminal error after exhausting attempts
        Err(other) => {
            panic!("expected a terminal Io error after exhausting retries, got {other:?}")
        }
        Ok(_) => panic!("expected a terminal Io error, but connect succeeded"),
    }
}

/// **The initial connect's waits, pinned verbatim before its doubling moved onto the one backoff** (connection plan
/// B3): the `backoff_ms` each transient failure states before it sleeps. The default policy (4 attempts, 250 ms
/// doubling to 2 s) waits 250, 500 and 1000 ms; six attempts of 20 ms doubling to 50 ms show the ceiling holding.
#[test]
fn the_initial_connects_waits_are_pinned_verbatim() {
    let waits = |policy: ConnectRetry| -> Vec<String> {
        let server = FakeCtrader::start_transient_then_serve(usize::MAX);
        let mut config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
        config.connect_retry = policy;
        let (outcome, events) =
            vike_log::capture::captured(|| connect_and_auth(config, Arc::new(NoopSink)).err());
        assert!(
            matches!(outcome, Some(ConnectionError::Io(_))),
            "every attempt fails: {outcome:?}"
        );
        events
            .iter()
            .filter(|e| e.message.contains("initial connect attempt failed transiently"))
            .filter_map(|e| e.field("backoff_ms").map(str::to_string))
            .collect()
    };
    assert_eq!(waits(ConnectRetry::default()), ["250", "500", "1000"]);
    let ms = Duration::from_millis;
    let six = ConnectRetry { max_attempts: 6, initial_backoff: ms(20), max_backoff: ms(50) };
    assert_eq!(waits(six), ["20", "40", "50", "50", "50"]);
}

#[test]
fn permanent_auth_rejection_fails_fast_without_burning_retries() {
    // The server rejects APPLICATION_AUTH_REQ with an auth-flavored ERROR_RES (`ConnectionError::Venue`).
    // Retrying a rejected credential is pointless, so the connect must return IMMEDIATELY — and the
    // server must have seen exactly ONE app-auth attempt, not the full `max_attempts` budget.
    let server = FakeCtrader::start_reject_app_auth();
    let mut config = ConnectionConfig::for_test(server.addr(), "cid", "secret", "token");
    config.connect_retry = fast_retry(); // 4 attempts available — a permanent fault must use only 1

    match connect_and_auth(config, Arc::new(NoopSink)) {
        Err(ConnectionError::Venue { .. }) => {}
        Err(other) => panic!("expected a permanent Venue auth rejection, got {other:?}"),
        Ok(_) => panic!("expected a permanent Venue auth rejection, but connect succeeded"),
    }

    // The call returned synchronously, so every (would-be) retry has already happened by now: a
    // correct fail-fast leaves exactly one app-auth on the wire; a buggy retry would leave four.
    assert_eq!(
        server.count_seen("APPLICATION_AUTH_REQ"),
        1,
        "a permanent auth rejection must not burn retry attempts"
    );
}

/// **The link harness' S12 on this venue's seam** (`crates/vike-bridge-core/tests/link_conformance/sessions.rs`'s
/// `CTRADER`): the REAL first connect, dialled at a peer that takes the SYN and then says nothing, is cut at the row's
/// `response_timeout`, the wait for the handshake's first reply, and never hangs on it. The dial window
/// (`dial_timeout`) bounds the TCP connect, which a loopback peer answers at once. Plaintext, as `FakeCtrader` speaks
/// (the TLS wrap production adds is not driven). One attempt: every attempt of the connect retry meets the same bound.
#[test]
fn a_peer_that_takes_the_dial_and_says_nothing_is_cut_at_the_rows_reply_window() {
    let window = vike_venues::venue_timings::CTRADER.exec().response_timeout();
    let began = Instant::now();
    let dial = vike_bridge_core::silent_peer::dial_silent_peer("127.0.0.1:0", move |url| {
        let addr = url.trim_start_matches("ws://").parse().expect("the peer's address");
        let mut config = ConnectionConfig::for_test(addr, "cid", "secret", "token");
        config.connect_retry = ConnectRetry { max_attempts: 1, ..ConnectRetry::default() };
        match connect_and_auth(config, Arc::new(NoopSink)) {
            Ok(handle) => {
                handle.shutdown();
                Ok(())
            }
            // The words the silent peer holds a cut handshake to: the first reply never came.
            Err(e @ ConnectionError::Timeout(_)) => {
                Err(format!("no answer within {window:?}: {e}"))
            }
            Err(other) => Err(format!("{other:?}")),
        }
    });
    let took = began.elapsed();
    assert!(
        took + Duration::from_millis(500) >= window && took <= window + Duration::from_secs(5),
        "the silent peer was cut after {took:?}; the row's reply window is {window:?} ({})",
        dial.failure
    );
}
