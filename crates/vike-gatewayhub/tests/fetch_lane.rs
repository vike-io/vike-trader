//! The fetch family END TO END and on the wire: paging, progress, cancel, and what the connection
//! owns.
//!
//! Drift register rows held here: R8 (a remote fetch never times out sooner than the in-process one:
//! progress frames keep a slow fetch alive), R9 (a payload above the frame ceiling splits and
//! reassembles identically), and the cancel contract of the wire design (a `FetchCancel` on the same
//! connection stops the lane; a link drop cancels every fetch the connection started). Plus the
//! connection-scoped read-once memo the OANDA lane will use.

#[path = "support/fake_lanes.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_lanes;
#[path = "support/harness.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod harness;
#[path = "support/raw_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod raw_peer;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_data::source::{KlineSource, SourceError};
use vike_gatewayhub::rest::FetchCtx;
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub::session::{ConnectionMemo, ConnectionScope, Redacted};
use vike_gatewayhub_client::client::GatewayTimings;
use vike_gatewayhub_client::history::{RemoteKlines, RemoteTicks};
use vike_gatewayhub_client::proto::{
    FETCH_PAGE_CEILING_BYTES, FetchFailure, FetchRows, FetchState, FetchWhat,
    GW_DOWN_FRAME_CEILING_BYTES, Request, Response, WireFundingPoint,
};
use vike_hub_proto::auth::Scope;
use vike_marketdata::{Bar, QuoteTick};

use harness::{Harness, test_keys};
use raw_peer::Raw;

