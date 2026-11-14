//! Decision 0271's run side through the harness: an input named, planned, read from a venue's series
//! and from a user's lane, remapped by `[inputs]`, judged before the price lane is read, and refused by
//! name with the way to fill it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vike_data::store::series::parse_input_name;
use vike_data::{HistoryStore, HistoryStoreError, HistoryStoreMemory, TimestampRange};
use vike_marketdata::Bar;
use vike_model::{OrderBroker, Strategy};
use vike_simulator::OrderBrokerSimulator;

use super::*;
use crate::data_plan::{DataPlanSeries, DataPlanSeriesRole, explain_document, plan_data};
use crate::harness::run_backtest;
use crate::harness::strategy_builder::StrategyBuilder;

/// A UTC midnight.
const S: i64 = 19_675 * DAY;
const HALF: i64 = 1_800_000;
const H4: i64 = 4 * 3_600_000;
const DAY: i64 = 86_400_000;
const FOUR_HOURLY: &str = "binance:BTCUSDT:4h";
const RATE: &str = "user-mine:binance:USDT-USD:1d";

fn bar(ts: i64, close: f64) -> Bar {
    Bar {
        timestamp: ts,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// The user lane's close on day `d` from `S`.
fn rate(d: i64) -> f64 {
    1.0 + d as f64 / 100.0
}

/// What each decision read: its bar's open and one value per input name.
type Seen = Arc<Mutex<Vec<(i64, Vec<Option<f64>>)>>>;

struct Reader {
    names: Vec<String>,
    seen: Seen,
}

impl Strategy<OrderBrokerSimulator> for Reader {
    fn inputs(&self) -> Vec<String> {
        self.names.clone()
    }
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, bar: &Bar) {
        let row = self.names.iter().map(|n| b.input(n).map(|x| x.close)).collect();
        self.seen.lock().unwrap().push((bar.timestamp, row));
    }
}

/// `days` days of the run's 30-minute bars from `S`, the 4h bars of the same symbol from one bar before
/// (closes 999, 1000, …), and the user's daily lane on `rate_days`.
fn store(days: i64, rate_days: &[i64]) -> Arc<HistoryStoreMemory> {
    let s = HistoryStoreMemory::new();
    let price: Vec<Bar> = (0..days * 48).map(|i| bar(S + i * HALF, 100.0)).collect();
    s.append_bars("binance", "BTCUSDT", "30m", &price, None).unwrap();
    let four: Vec<Bar> = (-1..days * 6).map(|k| bar(S + k * H4, 1_000.0 + k as f64)).collect();
    s.append_bars("binance", "BTCUSDT", "4h", &four, None).unwrap();
    let lane: Vec<Bar> = rate_days.iter().map(|&d| bar(S + d * DAY, rate(d))).collect();
    s.append_bars_sourced("binance", "user-mine", "USDT-USD", "1d", &lane, None).unwrap();
    Arc::new(s)
}

/// A bar run over `days` days whose strategy reads `names`; `top` goes before `[data]`, `data` inside it.
fn profile(days: i64, top: &str, data: &str, names: &[&str], seen: &Seen) -> ProfileBacktest {
    let toml = format!(
        "{top}\n[data]\nkind = \"bar\"\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\n\
         interval = \"30m\"\nfrom = \"{S}\"\nto = \"{}\"\n{data}\n[engine]\ncash = 10000.0\n\
         attach_funding = false\nfee = {{ kind = \"free\" }}\n\n[strategy]\nfile = \"other/input_reader.rs\"\n",
        S + days * DAY - 1
    );
    let mut p = ProfileBacktest::from_toml_str(&toml)
        .unwrap_or_else(|e| panic!("{e}\n{toml}"))
        .with_stand_in(vike_strategies_check::stand_in);
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    let seen = Arc::clone(seen);
    p.strategy.builder = Some(StrategyBuilder::new(move |_| {
        Ok(Box::new(Reader { names: names.clone(), seen: Arc::clone(&seen) }))
    }));
    p
}

/// The run's price reads, counted, over a [`HistoryStoreMemory`].
struct Counting {
    inner: Arc<HistoryStoreMemory>,
    price_reads: AtomicUsize,
}

impl HistoryStore for Counting {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
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
        self.inner.load_bars_sourced_capped(venue, source, symbol, interval, range, budget)
    }
    vike_data::hist_store_stubs!(inert: writes, scan_quotes, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

/// Both kinds of input through one run: each decision, at its bar's close, reads the last input bar
/// that had closed by then. The input bar closing WITH the decision is read (the 4h bar at every
/// eighth decision, the day at the last), the one in progress never is, and the bar that closed as
/// the window opened is the first value.
#[test]
fn an_input_is_read_from_a_venue_series_and_a_user_lane_as_of_each_close() {
    let seen = Seen::default();
    run_backtest(&profile(1, "", "", &[FOUR_HOURLY, RATE], &seen), store(1, &[-1, 0]))
        .expect("the run");
    let rows = seen.lock().unwrap().clone();
    assert_eq!(rows.len(), 48);
    for (ts, row) in &rows {
        let close = ts + HALF;
        let four = (close - S) / H4 - 1;
        let day = (close - S) / DAY - 1;
        assert_eq!(
            row,
            &vec![Some(1_000.0 + four as f64), Some(rate(day))],
            "at the close {close}"
        );
    }
    assert_eq!(rows[6].1[0], Some(999.0), "closes 3:30: the 0:00-4:00 bar is in progress");
    assert_eq!(rows[7].1[0], Some(1_000.0), "closes 4:00: the 0:00-4:00 bar closes with it");
    assert_eq!(rows[46].1[1], Some(rate(-1)));
    assert_eq!(rows[47].1[1], Some(rate(0)), "the last decision closes with the day and reads it");
}

/// `[inputs]` points the name the code writes at another series; without the row the name reads its
/// own series, which this store does not hold.
#[test]
fn the_inputs_table_remaps_a_name_without_touching_the_code() {
    let seen = Seen::default();
    let remap = format!("[inputs]\n\"binance:ETHUSDT:4h\" = \"{RATE}\"\n");
    run_backtest(&profile(1, &remap, "", &["binance:ETHUSDT:4h"], &seen), store(1, &[-1, 0]))
        .expect("the remapped run");
    let rows = seen.lock().unwrap().clone();
    assert_eq!(rows[0].1, vec![Some(rate(-1))]);
    assert_eq!(rows[47].1, vec![Some(rate(0))]);

    let own = profile(1, "", "", &["binance:ETHUSDT:4h"], &seen);
    let err = run_backtest(&own, store(1, &[-1, 0])).unwrap_err().to_string();
    assert!(err.contains("input \"binance:ETHUSDT:4h\": the store holds no bar of"), "{err}");
}

/// A missing input refuses the run by name, with the way to import it, before the price lane is
/// read and before the strategy runs.
#[test]
fn a_missing_input_is_refused_before_any_price_is_read_with_the_way_to_import_it() {
    let seen = Seen::default();
    let counting =
        Arc::new(Counting { inner: store(1, &[-1, 0]), price_reads: AtomicUsize::new(0) });
    let p = profile(1, "", "", &["user-absent:binance:EURUSD:1d"], &seen);
    let err = run_backtest(&p, Arc::clone(&counting) as Arc<dyn HistoryStore + Send + Sync>)
        .unwrap_err()
        .to_string();
    assert!(err.contains("Nothing ran"), "{err}");
    assert!(
        err.contains(
            "input \"user-absent:binance:EURUSD:1d\": the store holds no bar of binance EURUSD 1d \
             in the lane `user-absent`"
        ),
        "{err}"
    );
    assert!(
        err.contains(
            "import it: Studio → Data → Import CSV, or `vike-cli data hist import csv-bars <FILE> \
             --venue binance --symbol EURUSD --interval 1d --label absent`"
        ),
        "{err}"
    );
    assert_eq!(counting.price_reads.load(Ordering::SeqCst), 0, "no price was read");
    assert!(seen.lock().unwrap().is_empty(), "the strategy never ran");
}

/// A hole inside the window is refused and named; `data.max_gap` under `data.require_coverage` is
/// the tolerance that accepts it.
#[test]
fn a_holed_input_is_refused_naming_the_hole_and_max_gap_can_accept_it() {
    let seen = Seen::default();
    let err = run_backtest(&profile(3, "", "", &[RATE], &seen), store(3, &[-1, 0, 2]))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("does not cover the window for binance USDT-USD 1d in the lane `user-mine`"),
        "{err}"
    );
    assert!(err.contains("    interior "), "{err}");
    let tolerant = profile(3, "", "require_coverage = true\nmax_gap = \"1d\"\n", &[RATE], &seen);
    run_backtest(&tolerant, store(3, &[-1, 0, 2])).expect("a one-day hole within data.max_gap");
}

/// An `[inputs]` row that does not parse is refused at load; one naming no input the strategy reads
/// is refused when the run builds the strategy.
#[test]
fn an_inputs_row_is_refused_when_it_does_not_parse_or_names_no_input() {
    let toml = |row: &str| {
        format!(
            "[inputs]\n{row}\n[data]\nkind = \"bar\"\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\nfrom = \"0\"\n\
             to = \"1\"\n[engine]\ncash = 1.0\n[strategy]\nfile = \"other/x.rs\"\n"
        )
    };
    for (row, says) in [
        ("\"binance:BTCUSDT\" = \"binance:BTCUSDT:4h\"", "is not VENUE:SYMBOL:INTERVAL"),
        ("\"binance:BTCUSDT:4h\" = \"lane:binance:BTCUSDT:4h\"", "not a lane the store declares"),
        ("\"binance:BTCUSDT:4h\" = \"binance:BTCUSDT:fortnight\"", "not a bar length"),
        ("\"fortnight\" = \"binance:BTCUSDT:4h\"", "is not one"),
    ] {
        let err = ProfileBacktest::from_toml_str(&toml(row)).unwrap_err().to_string();
        assert!(err.contains("[inputs]") && err.contains(says), "{row}: {err}");
    }
    // A one-part name is the run's own symbol at that bar length, on either side of a row.
    for row in ["\"1d\" = \"user-mine:binance:BTCUSDT:1d\"", "\"binance:ETHUSDT:4h\" = \"4h\""] {
        ProfileBacktest::from_toml_str(&toml(row))
            .unwrap_or_else(|e| panic!("{row}: {e}"))
            .with_stand_in(vike_strategies_check::stand_in);
    }
    let seen = Seen::default();
    let stray = format!("[inputs]\n\"binance:BTCUSDT:1d\" = \"{RATE}\"\n");
    let err = run_backtest(&profile(1, &stray, "", &[FOUR_HOURLY], &seen), store(1, &[-1, 0]))
        .unwrap_err()
        .to_string();
    assert!(err.contains("names no input this strategy reads"), "{err}");
}

/// `data.explain` lists every input as a row of its own, carrying the name the code reads it by,
/// judged up to its last bar's close; a plan with no input carries no such field.
#[test]
fn data_explain_lists_each_input_by_its_name() {
    let seen = Seen::default();
    let p = profile(1, "", "", &[FOUR_HOURLY, RATE], &seen);
    let inputs = declared_inputs(&p).unwrap();
    let held = store(1, &[-1, 0]);
    let plan = plan_data(&p, &inputs, held.as_ref(), "test store", None).unwrap();
    let rows: Vec<&DataPlanSeries> =
        plan.series.iter().filter(|s| s.role == DataPlanSeriesRole::Input).collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].input.as_deref(), Some(FOUR_HOURLY));
    assert!(rows[0].held() && rows[0].missing.is_empty(), "{:?}", rows[0]);
    assert_eq!(
        (rows[1].input.as_deref(), rows[1].id.source.as_deref()),
        (Some(RATE), Some("user-mine"))
    );
    let lines = plan.lines();
    let four = "interval=4h [input] name=\"binance:BTCUSDT:4h\"";
    assert!(lines.iter().any(|l| l.contains(four)), "{lines:#?}");
    let lane = "interval=1d [input] source=user-mine name=\"user-mine:binance:USDT-USD:1d\"";
    assert!(lines.iter().any(|l| l.contains(lane)), "{lines:#?}");
    let doc = explain_document(&p, &plan).unwrap();
    let series = doc["series"].as_array().unwrap();
    assert!(series.iter().any(|s| s["role"] == "input" && s["input"] == FOUR_HOURLY), "{doc}");

    let bare = plan_data(&p, &[], held.as_ref(), "test store", None).unwrap();
    assert!(!serde_json::to_string(&bare).unwrap().contains("\"input\""));
}

