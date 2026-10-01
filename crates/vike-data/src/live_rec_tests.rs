use super::*;
use crate::live::StreamStatus;
use crate::{DataFusionHist, HistStore, TsRange};
use vike_model::{BookUpdate, BookUpdateKind, EquitySample, L2Book, QuoteTick, TradeTick};

const DAY: i64 = 86_400_000;

fn quote(ts: i64, bid: f64) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: 0,
        bid,
        ask: bid + 0.5,
        bid_size: 1.0,
        ask_size: 2.0,
        symbol: String::new(),
    }
}

fn trade(ts: i64, price: f64) -> TradeTick {
    TradeTick { ts, local_ts: 0, price, size: 1.0, is_buyer_maker: false, symbol: String::new() }
}

fn sample(ts: i64, equity: f64, missing_prices: u32) -> EquitySample {
    EquitySample {
        ts,
        venue: String::new(),
        equity,
        realized: equity - 1.0,
        unrealized: 1.0,
        missing_prices,
    }
}

fn open_store() -> (tempfile::TempDir, Arc<DataFusionHist>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DataFusionHist::open(dir.path()).unwrap());
    (dir, store)
}

fn assert_quotes_bit_eq(want: &[QuoteTick], got: &[QuoteTick]) {
    assert_eq!(want.len(), got.len(), "quote count");
    for (i, (x, y)) in want.iter().zip(got).enumerate() {
        assert_eq!(x.ts, y.ts, "ts[{i}]");
        assert_eq!(x.bid.to_bits(), y.bid.to_bits(), "bid[{i}]");
        assert_eq!(x.ask.to_bits(), y.ask.to_bits(), "ask[{i}]");
        assert_eq!(x.bid_size.to_bits(), y.bid_size.to_bits(), "bid_size[{i}]");
        assert_eq!(x.ask_size.to_bits(), y.ask_size.to_bits(), "ask_size[{i}]");
    }
}

fn assert_trades_bit_eq(want: &[TradeTick], got: &[TradeTick]) {
    assert_eq!(want.len(), got.len(), "trade count");
    for (i, (x, y)) in want.iter().zip(got).enumerate() {
        assert_eq!(x.ts, y.ts, "ts[{i}]");
        assert_eq!(x.price.to_bits(), y.price.to_bits(), "price[{i}]");
        assert_eq!(x.size.to_bits(), y.size.to_bits(), "size[{i}]");
        assert_eq!(x.is_buyer_maker, y.is_buyer_maker, "is_buyer_maker[{i}]");
    }
}

/// Count `.parquet` files under a dir (recurses `date=` subdirs) — same helper shape as
/// `tests/hist_datafusion.rs`'s `count_parquets`, used to observe the rollover producing two
/// separate sealed parts.
fn count_parquets(dir: &std::path::Path) -> usize {
    let mut n = 0;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                n += count_parquets(&p);
            } else if p.extension().is_some_and(|x| x == "parquet") {
                n += 1;
            }
        }
    }
    n
}

fn quote_series_dir(root: &std::path::Path, venue: &str, symbol: &str) -> std::path::PathBuf {
    root.join("kind=quote").join(format!("venue={venue}")).join(format!("symbol={symbol}"))
}

#[test]
fn flush_on_max_rows() {
    let (_dir, store) = open_store();
    let cfg = RecorderConfig { max_rows: 3, ..RecorderConfig::default() };
    let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();

    let sent = vec![quote(1_000, 100.0), quote(2_000, 100.5), quote(3_000, 101.0)];
    for q in &sent {
        sink.quote("polymarket", "TOK", q.clone());
    }

    // Bounded poll BEFORE any shutdown: this isolates the max-rows trigger itself. Polling
    // strictly before shutdown() is what proves the flush happened because the buffer hit
    // max_rows — shutdown's own flush-all would otherwise mask a broken max_rows path.
    let start = Instant::now();
    let got = loop {
        let got = store.scan_quotes("polymarket", "TOK", TsRange::all()).unwrap();
        if got.len() >= sent.len() {
            break got;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "max-rows flush did not fire in time");
        thread::sleep(Duration::from_millis(50));
    };
    assert_quotes_bit_eq(&sent, &got);

    handle.shutdown();
}

#[test]
fn flush_on_age() {
    let (_dir, store) = open_store();
    let cfg = RecorderConfig { max_age: Duration::from_millis(200), ..RecorderConfig::default() };
    let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();

    let sent = quote(5_000, 42.0);
    sink.quote("polymarket", "TOK", sent.clone());

    // Bounded poll: the writer's recv_timeout wakes at min(max_age, 1s) = 200ms, so the row
    // should be visible well within a couple of seconds without an explicit shutdown.
    let start = Instant::now();
    let got = loop {
        let got = store.scan_quotes("polymarket", "TOK", TsRange::all()).unwrap();
        if !got.is_empty() {
            break got;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "age-based flush did not fire in time");
        thread::sleep(Duration::from_millis(20));
    };
    assert_quotes_bit_eq(&[sent], &got);

    handle.shutdown();
}

