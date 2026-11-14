//! Decision 0243 phase 2's plan through the harness: a conversion pair named in `engine.conversion`,
//! refused at load unless it reads one way against the ledger, planned as a row of its own, read and
//! judged before any price is read, and refused by name with the way to fill it.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_data::{HistoryStoreError, HistoryStoreMemory, SeriesShortfall};
use vike_marketdata::Bar;
use vike_model::Strategy;
use vike_simulator::OrderBrokerSimulator;

use super::*;
use crate::data_plan::{DataPlanSeriesRole, explain_document, plan_data};
use crate::harness::run_backtest;
use crate::harness::strategy_builder::StrategyBuilder;

/// A UTC midnight.
const S: i64 = 19_675 * DAY;
const HALF: i64 = 1_800_000;
const DAY: i64 = 86_400_000;
const PAIR: &str = "user-mine:binance:USDT-USD:1d";

fn bar(ts: i64, close: f64) -> Bar {
    Bar::ohlcv(ts, close, close, close, close, 1.0)
}

struct Nothing;
impl Strategy<OrderBrokerSimulator> for Nothing {}

/// `days` days of the run's 30-minute bars from `S` and the user's daily USDT-USD lane on `pair_days`,
/// its close on day `d` `1 + d / 1000`.
fn store(days: i64, pair_days: &[i64]) -> Arc<HistoryStoreMemory> {
    let s = HistoryStoreMemory::new();
    let price: Vec<Bar> = (0..days * 48).map(|i| bar(S + i * HALF, 100.0)).collect();
    s.append_bars("binance", "BTCUSDT", "30m", &price, None).unwrap();
    let lane: Vec<Bar> =
        pair_days.iter().map(|&d| bar(S + d * DAY, 1.0 + d as f64 / 1000.0)).collect();
    s.append_bars_sourced("binance", "user-mine", "USDT-USD", "1d", &lane, None).unwrap();
    // The same pair as a venue's own series, which the in-memory store's inventory answers for.
    s.append_bars("coinbase", "USDT-USD", "1d", &lane, None).unwrap();
    Arc::new(s)
}

/// A bar run of BTCUSDT over `days` days; `engine` goes inside `[engine]`, `data` inside `[data]`.
fn toml(days: i64, data: &str, engine: &str) -> String {
    format!(
        "[data]\nkind = \"bar\"\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\ninterval = \"30m\"\n\
         from = \"{S}\"\nto = \"{}\"\n{data}\n[engine]\ncash = 10000.0\nattach_funding = false\n\
         fee = {{ kind = \"free\" }}\n{engine}\n[strategy]\nfile = \"other/nothing.rs\"\n",
        S + days * DAY - 1
    )
}