/// The coverage rule: each bar covers its own interval, so bars that meet leave no span.
#[test]
fn an_input_covers_the_window_with_each_bar_up_to_its_close() {
    let bars = |ts: &[i64]| ts.iter().map(|&t| bar(t, 1.0)).collect::<Vec<_>>();
    let spans = |m: Vec<SeriesMissingSpan>| {
        m.iter().map(|m| (m.kind, m.start_ms, m.end_ms)).collect::<Vec<_>>()
    };
    assert!(input_shortfall(&bars(&[0, 10, 20]), 10, Some(0), Some(29)).is_empty());
    assert_eq!(
        spans(input_shortfall(&bars(&[0, 10, 20]), 10, Some(0), Some(30))),
        vec![(SeriesShortfall::Trailing, 30, 30)]
    );
    assert_eq!(
        spans(input_shortfall(&bars(&[10, 20]), 10, Some(5), Some(29))),
        vec![(SeriesShortfall::Leading, 5, 9)]
    );
    assert_eq!(
        spans(input_shortfall(&bars(&[0, 20]), 10, Some(0), Some(29))),
        vec![(SeriesShortfall::Interior, 10, 19)]
    );
    assert_eq!(
        spans(input_shortfall(&bars(&[100]), 10, Some(0), Some(29))),
        vec![(SeriesShortfall::Everything, 0, 29)]
    );
    assert_eq!(
        spans(input_shortfall(&[], 10, None, None)),
        vec![(SeriesShortfall::Everything, i64::MIN, i64::MAX)]
    );
    // A bar before the window that covers its start counts; an unbounded side is never short.
    assert!(input_shortfall(&bars(&[-10, 0]), 10, Some(-5), None).is_empty());
}

