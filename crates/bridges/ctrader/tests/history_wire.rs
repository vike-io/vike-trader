//! The candle-history source over the LIVE wire (`vike_ctrader::conn::LiveHistoryWire`) against the
//! in-process fake server: the real handshake, framing, request and answer decoding, with no venue. What
//! the scripted suite (`tests/offline/klines.rs`) cannot reach: that one source opens ONE socket and
//! asks every window over it, that an unknown symbol is refused from the handshake's own list before any
//! trendbar request leaves, and that the process pace really admits four requests in a second and no
//! more.
//!
//! The fake answers every `GET_TRENDBARS_REQ` with its two scripted bars (minutes
//! `SEED_BAR_OLDER_MINUTES` and `SEED_BAR_NEWER_MINUTES`) whatever the window, which is what lets the
//! second test see the source's own clipping.
//!
//! The session permit is PROCESS-wide, so the tests that hold or wait for it assume one process a test
//! (nextest, the repo's runner): under plain `cargo test`'s threads another test's open session can make
//! them wait or refuse.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_ctrader::conn::{ConnConfig, LiveHistoryWire};
use vike_ctrader::klines::{
    CtraderKlines, HistoryConfigError, HistoryConfigProvider, HistoryWire, WireError,
};
use vike_ctrader::proto::pt;
use vike_data::source::{KlineSource, SourceError};

use common::{FakeCtrader, SEED_BAR_NEWER_MINUTES, SEED_BAR_OLDER_MINUTES, TrendbarsFault};

const MIN: i64 = 60_000;

fn provider(server: &FakeCtrader) -> HistoryConfigProvider {
    let addr = server.addr();
    Arc::new(move || Ok(ConnConfig::for_test(addr, "cid", "secret", "token")))
}

#[test]
fn one_socket_serves_every_window_and_the_bars_are_decoded_and_clipped() {
    let server = FakeCtrader::start_scripted();
    let source = CtraderKlines::new(provider(&server));
    let older = i64::from(SEED_BAR_OLDER_MINUTES) * MIN;
    let newer = i64::from(SEED_BAR_NEWER_MINUTES) * MIN;

    let both = source.fetch("EURUSD", "1m", older, newer).expect("fetch");
    assert_eq!(both.len(), 2);
    assert_eq!((both[0].ts, both[1].ts), (older, newer));
    // Older: low 113900, open +5, close +12, high +20; newer: low 113950, open +3, close +25, high +30.
    assert_eq!(
        (both[0].open, both[0].high, both[0].low, both[0].close, both[0].volume),
        (1.13905, 1.1392, 1.139, 1.13912, 42.0)
    );
    assert_eq!(
        (both[1].open, both[1].high, both[1].low, both[1].close, both[1].volume),
        (1.13953, 1.1398, 1.1395, 1.13975, 17.0)
    );

    // The fake sends both bars again whatever the window: the source keeps the one inside it.
    let only_older = source.fetch("EURUSD", "1m", older, older).expect("fetch");
    assert_eq!(only_older.len(), 1);
    assert_eq!(only_older[0].ts, older);

    assert_eq!(server.count_seen("GET_TRENDBARS_REQ"), 2);
    assert_eq!(server.count_seen("APPLICATION_AUTH_REQ"), 1, "one session, not one per window");
}

#[test]
fn an_unknown_symbol_is_refused_from_the_handshake_list_before_any_trendbar_request() {
    let server = FakeCtrader::start_scripted();
    let source = CtraderKlines::new(provider(&server));
    let err = source.fetch("NOSUCH", "1m", 0, MIN).expect_err("refused");
    assert!(matches!(err, SourceError::Refused(_)), "{err}");
    server.assert_saw(&["ACCOUNT_AUTH_REQ", "SYMBOLS_LIST_REQ"]);
    assert_eq!(server.count_seen("GET_TRENDBARS_REQ"), 0);
}

#[test]
fn the_process_pace_admits_four_requests_in_a_second_and_holds_the_fifth() {
    // The pace needs no session, so the provider is never asked.
    let never_asked: HistoryConfigProvider =
        Arc::new(|| -> Result<ConnConfig, HistoryConfigError> {
            Err(HistoryConfigError::NotConfigured)
        });
    let mut wire = LiveHistoryWire::shared(never_asked);
    let started = Instant::now();
    for _ in 0..5 {
        wire.pace();
    }
    // Any five admissions span a full second in a four-a-second window, whoever else took turns.
    assert!(
        started.elapsed() >= Duration::from_millis(950),
        "five paces took {:?}",
        started.elapsed()
    );
}

/// The scripted bars' stamps, in ms.
fn seed_window() -> (i64, i64) {
    (i64::from(SEED_BAR_OLDER_MINUTES) * MIN, i64::from(SEED_BAR_NEWER_MINUTES) * MIN)
}

/// A source over the live wire that waits at most `wait` for the process's one session permit.
fn impatient(server: &FakeCtrader, wait: Duration) -> CtraderKlines {
    CtraderKlines::with_wire(Box::new(
        LiveHistoryWire::shared(provider(server)).with_permit_wait(wait),
    ))
}

