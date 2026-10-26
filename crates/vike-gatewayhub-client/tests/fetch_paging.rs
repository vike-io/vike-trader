#![cfg(feature = "data")]
//! **R8 and R9 — a remote fetch pages, reassembles identically, and never times out sooner than the
//! in-process fetch would.**
//!
//! * R9: a payload above the page ceiling splits into `FetchPage` frames and reassembles into the
//!   SAME `Vec` the in-process source returns; a lost or repeated page, a total that disagrees with what
//!   arrived and a page of the wrong kind are errors, never a shorter answer.
//! * R8: the client's idle bound is per FRAME and follows the cadence the daemon STATES in each
//!   `FetchProgress` (`max(floor, 3 × next_in_ms)`), so a slow fetch that keeps emitting progress runs as
//!   long as it needs, while a daemon that goes silent is still called dead.
//! * Cancellation (wire design B.6): a stop probe held by the source is polled while the client waits;
//!   on `true` it sends `FetchCancel`, drops the connection and returns `Fetch("... cancelled ...")`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vike_data::source::{FundingRateSource, KlineSource, SourceError};
use vike_gatewayhub_client::client::{FETCH_CANCELLED, GatewayClient, GatewayLink, GatewayTimings};
use vike_gatewayhub_client::history::{RemoteFunding, RemoteKlines, RemoteTicks};
use vike_gatewayhub_client::proto::{
    FetchProgress, FetchRows, FetchState, FetchWhat, Request, Response, WireFundingPoint,
};
use vike_hub_proto::auth::Scope;
use vike_marketdata::{Bar, QuoteTick};

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/fake_peer.rs"]
mod fake_peer;

use fake_peer::{FakePeer, PeerConfig, Step, fast_timings, read_only_keys};