fn profile(days: i64, data: &str, engine: &str) -> ProfileBacktest {
    let text = toml(days, data, engine);
    let mut p = ProfileBacktest::from_toml_str(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    p.strategy.builder = Some(StrategyBuilder::new(|_| Ok(Box::new(Nothing))));
    p
}

fn usdt(series: &str) -> String {
    format!("conversion = {{ USDT = \"{series}\" }}")
}

/// Every read of the store, counted, over a [`HistoryStoreMemory`].
struct Counting {
    inner: Arc<HistoryStoreMemory>,
    price_reads: AtomicUsize,
    reads: AtomicUsize,
}

impl Counting {
    fn over(inner: Arc<HistoryStoreMemory>) -> Arc<Self> {
        Arc::new(Counting { inner, price_reads: AtomicUsize::new(0), reads: AtomicUsize::new(0) })
    }
}

impl HistoryStore for Counting {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if interval == "30m" {
            self.price_reads.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.load_bars(venue, symbol, interval, range)
    }
    fn load_bars_sourced_capped(
        &self,
        venue: &str,
        source: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.load_bars_sourced_capped(venue, source, symbol, interval, range, budget)
    }
    vike_data::hist_store_stubs!(inert: writes, scan_quotes, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

fn run(p: &ProfileBacktest, store: Arc<dyn HistoryStore + Send + Sync>) -> String {
    run_backtest(p, store).expect_err("refused").to_string()
}

fn counted(store: &Arc<Counting>) -> Arc<dyn HistoryStore + Send + Sync> {
    Arc::clone(store) as Arc<dyn HistoryStore + Send + Sync>
}

/// Every refusal at load, each naming the row: no currency code, the ledger itself, a currency named
/// twice, a series the grammar does not read, a pair that names neither way round. A pair that does
/// loads, on a non-dollar ledger too.
#[test]
fn a_conversion_row_is_refused_at_load_unless_it_reads_one_way_against_the_ledger() {
    for (engine, says) in [
        ("conversion = { \"U$D\" = \"dukascopy:USDJPY:1h\" }", "is not a currency code"),
        ("conversion = { \" \" = \"dukascopy:USDJPY:1h\" }", "is not a currency code"),
        ("conversion = { usd = \"dukascopy:EURUSD:1h\" }", "the ledger's own currency (USD"),
        (
            "account_currency = \"eur\"\nconversion = { EUR = \"dukascopy:EURUSD:1h\" }",
            "the ledger's own currency (EUR",
        ),
        (
            "conversion = { jpy = \"dukascopy:USDJPY:1h\", JPY = \"dukascopy:USDJPY:1h\" }",
            "names JPY twice",
        ),
        ("conversion = { JPY = \"dukascopy:USDJPY\" }", "is not VENUE:SYMBOL:INTERVAL"),
        ("conversion = { JPY = \"dukascopy:USDJPY:fortnight\" }", "not a bar length"),
        ("conversion = { JPY = \"lane:dukascopy:USDJPY:1h\" }", "not a lane the store declares"),
        ("conversion = { JPY = \"dukascopy:EURJPY:1h\" }", "the pair EURJPY is neither JPYUSD"),
        ("conversion = { JPY = \"dukascopy:USDJPYm:1h\" }", "nor USDJPY (a USD priced in JPY)"),
        (
            "account_currency = \"eur\"\nconversion = { USD = \"dukascopy:GBPUSD:1h\" }",
            "neither USDEUR",
        ),
        // One part is the run's own symbol, as an input reads it: BTCUSDT names no USDT/USD pair.
        ("conversion = { USDT = \"1d\" }", "the pair BTCUSDT is neither USDTUSD"),
    ] {
        let err = ProfileBacktest::from_toml_str(&toml(1, "", engine)).unwrap_err().to_string();
        assert!(err.contains("engine.conversion") && err.contains(says), "{engine}: {err}");
    }
    for engine in [
        usdt(PAIR),
        "conversion = { jpy = \"dukascopy:USD/JPY:1h\" }".to_string(),
        "account_currency = \"EUR\"\nconversion = { USD = \"dukascopy:EURUSD:1h\" }".to_string(),
    ] {
        ProfileBacktest::from_toml_str(&toml(1, "", &engine))
            .unwrap_or_else(|e| panic!("{engine}: {e}"));
    }
    let p = profile(
        1,
        "",
        "account_currency = \"EUR\"\nconversion = { USD = \"dukascopy:EURUSD:1h\" }",
    );
    let rows = resolve_conversions(&p).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].quote_currency.as_str(), rows[0].leg),
        ("USD", AccountPerQuoteLeg::AccountIsBase)
    );
    assert_eq!(
        (rows[0].spec(), rows[0].interval_ms),
        ("dukascopy:EURUSD:1h".to_string(), 3_600_000)
    );
}

/// A profile that names no pair reads nothing for one and hands the simulator none.
#[test]
fn a_profile_that_names_no_pair_reads_nothing() {
    let counting = Counting::over(store(1, &[-1, 0]));
    assert_eq!(load_conversions(&profile(1, "", ""), counting.as_ref()).unwrap(), Vec::new());
    assert_eq!(counting.reads.load(Ordering::SeqCst), 0);
}