/// Final-review fix: `recv_timeout` returns `Ok` immediately whenever ANY series has a message
/// queued, so with N series multiplexed on one channel a steadily-active series must not starve
/// the age-flush pass for a quiet one. A busy series is flooded continuously (keeping `recv`
/// returning `Ok`, never `Err(Timeout)`) while a quiet series sits on a single old row with a
/// short `max_age`; the quiet row must still surface via `scan_quotes` within a bounded poll,
/// WHILE the busy series is still running (no shutdown involved) — this is what proves the age
/// scan runs on a wall-clock cadence, not only in the `Err(Timeout)` arm. Against the old
/// Timeout-only code the quiet row NEVER surfaces (recv essentially never times out here), so
/// this is a condition wait: the loop exits on the row being there, never on the clock, and the
/// deadline can only ever turn a hang into a named failure.
///
/// ⚠ **The busy series floods a different KIND on purpose, and it is what took the flake out.**
/// It used to flood quotes, which put TWO incidental store commits between the age scan and the
/// assertion: at the same scan the busy buffer is also `>= max_age` old, so `flush_aged` flushed
/// it too — and `flush_aged` walks a `HashMap`, whose iteration order is `RandomState`-seeded
/// per PROCESS, so on a coin flip the busy series' whole commit ran BEFORE the quiet one's even
/// began. Instrumented on the CI box (1,032 runs of the CI shape under load), the slowest run
/// decomposed as: age scan on time at +200 ms, then `BUSY rows=2040 append_ms=983`, then
/// `QUIET rows=1 append_ms=2008` — a single ONE-ROW commit costing 2 s because
/// `DataFusionHist::commit_rows` fsyncs five times (WAL, parquet part, `date=` dir, manifest
/// tmp, series dir) and an fsync on that box costs 7 ms idle. None of that is the property under
/// test. Flooding trades instead pins the order: `writer_loop`'s age pass runs `flush_aged` over
/// quotes BEFORE trades, so the quiet quote is always committed first, while the starvation
/// condition is untouched — it is about the shared CHANNEL never going quiet, which is
/// kind-independent.
#[test]
fn age_flush_fires_under_busy_other_series() {
    let (_dir, store) = open_store();
    let cfg = RecorderConfig { max_age: Duration::from_millis(200), ..RecorderConfig::default() };
    let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();

    // Quiet series: one row, sent once, then never touched again.
    let quiet = quote(9_000, 7.0);
    sink.quote("polymarket", "QUIET", quiet.clone());

    // Busy series: flood a steady stream (faster than `wait`) so the writer's `recv_timeout`
    // keeps returning `Ok` — this is exactly the condition that starved the age flush before
    // the fix. A different kind (see the doc above) keeps its commit off the quiet series'
    // critical path without weakening that condition.
    let busy_sink = sink.clone();
    let busy_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let busy_stop_thread = busy_stop.clone();
    let busy = thread::spawn(move || {
        let mut i = 0i64;
        while !busy_stop_thread.load(Ordering::Relaxed) {
            busy_sink.trade("polymarket", "BUSY", trade(i, i as f64));
            i += 1;
            thread::sleep(Duration::from_millis(1));
        }
    });

    // Condition poll for the quiet series' row — must become visible while the busy stream is
    // STILL running (checked below), never via a shutdown-triggered flush-all.
    //
    // The deadline is [`REPORT_DEADLINE`], and it is doing the same job it does there: bounding
    // how long an ACTUALLY BROKEN writer takes to fail, not stating how long this should take.
    // It cannot manufacture a pass — the loop breaks only on the row being present — so the
    // failure it guards is "never", and against the Timeout-only writer no deadline is reached
    // by anything but the assert. The observation channel is a full DataFusion read (fresh
    // `SessionContext` + manifest re-read + parquet scan, measured at up to 1.2 s for ONE poll
    // under contention), so the poll interval is deliberately coarse: the test's own polling
    // competes with the writer thread it is waiting for.
    let start = Instant::now();
    let got = loop {
        let got = store.scan_quotes("polymarket", "QUIET", TsRange::all()).unwrap();
        if !got.is_empty() {
            break got;
        }
        assert!(
            start.elapsed() < REPORT_DEADLINE,
            "age flush starved by a busy other series (quiet series' row never surfaced \
                 after {:?})",
            start.elapsed()
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert_quotes_bit_eq(&[quiet], &got);

    busy_stop.store(true, Ordering::Relaxed);
    busy.join().unwrap();
    handle.shutdown();
}

/// The age-scan cadence rule, gated by arithmetic — no store, no writer thread, no sleeping.
///
/// [`age_flush_fires_under_busy_other_series`] above proves the rule end to end, but it can only
/// do so through a wall clock and five fsyncs. The rule itself is decidable without either, and
/// this is where it is decided.
mod age_scan_rule_tests {
    use super::*;

    /// The historical BROKEN cadence: the age scan ran ONLY in `recv_timeout`'s `Err(Timeout)`
    /// arm. Defined here, in the test, because it is the thing the real rule must disagree with.
    fn timeout_only_age_scan_due(
        since_last_scan: Duration,
        wait: Duration,
        recv_timed_out: bool,
    ) -> bool {
        recv_timed_out && since_last_scan >= wait
    }

    /// Walk one writer-loop's worth of iterations under a channel that NEVER goes quiet (every
    /// `recv` returns `Ok`, as it does whenever any of N multiplexed series is steadily active)
    /// and count how many age scans each rule would run. `rule` takes the same three facts, so
    /// both are driven by one checker over one scripted sequence.
    fn scans_over_a_busy_channel(rule: impl Fn(Duration, Duration, bool) -> bool) -> usize {
        let wait = Duration::from_millis(200);
        let mut since = Duration::ZERO;
        let mut scans = 0;
        // 1000 iterations at 10ms of simulated progress each = 10s of a channel that always has
        // a message ready — 50 `wait` periods' worth.
        for _ in 0..1000 {
            since += Duration::from_millis(10);
            if rule(since, wait, /* recv_timed_out */ false) {
                scans += 1;
                since = Duration::ZERO;
            }
        }
        scans
    }

    /// The negative control: one checker, pointed at the real rule and at the historical broken
    /// one, required to reach OPPOSITE verdicts. Without this, a rule that silently regained a
    /// `recv`-outcome dependency would still pass every timing-based test on a fast box.
    #[test]
    fn the_age_scan_rule_rejects_a_timeout_only_cadence() {
        let real = scans_over_a_busy_channel(|since, wait, _| age_scan_due(since, wait));
        let broken = scans_over_a_busy_channel(timeout_only_age_scan_due);

        assert_eq!(
            broken, 0,
            "the Timeout-only cadence must run ZERO age scans while the channel never goes \
                 quiet — that is the bug this rule replaced"
        );
        assert_eq!(
            real, 50,
            "the real rule must run one age scan per `wait` regardless of `recv`'s outcome \
                 (10s of simulated progress / 200ms wait)"
        );
    }

    /// The cadence is bounded ABOVE by `wait` too: a scan is due the moment `wait` has elapsed,
    /// never later, so a quiet series' exposure is `max_age + wait` and not a function of how
    /// much traffic the busy siblings pushed through in between.
    #[test]
    fn a_scan_is_due_exactly_once_wait_has_elapsed() {
        let wait = Duration::from_millis(200);
        assert!(!age_scan_due(Duration::from_millis(199), wait));
        assert!(age_scan_due(Duration::from_millis(200), wait));
        assert!(age_scan_due(Duration::from_secs(30), wait));
    }
}

#[test]
fn utc_rollover_splits_flushes() {
    let (dir, store) = open_store();
    // max_rows big enough that only the rollover (not row-count) drives the first flush.
    let cfg = RecorderConfig { max_rows: 1_000, ..RecorderConfig::default() };
    let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();

    let day0 = quote(1_000, 10.0); // 1970-01-01
    let day1 = quote(DAY + 1_000, 20.0); // 1970-01-02 — different UTC date -> rollover flush
    sink.quote("polymarket", "TOK", day0.clone());
    sink.quote("polymarket", "TOK", day1.clone());
    // Shutdown flushes the still-open day1 buffer; by the time it joins, the rollover flush of
    // day0 (triggered by day1 arriving) already ran — same ordered channel.
    handle.shutdown();

    let series = quote_series_dir(dir.path(), "polymarket", "TOK");
    assert_eq!(count_parquets(&series), 2, "one sealed part per UTC day (two commit keys)");
    assert!(series.join("date=1970-01-01").exists());
    assert!(series.join("date=1970-01-02").exists());

    let got = store.scan_quotes("polymarket", "TOK", TsRange::all()).unwrap();
    assert_quotes_bit_eq(&[day0, day1], &got);
}

#[test]
fn shutdown_flushes_remainder() {
    let (_dir, store) = open_store();
    let cfg = RecorderConfig { max_rows: 1_000, ..RecorderConfig::default() };
    let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();

    let sent = vec![trade(1_000, 5.0), trade(1_500, 5.1)];
    for t in &sent {
        sink.trade("polymarket", "TOK", t.clone());
    }
    // Neither max_rows nor max_age has fired yet — only shutdown's flush-all makes this visible.
    handle.shutdown();

    let got = store.scan_trades("polymarket", "TOK", TsRange::all()).unwrap();
    assert_trades_bit_eq(&sent, &got);
}

/// ⚠ **One producer, one series — so this test cannot see an attribution bug at all**, and for
/// a long time nothing else could either: it asserts `dropped() > 0`, which a counter covering
/// every series satisfies just as well as a keyed one. The two-producer twin
/// (`two_producers_overflow_and_the_quiet_series_is_named`) is where the key is gated; this one
/// stays as the "never blocks, never panics, still joins" case it was written for.
#[test]
fn overflow_drops_and_counts() {
    let (_dir, store) = open_store();
    let cfg = RecorderConfig { channel_cap: 1, ..RecorderConfig::default() };
    let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();

    // Flood far faster than the channel (capacity 1) can drain — some try_sends must fail.
    for i in 0..50_000i64 {
        sink.quote("polymarket", "TOK", quote(i, i as f64));
    }

    assert!(handle.dropped() > 0, "flooding a capacity-1 channel must drop rows, not block/panic");
    handle.shutdown(); // must still join cleanly — no deadlock from the overflow path
}

#[test]
fn book_is_noop() {
    let (dir, store) = open_store();
    let cfg = RecorderConfig::default();
    let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();

    let mut book = L2Book::new(0.01);
    book.apply_snapshot(1, &[BookLevel::new(10.0, 1.0)], &[BookLevel::new(10.5, 1.0)]);
    sink.book("polymarket", "TOK", Arc::new(book));
    handle.shutdown();

    // No book schema exists in the store at all — nothing was ever written for this series.
    assert!(!dir.path().join("kind=book").exists());
    let series = store.list_series().unwrap();
    assert!(series.is_empty(), "book() must not create any series");
}

fn book_upd(ts: i64, seq: u64, kind: BookUpdateKind) -> BookUpdate {
    BookUpdate {
        ts,
        local_ts: ts + 1,
        seq,
        kind,
        tick_size: 0.01,
        bids: vec![BookLevel::new(0.45, 10.0)],
        asks: vec![BookLevel::new(0.46, 5.0)],
        symbol: String::new(),
    }
}

#[test]
fn book_updates_flush_and_roundtrip() {
    let (_dir, store) = open_store();
    let cfg = RecorderConfig { max_rows: 2, ..RecorderConfig::default() };
    let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();
    sink.book_update("polymarket", "TOK", book_upd(1_000, 1, BookUpdateKind::Snapshot));
    sink.book_update("polymarket", "TOK", book_upd(1_005, 2, BookUpdateKind::Delta));
    handle.shutdown();
    let got = store.scan_book_updates("polymarket", "TOK", TsRange::all()).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].kind, BookUpdateKind::Snapshot);
    assert_eq!(got[1].seq, 2);
    assert_eq!(got[0].bids[0].price.to_bits(), 0.45f64.to_bits());
}

