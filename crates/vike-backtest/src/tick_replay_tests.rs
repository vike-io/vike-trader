use super::*;
use std::assert_matches;
use vike_marketdata::L2BookLevel;
use vike_marketdata::L2BookUpdateKind;

fn quote(ts: i64, bid: f64) -> TickQuote {
    TickQuote {
        timestamp: ts,
        local_timestamp: 0,
        bid,
        ask: bid + 1.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: "TKN".to_string(),
    }
}

fn trade(ts: i64, price: f64) -> TickTrade {
    TickTrade {
        timestamp: ts,
        local_timestamp: 0,
        price,
        size: 1.0,
        is_buyer_maker: false,
        symbol: "TKN".to_string(),
    }
}

#[test]
fn merge_interleaves_by_timestamp() {
    let quotes = vec![quote(1, 10.0), quote(3, 30.0)];
    let trades = vec![trade(2, 20.0), trade(4, 40.0)];
    let merged = merge_quote_trade(quotes, trades);
    assert_eq!(merged.len(), 4);
    match &merged[0] {
        TickEvent::Quote(q) => {
            assert_eq!(q.timestamp, 1);
            assert_eq!(q.bid, 10.0);
        }
        other => panic!("expected Quote at index 0, got {other:?}"),
    }
    match &merged[1] {
        TickEvent::Trade(t) => {
            assert_eq!(t.timestamp, 2);
            assert_eq!(t.price, 20.0);
        }
        other => panic!("expected Trade at index 1, got {other:?}"),
    }
    match &merged[2] {
        TickEvent::Quote(q) => {
            assert_eq!(q.timestamp, 3);
            assert_eq!(q.bid, 30.0);
        }
        other => panic!("expected Quote at index 2, got {other:?}"),
    }
    match &merged[3] {
        TickEvent::Trade(t) => {
            assert_eq!(t.timestamp, 4);
            assert_eq!(t.price, 40.0);
        }
        other => panic!("expected Trade at index 3, got {other:?}"),
    }
}

#[test]
fn merge_equal_timestamp_quote_first() {
    let quotes = vec![quote(5, 99.0)];
    let trades = vec![trade(5, 100.0)];
    let merged = merge_quote_trade(quotes, trades);
    assert_eq!(merged.len(), 2);
    match &merged[0] {
        TickEvent::Quote(q) => {
            assert_eq!(q.timestamp, 5);
            assert_eq!(q.bid, 99.0);
        }
        other => panic!("expected Quote to precede Trade at equal ts, got {other:?}"),
    }
    match &merged[1] {
        TickEvent::Trade(t) => {
            assert_eq!(t.timestamp, 5);
            assert_eq!(t.price, 100.0);
        }
        other => panic!("expected Trade second at equal ts, got {other:?}"),
    }
}

#[test]
fn merge_one_empty() {
    // empty trades -> all quotes, order preserved
    let quotes = vec![quote(1, 10.0), quote(2, 20.0)];
    let merged = merge_quote_trade(quotes, vec![]);
    assert_eq!(merged.len(), 2);
    match &merged[0] {
        TickEvent::Quote(q) => assert_eq!(q.timestamp, 1),
        other => panic!("expected Quote, got {other:?}"),
    }
    match &merged[1] {
        TickEvent::Quote(q) => assert_eq!(q.timestamp, 2),
        other => panic!("expected Quote, got {other:?}"),
    }

    // empty quotes -> all trades, order preserved
    let trades = vec![trade(3, 30.0), trade(4, 40.0)];
    let merged = merge_quote_trade(vec![], trades);
    assert_eq!(merged.len(), 2);
    match &merged[0] {
        TickEvent::Trade(t) => assert_eq!(t.timestamp, 3),
        other => panic!("expected Trade, got {other:?}"),
    }
    match &merged[1] {
        TickEvent::Trade(t) => assert_eq!(t.timestamp, 4),
        other => panic!("expected Trade, got {other:?}"),
    }
}