/// A covered pair is read from one of its bars before the window, oriented by its name, and the run
/// then reads its prices and books through it.
#[test]
fn a_covered_pair_is_read_from_the_bar_before_the_window_and_checked_before_any_price() {
    let p = profile(2, "", &usdt(PAIR));
    let counting = Counting::over(store(2, &[-1, 0, 1]));
    let series = load_conversions(&p, counting.as_ref()).unwrap();
    assert_eq!(series.len(), 1);
    let s = &series[0];
    assert_eq!((s.quote_currency.as_str(), s.pair.as_str()), ("USDT", PAIR));
    assert_eq!((s.leg, s.interval_ms), (AccountPerQuoteLeg::QuoteIsBase, DAY));
    assert_eq!(s.bars.iter().map(|b| b.timestamp).collect::<Vec<_>>(), vec![S - DAY, S, S + DAY]);
    // The first instant's rate is the close of the day before; the day in progress is never read.
    assert_eq!(s.rate_at(S).map(|r| r.rate.get()), Some(1.0 - 1.0 / 1000.0));
    assert_eq!(s.rate_at(S + DAY - 1).map(|r| r.rate.get()), Some(1.0 - 1.0 / 1000.0));

    assert_eq!(counting.price_reads.load(Ordering::SeqCst), 0, "no price was read for the pair");
    run_backtest(&p, counted(&counting)).expect("a covered pair books");
    assert!(counting.price_reads.load(Ordering::SeqCst) > 0, "the run read its prices");
}

/// A missing pair refuses the run by name, with the way to import it, before any price is read.
#[test]
fn a_missing_pair_is_refused_before_any_price_is_read_with_the_way_to_import_it() {
    let counting = Counting::over(store(1, &[-1, 0]));
    let err = run(&profile(1, "", &usdt("user-absent:binance:USDT-USD:1d")), counted(&counting));
    assert!(err.contains("Nothing ran"), "{err}");
    assert!(
        err.contains(
            "conversion pair of USDT \"user-absent:binance:USDT-USD:1d\" (engine.conversion.USDT): \
             the store holds no bar of binance USDT-USD 1d in the lane `user-absent`"
        ),
        "{err}"
    );
    assert!(
        err.contains(
            "import it: Studio → Data → Import CSV, or `vike-cli data hist import csv-bars <FILE> \
             --venue binance --symbol USDT-USD --interval 1d --label absent`"
        ),
        "{err}"
    );
    // A venue's own series is offered the fetch, and the remap names the row.
    let err = run(&profile(1, "", &usdt("kraken:USDT-USD:1d")), counted(&counting));
    assert!(
        err.contains("and point engine.conversion.USDT at `user-<label>:kraken:USDT-USD:1d`"),
        "{err}"
    );
    assert_eq!(counting.price_reads.load(Ordering::SeqCst), 0, "no price was read");
}

/// A hole inside the window is refused and named, the bar before the window included (its close is
/// the first rate); `data.max_gap` under `data.require_coverage` accepts a hole within it.
#[test]
fn a_holed_pair_is_refused_naming_the_hole_the_first_rate_included() {
    let err = run(&profile(3, "", &usdt(PAIR)), store(3, &[-1, 0, 2]));
    assert!(err.contains("does not cover the window for binance USDT-USD 1d in the lane"), "{err}");
    assert!(err.contains("    interior "), "{err}");
    let err = run(&profile(3, "", &usdt(PAIR)), store(3, &[0, 1, 2]));
    assert!(err.contains("    leading "), "the first instant has no closed bar: {err}");
    let tolerant = profile(3, "require_coverage = true\nmax_gap = \"1d\"\n", &usdt(PAIR));
    run_backtest(&tolerant, store(3, &[-1, 0, 2])).expect("a hole within max_gap runs");
}