#[test]
fn book_stream_status_recorded_as_markers() {
    let (_dir, store) = open_store();
    let (sink, handle) = RecorderSink::spawn(store.clone(), RecorderConfig::default()).unwrap();
    sink.book_update("polymarket", "TOK", book_upd(1_000, 1, BookUpdateKind::Snapshot));
    sink.stream_status("polymarket", "TOK", "book", StreamStatus::GapStart { at_ts_ms: 2_000 });
    sink.stream_status(
        "polymarket",
        "TOK",
        "book",
        StreamStatus::Live { gap_started_ts_ms: Some(2_000) },
    );
    // A non-L2 lane still records nothing here (quotes/trades gap provenance = follow-up), and
    // it must not leak into the BOOK series either:
    sink.stream_status("polymarket", "TOK", "quotes", StreamStatus::GapStart { at_ts_ms: 9_000 });
    handle.shutdown();
    let got = store.scan_book_updates("polymarket", "TOK", TsRange::all()).unwrap();
    assert_eq!(got.len(), 3);
    assert_eq!(got[1].kind, BookUpdateKind::GapStart);
    assert_eq!(got[1].ts, 2_000);
    assert!(got[1].bids.is_empty() && got[1].asks.is_empty());
    assert_eq!(got[2].kind, BookUpdateKind::LiveResume);
}

/// **The DEPTH lane's markers land in the DEPTH series** — the disclosure half of the
/// forty-day binance perp reconnect loop.
///
/// `stream_status` early-returned for every stream but `"book"`, so `depth_main`'s `GapStart`
/// (one per reconnect cycle, ~20,000 a day on the broken lane) was written into no series at
/// all: a backtest reading `kind=depth` saw a book teleporting every 4.3 s with nothing to say
/// the live feed had been down between the rows.
///
/// Three assertions, each failing on a different way of reopening it: the markers are RECORDED,
/// they are recorded under `kind=depth` and NOT under `kind=book` (a marker in the wrong series
/// would corrupt the book lane's own chain and is the tempting one-line "fix"), and a non-L2
/// stream label still records nothing.
///
/// MUTATION PROOF: restore `if stream != "book" { return; }` and the depth scan comes back
/// empty on the two markers; route the `"depth"` arm to `Msg::Book` and the `kind=book`
/// assertion goes red.
#[test]
fn depth_stream_status_is_recorded_into_the_depth_series() {
    let (_dir, store) = open_store();
    let (sink, handle) = RecorderSink::spawn(store.clone(), RecorderConfig::default()).unwrap();

    sink.l2_snapshot(
        "binance",
        "BTCUSDT.P",
        0.1,
        vec![BookLevel::new(60000.0, 1.0)],
        vec![BookLevel::new(60001.0, 1.0)],
        1_000,
    );
    sink.stream_status("binance", "BTCUSDT.P", "depth", StreamStatus::GapStart { at_ts_ms: 2_000 });
    sink.stream_status(
        "binance",
        "BTCUSDT.P",
        "depth",
        StreamStatus::Live { gap_started_ts_ms: Some(2_000) },
    );
    // …and a lane with no BookUpdate series of its own is still dropped.
    sink.stream_status(
        "binance",
        "BTCUSDT.P",
        "trades",
        StreamStatus::GapStart { at_ts_ms: 9_000 },
    );
    handle.shutdown();

    let got = store.scan_depth("binance", "BTCUSDT.P", TsRange::all()).unwrap();
    assert_eq!(got.len(), 3, "the snapshot plus BOTH depth markers: {got:?}");
    assert_eq!(got[1].kind, BookUpdateKind::GapStart);
    assert_eq!(got[1].ts, 2_000);
    assert!(got[1].bids.is_empty() && got[1].asks.is_empty(), "a marker carries no levels");
    assert_eq!(got[2].kind, BookUpdateKind::LiveResume);

    // No BOOK series exists at all — a depth marker written there would inject a `seq: 0` row
    // into a lane whose whole promise is a contiguous chain.
    let kinds: Vec<String> = store.list_series().unwrap().into_iter().map(|s| s.kind).collect();
    assert_eq!(kinds, vec!["depth".to_string()], "depth markers must not create a book series");
}

