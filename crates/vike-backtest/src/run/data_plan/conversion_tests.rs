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
use crate::run::data_plan::{DataPlanSeriesRole, explain_document, plan_data};
use crate::run::run_backtest;
use crate::run::strategy_builder::StrategyBuilder;

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
    // A USDT priced in euros on the same days: a USDT quote on a EURO ledger is refused for a missing rate as
    // every pair but USDT on the dollar ledger is (decision 0243, amended 2026-11-16).
    s.append_bars_sourced("binance", "user-mine", "USDT-EUR", "1d", &lane, None).unwrap();
    s.append_bars("coinbase", "USDT-EUR", "1d", &lane, None).unwrap();
    Arc::new(s)
}

/// `days` days of the run's 30-minute bars from `S`, and no rate series at all.
fn bare_store(days: i64) -> Arc<HistoryStoreMemory> {
    let s = HistoryStoreMemory::new();
    let price: Vec<Bar> = (0..days * 48).map(|i| bar(S + i * HALF, 100.0)).collect();
    s.append_bars("binance", "BTCUSDT", "30m", &price, None).unwrap();
    Arc::new(s)
}

/// A bar run of BTCUSDT over `days` days; `engine` goes inside `[engine]`, `data` inside `[data]`.
fn toml(days: i64, data: &str, engine: &str) -> String {
    format!(
        "[data]\nkind = \"bar\"\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\ninterval = \"30m\"\n\
         from = \"{S}\"\nto = \"{}\"\n{data}\n[engine]\ncash = 10000.0\nattach_funding = false\n\
         {engine}\n[strategy]\nfile = \"other/nothing.rs\"\n",
        S + days * DAY - 1
    )
}