/// The grammar: the CLI's series spec, with the lane before it.
#[test]
fn an_input_name_is_the_clis_series_spec_with_its_lane_before_it() {
    let (id, ms) = parse_input_name(FOUR_HOURLY).unwrap();
    assert_eq!(
        (id.venue.as_str(), id.symbol.as_str(), id.interval.as_deref(), id.source.as_deref(), ms),
        ("binance", "BTCUSDT", Some("4h"), None, H4)
    );
    let (id, ms) = parse_input_name(RATE).unwrap();
    assert_eq!(
        (id.source.as_deref(), id.symbol.as_str(), ms),
        (Some("user-mine"), "USDT-USD", DAY)
    );
    for (bad, says) in [
        ("BTCUSDT", "is not VENUE:SYMBOL:INTERVAL"),
        ("a:b:c:d:e", "is not VENUE:SYMBOL:INTERVAL"),
        ("binance::4h", "empty or padded"),
        ("user:binance:BTCUSDT:4h", "not a lane the store declares"),
        ("file:binance:BTCUSDT:4h", "not a lane the store declares"),
        ("binance:BTCUSDT:funding", "not a bar length"),
    ] {
        let err = parse_input_name(bad).unwrap_err();
        assert!(err.contains(says), "{bad}: {err}");
    }
}