#[test]
fn merge_both_empty() {
    let merged = merge_quote_trade(vec![], vec![]);
    assert!(merged.is_empty());
}

/// Same helpers as `quote`/`trade` but with an explicit machine receive stamp.
fn quote_at(ts: i64, local_ts: i64, bid: f64) -> TickQuote {
    TickQuote { local_timestamp: local_ts, ..quote(ts, bid) }
}

fn trade_at(ts: i64, local_ts: i64, price: f64) -> TickTrade {
    TickTrade { local_timestamp: local_ts, ..trade(ts, price) }
}

/// The arrival clock's three documented cases, at the source.
#[test]
fn arrival_clock_falls_back_clamps_and_lags() {
    // unstamped -> venue ts (today's position)
    let unstamped = TickEvent::Quote(quote_at(1_000, 0, 10.0));
    assert_eq!(tick_venue_timestamp(&unstamped), 1_000);
    assert_eq!(tick_local_timestamp(&unstamped), 0);
    assert_eq!(tick_arrival_timestamp(&unstamped), 1_000);

    // a real lag -> the recorded receive stamp
    let lagged = TickEvent::Trade(trade_at(1_000, 1_250, 10.0));
    assert_eq!(tick_arrival_timestamp(&lagged), 1_250);

    // CLOCK SKEW (local before venue — physically impossible) -> clamped UP to the venue ts,
    // so the modelled latency is never negative and never a look-ahead.
    let skewed = TickEvent::Quote(quote_at(1_000, 400, 10.0));
    assert_eq!(
        tick_arrival_timestamp(&skewed),
        1_000,
        "skewed local_timestamp clamps up to the venue ts"
    );
    assert!(tick_arrival_timestamp(&skewed) - tick_venue_timestamp(&skewed) >= 0);

    // a negative stamp is treated as "never stamped", not as a huge negative arrival
    let negative = TickEvent::Quote(quote_at(1_000, -5, 10.0));
    assert_eq!(tick_arrival_timestamp(&negative), 1_000);
}

/// OFF-PATH PROOF at the merge level: with every `local_timestamp` unstamped (the whole existing
/// fixture corpus), the arrival merge IS the venue merge — same ticks, same order, same
/// tie-break. A stable sort whose key equals the already-ascending venue ts is the identity.
#[test]
fn arrival_merge_equals_venue_merge_when_unstamped() {
    let quotes = vec![quote(1, 10.0), quote(3, 30.0), quote(3, 31.0)];
    let trades = vec![trade(2, 20.0), trade(3, 33.0)];
    let books = vec![L2BookUpdate {
        timestamp: 3,
        local_timestamp: 0,
        sequence: 1,
        kind: L2BookUpdateKind::Snapshot,
        tick_size: 0.01,
        bids: vec![L2BookLevel::new(0.4, 1.0)],
        asks: vec![L2BookLevel::new(0.6, 1.0)],
        symbol: "TKN".to_string(),
    }];

    let venue = merge_ticks(quotes.clone(), trades.clone(), books.clone());
    let arrival = merge_ticks_by_arrival(quotes, trades, books);
    assert_eq!(venue.len(), arrival.len());
    for (v, a) in venue.iter().zip(&arrival) {
        assert_eq!(tick_venue_timestamp(v), tick_venue_timestamp(a));
        assert_eq!(
            std::mem::discriminant(v),
            std::mem::discriminant(a),
            "same tick KIND at the same position (Book/Quote/Trade tie-break preserved)"
        );
    }
}

