use super::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// **The depth dial goes through the BOUNDED shared arm** — the test this defect needed and did
/// not have. [`connect_depth`] hand-rolled `tungstenite::connect`, which applies no connect
/// bound at all, and the market-pump work that bounded every other on-driver dial left this
/// driver alone on the claim that it was out of the recorder's path. It is not: see
/// [`connect_depth`]'s own doc for the chain (`RecorderSink::l2_snapshot` → `kind=depth`, and
/// `Stream::ALL` → `subscribe_depth`, which binance/bybit/okx all serve).
///
/// **How it observes the path with no network.** `crate::ws_proxy::connect_ws` branches on
/// `connect_timeout`: `Some` parses the TCP target ITSELF first (`ws_target`, whose rejection is
/// the distinctive `"bad ws url"`), `None` hands the whole string to `tungstenite::connect`,
/// which fails in its own parser with its own wording. An unparseable host therefore makes the
/// two arms say different things, offline and deterministically. The second half asserts they
/// really do differ, so the discriminator cannot decay into something both arms satisfy.
#[test]
fn the_depth_dial_goes_through_the_bounded_shared_path() {
    let bounded = match connect_depth("not a url", Duration::from_secs(2)) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("an unparseable url cannot dial"),
    };
    assert!(
        bounded.starts_with("bad ws url"),
        "the depth dial did not take `connect_ws`'s BOUNDED arm — it is dialing unbounded again, \
             so a black-holed route pins this thread past the stop flag. error was: {bounded}"
    );

    // …and the discriminator discriminates: the unbounded arm fails somewhere else entirely.
    let unbounded = connect_ws("not a url", None, None)
        .expect_err("an unparseable url cannot dial")
        .to_string();
    assert!(
        !unbounded.starts_with("bad ws url"),
        "the unbounded arm must be distinguishable from the bounded one, or this test proves \
             nothing. error was: {unbounded}"
    );
}

/// The depth driver spends the SAME window the market pump does — one constant, so the
/// recorder's `FEED_STOP_BUDGET_SECS` has one number to be derived from. A local copy here
/// would be a second thing to forget.
#[test]
fn the_depth_dial_window_is_the_shared_one() {
    assert_eq!(
        CONNECT_10S,
        crate::pump_spec::market_pump_spec("binance").knobs().connect_timeout.unwrap()
    );
    assert_eq!(
        CONNECT_10S,
        crate::pump_spec::market_pump_spec("bybit").knobs().connect_timeout.unwrap()
    );
    assert_eq!(
        CONNECT_10S,
        crate::pump_spec::market_pump_spec("okx").knobs().connect_timeout.unwrap()
    );
}

#[test]
fn infer_tick_takes_the_min_adjacent_gap() {
    // dense levels one tick (0.01) apart on each side → inferred tick 0.01
    let bids =
        vec![BookLevel::new(100.00, 1.0), BookLevel::new(99.99, 2.0), BookLevel::new(99.98, 3.0)];
    let asks = vec![BookLevel::new(100.02, 1.0), BookLevel::new(100.03, 1.0)];
    assert!((infer_tick_size(&bids, &asks) - 0.01).abs() < 1e-9);
}

#[test]
fn infer_tick_uses_both_sides_and_orders_them() {
    // unsorted, cross-side: the smallest gap (0.5, between the ask 101.0 and bid 100.5) wins
    let bids = vec![BookLevel::new(100.5, 1.0), BookLevel::new(100.0, 1.0)];
    let asks = vec![BookLevel::new(102.0, 1.0), BookLevel::new(101.0, 1.0)];
    assert!((infer_tick_size(&bids, &asks) - 0.5).abs() < 1e-9);
}

#[test]
fn infer_tick_falls_back_without_two_levels() {
    assert_eq!(infer_tick_size(&[], &[]), 0.01);
    assert_eq!(infer_tick_size(&[BookLevel::new(100.0, 1.0)], &[]), 0.01);
}

// ---- net-hardening §B: idle watchdog + gap/recovery disclosure over a scripted DepthStream ----

// The shared scripted DepthStream double (testing-arch Phase 4c) — the canonical copy of the
// `Step`/`ScriptedDepthStream` pair that used to live inline here. `send_text` records what
// the driver sent (subscribe + keepalive, read back via `.sent()`); `stalled`/`stalled_clocked`
// drive the §B idle/freshness watchdogs, the shared clock aging per read so consuming the
// script ages the book with no real sleep.
use crate::scripted::ScriptedStream as ScriptedDepthStream;

/// A tiny test book so `on_book` has something to report; content is irrelevant to the watchdog.
fn a_book() -> L2Book {
    let mut b = L2Book::new(1.0);
    b.apply_snapshot(1, &[BookLevel::new(100.0, 1.0)], &[BookLevel::new(101.0, 1.0)]);
    b
}