/// A door that loads no input refuses a strategy that reads one, by name, and passes one that reads
/// none.
#[test]
fn a_door_that_loads_no_input_refuses_a_strategy_that_reads_one() {
    assert!(refuse_unplanned_inputs(&[], "a walk-forward").is_ok());
    let err = refuse_unplanned_inputs(&[RATE.to_string()], "a walk-forward").unwrap_err();
    let err = err.to_string();
    assert!(err.contains("a walk-forward does not load a strategy's inputs yet"), "{err}");
    assert!(err.contains(RATE), "{err}");

    let seen = Seen::default();
    let p = profile(1, "", "", &[RATE], &seen);
    let handle: Arc<dyn HistoryStore + Send + Sync> = store(1, &[-1, 0]);
    let bars = handle.load_bars("binance", "BTCUSDT", "30m", TimestampRange::all()).unwrap();
    let held = vec![("BTCUSDT".to_string(), bars)];
    let err =
        crate::harness::run::run_backtest_over_bars(&p, &handle, held, &[], &[], &Arc::default())
            .unwrap_err()
            .to_string();
    assert!(err.contains("a run over bars it was handed"), "{err}");
}

// ---- closed market time, judged by the input's trading calendar --------------------------------

/// Epoch ms of `y-m-d` at `h:mi` UTC.
fn at(y: i64, m: u32, d: u32, h: i64, mi: i64) -> i64 {
    vike_utility::calendar::days_from_civil(y, m, d) * DAY + h * 3_600_000 + mi * 60_000
}

/// Whether `from .. to` is closed time or one holiday on `cal`.
fn excused(cal: &SessionCalendar, from: i64, to: i64) -> bool {
    closed_or_one_session(cal, from, to - 1)
}