/// A bar whose close is no price is no rate, and the pair is refused naming it.
#[test]
fn a_close_that_is_no_price_is_refused() {
    let s = HistoryStoreMemory::new();
    let lane = [bar(S - DAY, 0.0), bar(S, 1.0)];
    s.append_bars_sourced("binance", "user-mine", "USDT-USD", "1d", &lane, None).unwrap();
    let err = run(&profile(1, "", &usdt(PAIR)), Arc::new(s));
    assert!(err.contains("closes at 0, which is no price and so no rate"), "{err}");
}

/// `data.explain` lists each pair as a `conversion` row carrying its quote currency (and its lane),
/// judged from one of its bars before the window; a plan that converts nothing carries no such field.
#[test]
fn data_explain_lists_each_pair_by_its_quote_currency() {
    let held = store(1, &[-1, 0]);
    let lane = plan_data(&profile(1, "", &usdt(PAIR)), &[], held.as_ref(), "test store", None);
    let lines = lane.unwrap().lines();
    let row = "interval=1d [conversion] source=user-mine converts=USDT";
    assert!(lines.iter().any(|l| l.contains(row)), "{lines:#?}");

    let p = profile(1, "", &usdt("coinbase:USDT-USD:1d"));
    let plan = plan_data(&p, &[], held.as_ref(), "test store", None).unwrap();
    let rows: Vec<_> =
        plan.series.iter().filter(|s| s.role == DataPlanSeriesRole::Conversion).collect();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].conversion.as_deref(), rows[0].id.source.as_deref()), (Some("USDT"), None));
    assert!(rows[0].held() && rows[0].missing.is_empty(), "{:?}", rows[0]);
    let lines = plan.lines();
    let row = "venue=coinbase symbol=USDT-USD interval=1d [conversion] converts=USDT";
    assert!(lines.iter().any(|l| l.contains(row)), "{lines:#?}");
    let doc = explain_document(&p, &plan).unwrap();
    let series = doc["series"].as_array().unwrap();
    assert!(series.iter().any(|s| s["role"] == "conversion" && s["conversion"] == "USDT"), "{doc}");

    // The day before the window is planned too: without it the row is short at the leading end.
    let short = plan_data(&p, &[], store(1, &[0]).as_ref(), "test store", None).unwrap();
    let row = short.series.iter().find(|s| s.role == DataPlanSeriesRole::Conversion).unwrap();
    assert_eq!(row.missing.first().map(|m| m.kind), Some(SeriesShortfall::Leading), "{row:?}");

    let bare = plan_data(&profile(1, "", ""), &[], held.as_ref(), "test store", None).unwrap();
    assert!(!serde_json::to_string(&bare).unwrap().contains("\"conversion\""));
}

/// A door handed no pair refuses a profile that names one, naming the door; none passes.
#[test]
fn a_door_that_loads_no_pair_refuses_a_profile_that_names_one() {
    let p = profile(1, "", &usdt(PAIR));
    let err =
        refuse_unplanned_conversions(&p.engine.conversion, "a run over held bars").unwrap_err();
    let err = err.to_string();
    assert!(
        err.contains("a run over held bars does not load the conversion pairs")
            && err.contains("(USDT)")
    );
    refuse_unplanned_conversions(&profile(1, "", "").engine.conversion, "a run over held bars")
        .unwrap();
}

/// THE rule for which named quote currency a symbol is in (part B, `quoted_in`): the fiat quote its name
/// states, else a named code its name ends in; never a currency the profile did not name.
#[test]
fn a_symbol_is_quoted_in_the_named_currency_its_name_states_or_ends_in() {
    let named = |codes: &[&str]| codes.iter().map(|c| c.to_string()).collect::<Vec<String>>();
    let (yen, usdt) = (named(&["JPY", "USDT"]), named(&["USDT"]));
    for (symbol, want) in [
        ("USDJPY", Some("JPY")),
        ("EUR_JPY", Some("JPY")),
        ("CS.D.GBPJPY.CFD.IP", Some("JPY")),
        ("BTCUSDT", Some("USDT")),
        ("BTC-USDT", Some("USDT")),
        ("BTCUSDT.P", Some("USDT")),
        ("ETH/USDT", Some("USDT")),
        ("EURUSDT", Some("USDT")),
        ("EURUSD", None),
        ("BTCUSDC", None),
        ("USDT", None),
    ] {
        assert_eq!(quoted_in(symbol, &yen), want, "{symbol}");
    }
    assert_eq!(quoted_in("USDJPY", &usdt), None, "a fiat quote nobody named is not guessed");
    assert_eq!(quoted_in("BTCUSDT", &[]), None);
}

