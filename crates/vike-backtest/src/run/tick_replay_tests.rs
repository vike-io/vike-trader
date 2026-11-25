use super::tape::{TickReplayRows, tape_of};
use super::*;
use std::assert_matches;
use vike_marketdata::Bar;
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
        symbol: "TKN".into(),
    }
}

fn trade(ts: i64, price: f64) -> TickTrade {
    TickTrade {
        timestamp: ts,
        local_timestamp: 0,
        price,
        size: 1.0,
        is_buyer_maker: false,
        symbol: "TKN".into(),
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
        reference: Vec::new(),
        mark: Vec::new(),
        feed_status: false,
        series: None,
    };
    let rows = |symbol: &str, px: f64| -> TickReplayRows {
        vec![(symbol.to_string(), vec![quote(1, px)], vec![], vec![])]
    };

    let err = no_rows_core(rows("USDJPY", 150.0), Noop, config("USDJPY", None))
        .expect_err("a yen-quoted replay is refused on the dollar ledger");
    assert_matches!(&err, TickReplayError::ForeignQuote(why) if why.contains("USDJPY (JPY)"));

    let verdict = no_rows_core(rows("EURUSD", 1.08), Noop, config("EURUSD", None));
    assert!(
        !matches!(verdict, Err(TickReplayError::ForeignQuote(_))),
        "a dollar-quoted symbol runs"
    );

    // The same symbol on a yen ledger is admitted; a dollar-quoted one on it is the refusal.
    let verdict = no_rows_core(rows("USDJPY", 150.0), Noop, config("USDJPY", Some("JPY")));
    assert!(!matches!(verdict, Err(TickReplayError::ForeignQuote(_))), "a yen ledger takes USDJPY");
    let err = no_rows_core(rows("EURUSD", 1.08), Noop, config("EURUSD", Some("JPY")))
        .expect_err("a dollar-quoted replay is refused on the yen ledger");
    assert_matches!(&err, TickReplayError::ForeignQuote(why) if why.contains("EURUSD (USD)"));
}

/// The simulator cannot book a coin-margined contract (`crate::run::refuse_inverse_contracts`), so a
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
            reference: Vec::new(),
            mark: Vec::new(),
            feed_status: false,
            series,
        };
    let rows = |symbol: &str| -> TickReplayRows {
        vec![(symbol.to_string(), vec![quote(1, 65_700.0)], vec![], vec![])]
    };

    let by_venue = config("deribit", "BTCUSD.P", None);
    let err =
        no_rows_core(rows("BTCUSD.P"), Noop, by_venue).expect_err("an inverse replay is refused");
    assert_matches!(
        &err,
        TickReplayError::InverseContract(why) if why.contains("BTCUSD.P on deribit")
    );

    // `series` names each row's own venue, and that is the venue that decides (`BTCUSD.P` is no coin-margined
    // name on hyperliquid, and okx's inverse swap on okx).
    let series = vec![ProfileDataSeries::new("okx", "BTCUSD.P")];
    let by_series = config("hyperliquid", "BTCUSD.P", Some(series));
    let err =
        no_rows_core(rows("BTCUSD.P"), Noop, by_series).expect_err("an inverse series is refused");
    assert_matches!(&err, TickReplayError::InverseContract(why) if why.contains("BTCUSD.P on okx"));

    let verdict = no_rows_core(rows("BTCUSDC.P"), Noop, config("deribit", "BTCUSDC.P", None));
    assert!(!matches!(verdict, Err(TickReplayError::InverseContract(_))), "a linear book runs");
}

/// The replay after its scans ([`refuse_unrunnable`], [`tape_of`], [`run_tape`]) over no kept rows: what a store
/// that keeps no lists hands it.
fn no_rows_core<S: Strategy<OrderBrokerSimulator>>(
    rows: TickReplayRows,
    strategy: S,
    config: TickReplayConfig,
) -> Result<TradingRecord, TickReplayError> {
    refuse_unrunnable(&config, &KeptRows::default())?;
    let tape = TapeCut::whole(Arc::new(tape_of(rows, Vec::new(), &config)?));
    run_tape(Arc::new(ListsStore { lists: Some(Vec::new()) }), strategy, config, &tape)
}