/// Drive one session with a caller-provided [`StreamHealth`] (so a test can pre-open a transport
/// gap to exercise recovery) + injected `now_ms`, recording `on_health`/`on_book` calls in order.
/// `decode`: `"gap"` → Gap, `"ignore"` → Ignored, a bare integer text frame → `Updated(<that
/// i64>)` (its venue event-ts, so a test can feed a stale-STAMPED update), any other text →
/// `Updated(0)` = receive-time fallback (the pre-change behavior the existing freshness tests rely
/// on). Returns `(log, result)`. The `on_health` recorder stringifies each [`HealthEvent`] under
/// the same `status:` prefix the old driver-local status recorder used, so the migrated assertions
/// read almost unchanged.
fn drive_clocked_h(
    stream: &mut ScriptedDepthStream,
    stream_health: &mut StreamHealth,
    idle_threshold: Duration,
    reseed_interval: Option<Duration>,
    now_ms: &dyn Fn() -> i64,
    stop: &AtomicBool,
) -> (Vec<String>, Result<SessionOutcome, String>) {
    let log = RefCell::new(Vec::<String>::new());
    let mut seed = || None::<L2Book>;
    let mut decode = |txt: &str, book: &mut Option<L2Book>| -> BookOp {
        match txt {
            "gap" => BookOp::Gap,
            "ignore" => BookOp::Ignored,
            // A bare integer text frame IS the venue event-ts (epoch-ms) this update carries — lets
            // a test feed a stale-STAMPED update; any other text (`"snap"`) parses to `0` =
            // receive-time fallback, the pre-change behavior the other freshness tests rely on.
            _ => {
                *book = Some(a_book());
                BookOp::Updated(txt.parse::<i64>().unwrap_or(0))
            }
        }
    };
    let mut on_book = |b: &L2Book| log.borrow_mut().push(format!("book:{:?}", b.best_bid()));
    let mut on_health = |ev: HealthEvent| log.borrow_mut().push(format!("status:{ev:?}"));
    let res = run_depth_session(
        stream,
        None,
        None,
        &mut seed,
        &mut decode,
        &mut on_book,
        &mut on_health,
        stream_health,
        stop,
        idle_threshold,
        reseed_interval,
        now_ms,
    )
    .map_err(|e| e.to_string());
    (log.into_inner(), res)
}

/// Like [`drive_clocked_h`] but builds a FRESH [`StreamHealth`] from `freshness_threshold` — the
/// default for the freshness tests (no prior transport gap, so `recover()` is a no-op on the first
/// book, matching a first connect exactly as the venue's old transport-gap tracker did). Reseed is
/// OFF (`None`); [`drive_reseed`] threads a `reseed_interval` for the periodic-reseed tests.
fn drive_clocked(
    stream: &mut ScriptedDepthStream,
    idle_threshold: Duration,
    freshness_threshold: Duration,
    now_ms: &dyn Fn() -> i64,
    stop: &AtomicBool,
) -> (Vec<String>, Result<SessionOutcome, String>) {
    drive_reseed(stream, idle_threshold, freshness_threshold, None, now_ms, stop)
}

/// Like [`drive_clocked`] but threads a `reseed_interval` (the timed book re-seed) — for the
/// periodic-reseed tests. Fresh [`StreamHealth`] (no prior gap), so the only way the session ends
/// with [`SessionOutcome::Reseed`] is the reseed timer, not a recovered gap.
fn drive_reseed(
    stream: &mut ScriptedDepthStream,
    idle_threshold: Duration,
    freshness_threshold: Duration,
    reseed_interval: Option<Duration>,
    now_ms: &dyn Fn() -> i64,
    stop: &AtomicBool,
) -> (Vec<String>, Result<SessionOutcome, String>) {
    let mut health = StreamHealth::new(freshness_threshold.as_millis() as i64);
    drive_clocked_h(stream, &mut health, idle_threshold, reseed_interval, now_ms, stop)
}

/// The idle/recovery tests don't exercise DATA-freshness: a threshold larger than any scripted
/// clock advance + a constant `now_ms` keep that watchdog dormant, so these read exactly as
/// before (the freshness state machine is proven separately, below).
fn drive(
    stream: &mut ScriptedDepthStream,
    idle_threshold: Duration,
    stop: &AtomicBool,
) -> (Vec<String>, Result<SessionOutcome, String>) {
    drive_clocked(stream, idle_threshold, Duration::from_secs(86_400), &|| 0, stop)
}

/// Like [`drive`] (dormant freshness watchdog, constant clock) but with a caller-provided
/// [`StreamHealth`] — for the recovery test, which pre-opens a transport gap so the first book's
/// `recover()` fires with a `Live`.
fn drive_h(
    stream: &mut ScriptedDepthStream,
    stream_health: &mut StreamHealth,
    idle_threshold: Duration,
    stop: &AtomicBool,
) -> (Vec<String>, Result<SessionOutcome, String>) {
    drive_clocked_h(stream, stream_health, idle_threshold, None, &|| 0, stop)
}

/// A stalled-but-open stream (read-timeouts, no frames) past the idle threshold must return `Err`
/// so `run_depth_feed` reconnects — the dead-transport detection itself. `stall = 60s` vs a
/// `30s` threshold trips on the first timeout tick, with no real sleep.
#[test]
fn idle_watchdog_trips_when_stalled_past_threshold() {
    let mut stream = ScriptedDepthStream::stalled(Duration::from_secs(60));
    stream.push_timeout();
    let stop = AtomicBool::new(false);
    let (log, res) = drive(&mut stream, Duration::from_secs(30), &stop);
    let err = res.expect_err("a stalled stream must Err so the feed reconnects and opens a gap");
    assert!(err.contains("idle"), "the error names the idle-watchdog trip: {err}");
    assert!(log.is_empty(), "no data flowed, nothing published: {log:?}");
}