/// BTCUSDT 30-minute bars over two days, at 100 on the first and 110 on the second, EURJPY flat at 160
/// beside it, a USDT-USD lane closing 0.99, 0.98 and 0.97 on days -1, 0 and 1, and a USDJPY lane at 150.
fn booked_store() -> Arc<dyn HistoryStore + Send + Sync> {
    let s = HistoryStoreMemory::new();
    for (symbol, bars) in booked_bars() {
        s.append_bars("binance", &symbol, "30m", &bars, None).unwrap();
    }
    let usdt = [bar(S - DAY, 0.99), bar(S, 0.98), bar(S + DAY, 0.97)];
    s.append_bars_sourced("binance", "user-mine", "USDT-USD", "1d", &usdt, None).unwrap();
    let yen = [bar(S - DAY, 150.0), bar(S, 150.0), bar(S + DAY, 150.0)];
    s.append_bars_sourced("binance", "user-mine", "USDJPY", "1d", &yen, None).unwrap();
    Arc::new(s)
}

/// [`booked_store`]'s price bars, by symbol.
fn booked_bars() -> Vec<(String, Vec<Bar>)> {
    let btc: Vec<Bar> =
        (0..96).map(|i| bar(S + i * HALF, if i < 48 { 100.0 } else { 110.0 })).collect();
    let eur: Vec<Bar> = (0..96).map(|i| bar(S + i * HALF, 160.0)).collect();
    vec![("BTCUSDT".to_string(), btc), ("EURJPY".to_string(), eur)]
}

/// `buy_hold` of 10 BTCUSDT over [`booked_store`]'s two days, on the dollar ledger, BTCUSDT and EURJPY
/// in one run; `engine` goes inside `[engine]`.
fn buy_hold(engine: &str) -> ProfileBacktest {
    let text = toml(2, "", engine)
        .replace("symbols = [\"BTCUSDT\"]", "symbols = [\"BTCUSDT\", \"EURJPY\"]")
        .replace(
            "file = \"other/nothing.rs\"\n",
            "file = \"trend/buy_hold.rs\"\n[strategy.params]\nsize = 10.0\nsymbol = \"BTCUSDT\"\n",
        );
    ProfileBacktest::from_toml_str(&text)
        .unwrap_or_else(|e| panic!("{e}\n{text}"))
        .with_stand_in(vike_strategies_check::stand_in)
}

/// ⚠ **THE BOOKING, end to end, over a MIXED-quote run** (decision 0243 phase 2). 10 BTCUSDT bought at
/// 100 USDT while USDT-USD stood at 0.99 and marked at 110 USDT while it stands at 0.98: the 100 USDT of
/// PnL is 98 dollars at the rate now, where a run naming no USDT pair books 100 (the stablecoin read as
/// the dollar, as every run before the table). EURJPY, quoted in yen, shares the run through its own
/// pair, the "split the run" refusal it met before; without that pair it is refused and told to name one.
#[test]
fn a_mixed_quote_run_books_each_quote_through_its_pair_in_dollars() {
    let pairs = "conversion = { USDT = \"user-mine:binance:USDT-USD:1d\", \
                 JPY = \"user-mine:binance:USDJPY:1d\" }";
    let booked = run_backtest(&buy_hold(pairs), booked_store()).expect("both quotes have a pair");
    assert!(
        (booked.final_equity - (10_000.0 + 10.0 * 10.0 * 0.98)).abs() < 1e-9,
        "100 USDT of PnL at 0.98: {}",
        booked.final_equity
    );

    let yen_only = "conversion = { JPY = \"user-mine:binance:USDJPY:1d\" }";
    let at_par = run_backtest(&buy_hold(yen_only), booked_store()).expect("USDT at par runs");
    assert!(
        (at_par.final_equity - 10_100.0).abs() < 1e-9,
        "no USDT pair: USDT is the ledger's unit: {}",
        at_par.final_equity
    );

    let err = run(&buy_hold(""), booked_store());
    assert!(err.contains("EURJPY (JPY)") && err.contains("engine.conversion.JPY"), "{err}");

    // A row no symbol of the run is quoted in would convert nothing: refused, naming it.
    let idle = "conversion = { USDT = \"user-mine:binance:USDT-USD:1d\", \
                JPY = \"user-mine:binance:USDJPY:1d\", GBP = \"user-mine:binance:GBPUSD:1d\" }";
    let err = run(&buy_hold(idle), booked_store());
    assert!(
        err.contains("names a pair for GBP, and no symbol of this run is quoted in it"),
        "{err}"
    );
}