/// ON with a lagging stamp: the LATE tick is delivered after the one that arrived first, even
/// though the venue stamped it earlier — and its own venue `ts` is untouched (matching is still
/// venue time).
#[test]
fn arrival_merge_reorders_a_lagging_tick() {
    // Venue order: q@1000, q@2000. Arrival order: q@2000 (recv 2100) BEFORE q@1000 (recv 5000).
    let quotes = vec![quote_at(1_000, 5_000, 10.0), quote_at(2_000, 2_100, 20.0)];
    let merged = merge_ticks_by_arrival(quotes, vec![], vec![]);
    assert_eq!(merged.len(), 2);
    assert_eq!(tick_venue_timestamp(&merged[0]), 2_000, "the tick that ARRIVED first goes first");
    assert_eq!(tick_arrival_timestamp(&merged[0]), 2_100);
    assert_eq!(
        tick_venue_timestamp(&merged[1]),
        1_000,
        "venue ts is NOT rewritten by the re-order"
    );
    assert_eq!(tick_arrival_timestamp(&merged[1]), 5_000);
}

/// A PARTLY stamped tape: the unstamped tick falls back to its venue ts and keeps its place;
/// only the stamped, genuinely-late tick moves.
#[test]
fn arrival_merge_mixes_stamped_and_unstamped_cleanly() {
    let quotes = vec![
        quote_at(1_000, 0, 10.0),     // unstamped -> arrival 1000
        quote_at(2_000, 9_000, 20.0), // 7s late   -> arrival 9000
        quote_at(3_000, 0, 30.0),     // unstamped -> arrival 3000
    ];
    let merged = merge_ticks_by_arrival(quotes, vec![], vec![]);
    let order: Vec<i64> = merged.iter().map(tick_venue_timestamp).collect();
    assert_eq!(order, vec![1_000, 3_000, 2_000], "only the late stamped tick moves");
}

/// A SKEWED stamp must not jump the queue: clamped to its venue ts, the tick stays exactly
/// where the venue-ordered merge put it.
#[test]
fn arrival_merge_clamps_skew_instead_of_delivering_early() {
    let quotes = vec![
        quote_at(1_000, 0, 10.0),
        // local_timestamp claims it arrived at 500, BEFORE the 1000-stamped tick above; clamping to
        // its own venue ts (2000) keeps it second instead of promoting it to first.
        quote_at(2_000, 500, 20.0),
    ];
    let merged = merge_ticks_by_arrival(quotes, vec![], vec![]);
    let order: Vec<i64> = merged.iter().map(tick_venue_timestamp).collect();
    assert_eq!(order, vec![1_000, 2_000], "a skewed stamp never delivers a tick early");
}

#[test]
fn merge_ticks_equal_timestamp_book_quote_trade_order() {
    let books = vec![L2BookUpdate {
        timestamp: 5,
        local_timestamp: 0,
        sequence: 1,
        kind: L2BookUpdateKind::Snapshot,
        tick_size: 0.01,
        bids: vec![L2BookLevel::new(0.4, 1.0)],
        asks: vec![L2BookLevel::new(0.6, 1.0)],
        symbol: "TKN".to_string(),
    }];
    let merged = merge_ticks(vec![quote(5, 99.0)], vec![trade(5, 100.0)], books);
    assert_matches!(merged[0], TickEvent::Book(_), "book first at equal ts");
    assert_matches!(merged[1], TickEvent::Quote(_));
    assert_matches!(merged[2], TickEvent::Trade(_));
}