/// A connection kept alive purely by keepalive traffic keeps `since_last_frame` fresh, so MANY
/// read-timeout ticks are absorbed WITHOUT a false idle trip. Only true silence (nothing inbound
/// for the whole threshold) may trip. Here the 5s liveness clock stays under the 30s threshold,
/// so the session ends on the scripted close, not an idle trip.
#[test]
fn a_keepalive_fresh_stream_is_not_falsely_declared_idle() {
    let mut stream = ScriptedDepthStream::stalled(Duration::from_secs(5));
    stream.push_timeout();
    stream.push_timeout();
    stream.push_timeout();
    let stop = AtomicBool::new(false);
    let (log, res) = drive(&mut stream, Duration::from_secs(30), &stop);
    let err = res.expect_err("the script ends by exhaustion (Closed), not an idle trip");
    assert!(!err.contains("idle"), "a keepalive-fresh stream must NOT trip the watchdog: {err}");
    assert!(log.is_empty(), "no data flowed, nothing disclosed: {log:?}");
}

/// The first book of a session that RECOVERS from an open transport gap discloses `Live` (the
/// recovery) BEFORE the book itself, so a consumer sees "recovered" ahead of the re-seeded data
/// (§B ordering, via the pre-publish `recover()` check). Recovery is gap-aware now — the driver
/// owns the `StreamHealth` — so we pre-open a gap (a prior outage) to exercise it. This is the
/// translation of the old recovered-before-book test: the old DRIVER always emitted a recovered
/// status and the venue's transport-gap tracker turned it into `Live` (gap open) or a no-op (no
/// gap); the dedup now lives in the driver, so `recover()` emits `Live` ONLY when a gap is open.
/// End-to-end the venue sink sees the SAME thing (a `Live` closing the gap, before the book).
#[test]
fn recovery_is_disclosed_before_the_first_book() {
    let mut health = StreamHealth::new(86_400_000);
    health.enter_gap(1000); // a prior outage opened a transport gap
    let mut stream = ScriptedDepthStream::from_texts(["snap"]);
    let stop = AtomicBool::new(false);
    let (log, _res) = drive_h(&mut stream, &mut health, Duration::from_secs(30), &stop);
    assert_eq!(
        log[0], "status:Live { gap_started_ts_ms: Some(1000) }",
        "recovery (Live) disclosed first, closing the open gap: {log:?}"
    );
    assert!(log[1].starts_with("book:"), "then the re-seeded book: {log:?}");
    // exactly one Live per session — a second book does not re-disclose it
    let mut health2 = StreamHealth::new(86_400_000);
    health2.enter_gap(1000);
    let mut stream2 = ScriptedDepthStream::from_texts(["snap", "snap"]);
    let (log2, _) = drive_h(&mut stream2, &mut health2, Duration::from_secs(30), &stop);
    assert_eq!(
        log2.iter().filter(|c| c.contains("Live")).count(),
        1,
        "the recovery Live fires once per session, not per book: {log2:?}"
    );
}

/// A first connect with NO open transport gap discloses nothing before the first book —
/// `recover()` is a no-op (returns `None`), matching the venue's old transport-gap tracker's no-op
/// recover on first connect. (The recovered status the old DRIVER always emitted was a venue-side
/// no-op in exactly this case; the dedup moved into the driver's `StreamHealth`, so the venue sink
/// still sees the same thing: only the book.)
#[test]
fn a_first_connect_with_no_gap_discloses_no_recovery() {
    let mut stream = ScriptedDepthStream::from_texts(["snap"]);
    let stop = AtomicBool::new(false);
    let (log, _res) = drive(&mut stream, Duration::from_secs(30), &stop);
    assert!(
        log[0].starts_with("book:"),
        "no recovery disclosed on a gapless first connect — the book is first: {log:?}"
    );
    assert!(
        log.iter().all(|c| !c.starts_with("status:")),
        "a gapless session discloses no stream-health status: {log:?}"
    );
}

/// A sequence-gap frame ends the session with `Err` (so the reconnect loop opens a gap) and — a
/// gap on the very first frame, before any book — publishes nothing, so no `Recovered` leaks out.
#[test]
fn a_sequence_gap_frame_ends_the_session_with_err_and_no_recovery() {
    let mut stream = ScriptedDepthStream::from_texts(["gap"]);
    let stop = AtomicBool::new(false);
    let (log, res) = drive(&mut stream, Duration::from_secs(30), &stop);
    assert!(res.is_err(), "a gap frame ends the session so the driver resyncs");
    assert!(log.is_empty(), "no book published before the gap, so no disclosure: {log:?}");
}