fn bar(i: i64) -> Bar {
    Bar {
        ts: i * 60_000,
        open: 100.0 + i as f64,
        high: 101.0 + i as f64,
        low: 99.0 + i as f64,
        close: 100.5 + i as f64,
        volume: 12.5,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn progress(rid: u64, pages: u32, rows: u64, next_in_ms: u64) -> Response {
    Response::FetchProgress(FetchProgress {
        rid,
        state: FetchState::Running,
        pages,
        rows,
        elapsed_ms: 1,
        next_in_ms,
    })
}

/// The script a healthy daemon writes for `pages`: an immediate progress, every page, the totals.
fn page_script(pages: Vec<FetchRows>) -> Vec<Step> {
    let total: usize = pages.iter().map(FetchRows::len).sum();
    let n = u32::try_from(pages.len()).unwrap();
    let mut steps = vec![Step::Send(progress(1, 0, 0, 100))];
    for (i, rows) in pages.into_iter().enumerate() {
        steps.push(Step::Send(Response::FetchPage {
            rid: 1,
            page: u32::try_from(i).unwrap(),
            rows,
        }));
    }
    steps.push(Step::Send(Response::FetchDone { rid: 1, pages: n, rows: total as u64 }));
    steps
}

#[test]
fn a_payload_above_the_page_ceiling_reassembles_identically() {
    let rows: Vec<Bar> = (0..3_000).map(bar).collect();
    // A tiny ceiling forces dozens of pages from the real splitter the daemon uses.
    let pages: Vec<FetchRows> = FetchRows::Bars(rows.clone()).into_pages(16 * 1024).collect();
    assert!(pages.len() >= 10, "the split must really page: {} pages", pages.len());
    let peer = FakePeer::start(PeerConfig::new().with_rpc(move |_| page_script(pages.clone())));

    let got = RemoteKlines::new(peer.link(), "binance")
        .fetch("BTCUSDT.P", "1m", 1_000, 9_000_000)
        .expect("a paged answer");
    assert_eq!(got, rows, "same rows, same order, same bits");
    let requests = peer.requests();
    assert_eq!(requests.len(), 1, "one fetch, one request");
    assert_eq!(
        requests[0],
        Request::Fetch {
            rid: 1,
            what: FetchWhat::Klines {
                venue: "binance".into(),
                symbol: "BTCUSDT.P".into(),
                interval: "1m".into(),
                start_ms: 1_000,
                end_ms: 9_000_000,
            }
        },
        "the request carries the source's arguments verbatim"
    );
}

#[test]
fn funding_and_ticks_page_and_convert_without_losing_a_bit() {
    let points: Vec<WireFundingPoint> = (0..50)
        .map(|i| WireFundingPoint {
            ts_ms: i * 8 * 3_600_000,
            rate: -0.000_01 * (i as f64) / 3.0,
            premium: if i % 2 == 0 { Some(0.1 + 0.2) } else { None },
        })
        .collect();
    let funding_pages: Vec<FetchRows> =
        FetchRows::Funding(points.clone()).into_pages(512).collect();
    let peer = FakePeer::start(PeerConfig::new().with_rpc(move |req| match req {
        Request::Fetch { what: FetchWhat::Funding { .. }, .. } => {
            page_script(funding_pages.clone())
        }
        _ => vec![Step::Send(Response::Error("not scripted".into()))],
    }));
    let got = RemoteFunding::new(peer.link(), "binance").fetch("BTCUSDT.P", 0, 1).unwrap();
    assert_eq!(got.len(), points.len());
    for (g, w) in got.iter().zip(&points) {
        assert_eq!(
            (g.ts_ms, g.rate.to_bits(), g.premium.map(f64::to_bits)),
            (w.ts_ms, w.rate.to_bits(), w.premium.map(f64::to_bits))
        );
    }

    let quotes: Vec<QuoteTick> = (0..400)
        .map(|i| QuoteTick {
            ts: i,
            local_ts: 0,
            bid: 1.1 + (i as f64) * 1e-5,
            ask: 1.1001 + (i as f64) * 1e-5,
            bid_size: 1_000_000.0,
            ask_size: 1_500_000.0,
            symbol: "EURUSD".into(),
        })
        .collect();
    let pages: Vec<FetchRows> = FetchRows::Quotes(quotes.clone()).into_pages(4 * 1024).collect();
    let peer = FakePeer::start(PeerConfig::new().with_rpc(move |_| page_script(pages.clone())));
    let ticks = RemoteTicks::new(peer.link());
    // The datahub hands `backfill_quotes_then_bars` exactly this closure shape.
    let never_stop = || false;
    let mut fetch = ticks.fetcher(&never_stop);
    let got = fetch("EURUSD", 0, 1_000).expect("paged ticks");
    assert_eq!(got, quotes);
}

#[test]
fn a_lost_or_repeated_page_and_a_short_total_are_errors_never_a_shorter_answer() {
    let page = |n: i64| FetchRows::Bars(vec![bar(n)]);
    let cases: Vec<(&str, Vec<Step>, &str)> = vec![
        (
            "a page was lost",
            vec![
                Step::Send(progress(1, 0, 0, 100)),
                Step::Send(Response::FetchPage { rid: 1, page: 0, rows: page(0) }),
                Step::Send(Response::FetchPage { rid: 1, page: 2, rows: page(2) }),
                Step::Send(Response::FetchDone { rid: 1, pages: 3, rows: 3 }),
            ],
            "lost or repeated",
        ),
        (
            "a page was repeated",
            vec![
                Step::Send(progress(1, 0, 0, 100)),
                Step::Send(Response::FetchPage { rid: 1, page: 0, rows: page(0) }),
                Step::Send(Response::FetchPage { rid: 1, page: 0, rows: page(0) }),
                Step::Send(Response::FetchDone { rid: 1, pages: 2, rows: 2 }),
            ],
            "lost or repeated",
        ),
        (
            "the totals disagree with what arrived",
            vec![
                Step::Send(progress(1, 0, 0, 100)),
                Step::Send(Response::FetchPage { rid: 1, page: 0, rows: page(0) }),
                Step::Send(Response::FetchDone { rid: 1, pages: 1, rows: 5 }),
            ],
            "lost on the way",
        ),
        (
            "the pages are not bars",
            vec![
                Step::Send(progress(1, 0, 0, 100)),
                Step::Send(Response::FetchPage {
                    rid: 1,
                    page: 0,
                    rows: FetchRows::Quotes(vec![]),
                }),
                Step::Send(Response::FetchDone { rid: 1, pages: 1, rows: 0 }),
            ],
            "Quotes",
        ),
        (
            "a page of another fetch",
            vec![
                Step::Send(progress(1, 0, 0, 100)),
                Step::Send(Response::FetchPage { rid: 9, page: 0, rows: page(0) }),
            ],
            "one fetch at a time",
        ),
    ];
    for (name, steps, needle) in cases {
        let peer = FakePeer::start(PeerConfig::new().with_rpc(move |_| steps.clone()));
        let err = RemoteKlines::new(peer.link(), "binance").fetch("X", "1m", 0, 1).expect_err(name);
        let SourceError::Fetch(text) = err else {
            panic!("{name}: Refused is not a transport fault")
        };
        assert!(text.contains("gatewayhub") && text.contains(needle), "{name}: {text}");
    }
}

#[test]
fn zero_pages_is_an_empty_result_only_when_the_daemon_says_zero() {
    let peer = FakePeer::start(PeerConfig::new().with_rpc(|_| page_script(vec![])));
    let got = RemoteKlines::new(peer.link(), "binance").fetch("X", "1m", 0, 1);
    assert_eq!(
        got,
        Ok(Vec::new()),
        "FetchDone {{ pages: 0, rows: 0 }} is a legitimate empty answer"
    );
}

// ---- R8: the stall bound follows the daemon's stated cadence ---------------------------------------

fn timings(floor_ms: u64) -> GatewayTimings {
    GatewayTimings { fetch_stall_floor: Duration::from_millis(floor_ms), ..fast_timings() }
}

/// A fetch slower than the floor — ten gaps of 150 ms (1.5 s) against a 300 ms floor — survives because
/// every gap is shorter than `max(floor, 3 × stated cadence)`: progress keeps it alive.
#[test]
fn progress_frames_keep_a_slow_fetch_alive() {
    let mut steps = vec![Step::Send(progress(1, 0, 0, 100))];
    for i in 0..10u64 {
        steps.push(Step::Sleep(Duration::from_millis(150)));
        steps.push(Step::Send(progress(1, 0, i, 100)));
    }
    steps.extend(page_script(vec![FetchRows::Bars(vec![bar(1), bar(2)])]).into_iter().skip(1));
    let peer = FakePeer::start(PeerConfig::new().with_rpc(move |_| steps.clone()));

    let started = Instant::now();
    let got = RemoteKlines::new(peer.link().with_timings(timings(300)), "binance")
        .fetch("X", "1m", 0, 1)
        .expect("a fetch that keeps emitting frames is never clipped");
    assert_eq!(got, [bar(1), bar(2)]);
    assert!(
        started.elapsed() >= Duration::from_millis(1_400),
        "the fetch really was slow: {:?}",
        started.elapsed()
    );
}

#[test]
fn a_daemon_that_goes_silent_is_called_dead_after_the_stall_bound() {
    let peer = FakePeer::start(
        PeerConfig::new().with_rpc(|_| vec![Step::Send(progress(1, 0, 0, 50)), Step::Hang]),
    );
    let started = Instant::now();
    let err = RemoteKlines::new(peer.link().with_timings(timings(300)), "binance")
        .fetch("X", "1m", 0, 1)
        .expect_err("silence is not a result");
    let took = started.elapsed();
    let SourceError::Fetch(text) = err else { panic!() };
    assert!(text.contains("gatewayhub") && text.contains("no frame"), "{text}");
    assert!(took >= Duration::from_millis(250) && took < Duration::from_secs(3), "{took:?}");
}

/// The bound is the daemon's stated cadence times three when that exceeds the floor: a daemon that
/// promises a frame every 500 ms is believed for 1.5 s, not clipped at the 300 ms floor.
#[test]
fn the_stall_bound_follows_the_cadence_the_daemon_states() {
    let peer = FakePeer::start(
        PeerConfig::new().with_rpc(|_| vec![Step::Send(progress(1, 0, 0, 500)), Step::Hang]),
    );
    let started = Instant::now();
    let err = RemoteKlines::new(peer.link().with_timings(timings(300)), "binance")
        .fetch("X", "1m", 0, 1)
        .expect_err("it did go silent in the end");
    assert!(matches!(err, SourceError::Fetch(_)));
    assert!(
        started.elapsed() >= Duration::from_millis(1_300),
        "three stated periods, not the floor: {:?}",
        started.elapsed()
    );
}

// ---- Cancellation ---------------------------------------------------------------------------------

#[test]
fn a_stop_probe_cancels_a_fetch_in_flight_and_the_daemon_is_told() {
    let peer = FakePeer::start(
        PeerConfig::new().with_rpc(|_| vec![Step::Send(progress(1, 0, 0, 100)), Step::Hang]),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let source = RemoteKlines::new(peer.link().with_timings(timings(5_000)), "binance")
        .with_stop(Arc::new(move || flag.load(Ordering::SeqCst)));

    let trigger = Arc::clone(&stop);
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        trigger.store(true, Ordering::SeqCst);
    });
    let started = Instant::now();
    let err = source.fetch("X", "1m", 0, 1).expect_err("cancelled");
    t.join().unwrap();
    assert_eq!(err, SourceError::Fetch(FETCH_CANCELLED.to_string()));
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    assert!(
        FakePeer::wait_until(Duration::from_secs(3), || peer
            .requests()
            .contains(&Request::FetchCancel { rid: 1 })),
        "the daemon saw the cancel (and the dropped connection says the same)"
    );
}

#[test]
fn a_probe_that_never_fires_does_not_disturb_a_fetch() {
    let peer = FakePeer::start(
        PeerConfig::new().with_rpc(|_| page_script(vec![FetchRows::Bars(vec![bar(7)])])),
    );
    let never = || false;
    let got = RemoteKlines::new(peer.link(), "binance")
        .fetch_until("X", "1m", 0, 1, Some(&never))
        .unwrap();
    assert_eq!(got, [bar(7)]);
}

// ---- Scope ----------------------------------------------------------------------------------------

#[test]
fn every_window_fetch_dials_write_and_a_catalog_fetch_dials_read() {
    let peer = FakePeer::start(PeerConfig::new().with_rpc(|req| match req {
        Request::Fetch { rid, .. } | Request::FetchCredentialed { rid, .. } => {
            vec![
                Step::Send(progress(*rid, 0, 0, 100)),
                Step::Send(Response::FetchDone { rid: *rid, pages: 0, rows: 0 }),
            ]
        }
        _ => vec![Step::Send(Response::Error("not scripted".into()))],
    }));
    // The three lanes that walk a window the caller names, keyless or not, need the Write scope: the
    // datahub's `Backfill` was a Write verb, and a Read key must not walk a venue's history.
    RemoteKlines::new(peer.link(), "binance").fetch("X", "1m", 0, 1).unwrap();
    RemoteFunding::new(peer.link(), "binance").fetch("X", 0, 1).unwrap();
    RemoteTicks::new(peer.link()).fetch_quotes_range("EURUSD", 0, 1, None).unwrap();
    RemoteKlines::credentialed(peer.link(), "oanda").fetch("EUR_USD", "1m", 0, 1).unwrap();
    assert_eq!(
        peer.auths(),
        [(Scope::Write, true), (Scope::Write, true), (Scope::Write, true), (Scope::Write, true)]
    );
    // A catalog listing costs the venue its own universe and stays Read.
    let mut client = GatewayClient::connect(&peer.link(), Scope::Read).expect("a Read dial");
    client
        .fetch(&FetchWhat::CatalogList { venue: "binance".into() }, false, None)
        .expect("a catalog listing is admitted on a Read connection");
    assert_eq!(peer.auths().last(), Some(&(Scope::Read, true)));
    let verbs: Vec<&str> = peer
        .requests()
        .iter()
        .map(|r| match r {
            Request::Fetch { what: FetchWhat::Klines { .. }, .. } => "Fetch/klines",
            Request::Fetch { what: FetchWhat::Funding { .. }, .. } => "Fetch/funding",
            Request::Fetch { what: FetchWhat::Ticks { .. }, .. } => "Fetch/ticks",
            Request::Fetch { what: FetchWhat::CatalogList { .. }, .. } => "Fetch/catalog_list",
            Request::FetchCredentialed { .. } => "FetchCredentialed",
            _ => "other",
        })
        .collect();
    assert_eq!(
        verbs,
        ["Fetch/klines", "Fetch/funding", "Fetch/ticks", "FetchCredentialed", "Fetch/catalog_list"]
    );
}

/// **A link holding the Read key cannot start a window fetch**, keyless or not: the source dials under
/// the Write scope and the link has no key for it, so nothing is dialled and the error names the
/// missing key, as a retryable `Fetch` and never as a `Refused` source.
#[test]
fn a_link_without_a_write_key_cannot_even_dial_a_window_fetch() {
    let peer = FakePeer::start(PeerConfig::new());
    let read_only =
        GatewayLink::new(peer.addr_string(), read_only_keys()).with_timings(fast_timings());
    let klines = RemoteKlines::new(read_only.clone(), "binance").fetch("X", "1m", 0, 1);
    let funding = RemoteFunding::new(read_only.clone(), "binance").fetch("X", 0, 1);
    let ticks = RemoteTicks::new(read_only).fetch_quotes_range("EURUSD", 0, 1, None);
    for (lane, text) in [
        ("klines", klines.map(|_| ()).map_err(|e| format!("{e:?}"))),
        ("funding", funding.map(|_| ()).map_err(|e| format!("{e:?}"))),
        ("ticks", ticks.map(|_| ())),
    ] {
        let text = text.expect_err("a window fetch on a Read-only link");
        assert!(text.contains("control") && text.contains("no "), "{lane}: {text}");
        assert!(!text.contains("Refused"), "{lane}: a missing key is not a Refused source: {text}");
    }
    assert_eq!(peer.connections(), 0, "nothing was dialled: the key check is local");
}

#[test]
fn a_link_without_a_write_key_cannot_even_dial_the_credentialed_lane() {
    let peer = FakePeer::start(PeerConfig::new());
    let read_only =
        GatewayLink::new(peer.addr_string(), read_only_keys()).with_timings(fast_timings());
    let err =
        RemoteKlines::credentialed(read_only, "oanda").fetch("EUR_USD", "1m", 0, 1).unwrap_err();
    let SourceError::Fetch(text) = err else { panic!("a missing key is not a Refused source") };
    assert!(text.contains("control") && text.contains("no "), "{text}");
    assert_eq!(peer.connections(), 0, "nothing was dialled: the key check is local");
}

// ---- A source is a request: one connection for its life ----------------------------------------------

/// A script that echoes the request's `rid`, so one connection can carry several fetches. `then`
/// follows the answer (e.g. `Step::Close`: the daemon hangs up after every reply).
fn echo_script(
    rows: Vec<Bar>,
    then: Option<Step>,
) -> impl Fn(&Request) -> Vec<Step> + Send + Sync + 'static {
    move |req| {
        let (Request::Fetch { rid, .. } | Request::FetchCredentialed { rid, .. }) = req else {
            return vec![Step::Send(Response::Error("not scripted".into()))];
        };
        let mut steps = vec![
            Step::Send(progress(*rid, 0, 0, 100)),
            Step::Send(Response::FetchPage {
                rid: *rid,
                page: 0,
                rows: FetchRows::Bars(rows.clone()),
            }),
            Step::Send(Response::FetchDone { rid: *rid, pages: 1, rows: rows.len() as u64 }),
        ];
        steps.extend(then.clone());
        steps
    }
}