/// The replay books one ledger currency (`StrategyEngineConfig::account_currency`, the dollar when unset) and holds
/// no rate series, so a symbol its name says is not quoted in that currency is refused before the
/// engine is built; one that is, is not.
#[test]
fn a_foreign_quoted_symbol_is_refused_before_the_engine_is_built() {
    struct Noop;
    impl Strategy<OrderBrokerSimulator> for Noop {}

    let config = |symbol: &str, currency: Option<&str>| TickReplayConfig {
        venue: "oanda".to_string(),
        symbols: vec![symbol.to_string()],
        range: TimestampRange::all(),
        seed_bar_interval_ms: None,
        params: StrategyEngineConfig {
            account_currency: currency.map(str::to_string),
            ..Default::default()
        },
        snap_to_properties: false,
        feed_latency: false,
        series: None,
    };
    let rows = |symbol: &str, px: f64| -> TickReplayRows {
        vec![(symbol.to_string(), vec![quote(1, px)], vec![], vec![])]
    };

    let err = replay_ticks_core(rows("USDJPY", 150.0), Noop, config("USDJPY", None))
        .expect_err("a yen-quoted replay is refused on the dollar ledger");
    assert_matches!(&err, TickReplayError::ForeignQuote(why) if why.contains("USDJPY (JPY)"));

    let verdict = replay_ticks_core(rows("EURUSD", 1.08), Noop, config("EURUSD", None));
    assert!(
        !matches!(verdict, Err(TickReplayError::ForeignQuote(_))),
        "a dollar-quoted symbol runs"
    );

    // The same symbol on a yen ledger is admitted; a dollar-quoted one on it is the refusal.
    let verdict = replay_ticks_core(rows("USDJPY", 150.0), Noop, config("USDJPY", Some("JPY")));
    assert!(!matches!(verdict, Err(TickReplayError::ForeignQuote(_))), "a yen ledger takes USDJPY");
    let err = replay_ticks_core(rows("EURUSD", 1.08), Noop, config("EURUSD", Some("JPY")))
        .expect_err("a dollar-quoted replay is refused on the yen ledger");
    assert_matches!(&err, TickReplayError::ForeignQuote(why) if why.contains("EURUSD (USD)"));
}

/// The simulator cannot book a coin-margined contract (`crate::harness::refuse_inverse_contracts`), so a
/// replay over one is refused before the engine is built, whichever way the config names the series.
#[test]
fn an_inverse_contract_is_refused_before_the_engine_is_built() {
    struct Noop;
    impl Strategy<OrderBrokerSimulator> for Noop {}

    let config =
        |venue: &str, symbol: &str, series: Option<Vec<ProfileDataSeries>>| TickReplayConfig {
            venue: venue.to_string(),
            symbols: vec![symbol.to_string()],
            range: TimestampRange::all(),
            seed_bar_interval_ms: None,
            params: StrategyEngineConfig::default(),
            snap_to_properties: false,
            feed_latency: false,
            series,
        };
    let rows = |symbol: &str| -> TickReplayRows {
        vec![(symbol.to_string(), vec![quote(1, 65_700.0)], vec![], vec![])]
    };

    let by_venue = config("deribit", "BTC-PERPETUAL", None);
    let err = replay_ticks_core(rows("BTC-PERPETUAL"), Noop, by_venue)
        .expect_err("an inverse replay is refused");
    assert_matches!(
        &err,
        TickReplayError::InverseContract(why) if why.contains("BTC-PERPETUAL on deribit")
    );

    // `series` names each row's own venue, and that is the venue that decides.
    let series = vec![ProfileDataSeries::new("okx", "BTC-USD-SWAP")];
    let by_series = config("binance", "BTC-USD-SWAP", Some(series));
    let err = replay_ticks_core(rows("BTC-USD-SWAP"), Noop, by_series)
        .expect_err("an inverse series is refused");
    assert_matches!(&err, TickReplayError::InverseContract(why) if why.contains("BTC-USD-SWAP on okx"));

    let verdict = replay_ticks_core(
        rows("BTC_USDC-PERPETUAL"),
        Noop,
        config("deribit", "BTC_USDC-PERPETUAL", None),
    );
    assert!(!matches!(verdict, Err(TickReplayError::InverseContract(_))), "a linear book runs");
}

fn tick_of(tick_size: f64) -> SymbolProperties {
    SymbolProperties { tick_size, ..Default::default() }
}