/// End-to-end producer -> writer-thread actor -> shutdown-flush -> store round-trip for the
/// equity lane (portfolio-observer PR-3, Task 3) — the same shape as the quote/trade lane
/// tests above, proving `record_equity` is wired through `Msg::Equity` / `ingest` (keyed by the
/// `EQUITY` [`RecKind`]) into `HistStore::append_equity`, keyed by `(venue, symbol)` exactly
/// like `HistStore::scan_equity` expects (`venue` = the fixed `"portfolio"` namespace, `symbol`
/// = the per-exchange venue name or the cross-venue `"TOTAL"` rollup).
#[test]
fn recorder_persists_equity_samples() {
    let (_dir, store) = open_store();
    let (sink, handle) = RecorderSink::spawn(store.clone(), RecorderConfig::default()).unwrap();

    let venue_sample = sample(1, 100.0, 0);
    let total_sample = sample(1, 250.0, 2);
    sink.record_equity("portfolio", "binance", venue_sample.clone());
    sink.record_equity("portfolio", "TOTAL", total_sample.clone());
    handle.shutdown(); // flushes + joins

    let got_venue = store.scan_equity("portfolio", "binance", TsRange::all()).unwrap();
    let got_total = store.scan_equity("portfolio", "TOTAL", TsRange::all()).unwrap();
    assert_eq!(got_venue.len(), 1);
    assert_eq!(got_total.len(), 1);

    // `venue` on the scanned row is re-injected from the `symbol` argument (see
    // `datafusion_hist/codec.rs`'s `batch_to_equities` — same shape as `kind=properties`), not
    // carried through from the row's own `.venue` field at append time.
    assert_eq!(got_venue[0].venue, "binance");
    assert_eq!(got_total[0].venue, "TOTAL");

    assert_eq!(got_venue[0].ts, venue_sample.ts);
    assert_eq!(got_venue[0].equity.to_bits(), venue_sample.equity.to_bits());
    assert_eq!(got_venue[0].realized.to_bits(), venue_sample.realized.to_bits());
    assert_eq!(got_venue[0].unrealized.to_bits(), venue_sample.unrealized.to_bits());
    assert_eq!(got_venue[0].missing_prices, venue_sample.missing_prices);

    assert_eq!(got_total[0].equity.to_bits(), total_sample.equity.to_bits());
    assert_eq!(got_total[0].missing_prices, total_sample.missing_prices);
}

#[test]
fn equity_shutdown_flushes_remainder() {
    // Mirrors `shutdown_flushes_remainder` for quotes/trades: neither max_rows nor max_age
    // has fired — only shutdown's flush-all makes the buffered equity rows visible.
    let (_dir, store) = open_store();
    let cfg = RecorderConfig { max_rows: 1_000, ..RecorderConfig::default() };
    let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();

    let sent = vec![sample(1_000, 10.0, 0), sample(1_500, 11.0, 0)];
    for s in &sent {
        sink.record_equity("portfolio", "binance", s.clone());
    }
    handle.shutdown();

    let got = store.scan_equity("portfolio", "binance", TsRange::all()).unwrap();
    assert_eq!(got.len(), sent.len());
    for (want, got) in sent.iter().zip(&got) {
        assert_eq!(want.ts, got.ts);
        assert_eq!(want.equity.to_bits(), got.equity.to_bits());
    }
}

#[test]
fn equity_overflow_drops_and_counts() {
    // Mirrors `overflow_drops_and_counts`: flooding a capacity-1 channel must drop rows (via
    // the shared `send` helper's dropped-counter path), never block or panic the caller.
    let (_dir, store) = open_store();
    let cfg = RecorderConfig { channel_cap: 1, ..RecorderConfig::default() };
    let (sink, handle) = RecorderSink::spawn(store.clone(), cfg).unwrap();

    for i in 0..50_000i64 {
        sink.record_equity("portfolio", "binance", sample(i, i as f64, 0));
    }

    assert!(
        handle.dropped() > 0,
        "flooding a capacity-1 channel must drop equity rows, not block/panic"
    );
    handle.shutdown(); // must still join cleanly — no deadlock from the overflow path
}

// ---- loss reporting: `dropped`/`discarded` are ANNOUNCED, not merely counted ---------------
//
// The bug these cover: `RecorderHandle::dropped()` had exactly two call sites in the whole
// workspace, both `#[cfg(test)]` in this file. Nothing in `vike-app`/`vike-tradehub` read it, so
// a live recorder could gap its tape forever in silence — the failure mode `vike-alerting` was
// split out for, with its own metric unread.

/// Captures every [`DropReport`] the writer thread emits, so a test can assert on what an
/// operator (or a pager) would actually have seen.
#[derive(Clone, Default)]
struct ReportLog(Arc<std::sync::Mutex<Vec<DropReport>>>);