fn profile(days: i64, data: &str, engine: &str) -> ProfileBacktest {
    let text = toml(days, data, engine);
    let mut p = ProfileBacktest::from_toml_str(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    p.strategy.as_mut().unwrap().builder = Some(StrategyBuilder::new(|_| Ok(Box::new(Nothing))));
    p
}

fn usdt(series: &str) -> String {
    format!("conversion = {{ USDT = \"{series}\" }}")
}

/// The fixture's USDT-EUR lane.
const PAIR_EUR: &str = "user-mine:binance:USDT-EUR:1d";

/// A euro ledger converting USDT at `series`.
fn eur(series: &str) -> String {
    format!("account_currency = \"EUR\"\nconversion = {{ USDT = \"{series}\" }}")
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

/// A profile that names no pair and needs none (a euro ledger here) reads nothing for one and hands the
/// simulator none; on the dollar ledger with a symbol quoted in USDT, the store's own USDT-USD series is
/// found with no setting, the operator's import first (decision 0243, amended 2026-11-16).
#[test]
fn a_profile_that_names_no_pair_reads_nothing_but_the_usdt_rate_it_finds() {
    let counting = Counting::over(store(1, &[-1, 0]));
    let euro = profile(1, "", "account_currency = \"EUR\"");
    assert_eq!(load_conversions(&euro, counting.as_ref()).unwrap(), Vec::new());
    assert_eq!(counting.reads.load(Ordering::SeqCst), 0);
    let found = load_conversions(&profile(1, "", ""), store(1, &[-1, 0]).as_ref()).unwrap();
    let found: Vec<(&str, &str, bool)> = found
        .iter()
        .map(|c| (c.quote_currency.as_str(), c.pair.as_str(), c.par_where_missing))
        .collect();
    assert_eq!(found, [("USDT", PAIR, true)], "the user lane before coinbase's own series");
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

/// **A pair is read no further than its last bar the run can see** (2026-11-21: a perpetual's USDT pair read one run
/// bar past a window ending yesterday reached today, and its run was never answered from saved): the day after the
/// window is neither named ahead nor read, and every rate the run asks, to its last bar's close, is the one the
/// series read one run bar further (as before) answers, so the run's numbers are unchanged.
#[test]
fn a_pair_is_read_no_further_than_its_last_bar_the_run_can_see() {
    let p = profile(2, "", &usdt(PAIR));
    let end = S + 2 * DAY - 1;
    let planned = conversion_reads(&p);
    let [PlannedRead::BarsSourcedCapped { range, .. }] = &planned[..] else {
        panic!("the one pair, named ahead: {planned:?}");
    };
    assert_eq!(range.end, Some(end), "named ahead to the window's end");
    let lane = store(2, &[-1, 0, 1, 2]);
    let read = load_conversions(&p, lane.as_ref()).unwrap();
    let days: Vec<i64> = read[0].bars.iter().map(|b| b.timestamp).collect();
    assert_eq!(days, vec![S - DAY, S, S + DAY], "the day after the window is not read");
    let before = TimestampRange { start: Some(S - DAY), end: Some(end + HALF) };
    let held =
        lane.load_bars_sourced_capped("binance", "user-mine", "USDT-USD", "1d", before, None);
    let padded = ConversionSeries { bars: Arc::new(held.unwrap()), ..read[0].clone() };
    assert_eq!(padded.bars.len(), 4, "the read before held the day after the window");
    for t in (S - DAY..=end + 1 + HALF).step_by(HALF as usize) {
        assert_eq!(read[0].rate_at(t), padded.rate_at(t), "the rate at {t}");
    }
}

/// A missing pair refuses the run by name, with the way to import it, before any price is read.
#[test]
fn a_missing_pair_is_refused_before_any_price_is_read_with_the_way_to_import_it() {
    let counting = Counting::over(store(1, &[-1, 0]));
    let err = run(&profile(1, "", &eur("user-absent:binance:USDT-EUR:1d")), counted(&counting));
    assert!(err.contains("Nothing ran"), "{err}");
    assert!(
        err.contains(
            "conversion pair of USDT \"user-absent:binance:USDT-EUR:1d\" (engine.conversion.USDT): \
             the store holds no bar of binance USDT-EUR 1d in the lane `user-absent`"
        ),
        "{err}"
    );
    assert!(
        err.contains(
            "import it: Studio → Data → Import CSV, or `vike-cli data history import csv-bars <FILE> \
             --venue binance --symbol USDT-EUR --interval 1d --lane user-absent`"
        ),
        "{err}"
    );
    // A venue's own series is offered the fetch, and the remap names the row.
    let err = run(&profile(1, "", &eur("kraken:USDT-EUR:1d")), counted(&counting));
    assert!(
        err.contains("and point engine.conversion.USDT at `user-<label>:kraken:USDT-EUR:1d`"),
        "{err}"
    );
    assert_eq!(counting.price_reads.load(Ordering::SeqCst), 0, "no price was read");
}

/// The day of `ms`, as the report writes it.
fn day(ms: i64) -> String {
    vike_model::time::epoch_ms_to_utc_timestamp(ms)[..10].to_string()
}

/// The report's line listing the days a run booked one-to-one.
fn days_line(result: &vike_analytics::TradingRecord) -> String {
    result
        .data_warnings
        .iter()
        .find(|l| l.contains("booked 1:1:"))
        .cloned()
        .unwrap_or_else(|| panic!("{:#?}", result.data_warnings))
}

/// With NO USDT-USD series in the store (the normal user), a run on the dollar ledger with a symbol quoted in
/// USDT books the whole run one-to-one, converting nothing, and its report says so in one line (decision
/// 0243, amended 2026-11-16). Another ledger is refused as before.
#[test]
fn with_no_usdt_usd_series_a_dollar_run_books_one_to_one_and_says_so() {
    let p = profile(2, "", "");
    assert_eq!(load_conversions(&p, bare_store(2).as_ref()).unwrap(), Vec::new());
    let result = run_backtest(&p, bare_store(2)).expect("no rate series: the run is not refused");
    assert!(
        result.data_warnings.iter().any(|l| l == USDT_USD_NOT_FOUND),
        "{:#?}",
        result.data_warnings
    );
    let euro = run(&profile(2, "", "account_currency = \"EUR\""), bare_store(2));
    assert!(!euro.contains(USDT_USD_NOT_FOUND), "another ledger is refused as before: {euro}");
}

/// A USDT pair on the dollar ledger books every day it has no rate one-to-one and lists those days: after the
/// series ends, in a hole, before it starts; the days it has a rate are its closes, unchanged. Named or found.
#[test]
fn a_day_with_no_usdt_rate_is_booked_one_to_one_and_listed() {
    // After the series ends: day 2's rate would be day 1's close, which the store does not hold.
    let ended =
        load_conversions(&profile(3, "", &usdt(PAIR)), store(3, &[-1, 0]).as_ref()).unwrap();
    let rate = |t| ended[0].rate_at(t).expect("never none");
    assert_eq!((rate(S + DAY).rate.get(), rate(S + DAY).par), (1.0, false), "day 0's close");
    assert_eq!((rate(S + 2 * DAY).rate.get(), rate(S + 2 * DAY).par), (1.0, true));
    let result = run_backtest(&profile(3, "", &usdt(PAIR)), store(3, &[-1, 0])).expect("runs");
    let line = days_line(&result);
    let want = format!("no rate on 1 day(s) of this run, booked 1:1: {}", day(S + 2 * DAY));
    assert!(line.contains(&want), "{line}");
    // Found with no setting, the same.
    let found = run_backtest(&profile(3, "", ""), store(3, &[-1, 0])).expect("runs");
    assert_eq!(days_line(&found), line);
    // A hole (day 0's bar) and the days before the series starts (day 1's bar is its first).
    let holed =
        run_backtest(&profile(3, "", &usdt(PAIR)), store(3, &[-1, 1])).expect("a hole runs");
    let want = format!("1 day(s) of this run, booked 1:1: {}", day(S + DAY));
    assert!(days_line(&holed).contains(&want), "{}", days_line(&holed));
    let later = run_backtest(&profile(3, "", &usdt(PAIR)), store(3, &[1, 2])).expect("runs");
    let want = format!("2 day(s) of this run, booked 1:1: {} to {}", day(S), day(S + DAY));
    assert!(days_line(&later).contains(&want), "{}", days_line(&later));
    // A series with a rate every day of the run lists nothing.
    let covered = run_backtest(&profile(2, "", &usdt(PAIR)), store(2, &[-1, 0, 1])).expect("runs");
    assert!(
        covered.data_warnings.iter().all(|l| !l.contains("1:1")),
        "{:#?}",
        covered.data_warnings
    );
}

/// Only a USDT quote on the dollar ledger is ever one-to-one: the rule's whole table, and the flag a
/// resolved row carries.
#[test]
fn only_a_usdt_quote_on_the_dollar_ledger_is_one_to_one() {
    assert!(converts_one_to_one_where_missing("USDT", "USD"));
    for (quote, ledger) in [("USDC", "USD"), ("JPY", "USD"), ("USDT", "EUR"), ("USD", "USDT")] {
        assert!(!converts_one_to_one_where_missing(quote, ledger), "{quote} on {ledger}");
    }
    let rows =
        "conversion = { USDT = \"user-mine:binance:USDT-USD:1d\", JPY = \"binance:USDJPY:1d\" }";
    let resolved = resolve_conversions(&profile(1, "", rows)).unwrap();
    let flags: Vec<(&str, bool)> =
        resolved.iter().map(|c| (c.quote_currency.as_str(), c.par_where_missing)).collect();
    assert_eq!(flags, [("JPY", false), ("USDT", true)]);
}

/// A hole inside the window is refused and named, the bar before the window included (its close is
/// the first rate); `data.max_gap` under `data.require_coverage` accepts a hole within it.
#[test]
fn a_holed_pair_is_refused_naming_the_hole_the_first_rate_included() {
    // Every pair but USDT on the dollar ledger: a USDT on a euro ledger here.
    let err = run(&profile(3, "", &eur(PAIR_EUR)), store(3, &[-1, 0, 2]));
    assert!(err.contains("does not cover the window for binance USDT-EUR 1d in the lane"), "{err}");
    assert!(err.contains("    interior "), "{err}");
    let err = run(&profile(3, "", &eur(PAIR_EUR)), store(3, &[0, 1, 2]));
    assert!(err.contains("    leading "), "the first instant has no closed bar: {err}");
    let tolerant = profile(3, "require_coverage = true\nmax_gap = \"1d\"\n", &eur(PAIR_EUR));
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

    // The day before the window is planned too: without it the row is short at the leading end (the plan says
    // what the store holds; a USDT pair on the dollar ledger books that day one-to-one when it runs).
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
    let none = crate::run::KeptRows::default();
    for (venue, symbol, want) in [
        ("oanda", "USDJPY", Some("JPY")),
        ("oanda", "EUR_JPY", Some("JPY")),
        ("ig", "CS.D.GBPJPY.CFD.IP", Some("JPY")),
        ("binance", "BTCUSDT", Some("USDT")),
        ("okx", "BTC-USDT", Some("USDT")),
        ("binance", "BTCUSDT.P", Some("USDT")),
        ("hyperliquid", "ETH/USDT", Some("USDT")),
        ("binance", "EURUSDT", Some("USDT")),
        ("oanda", "EURUSD", None),
        ("binance", "BTCUSDC", None),
        ("binance", "USDT", None),
    ] {
        assert_eq!(quoted_in(&none, venue, symbol, &yen), want, "{symbol}");
    }
    assert_eq!(
        quoted_in(&none, "oanda", "USDJPY", &usdt),
        None,
        "a fiat quote nobody named is not guessed"
    );
    assert_eq!(quoted_in(&none, "binance", "BTCUSDT", &[]), None);
}

/// **IBKR-1 and IG-1, the conversion half: the pair books what the ledger guard reads.** An ibkr symbol is quoted
/// in its KEPT row's currency (decision 0290's O-10: `USDJPY`'s row `USD.JPY.IDEALPRO` is JPY, so
/// `engine.conversion.JPY` books it, where the name reader said nothing and the row was refused as idle), and with no
/// row its user name states none, so no pair books it; a KEPT row's quote decides (binance's `IDTRY` is lira); a
/// broker's symbol of no known currency is booked by no pair, whatever its name ends in.
#[test]
fn a_pair_books_the_currency_the_ledger_guard_reads() {
    let quotes = ["JPY".to_string(), "EUR".to_string(), "TRY".to_string(), "USDT".to_string()];
    let none = crate::run::KeptRows::default();
    let ibkr_rows = kept_rows();
    let series: Vec<(String, String)> =
        ["USDJPY", "SAP", "EURUSD"].iter().map(|s| ("ibkr".to_string(), s.to_string())).collect();
    let ibkr = crate::run::KeptRows::of(&ibkr_rows, &series);
    for (symbol, want) in [("USDJPY", Some("JPY")), ("SAP", Some("EUR")), ("EURUSD", None)] {
        assert_eq!(quoted_in(&ibkr, "ibkr", symbol, &quotes), want, "{symbol}");
    }
    for symbol in ["USDJPY", "AAPL", "USD.JPY.IDEALPRO"] {
        assert_eq!(
            quoted_in(&none, "ibkr", symbol, &quotes),
            None,
            "{symbol}: no row, no currency"
        );
    }
    assert_eq!(quoted_in(&none, "ig", "IX.D.DAX.IFD.IP", &quotes), None, "unknown: no pair");
    assert_eq!(quoted_in(&none, "oanda", "BTCUSDT", &quotes), None, "a broker's unknown currency");
    let row = vike_model::Instrument::new(
        "binance",
        "IDTRY",
        "ID",
        "TRY",
        vike_model::Contract::CryptoSpot,
    );
    let kept = crate::run::KeptRows::of(
        &vike_catalog::spelled(vec![row]),
        &[("binance".into(), "IDTRY".into())],
    );
    assert_eq!(quoted_in(&kept, "binance", "IDTRY", &quotes), Some("TRY"), "its row's quote");
    assert_eq!(quoted_in(&none, "binance", "IDTRY", &quotes), Some("TRY"), "its name ends in TRY");
    // A profile on an ibkr yen pair that names the JPY pair: the row books it, never refused as idle.
    let text = format!(
        "[data]\nkind = \"bar\"\nvenue = \"ibkr\"\nsymbols = [\"USDJPY\"]\ninterval = \"30m\"\n\
         from = \"{S}\"\nto = \"{}\"\n[engine]\ncash = 10000.0\nattach_funding = false\n\
         margin_mode = \"cross\"\n\
         conversion = {{ JPY = \"dukascopy:USDJPY:1h\" }}\n[strategy]\nfile = \"other/nothing.rs\"\n",
        S + DAY - 1
    );
    let mut p = ProfileBacktest::from_toml_str(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    p.kept = crate::run::KeptRows::of(&ibkr_rows, &series);
    assert_eq!(conversion_quotes(&p).unwrap(), ["JPY"]);
}

/// The kept rows these runs read, named as the datahub's write stores them (`vike_catalog::spelled`): ibkr's yen pair
/// `USDJPY`, its euro share `SAP` and its `EURUSD`, and dukascopy's `USDJPY`, a conversion pair's venue (decision 0290:
/// a profile's and a pair's names are read against the lists).
fn kept_rows() -> Vec<vike_model::Instrument> {
    use vike_model::{Contract, ContractEquity, ContractFx, Instrument};
    let fx = || Contract::Fx(ContractFx::default());
    let sap = Contract::Equity(ContractEquity { exchange: None, currency: Some("EUR".into()) });
    vike_catalog::spelled(vec![
        Instrument::new("ibkr", "USD.JPY.IDEALPRO", "USD", "JPY", fx()),
        Instrument::new("ibkr", "SAP.IBIS.EUR.STK", "SAP", "EUR", sap),
        Instrument::new("ibkr", "EUR.IDEALPRO.USD", "EUR", "USD", fx()),
        Instrument::new("dukascopy", "USDJPY", "USD", "JPY", fx()),
    ])
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
        .with_stand_in(vike_strategy_lab::stand_in)
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

    // No USDT row: the store's USDT-USD series is found with no setting and books the same (decision 0243,
    // amended 2026-11-16).
    let yen_only = "conversion = { JPY = \"user-mine:binance:USDJPY:1d\" }";
    let found = run_backtest(&buy_hold(yen_only), booked_store()).expect("the USDT rate is found");
    assert_eq!(
        found.final_equity.to_bits(),
        booked.final_equity.to_bits(),
        "{}",
        found.final_equity
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
/// read; on margin the same symbol goes on to its pair (here: missing from the store). Its currency is its kept row's
/// (decision 0290's O-10: an ibkr user name states none).
#[test]
fn a_cash_purchase_of_a_foreign_stock_is_refused_by_name() {
    let text = |mode: &str| {
        format!(
            "[data]\nkind = \"bar\"\nvenue = \"ibkr\"\nsymbols = [\"SAP\"]\ninterval = \
             \"1d\"\nfrom = \"{S}\"\nto = \"{}\"\n[engine]\ncash = 10000.0\nmargin_mode = \"{mode}\"\n\
             conversion = {{ EUR = \"ibkr:EURUSD:1d\" }}\n[strategy]\n\
             file = \"other/nothing.rs\"\n",
            S + DAY
        )
    };
    let profile_of = |mode: &str| {
        let mut p = ProfileBacktest::from_toml_str(&text(mode)).unwrap();
        p.strategy.as_mut().unwrap().builder =
            Some(StrategyBuilder::new(|_| Ok(Box::new(Nothing))));
        p.kept = crate::run::KeptRows::of(&kept_rows(), &[("ibkr".into(), "SAP".into())]);
        p
    };
    let counting = Counting::over(Arc::new(HistoryStoreMemory::new()));
    let err = run(&profile_of("cash"), counted(&counting));
    assert!(err.contains("SAP on ibkr is bought for cash and quoted in EUR"), "{err}");
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
    // At par: the same prices and no USDT-USD series at all (with one, a run naming none finds it).
    let bare = HistoryStoreMemory::new();
    bare.append_bars("binance", "BTCUSDT", "30m", &btc, None).unwrap();
    let bare: Arc<dyn HistoryStore + Send + Sync> = Arc::new(bare);
    let walk = |engine: &str| {
        let text = toml(days, "", engine).replace(
            "file = \"other/nothing.rs\"\n",
            "file = \"trend/buy_hold.rs\"\n[strategy.params]\nsize = 10.0\nsymbol = \"BTCUSDT\"\n",
        ) + "\n[walkforward]\nn_splits = 3\n";
        ProfileBacktest::from_toml_str(&text)
            .unwrap_or_else(|e| panic!("{e}\n{text}"))
            .with_stand_in(vike_strategy_lab::stand_in)
    };
    let run_walk = crate::walkforward::runner::run_walkforward;
    let at_par = run_walk(&walk(""), bare).expect("no USDT-USD series: USDT at par");
    let booked = run_walk(&walk(&usdt(PAIR)), Arc::clone(&store)).expect("the pair books");
    // No row, the series in the store: found with no setting, and booked the same (decision 0243, 2026-11-16).
    let found = run_walk(&walk(""), Arc::clone(&store)).expect("the pair is found");
    let returns = |r: &vike_analytics::walkforward::WalkforwardResults| {
        r.steps.iter().map(|w| w.results.total_return.to_bits()).collect::<Vec<_>>()
    };
    assert_eq!(returns(&found), returns(&booked));
    // The pair as the run reads it: the rate at a window's last bar is its `rate_at` there.
    let pair = load_conversions(&walk(&usdt(PAIR)), store.as_ref()).unwrap().remove(0);
    assert_eq!(at_par.steps.len(), booked.steps.len());
    let mut rates: Vec<u64> = Vec::new();
    for (par, conv) in at_par.steps.iter().zip(&booked.steps) {
        assert_eq!(par.test_range, conv.test_range);
        let last_timestamp = S + (par.test_range.1 as i64 - 1) * HALF;
        let rate = pair.rate_at(last_timestamp).expect("a rate in force").rate.get();
        let (par_return, conv_return) = (par.results.total_return, conv.results.total_return);
        assert!(par_return > 0.0, "the window must earn something: {par:?}");
        assert!(
            (conv_return - par_return * rate).abs() <= 1e-9 * par_return,
            "window {:?}: booked {conv_return} where the at-par {par_return} x the rate {rate} is {}",
            par.test_range,
            par_return * rate
        );
        rates.push(rate.to_bits());
    }
    rates.dedup();
    assert!(rates.len() >= 2, "the windows must meet different rates: {rates:?}");
    for (report, profile) in [(&at_par, walk("")), (&booked, walk(&usdt(PAIR))), (&found, walk(""))]
    {
        crate::walkforward::walkforward_tests::assert_bit_equal_to_the_fold_before_protocol_nine(
            report,
            profile.engine.cash,
            crate::run::report::periods_per_year(&profile),
        );
    }
}

/// ⚠ **A run over HELD bars books through the pairs its caller loaded** (the door a walk-forward window
/// and a search over held bars run through): over [`booked_store`]'s bars, with the pairs loaded and
/// booked as `run_backtest` does, 100 USDT of PnL books 98 dollars at USDT-USD 0.98 (100 at par with
/// only the yen row); handed no pair, a profile naming one is refused, never run at par.
#[test]
fn a_run_over_held_bars_books_through_the_pairs_its_caller_loaded() {
    let store = booked_store();
    let over = |p: &ProfileBacktest, pairs: &[vike_simulator::ConversionSeries]| {
        crate::run::run_backtest_over_bars(p, &store, booked_bars(), &[], pairs, &Arc::default())
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
    // No USDT row: its loader finds the store's USDT-USD series with no setting (decision 0243, 2026-11-16).
    let found = over(&yen_only, &loaded(&yen_only)).expect("the USDT rate is found");
    assert_eq!(
        found.final_equity.to_bits(),
        booked.final_equity.to_bits(),
        "{}",
        found.final_equity
    );
    assert!(
        (booked.final_equity - (10_000.0 + 100.0 * 0.98)).abs() < 1e-9,
        "100 USDT of PnL at 0.98: {}",
        booked.final_equity
    );
    let err = over(&both, &[]).expect_err("a pair named and none handed").to_string();
    assert!(err.contains("does not load the conversion pairs") && err.contains("USDT"), "{err}");
}