/// A tape of `(symbol, ts, grid)` rows that counts the store asks a source makes.
struct CountedTape {
    rows: Vec<(&'static str, i64, SymbolProperties)>,
    asks: std::sync::atomic::AtomicUsize,
}

impl CountedTape {
    fn over(rows: Vec<(&'static str, i64, SymbolProperties)>) -> Arc<Self> {
        Arc::new(Self { rows, asks: Default::default() })
    }

    /// The whole-scan answer every lookup must equal: the last row of the symbol at or before `ts`.
    fn scanned(&self, symbol: &str, ts: i64) -> Option<SymbolProperties> {
        self.rows.iter().rfind(|r| r.0 == symbol && r.1 <= ts).map(|r| r.2)
    }
}

impl HistoryStore for CountedTape {
    fn scan_symbol_properties(
        &self,
        _: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, HistoryStoreError> {
        self.asks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let end = range.end.unwrap_or(i64::MAX);
        Ok(self.rows.iter().filter(|r| r.0 == symbol && r.1 <= end).map(|r| (r.1, r.2)).collect())
    }
    vike_data::hist_store_stubs!(inert: writes, load_bars, scan_quotes, scan_trades,
        scan_book_updates, scan_equity, scan_exec_fills, scan_exec_orders);
}

/// Each lookup was one round trip to the datahub, and a snapping run makes several per fill. A day whose
/// answer stands asks the store twice, however many fills it holds.
#[test]
fn a_day_of_lookups_with_one_answer_asks_the_store_twice() {
    let day = 20 * vike_model::time::MS_PER_DAY;
    let tape = CountedTape::over(vec![("A", day - 5, tick_of(0.1))]);
    let lookup = properties_source(tape.clone(), Arc::default());

    for i in 0..100 {
        assert_eq!(lookup("v", "A", day + 1_000 + i), Some(tick_of(0.1)));
    }
    assert_eq!(tape.asks.load(std::sync::atomic::Ordering::Relaxed), 2);
}

/// The kept answer is the store's answer at every lookup: a row landing later in the day, a day that
/// starts on yesterday's row, a grid that returns to an earlier value, a lookup going back, a symbol
/// with no row at all, and a second symbol that must not read the first one's.
#[test]
fn a_kept_answer_is_what_the_whole_scan_answers_at_every_lookup() {
    use vike_model::time::MS_PER_DAY;
    let day = |n: i64| n * MS_PER_DAY;
    let (a, b, c) = (tick_of(0.1), tick_of(0.2), tick_of(0.3));
    let tape = CountedTape::over(vec![
        ("A", day(20) + 500, a),
        ("A", day(21) + 500, a),
        ("A", day(22) + 500, b),
        ("A", day(23) + 500, a),
        ("B", day(21) + 100, c),
    ]);
    let lookup = properties_source(tape.clone(), Arc::default());

    let mut stamps: Vec<i64> = Vec::new();
    for n in 19..=25 {
        stamps.extend([0, 499, 500, 501, MS_PER_DAY - 1].map(|at| day(n) + at));
    }
    let forward_then_back = stamps.iter().chain(stamps.iter().rev()).copied();
    for ts in forward_then_back {
        for symbol in ["A", "B", "NO-ROW"] {
            assert_eq!(lookup("v", symbol, ts), tape.scanned(symbol, ts), "{symbol} at {ts}");
        }
    }
}

/// A Studio tick slice's empty symbol reaches the wire: `to_wire_result` copies what the Studio renders,
/// and this is the sentence that says the report leaves a symbol out. A clean result's frame has no key.
#[test]
fn what_a_run_found_in_its_data_reaches_the_wire_answer() {
    let warned = BacktestResult {
        data_warnings: vec!["ETHUSDT: did not run".to_string()],
        ..Default::default()
    };
    let frame = serde_json::to_value(vike_backtesthub_client::wire_result::to_wire_result(&warned))
        .unwrap();
    assert_eq!(frame["data_warnings"], serde_json::json!(["ETHUSDT: did not run"]), "{frame}");

    let clean = serde_json::to_value(vike_backtesthub_client::wire_result::to_wire_result(
        &Default::default(),
    ));
    assert!(clean.unwrap().get("data_warnings").is_none());
}
