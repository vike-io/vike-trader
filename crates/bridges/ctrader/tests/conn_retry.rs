//! Bounded connect-retry integration tests: the INITIAL connect+auth rides out a TRANSIENT
//! network/TLS blip during startup (so the venue is not permanently dropped to paper) but fails
//! FAST on a PERMANENT auth rejection (retrying a bad credential only delays the paper fallback).
//! Both run network-free against the in-process fake cTrader server (plaintext, no TLS); each uses
//! a deliberately tiny backoff so the retry logic is exercised without a slow test.

mod common;

use std::sync::Arc;
use std::time::Duration;

use vike_ctrader::conn::{connect_and_auth, ConnConfig, ConnError, ConnectRetry};

use common::{FakeCtrader, NoopSink};

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
    // The fake server drops the first TWO connections mid-handshake (transient `ConnError::Io`),
    // then serves the full handshake on the third. With max_attempts=4 the retry must reach it.
    let server = FakeCtrader::start_transient_then_serve(2);
    let mut cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    cfg.connect_retry = fast_retry();

    let handle = connect_and_auth(cfg, Arc::new(NoopSink))
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
    let mut cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    cfg.connect_retry = ConnectRetry {
        max_attempts: 2,
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(20),
    };

    match connect_and_auth(cfg, Arc::new(NoopSink)) {
        Err(ConnError::Io(_)) => {} // the unchanged terminal error after exhausting attempts
        Err(other) => {
            panic!("expected a terminal Io error after exhausting retries, got {other:?}")
        }
        Ok(_) => panic!("expected a terminal Io error, but connect succeeded"),
    }
}

#[test]
fn permanent_auth_rejection_fails_fast_without_burning_retries() {
    // The server rejects APPLICATION_AUTH_REQ with an auth-flavored ERROR_RES (`ConnError::Venue`).
    // Retrying a rejected credential is pointless, so the connect must return IMMEDIATELY — and the
    // server must have seen exactly ONE app-auth attempt, not the full `max_attempts` budget.
    let server = FakeCtrader::start_reject_app_auth();
    let mut cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    cfg.connect_retry = fast_retry(); // 4 attempts available — a permanent fault must use only 1

    match connect_and_auth(cfg, Arc::new(NoopSink)) {
        Err(ConnError::Venue { .. }) => {}
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
