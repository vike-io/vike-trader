//! Flush triggers (rows, age, UTC rollover, shutdown), overflow, and the book/depth/equity lanes.

use super::*;

const DAY: i64 = 86_400_000;

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