/// A stop flag already raised before the first read returns `Ok(())` immediately, touching
/// nothing — the deterministic-teardown property.
#[test]
fn stop_flag_set_before_the_first_read_exits_cleanly() {
    let mut stream = ScriptedDepthStream::from_texts(["snap"]);
    let stop = AtomicBool::new(true);
    let (log, res) = drive(&mut stream, Duration::from_secs(30), &stop);
    assert!(res.is_ok(), "a pre-raised stop exits Ok");
    assert!(log.is_empty(), "a pre-raised stop must not process any frame: {log:?}");
}

/// **A stop already raised must skip the REST book SEED, not merely the read loop** — the test
/// this defect needed and did not have.
///
/// [`stop_flag_set_before_the_first_read_exits_cleanly`] above looks like it covers this and does
/// not: its `seed` returns `None` and records nothing, so it passed unchanged while the seed ran
/// on every stopped session. That matters because the seed is a REST round trip on binance/aster
/// — the LONGEST thing between a session's dial and the loop's first flag read, and the recorder's
/// `FEED_STOP_BUDGET_SECS` is derived from the largest single position a feed thread can be caught
/// in. A `stop` raised during the (bounded) dial would otherwise buy a whole extra network call.
///
/// MUTATION PROOF: delete the `stop` check in front of `let mut book = seed();` and this goes
/// red on `the seed ran anyway` — no network, no timing, no clock, so it fails the same way on
/// any box.
#[test]
fn a_stop_raised_before_the_session_skips_the_rest_seed() {
    let seeded = Cell::new(false);
    let stop = AtomicBool::new(true);
    let mut stream = ScriptedDepthStream::from_texts(["snap"]);
    let mut seed = || {
        seeded.set(true);
        Some(a_book())
    };
    let mut decode = |_t: &str, _b: &mut Option<L2Book>| BookOp::Ignored;
    let mut on_book = |_b: &L2Book| {};
    let mut on_health = |_ev: HealthEvent| {};
    let mut health = StreamHealth::new(86_400_000);
    let res = run_depth_session(
        &mut stream,
        None,
        None,
        &mut seed,
        &mut decode,
        &mut on_book,
        &mut on_health,
        &mut health,
        &stop,
        Duration::from_secs(30),
        None,
        &|| 0,
    );
    assert!(
        matches!(res, Ok(SessionOutcome::Stopped)),
        "a pre-raised stop ends the session cleanly"
    );
    assert!(
        !seeded.get(),
        "the seed ran anyway — a stop landing during the dial now pays for a whole extra REST \
             round trip on top of it, which is outside the recorder's FEED_STOP_BUDGET_SECS \
             derivation. Poll `stop` before calling `seed`."
    );
}

/// The subscribe frame is written once, before the first read.
#[test]
fn subscribe_is_sent_before_the_first_read() {
    let mut stream = ScriptedDepthStream::from_texts(["snap"]);
    let stop = AtomicBool::new(false);
    let log = RefCell::new(Vec::<String>::new());
    let mut seed = || None::<L2Book>;
    let mut decode = |_t: &str, book: &mut Option<L2Book>| {
        *book = Some(a_book());
        BookOp::Updated(0)
    };
    let mut on_book = |_b: &L2Book| log.borrow_mut().push("book".into());
    let mut on_health = |_ev: HealthEvent| {};
    let mut health = StreamHealth::new(86_400_000);
    let _ = run_depth_session(
        &mut stream,
        Some(r#"{"op":"subscribe"}"#),
        None,
        &mut seed,
        &mut decode,
        &mut on_book,
        &mut on_health,
        &mut health,
        &stop,
        Duration::from_secs(30),
        None, // reseed off
        &|| 0,
    );
    assert_eq!(stream.sent().first().map(String::as_str), Some(r#"{"op":"subscribe"}"#));
}

// ---- net-hardening §B: DATA-freshness watchdog (the data-side twin of the idle watchdog) ----
//
// Transport stays ALIVE (`stall` well under `idle_threshold`, so the idle watchdog never trips);
// the injected `now_ms` reads a clock the `ScriptedDepthStream` ages a fixed amount per read, so
// the book's data-age is driven deterministically by consuming the script — zero real sleeps.

/// A shared data clock + a `now_ms` closure reading it, for the freshness tests.
fn clock_and_now() -> (Rc<Cell<i64>>, impl Fn() -> i64) {
    let clock = Rc::new(Cell::new(0_i64));
    let now = {
        let c = Rc::clone(&clock);
        move || c.get()
    };
    (clock, now)
}

/// Pulls the `i64` value of a named field out of one recorded `status:Variant { field: N, .. }`
/// log entry (Debug-format parsing — `drive_clocked`'s log is `Vec<String>`, not raw
/// `HealthEvent`). Used below to prove a recovery `Live { gap_started_ts_ms }` echoes the exact
/// `now_ms` of the `Stale` disclosure it closes, without restructuring the string-based recorder
/// every other test in this module already relies on. Skips any non-numeric prefix after the
/// field key (e.g. an `Option`'s `Some(`), so both a bare `newest_data_ts_ms: 20000` and a wrapped
/// `gap_started_ts_ms: Some(60000)` parse to their inner `i64`.
fn field_i64(entry: &str, field: &str) -> i64 {
    let key = format!("{field}: ");
    let at = entry.find(&key).unwrap_or_else(|| panic!("{field} not found in {entry:?}"));
    let rest = &entry[at + key.len()..];
    // Skip any non-numeric prefix (e.g. an `Option`'s `Some(`) to the value's first digit / `-`.
    let start = rest
        .find(|c: char| c.is_ascii_digit() || c == '-')
        .unwrap_or_else(|| panic!("no numeric value for {field} in {entry:?}"));
    let rest = &rest[start..];
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '-')).unwrap_or(rest.len());
    rest[..end].parse::<i64>().unwrap_or_else(|_| panic!("failed to parse {field} from {entry:?}"))
}

/// (1) Book updates keep arriving, each within the freshness window (the clock advances 10s per
/// read, threshold 30s) → the data watchdog stays quiet: NO `Stale` ever disclosed.
#[test]
fn steady_book_updates_never_go_stale() {
    let (clock, now) = clock_and_now();
    let mut stream = ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 10_000);
    for _ in 0..3 {
        stream.push_text("snap");
        stream.push_text("snap");
        stream.push_timeout();
    }
    let stop = AtomicBool::new(false);
    let (log, _res) = drive_clocked(
        &mut stream,
        Duration::from_secs(300), // idle threshold — far above `stall`, never trips
        Duration::from_secs(30),  // freshness threshold
        &now,
        &stop,
    );
    assert!(
        !log.iter().any(|c| c.contains("Stale")),
        "a book updated within the freshness window must never be declared stale: {log:?}"
    );
}