/// The civil session, not the daylight-saving union: a weekend, a night and one whole session (a
/// holiday) are excused; a part of a session, or two sessions, are not; the FX week is one session
/// five days long, so a day inside it is never a holiday.
#[test]
fn closed_time_and_one_whole_session_are_excused_and_nothing_else_is() {
    use vike_venues::session::{FX_WEEK, US_EQUITY_REGULAR};
    let us = &US_EQUITY_REGULAR;
    // Thursday 2023-11-16, standard time: the session is 14:30-21:00 UTC.
    let thu = at(2023, 11, 16, 0, 0);
    let hour = 3_600_000;
    assert!(excused(us, thu, thu + DAY), "one whole session: a holiday");
    assert!(!excused(us, thu, thu + 2 * DAY), "two sessions");
    assert!(!excused(us, thu + 16 * hour, thu + 17 * hour), "an hour inside one");
    assert!(!excused(us, thu, thu + 15 * hour), "a session still open when the span ends");
    assert!(excused(us, thu + 2 * DAY, thu + 4 * DAY), "Saturday and Sunday");
    let (early, open) = (at(2023, 11, 16, 13, 30), at(2023, 11, 16, 14, 30));
    assert!(excused(us, early, open), "13:30 is closed in winter, open in the union");
    // Thursday 2024-07-11, daylight time: the session is 13:30-20:00 UTC.
    let summer = at(2024, 7, 11, 0, 0);
    assert!(excused(us, summer + 20 * hour, summer + 21 * hour), "20:00 is closed in summer");
    assert!(excused(us, summer, summer + DAY));

    let fx = &FX_WEEK;
    let (fri_w, fri_s) = (at(2023, 11, 17, 22, 0), at(2024, 7, 12, 21, 0));
    assert!(
        excused(fx, fri_w, fri_w + 2 * DAY),
        "the winter weekend: Friday 22:00 to Sunday 22:00"
    );
    assert!(excused(fx, fri_s, fri_s + 2 * DAY), "the summer weekend, an hour earlier");
    assert!(!excused(fx, fri_s, fri_s + 2 * DAY + hour), "an hour of Sunday evening's session");
    assert!(!excused(fx, at(2023, 11, 15, 0, 0), at(2023, 11, 16, 0, 0)), "a Wednesday");
}

/// The span rule over a calendar: a span no bar reaches is always a hole, one within the tolerance
/// never is, and with no calendar every span counts by its length, as before.
#[test]
fn a_span_is_a_hole_by_its_length_unless_its_calendar_excuses_it() {
    let us = vike_venues::session::US_EQUITY_REGULAR;
    let sat = at(2023, 11, 18, 0, 0);
    let span = |kind| SeriesMissingSpan { start_ms: sat, end_ms: sat + 2 * DAY - 1, kind };
    assert!(is_hole(&span(SeriesShortfall::Interior), None, 0));
    assert!(!is_hole(&span(SeriesShortfall::Interior), None, 2 * DAY));
    assert!(!is_hole(&span(SeriesShortfall::Interior), Some(&us), 0));
    assert!(!is_hole(&span(SeriesShortfall::Leading), Some(&us), 0));
    assert!(is_hole(&span(SeriesShortfall::Everything), Some(&us), i64::MAX));
}

/// Daily bars of `symbol` on `venue`, on the days from `S` in `days`.
fn daily_on(
    store: &HistoryStoreMemory,
    venue: &str,
    symbol: &str,
    days: impl Iterator<Item = i64>,
) {
    let bars: Vec<Bar> = days.map(|d| bar(S + d * DAY, 400.0 + d as f64)).collect();
    store.append_bars(venue, symbol, "1d", &bars, None).unwrap();
}

/// The weekdays from `S` (a Tuesday) in `from..to`: days 4 and 5 from it are a weekend.
fn weekdays(from: i64, to: i64) -> impl Iterator<Item = i64> {
    (from..to).filter(|d| !matches!(d.rem_euclid(7), 4 | 5))
}

/// `S` is Tuesday 2023-11-14, so day 9 from it is Thanksgiving.
const THANKSGIVING: i64 = 9;

/// `p` handed the KEPT row of alpaca's `SPY` (an ETF, the class the datahub's list states): what a door that holds
/// the kept lists hands a run (`ProfileBacktest::kept`).
fn with_spy_row(mut p: ProfileBacktest) -> ProfileBacktest {
    let etf = vike_model::Contract::Etf(vike_model::ContractEquity::default());
    p.kept.rows = vec![vike_model::Instrument::new("alpaca", "SPY", "SPY", "USD", etf)];
    p
}