/// A cash purchase of a foreign stock stays refused by name (decision 0243, item 4), before anything is
/// read; on margin the same symbol goes on to its pair (here: missing from the store).
#[test]
fn a_cash_purchase_of_a_foreign_stock_is_refused_by_name() {
    let text = |mode: &str| {
        format!(
            "[data]\nkind = \"bar\"\nvenue = \"ibkr\"\nsymbols = [\"SAP.SMART.EUR\"]\ninterval = \
             \"1d\"\nfrom = \"{S}\"\nto = \"{}\"\n[engine]\ncash = 10000.0\nmargin_mode = \"{mode}\"\n\
             fee = {{ kind = \"free\" }}\nconversion = {{ EUR = \"ibkr:EURUSD:1d\" }}\n[strategy]\n\
             file = \"other/nothing.rs\"\n",
            S + DAY
        )
    };
    let profile_of = |mode: &str| {
        let mut p = ProfileBacktest::from_toml_str(&text(mode)).unwrap();
        p.strategy.builder = Some(StrategyBuilder::new(|_| Ok(Box::new(Nothing))));
        p
    };
    let counting = Counting::over(Arc::new(HistoryStoreMemory::new()));
    let err = run(&profile_of("cash"), counted(&counting));
    assert!(err.contains("SAP.SMART.EUR on ibkr is bought for cash and quoted in EUR"), "{err}");
    assert!(err.contains("cash purchase of a foreign stock"), "{err}");
    assert_eq!(counting.reads.load(Ordering::SeqCst), 0, "refused before anything was read");

    let err = run(&profile_of("cross"), counted(&counting));
    assert!(!err.contains("cash purchase"), "on margin it converts: {err}");
}