/// (2) Transport stays alive but NO book update lands past the freshness window (only `Timeout`
/// ticks after one seed update; the clock advances 20s per read, threshold 30s) → EXACTLY ONE
/// `Stale`, and never a `Gap` (the idle watchdog never trips — the transport is alive).
#[test]
fn a_frozen_book_behind_a_live_transport_goes_stale_once() {
    let (clock, now) = clock_and_now();
    let mut stream = ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 20_000);
    stream.push_text("snap"); // one update at t=20s → the newest data ts
    stream.push_timeout(); // t=40s, age 20s < 30s
    stream.push_timeout(); // t=60s, age 40s > 30s → Stale
    stream.push_timeout(); // t=80s, age 60s — already stale, no re-fire
    stream.push_timeout(); // t=100s, still stale
    let stop = AtomicBool::new(false);
    let (log, res) =
        drive_clocked(&mut stream, Duration::from_secs(300), Duration::from_secs(30), &now, &stop);
    let stale = log.iter().filter(|c| c.contains("Stale")).count();
    assert_eq!(stale, 1, "a frozen book discloses Stale exactly once per episode: {log:?}");
    assert!(
        !log.iter().any(|c| c.contains("Gap")),
        "the transport stayed alive — no transport Gap may be disclosed: {log:?}"
    );
    let err = res.expect_err("the script ends by exhaustion (Closed), not an idle trip");
    assert!(!err.contains("idle"), "the freshness trip is NOT an idle-transport trip: {err}");
}

/// (3) After a `Stale` episode a book update resumes → EXACTLY ONE recovery `Live`, disclosed
/// BEFORE the resuming book (recovery-before-data). The old driver-local "fresh" status maps to
/// `HealthEvent::Live` (freshness recovery reuses the shared `Live`, exactly as the venue does
/// onto `vike_data::StreamStatus::Live`); this test uses a gapless session, so the only `Live` in
/// the log is this freshness recovery (no transport `Live`).
#[test]
fn a_resuming_book_update_discloses_fresh_once_before_its_book() {
    let (clock, now) = clock_and_now();
    let mut stream = ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 20_000);
    stream.push_text("snap"); // t=20s update
    stream.push_timeout(); // t=40s, age 20s
    stream.push_timeout(); // t=60s, age 40s > 30s → Stale
    stream.push_text("snap"); // t=80s update → Fresh, then the book
    stream.push_timeout(); // t=100s, age 20s — fresh again, quiet
    let stop = AtomicBool::new(false);
    let (log, _res) =
        drive_clocked(&mut stream, Duration::from_secs(300), Duration::from_secs(30), &now, &stop);
    assert_eq!(
        log.iter().filter(|c| c.contains("Live")).count(),
        1,
        "a resumed book update closes the episode with exactly one recovery Live: {log:?}"
    );
    let live_i = log.iter().position(|c| c.contains("Live")).expect("a Live was disclosed");
    let stale_i = log.iter().position(|c| c.contains("Stale")).expect("a Stale preceded it");
    assert!(stale_i < live_i, "Stale precedes the recovery Live: {log:?}");
    assert!(
        log[live_i + 1].starts_with("book:"),
        "the recovery Live is disclosed BEFORE the resuming book: {log:?}"
    );
    // Trip-time echo (mirrors `vike_data::StreamStatus::Live`'s `gap_started_ts_ms` contract,
    // and Polymarket's freshness recovery): the closing `Live`'s `gap_started_ts_ms` must equal
    // the `now_ms` the preceding `Stale` was judged at, not e.g. the `newest_data_ts_ms` or a
    // fresh clock read.
    assert_eq!(
        field_i64(&log[live_i], "gap_started_ts_ms"),
        field_i64(&log[stale_i], "now_ms"),
        "the recovery Live must echo the Stale episode's trip time (now_ms): {log:?}"
    );
}