/// An empty store that serves `lists` as its kept instrument lists, or FAILS to serve them (`None`).
struct ListsStore {
    lists: Option<Vec<vike_model::Instrument>>,
}

impl HistoryStore for ListsStore {
    fn load_bars(
        &self,
        _venue: &str,
        _symbol: &str,
        _interval: &str,
        _range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        Ok(Vec::new())
    }
    fn kept_instruments(&self) -> Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError> {
        let down = || HistoryStoreError::Query("the datahub is down".into());
        self.lists.clone().map(Some).ok_or_else(down)
    }
    vike_data::hist_store_stubs!(inert: writes, scan_quotes, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

/// **A replay no profile resolved reads the KEPT rows its store serves** (flag BYB-4's replay half): bybit's spot
/// `USDTUSD`, a name the old name rule read as a one-dollar inverse contract, runs as the linear pair its row says it
/// is; with no row the user name decides (`BTCUSD.P` is the inverse perpetual, decision 0290); a store that FAILS to
/// serve the lists refuses the replay in words, never reads the names in their place.
#[test]
fn a_replay_reads_the_kept_rows_its_store_serves_and_refuses_when_it_cannot() {
    struct Noop;
    impl Strategy<OrderBrokerSimulator> for Noop {}
    let config = |symbol: &str| TickReplayConfig {
        venue: "bybit".to_string(),
        symbols: vec![symbol.to_string()],
        range: TimestampRange::all(),
        seed_bar_interval_ms: None,
        params: StrategyEngineConfig::default(),
        snap_to_properties: false,
        feed_latency: false,
        reference: Vec::new(),
        mark: Vec::new(),
        feed_status: false,
        series: None,
    };
    let spot = vike_model::Instrument::new(
        "bybit",
        "USDTUSD",
        "USDT",
        "USD",
        vike_model::Contract::CryptoSpot,
    );
    // served as the datahub keeps it, named (`vike_catalog::spelled`)
    let served = ListsStore { lists: Some(vike_catalog::spelled(vec![spot])) };
    let verdict = replay_ticks(Arc::new(served), Noop, config("USDTUSD"));
    assert!(
        !matches!(verdict, Err(TickReplayError::InverseContract(_))),
        "its row is spot: {:?}",
        verdict.as_ref().err()
    );
    let none = ListsStore { lists: Some(Vec::new()) };
    let named =
        replay_ticks(Arc::new(none), Noop, config("BTCUSD.P")).expect_err("no row: the name");
    assert_matches!(&named, TickReplayError::InverseContract(why) if why.contains("BTCUSD.P on bybit"));
    let failed = ListsStore { lists: None };
    let err =
        replay_ticks(Arc::new(failed), Noop, config("USDTUSD")).expect_err("a failed read refuses");
    assert!(
        err.to_string().contains("kept instrument lists could not be read")
            && err.to_string().contains("the datahub is down"),
        "{err}"
    );
}

/// A store that serves every venue's kept `lists` and a quote tape for any symbol, and keeps the venues each
/// `kept_instruments_and_splits_of` asked for.
struct VenuesStore {
    lists: Vec<vike_model::Instrument>,
    asked: Mutex<Vec<Vec<String>>>,
}

impl HistoryStore for VenuesStore {
    fn kept_instruments(&self) -> Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError> {
        Ok(Some(self.lists.clone()))
    }
    fn kept_instruments_and_splits_of(
        &self,
        venues: &[String],
    ) -> (
        Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError>,
        Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError>,
    ) {
        self.asked.lock().expect("asked").push(venues.to_vec());
        let named = self.lists.iter().filter(|i| venues.contains(&i.venue)).cloned().collect();
        (Ok(Some(named)), Ok(None))
    }
    fn scan_quotes(
        &self,
        _venue: &str,
        symbol: &str,
        _range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        let tape = [quote(1_000, 1.0), quote(2_000, 1.01), quote(3_000, 0.99)];
        Ok(tape.into_iter().map(|q| TickQuote { symbol: symbol.into(), ..q }).collect())
    }
    vike_data::hist_store_stubs!(inert: writes, load_bars, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

/// **A replay asks the kept rows of the venues it reads alone, and replays exactly as over every venue's lists**: a
/// datahub's whole answer is 13.4 MB of JSON on the CI box, and every lookup the replay makes is of one of its series'
/// venues. Its series' rows, a row of another venue and a session venue's row are served; a series with no row is
/// named in the data warnings either way.
#[test]
fn a_replay_asks_its_own_venues_rows_and_replays_as_over_every_list() {
    struct Noop;
    impl Strategy<OrderBrokerSimulator> for Noop {}
    let config = || TickReplayConfig {
        venue: "bybit".to_string(),
        symbols: vec!["USDTUSD".to_string(), "USDCUSD".to_string()],
        range: TimestampRange::all(),
        seed_bar_interval_ms: None,
        params: StrategyEngineConfig::default(),
        snap_to_properties: false,
        feed_latency: false,
        reference: Vec::new(),
        mark: Vec::new(),
        feed_status: false,
        series: None,
    };
    let row = |venue: &str, symbol: &str, base: &str, quoted: &str| {
        vike_model::Instrument::new(venue, symbol, base, quoted, vike_model::Contract::CryptoSpot)
    };
    let lists = vike_catalog::spelled(vec![
        row("bybit", "USDTUSD", "USDT", "USD"),
        row("okx", "USDT-USD", "USDT", "USD"),
        row("dukascopy", "EUR/USD", "EUR", "USD"),
    ]);
    let store = Arc::new(VenuesStore { lists: lists.clone(), asked: Mutex::default() });

    let narrowed =
        replay_ticks(Arc::clone(&store) as Arc<dyn HistoryStore + Send + Sync>, Noop, config());
    assert_eq!(
        *store.asked.lock().expect("asked"),
        [vec!["bybit".to_string()]],
        "its own venue alone"
    );

    let series: Vec<(String, String)> =
        config().resolved_series().into_iter().map(|s| (s.venue, s.symbol)).collect();
    let every = KeptRows::answered(Ok(Some(lists)), &series);
    let whole = replay_ticks_kept(store, Noop, config(), &every, Vec::new(), None);
    let whole = whole.map(|mut record| {
        record.data_warnings.extend(every.warnings.clone());
        record
    });
    assert!(format!("{narrowed:?}").contains("USDCUSD on bybit"), "{narrowed:?}");
    assert_eq!(format!("{narrowed:?}"), format!("{whole:?}"), "the same replay");
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

/// A Studio tick slice's empty symbol reaches the wire: `to_wire_record` copies what the Studio renders,
/// and this is the sentence that says the report leaves a symbol out. A clean result's frame has no key.
#[test]
fn what_a_run_found_in_its_data_reaches_the_wire_answer() {
    let warned = TradingRecord {
        data_warnings: vec!["ETHUSDT: did not run".to_string()],
        ..Default::default()
    };
    let frame = serde_json::to_value(vike_backtesthub_client::wire_record::to_wire_record(&warned))
        .unwrap();
    assert_eq!(frame["data_warnings"], serde_json::json!(["ETHUSDT: did not run"]), "{frame}");

    let clean = serde_json::to_value(vike_backtesthub_client::wire_record::to_wire_record(
        &Default::default(),
    ));
    assert!(clean.unwrap().get("data_warnings").is_none());
}

/// One venue's quotes of one symbol, `(timestamp, bid, ask)`.
type QuoteTape = (&'static str, &'static str, Vec<(i64, f64, f64)>);

/// A store serving one quote tape per `(venue, symbol)`, each quote stamped with the store's symbol.
struct TwoVenueTape {
    quotes: Vec<QuoteTape>,
}

impl HistoryStore for TwoVenueTape {
    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        _range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        let tape = self.quotes.iter().find(|(v, s, _)| *v == venue && *s == symbol);
        let rows = tape.map(|(_, _, rows)| rows.as_slice()).unwrap_or_default();
        Ok(rows
            .iter()
            .map(|&(timestamp, bid, ask)| TickQuote {
                timestamp,
                local_timestamp: 0,
                bid,
                ask,
                bid_size: 5.0,
                ask_size: 5.0,
                symbol: symbol.into(),
            })
            .collect())
    }
    vike_data::hist_store_stubs!(inert: writes, load_bars, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

/// **A maker on binance `BTCUSDT.P` hedging on bybit's `BTCUSDT.P`** (decision 0293): ONE user name on two
/// venues, the bybit series naming its account (`account = "bybit/DEMO"`), so it is the leg
/// `BTCUSDT.P@bybit/DEMO`. The maker's bid fills on binance's slot, its hedge on bybit's, each fill names its
/// leg, and each leg reads its own position.
#[test]
fn a_maker_and_its_hedge_on_one_user_name_each_fill_on_their_own_venues_slot() {
    const HEDGE: &str = "BTCUSDT.P@bybit/DEMO";
    type Seen = std::rc::Rc<std::cell::RefCell<(Vec<(String, i32)>, Vec<(f64, f64)>)>>;
    struct MakerHedge {
        seen: Seen,
        quoted: bool,
    }
    impl Strategy<OrderBrokerSimulator> for MakerHedge {
        fn on_quote_tick(&mut self, broker: &mut OrderBrokerSimulator, _q: &TickQuote) {
            if !self.quoted {
                self.quoted = true;
                let bid = vike_model::OrderPlacement::Limit { price: 100.0 };
                vike_model::OrderBroker::place(broker, "", "", 1, 1.0, bid, false);
            }
            let positions = (
                vike_model::OrderBroker::position(broker, "BTCUSDT.P"),
                vike_model::OrderBroker::position(broker, HEDGE),
            );
            self.seen.borrow_mut().1.push(positions);
        }
        fn on_fill(&mut self, broker: &mut OrderBrokerSimulator, fill: &vike_model::Fill) {
            self.seen.borrow_mut().0.push((fill.symbol.clone(), fill.side));
            if fill.symbol == "BTCUSDT.P" {
                let market = vike_model::OrderPlacement::Market;
                vike_model::OrderBroker::place(
                    broker, HEDGE, "", -fill.side, fill.size, market, false,
                );
            }
        }
    }

    let store = TwoVenueTape {
        quotes: vec![
            ("binance", "BTCUSDT.P", vec![(1, 99.0, 101.0), (3, 98.0, 99.5), (5, 98.0, 99.5)]),
            ("bybit", "BTCUSDT.P", vec![(2, 100.2, 100.8), (4, 100.3, 100.9), (6, 100.3, 100.9)]),
        ],
    };
    let mut hedge_series = ProfileDataSeries::new("bybit", "BTCUSDT.P");
    hedge_series.account = Some("bybit/DEMO".to_string());
    let config = TickReplayConfig {
        venue: "binance".to_string(),
        symbols: Vec::new(),
        range: TimestampRange::all(),
        seed_bar_interval_ms: None,
        params: StrategyEngineConfig { cash: 1_000_000.0, ..Default::default() },
        snap_to_properties: false,
        feed_latency: false,
        series: Some(vec![ProfileDataSeries::new("binance", "BTCUSDT.P"), hedge_series]),
        reference: Vec::new(),
        mark: Vec::new(),
        feed_status: false,
    };
    let seen: Seen = Default::default();
    let strategy = MakerHedge { seen: seen.clone(), quoted: false };
    replay_ticks(Arc::new(store), strategy, config).expect("the two-venue replay runs");

    let (fills, positions) = seen.borrow().clone();
    assert_eq!(
        fills,
        vec![("BTCUSDT.P".to_string(), 1), (HEDGE.to_string(), -1)],
        "the maker's bid fills on binance's slot and its hedge on bybit's, each under its leg's name"
    );
    assert_eq!(
        positions.last().copied(),
        Some((1.0, -1.0)),
        "each leg reads its own venue's position: {positions:?}"
    );
}