/// ⚠ **A WALK-FORWARD books each window through the pair** (decision 0243 phase 2): the pair is read
/// and judged once over the whole walk, each window's fresh engine is handed it, and so each window's
/// money is at the rate in force in that window by the run's own rule. BTCUSDT rises one USDT a bar
/// over four days and USDT-USD closes 0.99, 0.98, 0.97, 0.96 and 0.95 on days -1 to 3: `buy_hold` of 10
/// in a window earns its USDT gain at the rate in force at the window's LAST bar (the open position is
/// marked at the rate now), so its booked return is the same walk's at-par return times that rate,
/// window by window, and the rates differ between windows.
#[test]
fn a_walk_forward_books_each_window_at_the_pairs_rate() {
    let days = 4;
    let s = HistoryStoreMemory::new();
    let btc: Vec<Bar> = (0..days * 48).map(|i| bar(S + i * HALF, 100.0 + i as f64)).collect();
    s.append_bars("binance", "BTCUSDT", "30m", &btc, None).unwrap();
    let lane: Vec<Bar> = (-1..days).map(|d| bar(S + d * DAY, 0.98 - 0.01 * d as f64)).collect();
    s.append_bars_sourced("binance", "user-mine", "USDT-USD", "1d", &lane, None).unwrap();
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(s);
    let walk = |engine: &str| {
        let text = toml(days, "", engine).replace(
            "file = \"other/nothing.rs\"\n",
            "file = \"trend/buy_hold.rs\"\n[strategy.params]\nsize = 10.0\nsymbol = \"BTCUSDT\"\n",
        ) + "\n[walkforward]\nn_splits = 3\n";
        ProfileBacktest::from_toml_str(&text)
            .unwrap_or_else(|e| panic!("{e}\n{text}"))
            .with_stand_in(vike_strategies_check::stand_in)
    };
    let run_walk = crate::walkforward::runner::run_walkforward;
    let at_par = run_walk(&walk(""), Arc::clone(&store)).expect("no USDT row: USDT at par");
    let booked = run_walk(&walk(&usdt(PAIR)), Arc::clone(&store)).expect("the pair books");
    // The pair as the run reads it: the rate at a window's last bar is its `rate_at` there.
    let pair = load_conversions(&walk(&usdt(PAIR)), store.as_ref()).unwrap().remove(0);
    assert_eq!(at_par.windows.len(), booked.windows.len());
    let mut rates: Vec<u64> = Vec::new();
    for (par, conv) in at_par.windows.iter().zip(&booked.windows) {
        assert_eq!(par.test_range, conv.test_range);
        let last_timestamp = S + (par.test_range.1 as i64 - 1) * HALF;
        let rate = pair.rate_at(last_timestamp).expect("a rate in force").rate.get();
        assert!(par.out_of_sample_return > 0.0, "the window must earn something: {par:?}");
        assert!(
            (conv.out_of_sample_return - par.out_of_sample_return * rate).abs()
                <= 1e-9 * par.out_of_sample_return,
            "window {:?}: booked {} where the at-par {} x the rate {rate} is {}",
            par.test_range,
            conv.out_of_sample_return,
            par.out_of_sample_return,
            par.out_of_sample_return * rate
        );
        rates.push(rate.to_bits());
    }
    rates.dedup();
    assert!(rates.len() >= 2, "the windows must meet different rates: {rates:?}");
}

/// ⚠ **A run over HELD bars books through the pairs its caller loaded** (the door a walk-forward window
/// and a search over held bars run through): over [`booked_store`]'s bars, with the pairs loaded and
/// booked as `run_backtest` does, 100 USDT of PnL books 98 dollars at USDT-USD 0.98 (100 at par with
/// only the yen row); handed no pair, a profile naming one is refused, never run at par.
#[test]
fn a_run_over_held_bars_books_through_the_pairs_its_caller_loaded() {
    let store = booked_store();
    let over = |p: &ProfileBacktest, pairs: &[vike_simulator::ConversionSeries]| {
        crate::harness::run::run_backtest_over_bars(
            p,
            &store,
            booked_bars(),
            &[],
            pairs,
            &Arc::default(),
        )
    };
    let loaded = |p: &ProfileBacktest| {
        let mut pairs = load_conversions(p, store.as_ref()).unwrap();
        book_conversions(p, &mut pairs, false).unwrap();
        pairs
    };
    let both = buy_hold(
        "conversion = { USDT = \"user-mine:binance:USDT-USD:1d\", \
         JPY = \"user-mine:binance:USDJPY:1d\" }",
    );
    let yen_only = buy_hold("conversion = { JPY = \"user-mine:binance:USDJPY:1d\" }");
    let booked = over(&both, &loaded(&both)).expect("both quotes have a pair");
    let at_par = over(&yen_only, &loaded(&yen_only)).expect("USDT at par");
    assert!((at_par.final_equity - 10_100.0).abs() < 1e-9, "{}", at_par.final_equity);
    assert!(
        (booked.final_equity - (10_000.0 + 100.0 * 0.98)).abs() < 1e-9,
        "100 USDT of PnL at 0.98: {}",
        booked.final_equity
    );
    let err = over(&both, &[]).expect_err("a pair named and none handed").to_string();
    assert!(err.contains("does not load the conversion pairs") && err.contains("USDT"), "{err}");
}