/// (4) A book updated within the freshness window, then a quiet stretch that never ages past it
/// (clock advances 5s per read, threshold 30s) → sparse-but-current: NO `Stale`.
#[test]
fn a_sparse_but_current_book_is_not_stale() {
    let (clock, now) = clock_and_now();
    let mut stream = ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 5_000);
    stream.push_text("snap"); // t=5s update
    for _ in 0..4 {
        stream.push_timeout(); // t=10s..25s, age never exceeds 20s < 30s
    }
    let stop = AtomicBool::new(false);
    let (log, _res) =
        drive_clocked(&mut stream, Duration::from_secs(300), Duration::from_secs(30), &now, &stop);
    assert!(
        !log.iter().any(|c| c.contains("Stale")),
        "a recently-updated book stays fresh through a quiet stretch under the window: {log:?}"
    );
}

/// (5) Re-arm: TWO freshness episodes in one session prove the stale episode resets per-episode,
/// not just once. Script: update → age past threshold → `Stale#1` → update resumes → recovery
/// `Live#1` → age past threshold again → `Stale#2` (the script ends there, so the SECOND episode
/// never recovers). Exactly two `Stale` and exactly one recovery `Live` are disclosed, and — since
/// the transport never drops in this (gapless) script — no `Gap` either.
#[test]
fn a_second_freshness_episode_re_arms_after_the_first_recovers() {
    let (clock, now) = clock_and_now();
    let mut stream = ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 20_000);
    stream.push_text("snap"); // t=20s update
    stream.push_timeout(); // t=40s, age 20s
    stream.push_timeout(); // t=60s, age 40s > 30s → Stale #1
    stream.push_text("snap"); // t=80s update → Fresh #1, then the book
    stream.push_timeout(); // t=100s, age 20s — fresh again, quiet
    stream.push_timeout(); // t=120s, age 40s > 30s → Stale #2 (re-armed; unresolved — script ends)
    let stop = AtomicBool::new(false);
    let (log, _res) =
        drive_clocked(&mut stream, Duration::from_secs(300), Duration::from_secs(30), &now, &stop);
    let stale = log.iter().filter(|c| c.contains("Stale")).count();
    let live = log.iter().filter(|c| c.contains("Live")).count();
    assert_eq!(stale, 2, "two separate episodes must each disclose their own Stale: {log:?}");
    assert_eq!(live, 1, "only the first episode recovers within this script (one Live): {log:?}");
    assert!(
        !log.iter().any(|c| c.contains("Gap")),
        "the transport never dropped in this script — no Gap may be disclosed: {log:?}"
    );
}

/// (6) THE NEW CAPABILITY: a frame that ARRIVES now but is STAMPED old — a venue replaying stale
/// data. The transport stays alive (`stall` ≪ idle threshold) and the injected `now_ms` barely
/// advances across the frame's arrival and the judging tick (1s/read), so a RECEIVE-time clock
/// would read the update as fresh (it just landed) and NEVER trip. Because the driver now clocks
/// staleness off the VENUE EVENT-TIME the `Updated` carries (here `5_000` epoch-ms, ~245s behind a
/// "now" pre-advanced to ~250s), the very next timeout tick judges it stale → EXACTLY ONE `Stale`,
/// and — the transport never dropped — no `Gap`.
#[test]
fn a_stale_stamped_update_behind_a_live_transport_goes_stale_once() {
    let (clock, now) = clock_and_now();
    clock.set(250_000); // "now" is already well past the stale event-stamp fed below
    let mut stream = ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 1_000);
    stream.push_text("5000"); // an Updated STAMPED at t=5s (epoch-ms) — ~245s behind "now"…
    stream.push_timeout(); // …now ≈ 252s, age = 252s − 5s = 247s > 30s → Stale (receive-time: age ≈ 1s, no trip)
    stream.push_timeout(); // still stale — no re-fire
    stream.push_timeout();
    let stop = AtomicBool::new(false);
    let (log, res) = drive_clocked(
        &mut stream,
        Duration::from_secs(300), // idle threshold — transport stays alive throughout
        Duration::from_secs(30),  // freshness threshold
        &now,
        &stop,
    );
    let stale = log.iter().filter(|c| c.contains("Stale")).count();
    assert_eq!(
        stale, 1,
        "a stale-STAMPED update (old venue ts) behind a live transport trips Stale exactly once, \
             even though the frame just arrived — receive-time freshness would MISS it: {log:?}"
    );
    // The Stale must carry the VENUE event-ts (5_000), not the receive-time (~251_000) — proof the
    // freshness clock is the venue stamp, which is the whole point of this change.
    let stale_entry = log.iter().find(|c| c.contains("Stale")).expect("a Stale was disclosed");
    assert_eq!(
        field_i64(stale_entry, "newest_data_ts_ms"),
        5_000,
        "Stale.newest_data_ts_ms is the venue event-time of the update, not receive-time: {log:?}"
    );
    assert!(
        !log.iter().any(|c| c.contains("Gap")),
        "the transport stayed alive — no transport Gap may be disclosed: {log:?}"
    );
    let err = res.expect_err("the script ends by exhaustion (Closed), not an idle trip");
    assert!(!err.contains("idle"), "the freshness trip is NOT an idle-transport trip: {err}");
}