/// The remote twin of the in-process per-request source: the daemon keeps its per-connection state (the
/// OANDA token memo) as long as the request lives, so a chunked ingest of thousands of day fetches must
/// ride ONE connection, not dial once per day.
#[test]
fn a_source_holds_one_connection_for_its_life_and_a_clone_is_a_new_request() {
    let peer = FakePeer::start(PeerConfig::new().with_rpc(echo_script(vec![bar(1)], None)));
    let source = RemoteKlines::credentialed(peer.link(), "oanda");
    for _ in 0..5 {
        assert_eq!(source.fetch("EUR_USD", "1m", 0, 1).unwrap(), [bar(1)]);
    }
    assert_eq!(peer.connections(), 1, "five fetches, one connection");
    assert_eq!(peer.auths(), [(Scope::Write, true)], "one handshake, signed once");
    let rids: Vec<u64> = peer
        .requests()
        .iter()
        .filter_map(|r| match r {
            Request::FetchCredentialed { rid, .. } => Some(*rid),
            _ => None,
        })
        .collect();
    assert_eq!(rids, [1, 2, 3, 4, 5], "request ids rise on the one connection");

    let other = source.clone();
    other.fetch("EUR_USD", "1m", 0, 1).unwrap();
    assert_eq!(peer.connections(), 2, "a clone is a new request with its own connection");
}