/// **A daily stock input over two weekends and a holiday runs with no `data.max_gap`**: alpaca's
/// US shares have a trading calendar, and the market was closed on every day the bars skip.
#[test]
fn a_daily_stock_input_runs_over_weekends_and_a_holiday_with_no_tolerance() {
    let seen = Seen::default();
    let held = store(14, &[]);
    daily_on(&held, "alpaca", "SPY", weekdays(-1, 14).filter(|d| *d != THANKSGIVING));
    run_backtest(&with_spy_row(profile(14, "", "", &["alpaca:SPY:1d"], &seen)), held)
        .expect("closed days");
    let rows = seen.lock().unwrap().clone();
    assert_eq!(
        rows[10 * 48].1,
        vec![Some(408.0)],
        "the day after the holiday reads Wednesday's bar"
    );
}

/// A REAL hole is still refused on a stock input: two trading days in a row on daily bars, and one
/// hour inside a session on hourly bars (whose nights and weekend are closed time and pass). The
/// refusal says the calendar judged it.
#[test]
fn a_hole_on_trading_days_is_still_refused_on_a_stock_input() {
    let seen = Seen::default();
    let held = store(7, &[]);
    daily_on(&held, "alpaca", "SPY", weekdays(-1, 7).filter(|d| !matches!(d, 1 | 2)));
    let err = run_backtest(&with_spy_row(profile(7, "", "", &["alpaca:SPY:1d"], &seen)), held)
        .unwrap_err()
        .to_string();
    assert!(err.contains("input \"alpaca:SPY:1d\": the store does not cover the window"), "{err}");
    assert!(err.contains("    interior "), "{err}");
    assert!(err.contains("judged by its market's trading calendar"), "{err}");

    // Hourly bars 14:00-20:00 UTC on the trading days: the winter session is 14:30-21:00.
    let hourly = |skip: Option<i64>| {
        let held = store(7, &[]);
        let bars: Vec<Bar> = weekdays(0, 7)
            .flat_map(|d| (14..=20).map(move |h| S + d * DAY + h * 3_600_000))
            .filter(|ts| Some(*ts) != skip)
            .map(|ts| bar(ts, 400.0))
            .collect();
        held.append_bars("alpaca", "SPY", "1h", &bars, None).unwrap();
        held
    };
    let hourly_run = with_spy_row(profile(7, "", "", &["alpaca:SPY:1h"], &seen));
    run_backtest(&hourly_run, hourly(None)).expect("nights and the weekend are closed time");
    let wednesday_four = S + DAY + 16 * 3_600_000;
    let err = run_backtest(&hourly_run, hourly(Some(wednesday_four))).unwrap_err().to_string();
    assert!(err.contains("input \"alpaca:SPY:1h\": the store does not cover the window"), "{err}");
}

/// **A crypto input behaves exactly as before**: its market never closes, so a weekend missing from
/// its daily bars is a hole, refused in the old words with no calendar line, and `data.max_gap`
/// still accepts it.
#[test]
fn a_crypto_input_keeps_the_plain_rule_and_its_old_words() {
    let seen = Seen::default();
    let held = || {
        let held = store(7, &[]);
        daily_on(&held, "binance", "BTCUSDT", weekdays(-1, 7));
        held
    };
    let daily = "binance:BTCUSDT:1d";
    let err = run_backtest(&profile(7, "", "", &[daily], &seen), held()).unwrap_err().to_string();
    assert!(
        err.contains("input \"binance:BTCUSDT:1d\": the store does not cover the window"),
        "{err}"
    );
    assert!(!err.contains("calendar"), "{err}");
    let tolerant = profile(7, "", "require_coverage = true\nmax_gap = \"2d\"\n", &[daily], &seen);
    run_backtest(&tolerant, held()).expect("a two-day gap within data.max_gap");
}

/// A venue whose class no kept row states (an IBKR share) keeps the plain rule, and its refusal says
/// that its closed hours count as missing.
#[test]
fn an_input_with_no_known_calendar_says_its_closed_hours_count() {
    let seen = Seen::default();
    let held = store(7, &[]);
    daily_on(&held, "ibkr", "SPY", weekdays(-1, 7));
    let err =
        run_backtest(&profile(7, "", "", &["ibkr:SPY:1d"], &seen), held).unwrap_err().to_string();
    assert!(err.contains("no trading calendar is known for ibkr SPY"), "{err}");
}