/// **One history session at a time in this process.** Each is an authenticated socket beside the account's
/// exec, data and recon sockets, and the venue's per-application connection cap is NOT MEASURED, so a loop
/// of `data hist fetch` runs must not open ten of them: a second source waits for the permit, is refused in
/// words when its wait is over without dialling anything, and gets in the moment the holder is dropped.
#[test]
fn a_second_source_is_refused_while_the_first_holds_the_one_session() {
    let (older, newer) = seed_window();
    let (first_server, second_server) =
        (FakeCtrader::start_scripted(), FakeCtrader::start_scripted());
    let first = CtraderKlines::new(provider(&first_server));
    first.fetch("EURUSD", "1m", older, newer).expect("the first source opens the session");

    let second = impatient(&second_server, Duration::from_millis(300));
    let started = Instant::now();
    let err =
        second.fetch("EURUSD", "1m", older, newer).expect_err("refused while the first holds it");
    assert!(
        matches!(&err, SourceError::Fetch(t) if t.contains("another cTrader history session")),
        "{err}"
    );
    assert!(started.elapsed() >= Duration::from_millis(250), "it waited its turn first");
    assert!(started.elapsed() < Duration::from_secs(10), "and not longer");
    assert_eq!(second_server.count_seen("APPLICATION_AUTH_REQ"), 0, "nothing was dialled");

    drop(first);
    second.fetch("EURUSD", "1m", older, newer).expect("the dropped session's permit is free");
    assert_eq!(second_server.count_seen("APPLICATION_AUTH_REQ"), 1);
}

#[test]
fn a_waiting_source_gets_the_session_the_moment_the_holder_is_dropped() {
    let (older, newer) = seed_window();
    let (first_server, second_server) =
        (FakeCtrader::start_scripted(), FakeCtrader::start_scripted());
    let first = CtraderKlines::new(provider(&first_server));
    first.fetch("EURUSD", "1m", older, newer).expect("the first source opens the session");
    let releasing = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        drop(first);
    });
    let second = impatient(&second_server, Duration::from_secs(20));
    let started = Instant::now();
    second.fetch("EURUSD", "1m", older, newer).expect("it waited for the permit and got it");
    assert!(started.elapsed() >= Duration::from_millis(250), "it did wait");
    assert!(started.elapsed() < Duration::from_secs(10), "and not for its whole deadline");
    releasing.join().expect("the releasing thread");
}

/// **The permit belongs to the WIRE, not to one session.** A fetch that drops its session and opens another
/// (an idle replacement, a drop, the second the token retry waits) must not lose its turn to a fetch that is
/// waiting for the permit: the waiter would take it in the gap and the first fetch would be refused,
/// mid-request, after its own wait.
#[test]
fn the_permit_is_kept_across_a_reopen_and_returned_when_the_wire_is_dropped() {
    // This fake serves EVERY connection (only the first one's trendbar request is faulty, and none is asked).
    let holder_server = FakeCtrader::start_trendbars_fault_first(TrendbarsFault::Eof);
    let waiter_server = FakeCtrader::start_scripted();
    let mut holder = LiveHistoryWire::shared(provider(&holder_server));
    let first = holder.open().expect("the holder opens its first session");
    drop(first); // the session is gone (idle, dropped): the wire is still the holder's

    let mut waiter = LiveHistoryWire::shared(provider(&waiter_server))
        .with_permit_wait(Duration::from_millis(300));
    let refused = waiter.open();
    assert!(
        matches!(&refused, Err(WireError::Failed(t)) if t.contains("another cTrader history session")),
        "the waiter must not slip in between the holder's two sessions"
    );
    assert_eq!(waiter_server.count_seen("APPLICATION_AUTH_REQ"), 0, "nothing was dialled");

    let second = holder.open().expect("the holder reopens at once: it still holds the permit");
    drop(second);
    drop(holder); // the wire goes: now the permit is free
    waiter.open().expect("the wire's drop returned the permit");
}

/// The venue's `ERROR_RES` for the trendbar request is the fetch's error in the venue's own words, at once
/// (a code that is no rate limit is not waited out or repeated).
#[test]
fn an_error_res_for_the_trendbar_request_is_the_venues_words_and_ends_the_fetch() {
    let (older, newer) = seed_window();
    let server = FakeCtrader::start_reject_trendbars_scripted();
    let source = CtraderKlines::new(provider(&server));
    let err = source.fetch("EURUSD", "1m", older, newer).expect_err("refused by the venue");
    assert!(
        matches!(&err, SourceError::Fetch(t)
            if t.contains("NOT_ENOUGH_MONEY") && t.contains("insufficient funds")),
        "{err}"
    );
    assert_eq!(server.count_seen("GET_TRENDBARS_REQ"), 1);
}

/// **Every way the venue ends a session mid-fetch is a dropped session**: the account's token invalidated,
/// the account's session disconnected, the socket closed. Each is read as such at once (not waited out to
/// the reply deadline), the source opens a new session once and asks the window again.
#[test]
fn a_session_the_venue_ends_mid_fetch_is_reopened_and_the_window_asked_again() {
    let (older, newer) = seed_window();
    for fault in [
        TrendbarsFault::Event(pt::ACCOUNTS_TOKEN_INVALIDATED_EVENT),
        TrendbarsFault::Event(pt::ACCOUNT_DISCONNECT_EVENT),
        TrendbarsFault::Eof,
    ] {
        let server = FakeCtrader::start_trendbars_fault_first(fault);
        let source = CtraderKlines::new(provider(&server));
        let started = Instant::now();
        let bars = source.fetch("EURUSD", "1m", older, newer).expect("the second session answers");
        assert_eq!(bars.len(), 2, "{fault:?}");
        assert_eq!(server.count_seen("GET_TRENDBARS_REQ"), 2, "{fault:?}: asked again");
        assert_eq!(server.count_seen("APPLICATION_AUTH_REQ"), 2, "{fault:?}: on a new session");
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{fault:?}: the session was waited out to the reply deadline ({:?})",
            started.elapsed()
        );
    }
}