/// A connection the daemon reaped while it idled is detected BEFORE any reply frame and repeated once
/// on a fresh connection; a failure AFTER the daemon began answering is the fetch's answer, never
/// repeated (the venue may have been spent).
#[test]
fn a_reaped_connection_is_redialled_once_before_any_reply_and_never_after_one() {
    // The daemon hangs up after every answer, as an idle reaper does.
    let peer =
        FakePeer::start(PeerConfig::new().with_rpc(echo_script(vec![bar(1)], Some(Step::Close))));
    let source = RemoteKlines::new(peer.link(), "binance");
    assert_eq!(source.fetch("X", "1m", 0, 1).unwrap(), [bar(1)]);
    assert_eq!(
        source.fetch("X", "1m", 0, 1).unwrap(),
        [bar(1)],
        "the stale connection is repeated"
    );
    assert_eq!(peer.connections(), 2, "exactly one redial");

    // Answered once; then a held connection that starts answering and dies is NOT repeated.
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let n = Arc::clone(&calls);
    let full = echo_script(vec![bar(2)], None);
    let peer = FakePeer::start(PeerConfig::new().with_rpc(move |req| {
        if n.fetch_add(1, Ordering::SeqCst) == 0 {
            full(req)
        } else {
            vec![Step::Send(progress(2, 0, 0, 100)), Step::Close]
        }
    }));
    let source = RemoteKlines::new(peer.link(), "binance");
    assert_eq!(source.fetch("X", "1m", 0, 1).unwrap(), [bar(2)]);
    let err = source.fetch("X", "1m", 0, 1).expect_err("it began answering and then died");
    assert!(matches!(err, SourceError::Fetch(_)), "{err:?}");
    assert_eq!(peer.connections(), 1, "no redial after a reply frame was heard");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "the venue was asked exactly twice");
}