/// **A run handed no kept row keeps the plain rule for a share too**: the class is never read off the name
/// (the owner's ruling of 2026-11-13), so alpaca's `SPY` with no row is a market of unknown class, and its
/// closed days count as missing.
#[test]
fn an_input_with_no_kept_row_is_of_unknown_class() {
    let seen = Seen::default();
    let held = store(14, &[]);
    daily_on(&held, "alpaca", "SPY", weekdays(-1, 14).filter(|d| *d != THANKSGIVING));
    let err = run_backtest(&profile(14, "", "", &["alpaca:SPY:1d"], &seen), held)
        .unwrap_err()
        .to_string();
    assert!(err.contains("no trading calendar is known for alpaca SPY"), "{err}");
}

// ---- the one-part name and the history read ----------------------------------------------------

/// What each event read of each input: the last closed bar's close and how many bars had closed.
type History = Arc<Mutex<Vec<(i64, Vec<(Option<f64>, usize)>)>>>;

struct HistoryReader {
    names: Vec<String>,
    seen: History,
}

impl HistoryReader {
    fn record(&self, b: &OrderBrokerSimulator, ts: i64) {
        let row = self.names.iter().map(|n| (b.input(n).map(|x| x.close), b.input_bars(n).len()));
        self.seen.lock().unwrap().push((ts, row.collect()));
    }
}

impl Strategy<OrderBrokerSimulator> for HistoryReader {
    fn inputs(&self) -> Vec<String> {
        self.names.clone()
    }
    fn on_bar(&mut self, b: &mut OrderBrokerSimulator, bar: &Bar) {
        self.record(b, bar.timestamp);
    }
    fn on_quote_tick(&mut self, b: &mut OrderBrokerSimulator, q: &vike_marketdata::TickQuote) {
        self.record(b, q.timestamp);
    }
}

/// `p` with its strategy a [`HistoryReader`] of `names`.
fn reading_history(mut p: ProfileBacktest, names: &[&str], seen: &History) -> ProfileBacktest {
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    let seen = Arc::clone(seen);
    p.strategy.builder = Some(StrategyBuilder::new(move |_| {
        Ok(Box::new(HistoryReader { names: names.clone(), seen: Arc::clone(&seen) }))
    }));
    p
}

/// A one-part name is the run's own symbol at that bar length, in its `data.source` lane when it
/// reads one; a run with no one own series refuses it, naming the full spelling. Through the run it
/// reads what the full name reads, and `data.explain` names it as the code wrote it.
#[test]
fn a_one_part_name_is_the_runs_own_symbol_at_that_bar_length() {
    let seen = Seen::default();
    let p = profile(1, "", "", &["4h"], &seen);
    let own = resolve_inputs(&p, &["4h".to_string()]).unwrap();
    assert_eq!((own[0].spec().as_str(), own[0].interval_ms), ("binance:BTCUSDT:4h", H4));
    let mut lane = p.clone();
    lane.data.source = Some("user-mine".to_string());
    let day = resolve_inputs(&lane, &["1d".to_string()]).unwrap();
    assert_eq!(day[0].spec(), "user-mine:binance:BTCUSDT:1d");

    let mut two = p.clone();
    two.data.symbols.push("ETHUSDT".to_string());
    let mut file = p.clone();
    file.data.file = Some("bars.csv".to_string());
    for (refused, says) in [(two, "trades no one symbol of one venue"), (file, "(data.file)")] {
        let err = resolve_inputs(&refused, &["4h".to_string()]).unwrap_err().to_string();
        assert!(err.contains(says) && err.contains("VENUE:SYMBOL:INTERVAL"), "{err}");
    }

    run_backtest(&p, store(1, &[])).expect("the run");
    let rows = seen.lock().unwrap().clone();
    assert_eq!(rows[7].1, vec![Some(1_000.0)], "closes 4:00: the 0:00-4:00 bar closes with it");
    let held = store(1, &[]);
    let plan = plan_data(&p, &declared_inputs(&p).unwrap(), held.as_ref(), "test", None).unwrap();
    let row = plan.series.iter().find(|s| s.role == DataPlanSeriesRole::Input).unwrap();
    assert_eq!((row.input.as_deref(), row.id.interval.as_deref()), (Some("4h"), Some("4h")));
    assert!(plan.lines().iter().any(|l| l.contains("interval=4h [input] name=\"4h\"")));
}