fn bars(n: usize) -> Vec<Bar> {
    (0..n)
        .map(|i| {
            let x = i as f64;
            Bar {
                ts: i as i64 * 60_000,
                open: 0.1 * x,
                high: 0.1 * x + 1.0 / 3.0,
                low: 0.1 * x - 1.0 / 7.0,
                close: 0.1 * x + 1.0 / 9.0,
                volume: x / 7.0,
                funding: Some(x * 1e-7),
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect()
}

fn klines_request(rid: u64, venue: &str) -> Request {
    Request::Fetch {
        rid,
        what: FetchWhat::Klines {
            venue: venue.into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            start_ms: 0,
            end_ms: 1,
        },
    }
}

/// Wait for `flag` for up to `wait`.
fn raised(flag: &AtomicBool, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if flag.load(Ordering::SeqCst) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    flag.load(Ordering::SeqCst)
}

/// Park until the fetch is cancelled (or `limit` passes); `true` when it was cancelled.
fn park_until_cancelled(ctx: &FetchCtx, limit: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < limit {
        if ctx.is_cancelled() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    ctx.is_cancelled()
}

#[test]
fn a_payload_above_the_page_ceiling_reassembles_identically() {
    const N: usize = 110_000;
    let h = Harness::start();
    let want = bars(N);
    let served = want.clone();
    h.rest.set_fetch(move |_, _, _, _| Ok(FetchRows::Bars(served.clone())));
    let source = RemoteKlines::new(h.link(), "binance");
    let got = source.fetch("BTCUSDT", "1m", 0, 1).expect("the fetch succeeds");
    assert_eq!(got.len(), N);
    assert!(got == want, "the reassembled series differs from the served one");
}

#[test]
fn the_pages_are_under_the_ceiling_numbered_from_zero_and_the_totals_add_up() {
    const N: usize = 110_000;
    let h = Harness::start();
    let served = bars(N);
    h.rest.set_fetch(move |_, _, _, _| Ok(FetchRows::Bars(served.clone())));
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Write);
    raw.send(&klines_request(7, "binance"));
    let (mut pages, mut rows) = (0u32, 0u64);
    let mut progress = 0;
    loop {
        match raw.recv::<Response>() {
            Response::FetchProgress(p) => {
                assert_eq!(p.rid, 7);
                progress += 1;
            }
            Response::FetchPage { rid, page, rows: r } => {
                assert_eq!((rid, page), (7, pages), "pages must count from 0 by one");
                pages += 1;
                rows += r.len() as u64;
            }
            Response::FetchDone { rid, pages: p, rows: r } => {
                assert_eq!(rid, 7);
                assert_eq!((p, r), (pages, rows), "the totals must be what was sent");
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(rows, N as u64);
    assert!(pages >= 3, "{N} bars fit {pages} pages: the fetch was not paged");
    assert!(progress >= 1, "the immediate progress frame is missing");
    // Every page frame fits what the client reads (checked by the client reading them), and each page's
    // payload budget is the protocol's: well under the frame ceiling.
    assert!((FETCH_PAGE_CEILING_BYTES as u32) < GW_DOWN_FRAME_CEILING_BYTES);
}

#[test]
fn the_first_frame_is_an_immediate_progress_stating_the_cadence() {
    let mut cfg = ServerConfig::PRODUCTION;
    cfg.timings.fetch_progress_every = Duration::from_millis(250);
    let h = Harness::start_config(cfg);
    let release = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&release);
    h.rest.set_fetch(move |_, _, _, _| {
        while !gate.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(FetchRows::Bars(Vec::new()))
    });
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Write);
    raw.send(&klines_request(3, "binance"));
    match raw.recv::<Response>() {
        Response::FetchProgress(p) => {
            assert_eq!((p.rid, p.state, p.pages, p.rows), (3, FetchState::Running, 0, 0));
            assert_eq!(p.next_in_ms, 250, "the promise must be the cadence the monitor keeps");
        }
        other => panic!("the first frame of an accepted fetch must be a progress, not {other:?}"),
    }
    release.store(true, Ordering::SeqCst);
    // An empty result is legitimate: zero pages, zero rows.
    loop {
        match raw.recv::<Response>() {
            Response::FetchProgress(_) => {}
            Response::FetchDone { pages: 0, rows: 0, .. } => break,
            other => panic!("{other:?}"),
        }
    }
}

/// **R8.** The client's idle bound is `max(floor, 3 x next_in_ms)`; its floor here is 400 ms and the
/// lane takes 1.5 s. Only the daemon's progress frames (every 100 ms) keep the fetch alive.
#[test]
fn progress_frames_keep_a_slow_fetch_alive() {
    let mut cfg = ServerConfig::PRODUCTION;
    cfg.timings.fetch_progress_every = Duration::from_millis(100);
    let h = Harness::start_config(cfg);
    h.rest.set_fetch(|_, _, _, ctx| {
        park_until_cancelled(ctx, Duration::from_millis(2_000));
        Ok(FetchRows::Bars(bars(10)))
    });
    let mut timings: GatewayTimings = harness::fast_timings();
    timings.fetch_stall_floor = Duration::from_millis(800);
    let link = h.link().with_timings(timings);
    let started = Instant::now();
    let got = RemoteKlines::new(link, "binance").fetch("BTCUSDT", "1m", 0, 1).expect("alive");
    assert_eq!(got.len(), 10);
    assert!(
        started.elapsed() >= Duration::from_millis(1_900),
        "the lane did not take its time ({:?}): the test proved nothing",
        started.elapsed()
    );

    // On the wire: a frame at least every cadence while the lane works (a shorter script this time).
    h.rest.set_fetch(|_, _, _, ctx| {
        park_until_cancelled(ctx, Duration::from_millis(1_000));
        Ok(FetchRows::Bars(bars(10)))
    });
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Write);
    raw.send(&klines_request(1, "binance"));
    let mut progress = 0;
    let mut last = Instant::now();
    loop {
        match raw.recv::<Response>() {
            Response::FetchProgress(p) => {
                assert_eq!(p.next_in_ms, 100);
                assert!(
                    last.elapsed() < Duration::from_millis(800),
                    "a gap of {:?} between frames",
                    last.elapsed()
                );
                last = Instant::now();
                progress += 1;
            }
            Response::FetchPage { .. } => last = Instant::now(),
            Response::FetchDone { .. } => break,
            other => panic!("{other:?}"),
        }
    }
    assert!(progress >= 6, "only {progress} progress frames in a 1 s fetch at a 100 ms cadence");
}

#[test]
fn a_waiting_lane_reports_queued_in_its_progress() {
    let mut cfg = ServerConfig::PRODUCTION;
    cfg.timings.fetch_progress_every = Duration::from_millis(80);
    let h = Harness::start_config(cfg);
    h.rest.set_fetch(|_, _, _, ctx| {
        ctx.set_state(FetchState::Queued);
        std::thread::sleep(Duration::from_millis(400));
        ctx.set_state(FetchState::Running);
        Ok(FetchRows::Bars(Vec::new()))
    });
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Write);
    raw.send(&klines_request(1, "binance"));
    let mut states = Vec::new();
    loop {
        match raw.recv::<Response>() {
            Response::FetchProgress(p) => states.push(p.state),
            Response::FetchDone { .. } => break,
            other => panic!("{other:?}"),
        }
    }
    assert!(states.contains(&FetchState::Queued), "a budget wait read as dead: {states:?}");
}

#[test]
fn a_fetch_cancel_on_the_same_connection_stops_the_lane_and_the_connection_goes_on() {
    let h = Harness::start();
    let saw_cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&saw_cancel);
    h.rest.set_fetch(move |_, _, _, ctx| {
        if park_until_cancelled(ctx, Duration::from_secs(20)) {
            flag.store(true, Ordering::SeqCst);
        }
        Ok(FetchRows::Bars(bars(3)))
    });
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Write);
    raw.send(&klines_request(9, "binance"));
    assert!(matches!(raw.recv::<Response>(), Response::FetchProgress(_)));
    raw.send(&Request::FetchCancel { rid: 9 });
    loop {
        match raw.recv::<Response>() {
            Response::FetchProgress(_) => {}
            Response::FetchCancelled { rid } => {
                assert_eq!(rid, 9);
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(raised(&saw_cancel, Duration::from_secs(3)), "the lane never saw the cancel");
    raw.send(&Request::Ping);
    assert!(matches!(raw.recv::<Response>(), Response::Pong));
}

#[test]
fn a_cancel_for_another_fetch_is_ignored() {
    let h = Harness::start();
    h.rest.set_fetch(|_, _, _, ctx| {
        assert!(
            !park_until_cancelled(ctx, Duration::from_millis(400)),
            "a stranger's cancel reached the fetch"
        );
        Ok(FetchRows::Bars(bars(2)))
    });
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Write);
    raw.send(&klines_request(1, "binance"));
    raw.send(&Request::FetchCancel { rid: 99 });
    loop {
        match raw.recv::<Response>() {
            Response::FetchProgress(_) | Response::FetchPage { .. } => {}
            Response::FetchDone { rows: 2, .. } => break,
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_stop_probe_cancels_a_running_fetch_through_the_real_client() {
    let h = Harness::start();
    let saw_cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&saw_cancel);
    h.rest.set_fetch(move |_, _, _, ctx| {
        if park_until_cancelled(ctx, Duration::from_secs(20)) {
            flag.store(true, Ordering::SeqCst);
        }
        Ok(FetchRows::Quotes(Vec::<QuoteTick>::new()))
    });
    let stop = Arc::new(AtomicBool::new(false));
    let probe_flag = Arc::clone(&stop);
    let trigger = std::thread::Builder::new()
        .name("test-stop-probe".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            probe_flag.store(true, Ordering::SeqCst);
        })
        .expect("spawn");
    let ticks = RemoteTicks::new(h.link());
    let probe = move || stop.load(Ordering::SeqCst);
    let started = Instant::now();
    let err = ticks.fetch_quotes_range("EURUSD", 0, 1, Some(&probe)).expect_err("the probe fired");
    trigger.join().expect("join");
    assert!(err.contains("cancelled"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(5), "the cancel took {:?}", started.elapsed());
    assert!(raised(&saw_cancel, Duration::from_secs(3)), "the lane never saw the cancel");
}

#[test]
fn a_link_drop_cancels_the_fetch_the_connection_started() {
    let h = Harness::start();
    let saw_cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&saw_cancel);
    h.rest.set_fetch(move |_, _, _, ctx| {
        if park_until_cancelled(ctx, Duration::from_secs(20)) {
            flag.store(true, Ordering::SeqCst);
        }
        Ok(FetchRows::Bars(Vec::new()))
    });
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Write);
    raw.send(&klines_request(1, "binance"));
    assert!(matches!(raw.recv::<Response>(), Response::FetchProgress(_)));
    drop(raw);
    assert!(
        raised(&saw_cancel, Duration::from_secs(3)),
        "a dead link left its fetch running: venue budget would be spent for nobody"
    );
}

#[test]
fn a_failure_keeps_its_variant_and_its_words() {
    let h = Harness::start();
    h.rest.set_fetch(|_, what, _, _| match what {
        FetchWhat::Klines { symbol, .. } if symbol == "REFUSED" => {
            Err(FetchFailure::Refused("an interval this source cannot express".into()))
        }
        _ => Err(FetchFailure::Fetch("HTTP 429 from the venue".into())),
    });
    let source = RemoteKlines::new(h.link(), "binance");
    match source.fetch("REFUSED", "7m", 0, 1) {
        Err(SourceError::Refused(text)) => {
            assert_eq!(text, "an interval this source cannot express")
        }
        other => panic!("{other:?}"),
    }
    match source.fetch("BTCUSDT", "1m", 0, 1) {
        Err(SourceError::Fetch(text)) => assert_eq!(text, "HTTP 429 from the venue"),
        other => panic!("{other:?}"),
    }
    // The connection a source holds survives a failed fetch.
    h.rest.set_fetch(|_, _, _, _| Ok(FetchRows::Bars(bars(2))));
    assert_eq!(source.fetch("BTCUSDT", "1m", 0, 1).expect("recovered").len(), 2);
}

#[test]
fn an_unserved_venue_is_refused_without_asking_the_lane() {
    let h = Harness::start();
    let source = RemoteKlines::new(h.link(), "okx");
    match source.fetch("BTCUSDT", "1m", 0, 1) {
        Err(SourceError::Refused(text)) => {
            assert!(text.contains("`okx`") && text.contains("binance, bybit"), "{text}");
        }
        other => panic!("{other:?}"),
    }
    assert!(h.rest.fetches.lock().unwrap_or_else(PoisonError::into_inner).is_empty());
}

#[test]
fn rows_of_the_wrong_kind_are_a_failure_and_not_a_short_answer() {
    let h = Harness::start();
    h.rest.set_fetch(|_, _, _, _| {
        Ok(FetchRows::Funding(vec![WireFundingPoint { ts_ms: 1, rate: 0.0001, premium: None }]))
    });
    match RemoteKlines::new(h.link(), "binance").fetch("BTCUSDT", "1m", 0, 1) {
        Err(SourceError::Fetch(text)) => {
            assert!(text.contains("Funding") && text.contains("klines"), "{text}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_lane_the_build_does_not_serve_is_a_positional_error_naming_the_string() {
    let mut facts = harness::full_facts();
    facts.ticks.clear();
    let h = Harness::start_with(
        fake_lanes::FakeData::new(&["binance"]),
        fake_lanes::FakeRest::new(facts),
        ServerConfig::PRODUCTION,
        test_keys(),
    );
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Write);
    raw.send(&Request::Fetch {
        rid: 1,
        what: FetchWhat::Ticks {
            venue: "dukascopy".into(),
            symbol: "EURUSD".into(),
            start_ms: 0,
            end_ms: 1,
        },
    });
    match raw.recv::<Response>() {
        Response::Error(text) => assert!(text.contains("\"gw_ticks\""), "{text}"),
        other => panic!("{other:?}"),
    }
    // The connection survived the refusal.
    raw.send(&Request::Ping);
    assert!(matches!(raw.recv::<Response>(), Response::Pong));
}

#[test]
fn a_request_pipelined_behind_a_fetch_is_answered_after_its_terminal_frame() {
    let h = Harness::start();
    h.rest.set_fetch(|_, _, _, _| {
        std::thread::sleep(Duration::from_millis(250));
        Ok(FetchRows::Bars(bars(1)))
    });
    let mut raw = Raw::open(h.addr, &test_keys(), Scope::Write);
    raw.send(&klines_request(1, "binance"));
    raw.send(&Request::Ping);
    let mut order = Vec::new();
    loop {
        match raw.recv::<Response>() {
            Response::FetchProgress(_) => order.push("progress"),
            Response::FetchPage { .. } => order.push("page"),
            Response::FetchDone { .. } => order.push("done"),
            Response::Pong => {
                order.push("pong");
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(order.last(), Some(&"pong"));
    assert!(order.contains(&"done"), "the pong came before the fetch finished: {order:?}");
    assert_eq!(order.iter().filter(|o| **o == "done").count(), 1);
}

/// What the OANDA lane keeps per connection: the token, read once. `dropped` flips when the value is
/// dropped, which is when the connection's scope is.
struct Probe {
    dropped: Arc<AtomicBool>,
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

type TokenMemo = ConnectionMemo<Redacted<Probe>, String>;

#[test]
fn the_credential_is_read_once_per_connection_and_dropped_with_it() {
    let h = Harness::start();
    let reads = Arc::new(AtomicUsize::new(0));
    let drops: Arc<Mutex<Vec<Arc<AtomicBool>>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let reads = Arc::clone(&reads);
        let drops = Arc::clone(&drops);
        h.rest.set_fetch(move |scope: &ConnectionScope, what, credentialed, _| {
            assert!(credentialed, "this lane is the credentialed one");
            assert_eq!(what.lane(), "klines");
            let memo = scope.slot::<TokenMemo>();
            let reads = Arc::clone(&reads);
            let drops = Arc::clone(&drops);
            let answer = memo.get_or_read(|| {
                reads.fetch_add(1, Ordering::SeqCst);
                let dropped = Arc::new(AtomicBool::new(false));
                drops.lock().unwrap_or_else(PoisonError::into_inner).push(Arc::clone(&dropped));
                Ok(Redacted::new(Probe { dropped }))
            });
            assert!(answer.is_ok());
            Ok(FetchRows::Bars(bars(1)))
        });
    }
    {
        // One request: a source built per request holds ONE connection, so three chunks read once.
        let first = RemoteKlines::credentialed(h.link(), "oanda");
        for _ in 0..3 {
            first.fetch("EUR_USD", "S5", 0, 1).expect("credentialed fetch");
        }
        assert_eq!(
            reads.load(Ordering::SeqCst),
            1,
            "the store was read more than once for one request"
        );
        // A second request is a second connection and reads again: a key rotated between requests is
        // what the next one sees.
        let second = RemoteKlines::credentialed(h.link(), "oanda");
        second.fetch("EUR_USD", "S5", 0, 1).expect("credentialed fetch");
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        let first_dropped = Arc::clone(&drops.lock().unwrap_or_else(PoisonError::into_inner)[0]);
        assert!(
            !first_dropped.load(Ordering::SeqCst),
            "the credential was dropped while its request lived"
        );
        drop(first);
        assert!(
            raised(&first_dropped, Duration::from_secs(5)),
            "the credential outlived the connection that read it"
        );
        drop(second);
    }
}

/// **The fetch workers are capped daemon-wide, and a worker that outlived its connection still
/// counts.** The connection cap bounds the threads that ASK; a worker is the thread inside the venue
/// call, and a venue call has no probe, so a client that starts a long fetch and drops - again and
/// again - used to leave a worker per attempt inside its call, each holding its answer. With the cap a
/// fetch past it is refused with a transient `Error` before any venue is asked, the lingering workers
/// hold their places after their connections are gone, and a place comes back when its worker
/// returns.
#[test]
fn the_worker_cap_counts_a_worker_that_outlived_its_dropped_connection() {
    let mut cfg = ServerConfig::PRODUCTION;
    cfg.limits.max_fetches = 2;
    // The connection thread gives up on a worker stuck in a venue call quickly.
    cfg.timings.stop_drain = Duration::from_millis(150);
    let h = Harness::start_config(cfg);
    let release = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicUsize::new(0));
    let (rel, count) = (Arc::clone(&release), Arc::clone(&started));
    h.rest.set_fetch(move |_, _, _, _ctx| {
        // A venue call with no probe: it never looks at the cancel.
        count.fetch_add(1, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !rel.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(FetchRows::Bars(Vec::new()))
    });

    // Two fetches fill the cap (both ACCEPTED: exactly at the cap is fine), then both clients drop.
    for rid in 1..=2 {
        let mut raw = Raw::open(h.addr, &test_keys(), Scope::Write);
        raw.send(&klines_request(rid, "binance"));
        assert!(matches!(raw.recv::<Response>(), Response::FetchProgress(_)), "fetch {rid}");
        drop(raw);
    }
    let began = Instant::now();
    while started.load(Ordering::SeqCst) < 2 {
        assert!(began.elapsed() < Duration::from_secs(5), "the workers never started");
        std::thread::sleep(Duration::from_millis(5));
    }
    // Let both connection threads finish their drain and leave: only the workers are left.
    std::thread::sleep(Duration::from_millis(500));

    // A third fetch is refused before the lane is asked, in words that say it is transient.
    let mut third = Raw::open(h.addr, &test_keys(), Scope::Write);
    third.send(&klines_request(3, "binance"));
    match third.recv::<Response>() {
        Response::Error(text) => {
            assert!(
                text.contains("already running 2 fetches") && text.contains("transient"),
                "{text}"
            );
        }
        other => panic!("a fetch past the cap was served: {other:?}"),
    }
    assert_eq!(started.load(Ordering::SeqCst), 2, "the refused fetch reached the venue lane");
    // The refusal is a bad request, not a bad connection.
    third.send(&Request::Ping);
    assert!(matches!(third.recv::<Response>(), Response::Pong));

    // A place comes back when a worker RETURNS.
    release.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        third.send(&klines_request(4, "binance"));
        match third.recv::<Response>() {
            Response::FetchProgress(_) => break,
            Response::Error(_) => {}
            other => panic!("{other:?}"),
        }
        assert!(Instant::now() < deadline, "the workers returned but the cap never opened");
        std::thread::sleep(Duration::from_millis(20));
    }
}