/// A daemon whose FIRST fetch is answered whole (so the source holds its connection) and whose every
/// later one is `later`; the counter is how many requests reached it.
fn answers_once_then(later: Vec<Step>) -> (FakePeer, Arc<std::sync::atomic::AtomicUsize>) {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let n = Arc::clone(&calls);
    let first = echo_script(vec![bar(1)], None);
    let peer = FakePeer::start(PeerConfig::new().with_rpc(move |req| {
        if n.fetch_add(1, Ordering::SeqCst) == 0 { first(req) } else { later.clone() }
    }));
    (peer, calls)
}

/// **A TIMEOUT is never "stale": a daemon that read the request and went quiet is not repeated.** The
/// held connection is redialled only when it was reaped while idle (it fails at once, with an EOF or a
/// reset, before any reply byte). A stall that fires before the first frame has the same `heard == false`
/// but means the opposite: the daemon HAS the request and may be spending the venue on it, so repeating
/// it would double the wait (the stall bound, then a handshake, then the bound again) and the venue
/// request. Exactly one request reaches the daemon for the wedged fetch, on the one connection.
///
/// ⚠ Slow by design: with no frame yet the bound is `3 × FETCH_PROGRESS_EVERY` (15 s), the protocol's
/// own number, which no timing knob narrows.
#[test]
fn a_held_connection_that_goes_silent_on_the_request_is_not_redialled_or_repeated() {
    let (peer, calls) = answers_once_then(vec![Step::Hang]);
    let source = RemoteKlines::new(peer.link().with_timings(timings(300)), "binance");
    assert_eq!(
        source.fetch("X", "1m", 0, 1).unwrap(),
        [bar(1)],
        "the first fetch holds the connection"
    );

    let started = Instant::now();
    let err = source.fetch("X", "1m", 0, 1).expect_err("silence is not a result");
    let SourceError::Fetch(text) = err else { panic!("{err:?}") };
    assert!(text.contains("gatewayhub") && text.contains("no frame"), "{text}");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "one request answered, ONE wedged: no repeat");
    assert_eq!(peer.connections(), 1, "no redial");
    assert!(
        started.elapsed() < Duration::from_secs(25),
        "one stall bound, not two: {:?}",
        started.elapsed()
    );
}

/// The same rule when the daemon BEGAN its answer: a frame header arrives and the body never does, so
/// the read times out mid-frame (still before any complete reply frame). That is a timeout too, and the
/// daemon has plainly seen the request.
#[test]
fn a_held_connection_that_tears_its_first_frame_is_not_redialled_or_repeated() {
    let (peer, calls) = answers_once_then(vec![Step::Torn, Step::Hang]);
    let source = RemoteKlines::new(peer.link().with_timings(timings(300)), "binance");
    assert_eq!(source.fetch("X", "1m", 0, 1).unwrap(), [bar(1)]);

    let err = source.fetch("X", "1m", 0, 1).expect_err("a torn frame is not a result");
    assert!(matches!(err, SourceError::Fetch(_)), "{err:?}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "no repeat of a request the daemon began to answer"
    );
    assert_eq!(peer.connections(), 1, "no redial");
}