/// **The history read on the bar lane**: a higher timeframe is read from its first stored bar, so
/// the first decision sees all of its history; an input as long as the run's bars is read from one
/// of its bars before the window.
#[test]
fn a_higher_timeframe_is_read_from_its_first_bar_and_a_shorter_one_from_one_bar_before() {
    let held = HistoryStoreMemory::new();
    let price: Vec<Bar> = (0..48).map(|i| bar(S + i * HALF, 100.0)).collect();
    held.append_bars("binance", "BTCUSDT", "30m", &price, None).unwrap();
    let four: Vec<Bar> = (-30..6).map(|k| bar(S + k * H4, 1_000.0 + k as f64)).collect();
    held.append_bars("binance", "BTCUSDT", "4h", &four, None).unwrap();
    let eth: Vec<Bar> = (-20..48).map(|i| bar(S + i * HALF, 50.0 + i as f64)).collect();
    held.append_bars("binance", "ETHUSDT", "30m", &eth, None).unwrap();

    let seen = History::default();
    let names = ["binance:BTCUSDT:4h", "binance:ETHUSDT:30m"];
    let p = reading_history(profile(1, "", "", &[], &Seen::default()), &names, &seen);
    run_backtest(&p, Arc::new(held)).expect("the run");
    let rows = seen.lock().unwrap().clone();
    assert_eq!(
        rows[0].1,
        vec![(Some(999.0), 30), (Some(50.0), 2)],
        "the first decision, at 0:30: all thirty 4h bars, and the two 30m bars closed by then"
    );
}

/// A store holding the run's quotes beside the [`HistoryStoreMemory`]'s bars.
struct WithQuotes {
    bars: HistoryStoreMemory,
    quotes: Vec<vike_marketdata::TickQuote>,
}

impl HistoryStore for WithQuotes {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.bars.load_bars(venue, symbol, interval, range)
    }
    /// The quotes in `range`, carrying `symbol` as a real store's read re-injects it (the engine
    /// routes each tick by it).
    fn scan_quotes(
        &self,
        _: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<vike_marketdata::TickQuote>, HistoryStoreError> {
        let inside =
            |ts: i64| range.start.is_none_or(|s| ts >= s) && range.end.is_none_or(|e| ts <= e);
        let quotes = self.quotes.iter().filter(|q| inside(q.timestamp));
        Ok(quotes
            .map(|q| vike_marketdata::TickQuote { symbol: symbol.to_string(), ..q.clone() })
            .collect())
    }
    vike_data::hist_store_stubs!(inert: writes, scan_trades, scan_book_updates,
        scan_symbol_properties, scan_equity, scan_exec_fills, scan_exec_orders);
}

/// **An input on the TICK lane, through `run_backtest`**: each tick sees the last input bar closed
/// by the tick's own time, so the 0:00-4:00 bar is unseen one millisecond before 4:00 and seen at
/// 4:00; and a higher timeframe on the tick lane is read from its first stored bar.
#[test]
fn a_tick_run_reads_an_input_as_of_each_tick_with_its_whole_history() {
    let bars = HistoryStoreMemory::new();
    let four: Vec<Bar> = (-5..6).map(|k| bar(S + k * H4, 1_000.0 + k as f64)).collect();
    bars.append_bars("binance", "BTCUSDT", "4h", &four, None).unwrap();
    let ticks = [S, S + H4 - 1, S + H4, S + 2 * H4 - 1];
    let quotes = ticks.iter().map(|&ts| vike_marketdata::test_support::quote(ts, 99.0, 101.0));
    let held = Arc::new(WithQuotes { bars, quotes: quotes.collect() });

    let toml = format!(
        "[data]\nkind = \"tick\"\nvenue = \"binance\"\nsymbols = [\"BTCUSDT\"]\ninterval = \"1m\"\n\
         from = \"{S}\"\nto = \"{}\"\n\n[engine]\ncash = 10000.0\nattach_funding = false\n\
         fee = {{ kind = \"free\" }}\n\n[strategy]\nfile = \"other/input_reader.rs\"\n",
        S + DAY - 1
    );
    let seen = History::default();
    let p = reading_history(
        ProfileBacktest::from_toml_str(&toml)
            .unwrap()
            .with_stand_in(vike_strategies_check::stand_in),
        &[FOUR_HOURLY],
        &seen,
    );
    run_backtest(&p, held).expect("the tick run");
    let rows = seen.lock().unwrap().clone();
    assert_eq!(
        rows,
        vec![
            (S, vec![(Some(999.0), 5)]),
            (S + H4 - 1, vec![(Some(999.0), 5)]),
            (S + H4, vec![(Some(1_000.0), 6)]),
            (S + 2 * H4 - 1, vec![(Some(1_000.0), 6)]),
        ],
        "the five bars stored before the window are read at the first tick"
    );
}