/// (7) THE §B ZERO-DATA REGRESSION this fix restores: a session that SEEDS/publishes a first book
/// but then receives ZERO `BookOp::Updated` frames behind a LIVE transport (only timeout ticks,
/// `stall` ≪ idle threshold) must STILL go `Stale` — via the arm-at-session-start floor. This is
/// the capability the old inline freshness had (it armed `last_update_ms = now_ms()` at session
/// start) that a naive `reset_freshness` clearing `newest_ts` to `None` would lose: a subscribe
/// that silently sends nothing (worst on REST-seeded Binance — the seed closes the transport gap,
/// so nothing else catches the frozen book). Exactly one `Stale` — carrying the session-start arm
/// time as `newest_data_ts_ms` (no real data ever landed) — and no `Gap` (transport stayed alive).
#[test]
fn a_seeded_but_dataless_session_goes_stale() {
    let (clock, now) = clock_and_now();
    clock.set(1_000); // session-start "now" — the arm floor
    let mut stream = ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 20_000);
    // NO data frames — only timeout ticks, so zero `BookOp::Updated`; the clock ages 20s/read.
    stream.push_timeout(); // now → 21_000, age 20s < 30s → still fresh
    stream.push_timeout(); // now → 41_000, age 40s > 30s → Stale (from the 1_000 arm floor)
    stream.push_timeout(); // now → 61_000, still stale — no re-fire
    let stop = AtomicBool::new(false);

    // A seed that publishes a first book (the REST snapshot), mirroring Binance; then only
    // timeouts. `seed` is called once by `run_depth_session`, so this returns the one book.
    let log = RefCell::new(Vec::<String>::new());
    let mut seed = || Some(a_book());
    let mut decode = |_txt: &str, _book: &mut Option<L2Book>| -> BookOp { BookOp::Ignored };
    let mut on_book = |b: &L2Book| log.borrow_mut().push(format!("book:{:?}", b.best_bid()));
    let mut on_health = |ev: HealthEvent| log.borrow_mut().push(format!("status:{ev:?}"));
    let mut health = StreamHealth::new(Duration::from_secs(30).as_millis() as i64);
    let _ = run_depth_session(
        &mut stream,
        None,
        None,
        &mut seed,
        &mut decode,
        &mut on_book,
        &mut on_health,
        &mut health,
        &stop,
        Duration::from_secs(300), // idle threshold — far above `stall`, never trips
        None,                     // reseed off
        &now,
    );
    let log = log.into_inner();
    assert!(log[0].starts_with("book:"), "the seed book is published first: {log:?}");
    let stale = log.iter().filter(|c| c.contains("Stale")).count();
    assert_eq!(
        stale, 1,
        "a seeded-but-dataless session still trips Stale exactly once from the arm floor: {log:?}"
    );
    let stale_entry = log.iter().find(|c| c.contains("Stale")).expect("a Stale was disclosed");
    assert_eq!(
        field_i64(stale_entry, "newest_data_ts_ms"),
        1_000,
        "Stale carries the session-start arm time (no real data ever landed): {log:?}"
    );
    assert!(
        !log.iter().any(|c| c.contains("Gap")),
        "the transport stayed alive — no transport Gap may be disclosed: {log:?}"
    );
}

/// (8) DIVERGENCE-2 PIN (accepted improvement): a stale-STAMPED update that ARRIVES while the book
/// is already `Stale` must NOT falsely recover. The old inline freshness flapped here — an update
/// received during a stale episode set `last_update_ms` and cleared stale, emitting a spurious
/// `Live`, even though the update's OWN (old) event-time was still past the threshold; the very
/// next tick then re-tripped `Stale` (a Live/Stale flap). The unified `StreamHealth` clocks
/// recovery off the update's VENUE event-time via `newest_ts`, so a stale-stamped update keeps the
/// episode open — recovery fires only when data is genuinely fresh again. Exactly one `Stale`,
/// zero `Live`, and the following tick discloses nothing new.
#[test]
fn a_stale_stamped_update_while_stale_does_not_falsely_recover() {
    let (clock, now) = clock_and_now();
    let mut stream = ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 20_000);
    stream.push_text("snap"); // t=20s: a fresh update (receive-time stamp) → newest data ts 20s
    stream.push_timeout(); // t=40s, age 20s < 30s
    stream.push_timeout(); // t=60s, age 40s > 30s → Stale
    stream.push_text("25000"); // t=80s: an update STAMPED at 25s — still 55s (> 30s) behind now
    stream.push_timeout(); // t=100s: still stale — no re-fire, no recovery
    let stop = AtomicBool::new(false);
    let (log, _res) =
        drive_clocked(&mut stream, Duration::from_secs(300), Duration::from_secs(30), &now, &stop);
    assert_eq!(
        log.iter().filter(|c| c.contains("Stale")).count(),
        1,
        "exactly one Stale — the episode opens once and stays open: {log:?}"
    );
    assert!(
        !log.iter().any(|c| c.contains("Live")),
        "a stale-STAMPED update received while stale must NOT falsely recover (no Live): {log:?}"
    );
    assert_eq!(
        log.iter().filter(|c| c.starts_with("status:")).count(),
        1,
        "only the single Stale is disclosed — the following tick emits nothing new: {log:?}"
    );
}