impl ReportLog {
    fn observer(&self) -> DropObserver {
        let inner = self.0.clone();
        // `clone`, not `*`: a report now carries its per-series breakdown, so it is not `Copy`.
        Arc::new(move |r: &DropReport| inner.lock().unwrap().push(r.clone()))
    }
    fn reports(&self) -> Vec<DropReport> {
        self.0.lock().unwrap().clone()
    }
    /// Named `count` rather than `len` so no reader mistakes this for a collection.
    fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

/// A short-cadence config: the writer wakes every 20ms (`max_age`) and considers reporting every
/// 50ms, so a test observes many report windows in well under a second.
fn chatty(channel_cap: usize) -> RecorderConfig {
    RecorderConfig {
        channel_cap,
        max_age: Duration::from_millis(20),
        drop_report_every: Duration::from_millis(50),
        ..RecorderConfig::default()
    }
}

/// How long a test may wait for a [`DropReport`] to reach the observer before calling it
/// broken. Twin of the bound `a_permanently_failed_flush_discards_and_counts_the_rows` already
/// used, hoisted so the two cannot drift apart.
///
/// It bounds only how long an ACTUALLY BROKEN test takes to fail — the waits below poll every
/// 2ms and break the instant the condition holds — so it never slows the happy path.
///
/// **10s is ~8x the worst latency ever measured, and it is not papering over a lost edge.**
/// A report's latency is bounded by the writer thread's store commits, not by
/// `drop_report_every` (see [`RecorderConfig::drop_report_every`]): MEASURED on the the CI box CI
/// box, the first report of a ~19,000-row burst lands ~80ms after the burst when idle, p90
/// 176ms / max 350ms under a 4-way parallel test load, and max 1,305ms under an 8-way one.
/// Across 260 instrumented runs the report was late 6 times and **lost zero times** — which is
/// the discriminator that says a deadline is the right tool here at all. Where a report CAN be
/// permanently lost, no deadline is big enough and widening one is the bug (see
/// `crates/vike-bridge-core/src/user_data.rs`'s `run_resync_supervisor`, whose 3s -> 30s
/// widening then failed at 30.083s because the edge was gone, not slow).
const REPORT_DEADLINE: Duration = Duration::from_secs(10);

/// The series key a `sink.quote(venue, symbol, _)` row is charged to when it is dropped — the
/// one constructor, so no test can pin a `kind` spelling the sink does not produce.
fn quote_key(venue: &str, symbol: &str) -> SeriesId {
    SeriesId::per_symbol("quote", venue, symbol, None)
}

/// Whether a report NAMES this series among the rows it says were dropped.
fn names_series(report: &DropReport, series: &SeriesId) -> bool {
    report.dropped_series.iter().any(|s| &s.series == series)
}

/// The single series every [`drive_meter`] burst is charged to — the arithmetic tests care about
/// the counting, not about who lost the rows, but the counting is now keyed so SOME key must be
/// named.
fn drive_series() -> SeriesId {
    quote_key("polymarket", "TOK")
}

/// A [`DropTally`] with `n` rows already lost on [`drive_series`] — the keyed replacement for
/// what used to be `Arc::new(AtomicU64::new(n))`.
fn tally_of(n: u64) -> Arc<DropTally> {
    let tally = Arc::new(DropTally::new());
    for _ in 0..n {
        tally.record(Some(drive_series()));
    }
    tally
}

/// Drive a real [`LossMeter`] with `every = ZERO` (so every `report_if_due` is due) through
/// `bursts`, calling `report_if_due` `silent_windows` extra times after each burst — the
/// windows in which nothing new was lost but the CUMULATIVE total is still non-zero. Ends with
/// the closing `report_final`. No store, no writer thread, no wall clock: the whole delta
/// discipline is decided by arithmetic here, which is why this is where it is gated.
fn drive_meter(bursts: &[u64], silent_windows: usize) -> Vec<DropReport> {
    let dropped = Arc::new(DropTally::new());
    let discarded = Arc::new(AtomicU64::new(0));
    let log = ReportLog::default();
    let mut meter = LossMeter::new(
        dropped.clone(),
        discarded,
        Some(log.observer()),
        Duration::ZERO, // every check is due — the cadence is not what is under test
    );
    for burst in bursts {
        // Row by row through the real `record`, on ONE series: the delta discipline is the
        // subject, and driving the real ledger is what puts the per-series half under the same
        // checker as the scalar half.
        for _ in 0..*burst {
            dropped.record(Some(drive_series()));
        }
        meter.report_if_due();
        for _ in 0..silent_windows {
            meter.report_if_due();
        }
    }
    meter.report_final();
    log.reports()
}

/// What a TOTAL-based meter would have handed the observer for the same input — the
/// obvious-but-wrong implementation this whole mechanism exists to not be. It re-states the
/// running total every window it is asked, so it never falls silent once anything is lost.
///
/// This is the NEGATIVE CONTROL's input, not production code: the checker below must reject it.
fn total_based_reports(bursts: &[u64], silent_windows: usize) -> Vec<DropReport> {
    let mut out = Vec::new();
    let mut total = 0u64;
    let mut push = |total: u64| {
        out.push(DropReport {
            dropped_delta: total, // ← THE mutation: the total, restated as if it were new
            dropped_total: total,
            // ...and the second half of the same mutation: a scalar report names nobody, which
            // is the state this whole change exists to leave behind.
            dropped_series: Vec::new(),
            discarded_delta: 0,
            discarded_total: 0,
            window: Duration::ZERO,
            final_report: false,
        });
    };
    for burst in bursts {
        total += burst;
        push(total);
        for _ in 0..silent_windows {
            push(total); // still non-zero ⇒ still "reportable" ⇒ one line per window, forever
        }
    }
    push(total); // the closing report re-states it one last time
    out
}

/// The delta discipline, as a CHECKER rather than a pile of inline asserts, so the exact same
/// judgement can be pointed at the real meter and at the broken one and be seen to disagree.
///
/// `Err` describes the violation; `Ok` means: one report per burst, in order, each carrying
/// that burst's own delta and the running total at the time — and nothing else at all.
fn check_delta_discipline(reports: &[DropReport], bursts: &[u64]) -> Result<(), String> {
    if let Some(empty) = reports.iter().find(|r| r.lost_delta() == 0) {
        return Err(format!("an empty report was emitted: {empty:?}"));
    }
    if reports.len() != bursts.len() {
        return Err(format!(
            "expected exactly one report per burst ({}), got {} — a report was re-emitted for \
                 a window in which nothing new was lost",
            bursts.len(),
            reports.len()
        ));
    }
    let mut running = 0u64;
    for (i, (report, burst)) in reports.iter().zip(bursts).enumerate() {
        running += burst;
        if report.dropped_delta != *burst {
            return Err(format!(
                "report {i}: dropped_delta {} is not the {burst} rows lost in THAT window \
                     (a total-based report would say {running})",
                report.dropped_delta
            ));
        }
        if report.dropped_total != running {
            return Err(format!(
                "report {i}: dropped_total {} is not the running total {running}",
                report.dropped_total
            ));
        }
        // The named series must ACCOUNT for the same delta. `drive_meter` charges every burst
        // to one series, so this is exact — and it is what fails on a report that counts rows
        // it cannot attribute (the bare-scalar shape), not merely on a wrong number.
        let named: u64 = report.dropped_series.iter().map(|s| s.delta).sum();
        if named != *burst {
            return Err(format!(
                "report {i}: the named series account for {named} of the {burst} rows lost in \
                     that window — a drop nothing can name is a gap nobody can find: {:?}",
                report.dropped_series
            ));
        }
    }
    Ok(())
}

/// THE property, gated where it actually lives: [`LossMeter`] reports each non-zero DELTA
/// exactly once and then stays silent, however many windows pass, however non-zero the
/// CUMULATIVE total remains.
///
/// Deterministic by construction — no store, no writer thread, no sleeping — because none of
/// those participate in the decision. The end-to-end test below proves the WIRING (a real burst
/// through a real writer thread reaches a real observer); this proves the RULE.
#[test]
fn the_meter_reports_each_delta_once_and_then_falls_silent() {
    let bursts = [7_u64, 5, 1];
    // 20 windows of nothing-new between bursts: a total-based meter would emit 20 lines each.
    let reports = drive_meter(&bursts, 20);
    check_delta_discipline(&reports, &bursts).expect("the real meter must satisfy this");

    // The deltas partition the total exactly: nothing double-counted, nothing missed.
    let summed: u64 = reports.iter().map(|r| r.dropped_delta).sum();
    assert_eq!(summed, bursts.iter().sum::<u64>(), "the deltas must sum to the total");
    // And the discriminator, stated outright: after the first burst a delta is NOT the total.
    assert_ne!(
        reports[1].dropped_delta, reports[1].dropped_total,
        "a delta that equals the running total is the total-based bug wearing a delta's name"
    );
    // Nothing was lost to a failed flush here, so that axis stays silent throughout.
    assert!(reports.iter().all(|r| r.discarded_delta == 0 && r.discarded_total == 0));
}

/// NEGATIVE CONTROL for the test above. The checks it uses are only worth running if they can
/// FAIL, so point the very same checker at what a total-based meter would have produced for the
/// very same input and require the opposite verdict.
///
/// Without this, softening `check_delta_discipline` (or the meter) leaves a test that passes
/// whether or not the delta discipline still holds — which is the failure mode of every
/// threshold quietly raised past anything a run can reach.
#[test]
fn the_delta_checks_reject_a_total_based_meter() {
    let bursts = [7_u64, 5, 1];
    let broken = total_based_reports(&bursts, 20);
    let verdict = check_delta_discipline(&broken, &bursts);
    assert!(
        verdict.is_err(),
        "the checker ACCEPTED a total-based meter — it proves nothing: {broken:?}"
    );
    // ...and it accepts the real one, from the identical input. One checker, two verdicts.
    assert!(check_delta_discipline(&drive_meter(&bursts, 20), &bursts).is_ok());
}

/// The closing report is emitted OFF cadence (that is its whole point — a burst in the final
/// window must not be swallowed by the shutdown), but it is still a delta: with everything
/// already reported it says nothing at all.
#[test]
fn the_closing_report_carries_only_what_was_not_yet_reported() {
    // Nothing reported yet ⇒ the closing report carries the whole burst.
    let only_final = {
        let log = ReportLog::default();
        let mut meter = LossMeter::new(
            tally_of(9),
            Arc::new(AtomicU64::new(0)),
            Some(log.observer()),
            Duration::from_secs(3_600), // never due on cadence — only `report_final` fires
        );
        meter.report_if_due();
        meter.report_final();
        log.reports()
    };
    assert_eq!(only_final.len(), 1, "the final window's loss is never swallowed");
    assert!(only_final[0].final_report);
    assert_eq!(only_final[0].dropped_delta, 9);

    // Already reported ⇒ the closing report is silent. `drive_meter` ends with `report_final`,
    // so the exact-count check above already proves this; stated here as its own case because
    // it is the load-immune half of the end-to-end test below.
    let bursts = [4_u64];
    assert_eq!(drive_meter(&bursts, 0).len(), 1, "a closing report with a zero delta is silent");
}

/// THE regression test for the reported shape, end to end: a burst of drops made by a real
/// `RecorderSink` is announced through a real writer thread, and then — while the CUMULATIVE
/// total stays non-zero forever — reporting falls SILENT again.
///
/// ⚠ **This test waits on CONDITIONS, never on a fixed sleep, and that is the fix for a real
/// flake** (it failed on the CI box in a full-roster run, then went 10/10 green in isolation). It
/// used to `thread::sleep(300ms)` and then assert the burst had been reported. But a report's
/// latency is not set by `drop_report_every` (50ms here) — it is set by when the writer thread
/// next gets between store commits, and a commit costs ~30-39ms idle and far more under load.
/// MEASURED on the CI box: the report lands ~80ms after the burst when idle, but p90 176ms / max
/// 350ms under a 4-way parallel test load and max 1,305ms under an 8-way one, so the 300ms
/// budget was ~1.7x the p90 and NEGATIVE at the tail. Under a deliberate 4-way load the old
/// shape failed **12 of 60 runs**, always at "the drop burst was never reported".
///
/// It is a fidelity bug, not a lost report: across 260 instrumented runs the report arrived
/// late 6 times and was lost **zero** times (the delta accumulates and the next report carries
/// it; teardown always emits a closing one). That is what makes a bounded wait the right tool —
/// see [`REPORT_DEADLINE`] for why the opposite finding would forbid one.
///
/// **The silence half is gated causally, not by a clock.** A quiet sleep can only produce a
/// false PASS (a stalled writer emits nothing, which is what the assertion wants to see), so it
/// cannot be the proof. `shutdown()` can: `report_final` emits UNCONDITIONALLY, so a total-based
/// implementation is guaranteed to hand the observer a fresh non-zero `dropped_delta` there,
/// and the sum below catches it with no timing involved. `the_meter_reports_each_delta_once_and_then_falls_silent`
/// gates the periodic windows the same way — by arithmetic.
#[test]
fn a_drop_burst_is_reported_once_per_delta_then_falls_silent() {
    let (_dir, store) = open_store();
    let log = ReportLog::default();
    let (sink, handle) =
        RecorderSink::spawn_with_observer(store.clone(), chatty(1), Some(log.observer())).unwrap();

    // ONE burst: flood a capacity-1 channel so a pile of `try_send`s fail.
    for i in 0..20_000i64 {
        sink.quote("polymarket", "TOK", quote(i, i as f64));
    }
    // Every `try_send` above ran on THIS thread, so the counter is final at this line: no other
    // producer exists and the writer never bumps it. That is what lets the wait below be a
    // CONDITION ("has all of it been reported yet?") rather than a guess about elapsed time.
    let dropped = handle.dropped();
    assert!(dropped > 0, "flooding a capacity-1 channel must have dropped rows");

    // Wait for the burst to be FULLY reported. Bounded, polled — never a fixed sleep: see this
    // test's doc for the measurements that killed the 300ms one.
    //
    // Reaching the `break` IS the "it was reported at all" assertion, so there is deliberately
    // no `reports.len() >= 1` line after it: with `dropped > 0` the sum cannot match on an
    // empty log, and an assertion that cannot fail is exactly what this PR is about deleting.
    let started = Instant::now();
    loop {
        let reports = log.reports();
        let summed: u64 = reports.iter().map(|r| r.dropped_delta).sum();
        if summed == dropped {
            break;
        }
        assert!(
            started.elapsed() < REPORT_DEADLINE,
            "the drop burst was never fully reported to the observer: {summed} of {dropped} \
                 rows across {} report(s) after {:?}",
            reports.len(),
            started.elapsed()
        );
        thread::sleep(Duration::from_millis(2));
    }

    // Every emitted report is a real loss, and this burst's losses are all `dropped` ones.
    let reports = log.reports();
    assert!(reports.iter().all(|r| r.lost_delta() > 0), "an empty report must never be emitted");
    assert!(
        reports.iter().all(|r| r.discarded_delta == 0),
        "channel overflow is a `dropped`, never a `discarded`"
    );
    assert_eq!(
        reports.last().expect("non-empty").dropped_total,
        dropped,
        "each report also carries the running total for context"
    );

    // The burst is over. No new rows are sent, so every subsequent window has a ZERO delta —
    // even though `dropped_total` remains non-zero for the rest of the recorder's life.
    //
    // ⚠ This sleep is CORROBORATION, not proof: a writer stalled through the whole window emits
    // nothing either, so it can only ever produce a false pass. The proof is the shutdown below
    // (causal) and `the_meter_reports_each_delta_once_and_then_falls_silent` (arithmetic).
    thread::sleep(chatty(1).drop_report_every * 4);
    let quiet: u64 = log.reports().iter().map(|r| r.dropped_delta).sum();
    assert_eq!(
        quiet, dropped,
        "a drop must be reported once per DELTA, not re-reported every window while the \
             cumulative total stays non-zero"
    );

    // THE mutation gate, and the one step here with no timing in it at all: `shutdown` joins
    // the writer, and `report_final` emits UNCONDITIONALLY on the way out. A total-based
    // implementation therefore MUST hand the observer another `dropped_delta` of `dropped`
    // right here, doubling this sum; the delta-based one contributes exactly nothing.
    //
    // Summing the deltas (rather than counting reports) is deliberate: a shutdown flush that
    // genuinely failed would emit a real closing report on the `discarded` axis, which is not
    // this test's subject and must not read as a failure of it.
    handle.shutdown();
    let after: u64 = log.reports().iter().map(|r| r.dropped_delta).sum();
    assert_eq!(
        after, dropped,
        "the closing report re-stated a dropped delta that had already been reported — the \
             deltas must partition the total exactly, teardown included"
    );
}

/// THE regression test for "a gapped tape can never be NAMED": TWO producers share one bounded
/// queue, and the report must say WHICH series lost rows, not merely how many rows were lost.
///
/// ⚠ **Two producers is the whole point, and no single-producer test can stand in.** The three
/// tests that already saturate the bound — `overflow_drops_and_counts`,
/// `equity_overflow_drops_and_counts` and
/// `a_drop_burst_is_reported_once_per_delta_then_falls_silent` — flood ONE series from ONE
/// thread and assert `dropped() > 0`, which a counter that names nobody passes exactly as well
/// as one that names everybody. `age_flush_fires_under_busy_other_series` IS two-producer, but
/// runs at the 65,536 default and never reaches the bound at all.
///
/// ⚠ **This is not starvation, and the test does not measure fairness.** While the queue is full
/// EVERY producer's `try_send` fails; the quiet one simply gets far fewer attempts at a freed
/// slot, so it loses a much larger SHARE of its own rows — which is the the CI box shape, a Polymarket
/// L2 stream sharing this queue with a Binance tape. What makes that invisible is that the quiet
/// series is still receiving SOME rows: `flush_buf_with` names a series only on a PERMANENT
/// FLUSH FAILURE (none here — asserted), and `liveness`/`SilenceWatch` name one only when it
/// stops ENTIRELY (it does not). PARTIAL loss is the residual between them, and it is what this
/// pins.
///
/// Every wait is a CONDITION, never a fixed sleep (this file's established idiom): the loop
/// sends quiet rows until the observer has NAMED the quiet series, so it exits the instant the
/// property holds and the deadline can only ever turn a hang into a named failure.
#[test]
fn two_producers_overflow_and_the_quiet_series_is_named() {
    let (_dir, store) = open_store();
    let log = ReportLog::default();
    // `channel_cap: 1` + the chatty report cadence: the queue is full essentially always, which
    // is the condition BOTH producers are dropping under.
    let (sink, handle) =
        RecorderSink::spawn_with_observer(store.clone(), chatty(1), Some(log.observer())).unwrap();

    // BUSY: floods `trade` rows continuously, as a live venue tape would. A different KIND and
    // a different venue, so the quiet series can only be found by its own key.
    let busy_sink = sink.clone();
    let busy_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let busy_stop_thread = busy_stop.clone();
    let busy = thread::spawn(move || {
        let mut i = 0i64;
        while !busy_stop_thread.load(Ordering::Relaxed) {
            // Bursts rather than an unbroken spin: 32 rows into a ONE-slot queue already keeps
            // it occupied through the writer's ~30ms commits, and pausing between them keeps a
            // test that shares a CI box with seven others from pinning a core.
            for _ in 0..32 {
                busy_sink.trade("binance", "BTCUSDT", trade(i, i as f64));
                i += 1;
            }
            thread::sleep(Duration::from_millis(1));
        }
    });

    // QUIET: a low-rate producer on its own series, sent from THIS thread — so once the loop
    // stops, its per-series count is FINAL (nothing else in this test sends a quote), which is
    // what lets the persisted-row equation below be an equality rather than a bound.
    let quiet = quote_key("polymarket", "QUIET");
    let mut sent_quiet = 0i64;
    let started = Instant::now();
    loop {
        for _ in 0..32 {
            sink.quote("polymarket", "QUIET", quote(sent_quiet, sent_quiet as f64));
            sent_quiet += 1;
        }
        let reports = log.reports();
        // ⚠ BOTH series, and that is a 2026-09-20 fix for a flake this test carried for a
        // month. The exit used to be "the quiet series is named", after which the busy producer
        // was STOPPED and the assertion below demanded it be named too — an ASSUMPTION that it
        // had already contended. On a loaded box it had not: measured on the CI box at a load average
        // three times the core count, `dropped_by_series` held `{QUIET: 60}` and nothing else,
        // because the busy THREAD had not been scheduled yet. Nothing was wrong with the
        // recorder; the test stopped a producer that had never run and then required evidence
        // only running produces.
        //
        // Waiting for both is a CONDITION, which is this file's own stated idiom and what the
        // rest of this test already does. It does not weaken what is pinned — the assertions
        // below are unchanged, and the one that matters ("the busy series must be named as
        // well") is now GUARANTEED by the exit rather than hoped for. A genuine regression, a
        // counter that names nobody or only the series asked about, still fails: it fails on
        // the deadline, which now says WHICH half is missing.
        let by_series = handle.dropped_by_series();
        let quiet_named = reports.iter().any(|r| names_series(r, &quiet));
        let busy_named = by_series.keys().any(|k| k.kind == "trade" && k.symbol == "BTCUSDT");
        if quiet_named && busy_named {
            break;
        }
        assert!(
            started.elapsed() < REPORT_DEADLINE,
            "after {:?} the drop report still does not name BOTH series — quiet named: \
                 {quiet_named}, busy named: {busy_named} ({sent_quiet} quiet rows sent; {} rows \
                 dropped in total, attributed to [{}]). A `busy named: false` with drops recorded \
                 for the quiet series means the busy producer never contended — widen the deadline \
                 or quiesce the box. Both false means nothing overflowed at all. Quiet false with \
                 busy true is the real regression this test exists to catch: per-series attribution \
                 that misses the series losing the LARGEST SHARE of its own rows",
            started.elapsed(),
            handle.dropped(),
            handle.dropped_summary()
        );
        thread::sleep(Duration::from_millis(2));
    }
    busy_stop.store(true, Ordering::Relaxed);
    busy.join().unwrap();

    // Both producers have stopped, so every count below is final.
    let by_series = handle.dropped_by_series();
    let dropped_quiet = by_series.get(&quiet).copied().unwrap_or(0);
    assert!(dropped_quiet > 0, "the loop exits only once the quiet series was named");
    // ⚠ Now GUARANTEED by the loop exit above, exactly like `dropped_quiet > 0` on the line
    // before it — both are re-asserted because a future edit to the exit condition must redden
    // something, and because the message is what a reader lands on. Until 2026-09-20 this was
    // the only place the property was checked, and it was checked against a producer the test
    // had just stopped without ever confirming it ran.
    assert!(
        by_series.keys().any(|k| k.kind == "trade" && k.symbol == "BTCUSDT"),
        "the busy series shares the queue and loses rows too, so it must be named as well — \
             not only whichever series the test thought to ask about: {by_series:?}"
    );
    let summary = handle.dropped_summary();
    assert!(
        summary.contains("quote/polymarket/QUIET="),
        "the rendered tally must spell the series the way `liveness` keys it: {summary}"
    );

    // WHAT THE QUIET SERIES PERSISTS. Every row that reached the writer flushes on the age
    // cadence (20ms here), so the store converges to exactly what was not dropped. A CONDITION
    // wait again — rows may still be in flight at this line — and coarse, because each poll is
    // a full DataFusion read competing with the writer it is waiting for.
    let settle = Instant::now();
    let want = sent_quiet - dropped_quiet as i64;
    let persisted = loop {
        let got = store.scan_quotes("polymarket", "QUIET", TsRange::all()).unwrap();
        if got.len() as i64 == want {
            break got.len() as i64;
        }
        assert!(
            settle.elapsed() < REPORT_DEADLINE,
            "the quiet series never settled at sent({sent_quiet}) - dropped({dropped_quiet}) \
                 = {want}: {} rows persisted after {:?}, discarded={}",
            got.len(),
            settle.elapsed(),
            handle.discarded()
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(handle.discarded(), 0, "no flush failed — every loss here was a drop");
    assert!(
        persisted < sent_quiet,
        "the quiet tape MUST have a hole: {dropped_quiet} of {sent_quiet} rows never made it \
             onto the queue, and a series still persisting {persisted} looks alive from every \
             other vantage point"
    );

    // ...and what the REPORT said about it. `shutdown` joins the writer after emitting the
    // closing report, so by the line after it everything lost has been reported exactly once.
    handle.shutdown();
    let mut reported_quiet = 0u64;
    for report in log.reports() {
        for row in &report.dropped_series {
            if row.series == quiet {
                reported_quiet += row.delta;
            }
        }
        // The named rows never exceed the scalar: a drop nothing could attribute is still
        // counted, and never counted twice (see `DropReport::dropped_series`).
        let named: u64 = report.dropped_series.iter().map(|s| s.delta).sum();
        assert!(
            named <= report.dropped_delta,
            "a report named {named} rows but claims {} were dropped: {report:?}",
            report.dropped_delta
        );
    }
    assert_eq!(
        reported_quiet, dropped_quiet,
        "the per-series deltas must partition that series' own total exactly, teardown \
             included — the same discipline the scalar deltas are held to"
    );
}

/// The other half of the contract: a recorder that loses nothing must be COMPLETELY silent.
/// This is what makes the first warn above meaningful rather than background noise.
#[test]
fn a_healthy_recorder_reports_nothing() {
    let (_dir, store) = open_store();
    let log = ReportLog::default();
    let cfg = RecorderConfig { max_rows: 10, ..chatty(65_536) };
    let (sink, handle) =
        RecorderSink::spawn_with_observer(store.clone(), cfg, Some(log.observer())).unwrap();

    for i in 0..100i64 {
        sink.quote("polymarket", "TOK", quote(i, i as f64));
    }
    thread::sleep(Duration::from_millis(300)); // many report windows pass

    assert_eq!(handle.dropped(), 0, "a roomy channel drops nothing");
    assert_eq!(handle.discarded(), 0, "a healthy store discards nothing");
    assert_eq!(handle.lost(), 0);
    // ...and the keyed half is silent too: no series is named, not even with a zero beside it.
    assert!(handle.dropped_by_series().is_empty(), "nothing lost ⇒ nothing named");
    assert_eq!(handle.dropped_summary(), "none");
    assert!(log.reports().is_empty(), "nothing lost ⇒ no report at all");

    handle.shutdown(); // the CLOSING report must also stay silent
    assert!(log.reports().is_empty(), "a clean teardown emits no closing report either");
}

/// A flush that fails permanently DISCARDS its buffer — that is unavoidable, there is nowhere
/// else to put the rows — but the loss must be counted and reported, not just logged once.
///
/// The wedge: plant a plain FILE where the series directory belongs, so `SeriesLock::acquire`'s
/// opening `create_dir_all` fails immediately and every `append_quotes` for this series fails
/// fast and permanently. (Failing FAST also exercises the retry — a slow failure is skipped, see
/// `append_retry_skips_a_slow_failure`.)
#[test]
fn a_permanently_failed_flush_discards_and_counts_the_rows() {
    let (dir, store) = open_store();
    let series = quote_series_dir(dir.path(), "polymarket", "TOK");
    std::fs::create_dir_all(series.parent().expect("series dir has a parent")).unwrap();
    std::fs::write(&series, b"not a directory").unwrap();

    let log = ReportLog::default();
    // max_rows == the row count, so ingesting the last row triggers the doomed flush directly —
    // no shutdown needed, which lets the handle still be read afterwards.
    let cfg = RecorderConfig { max_rows: 3, ..chatty(65_536) };
    let (sink, handle) =
        RecorderSink::spawn_with_observer(store.clone(), cfg, Some(log.observer())).unwrap();

    for i in 0..3i64 {
        sink.quote("polymarket", "TOK", quote(i, i as f64));
    }

    let start = Instant::now();
    while log.count() == 0 {
        assert!(
            start.elapsed() < REPORT_DEADLINE,
            "a discarded flush was never reported (discarded={})",
            handle.discarded()
        );
        thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(handle.discarded(), 3, "every row of the failed buffer is counted lost");
    assert_eq!(handle.dropped(), 0, "these rows reached the writer — they were not dropped");
    assert_eq!(handle.lost(), 3, "`lost` is the one number that answers 'any holes?'");

    let first = log.reports()[0].clone();
    assert_eq!(first.discarded_delta, 3);
    assert_eq!(first.discarded_total, 3);
    assert_eq!(first.dropped_delta, 0);
    // Nothing was lost at the CHANNEL, so nothing is named there — a discarded batch is named
    // by `flush_buf_with`'s own `venue`/`symbol` warn instead. The summary must say so in
    // words rather than render a blank field.
    assert!(first.dropped_series.is_empty(), "no row was dropped at the channel here");
    assert_eq!(first.dropped_series_summary(), "none");
    assert_eq!(first.lost_delta(), 3);
    assert_eq!(first.lost_total(), 3);
    assert!(!first.final_report, "this one came from the periodic cadence");

    handle.shutdown();
}

// ---- the flush retry policy (pure — no store needed, so the failure modes are exact) --------

#[test]
fn append_retry_does_not_re_run_a_success() {
    let mut calls = 0u32;
    // A long retry delay proves the success path never sleeps: a test that reached the sleep
    // would hang for 30s rather than finish instantly.
    let (res, attempts) = append_with_retry(
        || {
            calls += 1;
            Ok(3)
        },
        Duration::from_secs(30),
        Duration::from_secs(1),
    );
    assert_eq!(res.expect("ok"), 3);
    assert_eq!(attempts, 1);
    assert_eq!(calls, 1, "a successful append must not be repeated");
}

#[test]
fn append_retry_recovers_a_transient_fast_failure() {
    let mut calls = 0u32;
    let (res, attempts) = append_with_retry(
        || {
            calls += 1;
            if calls == 1 { Err(DataError::Io("transient".into())) } else { Ok(7) }
        },
        Duration::ZERO,
        Duration::from_secs(1),
    );
    assert_eq!(res.expect("recovered"), 7, "the retry's rows are NOT lost");
    assert_eq!(attempts, 2);
    assert_eq!(calls, 2, "one failure, one retry, done");
}

#[test]
fn append_retry_gives_up_after_exactly_one_extra_attempt() {
    let mut calls = 0u32;
    let (res, attempts) = append_with_retry(
        || {
            calls += 1;
            Err(DataError::Io("boom".into()))
        },
        Duration::ZERO,
        Duration::from_secs(1),
    );
    assert!(res.is_err(), "a permanent failure still fails");
    assert_eq!(attempts, 2, "bounded: ONE retry, never an unbounded loop");
    assert_eq!(calls, 2, "the append ran exactly twice, then the caller discards");
}

#[test]
fn append_retry_skips_a_slow_failure() {
    // A failure that took `slow_attempt` or longer has already burned the store's own retry
    // budget (`SeriesLock::acquire` spins ~4s on a contended lock). Retrying it would mostly
    // double this single writer thread's stall — and every stalled millisecond is more rows
    // dropped at the channel behind it — so it is deliberately not retried.
    let mut calls = 0u32;
    let (res, attempts) = append_with_retry(
        || {
            calls += 1;
            thread::sleep(Duration::from_millis(30));
            Err(DataError::Io("timeout acquiring series lock".into()))
        },
        Duration::ZERO,
        Duration::from_millis(10), // tiny threshold keeps the test fast
    );
    assert!(res.is_err());
    assert_eq!(attempts, 1, "a slow failure is not retried");
    assert_eq!(calls, 1);
}