// ---- net-hardening: the timed book re-seed (checksum-less venues) ----------------------------
//
// Same scripted-clock seam as the freshness tests: transport ALIVE (`stall` ≪ idle threshold) and
// the injected `now_ms` reads a clock the stream ages a fixed amount per read, so elapsed session
// time is driven purely by consuming the script — zero real sleeps.

/// A HEALTHY, in-sync stream (updates flowing, no gap, transport alive) whose injected `now_ms`
/// advances past `reseed_interval` must END the session with [`SessionOutcome::Reseed`] — so
/// [`run_depth_feed`] reconnects + re-snapshots — even though NOTHING is observably wrong (no
/// `BookOp::Gap`, no idle, no `Stale`). This is the whole point: it converts a book that silently
/// corrupts while its seq chain stays intact from "wrong forever" into "wrong ≤ reseed_interval".
/// And because the reseed is a PLANNED refresh, the session discloses NO stream-health status.
#[test]
fn a_periodic_reseed_fires_on_a_healthy_in_sync_stream_past_the_interval() {
    let (clock, now) = clock_and_now();
    // Healthy updates (each "snap" → BookOp::Updated, receive-time fresh); clock ages 20s/read.
    let mut stream = ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 20_000);
    for _ in 0..6 {
        stream.push_text("snap"); // an in-sync update — no gap ever
    }
    let stop = AtomicBool::new(false);
    let (log, res) = drive_reseed(
        &mut stream,
        Duration::from_secs(300), // idle threshold — transport stays alive throughout
        Duration::from_secs(600), // freshness threshold — never trips before the reseed
        Some(Duration::from_secs(60)), // reseed after ~60s of injected clock (trips on read #4)
        &now,
        &stop,
    );
    assert_eq!(
        res,
        Ok(SessionOutcome::Reseed),
        "a healthy in-sync stream past reseed_interval ends the session with Reseed: {log:?}"
    );
    // It really was HEALTHY: books were published and NOTHING unhealthy was disclosed — a planned
    // reseed must not flap Gap/Stale/Live (the suppression `run_depth_feed` relies on).
    assert!(
        log.iter().any(|c| c.starts_with("book:")),
        "the healthy stream published books before the reseed: {log:?}"
    );
    assert!(
        log.iter().all(|c| !c.starts_with("status:")),
        "a healthy periodic reseed discloses no gap/stale/live status: {log:?}"
    );
}

/// The timed re-seed is OFF by default: with `reseed_interval = None`, NO amount of elapsed
/// `now_ms` ends the session with [`SessionOutcome::Reseed`]. Drive the SAME healthy stream, its
/// clock racing far past any interval a caller might pick, and let the script exhaust — the session
/// ends by the stream closing (`Err`), never by a reseed. This is the zero-behavior-change guarantee.
#[test]
fn reseed_interval_none_never_triggers_a_reseed() {
    let (clock, now) = clock_and_now();
    // Clock ages 60s/read → races to 360s across the script, dwarfing any plausible interval.
    let mut stream = ScriptedDepthStream::stalled_clocked(Duration::from_secs(5), clock, 60_000);
    for _ in 0..6 {
        stream.push_text("snap"); // healthy updates the whole way
    }
    let stop = AtomicBool::new(false);
    let (log, res) = drive_reseed(
        &mut stream,
        Duration::from_secs(3_600), // idle threshold — never trips
        Duration::from_secs(7_200), // freshness threshold — never trips
        None,                       // reseed OFF (the default)
        &now,
        &stop,
    );
    assert!(
        !matches!(res, Ok(SessionOutcome::Reseed)),
        "reseed_interval=None must NEVER trigger a forced re-seed: {res:?} / {log:?}"
    );
    assert!(
        res.is_err(),
        "with reseed off, a healthy stream runs until the socket closes (script exhaustion): {log:?}"
    );
}

/// `parse_levels` is the string-priced book-side decode bybit, okx and the binance family share: a
/// `[px, qty]` pair of decimal STRINGS becomes a level, and anything else — a JSON number, a short
/// pair, a non-array row, an absent or non-array side — is skipped rather than guessed at.
#[test]
fn parse_levels_reads_string_pairs_and_skips_anything_else() {
    let side =
        serde_json::json!([["100.5", "2"], ["99", "0"], ["bad", "1"], [101.0, "1"], ["102"], "x"]);
    assert_eq!(
        parse_levels(Some(&side)),
        vec![BookLevel { price: 100.5, qty: 2.0 }, BookLevel { price: 99.0, qty: 0.0 }]
    );
    assert!(parse_levels(None).is_empty(), "an absent side is an empty side");
    assert!(parse_levels(Some(&serde_json::json!({ "a": 1 }))).is_empty(), "so is a non-array");
}
